use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use sysinfo::System;
use tauri::{AppHandle, Emitter, Manager};

const MODRINTH_API: &str = "https://api.modrinth.com/v2";
const FABRIC_META: &str = "https://meta.fabricmc.net/v2";
const NEOFORGE_VERSIONS_API: &str =
    "https://maven.neoforged.net/api/maven/versions/releases/net/neoforged/neoforge";
const NEOFORGE_MAVEN_BASE: &str =
    "https://maven.neoforged.net/releases/net/neoforged/neoforge";
const USER_AGENT: &str = "minecraft-mod-like-im-five/0.1";

// Curated profiles are embedded at compile time as a fallback. The runtime
// preference is to fetch the latest YAML from the `data-latest` GitHub
// release so adding a mod doesn't require a binary rebuild — clients pick
// up the new profiles on next launch.
const PROFILES_YAML: &str = include_str!("../../data/profiles.yaml");
const DATA_RELEASE_BASE: &str =
    "https://github.com/zlmitchell/minecraft-mod-like-im-five/releases/download/data-latest";
const DATA_RELEASE_API: &str =
    "https://api.github.com/repos/zlmitchell/minecraft-mod-like-im-five/releases/tags/data-latest";

// Dev escape hatch. Set MMLE5_LOCAL_DATA to a directory containing
// profiles.yaml (and optionally frameworks.yaml) to short-circuit the
// GitHub data-latest fetch and read straight from disk on every call.
// Used to test profile changes without publishing a data-latest release.
const LOCAL_DATA_ENV: &str = "MMLE5_LOCAL_DATA";

fn local_data_dir() -> Option<PathBuf> {
    std::env::var(LOCAL_DATA_ENV).ok().filter(|s| !s.is_empty()).map(PathBuf::from)
}

// Each `*Ref` carries `source` to discriminate between hosting providers.
// `modrinth`     — needs `slug`
// `url`          — needs `url` + `filename`
// `curseforge`   — needs `file_id` + `filename`. We build the forgecdn URL
//                  ourselves so the YAML doesn't carry the gnarly path math.

#[derive(Serialize, Deserialize, Clone, Debug)]
struct ModRef {
    source: String,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    file_id: Option<u64>,
    #[serde(default)]
    role: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct ShaderRef {
    source: String,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    default: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct ResourcePackRef {
    source: String,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    default: bool,
}

fn ref_display_name(source: &str, slug: &Option<String>, filename: &Option<String>) -> String {
    slug.clone()
        .or_else(|| filename.clone())
        .unwrap_or_else(|| source.to_string())
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct ModpackRef {
    /// Currently only "modrinth-modpack" (.mrpack). CurseForge zip is a
    /// future addition.
    source: String,
    slug: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Profile {
    id: String,
    name: String,
    short_description: String,
    /// Optional for modpack-style profiles — the .mrpack manifest pins both.
    #[serde(default)]
    minecraft_version: Option<String>,
    #[serde(default)]
    loader: Option<String>,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    accent: Option<String>,
    /// Mojang launcher icon. Either a vanilla block name (Crafting_Table,
    /// Furnace, Cake, Enchanting_Table, etc.) or a `data:image/png;base64,...`
    /// data URI. Defaults to Crafting_Table if absent.
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    not_implemented_in_phase_1: bool,
    #[serde(default)]
    java_xmx_gb: Option<u32>,
    /// If set, install via the modpack flow (download .mrpack, parse
    /// manifest, fetch every file, copy overrides). The mods/shaders/
    /// resource_packs arrays below are ignored.
    #[serde(default)]
    modpack: Option<ModpackRef>,
    /// Extra jars to drop into mods/ AFTER a modpack install. Used for
    /// CurseForge-only or otherwise non-redistributable mods that the
    /// .mrpack manifest can't ship — we pin a direct URL per profile.
    /// Pin the URL by file ID (forgecdn) so it doesn't drift; if the pack
    /// updates the dep version, update this entry too.
    #[serde(default)]
    modpack_supplements: Vec<ModRef>,
    #[serde(default)]
    mods: Vec<ModRef>,
    #[serde(default)]
    shaders: Vec<ShaderRef>,
    #[serde(default)]
    resource_packs: Vec<ResourcePackRef>,
}

/// .mrpack format spec: https://docs.modrinth.com/modpacks/format/
#[derive(Deserialize)]
struct MrpackIndex {
    #[serde(default)]
    name: String,
    files: Vec<MrpackFile>,
    dependencies: HashMap<String, String>,
}

#[derive(Deserialize)]
struct MrpackFile {
    path: String,
    #[serde(default)]
    hashes: ModrinthHashes,
    downloads: Vec<String>,
    #[serde(default)]
    env: Option<HashMap<String, String>>,
}

#[derive(Serialize, Deserialize)]
struct ProfilesFile {
    #[allow(dead_code)]
    version: u32,
    profiles: Vec<Profile>,
}

// Written into mods/.mmle5-state.json after every successful install. Lets a
// future install know which profile last owned mods/ so we can snapshot the
// jars under that profile's name and (if the user is switching back) restore
// from a prior snapshot instead of re-downloading.
#[derive(Serialize, Deserialize, Clone)]
struct MmleState {
    profile_id: String,
    profile_name: String,
    #[serde(default)]
    mc_version: Option<String>,
    #[serde(default)]
    loader: Option<String>,
    #[serde(default)]
    loader_version: Option<String>,
    #[serde(default)]
    modpack_version: Option<String>,
    installed_at: String,
}

const STATE_FILENAME: &str = ".mmle5-state.json";

#[derive(Serialize, Deserialize)]
struct InstallReport {
    profile_name: String,
    minecraft_version: String,
    loader_version: String,
    mods_installed: u32,
    shaders_installed: u32,
    resource_packs_installed: u32,
    skipped: Vec<String>,
}

#[derive(Serialize)]
struct LatestMcVersion {
    release: String,
    snapshot: Option<String>,
}

#[derive(Serialize)]
struct RunningProc {
    name: String,
    pid: u32,
}

#[derive(Serialize, Deserialize)]
struct JarIdentity {
    matched: bool,
    title: Option<String>,
    version_number: Option<String>,
    loaders: Vec<String>,
    game_versions: Vec<String>,
    project_id: Option<String>,
}

#[derive(Deserialize, Default, Clone)]
struct ModrinthHashes {
    #[serde(default)]
    sha1: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    sha512: Option<String>,
}

#[derive(Deserialize)]
struct ModrinthFile {
    url: String,
    filename: String,
    #[serde(default)]
    primary: bool,
    #[serde(default)]
    hashes: ModrinthHashes,
}

#[derive(Deserialize)]
struct ModrinthVersion {
    #[allow(dead_code)]
    id: String,
    version_number: String,
    files: Vec<ModrinthFile>,
    #[serde(default)]
    game_versions: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    loaders: Vec<String>,
}

fn default_minecraft_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var_os("APPDATA")?;
        Some(PathBuf::from(appdata).join(".minecraft"))
    }
    #[cfg(target_os = "macos")]
    {
        let home = dirs::home_dir()?;
        Some(home.join("Library/Application Support/minecraft"))
    }
    #[cfg(target_os = "linux")]
    {
        let home = dirs::home_dir()?;
        Some(home.join(".minecraft"))
    }
}

// NeoForge requires running its installer JAR. We need a JVM. Strategy:
// 1) Reuse the JRE that the Minecraft launcher already bundles. Its layout
//    is `<runtime_root>/<variant>/<arch>/<variant>/bin/java(.exe)`. The
//    variant names rotate (gamma/delta/...) when Mojang ships a new JRE,
//    so we prefer known names then fall back to whatever's there.
// 2) Fall back to system `java` on PATH.
// MC 1.21.1 + NeoForge requires Java 21 — `java-runtime-delta` ships Java 21,
// `java-runtime-gamma` is Java 17. Prefer delta first.
fn launcher_runtime_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    #[cfg(target_os = "windows")]
    {
        if let Ok(p) = std::env::var("ProgramFiles") {
            roots.push(PathBuf::from(p).join("Minecraft Launcher").join("runtime"));
        }
        if let Ok(p) = std::env::var("ProgramFiles(x86)") {
            roots.push(PathBuf::from(p).join("Minecraft Launcher").join("runtime"));
        }
        if let Ok(p) = std::env::var("LOCALAPPDATA") {
            roots.push(
                PathBuf::from(p).join(
                    r"Packages\Microsoft.4297127D64EC6_8wekyb3d8bbwe\LocalCache\Local\runtime",
                ),
            );
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(home) = dirs::home_dir() {
            roots.push(home.join(".minecraft").join("runtime"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = dirs::home_dir() {
            roots.push(
                home.join("Library/Application Support/minecraft").join("runtime"),
            );
        }
    }
    roots
}

fn java_arch_subdir() -> &'static str {
    #[cfg(target_os = "windows")]
    { "windows-x64" }
    #[cfg(target_os = "linux")]
    { "linux" }
    #[cfg(target_os = "macos")]
    { "mac-os" }
}

fn java_exe_name() -> &'static str {
    #[cfg(target_os = "windows")]
    { "java.exe" }
    #[cfg(not(target_os = "windows"))]
    { "java" }
}

fn find_launcher_java() -> Option<PathBuf> {
    let arch = java_arch_subdir();
    let exe = java_exe_name();
    // Prefer Java 21 (delta) for modern MC, then Java 17 (gamma), then anything.
    let preferred = [
        "java-runtime-delta",
        "java-runtime-gamma",
        "java-runtime-beta",
        "java-runtime-alpha",
        "jre-legacy",
    ];
    for root in launcher_runtime_roots() {
        if !root.exists() {
            continue;
        }
        for variant in &preferred {
            let p = root.join(variant).join(arch).join(variant).join("bin").join(exe);
            if p.exists() {
                return Some(p);
            }
        }
        // Generic fallback: first variant present on disk.
        if let Ok(rd) = fs::read_dir(&root) {
            for entry in rd.filter_map(|e| e.ok()) {
                let name = entry.file_name();
                let p = entry
                    .path()
                    .join(arch)
                    .join(name.to_string_lossy().as_ref())
                    .join("bin")
                    .join(exe);
                if p.exists() {
                    return Some(p);
                }
            }
        }
    }
    None
}

fn find_path_java() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let lookup = "where";
    #[cfg(not(target_os = "windows"))]
    let lookup = "which";
    let exe = java_exe_name();
    let out = std::process::Command::new(lookup).arg(exe).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let line = s.lines().next()?.trim();
    if line.is_empty() {
        return None;
    }
    let p = PathBuf::from(line);
    if p.exists() { Some(p) } else { None }
}

fn find_java() -> Option<PathBuf> {
    find_launcher_java().or_else(find_path_java)
}

#[tauri::command]
fn get_minecraft_dir() -> Result<String, String> {
    let dir = default_minecraft_dir()
        .ok_or_else(|| "Cannot determine .minecraft path".to_string())?;
    if !dir.exists() {
        return Err(format!(
            "Minecraft folder not found at {}. Open the Minecraft launcher once to create it.",
            dir.display()
        ));
    }
    Ok(dir.to_string_lossy().to_string())
}

fn cached_data_path(app: &AppHandle, filename: &str) -> Option<PathBuf> {
    app.path()
        .app_local_data_dir()
        .ok()
        .map(|dir| dir.join(filename))
}

fn read_profiles(app: &AppHandle) -> Result<Vec<Profile>, String> {
    // Local dev override: read straight from disk on every call so profile
    // edits show up without a rebuild. Errors here are surfaced (not silently
    // skipped) — if the user set the env var, they want feedback when the
    // file is malformed instead of silently falling back to GitHub data.
    if let Some(dir) = local_data_dir() {
        let path = dir.join("profiles.yaml");
        let content = fs::read_to_string(&path)
            .map_err(|e| format!("MMLE5_LOCAL_DATA read {}: {e}", path.display()))?;
        let parsed: ProfilesFile = serde_yaml::from_str(&content)
            .map_err(|e| format!("MMLE5_LOCAL_DATA parse {}: {e}", path.display()))?;
        return Ok(parsed.profiles);
    }
    if let Some(path) = cached_data_path(app, "profiles.yaml") {
        if let Ok(content) = fs::read_to_string(&path) {
            if let Ok(parsed) = serde_yaml::from_str::<ProfilesFile>(&content) {
                return Ok(parsed.profiles);
            }
        }
    }
    let parsed: ProfilesFile = serde_yaml::from_str(PROFILES_YAML)
        .map_err(|e| format!("parse profiles.yaml: {e}"))?;
    Ok(parsed.profiles)
}

#[tauri::command]
fn list_profiles(app: AppHandle) -> Result<Vec<Profile>, String> {
    read_profiles(&app)
}

#[tauri::command]
fn get_app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[derive(Serialize)]
struct DataManifest {
    label: String,
    published_at: Option<String>,
    using_cache: bool,
}

#[tauri::command]
async fn get_data_manifest(app: AppHandle) -> Result<DataManifest, String> {
    if let Some(dir) = local_data_dir() {
        return Ok(DataManifest {
            label: format!("local-dev: {}", dir.display()),
            published_at: None,
            using_cache: false,
        });
    }
    let using_cache = cached_data_path(&app, "profiles.yaml")
        .map(|p| p.exists())
        .unwrap_or(false);
    let client = http_client();
    let resp = client
        .get(DATA_RELEASE_API)
        .send()
        .await
        .map_err(|e| format!("data-latest api: {e}"))?;
    if !resp.status().is_success() {
        return Ok(DataManifest {
            label: "embedded".to_string(),
            published_at: None,
            using_cache,
        });
    }
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("data-latest json: {e}"))?;
    Ok(DataManifest {
        label: v
            .get("name")
            .and_then(|x| x.as_str())
            .map(String::from)
            .unwrap_or_else(|| "data-latest".to_string()),
        published_at: v
            .get("published_at")
            .and_then(|x| x.as_str())
            .map(String::from),
        using_cache,
    })
}

#[tauri::command]
async fn refresh_data_cache(app: AppHandle) -> Result<bool, String> {
    // Skip the GitHub fetch entirely when local-dev is on; otherwise the
    // cache would shadow whatever the user is editing on disk on next launch.
    if local_data_dir().is_some() {
        return Ok(false);
    }
    let cache_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("app local data dir: {e}"))?;
    fs::create_dir_all(&cache_dir).map_err(|e| format!("mkdir cache: {e}"))?;
    let client = http_client();
    let mut updated = false;
    for filename in ["profiles.yaml", "frameworks.yaml"] {
        let url = format!("{DATA_RELEASE_BASE}/{filename}");
        let resp = match client.get(&url).send().await {
            Ok(r) if r.status().is_success() => r,
            _ => continue,
        };
        let body = match resp.text().await {
            Ok(b) => b,
            Err(_) => continue,
        };
        // Validate parses before writing — never poison the cache.
        if serde_yaml::from_str::<Value>(&body).is_err() {
            continue;
        }
        let dest = cache_dir.join(filename);
        if fs::write(&dest, body).is_ok() {
            updated = true;
        }
    }
    Ok(updated)
}

