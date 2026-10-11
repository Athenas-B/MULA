//! The VSD server (`modules/vsd/server.py`): its configuration, its process and its output.
//!
//! Output lines and running/stopped changes are pushed to the UI as `vsd-log` and
//! `vsd-state-changed` events; a bounded backlog lets the UI show what happened before it opened.

use crate::util;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

static PROCESS: Mutex<Option<Child>> = Mutex::new(None);
static LOG_BACKLOG: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
const LOG_BACKLOG_LINES: usize = 1000;
static CONFIG_LOCK: Mutex<()> = Mutex::new(());

/// Authorizes MULA's configuration changes on the running server (`X-VSD-Admin-Token`);
/// a new secret for every launch of MULA.
static ADMIN_TOKEN: LazyLock<String> = LazyLock::new(|| {
    use rand::Rng;
    let bytes: [u8; 32] = rand::thread_rng().gen();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
});

// ── Configuration (`vsd.json`) ──────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VsdConfig {
    #[serde(default = "default_download_dir")]
    pub download_dir: String,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default = "default_port")]
    pub port: u16,
}

impl Default for VsdConfig {
    fn default() -> Self {
        serde_json::from_str("{}").expect("all VSD config fields have defaults")
    }
}

fn default_download_dir() -> String {
    dirs::home_dir()
        .map(|h| h.join("Downloads").join("VSD").to_string_lossy().to_string())
        .unwrap_or_default()
}

fn default_port() -> u16 {
    8765
}

fn config_path() -> Option<PathBuf> {
    util::mula_config_dir().map(|dir| dir.join("vsd.json"))
}

fn load_config() -> VsdConfig {
    config_path().map(|path| util::read_json_or_default(&path)).unwrap_or_default()
}

fn update_config(f: impl FnOnce(&mut VsdConfig)) -> Result<VsdConfig, String> {
    let _guard = CONFIG_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let path = config_path().ok_or("Could not determine config directory")?;
    let mut config: VsdConfig = util::read_json_or_default(&path);
    f(&mut config);
    util::write_json(&path, &config)?;
    Ok(config)
}

// ── Locating the server ──────────────────────────────────────────────────────

fn server_script() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    [
        // Development: relative to the project root (exe is in src-tauri/target/debug/)
        exe_dir.join("../../../modules/vsd/server.py"),
        // Installed: next to the executable
        exe_dir.join("modules/vsd/server.py"),
    ]
    .into_iter()
    .find(|candidate| candidate.exists())
    .map(|candidate| candidate.canonicalize().unwrap_or(candidate))
}

/// The Python of a local `.venv` next to the server if there is one, else the system Python.
fn python_command(dir: &Path) -> PathBuf {
    let venv_python = if cfg!(target_os = "windows") {
        dir.join(".venv").join("Scripts").join("python.exe")
    } else {
        dir.join(".venv").join("bin").join("python")
    };
    if venv_python.exists() {
        venv_python
    } else if cfg!(target_os = "windows") {
        PathBuf::from("python")
    } else {
        PathBuf::from("python3")
    }
}

// ── Output ───────────────────────────────────────────────────────────────────

fn emit_line(app: &AppHandle, line: impl Into<String>) {
    let line = line.into();
    log::info!("[VSD] {line}");
    let line = format!("{line}\n");
    if let Ok(mut backlog) = LOG_BACKLOG.lock() {
        backlog.push_back(line.clone());
        while backlog.len() > LOG_BACKLOG_LINES {
            backlog.pop_front();
        }
    }
    let _ = app.emit("vsd-log", line);
}

fn emit_state(app: &AppHandle, running: bool) {
    let _ = app.emit("vsd-state-changed", running);
}

/// Forwards one output stream of the server. The stdout reader also notices the exit.
fn spawn_reader(app: AppHandle, stream: impl std::io::Read + Send + 'static, watch_exit: bool) {
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { continue };
            emit_line(&app, line);
        }
        if watch_exit {
            wait_for_exit(&app);
        }
    });
}

/// After the server closed its output: wait for the process to end, then report it. Does
/// nothing if [`stop`] already took the process.
fn wait_for_exit(app: &AppHandle) {
    loop {
        let finished = {
            let mut guard = PROCESS.lock().unwrap_or_else(|p| p.into_inner());
            match guard.as_mut().map(|child| child.try_wait()) {
                None => return,
                Some(Ok(None)) => None,
                Some(Ok(Some(status))) => {
                    guard.take();
                    Some(status.code())
                }
                Some(Err(_)) => {
                    guard.take();
                    Some(None)
                }
            }
        };
        match finished {
            None => std::thread::sleep(Duration::from_millis(200)),
            Some(code) => {
                if code != Some(0) {
                    // Most likely a broken Python environment: reinstall dependencies next time.
                    forget_installed_requirements();
                    emit_line(app, format!("[Server exited with code {}]", code.map_or("?".into(), |c| c.to_string())));
                } else {
                    emit_line(app, "[Server stopped]");
                }
                emit_state(app, false);
                return;
            }
        }
    }
}

