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

function initVsd() {
  const toggleBtn = document.getElementById("vsd-toggle");
  toggleBtn.addEventListener("click", toggleVsd);
  document.getElementById("vsd-download-dir").addEventListener("click", changeDownloadDir);
  // Check initial state
  checkVsdStatus();
  loadDownloadDir();
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
    const selected = await openDialog({
      directory: true,
      title: "Select download location",
    });
    if (selected) {
      await invoke("vsd_set_download_dir", { path: selected });
      document.getElementById("vsd-dir-label").textContent = selected;
      document.getElementById("vsd-download-dir").title = selected;
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
  document.getElementById("app-config-path").addEventListener("click", openConfigDir);
});