async fn fetch_latest_minecraft_version(
    client: &reqwest::Client,
) -> Result<LatestMcVersion, String> {
    let resp: Value = client
        .get("https://launchermeta.mojang.com/mc/game/version_manifest_v2.json")
        .send()
        .await
        .map_err(|e| format!("mojang manifest: {e}"))?
        .json()
        .await
        .map_err(|e| format!("mojang manifest json: {e}"))?;
    let release = resp
        .get("latest")
        .and_then(|l| l.get("release"))
        .and_then(|r| r.as_str())
        .map(String::from)
        .ok_or_else(|| "manifest missing latest.release".to_string())?;
    let snapshot = resp
        .get("latest")
        .and_then(|l| l.get("snapshot"))
        .and_then(|s| s.as_str())
        .map(String::from);
    Ok(LatestMcVersion { release, snapshot })
}

#[tauri::command]
async fn get_latest_minecraft_version() -> Result<LatestMcVersion, String> {
    fetch_latest_minecraft_version(&http_client()).await
}

fn collect_minecraft_processes(sys: &System) -> Vec<RunningProc> {
    // Exclude ourselves: our binary name contains "minecraft" so a naive
    // substring match would have us asking the user to close the app they
    // just opened.
    let self_pid = std::process::id();
    let mut found = vec![];
    for (pid, proc) in sys.processes() {
        if pid.as_u32() == self_pid {
            continue;
        }
        let name = proc.name().to_string_lossy().to_string();
        let lower = name.to_lowercase();
        if lower.contains("mod-like-im-five") {
            continue;
        }
        let cmd_has_minecraft = proc.cmd().iter().any(|s| {
            s.to_string_lossy().to_lowercase().contains(".minecraft")
        });
        let is_mc_named = lower.contains("minecraft");
        let is_java_running_mc = matches!(lower.as_str(), "javaw.exe" | "java.exe" | "javaw" | "java")
            && cmd_has_minecraft;
        if is_mc_named || is_java_running_mc {
            found.push(RunningProc {
                name,
                pid: pid.as_u32(),
            });
        }
    }
    found
}

#[tauri::command]
fn find_minecraft_processes() -> Vec<RunningProc> {
    let mut sys = System::new_all();
    sys.refresh_all();
    collect_minecraft_processes(&sys)
}

#[tauri::command]
fn kill_minecraft_processes() -> u32 {
    let mut sys = System::new_all();
    sys.refresh_all();
    let mut killed = 0u32;
    let pids: Vec<u32> = collect_minecraft_processes(&sys)
        .into_iter()
        .map(|p| p.pid)
        .collect();
    for pid_u32 in pids {
        if let Some(proc) = sys.process(sysinfo::Pid::from_u32(pid_u32)) {
            if proc.kill() {
                killed += 1;
            }
        }
    }
    killed
}

#[tauri::command]
fn launch_minecraft_launcher() -> Result<(), String> {
    let mut tried: Vec<String> = vec![];

    // 1. Start Menu shortcuts. The Microsoft Store install drops one here,
    //    and `cmd /c start` resolves the .lnk to its real target via the
    //    Windows shell — works for both Store-app and standalone installs.
    let lnk_candidates: Vec<PathBuf> = [
        std::env::var("APPDATA").ok().map(|p| {
            PathBuf::from(p).join(r"Microsoft\Windows\Start Menu\Programs\Minecraft Launcher.lnk")
        }),
        std::env::var("ProgramData").ok().map(|p| {
            PathBuf::from(p).join(r"Microsoft\Windows\Start Menu\Programs\Minecraft Launcher.lnk")
        }),
        std::env::var("APPDATA").ok().map(|p| {
            PathBuf::from(p).join(r"Microsoft\Windows\Start Menu\Programs\Minecraft.lnk")
        }),
        std::env::var("ProgramData").ok().map(|p| {
            PathBuf::from(p).join(r"Microsoft\Windows\Start Menu\Programs\Minecraft.lnk")
        }),
    ]
    .into_iter()
    .flatten()
    .collect();

    for lnk in &lnk_candidates {
        if lnk.exists() {
            let lnk_str = lnk.to_string_lossy().to_string();
            match std::process::Command::new("cmd")
                .args(["/c", "start", "", &lnk_str])
                .spawn()
            {
                Ok(_) => return Ok(()),
                Err(e) => tried.push(format!("{}: {e}", lnk.display())),
            }
        }
    }

    // 2. Direct exe paths for standalone installs (older / non-Store).
    let exe_candidates: Vec<PathBuf> = [
        std::env::var("ProgramFiles(x86)").ok().map(|p| {
            PathBuf::from(p).join("Minecraft Launcher").join("MinecraftLauncher.exe")
        }),
        std::env::var("ProgramFiles").ok().map(|p| {
            PathBuf::from(p).join("Minecraft Launcher").join("MinecraftLauncher.exe")
        }),
        std::env::var("LOCALAPPDATA").ok().map(|p| {
            PathBuf::from(p).join(r"Programs\Minecraft Launcher\MinecraftLauncher.exe")
        }),
    ]
    .into_iter()
    .flatten()
    .collect();

    for exe in &exe_candidates {
        if exe.exists() {
            match std::process::Command::new(exe).spawn() {
                Ok(_) => return Ok(()),
                Err(e) => tried.push(format!("{}: {e}", exe.display())),
            }
        }
    }

    // 3. Microsoft Store app via shell:AppsFolder + AUMID. No file check
    //    possible; this either works or explorer pops a "not installed" UI.
    if std::process::Command::new("explorer.exe")
        .arg(r"shell:AppsFolder\Microsoft.4297127D64EC6_8wekyb3d8bbwe!Minecraft")
        .spawn()
        .is_ok()
    {
        return Ok(());
    }
    tried.push("shell:AppsFolder".into());

    // 4. URI schemes registered by the official launcher (last resort).
    for scheme in ["minecraft://", "minecraft-launcher://"] {
        if std::process::Command::new("cmd")
            .args(["/c", "start", "", scheme])
            .spawn()
            .is_ok()
        {
            return Ok(());
        }
        tried.push(scheme.into());
    }

    Err(format!(
        "Couldn't find the Minecraft launcher. Open it from the Start Menu yourself. Tried: {}",
        tried.join("; ")
    ))
}

// Pick a sensible -Xmx based on installed RAM. Default: half of system
// memory, capped at 8 GB and floored at 2 GB. A profile may set
// `java_xmx_gb` to request more (e.g. heavy modpacks), capped at
// total_ram - 2 GB to leave the OS room.
// Scale Java max heap to host memory:
//   <4 GB host  → 2 GB Java
//   4-7 GB      → half of host
//   8-15 GB     → 6 GB
//   16-31 GB    → 8 GB
//   32+ GB      → 12 GB
// Profile may request more via java_xmx_gb, capped at host_total - 2 GB
// (so the OS isn't starved). Modded MC rarely benefits past ~12 GB — large
// heaps actually hurt GC pause times.
fn calculate_java_xmx_gb(profile_override: Option<u32>) -> u32 {
    let mut sys = System::new_all();
    sys.refresh_memory();
    let total_gb = (sys.total_memory() / 1024 / 1024 / 1024) as u32;
    let safe_max = total_gb.saturating_sub(2).max(2);

    let target = profile_override.unwrap_or_else(|| {
        if total_gb >= 32 {
            12
        } else if total_gb >= 16 {
            8
        } else if total_gb >= 8 {
            6
        } else if total_gb >= 4 {
            (total_gb / 2).max(2)
        } else {
            2
        }
    });
    target.clamp(2, safe_max)
}

fn java_args_for_minecraft(profile_override: Option<u32>) -> String {
    let xmx = calculate_java_xmx_gb(profile_override);
    format!(
        "-Xmx{xmx}G -Xms{xmx}G -XX:+UnlockExperimentalVMOptions -XX:+UseG1GC \
         -XX:G1NewSizePercent=20 -XX:G1ReservePercent=20 -XX:MaxGCPauseMillis=50 \
         -XX:G1HeapRegionSize=32M"
    )
}

fn emit_progress(app: &AppHandle, message: impl Into<String>, level: &str) {
    let _ = app.emit(
        "install-progress",
        json!({ "message": message.into(), "level": level }),
    );
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .expect("reqwest client")
}

async fn fabric_pick_loader_version(
    client: &reqwest::Client,
    mc_version: &str,
) -> Result<String, String> {
    let url = format!("{FABRIC_META}/versions/loader/{mc_version}");
    let resp: Vec<Value> = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("fabric meta: {e}"))?
        .json()
        .await
        .map_err(|e| format!("fabric meta json: {e}"))?;
    for entry in &resp {
        let stable = entry
            .get("loader")
            .and_then(|l| l.get("stable"))
            .and_then(|s| s.as_bool())
            .unwrap_or(false);
        if stable {
            if let Some(ver) = entry
                .get("loader")
                .and_then(|l| l.get("version"))
                .and_then(|v| v.as_str())
            {
                return Ok(ver.to_string());
            }
        }
    }
    resp.first()
        .and_then(|e| e.get("loader"))
        .and_then(|l| l.get("version"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("no Fabric loader for MC {mc_version}"))
}

async fn fabric_fetch_profile_json(
    client: &reqwest::Client,
    mc_version: &str,
    loader_version: &str,
) -> Result<Value, String> {
    let url =
        format!("{FABRIC_META}/versions/loader/{mc_version}/{loader_version}/profile/json");
    client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("fabric profile: {e}"))?
        .json::<Value>()
        .await
        .map_err(|e| format!("fabric profile json: {e}"))
}

