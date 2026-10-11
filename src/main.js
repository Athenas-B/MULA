const { invoke } = window.__TAURI__.core;
const { revealItemInDir } = window.__TAURI__.opener;
const { open: openDialog } = window.__TAURI__.dialog;
const { listen } = window.__TAURI__.event;

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

async function initVsd() {
  const toggleBtn = document.getElementById("vsd-toggle");
  toggleBtn.addEventListener("click", toggleVsd);
  document.getElementById("vsd-download-dir").addEventListener("click", changeDownloadDir);
  document.getElementById("vsd-install-chrome").addEventListener("click", () => installExtension("chrome"));
  document.getElementById("vsd-install-firefox").addEventListener("click", () => installExtension("firefox"));
  document.getElementById("vsd-autostart").addEventListener("change", toggleAutostart);

  // The backend pushes server output and running/stopped changes (it also handles autostart).
  listen("vsd-log", (event) => appendLog(event.payload));
  listen("vsd-state-changed", (event) => updateVsdUi(event.payload));

  checkVsdStatus();
  loadVsdLogBacklog();
  loadDownloadDir();
  loadAutostart();
}

async function loadAutostart() {
  try {
    document.getElementById("vsd-autostart").checked = await invoke("vsd_get_autostart");
  } catch (e) {
    console.error("Failed to load autostart setting:", e);
  }
}

