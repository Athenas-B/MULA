// Light/dark theme: the user's choice (stored locally) or the OS setting.

const THEME_STORAGE_KEY = "mula-theme";

export function getStoredTheme() {
  try {
    return localStorage.getItem(THEME_STORAGE_KEY);
  } catch (_) {
    return null;
  }
}

function prefersDark() {
  return window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches;
}

export function applyTheme(theme) {
  document.documentElement.setAttribute("data-theme", theme);
  const toggle = document.getElementById("app-theme-toggle");
  const label = document.getElementById("app-theme-label");
  if (toggle) toggle.checked = theme === "dark";
  if (label) label.textContent = theme === "dark" ? "Dark mode" : "Light mode";
}

// Apply the effective theme immediately to avoid a flash of the wrong theme.
applyTheme(getStoredTheme() || (prefersDark() ? "dark" : "light"));

export function initTheme() {
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