// NeoForge version scheme: for MC 1.X.Y the NeoForge prefix is "X.Y." — e.g.
// MC 1.21.1 -> 21.1.123. MC versions without a patch (1.21) map to "X.0.".
fn neoforge_prefix_for_mc(mc_version: &str) -> Option<String> {
    let mut parts = mc_version.split('.');
    if parts.next()? != "1" {
        return None;
    }
    let major = parts.next()?;
    let minor = parts.next().unwrap_or("0");
    Some(format!("{major}.{minor}."))
}

async fn neoforge_pick_version(
    client: &reqwest::Client,
    mc_version: &str,
) -> Result<String, String> {
    let prefix = neoforge_prefix_for_mc(mc_version)
        .ok_or_else(|| format!("Can't map MC {mc_version} to a NeoForge version"))?;
    let resp: Value = client
        .get(NEOFORGE_VERSIONS_API)
        .send()
        .await
        .map_err(|e| format!("neoforge versions: {e}"))?
        .json()
        .await
        .map_err(|e| format!("neoforge versions json: {e}"))?;
    let versions = resp
        .get("versions")
        .and_then(|v| v.as_array())
        .ok_or("neoforge versions response missing 'versions' array")?;
    // The maven API returns oldest-first; iterate in reverse for newest-first.
    // Prefer stable releases (no -beta / -rc suffix) for the kid-friendly path.
    let mut newest_stable: Option<&str> = None;
    let mut newest_any: Option<&str> = None;
    for v in versions.iter().rev().filter_map(|x| x.as_str()) {
        if !v.starts_with(&prefix) {
            continue;
        }
        if newest_any.is_none() {
            newest_any = Some(v);
        }
        let lower = v.to_ascii_lowercase();
        if !lower.contains("beta") && !lower.contains("rc") && !lower.contains("alpha") {
            newest_stable = Some(v);
            break;
        }
    }
    newest_stable
        .or(newest_any)
        .map(String::from)
        .ok_or_else(|| format!("No NeoForge release for MC {mc_version}"))
}

// Walk versions/ for a folder name that looks like the NeoForge install. Both
// `neoforge-21.1.123` and `1.21.1-neoforge-21.1.123` formats have shipped over
// time; we accept either. The version_id is what goes into launcher_profiles.
fn find_neoforge_version_id(mc_dir: &Path, nf_version: &str) -> Option<String> {
    let versions_dir = mc_dir.join("versions");
    let exact = format!("neoforge-{nf_version}");
    if versions_dir.join(&exact).join(format!("{exact}.json")).exists() {
        return Some(exact);
    }
    let rd = fs::read_dir(&versions_dir).ok()?;
    for entry in rd.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.contains(nf_version) && name.to_ascii_lowercase().contains("neoforge") {
            let json = entry.path().join(format!("{name}.json"));
            if json.exists() {
                return Some(name);
            }
        }
    }
    None
}

async fn install_neoforge(
    app: &AppHandle,
    client: &reqwest::Client,
    mc_version: &str,
    mc_dir: &Path,
    // For modpacks: install the EXACT NeoForge version the .mrpack manifest
    // demands. None = pick the latest stable for this MC version (cobblemon
    // path).
    nf_version_override: Option<&str>,
) -> Result<(String, String), String> {
    let java = find_java().ok_or_else(|| {
        "Couldn't find Java. Open the Minecraft launcher once so it installs its bundled \
         Java runtime, then try again. (NeoForge requires Java to run its installer.)"
            .to_string()
    })?;
    emit_progress(app, format!("Java: {}", java.display()), "info");

    let nf_version = match nf_version_override {
        Some(v) => {
            emit_progress(app, format!("NeoForge {v} (from modpack manifest)"), "ok");
            v.to_string()
        }
        None => {
            emit_progress(app, "Resolving NeoForge version...", "info");
            let v = neoforge_pick_version(client, mc_version).await?;
            emit_progress(app, format!("NeoForge {v}"), "ok");
            v
        }
    };

    let installer_url =
        format!("{NEOFORGE_MAVEN_BASE}/{nf_version}/neoforge-{nf_version}-installer.jar");
    let temp_dir = std::env::temp_dir().join("mmle5-neoforge");
    fs::create_dir_all(&temp_dir).map_err(|e| format!("mkdir temp: {e}"))?;
    let installer = temp_dir.join(format!("neoforge-{nf_version}-installer.jar"));
    emit_progress(app, "Downloading NeoForge installer...", "info");
    download_to(client, &installer_url, &installer).await?;

    // The installer needs a launcher_profiles.json to upsert into. On a fresh
    // install the file may not exist yet — make sure it does, with the minimum
    // structure the installer expects.
    let lp_path = mc_dir.join("launcher_profiles.json");
    if !lp_path.exists() {
        let stub = json!({ "profiles": {}, "settings": {}, "version": 3 });
        let pretty = serde_json::to_vec_pretty(&stub).map_err(|e| e.to_string())?;
        fs::write(&lp_path, pretty)
            .map_err(|e| format!("write {}: {e}", lp_path.display()))?;
    }

    emit_progress(
        app,
        "Running NeoForge installer (this can take a minute)...",
        "info",
    );
    let mc_dir_str = mc_dir.to_string_lossy().to_string();
    let java_clone = java.clone();
    let installer_clone = installer.clone();
    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new(&java_clone)
            .arg("-jar")
            .arg(&installer_clone)
            .arg("--installClient")
            .arg(&mc_dir_str)
            .output()
    })
    .await
    .map_err(|e| format!("spawn java: {e}"))?
    .map_err(|e| format!("run java: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        // Show last few lines of each — installer logs are very chatty.
        let tail = |s: &str| {
            let lines: Vec<&str> = s.lines().collect();
            let start = lines.len().saturating_sub(20);
            lines[start..].join("\n")
        };
        return Err(format!(
            "NeoForge installer failed (exit {:?}).\nstdout (tail):\n{}\nstderr (tail):\n{}",
            output.status.code(),
            tail(&stdout),
            tail(&stderr)
        ));
    }

    let _ = fs::remove_file(&installer);

    let version_id = find_neoforge_version_id(mc_dir, &nf_version)
        .ok_or_else(|| format!("NeoForge installer ran but no versions/* entry was created for {nf_version}"))?;
    Ok((nf_version, version_id))
}

// Install a Modrinth-format modpack (.mrpack). Downloads the pack, extracts
// the manifest, installs the loader the manifest demands, fetches every
// file in the manifest's files[] list to its declared path, and copies
// overrides/ + client-overrides/ over the .minecraft/ tree.
async fn install_modpack_profile(
    app: &AppHandle,
    client: &reqwest::Client,
    mc_dir: &Path,
    profile: &Profile,
    modpack_slug: &str,
) -> Result<InstallReport, String> {
    emit_progress(app, format!("Resolving modpack {modpack_slug}..."), "info");

    // Get latest version of the modpack from Modrinth
    let url = format!("{MODRINTH_API}/project/{modpack_slug}/version");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("modpack versions: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("modpack {modpack_slug}: HTTP {}", resp.status()));
    }
    let versions: Vec<ModrinthVersion> = resp
        .json()
        .await
        .map_err(|e| format!("modpack json: {e}"))?;
    let version = versions
        .into_iter()
        .next()
        .ok_or_else(|| format!("no versions for modpack {modpack_slug}"))?;
    let pack_file = version
        .files
        .iter()
        .find(|f| f.primary)
        .or_else(|| version.files.first())
        .ok_or_else(|| format!("no files for modpack {modpack_slug}"))?;

    // Download .mrpack to temp
    let temp_dir = std::env::temp_dir().join("mmle5-modpack");
    fs::create_dir_all(&temp_dir).map_err(|e| format!("mkdir temp: {e}"))?;
    let mrpack_path = temp_dir.join(&pack_file.filename);
    if !mrpack_path.exists() {
        emit_progress(
            app,
            format!("Downloading modpack {} ...", pack_file.filename),
            "info",
        );
        download_to(client, &pack_file.url, &mrpack_path).await?;
    } else {
        emit_progress(
            app,
            format!("Reusing already-downloaded {}", pack_file.filename),
            "info",
        );
    }

    // Open as zip + read manifest
    let zip_file = std::fs::File::open(&mrpack_path)
        .map_err(|e| format!("open {}: {e}", mrpack_path.display()))?;
    let mut archive = zip::ZipArchive::new(zip_file)
        .map_err(|e| format!("read mrpack zip: {e}"))?;

    let manifest: MrpackIndex = {
        let mut entry = archive
            .by_name("modrinth.index.json")
            .map_err(|e| format!("manifest: {e}"))?;
        let mut buf = String::new();
        entry
            .read_to_string(&mut buf)
            .map_err(|e| format!("read manifest: {e}"))?;
        serde_json::from_str(&buf).map_err(|e| format!("parse manifest: {e}"))?
    };

    let mc_version = manifest
        .dependencies
        .get("minecraft")
        .cloned()
        .ok_or("modpack manifest missing minecraft dep")?;

    let (loader_name, loader_version_from_manifest) = if let Some(v) = manifest.dependencies.get("fabric-loader") {
        ("fabric".to_string(), v.clone())
    } else if let Some(v) = manifest.dependencies.get("neoforge") {
        ("neoforge".to_string(), v.clone())
    } else if let Some(v) = manifest.dependencies.get("quilt-loader") {
        ("quilt".to_string(), v.clone())
    } else if let Some(v) = manifest.dependencies.get("forge") {
        ("forge".to_string(), v.clone())
    } else {
        return Err(format!(
            "modpack manifest specifies no supported loader (deps: {:?})",
            manifest.dependencies.keys().collect::<Vec<_>>()
        ));
    };

    emit_progress(
        app,
        format!(
            "Modpack '{}' for MC {} ({} {})",
            manifest.name, mc_version, loader_name, loader_version_from_manifest
        ),
        "ok",
    );

    // Install the EXACT loader version the manifest demands. The previous
    // fall-back-to-latest-stable behavior caused boot-loops because modpacks
    // routinely require newer Fabric loaders (0.16+/0.18+) than what Fabric
    // marks "stable" for older MC versions, and an existing-on-disk install
    // from a previous profile was usually too old. Use what the pack asked
    // for; only reuse on disk if the version_id matches exactly.
    let (loader_version, version_id) = match loader_name.as_str() {
        "fabric" => {
            let target_lv = loader_version_from_manifest.clone();
            let target_vid = format!("fabric-loader-{target_lv}-{mc_version}");
            let target_json = mc_dir
                .join("versions")
                .join(&target_vid)
                .join(format!("{target_vid}.json"));
            if target_json.exists() {
                emit_progress(
                    app,
                    format!("Using existing Fabric loader {target_lv} (matches manifest)"),
                    "ok",
                );
                (target_lv, target_vid)
            } else {
                emit_progress(
                    app,
                    format!("Installing Fabric loader {target_lv} for MC {mc_version} (from manifest)..."),
                    "info",
                );
                let pj = fabric_fetch_profile_json(client, &mc_version, &target_lv).await?;
                let vid = pj
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or("fabric profile json missing 'id'")?
                    .to_string();
                write_fabric_version_files(mc_dir, &vid, &pj)?;
                (target_lv, vid)
            }
        }
        "neoforge" => {
            let target_nf = loader_version_from_manifest.clone();
            let target_vid = format!("neoforge-{target_nf}");
            let target_json = mc_dir
                .join("versions")
                .join(&target_vid)
                .join(format!("{target_vid}.json"));
            if target_json.exists() {
                emit_progress(
                    app,
                    format!("Using existing NeoForge {target_nf} (matches manifest)"),
                    "ok",
                );
                (target_nf, target_vid)
            } else {
                install_neoforge(app, client, &mc_version, mc_dir, Some(&target_nf)).await?
            }
        }
        other => {
            return Err(format!(
                "modpack uses unsupported loader: {other} (only fabric and neoforge work)"
            ));
        }
    };

    // Backup existing mods so the kid doesn't end up with stale jars.
    // Prime the cache *before* the backup move so any jars on disk
    // (including ones from the previous profile) become cache-resident
    // and the manifest install loop hits cache instead of re-downloading.
    let mods_dir = mc_dir.join("mods");
    fs::create_dir_all(&mods_dir).map_err(|e| format!("mkdir mods: {e}"))?;
    let _ = prime_cache_from_disk(app, mc_dir).await;
    backup_existing_mods(app, &mods_dir)?;

    // Download all files listed in manifest to their declared paths
    let mut installed = 0u32;
    let mut skipped: Vec<String> = Vec::new();
    for f in &manifest.files {
        // Skip server-only files for client install
        let env_client = f
            .env
            .as_ref()
            .and_then(|e| e.get("client"))
            .map(|s| s.as_str());
        if env_client == Some("unsupported") {
            continue;
        }
        let dest = mc_dir.join(&f.path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        let dl_url = match f.downloads.first() {
            Some(u) => u.clone(),
            None => {
                skipped.push(format!("{} (no download URL)", f.path));
                continue;
            }
        };
        match download_to_if_missing(app, client, &dl_url, &dest, f.hashes.sha1.as_deref()).await {
            Ok(_) => installed += 1,
            Err(e) => {
                skipped.push(format!("{} ({e})", f.path));
                emit_progress(app, format!("Skip {}: {e}", f.path), "warn");
            }
        }
    }

    // Extract overrides/ and client-overrides/ to .minecraft/
    let mut overrides_extracted = 0u32;
    let prefixes = ["overrides/", "client-overrides/"];
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("zip entry {i}: {e}"))?;
        let entry_name = entry.name().to_string();
        let rel = match prefixes
            .iter()
            .find_map(|p| entry_name.strip_prefix(p).map(|s| s.to_string()))
        {
            Some(r) => r,
            None => continue,
        };
        if rel.is_empty() || rel.ends_with('/') {
            continue;
        }
        let dest = mc_dir.join(&rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir override parent: {e}"))?;
        }
        let mut out = std::fs::File::create(&dest)
            .map_err(|e| format!("create override {}: {e}", dest.display()))?;
        std::io::copy(&mut entry, &mut out)
            .map_err(|e| format!("copy override {}: {e}", dest.display()))?;
        overrides_extracted += 1;
    }
    emit_progress(
        app,
        format!("Extracted {overrides_extracted} override file(s)"),
        "ok",
    );

    // Drop in any supplementary jars the .mrpack couldn't ship (typically
    // CurseForge-only mods whose authors don't allow Modrinth redistribution
    // but the pack's own mods declare as a hard dependency). Pinned by direct
    // forgecdn URL so no API auth is needed.
    if !profile.modpack_supplements.is_empty() {
        emit_progress(
            app,
            format!(
                "Installing {} supplemental mod(s)...",
                profile.modpack_supplements.len()
            ),
            "info",
        );
        for sup in &profile.modpack_supplements {
            let display = ref_display_name(&sup.source, &sup.slug, &sup.filename);
            match sup.source.as_str() {
                "url" | "planetminecraft" | "curseforge_url" => {
                    let url = match &sup.url {
                        Some(u) => u,
                        None => {
                            skipped.push(format!("supplement {display} (missing url)"));
                            continue;
                        }
                    };
                    let filename = match &sup.filename {
                        Some(f) => f,
                        None => {
                            skipped.push(format!("supplement {display} (missing filename)"));
                            continue;
                        }
                    };
                    let dest = mods_dir.join(filename);
                    match download_to_if_missing(app, client, url, &dest, None).await {
                        Ok(_) => installed += 1,
                        Err(e) => {
                            skipped.push(format!("supplement {filename} ({e})"));
                            emit_progress(app, format!("Skip supplement {filename}: {e}"), "warn");
                        }
                    }
                }
                "curseforge" => {
                    let file_id = match sup.file_id {
                        Some(id) => id,
                        None => {
                            skipped.push(format!("supplement {display} (missing file_id)"));
                            continue;
                        }
                    };
                    let filename = match &sup.filename {
                        Some(f) => f,
                        None => {
                            skipped.push(format!("supplement {display} (missing filename)"));
                            continue;
                        }
                    };
                    let url = curseforge_cdn_url(file_id, filename);
                    let dest = mods_dir.join(filename);
                    match download_to_if_missing(app, client, &url, &dest, None).await {
                        Ok(_) => installed += 1,
                        Err(e) => {
                            skipped.push(format!("supplement {filename} ({e})"));
                            emit_progress(app, format!("Skip supplement {filename}: {e}"), "warn");
                        }
                    }
                }
                other => {
                    skipped.push(format!("supplement {display} (unsupported source {other})"));
                }
            }
        }
    }

    // Launcher profile + JVM args + windowed default
    upsert_options_txt_setting(mc_dir, "fullscreen", "false")?;
    let java_args = java_args_for_minecraft(profile.java_xmx_gb);
    emit_progress(app, format!("Java args (auto-tuned): {java_args}"), "info");
    let launcher_profile_id = format!("mmle5-{}", profile.id);
    upsert_launcher_profile(
        mc_dir,
        &launcher_profile_id,
        &profile.name,
        &version_id,
        Some(&java_args),
        profile.icon.as_deref(),
    )?;

    let _ = fs::remove_file(&mrpack_path);

    Ok(InstallReport {
        profile_name: profile.name.clone(),
        minecraft_version: mc_version,
        loader_version,
        mods_installed: installed,
        shaders_installed: 0,
        resource_packs_installed: 0,
        skipped,
    })
}

