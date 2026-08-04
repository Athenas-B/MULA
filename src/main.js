const { invoke } = window.__TAURI__.core;
const { revealItemInDir } = window.__TAURI__.opener;
const { open: openDialog } = window.__TAURI__.dialog;

// Tab switching
function initTabs() {
  const tabs = document.querySelectorAll(".tab");
  const panels = document.querySelectorAll(".tab-panel");

  tabs.forEach((tab) => {
    tab.addEventListener("click", () => {
      tabs.forEach((t) => t.classList.remove("active"));
      panels.forEach((p) => p.classList.remove("active"));

      tab.classList.add("active");
      document.getElementById("tab-" + tab.dataset.tab).classList.add("active");
    });
  });
}

// Load system info from Rust backend
async function loadAppInfo() {
  try {
    const info = await invoke("get_app_info");
    document.getElementById("app-version").textContent = info.version;
    document.getElementById("app-platform").textContent = info.platform;
    document.getElementById("app-arch").textContent = info.arch;
    document.getElementById("app-config-path").textContent = info.config_path;
  } catch (e) {
    console.error("Failed to load app info:", e);
  }
}

// Open config directory in file explorer
async function openConfigDir(e) {
  e.preventDefault();
  try {
    const path = await invoke("open_config_dir");
    await revealItemInDir(path);
  } catch (err) {
    console.error("Failed to open config directory:", err);
  }
}

// ── VSD Server ──
let vsdRunning = false;
let vsdLogInterval = null;

async function initVsd() {
  const toggleBtn = document.getElementById("vsd-toggle");
  toggleBtn.addEventListener("click", toggleVsd);
  document.getElementById("vsd-download-dir").addEventListener("click", changeDownloadDir);
  document.getElementById("vsd-install-chrome").addEventListener("click", () => installExtension("chrome"));
  document.getElementById("vsd-install-firefox").addEventListener("click", () => installExtension("firefox"));
  document.getElementById("vsd-autostart").addEventListener("change", toggleAutostart);
  // Check initial state
  checkVsdStatus();
  loadDownloadDir();
  loadAutostart();
}

async function loadAutostart() {
  try {
    const enabled = await invoke("vsd_get_autostart");
    document.getElementById("vsd-autostart").checked = enabled;
    if (enabled && !vsdRunning) {
      // Auto-start the server
      updateVsdUi(true);
      clearLog();
      appendLog("[Autostart: starting server...]\n");
      startLogPolling();
      await invoke("vsd_start");
    }
  } catch (e) {
    console.error("Failed to load autostart setting:", e);
  }
}

async function toggleAutostart() {
  const enabled = document.getElementById("vsd-autostart").checked;
  try {
    await invoke("vsd_set_autostart", { enabled });
  } catch (e) {
    console.error("Failed to save autostart setting:", e);
  }
}

async function installExtension(browser) {
  const btn = document.getElementById(`vsd-install-${browser}`);
  const originalText = btn.querySelector(".btn-text").textContent;
  btn.disabled = true;
  btn.querySelector(".btn-text").textContent = "Building...";
  clearLog();
  try {
    const extPath = await invoke("vsd_install_extension", { browser });
    btn.querySelector(".btn-text").textContent = "Done";
    if (browser === "chrome") {
      appendLog(`Extension built successfully!\n\n`);
      appendLog(`Folder opened: ${extPath}\n\n`);
      appendLog(`To install in Chrome:\n`);
      appendLog(`  1. Open Chrome and go to chrome://extensions\n`);
      appendLog(`  2. Enable "Developer mode" (toggle in top-right)\n`);
      appendLog(`  3. Click "Load unpacked"\n`);
      appendLog(`  4. Select the folder that was just opened\n`);
    } else {
      appendLog(`Extension built successfully!\n\n`);
      appendLog(`Folder opened: ${extPath}\n\n`);
      appendLog(`To install in Firefox:\n`);
      appendLog(`  1. Open Firefox and go to about:debugging\n`);
      appendLog(`  2. Click "This Firefox" in the sidebar\n`);
      appendLog(`  3. Click "Load Temporary Add-on..."\n`);
      appendLog(`  4. Select manifest.json from the opened folder\n`);
    }
  } catch (err) {
    btn.querySelector(".btn-text").textContent = "Error";
    appendLog(`Extension build failed: ${err}\n`);
  } finally {
    setTimeout(() => {
      btn.querySelector(".btn-text").textContent = originalText;
      btn.disabled = false;
    }, 3000);
  }
}

async function loadDownloadDir() {
  try {
    const dir = await invoke("vsd_get_download_dir");
    document.getElementById("vsd-dir-label").textContent = dir;
    document.getElementById("vsd-download-dir").title = dir;
  } catch (e) {
    console.error("Failed to load download dir:", e);
  }
}

