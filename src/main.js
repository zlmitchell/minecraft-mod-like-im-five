const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { open: openDialog, save: saveDialog, ask } = window.__TAURI__.dialog;
const updater = window.__TAURI__.updater;
const processPlugin = window.__TAURI__.process;

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
const logOverlayEl = document.getElementById("log-overlay");
const logToggleBtn = document.getElementById("log-toggle");
const logCloseBtn = document.getElementById("log-close-btn");
const versionLineEl = document.getElementById("version-line");
const checkUpdatesBtn = document.getElementById("check-updates-btn");
const updatesPanel = document.getElementById("updates-panel");
const updatesListEl = document.getElementById("updates-list");
const updateAllBtn = document.getElementById("update-all-btn");
const updatesCloseBtn = document.getElementById("updates-close-btn");

// Show overlay (and hide the floating "Log" pill). Once a line lands the
// user has seen something happen — the overlay stays open until they hit ×.
function showLogOverlay() {
  logOverlayEl.hidden = false;
  logToggleBtn.hidden = true;
}

// Hide the overlay but leave the lines in place; expose the "Log" pill so the
// user can pop it back open without losing history.
function hideLogOverlay() {
  logOverlayEl.hidden = true;
  logToggleBtn.hidden = logEl.childElementCount === 0;
}

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
  showLogOverlay();
}

async function checkForUpdates() {
  if (!updater?.check) return;
  try {
    const update = await updater.check();
    if (!update) return;
    const proceed = await ask(
      `Version ${update.version} is available (you're on ${update.currentVersion}). Install now? The app will restart.`,
      {
        title: "Update available",
        kind: "info",
        okLabel: "Install update",
        cancelLabel: "Later",
      },
    );
    if (!proceed) return;
    let lastPct = -10;
    await update.downloadAndInstall((event) => {
      if (event.event === "Started") {
        logLine(`Downloading update ${update.version}...`, "info");
      } else if (event.event === "Progress") {
        const total = event.data?.contentLength || 0;
        if (total > 0) {
          const pct = Math.floor((event.data.downloaded / total) * 100);
          if (pct >= lastPct + 10) {
            logLine(`  ${pct}%`, "info");
            lastPct = pct;
          }
        }
      } else if (event.event === "Finished") {
        logLine("Update installed. Restarting...", "ok");
      }
    });
    if (processPlugin?.relaunch) {
      await processPlugin.relaunch();
    }
  } catch (err) {
    console.warn("update check:", err);
  }
}

function shortDate(iso) {
  if (!iso) return "?";
  return iso.slice(0, 10);
}

async function showVersions() {
  let appV = "?";
  let dataLine = "?";
  try {
    appV = await invoke("get_app_version");
  } catch {}
  versionLineEl.textContent = `App v${appV} · Data ${dataLine}`;
  try {
    const m = await invoke("get_data_manifest");
    if (m.label && m.label.startsWith("local-dev")) {
      versionLineEl.textContent = `App v${appV} · Data ${m.label}`;
    } else {
      const date = shortDate(m.published_at);
      const cacheTag = m.using_cache ? "" : " (embedded)";
      dataLine = `${date}${cacheTag}`;
      versionLineEl.textContent = `App v${appV} · Data ${dataLine}`;
    }
  } catch {
    versionLineEl.textContent = `App v${appV} · Data embedded`;
  }
}

async function init() {
  // Background update check — doesn't block UI setup
  checkForUpdates();

  showVersions();

  // Pull latest profiles/frameworks YAML from the data-latest GitHub release
  // in the background; if it succeeds, re-render the cards from the fresh
  // data so kids don't need to relaunch to see new mods.
  invoke("refresh_data_cache")
    .then(async (updated) => {
      if (updated) {
        try {
          const profiles = await invoke("list_profiles");
          renderProfiles(profiles);
          showVersions();
          logLine("Profiles refreshed from latest data release.", "ok");
        } catch (e) {
          console.warn("re-render after refresh:", e);
        }
      }
    })
    .catch((e) => console.warn("data refresh:", e));

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

  checkUpdatesBtn.addEventListener("click", runCheckForUpdates);
  updateAllBtn.addEventListener("click", runUpdateAll);
  updatesCloseBtn.addEventListener("click", () => {
    updatesPanel.hidden = true;
  });
  logCloseBtn.addEventListener("click", hideLogOverlay);
  logToggleBtn.addEventListener("click", showLogOverlay);
}