async fn modrinth_pick_version(
    client: &reqwest::Client,
    slug: &str,
    mc_version: &str,
    loader: &str,
) -> Result<Option<ModrinthVersion>, String> {
    let game_versions_json = format!("[\"{}\"]", mc_version);
    let loaders_json = format!("[\"{}\"]", loader);
    let url = format!("{MODRINTH_API}/project/{slug}/version");
    let resp = client
        .get(&url)
        .query(&[
            ("game_versions", game_versions_json.as_str()),
            ("loaders", loaders_json.as_str()),
        ])
        .send()
        .await
        .map_err(|e| format!("modrinth versions {slug}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("modrinth {slug}: HTTP {}", resp.status()));
    }
    let versions: Vec<ModrinthVersion> = resp
        .json()
        .await
        .map_err(|e| format!("modrinth versions {slug} json: {e}"))?;
    Ok(pick_best_version(versions, mc_version))
}

// Modrinth returns versions newest-first. A mod's "newest 1.21.9-compatible"
// version may actually be tagged primarily for 1.21.10 (e.g.,
// "0.138.3+1.21.10-fabric") and only loosely overlap into 1.21.9 — but its
// dependencies are pinned to the 1.21.10 ecosystem. Prefer versions whose
// version_number explicitly references our pinned MC version.
fn pick_best_version(versions: Vec<ModrinthVersion>, mc_version: &str) -> Option<ModrinthVersion> {
    let mut versions = versions;
    if let Some(idx) = versions
        .iter()
        .position(|v| v.version_number.contains(mc_version))
    {
        return Some(versions.swap_remove(idx));
    }
    versions.into_iter().next()
}

async fn modrinth_pick_shader_version(
    client: &reqwest::Client,
    slug: &str,
    mc_version: &str,
) -> Result<Option<ModrinthVersion>, String> {
    let game_versions_json = format!("[\"{}\"]", mc_version);
    let url = format!("{MODRINTH_API}/project/{slug}/version");
    let resp = client
        .get(&url)
        .query(&[("game_versions", game_versions_json.as_str())])
        .send()
        .await
        .map_err(|e| format!("modrinth shader {slug}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("modrinth {slug}: HTTP {}", resp.status()));
    }
    let versions: Vec<ModrinthVersion> = resp
        .json()
        .await
        .map_err(|e| format!("modrinth shader {slug} json: {e}"))?;
    Ok(pick_best_version(versions, mc_version))
}

// Downloads `url` to `dest` only if `dest` doesn't already exist. If
// `expected_sha1` is provided:
//   - existing file: re-hashed; on mismatch, redownloaded
//   - downloaded file: verified after write; on mismatch, file removed and
//     the call returns Err so the caller knows to skip / report
// Logs the outcome (already-present vs downloaded vs verified) so the
// install report shows what was reused vs fetched.
// Sha1-keyed content-addressed cache for mod jars. Lives in app local data,
// shared across all installs and across all profiles, so reinstalling a
// modpack (which `backup_existing_mods` empties out) skips the network on
// every file the manifest already pinned by sha1.
fn mod_cache_dir(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_local_data_dir()
        .ok()
        .map(|d| d.join("mod-cache"))
}

// Walk known on-disk locations for jars/zips and pull any missing ones into
// the sha1-keyed cache. Called once at install start. Cheap if cache is
// already primed (hash + skip-if-present); valuable on first install with
// the cache build, or after switching from a profile that downloaded files
// the new profile also needs. Best-effort throughout — IO errors per file
// are silently skipped so a busted file in mods.backup-* never blocks an
// install.
async fn prime_cache_from_disk(app: &AppHandle, mc_dir: &Path) -> u32 {
    let cache_dir = match mod_cache_dir(app) {
        Some(d) => d,
        None => return 0,
    };
    if fs::create_dir_all(&cache_dir).is_err() {
        return 0;
    }

    let mut sources: Vec<PathBuf> = vec![
        mc_dir.join("mods"),
        mc_dir.join("shaderpacks"),
        mc_dir.join("resourcepacks"),
    ];
    if let Ok(rd) = fs::read_dir(mc_dir) {
        for entry in rd.filter_map(|e| e.ok()) {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("mods.backup-") || name.starts_with("mods.backup.") {
                sources.push(entry.path());
            }
        }
    }

    let mut imported = 0u32;
    for src_dir in sources {
        let Ok(rd) = fs::read_dir(&src_dir) else { continue };
        for entry in rd.filter_map(|e| e.ok()) {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let ext_ok = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| {
                    let l = e.to_ascii_lowercase();
                    l == "jar" || l == "zip"
                })
                .unwrap_or(false);
            if !ext_ok {
                continue;
            }
            let sha1 = match hash_sha1_of(&path).await {
                Ok(s) => s,
                Err(_) => continue,
            };
            let cache_file = cache_dir.join(format!("{sha1}.bin"));
            if cache_file.exists() {
                continue;
            }
            if fs::copy(&path, &cache_file).is_ok() {
                imported += 1;
            }
        }
    }
    if imported > 0 {
        emit_progress(
            app,
            format!("Primed cache with {imported} existing file(s) from .minecraft"),
            "info",
        );
    }
    imported
}

// CurseForge's auth-free CDN serves files at /files/<file_id/1000>/<file_id%1000>/<filename>
// where the trailing chunk is zero-padded to 3 digits. So file 4926069 lives
// at /files/4926/069/, and 6402485 at /files/6402/485/. Older 6-digit IDs
// (e.g. 999999) work the same: 999/999/.
fn curseforge_cdn_url(file_id: u64, filename: &str) -> String {
    let high = file_id / 1000;
    let low = file_id % 1000;
    format!("https://edge.forgecdn.net/files/{high}/{low:03}/{filename}")
}

fn copy_no_clobber_replace(src: &Path, dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    fs::copy(src, dest).map_err(|e| format!("copy {}: {e}", dest.display()))?;
    Ok(())
}

// Cache key for `download_to_if_missing`. Prefer the manifest-pinned sha1
// (stable across URL changes, dedupes content shared by multiple profiles).
// Falls back to sha1(url) when there's no expected hash — this lets
// supplements (where YAML has no sha1) and any other no-hash callers benefit
// from caching too. Different keyspace; will never collide with content
// hashes in practice.
fn cache_key_for(expected_sha1: Option<&str>, url: &str) -> String {
    if let Some(s) = expected_sha1 {
        return s.to_ascii_lowercase();
    }
    let mut hasher = Sha1::new();
    hasher.update(b"url:");
    hasher.update(url.as_bytes());
    hex::encode(hasher.finalize())
}

async fn download_to_if_missing(
    app: &AppHandle,
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected_sha1: Option<&str>,
) -> Result<bool, String> {
    let filename = dest
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("?")
        .to_string();

    if dest.exists() {
        if let Some(expected) = expected_sha1 {
            match hash_sha1_of(dest).await {
                Ok(actual) if actual.eq_ignore_ascii_case(expected) => {
                    emit_progress(
                        app,
                        format!("Already present (sha1 matches): {filename}"),
                        "info",
                    );
                    return Ok(false);
                }
                Ok(actual) => {
                    emit_progress(
                        app,
                        format!(
                            "{filename} on disk has different content (got {}, expected {}); re-downloading",
                            &actual[..8.min(actual.len())],
                            &expected[..8.min(expected.len())]
                        ),
                        "warn",
                    );
                    let _ = fs::remove_file(dest);
                }
                Err(e) => {
                    emit_progress(
                        app,
                        format!("Couldn't hash existing {filename}: {e}; re-downloading"),
                        "warn",
                    );
                    let _ = fs::remove_file(dest);
                }
            }
        } else {
            emit_progress(
                app,
                format!("Already present, skipping: {filename}"),
                "info",
            );
            return Ok(false);
        }
    }

    // Cache lookup. For sha1-keyed entries we verify after copy (guards
    // against torn writes / disk rot). For url-keyed entries we trust the
    // cache: there's no integrity reference to check against, and corruption
    // there is a local-disk problem the user can resolve by clearing the
    // cache dir.
    let cache_key = cache_key_for(expected_sha1, url);
    if let Some(cache) = mod_cache_dir(app) {
        let cache_file = cache.join(format!("{cache_key}.bin"));
        if cache_file.exists() {
            copy_no_clobber_replace(&cache_file, dest)?;
            if let Some(expected) = expected_sha1 {
                match hash_sha1_of(dest).await {
                    Ok(actual) if actual.eq_ignore_ascii_case(expected) => {
                        emit_progress(app, format!("From cache: {filename}"), "info");
                        return Ok(true);
                    }
                    _ => {
                        let _ = fs::remove_file(&cache_file);
                        let _ = fs::remove_file(dest);
                    }
                }
            } else {
                emit_progress(app, format!("From cache: {filename}"), "info");
                return Ok(true);
            }
        }
    }

    emit_progress(app, format!("Downloading {filename}"), "info");
    download_to(client, url, dest).await?;

    if let Some(expected) = expected_sha1 {
        let actual = hash_sha1_of(dest).await?;
        if !actual.eq_ignore_ascii_case(expected) {
            let _ = fs::remove_file(dest);
            return Err(format!(
                "sha1 mismatch on {filename} (got {}, expected {})",
                &actual[..8.min(actual.len())],
                &expected[..8.min(expected.len())]
            ));
        }
    }
    // Save to cache (sha1-keyed or url-keyed, same code path now).
    // Best-effort — failures here shouldn't fail install.
    if let Some(cache) = mod_cache_dir(app) {
        if fs::create_dir_all(&cache).is_ok() {
            let cache_file = cache.join(format!("{cache_key}.bin"));
            let _ = fs::copy(dest, &cache_file);
        }
    }
    Ok(true)
}

async fn download_to(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
) -> Result<(), String> {
    let bytes = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("download {url}: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("download {url} body: {e}"))?;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    fs::write(dest, &bytes).map_err(|e| format!("write {}: {e}", dest.display()))?;
    Ok(())
}

// Look in .minecraft/versions/ for a directory that matches the requested
// loader + MC version. Used to detect an existing NeoForge / Forge / Quilt
// install so we can layer mods onto it without re-running the installer.
//
// Naming conventions:
//   fabric-loader-<loader>-<mc>           e.g. fabric-loader-0.16.10-1.21.9
//   quilt-loader-<loader>-<mc>            e.g. quilt-loader-0.27.0-1.21.1
//   neoforge-<neoforge-ver>               e.g. neoforge-21.1.95         (mc inferred via NeoForge version family)
//   forge-<mc>-<forge-ver>                e.g. forge-1.21.1-52.0.40
fn detect_existing_loader_version(
    mc_dir: &Path,
    loader: &str,
    mc_version: &str,
) -> Option<String> {
    let versions_dir = mc_dir.join("versions");
    if !versions_dir.exists() {
        return None;
    }
    let entries: Vec<String> = fs::read_dir(&versions_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();

    let mut matches: Vec<String> = entries
        .into_iter()
        .filter(|name| {
            let lower = name.to_lowercase();
            match loader {
                "fabric" => {
                    lower.starts_with("fabric-loader-")
                        && name.ends_with(&format!("-{mc_version}"))
                }
                "quilt" => {
                    lower.starts_with("quilt-loader-")
                        && name.ends_with(&format!("-{mc_version}"))
                }
                // NeoForge directories don't include MC version in name; we
                // accept any neoforge-* and let the launcher_profiles.json
                // record do the binding. This is a soft check.
                "neoforge" => lower.starts_with("neoforge-"),
                "forge" => lower.starts_with(&format!("forge-{mc_version}-")),
                _ => false,
            }
        })
        .collect();
    matches.sort();
    matches.into_iter().next_back()
}

fn write_fabric_version_files(
    mc_dir: &Path,
    version_id: &str,
    profile_json: &Value,
) -> Result<(), String> {
    let dir = mc_dir.join("versions").join(version_id);
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let json_path = dir.join(format!("{version_id}.json"));
    let pretty = serde_json::to_vec_pretty(profile_json).map_err(|e| e.to_string())?;
    fs::write(&json_path, pretty)
        .map_err(|e| format!("write {}: {e}", json_path.display()))?;
    let jar_path = dir.join(format!("{version_id}.jar"));
    if !jar_path.exists() {
        // Fabric profile inheritsFrom the vanilla client, but the launcher
        // expects a (possibly empty) jar to exist alongside the JSON.
        fs::write(&jar_path, [])
            .map_err(|e| format!("write {}: {e}", jar_path.display()))?;
    }
    Ok(())
}

// Set a single key in options.txt to a value, overwriting any existing line
// for that key. Used for simple settings like `fullscreen:false`.
fn upsert_options_txt_setting(mc_dir: &Path, key: &str, value: &str) -> Result<(), String> {
    let path = mc_dir.join("options.txt");
    let content = fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<String> = if content.is_empty() {
        Vec::new()
    } else {
        content.lines().map(String::from).collect()
    };
    let prefix = format!("{key}:");
    let mut found = false;
    for line in lines.iter_mut() {
        if line.starts_with(&prefix) {
            *line = format!("{key}:{value}");
            found = true;
            break;
        }
    }
    if !found {
        lines.push(format!("{key}:{value}"));
    }
    let new_content = if lines.is_empty() {
        String::new()
    } else {
        lines.join("\n") + "\n"
    };
    fs::write(&path, new_content).map_err(|e| format!("write options.txt: {e}"))?;
    Ok(())
}

// options.txt holds the resourcePacks setting as a JSON-encoded array string,
// e.g. resourcePacks:["vanilla","fabric","file/Faithful 32x.zip"]. Append our
// pack as "file/<filename>" if not already present, preserving the rest of
// the user's settings.
fn enable_resource_pack(mc_dir: &Path, filename: &str) -> Result<(), String> {
    let path = mc_dir.join("options.txt");
    let entry = format!("file/{filename}");
    let content = fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<String> = if content.is_empty() {
        Vec::new()
    } else {
        content.lines().map(String::from).collect()
    };
    let mut found = false;
    for line in lines.iter_mut() {
        if line.starts_with("resourcePacks:") {
            let json_part = &line["resourcePacks:".len()..];
            let mut packs: Vec<String> = serde_json::from_str(json_part)
                .unwrap_or_else(|_| vec!["vanilla".to_string()]);
            if !packs.iter().any(|p| p == &entry) {
                packs.push(entry.clone());
            }
            let serialized = serde_json::to_string(&packs).map_err(|e| e.to_string())?;
            *line = format!("resourcePacks:{serialized}");
            found = true;
            break;
        }
    }
    if !found {
        let packs = vec!["vanilla".to_string(), entry.clone()];
        let serialized = serde_json::to_string(&packs).map_err(|e| e.to_string())?;
        lines.push(format!("resourcePacks:{serialized}"));
    }
    let new_content = if lines.is_empty() {
        String::new()
    } else {
        lines.join("\n") + "\n"
    };
    fs::write(&path, new_content).map_err(|e| format!("write options.txt: {e}"))?;
    Ok(())
}

// Iris reads its enabled-shader pick from .minecraft/config/iris.properties.
// Without this file (or this key) the kid has to open Video Settings →
// Shader Packs → click → Apply just to see the shader they downloaded. We
// pre-select the profile's default shader so the first launch already looks
// pretty.
fn set_iris_default_shader(mc_dir: &Path, filename: &str) -> Result<(), String> {
    let config_dir = mc_dir.join("config");
    fs::create_dir_all(&config_dir).map_err(|e| format!("mkdir config: {e}"))?;
    let path = config_dir.join("iris.properties");
    let mut props: BTreeMap<String, String> = BTreeMap::new();
    if let Ok(existing) = fs::read_to_string(&path) {
        for line in existing.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Some(eq) = trimmed.find('=') {
                let k = trimmed[..eq].trim().to_string();
                let v = trimmed[eq + 1..].trim().to_string();
                if !k.is_empty() {
                    props.insert(k, v);
                }
            }
        }
    }
    props.insert("shaderPack".to_string(), filename.to_string());
    props.insert("enableShaders".to_string(), "true".to_string());
    let mut content = String::new();
    for (k, v) in &props {
        content.push_str(&format!("{k}={v}\n"));
    }
    fs::write(&path, content).map_err(|e| format!("write iris.properties: {e}"))?;
    Ok(())
}

// Existing jars in mods/ from previous installs cause Fabric loader errors
// (wrong MC version, conflicting Fabric API copies, OptiFabric vs Sodium, ...).
// Move them aside before installing our curated set so the kid gets a clean
// known-good environment. The user can recover anything from the timestamped
// backup if they cared about it.
fn backup_existing_mods(app: &AppHandle, mods_dir: &Path) -> Result<(), String> {
    let entries: Vec<_> = match fs::read_dir(mods_dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(_) => return Ok(()),
    };
    let jars: Vec<_> = entries
        .into_iter()
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| x.eq_ignore_ascii_case("jar"))
                .unwrap_or(false)
        })
        .collect();
    if jars.is_empty() {
        return Ok(());
    }
    let stamp = rfc3339_now().replace(':', "-");
    let backup_dir = mods_dir
        .parent()
        .ok_or("mods dir has no parent")?
        .join(format!("mods.backup-{stamp}"));
    fs::create_dir_all(&backup_dir)
        .map_err(|e| format!("mkdir {}: {e}", backup_dir.display()))?;
    for jar in &jars {
        let src = jar.path();
        let dest = backup_dir.join(jar.file_name());
        fs::rename(&src, &dest)
            .map_err(|e| format!("move {} -> {}: {e}", src.display(), dest.display()))?;
    }
    emit_progress(
        app,
        format!(
            "Moved {} existing mod{} to {}",
            jars.len(),
            if jars.len() == 1 { "" } else { "s" },
            backup_dir.file_name().and_then(|n| n.to_str()).unwrap_or("backup")
        ),
        "warn",
    );

    // Prune older backups — keep the 5 most recent.
    let parent = match mods_dir.parent() {
        Some(p) => p,
        None => return Ok(()),
    };
    let mut backups: Vec<PathBuf> = match fs::read_dir(parent) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.is_dir()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with("mods.backup-"))
                        .unwrap_or(false)
            })
            .collect(),
        Err(_) => return Ok(()),
    };
    if backups.len() <= 5 {
        return Ok(());
    }
    backups.sort_by_key(|p| p.file_name().map(|n| n.to_owned()));
    let to_remove = backups.len() - 5;
    for old in backups.iter().take(to_remove) {
        if let Err(e) = fs::remove_dir_all(old) {
            emit_progress(
                app,
                format!("Could not remove old backup {}: {e}", old.display()),
                "warn",
            );
        } else {
            emit_progress(
                app,
                format!(
                    "Pruned old backup: {}",
                    old.file_name().and_then(|n| n.to_str()).unwrap_or("?")
                ),
                "info",
            );
        }
    }
    Ok(())
}