async function changeDownloadDir() {
  try {
    const currentDir = document.getElementById("vsd-dir-label").textContent;
    const selected = await openDialog({
      directory: true,
      title: "Select download location",
      defaultPath: currentDir !== "..." ? currentDir : undefined,
    });
    if (selected) {
      await invoke("vsd_set_download_dir", { path: selected });
      document.getElementById("vsd-dir-label").textContent = selected;
      document.getElementById("vsd-download-dir").title = selected;
      if (vsdRunning) {
        appendLog(`[Download dir changed to: ${selected}]\n`);
      }
    }
  } catch (e) {
    console.error("Failed to change download dir:", e);
  }
}

async function checkVsdStatus() {
  try {
    const running = await invoke("vsd_is_running");
    updateVsdUi(running);
    if (running && !vsdLogInterval) {
      startLogPolling();
    }
  } catch (e) {
    console.error("Failed to check VSD status:", e);
  }
}

async function toggleVsd() {
  const toggleBtn = document.getElementById("vsd-toggle");
  toggleBtn.disabled = true;

  try {
    if (vsdRunning) {
      await invoke("vsd_stop");
      updateVsdUi(false);
      stopLogPolling();
      appendLog("\n[Server stopped]");
    } else {
      // Update UI immediately before the async start
      updateVsdUi(true);
      clearLog();
      appendLog("[Starting server...]\n");
      startLogPolling();

      await invoke("vsd_start");
    }
  } catch (err) {
    appendLog(`[Error] ${err}\n`);
    // Revert UI if start failed
    if (!vsdRunning) {
      updateVsdUi(false);
      stopLogPolling();
    }
  } finally {
    toggleBtn.disabled = false;
  }
}

function updateVsdUi(running) {
  vsdRunning = running;
  const toggleBtn = document.getElementById("vsd-toggle");
  if (running) {
    toggleBtn.classList.remove("stopped");
    toggleBtn.classList.add("running");
    toggleBtn.querySelector(".toggle-label").textContent = "Stop";
    toggleBtn.title = "Stop VSD Server";
  } else {
    toggleBtn.classList.remove("running");
    toggleBtn.classList.add("stopped");
    toggleBtn.querySelector(".toggle-label").textContent = "Start";
    toggleBtn.title = "Start VSD Server";
  }
}

function startLogPolling() {
  if (vsdLogInterval) return;
  vsdLogInterval = setInterval(pollLogs, 1000);
}

function stopLogPolling() {
  if (vsdLogInterval) {
    clearInterval(vsdLogInterval);
    vsdLogInterval = null;
  }
}

async function pollLogs() {
  try {
    const lines = await invoke("vsd_get_logs");
    if (lines && lines.length > 0) {
      appendLog(lines.join(""));
    }
  } catch (e) {
    // Server may have stopped
  }
}

// ANSI color code to CSS color mapping
const ANSI_COLORS = {
  30: "#4d4d4d", 31: "#e74c3c", 32: "#2ecc71", 33: "#f39c12",
  34: "#3498db", 35: "#9b59b6", 36: "#1abc9c", 37: "#ecf0f1",
  90: "#7f8c8d", 91: "#ff6b6b", 92: "#55efc4", 93: "#ffeaa7",
  94: "#74b9ff", 95: "#a29bfe", 96: "#81ecec", 97: "#ffffff",
};

function ansiToHtml(text) {
  let html = "";
  let i = 0;
  let openSpan = false;

  while (i < text.length) {
    if (text[i] === "\x1b" && text[i + 1] === "[") {
      // Parse ANSI sequence
      let j = i + 2;
      while (j < text.length && text[j] !== "m") j++;
      const codes = text.slice(i + 2, j).split(";").map(Number);
      i = j + 1;

      if (openSpan) {
        html += "</span>";
        openSpan = false;
      }

      for (const code of codes) {
        if (code === 0) {
          // Reset
        } else if (code === 1) {
          html += '<span style="font-weight:bold">';
          openSpan = true;
        } else if (ANSI_COLORS[code]) {
          html += `<span style="color:${ANSI_COLORS[code]}">`;
          openSpan = true;
        }
      }
    } else if (text[i] === "<") {
      html += "&lt;";
      i++;
    } else if (text[i] === ">") {
      html += "&gt;";
      i++;
    } else if (text[i] === "&") {
      html += "&amp;";
      i++;
    } else {
      html += text[i];
      i++;
    }
  }

  if (openSpan) html += "</span>";
  return html;
}

function appendLog(text) {
  const logEl = document.getElementById("vsd-log-content");
  logEl.innerHTML += ansiToHtml(text);
  // Auto-scroll to bottom
  const logContainer = document.getElementById("vsd-log");
  logContainer.scrollTop = logContainer.scrollHeight;
}

