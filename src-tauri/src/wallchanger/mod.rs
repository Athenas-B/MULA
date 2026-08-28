mod images;
mod monitors;
mod overlay;
mod queue;
pub mod service;
pub mod settings;
mod transition;

use serde::Serialize;

pub use monitors::Monitor;
pub use settings::Settings;

#[derive(Debug, Clone, Serialize)]
pub struct QueueImagePreview {
    pub path: String,
    pub file_name: String,
    pub last_resort: bool,
    pub is_next: bool,
    pub is_last_shown: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueuePreview {
    pub key: String,
    pub label: String,
    pub rotation_mode: String,
    pub image_count: usize,
    pub last_shown: Option<String>,
    pub images: Vec<QueueImagePreview>,
}

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

#[tauri::command]
pub fn wc_get_queue_preview() -> Result<Vec<QueuePreview>, String> {
    let mut settings = settings::load()?;
    let images = images::load_images(&mut settings)?;
    let monitors = monitors::get_monitors().unwrap_or_default();

    let queues = queue::build_queues(&images, &monitors, &settings);
    let rotation_label = match settings.rotation_mode {
        settings::RotationMode::Random => "Random",
        settings::RotationMode::Sequence => "Sequence",
    };

    let mut previews = Vec::new();

    if !settings.use_separate_monitor_queues || monitors.len() <= 1 {
        if let Some(queue) = queues.get("shared").cloned() {
            previews.push(build_queue_preview(&mut settings, "shared".to_string(), "Shared queue".to_string(), &queue, rotation_label));
        }
    } else {
        for (index, monitor) in monitors.iter().enumerate() {
            let key = queue::get_queue_key(&settings, monitors.len(), &monitor.id, false);
            if let Some(queue) = queues.get(&key).cloned() {
                let label = format!("Monitor {} ({}x{})", index + 1, monitor.width, monitor.height);
                previews.push(build_queue_preview(&mut settings, key, label, &queue, rotation_label));
            }
        }
    }

    Ok(previews)
}

fn build_queue_preview(
    settings: &mut Settings,
    key: String,
    label: String,
    queue: &[queue::RankedImage],
    rotation_label: &str,
) -> QueuePreview {
    let rotation_mode = settings.rotation_mode;
    let state = queue::ensure_queue_state(settings, &key, queue);
    let next_path = match rotation_mode {
        settings::RotationMode::Sequence => queue::peek_next_sequence_path(queue, state),
        settings::RotationMode::Random => None,
    };
    let last_shown = if state.last_image_path.is_empty() {
        None
    } else {
        Some(state.last_image_path.clone())
    };

    let images = queue
        .iter()
        .map(|ranked| QueueImagePreview {
            path: ranked.image.path.clone(),
            file_name: std::path::Path::new(&ranked.image.path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| ranked.image.path.clone()),
            last_resort: ranked.last_resort,
            is_next: next_path.as_deref() == Some(ranked.image.path.as_str()),
            is_last_shown: last_shown.as_deref() == Some(ranked.image.path.as_str()),
        })
        .collect();

    QueuePreview {
        key,
        label,
        rotation_mode: rotation_label.to_string(),
        image_count: queue.len(),
        last_shown,
        images,
    }
}
