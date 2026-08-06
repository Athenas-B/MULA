const { invoke } = window.__TAURI__.core;
const { revealItemInDir } = window.__TAURI__.opener;
const { open: openDialog } = window.__TAURI__.dialog;

// Forward console messages to the Rust file logger
(function setupConsoleLogging() {
  const forward = (level, original) => {
    return function (...args) {
      original.apply(console, args);
      try {
        const message = args.map((a) => (typeof a === "object" ? JSON.stringify(a) : String(a))).join(" ");
        if (message) {
          invoke("log_message", { level, message }).catch(() => {});
        }
      } catch (_) {}
    };
  };
  console.log = forward("info", console.log);
  console.info = forward("info", console.info);
  console.warn = forward("warn", console.warn);
  console.error = forward("error", console.error);
})();

// ── Theme (light/dark) ──
const THEME_STORAGE_KEY = "mula-theme";

function getStoredTheme() {
  try {
    return localStorage.getItem(THEME_STORAGE_KEY);
  } catch (_) {
    return null;
  }
}

function prefersDark() {
  return window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches;
}

function applyTheme(theme) {
  document.documentElement.setAttribute("data-theme", theme);
  const toggle = document.getElementById("app-theme-toggle");
  const label = document.getElementById("app-theme-label");
  if (toggle) toggle.checked = theme === "dark";
  if (label) label.textContent = theme === "dark" ? "Dark mode" : "Light mode";
}

// Apply the effective theme immediately to avoid a flash of the wrong theme.
applyTheme(getStoredTheme() || (prefersDark() ? "dark" : "light"));

function initTheme() {
  const toggle = document.getElementById("app-theme-toggle");
  applyTheme(getStoredTheme() || (prefersDark() ? "dark" : "light"));

  toggle?.addEventListener("change", () => {
    const theme = toggle.checked ? "dark" : "light";
    try {
      localStorage.setItem(THEME_STORAGE_KEY, theme);
    } catch (_) {}
    applyTheme(theme);
  });

  // Follow OS theme changes live, unless the user has made an explicit choice.
  window.matchMedia?.("(prefers-color-scheme: dark)").addEventListener("change", (e) => {
    if (getStoredTheme()) return;
    applyTheme(e.matches ? "dark" : "light");
  });
}

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

async function loadAppAutostart() {
  try {
    const enabled = await invoke("get_autostart");
    document.getElementById("app-autostart").checked = enabled;
  } catch (e) {
    console.error("Failed to load autostart setting:", e);
  }
}

