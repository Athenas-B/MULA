//! The VSD server (`modules/vsd/server.py`): its configuration, its process and its output.
//!
//! Output lines and running/stopped changes are pushed to the UI as `vsd-log` and
//! `vsd-state-changed` events; a bounded backlog lets the UI show what happened before it opened.

use crate::error::CmdResult;
use crate::{events, util};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::Duration;

/// The running server. Only held briefly: never across pip or other slow work.
static PROCESS: Mutex<Option<Child>> = Mutex::new(None);
/// Set while a start is in progress (installing dependencies can take a while).
static STARTING: AtomicBool = AtomicBool::new(false);
/// Whether the current server run printed an import error (a broken Python environment).
static SAW_IMPORT_ERROR: AtomicBool = AtomicBool::new(false);
static LOG_BACKLOG: Mutex<VecDeque<LogLine>> = Mutex::new(VecDeque::new());
static LOG_SEQ: AtomicU64 = AtomicU64::new(0);
const LOG_BACKLOG_LINES: usize = 1000;
static CONFIG_LOCK: Mutex<()> = Mutex::new(());

/// Authorizes MULA's configuration changes on the running server (`X-VSD-Admin-Token`);
/// a new secret for every launch of MULA.
static ADMIN_TOKEN: LazyLock<String> = LazyLock::new(|| util::random_hex(32));

/// One line of server output. `seq` increases with every line, so the UI can merge the backlog
/// with live `vsd-log` events without showing a line twice.
#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    pub seq: u64,
    pub text: String,
}

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

fn config_path() -> Result<PathBuf, String> {
    util::mula_path("vsd.json")
}

fn load_config() -> Result<VsdConfig, String> {
    util::read_json_or_default(&config_path()?)
}

fn update_config(f: impl FnOnce(&mut VsdConfig)) -> Result<VsdConfig, String> {
    let _guard = util::lock(&CONFIG_LOCK);
    let path = config_path()?;
    let mut config: VsdConfig = util::read_json_or_default(&path)?;
    f(&mut config);
    util::write_json(&path, &config)?;
    Ok(config)
}

// ── Locations ────────────────────────────────────────────────────────────────

/// The VSD module folder (`server.py`, `extension/`), set once at startup by [`init`].
static MODULE_DIR: OnceLock<PathBuf> = OnceLock::new();

/// `resource_dir` is where the app's bundled resources are (`bundle.resources` in
/// tauri.conf.json): next to the exe on Windows, `/usr/lib/MULA` for a .deb. Debug builds use the
/// project's own copy instead, so edits to server.py apply without a rebuild.
pub fn init(resource_dir: Option<PathBuf>) {
    // Not canonicalized: on Windows that gives a `\\?\C:\...` path, which the UI would show and
    // Chrome's folder picker may not take.
    let dir = if cfg!(debug_assertions) {
        Path::new(env!("CARGO_MANIFEST_DIR")).parent().map(|project| project.join("modules").join("vsd"))
    } else {
        resource_dir.map(|dir| dir.join("modules").join("vsd"))
    };
    if let Some(dir) = dir {
        let _ = MODULE_DIR.set(dir);
    }
}

fn module_dir() -> Result<PathBuf, String> {
    MODULE_DIR
        .get()
        .filter(|dir| dir.join("server.py").exists())
        .cloned()
        .ok_or_else(|| "Could not find the VSD module".to_string())
}

/// Where VSD keeps what it writes (venv, logs, settings, built extension): MULA's data folder,
/// since the module folder may be read-only once installed (Program Files, /usr/lib).
fn data_path(name: &str) -> Result<PathBuf, String> {
    Ok(util::mula_path("vsd")?.join(name))
}

// ── Python ───────────────────────────────────────────────────────────────────

fn venv_python(venv: &Path) -> PathBuf {
    if cfg!(target_os = "windows") {
        venv.join("Scripts").join("python.exe")
    } else {
        venv.join("bin").join("python")
    }
}

fn system_python() -> &'static str {
    if cfg!(target_os = "windows") {
        "python"
    } else {
        "python3"
    }
}

