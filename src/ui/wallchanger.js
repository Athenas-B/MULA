// Wall Changer tab. The backend owns the settings: the UI sends partial updates
// (`wc_update_settings`) and renders whatever comes back, or arrives with the
// `wc-settings-changed` event (e.g. from the tray).

import { invoke, listen, openDialog } from "./tauri.js";
import { errorText, escapeHtml, setRunningUi } from "./dom.js";

export const state = {
  settings: null,
  selectedSourceIndex: -1,
  // Edits not sent yet, and the timer that sends them.
  pendingPatch: {},
  patchTimer: null,
  // Revision of the settings shown; older responses and events are ignored.
  revision: -1,
  // Previews are only rebuilt while the window is shown; changes while it is hidden (e.g. the
  // interval service with the app in the tray) are picked up when it is shown again.
  previewsStale: false,
};

// Form field id → [setting, how to read it]. A change sends only that field's setting.
const WC_FORM_FIELDS = {
  "wc-interval": ["interval_minutes", "int"],
  "wc-max-level": ["maximum_source_level", "int"],
  "wc-wallhaven-api-key": ["wallhaven_api_key", "text"],
  "wc-rotation": ["rotation_mode", "text"],
  "wc-scaling": ["scaling_mode", "text"],
  "wc-bg-color": ["background_color_argb", "color"],
  "wc-separate-queues": ["use_separate_monitor_queues", "checked"],
  "wc-unique-queues": ["keep_image_in_single_monitor_queue", "checked"],
  "wc-stop-slideshow": ["disable_windows_slideshow_when_running", "checked"],
  "wc-one-monitor": ["change_one_monitor_per_interval", "checked"],
  "wc-fade-enabled": ["enable_wallpaper_fade_transition", "checked"],
  "wc-fade-duration": ["wallpaper_fade_duration_ms", "int"],
  "wc-fade-steps": ["wallpaper_fade_steps", "int"],
  "wc-overlay-enabled": ["show_picture_name_overlay", "checked"],
  "wc-overlay-mode": ["picture_name_overlay_text_mode", "text"],
  "wc-overlay-font-size": ["picture_name_overlay_font_size", "int"],
  "wc-overlay-color": ["picture_name_overlay_text_color_argb", "color"],
  "wc-overlay-offset-x": ["picture_name_overlay_offset_x", "int"],
  "wc-overlay-offset-y": ["picture_name_overlay_offset_y", "int"],
  "wc-overlay-backdrop": ["picture_name_overlay_use_backdrop", "checked"],
};

export async function initWallchanger() {
  // Settings changed elsewhere (e.g. the tray's max source level) or normalized by the backend.
  listen("wc-settings-changed", (event) => {
    if (state.settings) wcApplySettings(event.payload);
  });
  // The service can be started/stopped from the tray, too.
  listen("wc-service-changed", (event) => wcUpdateToggleUi(event.payload));
  // Emitted after every wallpaper change, whether from this tab, the tray or the interval service.
  listen("wc-wallpaper-changed", wcRefreshPreviews);

  document.getElementById("wc-toggle")?.addEventListener("click", wcToggleService);
  document.getElementById("wc-change-now")?.addEventListener("click", wcChangeNow);
  document.getElementById("wc-add-folder")?.addEventListener("click", wcAddFolder);
  document.getElementById("wc-add-wallhaven")?.addEventListener("click", wcAddWallhavenUrl);
  document.getElementById("wc-remove-source")?.addEventListener("click", wcRemoveSource);
  document.getElementById("wc-move-up")?.addEventListener("click", () => wcMoveSource(-1));
  document.getElementById("wc-move-down")?.addEventListener("click", () => wcMoveSource(1));

  for (const id of Object.keys(WC_FORM_FIELDS)) {
    document.getElementById(id)?.addEventListener("change", () => {
      const patch = wcFieldPatch(id);
      if (patch) wcQueuePatch(patch);
      else wcRenderSettings(); // e.g. an emptied number field: show the current value again
      wcUpdateBgColorVisibility();
      wcUpdateOverlayVisibility();
    });
  }

  document.getElementById("wc-queue-refresh")?.addEventListener("click", wcLoadQueuePreview);
  document.getElementById("wc-monitor-preview-refresh")?.addEventListener("click", wcLoadMonitorPreview);

  document.addEventListener("visibilitychange", () => {
    if (!document.hidden && state.previewsStale) wcRefreshPreviews();
  });

  await Promise.all([wcLoadSettings(), wcLoadStatus(), wcLoadMonitors(), wcRefreshPreviews()]);
}