// ── Dependencies ─────────────────────────────────────────────────────────────

fn requirements_marker() -> Option<PathBuf> {
    util::mula_config_dir().map(|dir| dir.join("vsd-requirements.sha256"))
}

fn forget_installed_requirements() {
    if let Some(marker) = requirements_marker() {
        let _ = std::fs::remove_file(marker);
    }
}

/// Identifies "these requirements installed into this interpreter".
fn requirements_fingerprint(python: &Path, requirements: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(python.to_string_lossy().as_bytes());
    hasher.update([0]);
    hasher.update(requirements);
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Runs pip only when the interpreter or `requirements.txt` changed since the last successful
/// install (pip takes seconds even when nothing is missing). A server that exits with an error
/// clears the record, so a broken environment is repaired on the next start.
fn ensure_requirements(app: &AppHandle, python: &Path, working_dir: &Path) -> Result<(), String> {
    let requirements = working_dir.join("requirements.txt");
    let Ok(contents) = std::fs::read(&requirements) else {
        return Ok(());
    };
    let fingerprint = requirements_fingerprint(python, &contents);
    let marker = requirements_marker();
    if marker
        .as_ref()
        .and_then(|m| std::fs::read_to_string(m).ok())
        .is_some_and(|recorded| recorded.trim() == fingerprint)
    {
        return Ok(());
    }

    emit_line(app, "[Installing dependencies...]");
    let output = Command::new(python)
        .args(["-m", "pip", "install", "--quiet", "-r"])
        .arg(&requirements)
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to run pip: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        emit_line(app, format!("[pip install failed: {}]", stderr.trim()));
        return Err(format!("Failed to install dependencies: {}", stderr.trim()));
    }
    if let Some(marker) = marker {
        let _ = util::write_atomic(&marker, &fingerprint);
    }
    emit_line(app, "[Dependencies OK]");
    Ok(())
}

// ── Process lifecycle ────────────────────────────────────────────────────────

fn start(app: &AppHandle) -> Result<(), String> {
    let mut guard = PROCESS.lock().map_err(|e| e.to_string())?;
    if guard.is_some() {
        return Err("VSD Server is already running".to_string());
    }

    let script = server_script().ok_or("Could not find VSD server.py")?;
    let working_dir = script.parent().ok_or("Invalid server path")?.to_path_buf();
    let python = python_command(&working_dir);
    ensure_requirements(app, &python, &working_dir)?;

    let config = load_config();
    let mut child = Command::new(&python)
        .arg(&script)
        .current_dir(&working_dir)
        .env("VSD_DOWNLOAD_DIR", &config.download_dir)
        .env("VSD_PORT", config.port.to_string())
        .env("VSD_ADMIN_TOKEN", ADMIN_TOKEN.as_str())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start VSD server: {e}"))?;

    if let Some(stdout) = child.stdout.take() {
        spawn_reader(app.clone(), stdout, true);
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_reader(app.clone(), stderr, false);
    }
    *guard = Some(child);
    drop(guard);

    emit_state(app, true);
    Ok(())
}

/// Stops the server if it runs; returns whether it did.
pub fn stop_process() -> bool {
    let child = PROCESS.lock().ok().and_then(|mut guard| guard.take());
    match child {
        Some(mut child) => {
            let _ = child.kill();
            let _ = child.wait();
            true
        }
        None => false,
    }
}

fn is_running() -> bool {
    let mut guard = PROCESS.lock().unwrap_or_else(|p| p.into_inner());
    match guard.as_mut().map(|child| child.try_wait()) {
        Some(Ok(None)) => true,
        Some(_) => {
            guard.take();
            false
        }
        None => false,
    }
}

/// Starts the server at launch when the user enabled "start automatically".
pub fn autostart(app: AppHandle) {
    if !load_config().autostart {
        return;
    }
    std::thread::spawn(move || {
        emit_line(&app, "[Autostart: starting server...]");
        if let Err(e) = start(&app) {
            emit_line(&app, format!("[Error] {e}"));
            emit_state(&app, false);
        }
    });
}

