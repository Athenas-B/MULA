const { invoke } = window.__TAURI__.core;

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

window.addEventListener("DOMContentLoaded", () => {
  initTabs();
  loadAppInfo();
});
