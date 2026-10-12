// Helpers shared by the tabs.

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