fn upsert_launcher_profile(
    mc_dir: &Path,
    profile_id: &str,
    name: &str,
    version_id: &str,
    java_args: Option<&str>,
    icon: Option<&str>,
) -> Result<(), String> {
    let path = mc_dir.join("launcher_profiles.json");
    let mut root: Value = if path.exists() {
        let s =
            fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        serde_json::from_str(&s).map_err(|e| format!("parse {}: {e}", path.display()))?
    } else {
        json!({ "profiles": {}, "settings": {}, "version": 3 })
    };
    let now = rfc3339_now();
    // Accept a vanilla block name or a `data:image/png;base64,...` URI; the
    // launcher renders both. Empty/missing falls back to Crafting_Table.
    let resolved_icon = icon
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Crafting_Table");
    let mut entry = json!({
        "name": name,
        "type": "custom",
        "created": now,
        "lastUsed": now,
        "lastVersionId": version_id,
        "icon": resolved_icon,
    });
    if let Some(args) = java_args {
        entry
            .as_object_mut()
            .unwrap()
            .insert("javaArgs".to_string(), Value::String(args.to_string()));
    }
    // Default to a sane windowed size. Combined with fullscreen:false in
    // options.txt, this gives kids a movable window they can F11 if they
    // want fullscreen.
    entry
        .as_object_mut()
        .unwrap()
        .insert(
            "resolution".to_string(),
            json!({ "width": 1280, "height": 720 }),
        );
    let profiles = root
        .as_object_mut()
        .and_then(|m| m.get_mut("profiles"))
        .and_then(|p| p.as_object_mut())
        .ok_or("launcher_profiles.json missing 'profiles' object")?;
    profiles.insert(profile_id.to_string(), entry);
    let pretty = serde_json::to_vec_pretty(&root).map_err(|e| e.to_string())?;
    fs::write(&path, pretty).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

fn rfc3339_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, mo, d, h, mi, s) = epoch_to_ymdhms(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

fn epoch_to_ymdhms(mut secs: u64) -> (i32, u32, u32, u32, u32, u32) {
    let s = (secs % 60) as u32;
    secs /= 60;
    let mi = (secs % 60) as u32;
    secs /= 60;
    let h = (secs % 24) as u32;
    secs /= 24;
    let mut days = secs as i64;
    let mut y = 1970i32;
    loop {
        let dy = if is_leap(y) { 366 } else { 365 };
        if days >= dy {
            days -= dy;
            y += 1;
        } else {
            break;
        }
    }
    let mdays: [i64; 12] = [
        31,
        if is_leap(y) { 29 } else { 28 },
        31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
    ];
    let mut mo: u32 = 1;
    for &md in &mdays {
        if days >= md {
            days -= md;
            mo += 1;
        } else {
            break;
        }
    }
    (y, mo, (days + 1) as u32, h, mi, s)
}

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

#[tauri::command]
async fn install_profile(
    app: AppHandle,
    profile_id: String,
    minecraft_dir: Option<String>,
) -> Result<InstallReport, String> {
    let profiles = read_profiles(&app)?;
    let profile = profiles
        .into_iter()
        .find(|p| p.id == profile_id)
        .ok_or_else(|| format!("Unknown profile: {profile_id}"))?;

    if profile.not_implemented_in_phase_1 {
        return Err(format!(
            "'{}' is marked not-yet-supported in profiles.yaml.",
            profile.name
        ));
    }

    let mc_dir = match minecraft_dir.filter(|s| !s.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => default_minecraft_dir().ok_or("can't find .minecraft")?,
    };
    if !mc_dir.exists() {
        return Err(format!(
            "Folder doesn't exist: {}",
            mc_dir.display()
        ));
    }

    let client = http_client();

    // Modpack profile: download .mrpack, parse manifest, install loader
    // from manifest deps, fetch every file in the manifest, copy overrides.
    if let Some(mp) = profile.modpack.clone() {
        if mp.source != "modrinth-modpack" {
            return Err(format!(
                "Unsupported modpack source '{}' (expected modrinth-modpack)",
                mp.source
            ));
        }
        return install_modpack_profile(&app, &client, &mc_dir, &profile, &mp.slug).await;
    }

    let loader_name = profile
        .loader
        .clone()
        .ok_or("profile has no `loader` and no `modpack` field")?;
    if loader_name != "fabric" && loader_name != "neoforge" {
        return Err(format!(
            "Unsupported loader '{}' on profile '{}'.",
            loader_name, profile.name
        ));
    }
    let mc_version_raw = profile
        .minecraft_version
        .clone()
        .ok_or("profile has no `minecraft_version` and no `modpack` field")?;
    let mc_version = if mc_version_raw.eq_ignore_ascii_case("latest") {
        emit_progress(&app, "Resolving latest Minecraft release...", "info");
        let v = fetch_latest_minecraft_version(&client).await?;
        emit_progress(
            &app,
            format!("Latest Minecraft release is {}", v.release),
            "ok",
        );
        v.release
    } else {
        mc_version_raw
    };

    let (loader_version, version_id) = match loader_name.as_str() {
        "fabric" => {
            if let Some(existing) =
                detect_existing_loader_version(&mc_dir, "fabric", mc_version.as_str())
            {
                emit_progress(
                    &app,
                    format!("Using existing Fabric install: {existing} (skipping meta fetch)"),
                    "ok",
                );
                ("(existing)".to_string(), existing)
            } else {
                emit_progress(
                    &app,
                    format!("Picking Fabric loader for MC {mc_version}..."),
                    "info",
                );
                let lv = fabric_pick_loader_version(&client, &mc_version).await?;
                emit_progress(&app, format!("Fabric loader {lv}"), "ok");

                emit_progress(&app, "Fetching loader profile...", "info");
                let profile_json = fabric_fetch_profile_json(&client, &mc_version, &lv).await?;
                let vid = profile_json
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or("fabric profile json missing 'id'")?
                    .to_string();

                emit_progress(&app, format!("Writing version files: {vid}"), "info");
                write_fabric_version_files(&mc_dir, &vid, &profile_json)?;
                (lv, vid)
            }
        }
        "neoforge" => {
            if let Some(existing) = detect_existing_loader_version(&mc_dir, "neoforge", mc_version.as_str()) {
                emit_progress(
                    &app,
                    format!("Using existing NeoForge install: {existing} (skipping installer)"),
                    "ok",
                );
                ("(existing)".to_string(), existing)
            } else {
                install_neoforge(&app, &client, &mc_version, &mc_dir, None).await?
            }
        }
        _ => unreachable!("loader gate above rejects everything else"),
    };

    upsert_options_txt_setting(&mc_dir, "fullscreen", "false")?;
    emit_progress(
        &app,
        "options.txt -> set fullscreen:false (windowed by default; press F11 in-game to toggle)",
        "ok",
    );

    let java_args = java_args_for_minecraft(profile.java_xmx_gb);
    emit_progress(
        &app,
        format!("Java args (auto-tuned): {java_args}"),
        "info",
    );

    emit_progress(&app, "Updating launcher profiles...", "info");
    let launcher_profile_id = format!("mmle5-{}", profile.id);
    upsert_launcher_profile(
        &mc_dir,
        &launcher_profile_id,
        &profile.name,
        &version_id,
        Some(&java_args),
        profile.icon.as_deref(),
    )?;

    let mods_dir = mc_dir.join("mods");
    fs::create_dir_all(&mods_dir).map_err(|e| format!("mkdir mods: {e}"))?;
    let _ = prime_cache_from_disk(&app, &mc_dir).await;
    backup_existing_mods(&app, &mods_dir)?;

    let mut installed = 0u32;
    let mut skipped: Vec<String> = Vec::new();

    for m in &profile.mods {
        let display = ref_display_name(&m.source, &m.slug, &m.filename);
        match m.source.as_str() {
            "modrinth" => {
                let slug = match &m.slug {
                    Some(s) => s,
                    None => {
                        skipped.push(format!("{display} (modrinth source missing slug)"));
                        continue;
                    }
                };
                emit_progress(&app, format!("Resolving {}...", slug), "info");
                match modrinth_pick_version(&client, slug, &mc_version, &loader_name).await {
                    Ok(Some(ver)) => {
                        if !ver.game_versions.iter().any(|g| g == &mc_version) {
                            let supports = ver.game_versions.join(", ");
                            skipped.push(format!(
                                "{slug} {} (supports MC {supports} but not {mc_version})",
                                ver.version_number
                            ));
                            emit_progress(
                                &app,
                                format!(
                                    "Skip {slug} {} — supports {supports} not {mc_version}",
                                    ver.version_number
                                ),
                                "warn",
                            );
                            continue;
                        }
                        let file = ver
                            .files
                            .iter()
                            .find(|f| f.primary)
                            .or_else(|| ver.files.first())
                            .ok_or_else(|| format!("no files for {slug}"))?;
                        let supports = ver.game_versions.join(", ");
                        let dest = mods_dir.join(&file.filename);
                        emit_progress(
                            &app,
                            format!("{slug} {} (supports MC: {supports})", ver.version_number),
                            "info",
                        );
                        download_to_if_missing(
                            &app,
                            &client,
                            &file.url,
                            &dest,
                            file.hashes.sha1.as_deref(),
                        )
                        .await?;
                        installed += 1;
                    }
                    Ok(None) => {
                        skipped.push(format!(
                            "{slug} (no version for MC {mc_version} + {loader_name})"
                        ));
                        emit_progress(
                            &app,
                            format!("Skip {slug} (no compatible version)"),
                            "warn",
                        );
                    }
                    Err(e) => {
                        skipped.push(format!("{slug} ({e})"));
                        emit_progress(&app, format!("Skip {slug}: {e}"), "warn");
                    }
                }
            }
            "url" | "planetminecraft" | "curseforge_url" => {
                let url = match &m.url {
                    Some(u) => u,
                    None => {
                        skipped.push(format!("{display} (url source missing url)"));
                        continue;
                    }
                };
                let filename = match &m.filename {
                    Some(f) => f,
                    None => {
                        skipped.push(format!("{display} (url source missing filename)"));
                        continue;
                    }
                };
                let dest = mods_dir.join(filename);
                emit_progress(
                    &app,
                    format!("{filename} (url source, no MC version check)"),
                    "info",
                );
                match download_to_if_missing(&app, &client, url, &dest, None).await {
                    Ok(_) => installed += 1,
                    Err(e) => {
                        skipped.push(format!("{filename} ({e})"));
                        emit_progress(&app, format!("Skip {filename}: {e}"), "warn");
                    }
                }
            }
            other => {
                skipped.push(format!("{display} (unsupported source: {other})"));
                emit_progress(&app, format!("Skip {display}: unknown source {other}"), "warn");
            }
        }
    }

    let shaderpacks_dir = mc_dir.join("shaderpacks");
    let mut shaders_installed = 0u32;
    let mut default_shader_filename: Option<String> = None;
    if !profile.shaders.is_empty() {
        fs::create_dir_all(&shaderpacks_dir)
            .map_err(|e| format!("mkdir shaderpacks: {e}"))?;
        for s in &profile.shaders {
            let display = ref_display_name(&s.source, &s.slug, &s.filename);
            match s.source.as_str() {
                "modrinth" => {
                    let slug = match &s.slug {
                        Some(x) => x,
                        None => {
                            skipped.push(format!("shader {display} (modrinth source missing slug)"));
                            continue;
                        }
                    };
                    emit_progress(&app, format!("Resolving shader {slug}..."), "info");
                    match modrinth_pick_shader_version(&client, slug, &mc_version).await {
                        Ok(Some(ver)) => {
                            let file = ver
                                .files
                                .iter()
                                .find(|f| f.primary)
                                .or_else(|| ver.files.first())
                                .ok_or_else(|| format!("no files for shader {slug}"))?;
                            let dest = shaderpacks_dir.join(&file.filename);
                            download_to_if_missing(
                            &app,
                            &client,
                            &file.url,
                            &dest,
                            file.hashes.sha1.as_deref(),
                        )
                        .await?;
                            shaders_installed += 1;
                            if s.default && default_shader_filename.is_none() {
                                default_shader_filename = Some(file.filename.clone());
                            }
                        }
                        Ok(None) => {
                            skipped.push(format!("shader {slug} (no version for MC {mc_version})"));
                        }
                        Err(e) => {
                            skipped.push(format!("shader {slug} ({e})"));
                        }
                    }
                }
                "url" | "planetminecraft" | "curseforge_url" => {
                    let url = match &s.url {
                        Some(u) => u,
                        None => {
                            skipped.push(format!("shader {display} (url source missing url)"));
                            continue;
                        }
                    };
                    let filename = match &s.filename {
                        Some(f) => f,
                        None => {
                            skipped.push(format!("shader {display} (url source missing filename)"));
                            continue;
                        }
                    };
                    let dest = shaderpacks_dir.join(filename);
                    match download_to_if_missing(&app, &client, url, &dest, None).await {
                        Ok(_) => {
                            shaders_installed += 1;
                            if s.default && default_shader_filename.is_none() {
                                default_shader_filename = Some(filename.clone());
                            }
                        }
                        Err(e) => {
                            skipped.push(format!("shader {filename} ({e})"));
                            emit_progress(&app, format!("Skip shader {filename}: {e}"), "warn");
                        }
                    }
                }
                other => {
                    skipped.push(format!("shader {display} (unsupported source: {other})"));
                }
            }
        }
    }

    if let Some(filename) = &default_shader_filename {
        let iris_path = mc_dir.join("config").join("iris.properties");
        set_iris_default_shader(&mc_dir, filename)?;
        emit_progress(
            &app,
            format!(
                "iris.properties -> {} (shaderPack={}, enableShaders=true)",
                iris_path.display(),
                filename
            ),
            "ok",
        );
        let shader_zip = shaderpacks_dir.join(filename);
        if shader_zip.exists() {
            emit_progress(
                &app,
                format!("verified shader zip exists: {}", shader_zip.display()),
                "ok",
            );
        } else {
            emit_progress(
                &app,
                format!("WARNING: shader zip missing at {}", shader_zip.display()),
                "warn",
            );
        }
    }

    let resourcepacks_dir = mc_dir.join("resourcepacks");
    let mut resource_packs_installed = 0u32;
    let mut default_resourcepack_filename: Option<String> = None;
    if !profile.resource_packs.is_empty() {
        fs::create_dir_all(&resourcepacks_dir)
            .map_err(|e| format!("mkdir resourcepacks: {e}"))?;
        for rp in &profile.resource_packs {
            let display = ref_display_name(&rp.source, &rp.slug, &rp.filename);
            match rp.source.as_str() {
                "modrinth" => {
                    let slug = match &rp.slug {
                        Some(x) => x,
                        None => {
                            skipped.push(format!("resource pack {display} (modrinth source missing slug)"));
                            continue;
                        }
                    };
                    emit_progress(&app, format!("Resolving resource pack {slug}..."), "info");
                    match modrinth_pick_shader_version(&client, slug, &mc_version).await {
                        Ok(Some(ver)) => {
                            let file = ver
                                .files
                                .iter()
                                .find(|f| f.primary)
                                .or_else(|| ver.files.first())
                                .ok_or_else(|| format!("no files for resource pack {slug}"))?;
                            let dest = resourcepacks_dir.join(&file.filename);
                            download_to_if_missing(
                            &app,
                            &client,
                            &file.url,
                            &dest,
                            file.hashes.sha1.as_deref(),
                        )
                        .await?;
                            resource_packs_installed += 1;
                            if rp.default && default_resourcepack_filename.is_none() {
                                default_resourcepack_filename = Some(file.filename.clone());
                            }
                        }
                        Ok(None) => {
                            skipped.push(format!(
                                "resource pack {slug} (no version for MC {mc_version})"
                            ));
                        }
                        Err(e) => {
                            skipped.push(format!("resource pack {slug} ({e})"));
                        }
                    }
                }
                "url" | "planetminecraft" | "curseforge_url" => {
                    let url = match &rp.url {
                        Some(u) => u,
                        None => {
                            skipped.push(format!("resource pack {display} (url source missing url)"));
                            continue;
                        }
                    };
                    let filename = match &rp.filename {
                        Some(f) => f,
                        None => {
                            skipped.push(format!("resource pack {display} (url source missing filename)"));
                            continue;
                        }
                    };
                    let dest = resourcepacks_dir.join(filename);
                    match download_to_if_missing(&app, &client, url, &dest, None).await {
                        Ok(_) => {
                            resource_packs_installed += 1;
                            if rp.default && default_resourcepack_filename.is_none() {
                                default_resourcepack_filename = Some(filename.clone());
                            }
                        }
                        Err(e) => {
                            skipped.push(format!("resource pack {filename} ({e})"));
                            emit_progress(&app, format!("Skip resource pack {filename}: {e}"), "warn");
                        }
                    }
                }
                other => {
                    skipped.push(format!("resource pack {display} (unsupported source: {other})"));
                }
            }
        }
    }

    if let Some(filename) = &default_resourcepack_filename {
        let opts_path = mc_dir.join("options.txt");
        enable_resource_pack(&mc_dir, filename)?;
        emit_progress(
            &app,
            format!(
                "options.txt -> {} (added \"file/{}\" to resourcePacks)",
                opts_path.display(),
                filename
            ),
            "ok",
        );
        let rp_path = resourcepacks_dir.join(filename);
        if rp_path.exists() {
            emit_progress(
                &app,
                format!("verified resource pack zip: {}", rp_path.display()),
                "ok",
            );
        } else {
            emit_progress(
                &app,
                format!("WARNING: resource pack missing at {}", rp_path.display()),
                "warn",
            );
        }
    }

    Ok(InstallReport {
        profile_name: profile.name,
        minecraft_version: mc_version,
        loader_version,
        mods_installed: installed,
        shaders_installed,
        resource_packs_installed,
        skipped,
    })
}

#[derive(Serialize, Deserialize, Clone)]
struct UpdateInfo {
    kind: String, // "mod" | "shader" | "resourcepack"
    title: String,
    project_id: String,
    current_filename: String,
    current_version: String,
    latest_version: String,
    latest_filename: String,
    latest_url: String,
    current_path: String,
}

async fn hash_sha1_of(path: &Path) -> Result<String, String> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut hasher = Sha1::new();
    hasher.update(&bytes);
    Ok(hex::encode(hasher.finalize()))
}