/// Tells a running server about a new download folder.
fn notify_download_dir(port: u16, path: &str) -> Result<(), String> {
    ureq::post(&format!("http://127.0.0.1:{port}/config/download_dir"))
        .timeout(Duration::from_secs(5))
        .set("X-VSD-Admin-Token", &ADMIN_TOKEN)
        .set("Content-Type", "application/json")
        .send_string(&serde_json::json!({ "path": path }).to_string())
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ── Commands ─────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn vsd_start(app: AppHandle) -> Result<(), String> {
    util::blocking(move || start(&app)).await
}

#[tauri::command]
pub async fn vsd_stop(app: AppHandle) -> Result<(), String> {
    util::blocking(move || {
        if !stop_process() {
            return Err("VSD Server is not running".to_string());
        }
        emit_line(&app, "[Server stopped]");
        emit_state(&app, false);
        Ok(())
    })
    .await
}

#[tauri::command]
pub fn vsd_is_running() -> bool {
    is_running()
}

/// Recent server output (the UI receives new lines as `vsd-log` events).
#[tauri::command]
pub fn vsd_get_logs() -> Vec<String> {
    LOG_BACKLOG.lock().map(|backlog| backlog.iter().cloned().collect()).unwrap_or_default()
}

#[tauri::command]
pub fn vsd_get_download_dir() -> String {
    load_config().download_dir
}

#[tauri::command]
pub fn vsd_set_download_dir(path: String) -> Result<(), String> {
    let config = update_config(|c| c.download_dir = path.clone())?;
    if is_running() {
        std::thread::spawn(move || {
            if let Err(e) = notify_download_dir(config.port, &path) {
                log::error!("Failed to update the running server's download dir: {e}");
            }
        });
    }
    Ok(())
}

#[tauri::command]
pub fn vsd_get_autostart() -> bool {
    load_config().autostart
}

#[tauri::command]
pub fn vsd_set_autostart(enabled: bool) -> Result<(), String> {
    update_config(|c| c.autostart = enabled).map(|_| ())
}

/// Builds the browser extension and returns its folder (the UI reveals it).
#[tauri::command]
pub async fn vsd_install_extension(browser: String) -> Result<String, String> {
    util::blocking(move || {
        let vsd_dir = server_script()
            .and_then(|s| s.parent().map(Path::to_path_buf))
            .ok_or("Could not find the VSD module")?;
        let build_script = vsd_dir.join("extension").join("build.py");
        if !build_script.exists() {
            return Err("Could not find extension build script".to_string());
        }

        let output = Command::new(python_command(&vsd_dir.join("extension")))
            .arg(&build_script)
            .arg(&browser)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("Failed to run build script: {e}"))?;
        if !output.status.success() {
            return Err(format!("Build failed: {}", String::from_utf8_lossy(&output.stderr).trim()));
        }

        let ext_dir = vsd_dir.join("extension").join("dist").join(&browser);
        if !ext_dir.exists() {
            return Err(format!("Extension not found at {}", ext_dir.display()));
        }
        Ok(ext_dir.to_string_lossy().to_string())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn config_defaults() {
        let config = VsdConfig::default();
        assert_eq!(config.port, 8765);
        assert!(!config.autostart);
        assert!(config.download_dir.ends_with("VSD"), "{}", config.download_dir);
    }

    #[test]
    fn config_round_trips_through_json() {
        let dir = TempDir::new("vsd_config");
        let path = dir.join("vsd.json");
        let config = VsdConfig { download_dir: r"D:\Videos".into(), autostart: true, port: 9000 };
        util::write_json(&path, &config).unwrap();
        assert_eq!(util::read_json_or_default::<VsdConfig>(&path), config);
        // A partial file keeps the defaults for what's missing.
        std::fs::write(&path, r#"{"autostart": true}"#).unwrap();
        let partial: VsdConfig = util::read_json_or_default(&path);
        assert!(partial.autostart);
        assert_eq!(partial.port, 8765);
    }

    #[test]
    fn python_from_a_local_venv_is_preferred() {
        let dir = TempDir::new("venv");
        let system = python_command(dir.path());
        assert!(system == Path::new("python") || system == Path::new("python3"));

        let venv_python = if cfg!(target_os = "windows") {
            dir.join(".venv").join("Scripts").join("python.exe")
        } else {
            dir.join(".venv").join("bin").join("python")
        };
        std::fs::create_dir_all(venv_python.parent().unwrap()).unwrap();
        std::fs::write(&venv_python, b"").unwrap();
        assert_eq!(python_command(dir.path()), venv_python);
    }

    #[test]
    fn the_requirements_fingerprint_changes_with_interpreter_and_requirements() {
        let a = requirements_fingerprint(Path::new("python"), b"flask>=3\n");
        assert_eq!(a, requirements_fingerprint(Path::new("python"), b"flask>=3\n"));
        assert_ne!(a, requirements_fingerprint(Path::new("C:/venv/python.exe"), b"flask>=3\n"));
        assert_ne!(a, requirements_fingerprint(Path::new("python"), b"flask>=3\nrequests\n"));
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn the_admin_token_is_a_long_random_secret() {
        assert_eq!(ADMIN_TOKEN.len(), 64);
        assert!(ADMIN_TOKEN.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
