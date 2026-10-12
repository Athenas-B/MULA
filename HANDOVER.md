# MULA — Handover Notes for the Next Agent

## Repository

- **Path:** `C:\Users\olda9\CascadeProjects\MULA`
- **Remote:** `https://github.com/Athenas-B/MULA.git`
- **Branch:** `master`
- **Last reviewed:** 2026-10-12 (architecture pass, see "Architecture notes" below)
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

- **Wall Changer data model (`wallchanger/settings.rs`).** Two separate types and files:
  `Settings` (user configuration, `settings.json`) and `RuntimeState` (queues, image cache, show
  stats, next monitor — `state.json`, written only by `service::apply` via `save_state`). The
  service on/off flag has its own `service.json` (`service_running` / `set_service_running`). Sources are typed: `Source { path, enabled, level,
  kind }` with `SourceKind::Folder { include_subfolders }` / `SourceKind::Wallhaven { page_limit,
  purity: Purity { sfw, sketchy, nsfw } }` (serialized with a `"kind"` tag). Unreadable files
  are moved aside to `*.bak` and defaults are used. All JSON files are written atomically.
  The three files are `util::JsonStore`s: read once, then served from memory; every change is
  written first and only adopted in memory when the write succeeded (hand edits to these files
  while MULA runs are not picked up). `vsd.json` is still read on demand, so a hand-edited
  `port` applies on the next server start.
- **Backend-owned settings.** The UI sends partial updates (`wc_update_settings(patch)` → normalized
  `Settings`) and renders from the result or the `wc-settings-changed` event, which every change
  (UI or tray) emits via `wallchanger::on_settings_changed`. Unknown keys are rejected. Each form
  change sends only its own setting; responses and events carry a `revision` and older ones are
  ignored. The UI keeps not-yet-sent edits when settings arrive (`wcApplySettings`) and does no
  clamping itself. JSON files that can't be read (other than missing) are errors, never defaults.
- **Threading.** Commands do blocking work through `util::blocking` / `logged_blocking` (sync
  Tauri commands run on the UI thread). `service::apply` is serialized by `APPLY_LOCK`; tray
  actions spawn blocking tasks. The interval worker is a plain thread that sleeps on a `Condvar`
  (`WORKER` state: generation, active, `last_apply`); `stop`, every apply and settings changes
  (`service::wake`) wake it, so a manual change restarts the interval. Mutexes are taken with the
  poison-recovering `util::lock`.
- **Events to the UI (`events.rs`).** Event names are constants there and `events::emit` sends
  them through the `AppHandle` stored at setup: `wc-service-changed`, `wc-wallpaper-changed`,
  `wc-settings-changed`, `vsd-log`, `vsd-state-changed`, `autostart-changed`. No polling for logs
  or state. While the window is hidden the UI skips preview reloads and catches up on show.
- **Logging (`logger.rs`).** `%APPDATA%\mula\logs\mula.log`, appended by a background thread in
  batches and rotated to `mula.log.1` past 1 MB.
- **Wallhaven.** Results are cached per source in `manifest.json` for 6 h. Stale results are
  refreshed on a background thread (`refresh_in_background`) while cached images keep being used;
  only a source with nothing cached is fetched synchronously. Images download with 4 workers
  through `.part` files. The API key is sent as an `X-API-Key` header, never in URLs.
- **Typed command errors (`error.rs`).** Every command returns `CmdResult<T>`, i.e. rejects with
  a `CommandError` (`{ kind: "smartctl_missing" | "needs_elevation" | "elevation_declined" |
  "other", message? }`). The frontend shows errors with `errorText(err)` and switches on
  `err.kind` (`invokeSmart`); no text matching. External programs run through
  `util::run_command` (stdout, or "<what> failed: <stderr>").
- **SMART (`smart.rs`).** smartctl runs with `--json=o` (structured data plus the usual text).
  Exit-status bit 1 (device open failed) becomes `NeedsElevation` when not elevated; bits 3–7 are
  disk health, not errors. Self-test progress/log is parsed in Rust (`SelfTestStatus`, ATA and
  NVMe) and polled with `-c -l selftest`. smartmontools is never installed silently: a missing
  smartctl yields `SmartctlMissing`, the UI asks, then calls `install_smartmontools`. Each drive's
  resolved smartctl device is cached.