async fn modrinth_lookup_by_hash(client: &reqwest::Client, hash: &str) -> Option<Value> {
    let url = format!("{MODRINTH_API}/version_file/{hash}?algorithm=sha1");
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<Value>().await.ok()
}

async fn modrinth_project_title(client: &reqwest::Client, project_id: &str) -> Option<String> {
    let url = format!("{MODRINTH_API}/project/{project_id}");
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: Value = resp.json().await.ok()?;
    v.get("title")
        .and_then(|t| t.as_str())
        .map(String::from)
}

async fn modrinth_latest_compatible(
    client: &reqwest::Client,
    project_id: &str,
    game_versions: &[String],
    loaders: &[String],
) -> Option<ModrinthVersion> {
    let url = format!("{MODRINTH_API}/project/{project_id}/version");
    let mut req = client.get(&url);
    if !game_versions.is_empty() {
        let json = format!(
            "[{}]",
            game_versions
                .iter()
                .map(|g| format!("\"{g}\""))
                .collect::<Vec<_>>()
                .join(",")
        );
        req = req.query(&[("game_versions", json)]);
    }
    if !loaders.is_empty() {
        let json = format!(
            "[{}]",
            loaders
                .iter()
                .map(|l| format!("\"{l}\""))
                .collect::<Vec<_>>()
                .join(",")
        );
        req = req.query(&[("loaders", json)]);
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let versions: Vec<ModrinthVersion> = resp.json().await.ok()?;
    versions.into_iter().next()
}

#[tauri::command]
async fn check_for_updates(
    app: AppHandle,
    minecraft_dir: Option<String>,
) -> Result<Vec<UpdateInfo>, String> {
    let mc_dir = match minecraft_dir.filter(|s| !s.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => default_minecraft_dir().ok_or("can't find .minecraft")?,
    };
    if !mc_dir.exists() {
        return Err(format!("Folder doesn't exist: {}", mc_dir.display()));
    }

    let client = http_client();
    let mut updates: Vec<UpdateInfo> = Vec::new();

    let scan_targets: [(&str, PathBuf, &str); 3] = [
        ("mod", mc_dir.join("mods"), "jar"),
        ("shader", mc_dir.join("shaderpacks"), "zip"),
        ("resourcepack", mc_dir.join("resourcepacks"), "zip"),
    ];

    for (kind, dir, ext) in scan_targets {
        if !dir.exists() {
            continue;
        }
        let entries: Vec<_> = match fs::read_dir(&dir) {
            Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
            Err(_) => continue,
        };
        for entry in entries {
            let path = entry.path();
            if !path
                .extension()
                .and_then(|x| x.to_str())
                .map(|s| s.eq_ignore_ascii_case(ext))
                .unwrap_or(false)
            {
                continue;
            }
            let filename = entry.file_name().to_string_lossy().to_string();
            emit_progress(&app, format!("Checking {filename}..."), "info");

            let hash = match hash_sha1_of(&path).await {
                Ok(h) => h,
                Err(_) => continue,
            };
            let info = match modrinth_lookup_by_hash(&client, &hash).await {
                Some(v) => v,
                None => continue,
            };
            let project_id = info
                .get("project_id")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let current_version = info
                .get("version_number")
                .and_then(|x| x.as_str())
                .unwrap_or("?")
                .to_string();
            let game_versions: Vec<String> = info
                .get("game_versions")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let loaders: Vec<String> = info
                .get("loaders")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            // For shaders / resource packs, loader filter would be empty —
            // pass an empty slice so Modrinth doesn't reject the query.
            let loader_filter: &[String] = if kind == "mod" { &loaders } else { &[] };
            let latest = match modrinth_latest_compatible(
                &client,
                &project_id,
                &game_versions,
                loader_filter,
            )
            .await
            {
                Some(v) => v,
                None => continue,
            };
            if latest.version_number == current_version {
                continue;
            }
            let file = match latest
                .files
                .iter()
                .find(|f| f.primary)
                .or_else(|| latest.files.first())
            {
                Some(f) => f,
                None => continue,
            };
            let title = modrinth_project_title(&client, &project_id)
                .await
                .unwrap_or_else(|| project_id.clone());
            updates.push(UpdateInfo {
                kind: kind.to_string(),
                title,
                project_id: project_id.clone(),
                current_filename: filename,
                current_version,
                latest_version: latest.version_number.clone(),
                latest_filename: file.filename.clone(),
                latest_url: file.url.clone(),
                current_path: path.to_string_lossy().to_string(),
            });
        }
    }

    emit_progress(
        &app,
        format!("Found {} update(s).", updates.len()),
        if updates.is_empty() { "info" } else { "ok" },
    );
    Ok(updates)
}

fn replace_in_iris_properties(
    mc_dir: &Path,
    old_filename: &str,
    new_filename: &str,
) -> Result<(), String> {
    let path = mc_dir.join("config").join("iris.properties");
    let content = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return Ok(()), // file doesn't exist — nothing to update
    };
    if !content.contains(old_filename) {
        return Ok(());
    }
    let updated = content.replace(old_filename, new_filename);
    fs::write(&path, updated).map_err(|e| format!("write iris.properties: {e}"))?;
    Ok(())
}

