use serde::Serialize;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

static VSD_PROCESS: Mutex<Option<Child>> = Mutex::new(None);
static VSD_LOG_BUFFER: Mutex<Vec<String>> = Mutex::new(Vec::new());
static VSD_DEPS_INSTALLED: Mutex<bool> = Mutex::new(false);

#[derive(Serialize)]
pub struct AppInfo {
    version: String,
    config_path: String,
    platform: String,
    arch: String,
}

#[tauri::command]
fn get_app_info() -> AppInfo {
    let config_dir = dirs::config_dir()
        .map(|p| p.join("mula").to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    AppInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        config_path: config_dir,
        platform: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
    }
}

#[tauri::command]
fn open_config_dir() -> Result<String, String> {
    let config_dir = dirs::config_dir()
        .map(|p| p.join("mula"))
        .ok_or("Could not determine config directory")?;

    std::fs::create_dir_all(&config_dir)
        .map_err(|e| format!("Failed to create config directory: {e}"))?;

    Ok(config_dir.to_string_lossy().to_string())
}

// ── VSD Server commands ──

fn get_vsd_server_path() -> Option<std::path::PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();

    let candidates: Vec<std::path::PathBuf> = vec![
        // Development: relative to project root (exe is in src-tauri/target/debug/)
        exe_dir.join("../../../modules/vsd/server.py"),
        // Installed: next to the executable
        exe_dir.join("modules/vsd/server.py"),
    ];

    // Also check legacy location
    let mut all_candidates = candidates;
    if let Some(home) = dirs::home_dir() {
        all_candidates.push(home.join("CascadeProjects").join("VSD Experiment").join("companion").join("server.py"));
    }

    for candidate in all_candidates {
        if candidate.exists() {
            return candidate.canonicalize().ok().or(Some(candidate));
        }
    }
    None
}

fn get_python_command() -> String {
    // Try python3 first, fall back to python
    if cfg!(target_os = "windows") {
        "python".to_string()
    } else {
        "python3".to_string()
    }
}

#[tauri::command]
fn vsd_start() -> Result<(), String> {
    let mut proc_guard = VSD_PROCESS.lock().map_err(|e| e.to_string())?;

    if proc_guard.is_some() {
        return Err("VSD Server is already running".to_string());
    }

    let server_path = get_vsd_server_path()
        .ok_or("Could not find VSD server.py")?;

    let python = get_python_command();
    let working_dir = server_path.parent().unwrap().to_path_buf();

    // Install dependencies if needed
    let requirements = working_dir.join("requirements.txt");
    if requirements.exists() {
        if let Ok(mut logs) = VSD_LOG_BUFFER.lock() {
            logs.clear();
            logs.push("[Checking dependencies...]\n".to_string());
        }

        let install = Command::new(&python)
            .args(["-m", "pip", "install", "-r"])
            .arg(&requirements)
            .arg("--quiet")
            .current_dir(&working_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();

        match install {
            Ok(output) => {
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    if let Ok(mut logs) = VSD_LOG_BUFFER.lock() {
                        logs.push(format!("[pip install failed: {}]\n", stderr.trim()));
                    }
                    return Err(format!("Failed to install dependencies: {}", stderr.trim()));
                }
                if let Ok(mut logs) = VSD_LOG_BUFFER.lock() {
                    logs.push("[Dependencies OK]\n".to_string());
                }
            }
            Err(e) => {
                return Err(format!("Failed to run pip: {e}"));
            }
        }
    } else {
        // Clear log buffer
        if let Ok(mut logs) = VSD_LOG_BUFFER.lock() {
            logs.clear();
        }
    }

    // Pass configured download dir to server via env var
    let download_dir = vsd_get_download_dir();

    let mut child = Command::new(&python)
        .arg(&server_path)
        .current_dir(&working_dir)
        .env("VSD_DOWNLOAD_DIR", &download_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start VSD server: {e}"))?;

    // Spawn thread to read stdout
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                if let Ok(line) = line {
                    if let Ok(mut logs) = VSD_LOG_BUFFER.lock() {
                        logs.push(format!("{line}\n"));
                        if logs.len() > 1000 {
                            logs.drain(0..100);
                        }
                    }
                }
            }
        });
    }

    // Spawn thread to read stderr
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines() {
                if let Ok(line) = line {
                    if let Ok(mut logs) = VSD_LOG_BUFFER.lock() {
                        logs.push(format!("{line}\n"));
                        if logs.len() > 1000 {
                            logs.drain(0..100);
                        }
                    }
                }
            }
        });
    }

    *proc_guard = Some(child);
    Ok(())
}

