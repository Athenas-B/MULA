# MULA — Handover Notes for the Next Agent

## Repository

- **Path:** `C:\Users\olda9\CascadeProjects\MULA`
- **Remote:** `https://github.com/Athenas-B/MULA.git`
- **Branch:** `master`
- **Last reviewed:** 2026-10-11 (security / robustness pass, see "Architecture notes" below)
- **Platform focus:** Windows 11 (Tauri app with Windows COM wallpaper APIs)

## Tech Stack

- **Backend:** Rust, Tauri 2
- **Frontend:** Plain HTML/CSS/JavaScript (no framework)
- **Build tooling:** Cargo + npm
- **Key crates:** `windows 0.58`, `image 0.25`, `imageproc 0.25`, `ab_glyph`, `rand`, `serde`, `chrono`, `ureq 2`, `base64 0.22`, `winreg`

## App Overview

MULA has four tabs: **VSD** (Python Flask HLS downloader + browser extension, `modules/vsd`),
**Drive Test** (drive inventory, SMART via smartctl, self-tests, full format), **Wall Changer**
(below) and **System** (start with Windows, theme). Closing the window hides MULA to the tray;
the tray's *Exit* quits and stops the elevated helper and the VSD server.

## Architecture notes (read before changing these areas)

- **Wall Changer settings vs. runtime state.** `settings.json` holds only user configuration and is
  the only thing the UI reads/writes (`wc_get_settings` / `wc_save_settings`). Runtime state
  (`queue_states`, `image_cache_entries`, `image_show_stats`, `next_monitor_change_index`,
  `change_service_running`) lives in `state.json`, written only by `service::apply`
  (`settings::save_apply_state`) and `service::set_running`. These fields are
  `#[serde(skip_serializing)]` on `Settings`, so a UI save can never roll back service progress.
  Old combined `settings.json` files are migrated on first `settings::load()`. Both files are
  written atomically (temp file + rename).
- **Threading.** All `wc_*` commands are `async` + `spawn_blocking` (sync Tauri commands run on
  the UI thread). `service::apply` is serialized by `APPLY_LOCK`; tray actions spawn blocking
  tasks. The interval worker is a plain thread guarded by a generation counter.
- **Events to the UI.** `wc-service-changed` (bool) and `wc-wallpaper-changed` (message) are
  emitted from the backend via `wallchanger::emit`, so the tab stays in sync with tray/service.
- **Wallhaven.** Results are cached per source in `manifest.json` for 6 h (`RESULTS_TTL`);
  `NetworkPolicy::CacheOnly` is used by previews. Failed refreshes fall back to cached images,
  and one failing source no longer fails `load_images`. Downloads go through `.part` files.
  Cache folders are named with FNV-1a (`fnv<hash>`); legacy `DefaultHasher` folders are migrated.
  API keys are redacted from errors/logs (`wallhaven::redact`).
- **Elevation (`elevated_helper.rs`).** The long-lived smartctl helper is started via an inline
  `-EncodedCommand` stub that verifies the SHA-256 of the script before running it; it only runs
  the smartctl path fixed at launch (must be outside the user profile) with an allowlist of
  argument shapes, requires a per-session token, names its own response files, and exits when
  MULA's PID disappears. Formatting uses `run_elevated_script` (separate UAC prompt) and refuses
  system/boot disks both in Rust (`DriveInfo::is_system`) and in the PowerShell script.
  Residual risk: a same-user process can still ask the helper for SMART reads / self-tests.
- **VSD server.** Rejects any `Origin` other than browser-extension origins, rejects non-loopback
  `Host` headers (DNS rebinding), `/open` only opens files VSD produced or inside the download
  dir, `/download` only accepts http(s), and only MULA (no Origin) may change the download dir.

## Wall Changer Port Status

The original standalone Windows **Wall Changer** app has been ported into MULA as a single tab (`#tab-wallchanger`). The original multi-tab settings dialog was intentionally consolidated into one tab.

### Implemented (matches or improves on the original)

| Feature | Status | Notes |
|---|---|---|
| Local image folders | Done | Add/remove/reorder, enable/disable, level 1–10, subfolders. |
| Wallhaven URL sources | Done | Accepts `https://wallhaven.cc/search?...` or `/api/v1/search` URLs; per-source page limit (0 = unlimited) and SFW/Sketchy/NSFW purity toggles; API key support for NSFW. Cached under `%APPDATA%\mula\wallchanger\wallhaven`. |
| Monitor detection | Done | Uses `IDesktopWallpaper`; skips monitors whose `GetMonitorRECT` fails. |
| Monitor preview block | Done | Shows each monitor frame and how the current Windows wallpaper is fitted (Fill / FitInsideScreen). |
| Image dimension caching | Done | Cached in `settings.json` with size + mtime. |
| Shared / per-monitor queues | Done | Separate queues, exclusive queue membership, fallback queues. |
| Rotation modes | Done | Random and Sequence; Sequence index persists across restarts. |
| Scaling modes | Done | Fill and FitInsideScreen with configurable background color. |
| Interval service | Done | Runs in background; 1–1440 minutes. |
| Manual Change / Apply | Done | In tab and tray menu. |
| Change one monitor per interval | Done | Rotates target monitor index. |
| Picture-name overlay | Done | Filename/path text, font size, color, offset, optional backdrop; cached rendered copies. |
| Fade transition | Done | Generates blended frames and steps through them. |
| Rendered cache cleanup | Done | Prunes by total size and file count. |
| Windows slideshow conflict handling | Done | Detects `DSS_ENABLED` and reapplies current wallpaper to break Windows slideshow before MULA takes over. |
| Queue preview / state UI | Done | Read-only preview with next/last/last-resort markers. |
| Tray integration | Done | Renamed to MULA; actions grouped under a “Wall Changer” submenu; max source level quick-switch; start-with-Windows toggle. |
| Start with Windows | Done | Registry Run entry with `--minimized`; exposed in System tab and tray. |
| Auto-save settings | Done | All changes persist automatically; no Save button. |
| Dark / light mode | Done | User toggle on System tab. |

