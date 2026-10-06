# MULA — Handover Notes for the Next Agent

## Repository

- **Path:** `C:\Users\olda9\CascadeProjects\MULA`
- **Remote:** `https://github.com/Athenas-B/MULA.git`
- **Branch:** `master`
- **Current HEAD:** `d7d9035 Improve Wallhaven purity UI and allow unlimited page limit (0)`
- **Platform focus:** Windows 11 (Tauri app with Windows COM wallpaper APIs)

## Tech Stack

- **Backend:** Rust, Tauri 2
- **Frontend:** Plain HTML/CSS/JavaScript (no framework)
- **Build tooling:** Cargo + npm
- **Key crates:** `windows 0.58`, `image 0.25`, `imageproc 0.25`, `ab_glyph`, `rand`, `serde`, `chrono`, `ureq 2`, `base64 0.22`, `winreg`

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
- `src-tauri/src/wallchanger/monitors.rs` — Windows monitor COM API wrappers.
- `src-tauri/src/wallchanger/overlay.rs` — Picture-name overlay rendering.
- `src-tauri/src/wallchanger/transition.rs` — Fade transition frame generation.
- `src-tauri/src/wallchanger/preview.rs` — Monitor-fit preview image rendering.
- `src/index.html` / `src/main.js` / `src/styles.css` — Frontend.

## Settings / Cache Paths

- Settings: `%APPDATA%\mula\wallchanger\settings.json`
- Rendered overlay cache: `%APPDATA%\mula\wallchanger\rendered`
- Fade transition frames: `%APPDATA%\mula\wallchanger\rendered\transitions`
- Wallhaven download cache: `%APPDATA%\mula\wallchanger\wallhaven`
- Startup registry: `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value `MULA`

## Build & Test Commands

```powershell
# Quick Rust checks
cd src-tauri
cargo check
cargo test --lib

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
6. **README update** — README still lists the wallpaper changer as “coming soon”.