#[tauri::command]
fn vsd_stop() -> Result<(), String> {
    let mut proc_guard = VSD_PROCESS.lock().map_err(|e| e.to_string())?;

    if let Some(mut child) = proc_guard.take() {
        child.kill().map_err(|e| format!("Failed to stop VSD server: {e}"))?;
        let _ = child.wait();
        Ok(())
    } else {
        Err("VSD Server is not running".to_string())
    }
}

#[tauri::command]
fn vsd_is_running() -> bool {
    if let Ok(mut proc_guard) = VSD_PROCESS.lock() {
        if let Some(child) = proc_guard.as_mut() {
            // Check if process is still alive
            match child.try_wait() {
                Ok(None) => true,     // still running
                Ok(Some(_)) => {
                    *proc_guard = None; // process exited
                    false
                }
                Err(_) => false,
            }
        } else {
            false
        }
    } else {
        false
    }
}

/// Returns new log lines since last call (drains the buffer)
#[tauri::command]
fn vsd_get_logs() -> Vec<String> {
    if let Ok(mut logs) = VSD_LOG_BUFFER.lock() {
        let lines: Vec<String> = logs.drain(..).collect();
        lines
    } else {
        vec![]
    }
}

/// Get the MULA config directory (e.g. C:\Users\<user>\AppData\Roaming\mula)
fn get_config_dir() -> std::path::PathBuf {
    dirs::config_dir()
        .map(|p| p.join("mula"))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// Get the config file path for the VSD module settings
fn get_vsd_config_path() -> std::path::PathBuf {
    get_config_dir().join("vsd.conf")
}

#[tauri::command]
fn vsd_get_download_dir() -> String {
    let config_path = get_vsd_config_path();
    if config_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&config_path) {
            for line in content.lines() {
                if let Some(val) = line.strip_prefix("VSD_DOWNLOAD_DIR=") {
                    return val.trim().to_string();
                }
            }
        }
    }

    dirs::home_dir()
        .map(|h| h.join("Downloads").join("VSD").to_string_lossy().to_string())
        .unwrap_or_default()
}

