# MULA - Managed Utility Local Automator

A cross-platform companion app built with Tauri (Rust + Web frontend) that manages background services and utilities.

## Features

- **VSD Server** — HLS stream downloader (M3U8 + TS segments → MP4 via ffmpeg)
- **Wallpaper Changer** — Automated wallpaper management (coming soon)
- **System Tray** — Runs quietly in the background with tray controls (coming soon)

## Platforms

- Windows 11
- Pop!_OS (Linux)

## Tech Stack

- **Backend:** Rust (Tauri v2)
- **Frontend:** HTML/CSS/JavaScript
- **Modules:** Python (VSD server)
- **Build:** Cargo + npm

## Setup

### Prerequisites

- [Rust](https://rustup.rs/) (1.70+)
- [Node.js](https://nodejs.org/) (18+)
- [Python](https://python.org/) (3.10+)
- [ffmpeg](https://ffmpeg.org/) (on PATH or configured in .env)
- Platform-specific:
  - **Linux:** `sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev`
  - **Windows:** WebView2 (pre-installed on Windows 11), Visual Studio Build Tools

### Install module dependencies

```bash
cd modules/vsd
pip install -r requirements.txt
```

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
├── src/                  # Frontend (HTML/CSS/JS)
├── src-tauri/            # Rust backend (Tauri)
│   ├── src/
│   │   ├── main.rs       # Entry point
│   │   └── lib.rs        # App logic & Tauri commands
│   ├── Cargo.toml
│   └── tauri.conf.json
├── modules/              # Python service modules
│   └── vsd/
│       ├── server.py     # VSD companion server (Flask)
│       ├── requirements.txt
│       ├── setup.py      # Dependency installer
│       └── .env.example  # Configuration template
└── package.json
```

## VSD Module

The VSD server downloads HLS streams by:
1. Fetching the M3U8 playlist
2. Downloading all .ts segments (with retry)
3. Remuxing into MP4 with ffmpeg (preserving metadata)

### API

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/health` | GET | Server status |
| `/download` | POST | Start a download job |
| `/progress/<id>` | GET | Check job progress |
| `/logs` | GET | Tail server logs |
| `/open` | POST | Open a file with default app |

## License

MIT