/** Shows output the server produced before this window loaded. */
async function loadVsdLogBacklog() {
  try {
    const lines = await invoke("vsd_get_logs");
    if (lines?.length) appendLog(lines.join(""));
  } catch (e) {
    console.error("Failed to load VSD log:", e);
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
    await revealItemInDir(extPath).catch(() => {});
    const steps =
      browser === "chrome"
        ? [
            "  1. Open Chrome and go to chrome://extensions",
            '  2. Enable "Developer mode" (toggle in top-right)',
            '  3. Click "Load unpacked"',
            `  4. Select the folder: ${extPath} (contains manifest.json)`,
          ]
        : [
            "  1. Open Firefox and go to about:debugging",
            '  2. Click "This Firefox" in the sidebar',
            '  3. Click "Load Temporary Add-on..."',
            `  4. Select the manifest file: ${extPath}\\manifest.json`,
          ];
    const name = browser === "chrome" ? "Chrome" : "Firefox";
    appendLog(`Extension built successfully: ${extPath}\n\nTo install in ${name}:\n${steps.join("\n")}\n`);
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
    updateVsdUi(await invoke("vsd_is_running"));
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
    } else {
      // Show the new state right away; the backend confirms it with `vsd-state-changed`.
      updateVsdUi(true);
      appendLog("[Starting server...]\n");
      await invoke("vsd_start");
    }
  } catch (err) {
    appendLog(`[Error] ${err}\n`);
    checkVsdStatus();
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
  // A sequence like ESC[1;31m opens one span per code, so track how many are open.
  let openSpans = 0;

  while (i < text.length) {
    if (text[i] === "\x1b" && text[i + 1] === "[") {
      // Parse ANSI sequence
      let j = i + 2;
      while (j < text.length && text[j] !== "m") j++;
      const codes = text.slice(i + 2, j).split(";").map(Number);
      i = j + 1;

      html += "</span>".repeat(openSpans);
      openSpans = 0;

      for (const code of codes) {
        if (code === 0) {
          // Reset
        } else if (code === 1) {
          html += '<span style="font-weight:bold">';
          openSpans++;
        } else if (ANSI_COLORS[code]) {
          html += `<span style="color:${ANSI_COLORS[code]}">`;
          openSpans++;
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

  html += "</span>".repeat(openSpans);
  return html;
}

// The VSD log is append-only; keep the view bounded (each chunk is ~1 s of output).
const MAX_LOG_CHUNKS = 500;

function appendLog(text) {
  const logEl = document.getElementById("vsd-log-content");
  // insertAdjacentHTML appends without re-parsing everything already shown
  // (`innerHTML +=` re-serializes the whole log on every poll).
  logEl.insertAdjacentHTML("beforeend", `<span class="vsd-log-chunk">${ansiToHtml(text)}</span>`);
  while (logEl.childElementCount > MAX_LOG_CHUNKS) {
    logEl.firstElementChild.remove();
  }
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

  // Keep the checkbox in sync when autostart is toggled from the tray menu.
  listen("autostart-changed", (event) => {
    const checkbox = document.getElementById("app-autostart");
    if (checkbox) checkbox.checked = event.payload;
  });

  // Settings changed elsewhere (e.g. the tray's max source level) or normalized by the backend.
  listen("wc-settings-changed", (event) => {
    if (wcSettings) wcApplySettings(event.payload);
  });

  // The service can be started/stopped from the tray, too.
  listen("wc-service-changed", (event) => wcUpdateToggleUi(event.payload));

  // Emitted after every wallpaper change, whether from this tab, the tray or the interval service.
  listen("wc-wallpaper-changed", () => {
    wcLoadMonitorPreview();
    wcLoadQueuePreview();
  });
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

  // Enumerating drives runs PowerShell for a few seconds; wait until the tab is opened.
  const driveTab = document.querySelector('.tab[data-tab="drivetest"]');
  let drivesLoaded = false;
  driveTab?.addEventListener("click", () => {
    if (drivesLoaded) return;
    drivesLoaded = true;
    loadDrives();
  });
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

/** Readable text for a command error: typed `{ kind, message }` objects or plain strings. */
function errorText(err) {
  if (err && typeof err === "object" && "kind" in err) {
    switch (err.kind) {
      case "smartctl_missing":
        return "smartmontools (smartctl) is not installed.";
      case "elevation_declined":
        return "Administrator rights were declined.";
      default:
        return err.message || err.kind;
    }
  }
  return String(err?.message ?? err);
}

// smartmontools is never installed silently: if smartctl is missing, ask the user first.
async function invokeSmart(command, args) {
  try {
    return await invoke(command, args);
  } catch (err) {
    if (err?.kind !== "smartctl_missing") throw err;
    const install = window.confirm(
      "SMART features need smartmontools (smartctl), which is not installed.\n\n" +
        "Install it now with winget? This accepts the smartmontools package license."
    );
    if (!install) {
      throw new Error("smartmontools is not installed. Install it from https://www.smartmontools.org/ to use SMART features.");
    }
    await invoke("install_smartmontools");
    return await invoke(command, args);
  }
}

async function loadSmartData(id, elevated) {
  const smartData = document.getElementById("smart-data");
  const smartStatus = document.getElementById("smart-status");
  const smartAdmin = document.getElementById("smart-admin");

  if (smartAdmin) smartAdmin.classList.add("hidden");
  smartData.textContent = "";
  smartStatus.textContent = elevated ? "Loading SMART data with administrator privileges..." : "Loading SMART data...";
  smartStatus.classList.remove("hidden");
  try {
    smartData.textContent = await invokeSmart("get_drive_smart", { id, elevated });
    smartLoadedId = id;
  } catch (err) {
    console.error(err);
    smartData.textContent = `Error: ${errorText(err)}`;
    if (smartAdmin && err?.kind === "needs_elevation") smartAdmin.classList.remove("hidden");
  } finally {
    smartStatus.classList.add("hidden");
  }
}

async function onSmartRetryAdmin() {
  const id = document.getElementById("drive-select").value;
  if (id) await loadSmartData(id, true);
}

async function onSmartToggle(toggle, content) {
  const id = document.getElementById("drive-select").value;
  const smartAdmin = document.getElementById("smart-admin");

  const collapsed = content.classList.toggle("collapsed");
  toggle.setAttribute("aria-expanded", String(!collapsed));
  toggle.querySelector(".toggle-icon").textContent = collapsed ? "+" : "-";
  if (smartAdmin) smartAdmin.classList.add("hidden");

  if (!collapsed && id && smartLoadedId !== id) await loadSmartData(id, false);
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
      const path = await invokeSmart("save_smart_snapshot", { id, label: "before" });
      appendLine(`SMART snapshot saved before tests:\n  ${path}`);
    }

    for (const type of types) {
      try {
        const output = await invokeSmart("run_drive_test", { id, testType: type });
        appendLine(`${type}: started\n  ${output}`);
      } catch (err) {
        appendLine(`${type}: error - ${errorText(err)}`, true);
      }
    }

    if (saveAfter) {
      const path = await invokeSmart("save_smart_snapshot", { id, label: "after" });
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
      testStatus.textContent = `Error: ${errorText(err)}`;
      testStatus.className = "smart-test-status error";
    }
  } finally {
    if (runBtn) runBtn.disabled = false;
  }
}

/** Text for a self-test status from `get_drive_self_test_status`. */
function formatSelfTestStatus(status) {
  const log = status.log.length
    ? status.log
        .map((e) => `  ${e.test}: ${e.result}${e.hours != null ? ` (at ${e.hours} h)` : ""}`)
        .join("\n")
    : "  No self-tests logged";
  return `Self-test status: ${status.status}\n\nSelf-test log (most recent first):\n${log}`;
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
    const status = await invokeSmart("get_drive_self_test_status", { id });

    let header;
    if (status.in_progress) {
      header =
        status.percent_remaining != null
          ? `Monitoring self-test progress... (${status.percent_remaining}% remaining, updates every 10s)`
          : "Monitoring self-test progress... (updates every 10s)";
    } else if (status.failed) {
      header = "Self-test failed";
    } else {
      header = "Self-test finished";
    }

    if (testStatus) {
      testStatus.textContent = `${header}\n\n${formatSelfTestStatus(status)}`;
      testStatus.className = status.failed ? "smart-test-status error" : "smart-test-status";
    }

    if (status.failed) {
      stopTestProgressPolling();
      return;
    }

    if (status.in_progress) {
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
      testStatus.textContent = `Error checking status: ${errorText(err)}`;
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
    const status = await invokeSmart("get_drive_self_test_status", { id });
    if (testStatus) {
      testStatus.textContent = formatSelfTestStatus(status);
      testStatus.className = status.failed
        ? "smart-test-status error"
        : status.in_progress
          ? "smart-test-status"
          : "smart-test-status success";
    }
  } catch (err) {
    console.error(err);
    if (testStatus) {
      testStatus.textContent = `Error: ${errorText(err)}`;
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
  if (drive?.is_system) return;
  const driveLabel = drive ? `${drive.vendor || ""} ${drive.model || ""} ${drive.serial || ""}`.trim() || id : id;

  const confirmed = window.confirm(
    `WARNING: This will permanently erase ALL data on the selected drive.\n\n` +
      `Drive: ${driveLabel}\n\n` +
      `This action will remove all partitions, create a new single partition, and perform a full format.\n` +
      `Windows will ask for administrator rights.\n` +
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
      formatStatus.textContent = `Error: ${errorText(err)}`;
      formatStatus.className = "drive-format-status error";
    }
  } finally {
    if (formatBtn) formatBtn.disabled = !!cachedDrives.find((d) => d.id === id)?.is_system;
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
    .map(([label, value]) => `<tr><td class="label">${label}</td><td class="value">${escapeHtml(value || "N/A")}</td></tr>`)
    .join("");

  if (smartTest) {
    smartTest.classList.remove("hidden");
  }
  if (formatSection) {
    formatSection.classList.remove("hidden");
    const formatBtn = document.getElementById("drive-format-btn");
    if (formatBtn) formatBtn.disabled = !!drive.is_system;
    if (formatStatus) {
      formatStatus.textContent = drive.is_system
        ? "This disk holds the running Windows installation and cannot be formatted."
        : "";
      formatStatus.className = "drive-format-status";
    }
  }
  if (runBtn) runBtn.disabled = false;
  if (checkBtn) checkBtn.disabled = false;

  empty.classList.add("hidden");
  details.classList.remove("hidden");
}

// ── Wall Changer ──
// The backend owns the settings: the UI sends partial updates (`wc_update_settings`) and renders
// whatever comes back, or arrives with the `wc-settings-changed` event (e.g. from the tray).
let wcSettings = null;
let wcSelectedSourceIndex = -1;
let wcPendingPatch = {};
let wcPatchTimer = null;

const WC_FORM_FIELDS = [
  "wc-interval", "wc-max-level", "wc-wallhaven-api-key", "wc-rotation", "wc-scaling", "wc-bg-color",
  "wc-separate-queues", "wc-unique-queues", "wc-stop-slideshow", "wc-one-monitor",
  "wc-fade-enabled", "wc-fade-duration", "wc-fade-steps",
  "wc-overlay-enabled", "wc-overlay-mode", "wc-overlay-font-size", "wc-overlay-color",
  "wc-overlay-offset-x", "wc-overlay-offset-y", "wc-overlay-backdrop",
];

async function initWallchanger() {
  document.getElementById("wc-toggle")?.addEventListener("click", wcToggleService);
  document.getElementById("wc-change-now")?.addEventListener("click", wcChangeNow);
  document.getElementById("wc-apply")?.addEventListener("click", wcChangeNow);
  document.getElementById("wc-add-folder")?.addEventListener("click", wcAddFolder);
  document.getElementById("wc-add-wallhaven")?.addEventListener("click", wcAddWallhavenUrl);
  document.getElementById("wc-remove-source")?.addEventListener("click", wcRemoveSource);
  document.getElementById("wc-move-up")?.addEventListener("click", () => wcMoveSource(-1));
  document.getElementById("wc-move-down")?.addEventListener("click", () => wcMoveSource(1));

  for (const id of WC_FORM_FIELDS) {
    document.getElementById(id)?.addEventListener("change", () => {
      wcQueuePatch(wcReadForm());
      wcUpdateBgColorVisibility();
      wcUpdateOverlayVisibility();
    });
  }

  document.getElementById("wc-queue-refresh")?.addEventListener("click", wcLoadQueuePreview);
  document.getElementById("wc-monitor-preview-refresh")?.addEventListener("click", wcLoadMonitorPreview);

  await Promise.all([wcLoadSettings(), wcLoadStatus(), wcLoadMonitors(), wcLoadMonitorPreview(), wcLoadQueuePreview()]);
}

async function wcLoadSettings() {
  try {
    wcApplySettings(await invoke("wc_get_settings"));
  } catch (err) {
    wcShowMessage(`Error loading settings: ${err}`, "error");
  }
}

/** Adopts settings from the backend, keeping local edits that haven't been sent yet. */
function wcApplySettings(settings) {
  const next = { ...settings, ...wcPendingPatch };
  const changed = JSON.stringify(next) !== JSON.stringify(wcSettings);
  wcSettings = next;
  if (changed) {
    wcRenderSettings();
    wcRenderSources();
  }
}

/** Records a partial settings update; it is sent after a short pause in editing. */
function wcQueuePatch(patch) {
  if (!wcSettings) return;
  Object.assign(wcPendingPatch, patch);
  Object.assign(wcSettings, patch);
  clearTimeout(wcPatchTimer);
  wcPatchTimer = setTimeout(wcFlushPatch, 300);
}

/** Sends pending edits now; the backend answers with the normalized settings. */
async function wcFlushPatch() {
  clearTimeout(wcPatchTimer);
  if (Object.keys(wcPendingPatch).length === 0) return;
  const patch = wcPendingPatch;
  wcPendingPatch = {};
  try {
    wcApplySettings(await invoke("wc_update_settings", { patch }));
  } catch (err) {
    wcShowMessage(`Error saving settings: ${err}`, "error");
    await wcLoadSettings();
  }
}

/** The form's values as a settings patch (the backend clamps and validates them). */
function wcReadForm() {
  const el = (id) => document.getElementById(id);
  const patch = {
    wallhaven_api_key: el("wc-wallhaven-api-key").value || "",
    rotation_mode: el("wc-rotation").value,
    scaling_mode: el("wc-scaling").value,
    background_color_argb: wcHexToArgb(el("wc-bg-color").value),
    use_separate_monitor_queues: el("wc-separate-queues").checked,
    keep_image_in_single_monitor_queue: el("wc-unique-queues").checked,
    disable_windows_slideshow_when_running: el("wc-stop-slideshow").checked,
    change_one_monitor_per_interval: el("wc-one-monitor").checked,
    show_picture_name_overlay: el("wc-overlay-enabled").checked,
    picture_name_overlay_text_mode: el("wc-overlay-mode").value,
    picture_name_overlay_text_color_argb: wcHexToArgb(el("wc-overlay-color").value),
    picture_name_overlay_use_backdrop: el("wc-overlay-backdrop").checked,
    enable_wallpaper_fade_transition: el("wc-fade-enabled").checked,
  };
  const numbers = {
    interval_minutes: "wc-interval",
    maximum_source_level: "wc-max-level",
    picture_name_overlay_font_size: "wc-overlay-font-size",
    picture_name_overlay_offset_x: "wc-overlay-offset-x",
    picture_name_overlay_offset_y: "wc-overlay-offset-y",
    wallpaper_fade_duration_ms: "wc-fade-duration",
    wallpaper_fade_steps: "wc-fade-steps",
  };
  for (const [key, id] of Object.entries(numbers)) {
    const n = parseInt(el(id).value, 10);
    if (Number.isFinite(n)) patch[key] = n;
  }
  return patch;
}

function wcArgbToHex(argb) {
  const u32 = (argb ?? 0xFF000000) >>> 0;
  const r = (u32 >> 16) & 0xFF;
  const g = (u32 >> 8) & 0xFF;
  const b = u32 & 0xFF;
  return "#" + [r, g, b].map((x) => x.toString(16).padStart(2, "0")).join("");
}

function wcHexToArgb(hex) {
  const clean = hex.replace("#", "");
  const r = parseInt(clean.substring(0, 2), 16) || 0;
  const g = parseInt(clean.substring(2, 4), 16) || 0;
  const b = parseInt(clean.substring(4, 6), 16) || 0;
  const u32 = ((0xFF << 24) | (r << 16) | (g << 8) | b) >>> 0;
  return u32 > 0x7FFFFFFF ? u32 - 0x100000000 : u32;
}

function wcUpdateBgColorVisibility() {
  // Background color only affects "Fit inside screen" mode, but stays visible
  // (disabled otherwise) so it's easy to discover.
  const input = document.getElementById("wc-bg-color");
  const label = document.querySelector('label[for="wc-bg-color"]');
  const isFit = document.getElementById("wc-scaling").value === "FitInsideScreen";
  if (input) input.disabled = !isFit;
  if (label) label.classList.toggle("wallchanger-label-disabled", !isFit);
}

function wcUpdateOverlayVisibility() {
  const enabled = document.getElementById("wc-overlay-enabled").checked;
  for (const id of ["wc-overlay-mode", "wc-overlay-font-size", "wc-overlay-color", "wc-overlay-offset-x", "wc-overlay-offset-y", "wc-overlay-backdrop"]) {
    const el = document.getElementById(id);
    if (el) el.disabled = !enabled;
    const label = document.querySelector(`label[for="${id}"]`);
    if (label) label.classList.toggle("wallchanger-label-disabled", !enabled);
  }
}

function wcRenderSettings() {
  document.getElementById("wc-interval").value = wcSettings.interval_minutes;
  document.getElementById("wc-max-level").value = wcSettings.maximum_source_level;
  document.getElementById("wc-wallhaven-api-key").value = wcSettings.wallhaven_api_key || "";
  document.getElementById("wc-rotation").value = wcSettings.rotation_mode;
  document.getElementById("wc-scaling").value = wcSettings.scaling_mode;
  document.getElementById("wc-bg-color").value = wcArgbToHex(wcSettings.background_color_argb);
  document.getElementById("wc-separate-queues").checked = wcSettings.use_separate_monitor_queues;
  document.getElementById("wc-unique-queues").checked = wcSettings.keep_image_in_single_monitor_queue;
  document.getElementById("wc-stop-slideshow").checked = wcSettings.disable_windows_slideshow_when_running;
  document.getElementById("wc-one-monitor").checked = wcSettings.change_one_monitor_per_interval;
  document.getElementById("wc-overlay-enabled").checked = wcSettings.show_picture_name_overlay;
  document.getElementById("wc-overlay-mode").value = wcSettings.picture_name_overlay_text_mode;
  document.getElementById("wc-overlay-font-size").value = wcSettings.picture_name_overlay_font_size;
  document.getElementById("wc-overlay-color").value = wcArgbToHex(wcSettings.picture_name_overlay_text_color_argb);
  document.getElementById("wc-overlay-offset-x").value = wcSettings.picture_name_overlay_offset_x;
  document.getElementById("wc-overlay-offset-y").value = wcSettings.picture_name_overlay_offset_y;
  document.getElementById("wc-overlay-backdrop").checked = wcSettings.picture_name_overlay_use_backdrop;
  document.getElementById("wc-fade-enabled").checked = wcSettings.enable_wallpaper_fade_transition;
  document.getElementById("wc-fade-duration").value = wcSettings.wallpaper_fade_duration_ms;
  document.getElementById("wc-fade-steps").value = wcSettings.wallpaper_fade_steps;
  wcUpdateBgColorVisibility();
  wcUpdateOverlayVisibility();
}

function escapeHtml(text) {
  return String(text)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#039;");
}

/** Replaces the source list (structural changes re-render it). */
function wcSetSources(sources, { render = true } = {}) {
  wcQueuePatch({ source_folders: sources });
  if (render) wcRenderSources();
}

/** Changes fields of one source without re-rendering the list. */
function wcEditSource(index, changes) {
  wcSetSources(
    wcSettings.source_folders.map((source, i) => (i === index ? { ...source, ...changes } : source)),
    { render: false }
  );
}

function wcRenderSources() {
  const container = document.getElementById("wc-sources");
  container.innerHTML = "";

  wcSettings.source_folders.forEach((source, index) => {
    const isWallhaven = source.kind === "wallhaven";
    const row = document.createElement("div");
    row.className = `wallchanger-source${index === wcSelectedSourceIndex ? " selected" : ""}`;
    row.style.gridTemplateColumns = isWallhaven ? "24px 1fr 60px 80px 180px" : "24px 1fr 60px 110px";
    const purity = source.purity || {};
    row.innerHTML = `
      <input type="checkbox" ${source.enabled ? "checked" : ""} title="Enabled">
      <input type="text" value="${escapeHtml(source.path)}" placeholder="Folder or URL" ${isWallhaven ? "" : "readonly"}>
      <input type="number" min="1" max="10" value="${source.level}" title="Level">
      ${isWallhaven ? `
        <input type="number" min="0" max="20" value="${source.page_limit}" title="Page limit (0 = unlimited)">
        <div class="wallchanger-purity" title="Purity">
          <label class="wallchanger-option" title="Safe for work"><input type="checkbox" data-purity="sfw" ${purity.sfw ? "checked" : ""}><span>SFW</span></label>
          <label class="wallchanger-option" title="Sketchy"><input type="checkbox" data-purity="sketchy" ${purity.sketchy ? "checked" : ""}><span>Sketchy</span></label>
          <label class="wallchanger-option" title="Not safe for work; requires API key"><input type="checkbox" data-purity="nsfw" ${purity.nsfw ? "checked" : ""}><span>NSFW</span></label>
        </div>
      ` : `
        <label class="wallchanger-option" title="Include subfolders">
          <input type="checkbox" ${source.include_subfolders ? "checked" : ""}>
          <span>Subfolders</span>
        </label>
      `}
    `;

    row.addEventListener("click", (e) => {
      if (e.target.tagName === "INPUT") return;
      wcSelectedSourceIndex = index;
      wcRenderSources();
      wcUpdateToolbar();
    });

    row.querySelector('input[type="checkbox"]').addEventListener("change", (e) => {
      wcEditSource(index, { enabled: e.target.checked });
    });

    const inputs = row.querySelectorAll('input[type="text"], input[type="number"]');
    const [pathInput, levelInput, pageLimitInput] = inputs;

    pathInput.addEventListener("change", (e) => wcEditSource(index, { path: e.target.value.trim() }));
    levelInput.addEventListener("change", (e) => {
      const level = parseInt(e.target.value, 10);
      if (Number.isFinite(level)) wcEditSource(index, { level });
    });

    if (isWallhaven) {
      pageLimitInput.addEventListener("change", (e) => {
        const pageLimit = parseInt(e.target.value, 10);
        if (Number.isFinite(pageLimit)) wcEditSource(index, { page_limit: pageLimit });
      });
      const purityInputs = row.querySelectorAll(".wallchanger-purity input[type=\"checkbox\"]");
      purityInputs.forEach((cb) => {
        cb.addEventListener("change", () => {
          const next = {};
          purityInputs.forEach((p) => (next[p.dataset.purity] = p.checked));
          wcEditSource(index, { purity: next });
        });
      });
    } else {
      const subCheck = row.querySelector('label.wallchanger-option input[type="checkbox"]');
      subCheck?.addEventListener("change", (e) => wcEditSource(index, { include_subfolders: e.target.checked }));
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

async function wcAddFolder() {
  try {
    const folder = await openDialog({ directory: true });
    if (!folder) return;
    wcSetSources([
      ...wcSettings.source_folders,
      { path: folder, enabled: true, level: 5, kind: "folder", include_subfolders: true },
    ]);
  } catch (err) {
    wcShowMessage(`Error adding folder: ${err}`, "error");
  }
}

async function wcAddWallhavenUrl() {
  const url = window.prompt("Enter a Wallhaven search or /api/v1/search URL:");
  if (!url) return;
  const trimmed = url.trim();
  if (!/^https?:\/\//i.test(trimmed) || !trimmed.toLowerCase().includes("wallhaven.cc")) {
    wcShowMessage("Please enter a valid Wallhaven URL.", "error");
    return;
  }
  wcSetSources([
    ...wcSettings.source_folders,
    {
      path: trimmed,
      enabled: true,
      level: 5,
      kind: "wallhaven",
      page_limit: 1,
      purity: { sfw: true, sketchy: true, nsfw: false },
    },
  ]);
}

function wcRemoveSource() {
  if (wcSelectedSourceIndex < 0) return;
  const sources = wcSettings.source_folders.filter((_, i) => i !== wcSelectedSourceIndex);
  wcSelectedSourceIndex = Math.min(wcSelectedSourceIndex, sources.length - 1);
  wcSetSources(sources);
}

function wcMoveSource(delta) {
  if (wcSelectedSourceIndex < 0) return;
  const newIndex = wcSelectedSourceIndex + delta;
  if (newIndex < 0 || newIndex >= wcSettings.source_folders.length) return;
  const sources = [...wcSettings.source_folders];
  const [moved] = sources.splice(wcSelectedSourceIndex, 1);
  sources.splice(newIndex, 0, moved);
  wcSelectedSourceIndex = newIndex;
  wcSetSources(sources);
}

async function wcChangeNow() {
  try {
    await wcFlushPatch();
    const result = await invoke("wc_change_now");
    wcShowMessage(result, "success");
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

async function wcLoadQueuePreview() {
  const container = document.getElementById("wc-queues");
  if (!container) return;

  container.innerHTML = `<div class="wallchanger-empty">Loading...</div>`;
  try {
    const queues = await invoke("wc_get_queue_preview");
    if (!queues.length) {
      container.innerHTML = `<div class="wallchanger-empty">No queues yet. Add a source folder to build a queue.</div>`;
      return;
    }

    container.innerHTML = queues.map(wcRenderQueueCard).join("");
  } catch (err) {
    container.innerHTML = "";
    wcShowMessage(`Error loading queue preview: ${err}`, "error");
  }
}

function wcRenderQueueCard(q) {
  const rows = q.images
    .map((img) => {
      const badges = [
        img.is_next ? `<span class="wallchanger-badge next">Next</span>` : "",
        img.is_last_shown ? `<span class="wallchanger-badge last">Last shown</span>` : "",
        img.last_resort ? `<span class="wallchanger-badge last-resort">Last resort</span>` : "",
      ].join("");
      return `<li class="wallchanger-queue-item${img.is_next ? " next" : ""}${img.last_resort ? " last-resort" : ""}">
        <span class="wallchanger-queue-filename" title="${escapeHtml(img.path)}">${escapeHtml(img.file_name)}</span>
        ${badges}
      </li>`;
    })
    .join("");

  const hidden = q.image_count - q.images.length;
  const more = hidden > 0 ? `<li class="wallchanger-queue-more">+${hidden} more</li>` : "";
  const nextHint = q.rotation_mode === "Random" ? "Next picture is chosen randomly" : "";

  return `<div class="wallchanger-queue-card">
    <div class="wallchanger-queue-title">
      <strong>${escapeHtml(q.label)}</strong>
      <span class="wallchanger-queue-meta">${q.image_count} image(s) &middot; ${q.rotation_mode}${nextHint ? " &middot; " + nextHint : ""}</span>
    </div>
    <ul class="wallchanger-queue-list">${rows}${more}</ul>
  </div>`;
}

async function wcLoadMonitorPreview() {
  const container = document.getElementById("wc-monitor-preview");
  if (!container) return;
  container.innerHTML = `<div class="wallchanger-empty">Loading...</div>`;
  try {
    const previews = await invoke("wc_get_monitor_preview");
    if (!previews.length) {
      container.innerHTML = `<div class="wallchanger-empty">No monitors found.</div>`;
      return;
    }
    container.innerHTML = previews.map(wcRenderMonitorPreview).join("");
  } catch (err) {
    container.innerHTML = "";
    wcShowMessage(`Error loading monitor preview: ${err}`, "error");
  }
}

function wcRenderMonitorPreview(p) {
  const imageDims = p.image_width ? `${p.image_width}×${p.image_height}` : "—";
  const aspect = p.width && p.height ? `${p.width} / ${p.height}` : "16 / 10";
  return `<div class="wallchanger-monitor-preview-card">
    <div class="wallchanger-monitor-preview-title">
      <strong>Monitor ${p.monitor_index}</strong>
      <span>${p.width}×${p.height}</span>
    </div>
    <div class="wallchanger-monitor-preview-frame" style="aspect-ratio: ${aspect}">
      <img src="${p.preview_data_url}" alt="Monitor ${p.monitor_index} preview">
    </div>
    <div class="wallchanger-monitor-preview-meta">
      <span title="${escapeHtml(p.fit)}">${escapeHtml(p.fit)}</span>
      <span title="${escapeHtml(p.wallpaper_path)}">Image: ${imageDims}</span>
    </div>
  </div>`;
}

function wcShowMessage(text, kind) {
  const el = document.getElementById("wc-message");
  el.textContent = text;
  el.className = `wallchanger-message${kind ? " " + kind : ""}`;
}