function renderProfiles(profiles) {
  cardsEl.innerHTML = "";
  for (const p of profiles) {
    const card = document.createElement("div");
    card.className = "card";
    card.style.setProperty("--card-color", p.color || "#7c4dff");

    const disabled = p.not_implemented_in_phase_1 ? "disabled" : "";
    const buttonLabel = p.not_implemented_in_phase_1 ? "Coming soon" : "Set it up";
    const isModpack = !!p.modpack;
    let versionLabel;
    if (isModpack) {
      versionLabel = "modpack (manifest pins MC + loader)";
    } else {
      const mcVer = p.minecraft_version || "?";
      const isAuto = mcVer.toLowerCase() === "latest";
      versionLabel = isAuto
        ? (latestMcVersion ? `MC ${latestMcVersion} (latest)` : "MC latest (auto)")
        : `MC ${mcVer}`;
    }
    const loaderLabel = isModpack ? p.modpack.slug : (p.loader || "?");
    const itemsLabel = isModpack
      ? "modpack"
      : `${(p.mods || []).length} mods`;

    card.innerHTML = `
      <div class="card-meta">
        <span class="tag">${versionLabel}</span>
        <span class="tag">${loaderLabel}</span>
        <span class="tag">${itemsLabel}</span>
      </div>
      <div class="card-name">${p.name}</div>
      <div class="card-desc">${p.short_description}</div>
      <div class="install-row">
        <button class="install" ${disabled} data-id="${p.id}">${buttonLabel}</button>
        <button class="server-pack" ${disabled} data-id="${p.id}" title="Download server-side mods as a zip" aria-label="Download server pack">↓</button>
      </div>
    `;

    card.querySelector("button.install").addEventListener("click", () => installProfile(p));
    card
      .querySelector("button.server-pack")
      .addEventListener("click", () => downloadServerPack(p));
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

async function downloadServerPack(profile) {
  if (profile.not_implemented_in_phase_1) return;
  const defaultName = `${profile.id}-server-pack-mc${profile.minecraft_version}.zip`;
  let target;
  try {
    target = await saveDialog({
      title: "Save server pack",
      defaultPath: defaultName,
      filters: [{ name: "Zip archive", extensions: ["zip"] }],
    });
  } catch (err) {
    logLine(`Couldn't open save dialog: ${err}`, "bad");
    return;
  }
  if (!target) return;

  logEl.innerHTML = "";
  logLine(`Building server pack for "${profile.name}"...`);
  try {
    const report = await invoke("download_server_pack", {
      profileId: profile.id,
      targetPath: target,
    });
    const skippedNote = report.skipped.length
      ? ` (${report.skipped.length} skipped)`
      : "";
    logLine(
      `Done. ${report.mods_included} server mod${report.mods_included === 1 ? "" : "s"}${skippedNote} → ${report.output_path}`,
      "ok",
    );
  } catch (err) {
    logLine(`Server pack failed: ${err}`, "bad");
  }
}

let pendingUpdates = [];

function renderUpdates() {
  updatesListEl.innerHTML = "";
  if (pendingUpdates.length === 0) {
    const empty = document.createElement("div");
    empty.className = "update-row";
    empty.innerHTML = `<div class="name">Everything is up to date.</div><div></div><div></div>`;
    updatesListEl.appendChild(empty);
    updateAllBtn.disabled = true;
    return;
  }
  updateAllBtn.disabled = false;
  pendingUpdates.forEach((u, idx) => {
    const row = document.createElement("div");
    row.className = "update-row";
    row.innerHTML = `
      <div>
        <div class="name">${u.title}</div>
        <div class="versions">${u.current_version} → ${u.latest_version}</div>
      </div>
      <span class="kind">${u.kind}</span>
      <button data-idx="${idx}">Update</button>
    `;
    row.querySelector("button").addEventListener("click", async (e) => {
      const btn = e.currentTarget;
      btn.disabled = true;
      btn.textContent = "...";
      try {
        await invoke("apply_update", { update: u, minecraftDir: activeMcDir() });
        logLine(`Updated ${u.title} → ${u.latest_version}`, "ok");
        pendingUpdates.splice(idx, 1);
        renderUpdates();
      } catch (err) {
        logLine(`Update failed for ${u.title}: ${err}`, "bad");
        btn.disabled = false;
        btn.textContent = "Update";
      }
    });
    updatesListEl.appendChild(row);
  });
}

async function runCheckForUpdates() {
  checkUpdatesBtn.disabled = true;
  checkUpdatesBtn.textContent = "Checking...";
  logEl.innerHTML = "";
  try {
    pendingUpdates = await invoke("check_for_updates", {
      minecraftDir: activeMcDir(),
    });
    updatesPanel.hidden = false;
    renderUpdates();
    logLine(`Check complete — ${pendingUpdates.length} update(s) available.`, pendingUpdates.length ? "ok" : "info");
  } catch (err) {
    logLine(`Check failed: ${err}`, "bad");
  } finally {
    checkUpdatesBtn.disabled = false;
    checkUpdatesBtn.textContent = "Check for updates";
  }
}

async function runUpdateAll() {
  updateAllBtn.disabled = true;
  const list = [...pendingUpdates];
  for (const u of list) {
    try {
      await invoke("apply_update", { update: u, minecraftDir: activeMcDir() });
      logLine(`Updated ${u.title} → ${u.latest_version}`, "ok");
      const idx = pendingUpdates.indexOf(u);
      if (idx >= 0) pendingUpdates.splice(idx, 1);
      renderUpdates();
    } catch (err) {
      logLine(`Update failed for ${u.title}: ${err}`, "bad");
    }
  }
  updateAllBtn.disabled = false;
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
