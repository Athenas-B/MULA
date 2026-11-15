// Tab switching and the System tab (app info, start with Windows, config folder).

import { invoke, listen, revealItemInDir } from "./tauri.js";
import { bindSettingCheckbox } from "./dom.js";

// Tab switching
export function initTabs() {
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

export function initSystem() {
  loadAppInfo();
  document.getElementById("app-config-path").addEventListener("click", openConfigDir);

  const autostart = document.getElementById("app-autostart");
  if (autostart) {
    bindSettingCheckbox(autostart, "get_autostart", "set_autostart");
    // Keep the checkbox in sync when autostart is toggled from the tray menu.
    listen("autostart-changed", (event) => {
      autostart.checked = event.payload;
    });
  }
}