/// The Python the server runs with: a `.venv` next to the server if there is one (a developer's
/// own), else MULA's venv in its data folder. Packages never go into the system Python: Debian,
/// Ubuntu and Pop!_OS refuse that (PEP 668), and an all-users Windows Python needs admin rights.
/// Only called from [`start`], whose `STARTING` flag keeps two creations from overlapping.
fn vsd_python(server_dir: &Path) -> Result<PathBuf, String> {
    let local = venv_python(&server_dir.join(".venv"));
    if local.exists() {
        return Ok(local);
    }
    let venv = data_path("venv")?;
    match venv_state(&venv) {
        VenvState::Works => {}
        // Rebuilding also drops the record of installed requirements, which lives inside it.
        VenvState::Broken => create_venv(system_python(), &venv)?,
        VenvState::Unknown(detail) => {
            // Not evidence against the venv (e.g. a stray PYTHONHOME, or antivirus blocking
            // Python): keep it, since rebuilding means downloading its packages again.
            let message = format!(
                "Could not check the Python environment ({detail}). If this persists, delete {}",
                venv.display()
            );
            emit_line(format!("[{message}]"));
            return Err(message);
        }
    }
    Ok(venv_python(&venv))
}

#[derive(Debug, PartialEq)]
enum VenvState {
    Works,
    /// Never made, interrupted while being made, its base Python removed, or a Python upgrade
    /// left its packages behind: rebuild it.
    Broken,
    /// The check itself failed: no verdict.
    Unknown(String),
}

/// Exits 3 when pip is missing or doesn't come from the venv: with `--system-site-packages`,
/// the system's pip would otherwise hide a venv whose own packages Python no longer reaches
/// (on Linux, `bin/python` follows a `python3` symlink to the new version after an upgrade).
const VENV_CHECK: &str = "\
import os, sys
try:
    import pip
except ImportError:
    sys.exit(3)
where = getattr(pip, \"__file__\", None) or \"\"
inside = os.path.realpath(where).startswith(os.path.realpath(sys.prefix) + os.sep)
sys.exit(0 if inside else 3)
";
const VENV_CHECK_BROKEN: i32 = 3;

fn venv_state(venv: &Path) -> VenvState {
    let python = venv_python(venv);
    let base_gone = venv_home(venv).is_some_and(|home| !home.exists());
    if !python.exists() || base_gone || !has_own_pip(venv) {
        return VenvState::Broken;
    }
    match util::command(&python).args(["-c", VENV_CHECK]).output() {
        Ok(output) if output.status.success() => VenvState::Works,
        Ok(output) if output.status.code() == Some(VENV_CHECK_BROKEN) => VenvState::Broken,
        Ok(output) => VenvState::Unknown(
            String::from_utf8_lossy(&output.stderr).trim().lines().last().unwrap_or("no output").to_string(),
        ),
        Err(e) => VenvState::Unknown(e.to_string()),
    }
}

/// The base Python's folder (`home` in `pyvenv.cfg`).
fn venv_home(venv: &Path) -> Option<PathBuf> {
    let config = std::fs::read_to_string(venv.join("pyvenv.cfg")).ok()?;
    config.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "home").then(|| PathBuf::from(value.trim()))
    })
}

/// Whether pip was installed into the venv (the last step of creating one), checked on disk.
fn has_own_pip(venv: &Path) -> bool {
    if cfg!(target_os = "windows") {
        return venv.join("Lib").join("site-packages").join("pip").is_dir();
    }
    std::fs::read_dir(venv.join("lib")).is_ok_and(|entries| {
        entries.flatten().any(|entry| entry.path().join("site-packages").join("pip").is_dir())
    })
}

/// (Re)creates the venv at `venv` from `base_python`, replacing whatever is there. It sees the
/// system's packages, so requirements already installed there need no download. A failed
/// creation may leave a half-made venv; [`venv_state`] calls it broken, so the next start retries.
fn create_venv(base_python: &str, venv: &Path) -> Result<(), String> {
    emit_line("[Creating the Python environment...]");
    let _ = std::fs::remove_dir_all(venv);
    let result = util::run_command(
        util::command(base_python).args(["-m", "venv", "--system-site-packages"]).arg(venv),
        "creating the Python environment",
    );
    if let Err(e) = result {
        let hint = if cfg!(target_os = "linux") {
            " (on Debian, Ubuntu or Pop!_OS: sudo apt install python3-venv)"
        } else {
            ""
        };
        let message = format!("{e}{hint}");
        emit_line(format!("[{message}]"));
        return Err(message);
    }
    Ok(())
}

// ── Output ───────────────────────────────────────────────────────────────────