export function wcRefreshPreviews() {
  state.previewsStale = !!document.hidden;
  if (state.previewsStale) return;
  return Promise.all([wcLoadMonitorPreview(), wcLoadQueuePreview()]);
}

async function wcLoadSettings() {
  try {
    wcApplySettings(await invoke("wc_get_settings"));
  } catch (err) {
    wcShowMessage(`Error loading settings: ${errorText(err)}`, "error");
  }
}

/** Adopts `{ revision, settings }` from the backend unless it is older than what is shown,
 *  keeping local edits that haven't been sent yet. */
export function wcApplySettings({ revision, settings }) {
  if (revision < state.revision) return;
  state.revision = revision;
  const next = { ...settings, ...state.pendingPatch };
  const changed = JSON.stringify(next) !== JSON.stringify(state.settings);
  state.settings = next;
  if (changed) {
    wcRenderSettings();
    wcRenderSources();
  }
}

/** Records a partial settings update; it is sent after a short pause in editing. */
export function wcQueuePatch(patch) {
  if (!state.settings) return;
  Object.assign(state.pendingPatch, patch);
  Object.assign(state.settings, patch);
  clearTimeout(state.patchTimer);
  state.patchTimer = setTimeout(wcFlushPatch, 300);
}

/** Sends pending edits now; the backend answers with the normalized settings. */
export async function wcFlushPatch() {
  clearTimeout(state.patchTimer);
  if (Object.keys(state.pendingPatch).length === 0) return;
  const patch = state.pendingPatch;
  state.pendingPatch = {};
  try {
    wcApplySettings(await invoke("wc_update_settings", { patch }));
  } catch (err) {
    wcShowMessage(`Error saving settings: ${errorText(err)}`, "error");
    await wcLoadSettings();
  }
}

/** The patch for one changed form field (the backend clamps and validates it), or null when the
 *  value isn't usable, e.g. an empty number field. */
export function wcFieldPatch(id) {
  const [key, kind] = WC_FORM_FIELDS[id];
  const el = document.getElementById(id);
  switch (kind) {
    case "checked":
      return { [key]: el.checked };
    case "color":
      return { [key]: wcHexToArgb(el.value) };
    case "int": {
      const n = parseInt(el.value, 10);
      return Number.isFinite(n) ? { [key]: n } : null;
    }
    default:
      return { [key]: el.value || "" };
  }
}

export function wcArgbToHex(argb) {
  const u32 = (argb ?? 0xFF000000) >>> 0;
  const r = (u32 >> 16) & 0xFF;
  const g = (u32 >> 8) & 0xFF;
  const b = u32 & 0xFF;
  return "#" + [r, g, b].map((x) => x.toString(16).padStart(2, "0")).join("");
}