### Still missing or partial

| Feature | Status | Notes / Where to look |
|---|---|---|
| Wallpaper-change notifications | Setting only | `show_wallpaper_change_notifications` exists in `Settings` but is never fired. |
| Change wallpaper on startup | Setting only | `change_on_start` exists but is not acted on. |
| Random source order | Setting only | `use_random_source_order` exists but is not implemented. |
| Image show history / stats | Partial | `image_show_stats` is tracked in `service.rs`, but no UI exposes it. |
| Wallhaven cache management | Partial | Images are cached and skipped if present, but there is no explicit “clear / re-download all” UI. |
| Multi-tab settings dialog | N/A | Replaced by single Wall Changer tab by design. |
| Standalone process | N/A | Integrated into MULA by design. |

## Important Source Files

- `src-tauri/src/lib.rs` — Tauri command registration and tray setup.
- `src-tauri/src/wallchanger/mod.rs` — Tauri commands for the Wall Changer tab.
- `src-tauri/src/wallchanger/settings.rs` — Settings model, load/save/normalize.
- `src-tauri/src/wallchanger/images.rs` — Local image scanning + Wallhaven cache inclusion.
- `src-tauri/src/wallchanger/wallhaven.rs` — Wallhaven API fetch + local cache.
- `src-tauri/src/wallchanger/queue.rs` — Image ranking, queue building, sequence state.
- `src-tauri/src/wallchanger/service.rs` — Background timer service.
- `src-tauri/src/wallchanger/monitors/windows.rs` / `linux.rs` — Monitor + wallpaper COM API wrappers (selected via `#[path]`).
- `src-tauri/src/elevated_helper.rs` — Admin-rights execution (see Architecture notes).
- `src-tauri/src/wallchanger/overlay.rs` — Picture-name overlay rendering.
- `src-tauri/src/wallchanger/transition.rs` — Fade transition frame generation.
- `src-tauri/src/wallchanger/preview.rs` — Monitor-fit preview rendering; `fit_to_canvas` is shared with the fade transition.
- `src/index.html` / `src/main.js` / `src/styles.css` — Frontend.

## Settings / Cache Paths

- Settings (user configuration only): `%APPDATA%\mula\wallchanger\settings.json`
- Runtime state (queues, image cache, stats, service on/off): `%APPDATA%\mula\wallchanger\state.json`
- Rendered overlay cache: `%APPDATA%\mula\wallchanger\rendered`
- Fade transition frames: `%APPDATA%\mula\wallchanger\rendered\transitions`
- Wallhaven download cache: `%APPDATA%\mula\wallchanger\wallhaven\fnv<hash>\` (+ `manifest.json`)
- Elevated helper / one-shot script dirs: `%TEMP%\mula_smartd_*`, `%TEMP%\mula_elevated_*`
- Startup registry: `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value `MULA`

## Build & Test Commands

```powershell
# Quick Rust checks
cd src-tauri
cargo check
cargo test --lib   # includes helper protocol tests that run PowerShell unelevated (no UAC)

# Full debug bundle (MSI + NSIS installer)
cd ..
npm run tauri build -- --debug
```

Debug bundle outputs:
- `src-tauri/target/debug/bundle/msi/MULA_2.0.0_x64_en-US.msi`
- `src-tauri/target/debug/bundle/nsis/MULA_2.0.0_x64-setup.exe`

## Common Issues / Notes

- **Line-ending warnings:** Git warns about LF → CRLF replacement on Windows; this is harmless.
- **COM initialization:** Wallpaper COM calls run on a dedicated blocking thread; `CoInitializeEx`/`CoUninitialize` are managed in `monitors.rs`.
- **Wallhaven API key:** Required for NSFW purity. Stored in `wallhaven_api_key`; `use_wallhaven_api_key` is auto-managed by normalization.
- **Wallhaven page limit:** `0` now means unlimited pages (use with caution; the API still paginates and respects rate limits).
- **Wallhaven URL validation:** Only URLs containing `wallhaven.cc` are treated as Wallhaven sources; anything else is treated as a local folder path.
- **Tray menu state:** `MaxLevelMenuState` and `AutostartMenuState` are managed in `lib.rs` setup so tray items stay in sync with the UI.

## Next Logical Tasks

1. **Wallpaper-change notifications** — wire `show_wallpaper_change_notifications` to a Windows toast / Tauri notification.
2. **Change on start** — read `change_on_start` on app startup and trigger one wallpaper apply.
3. **Random source order** — implement `use_random_source_order` when building the image pool.
4. **Image show history UI** — expose `image_show_stats` in the Wall Changer tab.
5. **Wallhaven cache management UI** — add a “Clear Wallhaven cache” / “Re-download” button.
6. **Manual verification still needed** (cannot be covered by unit tests): the real UAC flow of
   the smartctl helper and of Format (on a spare drive), and the first-run migration of an
   existing `settings.json` into `state.json`.
