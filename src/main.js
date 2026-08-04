const { invoke } = window.__TAURI__.core;
const { revealItemInDir } = window.__TAURI__.opener;

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

function initVsd() {
  const toggleBtn = document.getElementById("vsd-toggle");
  toggleBtn.addEventListener("click", toggleVsd);
  // Check initial state
  checkVsdStatus();
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
      await invoke("vsd_start");
      updateVsdUi(true);
      clearLog();
      appendLog("[Server starting...]\n");
      startLogPolling();
    }
  } catch (err) {
    appendLog(`[Error] ${err}\n`);
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

function appendLog(text) {
  const logEl = document.getElementById("vsd-log-content");
  logEl.textContent += text;
  // Auto-scroll to bottom
  const logContainer = document.getElementById("vsd-log");
  logContainer.scrollTop = logContainer.scrollHeight;
}

function clearLog() {
  document.getElementById("vsd-log-content").textContent = "";
}

window.addEventListener("DOMContentLoaded", () => {
  initTabs();
  loadAppInfo();
  initVsd();
  document.getElementById("app-config-path").addEventListener("click", openConfigDir);
});