#[tauri::command]
fn vsd_set_download_dir(path: String) -> Result<(), String> {
    let config_path = get_vsd_config_path();
    let config_dir = config_path.parent().unwrap();
    std::fs::create_dir_all(config_dir)
        .map_err(|e| format!("Failed to create config dir: {e}"))?;

    // Read existing config or start fresh
    let mut lines: Vec<String> = if config_path.exists() {
        std::fs::read_to_string(&config_path)
            .unwrap_or_default()
            .lines()
            .map(|l| l.to_string())
            .collect()
    } else {
        vec![]
    };

    // Update or add the VSD_DOWNLOAD_DIR line
    let new_line = format!("VSD_DOWNLOAD_DIR={path}");
    let mut found = false;
    for line in lines.iter_mut() {
        if line.starts_with("VSD_DOWNLOAD_DIR=") {
            *line = new_line.clone();
            found = true;
            break;
        }
    }
    if !found {
        lines.push(new_line);
    }

    std::fs::write(&config_path, lines.join("\n") + "\n")
        .map_err(|e| format!("Failed to write config: {e}"))?;

    // If server is running, update it live via API
    if vsd_is_running() {
        let body = format!(r#"{{"path":"{}"}}"#, path.replace('\\', "\\\\").replace('"', "\\\""));
        let _ = std::thread::spawn(move || {
            let req = urllib_post("http://127.0.0.1:8765/config/download_dir", &body);
            if let Err(e) = req {
                eprintln!("Failed to update running server download dir: {e}");
            }
        });
    }

    Ok(())
}

fn urllib_post(url: &str, body: &str) -> Result<(), String> {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let addr = "127.0.0.1:8765";
    let mut stream = TcpStream::connect(addr).map_err(|e| e.to_string())?;
    let request = format!(
        "POST /config/download_dir HTTP/1.1\r\nHost: 127.0.0.1:8765\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(), body
    );
    stream.write_all(request.as_bytes()).map_err(|e| e.to_string())?;
    let mut response = String::new();
    stream.read_to_string(&mut response).map_err(|e| e.to_string())?;
    let _ = url; // used in format above
    Ok(())
}

/// Get the extension directory for a given browser target
fn get_extension_dir(browser: &str) -> Option<std::path::PathBuf> {
    let vsd_dir = get_vsd_server_path()?.parent()?.to_path_buf();
    let ext_dir = vsd_dir.join("extension").join("dist").join(browser);
    Some(ext_dir)
}

/// Get the extension build script path
fn get_extension_build_script() -> Option<std::path::PathBuf> {
    let vsd_dir = get_vsd_server_path()?.parent()?.to_path_buf();
    let build_script = vsd_dir.join("extension").join("build.py");
    if build_script.exists() { Some(build_script) } else { None }
}

#[tauri::command]
fn vsd_install_extension(browser: String) -> Result<String, String> {
    let build_script = get_extension_build_script()
        .ok_or("Could not find extension build script")?;

    let python = get_python_command();

    // Build the extension
    let output = Command::new(&python)
        .arg(&build_script)
        .arg(&browser)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to run build script: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Build failed: {}", stderr.trim()));
    }

    let ext_dir = get_extension_dir(&browser)
        .ok_or("Could not determine extension output directory")?;

    if !ext_dir.exists() {
        return Err(format!("Extension not found at {}", ext_dir.display()));
    }

    // Open the browser's extension management page
    let open_result = if cfg!(target_os = "windows") {
        match browser.as_str() {
            "chrome" => {
                // Use chrome.exe directly with the chrome:// URL
                Command::new("chrome").arg("chrome://extensions/").spawn()
                    .or_else(|_| Command::new("C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe")
                        .arg("chrome://extensions/").spawn())
                    .or_else(|_| Command::new("C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe")
                        .arg("chrome://extensions/").spawn())
            }
            "firefox" => {
                Command::new("firefox").arg("about:debugging#/runtime/this-firefox").spawn()
                    .or_else(|_| Command::new("C:\\Program Files\\Mozilla Firefox\\firefox.exe")
                        .arg("about:debugging#/runtime/this-firefox").spawn())
            }
            _ => return Err(format!("Unknown browser: {browser}")),
        }
    } else {
        match browser.as_str() {
            "chrome" => Command::new("google-chrome").arg("chrome://extensions/").spawn()
                .or_else(|_| Command::new("chromium-browser").arg("chrome://extensions/").spawn()),
            "firefox" => Command::new("firefox").arg("about:debugging#/runtime/this-firefox").spawn(),
            _ => return Err(format!("Unknown browser: {browser}")),
        }
    };

    if let Err(e) = open_result {
        return Err(format!("Built extension but failed to open {browser}: {e}\nLoad manually from: {}", ext_dir.display()));
    }

    Ok(ext_dir.to_string_lossy().to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            get_app_info,
            open_config_dir,
            vsd_start,
            vsd_stop,
            vsd_is_running,
            vsd_get_logs,
            vsd_get_download_dir,
            vsd_set_download_dir,
            vsd_install_extension,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