- **Elevation (`elevated_helper.rs`).** The long-lived smartctl helper is started via an inline
  `-EncodedCommand` stub that verifies the SHA-256 of the script before running it; it only runs
  the smartctl path fixed at launch (must be outside the user profile) with an allowlist of
  argument shapes (optionally prefixed by `--json=o`), requires a per-session token, names its
  own response files, and exits when MULA's PID disappears. Neither side polls: the helper sleeps
  in `Wait-Event` on a `FileSystemWatcher` (events only wake it; it still lists and validates the
  folder) and MULA waits with `FindFirstChangeNotificationW` (`wait_for_file`), so a round trip
  takes ~16 ms instead of ~260 ms and the idle helper wakes every 2 s instead of 4×/s. Files are
  kept over a named pipe on purpose: a UAC-started process can't inherit handles, and a pipe name
  chosen up front could be claimed first by another same-user process (pipe squatting). Script inputs are always passed as
  PowerShell variables declared as single-quoted literals (`ps_script`) — no text placeholders.
  Formatting uses `run_elevated_script` (separate UAC prompt; a declined prompt is detected by
  ERROR_CANCELLED) and refuses system/boot disks in Rust and in the script.
  Residual risk: a same-user process can still ask the helper for SMART reads / self-tests.
- **VSD (`vsd.rs` + `modules/vsd/server.py`).** Config lives in `vsd.json` (`download_dir`,
  `autostart`, `port`). MULA builds the extension with `build.py --server-url` so it targets the
  configured port, and starts the server with `VSD_DOWNLOAD_DIR`, `VSD_PORT` and a
  per-launch `VSD_ADMIN_TOKEN`; log lines carry a `seq` so the UI merges backlog and live events;
  the process lock is never held across pip; config changes on the running server are sent with the
  `X-VSD-Admin-Token` header. Autostart happens in the backend at launch. pip runs only when the
  interpreter or `requirements.txt` changed (fingerprint in `vsd-requirements.sha256`; a server
  that exits with an error clears it). The server rejects non-extension `Origin`s and
  non-loopback `Host`s, only opens files it produced, only fetches http(s) (redirects included),
  and downloads segments in parallel (`VSD_SEGMENT_WORKERS`, default 6) with connection reuse.
- **Browser extension (`modules/vsd/extension/src`).** Shared `config.js` (server URL helpers).
  The background owns the job lifecycle: jobs live on the stream records in storage (`stream.job`),
  one poll loop updates them and resumes when the service worker starts; every storage
  read-modify-write goes through `updateStore`. webRequest listeners only see `*.m3u*` URLs. The
  popup renders from `storage.onChanged` (no timers); its tooltip lists the same ffmpeg metadata
  keys as `server.py`'s `METADATA_KEYS`. Detected playlists are verified once and cached; the content script uses a
  MutationObserver and sends metadata only when it changes.

## Wall Changer Port Status

The original standalone Windows **Wall Changer** app has been ported into MULA as a single tab (`#tab-wallchanger`). The original multi-tab settings dialog was intentionally consolidated into one tab.

### Implemented (matches or improves on the original)

| Feature | Status | Notes |
|---|---|---|
| Local image folders | Done | Add/remove/reorder, enable/disable, level 1–10, subfolders. |
| Wallhaven URL sources | Done | Accepts `https://wallhaven.cc/search?...` or `/api/v1/search` URLs; per-source page limit (0 = unlimited) and SFW/Sketchy/NSFW purity toggles; API key support for NSFW. Cached under `%APPDATA%\mula\wallchanger\wallhaven`. |
| Monitor detection | Done | Uses `IDesktopWallpaper`; skips monitors whose `GetMonitorRECT` fails. |
| Monitor preview block | Done | Shows each monitor frame and how the current Windows wallpaper is fitted (Fill / FitInsideScreen); thumbnails are reused while file, monitor and fit settings are unchanged. |
| Image dimension caching | Done | Cached in `state.json` with size + mtime (unreadable files as 0×0). |
| Shared / per-monitor queues | Done | Separate queues, exclusive queue membership, fallback queues. |
| Rotation modes | Done | Random and Sequence; Sequence index persists across restarts. |
| Scaling modes | Done | Fill and FitInsideScreen with configurable background color. |
| Interval service | Done | Runs in background; 1–1440 minutes. |
| Manual Change / Apply | Done | In tab and tray menu. |
| Change one monitor per interval | Done | Rotates target monitor index. |
| Picture-name overlay | Done | Filename/path text, font size, color, offset, optional backdrop; cached rendered copies. |
| Fade transition | Done | All monitors fade together: images load, overlays render and frames blend/encode in parallel (`util::parallel_map`, `IMAGE_WORKERS`), then frames play in lockstep. |
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