function clearLog() {
  document.getElementById("vsd-log-content").innerHTML = "";
}

window.addEventListener("DOMContentLoaded", () => {
  initTabs();
  loadAppInfo();
  initVsd();
  initDriveTest();
  document.getElementById("app-config-path").addEventListener("click", openConfigDir);
});

// ── Drive Test ──
let cachedDrives = [];

async function initDriveTest() {
  const select = document.getElementById("drive-select");
  const refreshBtn = document.getElementById("drive-refresh");
  const toggle = document.getElementById("drive-toggle");
  const content = document.getElementById("drive-info-content");
  if (!select || !refreshBtn) return;

  select.addEventListener("change", onDriveSelect);
  refreshBtn.addEventListener("click", loadDrives);
  if (toggle && content) {
    toggle.addEventListener("click", () => {
      const collapsed = content.classList.toggle("collapsed");
      toggle.setAttribute("aria-expanded", String(!collapsed));
      toggle.querySelector(".toggle-icon").textContent = collapsed ? "+" : "-";
    });
  }
  await loadDrives();
}

async function loadDrives() {
  const select = document.getElementById("drive-select");
  const empty = document.getElementById("drive-empty");
  const details = document.getElementById("drive-details");
  const tbody = document.querySelector("#drive-info-table tbody");

  select.disabled = true;
  select.innerHTML = '<option value="">Loading drives...</option>';

  try {
    const drives = await invoke("list_physical_drives");
    cachedDrives = drives || [];
    select.innerHTML = '';

    if (cachedDrives.length === 0) {
      select.innerHTML = '<option value="">No physical drives found</option>';
      empty.textContent = "No physical drives found.";
      empty.classList.remove("hidden");
      details.classList.add("hidden");
      return;
    }

    const defaultOpt = document.createElement("option");
    defaultOpt.value = "";
    defaultOpt.textContent = "Select a drive...";
    select.appendChild(defaultOpt);

    for (const d of cachedDrives) {
      const opt = document.createElement("option");
      opt.value = d.id;
      const letters = d.drive_letters?.length ? ` [${d.drive_letters.join(" ")}]` : "";
      opt.textContent = `${d.model} (${d.size_text})${letters}`;
      select.appendChild(opt);
    }

    empty.classList.remove("hidden");
    empty.textContent = "Select a drive to see details.";
    details.classList.add("hidden");
    tbody.innerHTML = "";
  } catch (err) {
    console.error("Failed to list drives:", err);
    select.innerHTML = '<option value="">Failed to load drives</option>';
    empty.textContent = `Error: ${err}`;
    empty.classList.remove("hidden");
    details.classList.add("hidden");
  } finally {
    select.disabled = false;
  }
}

function onDriveSelect() {
  const select = document.getElementById("drive-select");
  const id = select.value;
  const empty = document.getElementById("drive-empty");
  const details = document.getElementById("drive-details");
  const tbody = document.querySelector("#drive-info-table tbody");

  if (!id) {
    empty.classList.remove("hidden");
    details.classList.add("hidden");
    tbody.innerHTML = "";
    return;
  }

  const drive = cachedDrives.find((d) => d.id === id);
  if (!drive) {
    empty.classList.remove("hidden");
    details.classList.add("hidden");
    return;
  }

  const fmtList = (arr) => (arr?.length ? arr.join(", ") : "None");

  const fields = [
    ["Device", drive.device_id],
    ["Vendor", drive.vendor],
    ["Model", drive.model],
    ["Serial", drive.serial || "N/A"],
    ["Type", drive.type],
    ["Bus", drive.bus_type],
    ["Connection speed", drive.connection_speed],
    ["Media type", drive.media_type],
    ["Interface", drive.interface_type],
    ["Size", `${drive.size_text} (${drive.size.toLocaleString()} bytes)`],
    ["Partitions", drive.partitions.toString()],
    ["Drive letters", fmtList(drive.drive_letters)],
    ["Mount points", fmtList(drive.mount_points)],
    ["Health", drive.health_status || "N/A"],
    ["SMART capable", drive.smart_capable || "Unknown"],
    ["TRIM capable", drive.trim_capable || "Unknown"],
    ["Status", drive.status],
    ["Firmware", drive.firmware || "N/A"],
    ["PNP ID", drive.pnp_device_id || "N/A"],
  ];

  tbody.innerHTML = fields
    .map(([label, value]) => `<tr><td class="label">${label}</td><td class="value">${value || "N/A"}</td></tr>`)
    .join("");

  empty.classList.add("hidden");
  details.classList.remove("hidden");
}