fn replace_in_options_resourcepacks(
    mc_dir: &Path,
    old_filename: &str,
    new_filename: &str,
) -> Result<(), String> {
    let path = mc_dir.join("options.txt");
    let content = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return Ok(()),
    };
    let mut lines: Vec<String> = content.lines().map(String::from).collect();
    let mut changed = false;
    for line in lines.iter_mut() {
        if line.starts_with("resourcePacks:") {
            let json_part = &line["resourcePacks:".len()..];
            let mut packs: Vec<String> = match serde_json::from_str(json_part) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let old_entry = format!("file/{old_filename}");
            let new_entry = format!("file/{new_filename}");
            for p in packs.iter_mut() {
                if p == &old_entry {
                    *p = new_entry.clone();
                    changed = true;
                }
            }
            if changed {
                let serialized =
                    serde_json::to_string(&packs).map_err(|e| e.to_string())?;
                *line = format!("resourcePacks:{serialized}");
            }
        }
    }
    if changed {
        let new_content = lines.join("\n") + "\n";
        fs::write(&path, new_content).map_err(|e| format!("write options.txt: {e}"))?;
    }
    Ok(())
}

#[tauri::command]
async fn apply_update(
    app: AppHandle,
    update: UpdateInfo,
    minecraft_dir: Option<String>,
) -> Result<(), String> {
    let mc_dir = match minecraft_dir.filter(|s| !s.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => default_minecraft_dir().ok_or("can't find .minecraft")?,
    };
    let dir = match update.kind.as_str() {
        "mod" => mc_dir.join("mods"),
        "shader" => mc_dir.join("shaderpacks"),
        "resourcepack" => mc_dir.join("resourcepacks"),
        other => return Err(format!("unknown kind: {other}")),
    };
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;

    let client = http_client();
    let dest = dir.join(&update.latest_filename);
    emit_progress(
        &app,
        format!("Downloading {} (was {})", update.latest_filename, update.current_filename),
        "info",
    );
    download_to(&client, &update.latest_url, &dest).await?;

    let old_path = PathBuf::from(&update.current_path);
    if old_path != dest && old_path.exists() {
        fs::remove_file(&old_path)
            .map_err(|e| format!("remove old {}: {e}", old_path.display()))?;
    }

    match update.kind.as_str() {
        "shader" => {
            replace_in_iris_properties(&mc_dir, &update.current_filename, &update.latest_filename)?;
        }
        "resourcepack" => {
            replace_in_options_resourcepacks(
                &mc_dir,
                &update.current_filename,
                &update.latest_filename,
            )?;
        }
        _ => {}
    }

    emit_progress(
        &app,
        format!("Updated {} -> {}", update.title, update.latest_version),
        "ok",
    );
    Ok(())
}

