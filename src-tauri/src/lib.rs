use serde::Serialize;

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

    // Ensure the directory exists before opening
    std::fs::create_dir_all(&config_dir)
        .map_err(|e| format!("Failed to create config directory: {e}"))?;

    Ok(config_dir.to_string_lossy().to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![get_app_info, open_config_dir])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
