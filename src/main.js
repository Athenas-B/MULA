// MULA's frontend entry point: forwards console output to the app log and starts each tab.

import { invoke } from "./ui/tauri.js";
import { initTheme } from "./ui/theme.js";
import { initSystem, initTabs } from "./ui/system.js";
import { initVsd } from "./ui/vsd.js";
import { initDriveTest } from "./ui/drives.js";
import { initWallchanger } from "./ui/wallchanger.js";

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

window.addEventListener("DOMContentLoaded", () => {
  initTabs();
  initTheme();
  initSystem();
  initVsd();
  initDriveTest();
  initWallchanger();
});