fn emit_line(line: impl Into<String>) {
    let line = line.into();
    log::info!("[VSD] {line}");
    if is_import_error(&line) {
        SAW_IMPORT_ERROR.store(true, Ordering::SeqCst);
    }
    let entry = LogLine { seq: LOG_SEQ.fetch_add(1, Ordering::SeqCst) + 1, text: format!("{line}\n") };
    let mut backlog = util::lock(&LOG_BACKLOG);
    backlog.push_back(entry.clone());
    while backlog.len() > LOG_BACKLOG_LINES {
        backlog.pop_front();
    }
    drop(backlog);
    events::emit(events::VSD_LOG, entry);
}

fn is_import_error(line: &str) -> bool {
    line.contains("ModuleNotFoundError") || line.contains("ImportError")
}

fn emit_state(running: bool) {
    events::emit(events::VSD_STATE_CHANGED, running);
}

/// Forwards one output stream of the server. The stdout reader also notices the exit; it gets
/// the server's Python (`exit_watch`), whose record of installed requirements an import error
/// clears.
fn spawn_reader(stream: impl std::io::Read + Send + 'static, exit_watch: Option<PathBuf>) {
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { continue };
            emit_line(line);
        }
        if let Some(python) = exit_watch {
            wait_for_exit(&python);
        }
    });
}

enum ServerStatus {
    Absent,
    Running,
    Exited(Option<i32>),
}

/// The server's state; an exited server is reaped (removed from [`PROCESS`]).
fn poll_server() -> ServerStatus {
    let mut guard = util::lock(&PROCESS);
    let status = match guard.as_mut().map(|child| child.try_wait()) {
        None => return ServerStatus::Absent,
        Some(Ok(None)) => return ServerStatus::Running,
        Some(Ok(Some(status))) => ServerStatus::Exited(status.code()),
        Some(Err(_)) => ServerStatus::Exited(None),
    };
    guard.take();
    status
}

/// After the server closed its output: wait for the process to end, then report it. Does
/// nothing if [`stop_process`] already took the process.
fn wait_for_exit(python: &Path) {
    loop {
        match poll_server() {
            ServerStatus::Absent => return,
            ServerStatus::Running => std::thread::sleep(Duration::from_millis(200)),
            ServerStatus::Exited(code) => {
                if code != Some(0) {
                    if SAW_IMPORT_ERROR.load(Ordering::SeqCst) {
                        // A missing module: reinstall dependencies on the next start.
                        let _ = std::fs::remove_file(requirements_marker(python));
                    }
                    emit_line(format!("[Server exited with code {}]", code.map_or("?".into(), |c| c.to_string())));
                } else {
                    emit_line("[Server stopped]");
                }
                emit_state(false);
                return;
            }
        }
    }
}

// ── Dependencies ─────────────────────────────────────────────────────────────

/// The record of what was installed into the venv of `python`, kept inside that venv: a rebuilt
/// or deleted venv takes it along, so it can't claim packages the venv doesn't have.
fn requirements_marker(python: &Path) -> PathBuf {
    let venv = python.parent().and_then(Path::parent).unwrap_or(Path::new("."));
    venv.join("mula-requirements.sha256")
}

/// Identifies "these requirements installed into this interpreter".
fn requirements_fingerprint(python: &Path, requirements: &[u8]) -> String {
    let mut identity = python.to_string_lossy().as_bytes().to_vec();
    identity.push(0);
    identity.extend_from_slice(requirements);
    util::sha256_hex(&identity)
}

/// Runs pip only when the interpreter or `requirements.txt` changed since the last successful
/// install (pip takes seconds even when nothing is missing). A server that exits with an error
/// with an import error clears the record, so a broken environment is repaired on the next start.
fn ensure_requirements(python: &Path, working_dir: &Path) -> Result<(), String> {
    let requirements = working_dir.join("requirements.txt");
    let Ok(contents) = std::fs::read(&requirements) else {
        return Ok(());
    };
    let fingerprint = requirements_fingerprint(python, &contents);
    let marker = requirements_marker(python);
    if std::fs::read_to_string(&marker).is_ok_and(|recorded| recorded.trim() == fingerprint) {
        return Ok(());
    }

    emit_line("[Installing dependencies...]");
    let mut pip = util::command(python);
    pip.args(["-m", "pip", "install", "--quiet", "--disable-pip-version-check", "-r"])
        .arg(&requirements)
        .current_dir(working_dir);
    if let Err(e) = util::run_command(&mut pip, "pip install") {
        emit_line(format!("[{e}]"));
        return Err(e);
    }
    let _ = util::write_atomic(&marker, &fingerprint);
    emit_line("[Dependencies OK]");
    Ok(())
}

