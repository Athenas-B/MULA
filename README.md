# MULA - Managed Utility Local Automator

A cross-platform companion app built with Tauri (Rust + Web frontend) that manages background services and utilities.

## Features (Planned)

- **VSD Experiment Server** - Background server for the VSD browser extension
- **Wallpaper Changer** - Automated wallpaper management (coming soon)
- **System Tray** - Runs quietly in the background with tray controls

## Platforms

- Windows 11
- Pop!_OS (Linux)

## Tech Stack

- **Backend:** Rust (Tauri v2)
- **Frontend:** HTML/CSS/JavaScript
- **Build:** Cargo + npm

## Development

### Prerequisites

- [Rust](https://rustup.rs/) (1.70+)
- [Node.js](https://nodejs.org/) (18+)
- Platform-specific dependencies:
  - **Linux:** `sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev`
  - **Windows:** WebView2 (pre-installed on Windows 11)

### Run in development

```bash
npm run tauri dev
```

### Build for production

```bash
npm run tauri build
```

## Project Structure

```
MULA/
├── src/              # Frontend (HTML/CSS/JS)
├── src-tauri/        # Rust backend
│   ├── src/
│   │   ├── main.rs   # Entry point
│   │   └── lib.rs    # App logic & Tauri commands
│   ├── Cargo.toml    # Rust dependencies
│   └── tauri.conf.json # Tauri configuration
└── package.json
```

## License

MIT