- `src-tauri/src/lib.rs` — App setup, tray, command registration, drive listing and format.
- `src-tauri/src/smart.rs` — smartctl discovery/install, device mapping, JSON parsing, SMART commands.
- `src-tauri/src/vsd.rs` — VSD server config, process, output events, extension build.
- `src-tauri/src/error.rs` — `CommandError`, the typed error returned to the UI.
- `src-tauri/src/wallchanger/mod.rs` — Tauri commands for the Wall Changer tab.
- `src-tauri/src/wallchanger/settings.rs` — Settings model, load/save/normalize.
- `src-tauri/src/wallchanger/images.rs` — Local image scanning + Wallhaven cache inclusion.
- `src-tauri/src/wallchanger/wallhaven.rs` — Wallhaven API fetch + local cache.
- `src-tauri/src/wallchanger/queue.rs` — Image ranking, queue building, sequence state.
- `src-tauri/src/wallchanger/service.rs` — Background timer service.
- `src-tauri/src/wallchanger/monitors/windows.rs` / `linux.rs` — Monitor + wallpaper COM API wrappers (selected via `#[path]`).
- `src-tauri/src/elevated_helper.rs` — Admin-rights execution (see Architecture notes).
- `src-tauri/src/util.rs` — Shared helpers: `mula_config_dir`/`mula_path`, `blocking`/`logged_blocking`, `lock`, `parallel_map`, `run_command`, `JsonStore`, `read_json_or_default`/`write_json`/`write_atomic`, `safe_file_name`, `stable_hash` (FNV-1a for cache names).
- `src-tauri/src/events.rs` — Event names and `emit`.
- `src-tauri/src/logger.rs` — Rotating app log.
- `src-tauri/src/wallchanger/overlay.rs` — Picture-name overlay rendering.
- `src-tauri/src/wallchanger/transition.rs` — Fade transition frame generation.
- `src-tauri/src/wallchanger/preview.rs` — Monitor-fit preview rendering; `fit_to_canvas` is shared with the fade transition.
- `src/index.html` / `src/main.js` / `src/styles.css` — Frontend.

## Settings / Cache Paths

- Settings (user configuration only): `%APPDATA%\mula\wallchanger\settings.json`
- Runtime state (queues, image cache, stats): `%APPDATA%\mula\wallchanger\state.json`
- Service on/off: `%APPDATA%\mula\wallchanger\service.json`
- App log: `%APPDATA%\mula\logs\mula.log` (+ `mula.log.1`)
- Rendered overlay cache: `%APPDATA%\mula\wallchanger\rendered`
- Fade transition frames: `%APPDATA%\mula\wallchanger\rendered\transitions`
- Wallhaven download cache: `%APPDATA%\mula\wallchanger\wallhaven\fnv<hash>\` (+ `manifest.json`)
- Elevated helper / one-shot script dirs: `%TEMP%\mula_smartd_*`, `%TEMP%\mula_elevated_*`
- VSD config: `%APPDATA%\mula\vsd.json`; pip fingerprint: `%APPDATA%\mula\vsd-requirements.sha256`
- SMART snapshots: `%APPDATA%\mula\snapshots\<drive>\`
- Startup registry: `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value `MULA`

## Build & Test Commands

```powershell
# Quick Rust checks
cd src-tauri
cargo check
cargo test --lib   # includes helper protocol tests that run PowerShell unelevated (no UAC)

# Frontend + browser extension (Node built-in test runner, no npm deps)
cd ..
npm test

# VSD server + extension builder (stdlib unittest)
cd modules/vsd
$env:PYTHONDONTWRITEBYTECODE=1; python -m unittest discover -s tests -v

# Full debug bundle (MSI + NSIS installer)
cd ..
npm run tauri build -- --debug
```