// ── Process lifecycle ────────────────────────────────────────────────────────

fn start() -> Result<(), String> {
    if is_running() || STARTING.swap(true, Ordering::SeqCst) {
        return Err("VSD Server is already running".to_string());
    }
    let result = spawn_server();
    STARTING.store(false, Ordering::SeqCst);
    result
}

/// Installs dependencies if needed and launches the server. The process lock is only taken to
/// store the child, so status checks never wait for pip.
fn spawn_server() -> Result<(), String> {
    let working_dir = module_dir()?;
    let python = vsd_python(&working_dir)?;
    ensure_requirements(&python, &working_dir)?;

    let config = load_config()?;
    SAW_IMPORT_ERROR.store(false, Ordering::SeqCst);
    let mut child = util::command(&python)
        .arg(working_dir.join("server.py"))
        .current_dir(&working_dir)
        .env("VSD_LOG_DIR", data_path("logs")?)
        .env("VSD_ENV_FILE", data_path("vsd.env")?)
        .env("VSD_DOWNLOAD_DIR", &config.download_dir)
        .env("VSD_PORT", config.port.to_string())
        .env("VSD_ADMIN_TOKEN", ADMIN_TOKEN.as_str())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start VSD server: {e}"))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    // Store the child before the readers start, so a server that exits at once is noticed.
    *util::lock(&PROCESS) = Some(child);
    if let Some(stdout) = stdout {
        spawn_reader(stdout, Some(python));
    }
    if let Some(stderr) = stderr {
        spawn_reader(stderr, None);
    }

    emit_state(true);
    Ok(())
}

/// Stops the server if it runs; returns whether it did.
pub fn stop_process() -> bool {
    let child = util::lock(&PROCESS).take();
    match child {
        Some(mut child) => {
            let _ = child.kill();
            let _ = child.wait();
            true
        }
        None => false,
    }
}

/// Whether the server runs. Doesn't reap an exited server, so [`wait_for_exit`] still reports it.
fn is_running() -> bool {
    matches!(util::lock(&PROCESS).as_mut().map(|child| child.try_wait()), Some(Ok(None)))
}

