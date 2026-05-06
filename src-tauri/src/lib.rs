use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use sysinfo::System;
use tauri::{AppHandle, Emitter};

const MODRINTH_API: &str = "https://api.modrinth.com/v2";
const FABRIC_META: &str = "https://meta.fabricmc.net/v2";
const USER_AGENT: &str = "minecraft-mod-like-im-five/0.1";

// Curated profiles are embedded at compile time. Editing the YAML and
// rebuilding the app updates the available profiles.
const PROFILES_YAML: &str = include_str!("../../data/profiles.yaml");

#[derive(Serialize, Deserialize, Clone, Debug)]
struct ModRef {
    source: String,
    slug: String,
    #[serde(default)]
    role: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct ShaderRef {
    source: String,
    slug: String,
    #[serde(default)]
    default: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct ResourcePackRef {
    source: String,
    slug: String,
    #[serde(default)]
    default: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Profile {
    id: String,
    name: String,
    short_description: String,
    minecraft_version: String,
    loader: String,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    accent: Option<String>,
    #[serde(default)]
    not_implemented_in_phase_1: bool,
    mods: Vec<ModRef>,
    #[serde(default)]
    shaders: Vec<ShaderRef>,
    #[serde(default)]
    resource_packs: Vec<ResourcePackRef>,
}

#[derive(Serialize, Deserialize)]
struct ProfilesFile {
    #[allow(dead_code)]
    version: u32,
    profiles: Vec<Profile>,
}

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

#[derive(Deserialize)]
struct ModrinthFile {
    url: String,
    filename: String,
    #[serde(default)]
    primary: bool,
    #[serde(default)]
    #[allow(dead_code)]
    hashes: Option<Value>,
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

fn read_profiles() -> Result<Vec<Profile>, String> {
    let parsed: ProfilesFile = serde_yaml::from_str(PROFILES_YAML)
        .map_err(|e| format!("parse profiles.yaml: {e}"))?;
    Ok(parsed.profiles)
}

#[tauri::command]
fn list_profiles() -> Result<Vec<Profile>, String> {
    read_profiles()
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
    let mut found = vec![];
    for (pid, proc) in sys.processes() {
        let name = proc.name().to_string_lossy().to_string();
        let lower = name.to_lowercase();
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
    let mut candidates: Vec<PathBuf> = vec![];
    if let Ok(p) = std::env::var("ProgramFiles(x86)") {
        candidates.push(PathBuf::from(p).join("Minecraft Launcher").join("MinecraftLauncher.exe"));
    }
    if let Ok(p) = std::env::var("ProgramFiles") {
        candidates.push(PathBuf::from(p).join("Minecraft Launcher").join("MinecraftLauncher.exe"));
    }
    if let Ok(p) = std::env::var("LOCALAPPDATA") {
        candidates.push(PathBuf::from(p).join(r"Microsoft\WindowsApps\Microsoft.4297127D64EC6_8wekyb3d8bbwe\Minecraft.exe"));
    }
    for path in &candidates {
        if path.exists() {
            std::process::Command::new(path)
                .spawn()
                .map_err(|e| format!("spawn launcher {}: {e}", path.display()))?;
            return Ok(());
        }
    }
    // Fallback to URI scheme registered by the official launcher
    std::process::Command::new("cmd")
        .args(["/c", "start", "", "minecraft-launcher://"])
        .spawn()
        .map_err(|e| format!("fallback launcher: {e}"))?;
    Ok(())
}

// Pick a sensible -Xmx based on installed RAM. Half of system memory, capped
// at 8 GB (Minecraft rarely benefits past that and large heaps slow GC), with
// a 2 GB floor for low-RAM machines.
fn calculate_java_xmx_gb() -> u32 {
    let mut sys = System::new_all();
    sys.refresh_memory();
    let total_gb = (sys.total_memory() / 1024 / 1024 / 1024) as u32;
    (total_gb / 2).clamp(2, 8)
}

fn java_args_for_minecraft() -> String {
    let xmx = calculate_java_xmx_gb();
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
    Ok(())
}

fn upsert_launcher_profile(
    mc_dir: &Path,
    profile_id: &str,
    name: &str,
    version_id: &str,
    java_args: Option<&str>,
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
    let mut entry = json!({
        "name": name,
        "type": "custom",
        "created": now,
        "lastUsed": now,
        "lastVersionId": version_id,
        "icon": "Crafting_Table",
    });
    if let Some(args) = java_args {
        entry
            .as_object_mut()
            .unwrap()
            .insert("javaArgs".to_string(), Value::String(args.to_string()));
    }
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
    let profiles = read_profiles()?;
    let profile = profiles
        .into_iter()
        .find(|p| p.id == profile_id)
        .ok_or_else(|| format!("Unknown profile: {profile_id}"))?;

    if profile.not_implemented_in_phase_1 {
        return Err(format!(
            "'{}' uses {} which isn't supported yet — coming in phase 2.",
            profile.name, profile.loader
        ));
    }
    if profile.loader != "fabric" {
        return Err(format!(
            "Phase 1 only supports Fabric — '{}' uses {}.",
            profile.name, profile.loader
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
    let loader_name = profile.loader.clone();
    let mc_version = if profile.minecraft_version.eq_ignore_ascii_case("latest") {
        emit_progress(&app, "Resolving latest Minecraft release...", "info");
        let v = fetch_latest_minecraft_version(&client).await?;
        emit_progress(
            &app,
            format!("Latest Minecraft release is {}", v.release),
            "ok",
        );
        v.release
    } else {
        profile.minecraft_version.clone()
    };

    emit_progress(
        &app,
        format!("Picking Fabric loader for MC {mc_version}..."),
        "info",
    );
    let loader_version = fabric_pick_loader_version(&client, &mc_version).await?;
    emit_progress(&app, format!("Fabric loader {loader_version}"), "ok");

    emit_progress(&app, "Fetching loader profile...", "info");
    let profile_json = fabric_fetch_profile_json(&client, &mc_version, &loader_version).await?;
    let version_id = profile_json
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or("fabric profile json missing 'id'")?
        .to_string();

    emit_progress(&app, format!("Writing version files: {version_id}"), "info");
    write_fabric_version_files(&mc_dir, &version_id, &profile_json)?;

    let java_args = java_args_for_minecraft();
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
    )?;

    let mods_dir = mc_dir.join("mods");
    fs::create_dir_all(&mods_dir).map_err(|e| format!("mkdir mods: {e}"))?;
    backup_existing_mods(&app, &mods_dir)?;

    let mut installed = 0u32;
    let mut skipped: Vec<String> = Vec::new();

    for m in &profile.mods {
        if m.source != "modrinth" {
            skipped.push(format!("{} (unsupported source {})", m.slug, m.source));
            continue;
        }
        emit_progress(&app, format!("Resolving {}...", m.slug), "info");
        match modrinth_pick_version(&client, &m.slug, &mc_version, &loader_name).await {
            Ok(Some(ver)) => {
                if !ver.game_versions.iter().any(|g| g == &mc_version) {
                    let supports = ver.game_versions.join(", ");
                    skipped.push(format!(
                        "{} {} (supports MC {} but not {mc_version})",
                        m.slug, ver.version_number, supports
                    ));
                    emit_progress(
                        &app,
                        format!(
                            "Skip {} {} — supports {} not {mc_version}",
                            m.slug, ver.version_number, supports
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
                    .ok_or_else(|| format!("no files for {}", m.slug))?;
                let supports = ver.game_versions.join(", ");
                let dest = mods_dir.join(&file.filename);
                emit_progress(
                    &app,
                    format!("{} {} (supports MC: {supports})", m.slug, ver.version_number),
                    "info",
                );
                emit_progress(&app, format!("Downloading {}", file.filename), "info");
                download_to(&client, &file.url, &dest).await?;
                installed += 1;
            }
            Ok(None) => {
                skipped.push(format!(
                    "{} (no version for MC {mc_version} + {loader_name})",
                    m.slug
                ));
                emit_progress(
                    &app,
                    format!("Skip {} (no compatible version)", m.slug),
                    "warn",
                );
            }
            Err(e) => {
                skipped.push(format!("{} ({e})", m.slug));
                emit_progress(&app, format!("Skip {}: {e}", m.slug), "warn");
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
            if s.source != "modrinth" {
                continue;
            }
            emit_progress(&app, format!("Resolving shader {}...", s.slug), "info");
            match modrinth_pick_shader_version(&client, &s.slug, &mc_version).await {
                Ok(Some(ver)) => {
                    let file = ver
                        .files
                        .iter()
                        .find(|f| f.primary)
                        .or_else(|| ver.files.first())
                        .ok_or_else(|| format!("no files for shader {}", s.slug))?;
                    let dest = shaderpacks_dir.join(&file.filename);
                    emit_progress(&app, format!("Downloading {}", file.filename), "info");
                    download_to(&client, &file.url, &dest).await?;
                    shaders_installed += 1;
                    if s.default && default_shader_filename.is_none() {
                        default_shader_filename = Some(file.filename.clone());
                    }
                }
                Ok(None) => {
                    skipped.push(format!(
                        "shader {} (no version for MC {mc_version})",
                        s.slug
                    ));
                }
                Err(e) => {
                    skipped.push(format!("shader {} ({e})", s.slug));
                }
            }
        }
    }

    if let Some(filename) = &default_shader_filename {
        set_iris_default_shader(&mc_dir, filename)?;
        emit_progress(&app, format!("Pre-selected shader: {filename}"), "ok");
    }

    let resourcepacks_dir = mc_dir.join("resourcepacks");
    let mut resource_packs_installed = 0u32;
    let mut default_resourcepack_filename: Option<String> = None;
    if !profile.resource_packs.is_empty() {
        fs::create_dir_all(&resourcepacks_dir)
            .map_err(|e| format!("mkdir resourcepacks: {e}"))?;
        for rp in &profile.resource_packs {
            if rp.source != "modrinth" {
                continue;
            }
            emit_progress(&app, format!("Resolving resource pack {}...", rp.slug), "info");
            match modrinth_pick_shader_version(&client, &rp.slug, &mc_version).await {
                Ok(Some(ver)) => {
                    let file = ver
                        .files
                        .iter()
                        .find(|f| f.primary)
                        .or_else(|| ver.files.first())
                        .ok_or_else(|| format!("no files for resource pack {}", rp.slug))?;
                    let dest = resourcepacks_dir.join(&file.filename);
                    emit_progress(&app, format!("Downloading {}", file.filename), "info");
                    download_to(&client, &file.url, &dest).await?;
                    resource_packs_installed += 1;
                    if rp.default && default_resourcepack_filename.is_none() {
                        default_resourcepack_filename = Some(file.filename.clone());
                    }
                }
                Ok(None) => {
                    skipped.push(format!(
                        "resource pack {} (no version for MC {mc_version})",
                        rp.slug
                    ));
                }
                Err(e) => {
                    skipped.push(format!("resource pack {} ({e})", rp.slug));
                }
            }
        }
    }

    if let Some(filename) = &default_resourcepack_filename {
        enable_resource_pack(&mc_dir, filename)?;
        emit_progress(&app, format!("Enabled resource pack: {filename}"), "ok");
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

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            get_minecraft_dir,
            get_latest_minecraft_version,
            list_profiles,
            install_profile,
            identify_jar,
            find_minecraft_processes,
            kill_minecraft_processes,
            launch_minecraft_launcher,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