#[tauri::command]
async fn identify_jar(path: String) -> Result<JarIdentity, String> {
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|e| format!("read {path}: {e}"))?;
    let mut hasher = Sha1::new();
    hasher.update(&bytes);
    let hash = hex::encode(hasher.finalize());
    let client = http_client();
    let url = format!("{MODRINTH_API}/version_file/{hash}?algorithm=sha1");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("hash lookup: {e}"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(JarIdentity {
            matched: false,
            title: None,
            version_number: None,
            loaders: vec![],
            game_versions: vec![],
            project_id: None,
        });
    }
    if !resp.status().is_success() {
        return Err(format!("hash lookup: HTTP {}", resp.status()));
    }
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("hash lookup json: {e}"))?;
    let project_id = v
        .get("project_id")
        .and_then(|x| x.as_str())
        .map(String::from);

    let title = if let Some(pid) = &project_id {
        let purl = format!("{MODRINTH_API}/project/{pid}");
        match client.get(&purl).send().await {
            Ok(r) => match r.json::<Value>().await {
                Ok(p) => p.get("title").and_then(|t| t.as_str()).map(String::from),
                Err(_) => None,
            },
            Err(_) => None,
        }
    } else {
        None
    };

    Ok(JarIdentity {
        matched: true,
        title,
        version_number: v
            .get("version_number")
            .and_then(|x| x.as_str())
            .map(String::from),
        loaders: v
            .get("loaders")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        game_versions: v
            .get("game_versions")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        project_id,
    })
}

#[derive(Deserialize)]
struct ModrinthProjectMeta {
    #[serde(default)]
    title: Option<String>,
    // Modrinth says one of: required, optional, unsupported, unknown.
    #[serde(default)]
    server_side: Option<String>,
}

async fn modrinth_project_meta(
    client: &reqwest::Client,
    slug: &str,
) -> Result<ModrinthProjectMeta, String> {
    let url = format!("{MODRINTH_API}/project/{slug}");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("modrinth project {slug}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("modrinth project {slug}: HTTP {}", resp.status()));
    }
    resp.json::<ModrinthProjectMeta>()
        .await
        .map_err(|e| format!("modrinth project {slug} json: {e}"))
}

#[derive(Serialize)]
struct ServerPackReport {
    profile_name: String,
    minecraft_version: String,
    loader: String,
    output_path: String,
    mods_included: u32,
    skipped: Vec<String>,
}

fn build_server_readme(
    profile_name: &str,
    profile_id: &str,
    mc_version: &str,
    loader: &str,
    included: &[String],
    skipped: &[String],
) -> String {
    let mut s = String::new();
    s.push_str(&format!("# {profile_name} — Server Pack\n\n"));
    s.push_str(&format!(
        "Generated by Minecraft Mods, Easy from profile `{profile_id}`.\n\n\
         Minecraft **{mc_version}** · loader **{loader}**.\n\n",
    ));
    s.push_str("## Install\n\n");
    s.push_str("1. Make a fresh folder for your server (e.g. `my-server/`).\n");
    match loader {
        "fabric" => {
            s.push_str(
                "2. Download the Fabric server installer from <https://fabricmc.net/use/server/>.\n",
            );
            s.push_str(&format!(
                "3. Run the installer with the matching Minecraft version, e.g.:\n   ```\n   java -jar fabric-installer-<X.Y.Z>.jar server -mcversion {mc_version} -downloadMinecraft\n   ```\n",
            ));
            s.push_str(
                "   This creates `fabric-server-launch.jar` and downloads the vanilla server jar.\n",
            );
            s.push_str("4. Drop the `mods/` folder from this zip into the server folder.\n");
            s.push_str("5. Edit `eula.txt`: set `eula=true` (you must accept Mojang's EULA before the server runs).\n");
            s.push_str("6. Start the server: `java -jar fabric-server-launch.jar nogui`\n\n");
        }
        "neoforge" => {
            s.push_str(&format!(
                "2. Download the NeoForge installer for MC {mc_version} — same major.minor prefix as the version your Minecraft Mods, Easy client picked. Browse releases at <https://projects.neoforged.net/neoforged/neoforge>.\n",
            ));
            s.push_str(
                "3. Run the installer in your server folder:\n   ```\n   java -jar neoforge-<version>-installer.jar --installServer .\n   ```\n",
            );
            s.push_str("4. Drop the `mods/` folder from this zip into the server folder.\n");
            s.push_str("5. Edit `eula.txt`: set `eula=true` (you must accept Mojang's EULA before the server runs).\n");
            s.push_str("6. Start the server using the script the installer generated (`run.sh` on Linux/macOS, `run.bat` on Windows).\n\n");
        }
        other => {
            s.push_str(&format!(
                "2. Install the {other} server loader for MC {mc_version} per the loader's documentation.\n",
            ));
            s.push_str("3. Drop the `mods/` folder from this zip into the server folder.\n");
            s.push_str("4. Accept the EULA in `eula.txt` and start the server.\n\n");
        }
    }
    s.push_str("## Mods included\n\n");
    if included.is_empty() {
        s.push_str("_None._\n\n");
    } else {
        for line in included {
            s.push_str(&format!("- {line}\n"));
        }
        s.push('\n');
    }
    if !skipped.is_empty() {
        s.push_str("## Skipped (client-only or unavailable)\n\n");
        for line in skipped {
            s.push_str(&format!("- {line}\n"));
        }
        s.push('\n');
    }
    s.push_str(
        "Players need a matching mod set on the **client** to join. Run the same profile in Minecraft Mods, Easy on each player's machine to get a compatible client.\n",
    );
    s
}

fn write_server_pack_zip(
    target: &Path,
    mod_files: &[(String, Vec<u8>)],
    readme: &str,
) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
    }
    let f = fs::File::create(target).map_err(|e| format!("create {}: {e}", target.display()))?;
    let mut zip = zip::ZipWriter::new(f);
    let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    zip.start_file("README.md", opts)
        .map_err(|e| format!("zip README: {e}"))?;
    zip.write_all(readme.as_bytes())
        .map_err(|e| format!("zip write README: {e}"))?;

    // Empty mods/ folder marker — if every mod is client-only the user still
    // needs the dir to exist so they have somewhere to drop server-side mods.
    zip.add_directory("mods/", opts)
        .map_err(|e| format!("zip mkdir mods/: {e}"))?;

    for (name, bytes) in mod_files {
        zip.start_file(format!("mods/{name}"), opts)
            .map_err(|e| format!("zip {name}: {e}"))?;
        zip.write_all(bytes)
            .map_err(|e| format!("zip write {name}: {e}"))?;
    }
    zip.finish().map_err(|e| format!("zip finish: {e}"))?;
    Ok(())
}

#[tauri::command]
async fn download_server_pack(
    app: AppHandle,
    profile_id: String,
    target_path: String,
) -> Result<ServerPackReport, String> {
    let profiles = read_profiles(&app)?;
    let profile = profiles
        .into_iter()
        .find(|p| p.id == profile_id)
        .ok_or_else(|| format!("Unknown profile: {profile_id}"))?;
    if profile.modpack.is_some() {
        return Err(format!(
            "Server-pack export isn't supported for modpack profiles like '{}' — install it client-side first.",
            profile.name
        ));
    }
    let loader_name = profile
        .loader
        .clone()
        .ok_or("profile has no loader")?;
    if loader_name != "fabric" && loader_name != "neoforge" {
        return Err(format!(
            "Server-pack export only supports Fabric and NeoForge — '{}' uses {}.",
            profile.name, loader_name
        ));
    }

    let target = PathBuf::from(&target_path);
    if target.as_os_str().is_empty() {
        return Err("No output path selected".to_string());
    }

    let client = http_client();
    let mc_version_raw = profile
        .minecraft_version
        .clone()
        .ok_or("profile has no minecraft_version")?;
    let mc_version = if mc_version_raw.eq_ignore_ascii_case("latest") {
        emit_progress(&app, "Resolving latest Minecraft release...", "info");
        let v = fetch_latest_minecraft_version(&client).await?;
        emit_progress(&app, format!("Latest Minecraft release is {}", v.release), "ok");
        v.release
    } else {
        mc_version_raw
    };

    emit_progress(
        &app,
        format!(
            "Building server pack for '{}' (MC {mc_version}, {})",
            profile.name, loader_name
        ),
        "info",
    );

    let mut mod_files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut included: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();

    for m in &profile.mods {
        let display = ref_display_name(&m.source, &m.slug, &m.filename);
        if m.source != "modrinth" {
            skipped.push(format!("{display} (unsupported source {})", m.source));
            continue;
        }
        let slug = match &m.slug {
            Some(s) => s.clone(),
            None => {
                skipped.push(format!("{display} (modrinth source missing slug)"));
                continue;
            }
        };
        emit_progress(&app, format!("Checking {slug}..."), "info");
        let meta = match modrinth_project_meta(&client, &slug).await {
            Ok(meta) => meta,
            Err(e) => {
                skipped.push(format!("{slug} ({e})"));
                emit_progress(&app, format!("Skip {slug}: {e}"), "warn");
                continue;
            }
        };
        let server_side = meta.server_side.as_deref().unwrap_or("unknown");
        if server_side == "unsupported" {
            skipped.push(format!("{slug} (client-only)"));
            emit_progress(&app, format!("Skip {slug} (client-only)"), "info");
            continue;
        }
        let pretty = meta.title.clone().unwrap_or_else(|| slug.clone());
        let ver = match modrinth_pick_version(&client, &slug, &mc_version, &loader_name).await {
            Ok(Some(v)) => v,
            Ok(None) => {
                skipped.push(format!(
                    "{slug} (no version for MC {mc_version} + {loader_name})"
                ));
                emit_progress(&app, format!("Skip {slug} (no compatible version)"), "warn");
                continue;
            }
            Err(e) => {
                skipped.push(format!("{slug} ({e})"));
                emit_progress(&app, format!("Skip {slug}: {e}"), "warn");
                continue;
            }
        };
        if !ver.game_versions.iter().any(|g| g == &mc_version) {
            skipped.push(format!(
                "{slug} {} (supports {} not {mc_version})",
                ver.version_number,
                ver.game_versions.join(", ")
            ));
            continue;
        }
        let file = ver
            .files
            .iter()
            .find(|f| f.primary)
            .or_else(|| ver.files.first())
            .ok_or_else(|| format!("no files for {slug}"))?;

        emit_progress(&app, format!("Downloading {}", file.filename), "info");
        let bytes = client
            .get(&file.url)
            .send()
            .await
            .map_err(|e| format!("download {slug}: {e}"))?
            .bytes()
            .await
            .map_err(|e| format!("download {slug} body: {e}"))?
            .to_vec();
        mod_files.push((file.filename.clone(), bytes));
        included.push(format!("{pretty} {} (`{}`)", ver.version_number, file.filename));
    }

    emit_progress(&app, format!("Writing zip to {}", target.display()), "info");
    let readme = build_server_readme(
        &profile.name,
        &profile.id,
        &mc_version,
        &loader_name,
        &included,
        &skipped,
    );
    write_server_pack_zip(&target, &mod_files, &readme)?;

    let count = mod_files.len() as u32;
    emit_progress(
        &app,
        format!(
            "Wrote {count} mod{} to {}",
            if count == 1 { "" } else { "s" },
            target.display()
        ),
        "ok",
    );

    Ok(ServerPackReport {
        profile_name: profile.name,
        minecraft_version: mc_version,
        loader: loader_name,
        output_path: target.to_string_lossy().to_string(),
        mods_included: count,
        skipped,
    })
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .invoke_handler(tauri::generate_handler![
            get_minecraft_dir,
            get_latest_minecraft_version,
            list_profiles,
            install_profile,
            identify_jar,
            find_minecraft_processes,
            kill_minecraft_processes,
            launch_minecraft_launcher,
            get_app_version,
            get_data_manifest,
            refresh_data_cache,
            check_for_updates,
            apply_update,
            download_server_pack,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