async function toggleAppAutostart() {
  const checkbox = document.getElementById("app-autostart");
  const enabled = checkbox.checked;
  try {
    await invoke("set_autostart", { enabled });
  } catch (e) {
    checkbox.checked = !enabled;
    console.error("Failed to update autostart setting:", e);
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
  try {
    const extPath = await invoke("vsd_install_extension", { browser });
    btn.querySelector(".btn-text").textContent = "Done";
    if (browser === "chrome") {
      const manifestPath = `${extPath}\\chrome\\manifest.json`;
      appendLog(`Extension built successfully!\n\n`);
      appendLog(`Folder opened: ${extPath}\n\n`);
      appendLog(`To install in Chrome:\n`);
      appendLog(`  1. Open Chrome and go to chrome://extensions\n`);
      appendLog(`  2. Enable "Developer mode" (toggle in top-right)\n`);
      appendLog(`  3. Click "Load unpacked"\n`);
      appendLog(`  4. Select the folder: ${extPath}\\chrome (contains manifest.json)\n`);
      appendLog(`     Manifest path: ${manifestPath}\n`);
    } else {
      const manifestPath = `${extPath}\\firefox\\manifest.json`;
      appendLog(`Extension built successfully!\n\n`);
      appendLog(`Folder opened: ${extPath}\n\n`);
      appendLog(`To install in Firefox:\n`);
      appendLog(`  1. Open Firefox and go to about:debugging\n`);
      appendLog(`  2. Click "This Firefox" in the sidebar\n`);
      appendLog(`  3. Click "Load Temporary Add-on..."\n`);
      appendLog(`  4. Select the manifest file: ${manifestPath}\n`);
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
  initTheme();
  loadAppInfo();
  loadAppAutostart();
  initVsd();
  initDriveTest();
  initWallchanger();
  document.getElementById("app-config-path").addEventListener("click", openConfigDir);
  document.getElementById("app-autostart")?.addEventListener("change", toggleAppAutostart);
});

// ── Drive Test ──
let cachedDrives = [];

async function initDriveTest() {
  const select = document.getElementById("drive-select");
  const refreshBtn = document.getElementById("drive-refresh");
  const toggle = document.getElementById("drive-toggle");
  const content = document.getElementById("drive-info-content");
  const smartToggle = document.getElementById("smart-toggle");
  const smartContent = document.getElementById("smart-content");
  const smartRetry = document.getElementById("smart-retry-admin");
  const smartRunTest = document.getElementById("smart-run-test");
  const smartCheckStatus = document.getElementById("smart-check-status");
  const formatBtn = document.getElementById("drive-format-btn");
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
  if (smartToggle && smartContent) {
    smartToggle.addEventListener("click", () => onSmartToggle(smartToggle, smartContent));
  }
  if (smartRetry) {
    smartRetry.addEventListener("click", () => onSmartRetryAdmin());
  }
  if (smartRunTest) {
    smartRunTest.addEventListener("click", () => onRunDriveTest());
  }
  if (smartCheckStatus) {
    smartCheckStatus.addEventListener("click", () => onCheckDriveTestStatus());
  }
  if (formatBtn) {
    formatBtn.addEventListener("click", () => onFormatDrive());
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

let smartLoadedId = null;
let testPollInterval = null;
let testPollNoProgressCount = 0;
let formatTimer = null;

function stopFormatTimer() {
  if (formatTimer) {
    clearInterval(formatTimer);
    formatTimer = null;
  }
}

function startFormatTimer(statusEl) {
  stopFormatTimer();
  const start = Date.now();
  formatTimer = setInterval(() => {
    const elapsed = Math.floor((Date.now() - start) / 1000);
    const m = Math.floor(elapsed / 60).toString().padStart(2, "0");
    const s = (elapsed % 60).toString().padStart(2, "0");
    if (statusEl) {
      statusEl.textContent = `Formatting... (elapsed ${m}:${s}). Do not close the app.`;
      statusEl.className = "drive-format-status";
    }
  }, 1000);
}

function stopTestProgressPolling() {
  if (testPollInterval) {
    clearInterval(testPollInterval);
    testPollInterval = null;
  }
  testPollNoProgressCount = 0;
}

function needsAdminRights(text) {
  const t = String(text).toLowerCase();
  return (
    t.includes("error=5") ||
    t.includes("access is denied") ||
    t.includes("access denied") ||
    t.includes("admin rights") ||
    t.includes("requires admin") ||
    t.includes("administrator") ||
    t.includes("elevation") ||
    t.includes("permission")
  );
}

async function onSmartRetryAdmin() {
  const select = document.getElementById("drive-select");
  const smartData = document.getElementById("smart-data");
  const smartStatus = document.getElementById("smart-status");
  const smartAdmin = document.getElementById("smart-admin");
  const id = select.value;
  if (!id) return;

  if (smartAdmin) smartAdmin.classList.add("hidden");
  smartData.textContent = "";
  smartStatus.textContent = "Loading SMART data with administrator privileges...";
  smartStatus.classList.remove("hidden");
  try {
    const data = await invoke("get_drive_smart_elevated", { id });
    smartData.textContent = data;
    smartLoadedId = id;
  } catch (err) {
    console.error(err);
    smartData.textContent = `Error: ${err}`;
  } finally {
    smartStatus.classList.add("hidden");
  }
}

async function onRunDriveTest() {
  const select = document.getElementById("drive-select");
  const testCheckboxes = document.querySelectorAll('input[name="smart-test-type"]:checked');
  const saveBefore = document.querySelector('input[name="smart-test-save"][value="before"]')?.checked;
  const saveAfter = document.querySelector('input[name="smart-test-save"][value="after"]')?.checked;
  const testStatus = document.getElementById("smart-test-status");
  const runBtn = document.getElementById("smart-run-test");
  const id = select.value;
  const types = Array.from(testCheckboxes).map((cb) => cb.value);

  if (!id || types.length === 0) {
    if (testStatus) {
      testStatus.textContent = "Select at least one test type to run";
      testStatus.className = "smart-test-status error";
    }
    return;
  }

  stopTestProgressPolling();

  if (testStatus) {
    testStatus.textContent = "Starting tests with administrator privileges...";
    testStatus.className = "smart-test-status";
  }
  if (runBtn) runBtn.disabled = true;

  const lines = [];
  let hasError = false;

  const appendLine = (line, isError) => {
    lines.push(line);
    if (isError) hasError = true;
    if (testStatus) testStatus.textContent = lines.join("\n");
  };

  try {
    if (saveBefore) {
      const path = await invoke("save_smart_snapshot", { id, label: "before" });
      appendLine(`SMART snapshot saved before tests:\n  ${path}`);
    }

    for (const type of types) {
      try {
        const data = await invoke("run_drive_test_elevated", { id, testType: type });
        if (String(data).toLowerCase().includes("failed")) {
          appendLine(`${type}: failed\n  ${data}`, true);
        } else {
          appendLine(`${type}: started\n  ${data}`);
        }
      } catch (err) {
        appendLine(`${type}: error - ${err}`, true);
      }
    }

    if (saveAfter) {
      const path = await invoke("save_smart_snapshot", { id, label: "after" });
      appendLine(`SMART snapshot saved after tests:\n  ${path}`);
    }

    if (testStatus) {
      testStatus.className = hasError ? "smart-test-status error" : "smart-test-status success";
    }

    if (!hasError) {
      setTimeout(() => startTestProgressPolling(id), 2000);
    }
  } catch (err) {
    console.error(err);
    if (testStatus) {
      testStatus.textContent = `Error: ${err}`;
      testStatus.className = "smart-test-status error";
    }
  } finally {
    if (runBtn) runBtn.disabled = false;
  }
}

function extractSelfTestProgress(full) {
  const statusRegex = /(?:Self-test execution status|Self-test status):[\s\S]*?(?=\n[A-Z]|$)/i;
  const logRegex = /(?:SMART (?:Extended )?Self-test log|Self-test Log|No Self-tests Logged)[\s\S]*?(?=\n(?:SMART|={3,})|$)/i;
  const statusMatch = full.match(statusRegex);
  const logMatch = full.match(logRegex);
  const status = statusMatch ? statusMatch[0].trim() : "Self-test status not available";
  const log = logMatch ? logMatch[0].trim() : "No self-test log available";
  const lowerStatus = status.toLowerCase();
  const inProgress =
    lowerStatus.includes("in progress") &&
    !lowerStatus.includes("no self-test in progress") &&
    !lowerStatus.includes("no selftest in progress");
  const hasFailed = lowerStatus.includes("failed");
  const progressMatch = status.match(/(\d+)%\s*(?:remaining|left)/i);
  const progress = progressMatch ? `${progressMatch[1]}% remaining` : null;
  return { status, log, text: `${status}\n\n${log}`, inProgress, hasFailed, progress };
}

function startTestProgressPolling(id) {
  stopTestProgressPolling();
  const testStatus = document.getElementById("smart-test-status");
  if (testStatus) {
    testStatus.textContent = "Monitoring self-test progress...";
    testStatus.className = "smart-test-status";
  }
  pollTestStatus(id);
  testPollInterval = setInterval(() => pollTestStatus(id), 10000);
}

async function pollTestStatus(id) {
  const select = document.getElementById("drive-select");
  const testStatus = document.getElementById("smart-test-status");
  if (!id || select.value !== id) {
    stopTestProgressPolling();
    return;
  }

  try {
    const full = await invoke("get_drive_smart_elevated", { id });
    const { status, log, inProgress, hasFailed, progress } = extractSelfTestProgress(full);

    let header;
    if (inProgress) {
      header = progress
        ? `Monitoring self-test progress... (${progress}, updates every 10s)`
        : "Monitoring self-test progress... (updates every 10s)";
    } else if (hasFailed) {
      header = "Self-test failed";
    } else {
      header = "Self-test finished";
    }

    if (testStatus) {
      testStatus.textContent = `${header}\n\n${status}\n\n${log}`;
      testStatus.className = hasFailed ? "smart-test-status error" : "smart-test-status";
    }

    if (hasFailed) {
      stopTestProgressPolling();
      return;
    }

    if (inProgress) {
      testPollNoProgressCount = 0;
    } else {
      testPollNoProgressCount += 1;
      if (testPollNoProgressCount >= 2) {
        if (testStatus) testStatus.className = "smart-test-status success";
        stopTestProgressPolling();
      }
    }
  } catch (err) {
    console.error(err);
    if (testStatus) {
      testStatus.textContent = `Error checking status: ${err}`;
      testStatus.className = "smart-test-status error";
    }
    stopTestProgressPolling();
  }
}

async function onCheckDriveTestStatus() {
  const select = document.getElementById("drive-select");
  const testStatus = document.getElementById("smart-test-status");
  const checkBtn = document.getElementById("smart-check-status");
  const id = select.value;
  if (!id) return;

  if (testStatus) {
    testStatus.textContent = "Checking test status with administrator privileges...";
    testStatus.className = "smart-test-status";
  }
  if (checkBtn) checkBtn.disabled = true;

  try {
    const full = await invoke("get_drive_smart_elevated", { id });
    const { text, inProgress, hasFailed } = extractSelfTestProgress(full);
    if (testStatus) {
      testStatus.textContent = text;
      testStatus.className = hasFailed ? "smart-test-status error" : inProgress ? "smart-test-status" : "smart-test-status success";
    }
  } catch (err) {
    console.error(err);
    if (testStatus) {
      testStatus.textContent = `Error: ${err}`;
      testStatus.className = "smart-test-status error";
    }
  } finally {
    if (checkBtn) checkBtn.disabled = false;
  }
}

async function onFormatDrive() {
  const select = document.getElementById("drive-select");
  const formatStatus = document.getElementById("drive-format-status");
  const formatBtn = document.getElementById("drive-format-btn");
  const id = select.value;
  if (!id) return;

  stopTestProgressPolling();

  const drive = cachedDrives.find((d) => d.id === id);
  const driveLabel = drive ? `${drive.vendor || ""} ${drive.model || ""} ${drive.serial || ""}`.trim() || id : id;

  const confirmed = window.confirm(
    `WARNING: This will permanently erase ALL data on the selected drive.\n\n` +
      `Drive: ${driveLabel}\n\n` +
      `This action will remove all partitions, create a new single partition, and perform a full format.\n` +
      `It cannot be undone.\n\nDo you want to continue?`
  );
  if (!confirmed) return;

  if (formatBtn) formatBtn.disabled = true;
  startFormatTimer(formatStatus);

  try {
    const result = await invoke("format_drive", { id });
    stopFormatTimer();
    await loadDrives();
    if (select) select.value = id;
    onDriveSelect();
    if (formatStatus) {
      formatStatus.textContent = result;
      formatStatus.className = "drive-format-status success";
    }
  } catch (err) {
    stopFormatTimer();
    console.error(err);
    if (formatStatus) {
      formatStatus.textContent = `Error: ${err}`;
      formatStatus.className = "drive-format-status error";
    }
  } finally {
    if (formatBtn) formatBtn.disabled = false;
  }
}

async function onSmartToggle(toggle, content) {
  const select = document.getElementById("drive-select");
  const smartData = document.getElementById("smart-data");
  const smartStatus = document.getElementById("smart-status");
  const smartAdmin = document.getElementById("smart-admin");
  const id = select.value;

  const collapsed = content.classList.toggle("collapsed");
  toggle.setAttribute("aria-expanded", String(!collapsed));
  toggle.querySelector(".toggle-icon").textContent = collapsed ? "+" : "-";

  if (smartAdmin) smartAdmin.classList.add("hidden");

  if (!collapsed && id && smartLoadedId !== id) {
    smartData.textContent = "";
    smartStatus.textContent = "Loading SMART data...";
    smartStatus.classList.remove("hidden");
    try {
      const data = await invoke("get_drive_smart", { id });
      smartData.textContent = data;
      smartLoadedId = id;
      if (smartAdmin && needsAdminRights(data)) {
        smartAdmin.classList.remove("hidden");
      }
    } catch (err) {
      console.error(err);
      smartData.textContent = `Error: ${err}`;
      if (smartAdmin && needsAdminRights(err)) {
        smartAdmin.classList.remove("hidden");
      }
    } finally {
      smartStatus.classList.add("hidden");
    }
  }
}

function onDriveSelect() {
  stopTestProgressPolling();
  const select = document.getElementById("drive-select");
  const id = select.value;
  const empty = document.getElementById("drive-empty");
  const details = document.getElementById("drive-details");
  const tbody = document.querySelector("#drive-info-table tbody");
  const smartToggle = document.getElementById("smart-toggle");
  const smartContent = document.getElementById("smart-content");
  const smartData = document.getElementById("smart-data");
  const smartAdmin = document.getElementById("smart-admin");
  const smartTest = document.getElementById("smart-test");
  const runBtn = document.getElementById("smart-run-test");
  const checkBtn = document.getElementById("smart-check-status");
  const formatSection = document.getElementById("drive-format");
  const formatStatus = document.getElementById("drive-format-status");

  smartLoadedId = null;
  smartData.textContent = "Select a drive and expand to load SMART data.";
  if (smartContent) smartContent.classList.add("collapsed");
  if (smartAdmin) smartAdmin.classList.add("hidden");
  if (smartToggle) {
    smartToggle.setAttribute("aria-expanded", "false");
    smartToggle.querySelector(".toggle-icon").textContent = "+";
  }

  if (!id) {
    if (smartTest) smartTest.classList.add("hidden");
    if (formatSection) {
      formatSection.classList.add("hidden");
      formatStatus.textContent = "";
    }
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

  if (smartTest) {
    smartTest.classList.remove("hidden");
  }
  if (formatSection) {
    formatSection.classList.remove("hidden");
    if (formatStatus) formatStatus.textContent = "";
  }
  if (runBtn) runBtn.disabled = false;
  if (checkBtn) checkBtn.disabled = false;

  empty.classList.add("hidden");
  details.classList.remove("hidden");
}

// ── Wall Changer ──
let wcSettings = null;
let wcSelectedSourceIndex = -1;

async function initWallchanger() {
  document.getElementById("wc-toggle")?.addEventListener("click", wcToggleService);
  document.getElementById("wc-change-now")?.addEventListener("click", wcChangeNow);
  document.getElementById("wc-apply")?.addEventListener("click", wcApply);
  document.getElementById("wc-add-folder")?.addEventListener("click", wcAddFolder);
  document.getElementById("wc-remove-source")?.addEventListener("click", wcRemoveSource);
  document.getElementById("wc-move-up")?.addEventListener("click", () => wcMoveSource(-1));
  document.getElementById("wc-move-down")?.addEventListener("click", () => wcMoveSource(1));

  for (const id of ["wc-interval", "wc-max-level", "wc-rotation", "wc-scaling", "wc-separate-queues", "wc-unique-queues", "wc-stop-slideshow", "wc-one-monitor"]) {
    document.getElementById(id)?.addEventListener("change", () => {
      wcUpdateModelFromUi();
      wcAutoSave();
    });
  }

  await wcLoadSettings();
  await wcLoadStatus();
  await wcLoadMonitors();
}

async function wcLoadSettings() {
  try {
    wcSettings = await invoke("wc_get_settings");
    wcRenderSettings();
    wcRenderSources();
  } catch (err) {
    wcShowMessage(`Error loading settings: ${err}`, "error");
  }
}

function wcRenderSettings() {
  document.getElementById("wc-interval").value = wcSettings.interval_minutes;
  document.getElementById("wc-max-level").value = wcSettings.maximum_source_level;
  document.getElementById("wc-rotation").value = wcSettings.rotation_mode;
  document.getElementById("wc-scaling").value = wcSettings.scaling_mode;
  document.getElementById("wc-separate-queues").checked = wcSettings.use_separate_monitor_queues;
  document.getElementById("wc-unique-queues").checked = wcSettings.keep_image_in_single_monitor_queue;
  document.getElementById("wc-stop-slideshow").checked = wcSettings.disable_windows_slideshow_when_running;
  document.getElementById("wc-one-monitor").checked = wcSettings.change_one_monitor_per_interval;
}

function wcRenderSources() {
  const container = document.getElementById("wc-sources");
  container.innerHTML = "";

  wcSettings.source_folders.forEach((source, index) => {
    const row = document.createElement("div");
    row.className = `wallchanger-source${index === wcSelectedSourceIndex ? " selected" : ""}`;
    row.innerHTML = `
      <input type="checkbox" ${source.enabled ? "checked" : ""} title="Enabled">
      <input type="text" value="${source.path}" placeholder="Folder or URL" readonly>
      <input type="number" min="1" max="10" value="${source.level}" title="Level">
      <label class="wallchanger-option" title="Include subfolders">
        <input type="checkbox" ${source.include_subfolders ? "checked" : ""}>
        <span>Subfolders</span>
      </label>
    `;

    row.addEventListener("click", (e) => {
      if (e.target.tagName === "INPUT") return;
      wcSelectedSourceIndex = index;
      wcRenderSources();
      wcUpdateToolbar();
    });

    row.querySelector('input[type="checkbox"]').addEventListener("change", (e) => {
      wcSettings.source_folders[index].enabled = e.target.checked;
      wcAutoSave();
    });

    row.querySelector('input[type="number"]').addEventListener("change", (e) => {
      wcSettings.source_folders[index].level = Math.min(10, Math.max(1, parseInt(e.target.value, 10) || 1));
      wcAutoSave();
    });

    row.querySelector('input[type="text"]').addEventListener("change", (e) => {
      wcSettings.source_folders[index].path = e.target.value.trim();
      wcAutoSave();
    });

    const subCheck = row.querySelectorAll('input[type="checkbox"]')[1];
    if (subCheck) {
      subCheck.addEventListener("change", (e) => {
        wcSettings.source_folders[index].include_subfolders = e.target.checked;
        wcAutoSave();
      });
    }

    container.appendChild(row);
  });

  wcUpdateToolbar();
}

function wcUpdateToolbar() {
  const hasSelection = wcSelectedSourceIndex >= 0 && wcSelectedSourceIndex < wcSettings.source_folders.length;
  document.getElementById("wc-remove-source").disabled = !hasSelection;
  document.getElementById("wc-move-up").disabled = !hasSelection || wcSelectedSourceIndex === 0;
  document.getElementById("wc-move-down").disabled = !hasSelection || wcSelectedSourceIndex === wcSettings.source_folders.length - 1;
}

function wcUpdateModelFromUi() {
  if (!wcSettings) return;
  wcSettings.interval_minutes = Math.min(1440, Math.max(1, parseInt(document.getElementById("wc-interval").value, 10) || 30));
  wcSettings.maximum_source_level = Math.min(10, Math.max(1, parseInt(document.getElementById("wc-max-level").value, 10) || 10));
  wcSettings.rotation_mode = document.getElementById("wc-rotation").value;
  wcSettings.scaling_mode = document.getElementById("wc-scaling").value;
  wcSettings.use_separate_monitor_queues = document.getElementById("wc-separate-queues").checked;
  wcSettings.keep_image_in_single_monitor_queue = document.getElementById("wc-unique-queues").checked;
  wcSettings.disable_windows_slideshow_when_running = document.getElementById("wc-stop-slideshow").checked;
  wcSettings.change_one_monitor_per_interval = document.getElementById("wc-one-monitor").checked;
}

async function wcAddFolder() {
  try {
    const folder = await openDialog({ directory: true });
    if (!folder) return;
    wcSettings.source_folders.push({
      path: folder,
      enabled: true,
      include_subfolders: true,
      level: 5,
      wallhaven_page_limit: 1,
      wallhaven_purity: "110",
    });
    wcRenderSources();
    await wcAutoSave();
  } catch (err) {
    wcShowMessage(`Error adding folder: ${err}`, "error");
  }
}

function wcRemoveSource() {
  if (wcSelectedSourceIndex < 0) return;
  wcSettings.source_folders.splice(wcSelectedSourceIndex, 1);
  wcSelectedSourceIndex = Math.min(wcSelectedSourceIndex, wcSettings.source_folders.length - 1);
  wcRenderSources();
  wcAutoSave();
}

function wcMoveSource(delta) {
  if (wcSelectedSourceIndex < 0) return;
  const newIndex = wcSelectedSourceIndex + delta;
  if (newIndex < 0 || newIndex >= wcSettings.source_folders.length) return;
  const [moved] = wcSettings.source_folders.splice(wcSelectedSourceIndex, 1);
  wcSettings.source_folders.splice(newIndex, 0, moved);
  wcSelectedSourceIndex = newIndex;
  wcRenderSources();
  wcAutoSave();
}

let wcAutoSaveTimer = null;

function wcAutoSave() {
  clearTimeout(wcAutoSaveTimer);
  wcAutoSaveTimer = setTimeout(async () => {
    try {
      await invoke("wc_save_settings", { settings: wcSettings });
    } catch (err) {
      wcShowMessage(`Error saving settings: ${err}`, "error");
    }
  }, 300);
}

async function wcApply() {
  try {
    wcUpdateModelFromUi();
    await invoke("wc_save_settings", { settings: wcSettings });
    const result = await invoke("wc_apply");
    wcShowMessage(result, "success");
    await wcLoadSettings();
  } catch (err) {
    wcShowMessage(`Error applying wallpaper: ${err}`, "error");
  }
}

async function wcChangeNow() {
  try {
    wcUpdateModelFromUi();
    await invoke("wc_save_settings", { settings: wcSettings });
    const result = await invoke("wc_change_now");
    wcShowMessage(result, "success");
    await wcLoadSettings();
  } catch (err) {
    wcShowMessage(`Error changing wallpaper: ${err}`, "error");
  }
}

async function wcToggleService() {
  try {
    const running = await invoke("wc_toggle_service");
    wcUpdateToggleUi(running);
  } catch (err) {
    wcShowMessage(`Error toggling service: ${err}`, "error");
  }
}

async function wcLoadStatus() {
  try {
    const status = await invoke("wc_get_status");
    wcUpdateToggleUi(status.running);
  } catch (err) {
    wcShowMessage(`Error loading status: ${err}`, "error");
  }
}

function wcUpdateToggleUi(running) {
  const btn = document.getElementById("wc-toggle");
  const label = document.getElementById("wc-toggle-label");
  const status = document.getElementById("wc-status");
  if (running) {
    btn.classList.add("running");
    btn.classList.remove("stopped");
    label.textContent = "Stop";
    status.textContent = "running";
  } else {
    btn.classList.add("stopped");
    btn.classList.remove("running");
    label.textContent = "Start";
    status.textContent = "stopped";
  }
}

async function wcLoadMonitors() {
  try {
    const monitors = await invoke("wc_get_monitors");
    const list = document.getElementById("wc-monitors");
    list.innerHTML = monitors
      .map((m, i) => `<li>Monitor ${i + 1}: ${m.width} x ${m.height}</li>`)
      .join("");
  } catch (err) {
    wcShowMessage(`Error loading monitors: ${err}`, "error");
  }
}

function wcShowMessage(text, kind) {
  const el = document.getElementById("wc-message");
  el.textContent = text;
  el.className = `wallchanger-message${kind ? " " + kind : ""}`;
}