export function wcHexToArgb(hex) {
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

export function wcRenderSettings() {
  for (const [id, [key, kind]] of Object.entries(WC_FORM_FIELDS)) {
    const el = document.getElementById(id);
    if (!el) continue;
    const value = state.settings[key];
    if (kind === "checked") el.checked = value;
    else if (kind === "color") el.value = wcArgbToHex(value);
    else el.value = value ?? "";
  }
  wcUpdateBgColorVisibility();
  wcUpdateOverlayVisibility();
}

/** Replaces the source list (structural changes re-render it). */
function wcSetSources(sources, { render = true } = {}) {
  wcQueuePatch({ source_folders: sources });
  if (render) wcRenderSources();
}

/** Changes fields of one source without re-rendering the list. */
function wcEditSource(index, changes) {
  wcSetSources(
    state.settings.source_folders.map((source, i) => (i === index ? { ...source, ...changes } : source)),
    { render: false }
  );
}

export function wcRenderSources() {
  const container = document.getElementById("wc-sources");
  container.innerHTML = "";

  state.settings.source_folders.forEach((source, index) => {
    const isWallhaven = source.kind === "wallhaven";
    const row = document.createElement("div");
    row.className = `wallchanger-source${index === state.selectedSourceIndex ? " selected" : ""}`;
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
      state.selectedSourceIndex = index;
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
  const hasSelection = state.selectedSourceIndex >= 0 && state.selectedSourceIndex < state.settings.source_folders.length;
  document.getElementById("wc-remove-source").disabled = !hasSelection;
  document.getElementById("wc-move-up").disabled = !hasSelection || state.selectedSourceIndex === 0;
  document.getElementById("wc-move-down").disabled = !hasSelection || state.selectedSourceIndex === state.settings.source_folders.length - 1;
}

export async function wcAddFolder() {
  try {
    const folder = await openDialog({ directory: true });
    if (!folder) return;
    wcSetSources([
      ...state.settings.source_folders,
      { path: folder, enabled: true, level: 5, kind: "folder", include_subfolders: true },
    ]);
  } catch (err) {
    wcShowMessage(`Error adding folder: ${errorText(err)}`, "error");
  }
}

export async function wcAddWallhavenUrl() {
  const url = window.prompt("Enter a Wallhaven search or /api/v1/search URL:");
  if (!url) return;
  const trimmed = url.trim();
  if (!/^https?:\/\//i.test(trimmed) || !trimmed.toLowerCase().includes("wallhaven.cc")) {
    wcShowMessage("Please enter a valid Wallhaven URL.", "error");
    return;
  }
  wcSetSources([
    ...state.settings.source_folders,
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

export function wcRemoveSource() {
  if (state.selectedSourceIndex < 0) return;
  const sources = state.settings.source_folders.filter((_, i) => i !== state.selectedSourceIndex);
  state.selectedSourceIndex = Math.min(state.selectedSourceIndex, sources.length - 1);
  wcSetSources(sources);
}

export function wcMoveSource(delta) {
  if (state.selectedSourceIndex < 0) return;
  const newIndex = state.selectedSourceIndex + delta;
  if (newIndex < 0 || newIndex >= state.settings.source_folders.length) return;
  const sources = [...state.settings.source_folders];
  const [moved] = sources.splice(state.selectedSourceIndex, 1);
  sources.splice(newIndex, 0, moved);
  state.selectedSourceIndex = newIndex;
  wcSetSources(sources);
}

export async function wcChangeNow() {
  try {
    await wcFlushPatch();
    const result = await invoke("wc_change_now");
    wcShowMessage(result, "success");
  } catch (err) {
    wcShowMessage(`Error changing wallpaper: ${errorText(err)}`, "error");
  }
}

async function wcToggleService() {
  try {
    const running = await invoke("wc_toggle_service");
    wcUpdateToggleUi(running);
  } catch (err) {
    wcShowMessage(`Error toggling service: ${errorText(err)}`, "error");
  }
}

async function wcLoadStatus() {
  try {
    wcUpdateToggleUi(await invoke("wc_get_status"));
  } catch (err) {
    wcShowMessage(`Error loading status: ${errorText(err)}`, "error");
  }
}

export function wcUpdateToggleUi(running) {
  setRunningUi(document.getElementById("wc-toggle"), document.getElementById("wc-toggle-label"), running);
  document.getElementById("wc-status").textContent = running ? "running" : "stopped";
}

async function wcLoadMonitors() {
  try {
    const monitors = await invoke("wc_get_monitors");
    const list = document.getElementById("wc-monitors");
    list.innerHTML = monitors
      .map((m, i) => `<li>Monitor ${i + 1}: ${m.width} x ${m.height}</li>`)
      .join("");
  } catch (err) {
    wcShowMessage(`Error loading monitors: ${errorText(err)}`, "error");
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
    wcShowMessage(`Error loading queue preview: ${errorText(err)}`, "error");
  }
}

export function wcRenderQueueCard(q) {
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
    wcShowMessage(`Error loading monitor preview: ${errorText(err)}`, "error");
  }
}

export function wcRenderMonitorPreview(p) {
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

export function wcShowMessage(text, kind) {
  const el = document.getElementById("wc-message");
  el.textContent = text;
  el.className = `wallchanger-message${kind ? " " + kind : ""}`;
}
