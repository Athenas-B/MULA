// Helpers shared by the tabs.

import { invoke } from "./tauri.js";

/** Readable text for a command error: typed `{ kind, message }` objects or plain strings. */
export function errorText(err) {
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

export function escapeHtml(text) {
  return String(text)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#039;");
}

/** Shows a start/stop toggle button as running or stopped. */
export function setRunningUi(btn, label, running) {
  btn.classList.toggle("running", running);
  btn.classList.toggle("stopped", !running);
  label.textContent = running ? "Stop" : "Start";
}

/** Collapses or expands (`collapsed` omitted: toggles) a section; returns whether it is collapsed. */
export function setCollapsed(toggle, content, collapsed) {
  const isCollapsed = content.classList.toggle("collapsed", collapsed);
  toggle.setAttribute("aria-expanded", String(!isCollapsed));
  toggle.querySelector(".toggle-icon").textContent = isCollapsed ? "+" : "-";
  return isCollapsed;
}

/** Shows `text` in a status element (if it exists), marked as `kind`: "error", "success" or neither. */
export function setStatus(el, baseClass, text, kind) {
  if (!el) return;
  el.textContent = text;
  el.className = kind ? `${baseClass} ${kind}` : baseClass;
}

/**
 * Ties a checkbox to a pair of backend commands: shows the stored value and saves every change.
 * A change that can't be saved is undone, so the box never shows a state that isn't stored.
 */
export async function bindSettingCheckbox(checkbox, getCommand, setCommand) {
  checkbox.addEventListener("change", async () => {
    const enabled = checkbox.checked;
    try {
      await invoke(setCommand, { enabled });
    } catch (e) {
      checkbox.checked = !enabled;
      console.error(`${setCommand} failed:`, e);
    }
  });
  try {
    checkbox.checked = await invoke(getCommand);
  } catch (e) {
    console.error(`${getCommand} failed:`, e);
  }
}
