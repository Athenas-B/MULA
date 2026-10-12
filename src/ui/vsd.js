// VSD tab: the downloader server (start/stop, log, download folder, autostart) and the
// browser extension build.

import { invoke, listen, openDialog, revealItemInDir } from "./tauri.js";
import { errorText, escapeHtml, setRunningUi } from "./dom.js";

export const state = {
  running: false,
  // Highest server log line shown, and live lines that arrive before the backlog is loaded.
  logSeq: 0,
  logPending: [],
};

export async function initVsd() {
  const toggleBtn = document.getElementById("vsd-toggle");
  toggleBtn.addEventListener("click", toggleVsd);
  document.getElementById("vsd-download-dir").addEventListener("click", changeDownloadDir);
  document.getElementById("vsd-install-chrome").addEventListener("click", () => installExtension("chrome"));
  document.getElementById("vsd-install-firefox").addEventListener("click", () => installExtension("firefox"));
  document.getElementById("vsd-autostart").addEventListener("change", toggleAutostart);

  // The backend pushes server output and running/stopped changes (it also handles autostart).
  listen("vsd-log", (event) => onVsdLogLine(event.payload));
  listen("vsd-state-changed", (event) => updateVsdUi(event.payload));

  checkVsdStatus();
  loadVsdLogBacklog();
  loadDownloadDir();
  loadAutostart();
}

export async function loadAutostart() {
  try {
    document.getElementById("vsd-autostart").checked = await invoke("vsd_get_autostart");
  } catch (e) {
    console.error("Failed to load autostart setting:", e);
  }
}

export function onVsdLogLine(line) {
  if (state.logPending) state.logPending.push(line);
  else showVsdLogLines([line]);
}

/** Appends lines not shown yet (each line has an increasing `seq`). */
function showVsdLogLines(lines) {
  const fresh = lines.filter((line) => line.seq > state.logSeq);
  if (fresh.length === 0) return;
  state.logSeq = fresh[fresh.length - 1].seq;
  appendLog(fresh.map((line) => line.text).join(""));
}

/** Shows output the server produced before this window loaded, then the live lines. */
export async function loadVsdLogBacklog() {
  try {
    showVsdLogLines((await invoke("vsd_get_logs")) || []);
  } catch (e) {
    console.error("Failed to load VSD log:", e);
  } finally {
    const pending = state.logPending || [];
    state.logPending = null;
    showVsdLogLines(pending);
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
    appendLog(`Extension build failed: ${errorText(err)}\n`);
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
      if (state.running) {
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

export async function toggleVsd() {
  const toggleBtn = document.getElementById("vsd-toggle");
  toggleBtn.disabled = true;

  try {
    if (state.running) {
      await invoke("vsd_stop");
    } else {
      // Show the new state right away; the backend confirms it with `vsd-state-changed`.
      updateVsdUi(true);
      appendLog("[Starting server...]\n");
      await invoke("vsd_start");
    }
  } catch (err) {
    appendLog(`[Error] ${errorText(err)}\n`);
    checkVsdStatus();
  } finally {
    toggleBtn.disabled = false;
  }
}

export function updateVsdUi(running) {
  state.running = running;
  const toggleBtn = document.getElementById("vsd-toggle");
  setRunningUi(toggleBtn, toggleBtn.querySelector(".toggle-label"), running);
  toggleBtn.title = running ? "Stop VSD Server" : "Start VSD Server";
}

// ANSI color code to CSS color mapping
const ANSI_COLORS = {
  30: "#4d4d4d", 31: "#e74c3c", 32: "#2ecc71", 33: "#f39c12",
  34: "#3498db", 35: "#9b59b6", 36: "#1abc9c", 37: "#ecf0f1",
  90: "#7f8c8d", 91: "#ff6b6b", 92: "#55efc4", 93: "#ffeaa7",
  94: "#74b9ff", 95: "#a29bfe", 96: "#81ecec", 97: "#ffffff",
};

export function ansiToHtml(text) {
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
    } else {
      // Plain text up to the next escape sequence.
      let end = text.indexOf("\x1b[", i + 1);
      if (end === -1) end = text.length;
      html += escapeHtml(text.slice(i, end));
      i = end;
    }
  }

  html += "</span>".repeat(openSpans);
  return html;
}

// The VSD log is append-only; keep the view bounded (each chunk is ~1 s of output).
export const MAX_LOG_CHUNKS = 500;

export function appendLog(text) {
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