Debug bundle outputs:
- `src-tauri/target/debug/bundle/msi/MULA_2.0.0_x64_en-US.msi`
- `src-tauri/target/debug/bundle/nsis/MULA_2.0.0_x64-setup.exe`

## Tests

| Suite | Location | Notes |
|---|---|---|
| Rust | `#[cfg(test)]` modules in `src-tauri/src/**` | Shared helpers in `src-tauri/src/test_support.rs` (`TempDir`, `write_png`). Document a known bug as `#[ignore = "bug: ..."]`. |
| JavaScript | `tests/js/*.test.js` | Loads the unmodified browser scripts in `node:vm` with stubs (`tests/js/helpers/vm-loader.js`). Document a known bug as `{ todo }`. |
| Python | `modules/vsd/tests/` | Points `VSD_DOWNLOAD_DIR`/`VSD_LOG_DIR` at temp dirs before importing `server`. Document a known bug as `@unittest.expectedFailure`. |

Unit tests never call the real wallpaper COM API, the network, ffmpeg or UAC. Two Rust tests touch
`%APPDATA%\mula\wallchanger` (the overlay render test writes one cached image; the Wallhaven
cache-only test creates and removes an empty folder).

## Common Issues / Notes

- **Line-ending warnings:** Git warns about LF → CRLF replacement on Windows; this is harmless.
- **COM initialization:** Wallpaper COM calls run on blocking threads; `monitors/windows.rs` (`desktop_wallpaper`) initializes COM on the calling thread before each use.
- **Wallhaven API key:** Required for NSFW purity. Stored in `wallhaven_api_key`; it is used whenever it is non-empty.
- **Wallhaven page limit:** `0` now means unlimited pages (use with caution; the API still paginates and respects rate limits).
- **Source kinds:** A source's kind is chosen when it is added ("Add folder" vs "Add Wallhaven URL"); nothing is inferred from the path.
- **Tray menu state:** `MaxLevelMenuState` and `AutostartMenuState` are managed in `lib.rs` setup so tray items stay in sync with the UI.

## Decisions (considered and not pursued)

Recorded so they aren't re-investigated without new facts.

- **Image index / file watchers for folder sources (dropped 2026-10-12).** Idea: keep an index
  of each folder source, updated by file-system watchers (`notify` crate), instead of walking
  the folders in `images::load_images` on every wallpaper change and queue preview.
  Measured with a release build on a synthetic tree of 10,000 files in 250 folders (local NTFS
  SSD): the first pass, which reads every image's dimensions, took ~500 ms; every later
  `load_images` took **~19 ms**, of which the folder walk itself was ~14 ms. It is cheap
  because the walk uses the metadata the directory listing already returns (no per-file open
  or stat on Windows) and dimensions are cached by size + mtime in `state.json` (unreadable
  files too, as 0×0). The scan runs at most once per wallpaper change (interval ≥ 1 min,
  30 min by default) and when the queue preview loads, so an index would save ~15–20 ms per
  change. The cost would be a new dependency, watchers created/dropped whenever sources
  change, recovery from dropped events (watcher buffer overflow), and a full-scan fallback
  anyway. The one slow case, large collections on network shares, is also where watchers are
  least reliable. Revisit only if real usage shows slow scans; a cheaper first step would be
  reusing a folder's listing while its directory mtime is unchanged.

## Next Logical Tasks

1. **Wallpaper-change notifications** — wire `show_wallpaper_change_notifications` to a Windows toast / Tauri notification.
2. **Change on start** — read `change_on_start` on app startup and trigger one wallpaper apply.
3. **Random source order** — implement `use_random_source_order` when building the image pool.
4. **Image show history UI** — expose `image_show_stats` in the Wall Changer tab.
5. **Wallhaven cache management UI** — add a “Clear Wallhaven cache” / “Re-download” button.
6. **Manual verification still needed** (cannot be covered by unit tests): the real UAC flow of
   the smartctl helper and of Format (on a spare drive), smartctl's JSON on real ATA/NVMe drives,
   and the browser extension loaded in Chrome/Firefox (its tests run the scripts in `node:vm`).
   Settings files from before the typed source format are moved aside to `*.bak`.
