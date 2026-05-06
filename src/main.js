const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { open: openDialog, ask } = window.__TAURI__.dialog;

const STORAGE_KEY = "minecraft_dir_override";

let latestMcVersion = null;

function activeMcDir() {
  return localStorage.getItem(STORAGE_KEY) || null;
}

const statusEl = document.getElementById("mc-status");
const versionEl = document.getElementById("mc-version");
const cardsEl = document.getElementById("profiles");
const dropEl = document.getElementById("dropzone");
const logEl = document.getElementById("log");

async function refreshMcDirStatus() {
  statusEl.classList.remove("ok", "bad");
  const override = activeMcDir();
  if (override) {
    statusEl.textContent = `Minecraft: ${override}  (click to change)`;
    statusEl.classList.add("ok");
    return;
  }
  try {
    const dir = await invoke("get_minecraft_dir");
    statusEl.textContent = `Minecraft: ${dir}  (click to change)`;
    statusEl.classList.add("ok");
  } catch (err) {
    statusEl.textContent = `Click to pick .minecraft folder`;
    statusEl.classList.add("bad");
  }
}

async function pickMcDir() {
  const start = activeMcDir() || undefined;
  const picked = await openDialog({
    directory: true,
    multiple: false,
    defaultPath: start,
    title: "Select your .minecraft folder",
  });
  if (!picked) return;
  localStorage.setItem(STORAGE_KEY, picked);
  await refreshMcDirStatus();
  logLine(`Using Minecraft folder: ${picked}`, "ok");
}

function logLine(message, level = "info") {
  const div = document.createElement("div");
  div.className = `log-line ${level}`;
  div.textContent = message;
  logEl.appendChild(div);
  logEl.scrollTop = logEl.scrollHeight;
}

async function init() {
  try {
    const v = await invoke("get_latest_minecraft_version");
    latestMcVersion = v.release;
    versionEl.textContent = `Latest MC: ${v.release}`;
    versionEl.classList.add("ok");
  } catch (err) {
    versionEl.textContent = `Latest MC: ?`;
  }

  await refreshMcDirStatus();
  statusEl.addEventListener("click", pickMcDir);

  try {
    const profiles = await invoke("list_profiles");
    renderProfiles(profiles);
  } catch (err) {
    logLine(`Failed to load profiles: ${err}`, "bad");
  }

  await listen("install-progress", (event) => {
    const { message, level } = event.payload;
    logLine(message, level || "info");
  });

  setupDropzone();
}

function renderProfiles(profiles) {
  cardsEl.innerHTML = "";
  for (const p of profiles) {
    const card = document.createElement("div");
    card.className = "card";
    card.style.setProperty("--card-color", p.color || "#7c4dff");

    const disabled = p.not_implemented_in_phase_1 ? "disabled" : "";
    const buttonLabel = p.not_implemented_in_phase_1 ? "Coming soon" : "Set it up";
    const isAuto = p.minecraft_version.toLowerCase() === "latest";
    const versionLabel = isAuto
      ? (latestMcVersion ? `MC ${latestMcVersion} (latest)` : "MC latest (auto)")
      : `MC ${p.minecraft_version}`;

    card.innerHTML = `
      <div class="card-meta">
        <span class="tag">${versionLabel}</span>
        <span class="tag">${p.loader}</span>
        <span class="tag">${p.mods.length} mods</span>
      </div>
      <div class="card-name">${p.name}</div>
      <div class="card-desc">${p.short_description}</div>
      <button class="install" ${disabled} data-id="${p.id}">${buttonLabel}</button>
    `;

    const btn = card.querySelector("button");
    btn.addEventListener("click", () => installProfile(p));
    cardsEl.appendChild(card);
  }
}

async function installProfile(profile) {
  logEl.innerHTML = "";

  try {
    const running = await invoke("find_minecraft_processes");
    if (running.length > 0) {
      const names = [...new Set(running.map((p) => p.name))].join(", ");
      const proceed = await ask(
        `Minecraft is running (${names}). It needs to close before we can install mods. Close it now?`,
        {
          title: "Minecraft is open",
          kind: "warning",
          okLabel: "Close it",
          cancelLabel: "Cancel",
        },
      );
      if (!proceed) {
        logLine("Cancelled — close Minecraft yourself and try again.", "warn");
        return;
      }
      logLine(`Closing Minecraft (${names})...`, "info");
      const killed = await invoke("kill_minecraft_processes");
      logLine(`Closed ${killed} process${killed === 1 ? "" : "es"}.`, "ok");
      await new Promise((r) => setTimeout(r, 600));
    }
  } catch (err) {
    logLine(`Process check failed: ${err}`, "warn");
  }

  logLine(`Setting up "${profile.name}"...`);
  try {
    const report = await invoke("install_profile", {
      profileId: profile.id,
      minecraftDir: activeMcDir(),
    });
    const extras = [];
    if (report.shaders_installed) extras.push(`${report.shaders_installed} shader`);
    if (report.resource_packs_installed)
      extras.push(`${report.resource_packs_installed} resource pack`);
    const extrasStr = extras.length ? `, ${extras.join(", ")}` : "";
    logLine(
      `Done. ${report.mods_installed} mods${extrasStr}. Fabric ${report.loader_version} for MC ${report.minecraft_version}.`,
      "ok",
    );

    logLine(`Opening the Minecraft launcher...`, "info");
    try {
      await invoke("launch_minecraft_launcher");
      logLine(`Launcher opened — pick "${report.profile_name}" in the dropdown.`, "ok");
    } catch (e) {
      logLine(`Couldn't auto-open launcher: ${e}. Open it manually.`, "warn");
    }
  } catch (err) {
    logLine(`Setup failed: ${err}`, "bad");
  }
}

function setupDropzone() {
  for (const ev of ["dragenter", "dragover"]) {
    dropEl.addEventListener(ev, (e) => {
      e.preventDefault();
      dropEl.classList.add("over");
    });
  }
  for (const ev of ["dragleave", "drop"]) {
    dropEl.addEventListener(ev, (e) => {
      e.preventDefault();
      dropEl.classList.remove("over");
    });
  }
  // Tauri 2 file drop comes through the window event, not browser DataTransfer
  listen("tauri://drag-drop", async (event) => {
    const paths = event.payload?.paths || [];
    for (const path of paths) {
      try {
        const info = await invoke("identify_jar", { path });
        if (info.matched) {
          logLine(
            `Identified ${info.title} ${info.version_number} (${info.loaders.join(", ")} / MC ${info.game_versions.join(", ")})`,
            "ok",
          );
        } else {
          logLine(`Unknown jar: ${path} — not on Modrinth`, "warn");
        }
      } catch (err) {
        logLine(`Couldn't read ${path}: ${err}`, "bad");
      }
    }
  });
}

init();
