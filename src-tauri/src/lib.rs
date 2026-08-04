use serde::Serialize;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

static VSD_PROCESS: Mutex<Option<Child>> = Mutex::new(None);
static VSD_LOG_BUFFER: Mutex<Vec<String>> = Mutex::new(Vec::new());

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

    let mut child = Command::new(&python)
        .arg(&server_path)
        .current_dir(&working_dir)
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

#[tauri::command]
fn vsd_get_download_dir() -> String {
    let vsd_dir = get_vsd_server_path()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));

    if let Some(dir) = vsd_dir {
        let env_path = dir.join(".env");
        if env_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&env_path) {
                for line in content.lines() {
                    if let Some(val) = line.strip_prefix("VSD_DOWNLOAD_DIR=") {
                        return val.trim().to_string();
                    }
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
    let vsd_dir = get_vsd_server_path()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .ok_or("Could not find VSD module directory")?;

    let env_path = vsd_dir.join(".env");

    // Read existing .env or start fresh
    let mut lines: Vec<String> = if env_path.exists() {
        std::fs::read_to_string(&env_path)
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

    std::fs::write(&env_path, lines.join("\n") + "\n")
        .map_err(|e| format!("Failed to write .env: {e}"))?;

    Ok(())
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
