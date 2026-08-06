mod images;
mod monitors;
mod queue;
pub mod service;
pub mod settings;

pub use monitors::Monitor;
pub use settings::Settings;

/// Holds handles to the tray's "Max source level" check items (one per level 1-10)
/// so they can be kept in sync with the level chosen in the Wall Changer tab.
pub struct MaxLevelMenuState(pub Vec<(i32, tauri::menu::CheckMenuItem<tauri::Wry>)>);

pub fn sync_max_level_menu(app: &tauri::AppHandle, level: i32) {
    use tauri::Manager;
    if let Some(state) = app.try_state::<MaxLevelMenuState>() {
        for (item_level, item) in &state.0 {
            let _ = item.set_checked(*item_level == level);
        }
    }
}

#[tauri::command]
pub fn wc_get_settings() -> Result<Settings, String> {
    settings::load()
}

#[tauri::command]
pub fn wc_save_settings(app: tauri::AppHandle, mut settings: Settings) -> Result<(), String> {
    use tauri::Emitter;

    settings::normalize(&mut settings);
    settings::save(&settings)?;

    sync_max_level_menu(&app, settings.maximum_source_level);
    let _ = app.emit("max-level-changed", settings.maximum_source_level);

    Ok(())
}

#[tauri::command]
pub fn wc_get_monitors() -> Result<Vec<Monitor>, String> {
    monitors::get_monitors()
}

#[tauri::command]
pub fn wc_apply() -> Result<String, String> {
    let mut settings = settings::load()?;
    let result = service::apply(&mut settings, false)?;
    settings::save(&settings)?;
    Ok(result)
}

#[tauri::command]
pub fn wc_change_now() -> Result<String, String> {
    let mut settings = settings::load()?;
    let result = service::apply(&mut settings, false)?;
    settings::save(&settings)?;
    Ok(result)
}

#[tauri::command]
pub fn wc_start_service() -> Result<(), String> {
    let mut settings = settings::load()?;
    settings.change_service_running = true;
    settings::save(&settings)?;
    service::start()
}

#[tauri::command]
pub fn wc_stop_service() -> Result<(), String> {
    let mut settings = settings::load()?;
    settings.change_service_running = false;
    settings::save(&settings)?;
    service::stop()
}

#[tauri::command]
pub fn wc_toggle_service() -> Result<bool, String> {
    service::toggle_service_running()
}

#[tauri::command]
pub fn wc_get_status() -> Result<serde_json::Value, String> {
    let settings = settings::load()?;
    Ok(serde_json::json!({
        "running": settings.change_service_running,
        "interval_minutes": settings.interval_minutes,
        "maximum_source_level": settings.maximum_source_level,
    }))
}