/// Starts the server at launch when the user enabled "start automatically".
pub fn autostart() {
    match load_config() {
        Ok(config) if config.autostart => {}
        Ok(_) => return,
        Err(e) => {
            log::error!("Could not read the VSD settings: {e}");
            return;
        }
    }
    std::thread::spawn(move || {
        emit_line("[Autostart: starting server...]");
        if let Err(e) = start() {
            emit_line(format!("[Error] {e}"));
            emit_state(false);
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
pub async fn vsd_start() -> CmdResult<()> {
    Ok(util::blocking(start).await?)
}

#[tauri::command]
pub async fn vsd_stop() -> CmdResult<()> {
    util::blocking(move || {
        if !stop_process() {
            return Err("VSD Server is not running".into());
        }
        emit_line("[Server stopped]");
        emit_state(false);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn vsd_is_running() -> CmdResult<bool> {
    util::blocking(|| Ok(is_running())).await
}

/// Recent server output (the UI receives new lines as `vsd-log` events).
#[tauri::command]
pub fn vsd_get_logs() -> Vec<LogLine> {
    util::lock(&LOG_BACKLOG).iter().cloned().collect()
}

#[tauri::command]
pub async fn vsd_get_download_dir() -> CmdResult<String> {
    util::blocking(|| Ok(load_config()?.download_dir)).await
}

#[tauri::command]
pub async fn vsd_set_download_dir(path: String) -> CmdResult<()> {
    util::blocking(move || {
        let config = update_config(|c| c.download_dir = path.clone())?;
        if is_running() {
            if let Err(e) = notify_download_dir(config.port, &path) {
                log::error!("Failed to update the running server's download dir: {e}");
            }
        }
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn vsd_get_autostart() -> CmdResult<bool> {
    util::blocking(|| Ok(load_config()?.autostart)).await
}

#[tauri::command]
pub async fn vsd_set_autostart(enabled: bool) -> CmdResult<()> {
    util::blocking(move || Ok(update_config(|c| c.autostart = enabled).map(|_| ())?)).await
}

/// Builds the browser extension and returns its folder (the UI reveals it).
#[tauri::command]
pub async fn vsd_install_extension(browser: String) -> CmdResult<String> {
    util::blocking(move || {
        let vsd_dir = module_dir()?;
        // Debug builds keep it in the project (`extension/dist`, where a loaded unpacked extension
        // already points); release builds use MULA's data folder, as the installed VSD folder may
        // be read-only.
        let dist = if cfg!(debug_assertions) {
            vsd_dir.join("extension").join("dist")
        } else {
            data_path("extension")?
        };
        let ext_dir = build_extension(&vsd_dir, &browser, load_config()?.port, &dist)?;
        Ok(ext_dir.to_string_lossy().to_string())
    })
    .await
}

/// Builds the `browser` extension into `dist/<browser>`, talking to the server on `port` (the one
/// this MULA starts it on), and returns that folder.
fn build_extension(vsd_dir: &Path, browser: &str, port: u16, dist: &Path) -> Result<PathBuf, String> {
    let build_script = vsd_dir.join("extension").join("build.py");
    if !build_script.exists() {
        return Err("Could not find extension build script".into());
    }
    let server_url = format!("http://127.0.0.1:{port}");
    // build.py only needs the standard library, so the system Python will do.
    util::run_command(
        util::command(system_python())
            .arg(&build_script)
            .arg(browser)
            .args(["--server-url", &server_url])
            .arg("--out-dir")
            .arg(dist),
        "the extension build",
    )?;

    let ext_dir = dist.join(browser);
    if !ext_dir.exists() {
        return Err(format!("Extension not found at {}", ext_dir.display()));
    }
    Ok(ext_dir)
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
        assert_eq!(util::read_json_or_default::<VsdConfig>(&path).unwrap(), config);
        // A partial file keeps the defaults for what's missing.
        std::fs::write(&path, r#"{"autostart": true}"#).unwrap();
        let partial: VsdConfig = util::read_json_or_default(&path).unwrap();
        assert!(partial.autostart);
        assert_eq!(partial.port, 8765);
    }

    #[test]
    fn python_from_a_local_venv_is_preferred() {
        // With a `.venv` next to the server, MULA's own venv is neither used nor created.
        let dir = TempDir::new("venv");
        let local = venv_python(&dir.join(".venv"));
        std::fs::create_dir_all(local.parent().unwrap()).unwrap();
        std::fs::write(&local, b"").unwrap();
        assert_eq!(vsd_python(dir.path()).unwrap(), local);
    }

    /// A venv with a (fake) python, its own pip and a `pyvenv.cfg` naming `home`.
    fn fake_venv(dir: &TempDir, home: &Path) -> PathBuf {
        let venv = dir.join("venv");
        let python = venv_python(&venv);
        std::fs::create_dir_all(python.parent().unwrap()).unwrap();
        std::fs::write(&python, b"").unwrap();
        let site_packages = if cfg!(target_os = "windows") {
            venv.join("Lib").join("site-packages")
        } else {
            venv.join("lib").join("python3.12").join("site-packages")
        };
        std::fs::create_dir_all(site_packages.join("pip")).unwrap();
        std::fs::write(venv.join("pyvenv.cfg"), format!("home = {}\nversion = 3.12.0\n", home.display()))
            .unwrap();
        venv
    }

    #[test]
    fn a_missing_or_half_made_venv_is_broken() {
        let dir = TempDir::new("venv_broken");
        let venv = dir.join("venv");
        assert_eq!(venv_state(&venv), VenvState::Broken);
        // Interrupted before pip was installed: a python but no pip.
        let python = venv_python(&venv);
        std::fs::create_dir_all(python.parent().unwrap()).unwrap();
        std::fs::write(&python, b"").unwrap();
        assert_eq!(venv_state(&venv), VenvState::Broken);
    }

    #[test]
    fn a_venv_whose_base_python_is_gone_is_broken() {
        let dir = TempDir::new("venv_no_base");
        let venv = fake_venv(&dir, &dir.join("removed-python"));
        assert_eq!(venv_home(&venv), Some(dir.join("removed-python")));
        assert_eq!(venv_state(&venv), VenvState::Broken);
    }

    #[test]
    fn a_venv_that_cannot_be_checked_is_kept() {
        // Complete on disk, but its python can't run (the fake is an empty file): no verdict, so
        // it isn't deleted and rebuilt.
        let dir = TempDir::new("venv_unknown");
        let venv = fake_venv(&dir, dir.path());
        assert!(matches!(venv_state(&venv), VenvState::Unknown(_)));
    }

    #[test]
    fn creating_a_venv_replaces_a_broken_one_and_reports_failure() {
        let dir = TempDir::new("venv_failed");
        let venv = dir.join("venv");
        std::fs::create_dir_all(venv.join("Scripts")).unwrap();
        std::fs::write(venv.join("pyvenv.cfg"), b"half made").unwrap();
        assert!(create_venv("mula-test-no-such-python", &venv).is_err());
        assert!(!venv.exists());
    }

    /// Slow (a real venv with pip) and needs Python: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn a_created_venv_works() {
        let dir = TempDir::new("venv_created");
        let venv = dir.join("venv");
        create_venv(system_python(), &venv).unwrap();
        assert_eq!(venv_state(&venv), VenvState::Works);
        // pip only from the system, as after a Python upgrade on Linux: broken.
        let site_packages = if cfg!(target_os = "windows") {
            venv.join("Lib").join("site-packages")
        } else {
            std::fs::read_dir(venv.join("lib")).unwrap().flatten().next().unwrap().path().join("site-packages")
        };
        std::fs::rename(site_packages.join("pip"), site_packages.join("pip-moved")).unwrap();
        std::fs::create_dir_all(site_packages.join("pip")).unwrap(); // on disk, but not importable
        assert_eq!(venv_state(&venv), VenvState::Broken);
    }

    #[test]
    fn the_requirements_record_lives_in_the_venv() {
        let venv = Path::new("data").join("vsd").join("venv");
        assert_eq!(requirements_marker(&venv_python(&venv)), venv.join("mula-requirements.sha256"));
    }

    fn project_module_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("modules").join("vsd")
    }

    /// The way release builds use it: into a folder outside the module (MULA's data folder).
    #[test]
    fn the_extension_builds_into_the_given_folder_for_the_given_port() {
        let dir = TempDir::new("extension");
        let dist = dir.join("extension");
        let built = build_extension(&project_module_dir(), "chrome", 9123, &dist).unwrap();
        assert_eq!(built, dist.join("chrome"));
        assert!(built.join("manifest.json").exists());
        let config = std::fs::read_to_string(built.join("config.js")).unwrap();
        assert!(config.contains("'http://127.0.0.1:9123'"));
    }

    /// Debug builds run the project's copy, so a file missing from `bundle.resources` would only
    /// break installed builds. Every file the module ships must be bundled.
    #[test]
    fn every_module_file_is_bundled() {
        let conf: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json")).unwrap(),
        )
        .unwrap();
        // Sources are relative to src-tauri, e.g. "../modules/vsd/server.py" or a folder ending in "/".
        let sources: Vec<String> = conf["bundle"]["resources"]
            .as_object()
            .unwrap()
            .keys()
            .map(|source| source.trim_start_matches("../modules/vsd/").to_string())
            .collect();
        // Not shipped: the user's own settings, generated output, tests and developer tooling.
        let not_shipped = [".env", ".venv", "logs", "tests", "extension/dist", "__pycache__"];

        fn walk(dir: &Path, root: &Path, files: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                let relative = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
                if path.is_dir() {
                    walk(&path, root, files);
                } else {
                    files.push(relative);
                }
            }
        }
        let root = project_module_dir();
        let mut files = Vec::new();
        walk(&root, &root, &mut files);

        let unbundled: Vec<&String> = files
            .iter()
            .filter(|file| {
                !not_shipped.iter().any(|skip| *file == skip || file.starts_with(&format!("{skip}/")))
            })
            .filter(|file| !file.contains("/__pycache__/"))
            .filter(|file| {
                !sources.iter().any(|source| *file == source || (source.ends_with('/') && file.starts_with(source.as_str())))
            })
            .collect();
        assert!(unbundled.is_empty(), "add these to bundle.resources in tauri.conf.json: {unbundled:?}");
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
    fn only_import_errors_mark_the_environment_as_broken() {
        assert!(is_import_error("ModuleNotFoundError: No module named 'flask'"));
        assert!(is_import_error("ImportError: cannot import name 'CORS'"));
        assert!(!is_import_error("OSError: [WinError 10048] Only one usage of each socket address"));
    }

    #[test]
    fn the_admin_token_is_a_long_random_secret() {
        assert_eq!(ADMIN_TOKEN.len(), 64);
        assert!(ADMIN_TOKEN.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
