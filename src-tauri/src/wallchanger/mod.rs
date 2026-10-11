mod images;
#[cfg_attr(target_os = "windows", path = "monitors/windows.rs")]
#[cfg_attr(not(target_os = "windows"), path = "monitors/linux.rs")]
mod monitors;
mod overlay;
mod preview;
mod queue;
pub mod service;
pub mod settings;
mod transition;
mod wallhaven;

use serde::Serialize;
use std::sync::OnceLock;

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

static APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();

/// Lets background code (the interval service, tray actions) notify the UI.
pub fn set_app_handle(app: tauri::AppHandle) {
    let _ = APP_HANDLE.set(app);
}

pub(crate) fn emit<S: Serialize + Clone>(event: &str, payload: S) {
    use tauri::Emitter;
    if let Some(app) = APP_HANDLE.get() {
        let _ = app.emit(event, payload);
    }
}

pub fn sync_max_level_menu(app: &tauri::AppHandle, level: i32) {
    use tauri::Manager;
    if let Some(state) = app.try_state::<MaxLevelMenuState>() {
        for (item_level, item) in &state.0 {
            let _ = item.set_checked(*item_level == level);
        }
    }
}

/// Runs blocking work (disk scans, COM calls, image decoding, network) off the UI thread.
/// Tauri runs synchronous commands on the main thread, which would freeze the window.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("Background task failed: {e}"))?
}

#[tauri::command]
pub async fn wc_get_settings() -> Result<Settings, String> {
    blocking(settings::load_config).await
}

/// Saves the user-editable configuration only; runtime state (queue positions, statistics,
/// image cache, service on/off) lives in a separate file the UI never writes.
#[tauri::command]
pub async fn wc_save_settings(app: tauri::AppHandle, settings: Settings) -> Result<(), String> {
    use tauri::Emitter;

    let saved = blocking(move || {
        let mut settings = settings;
        settings::normalize(&mut settings);
        settings::save_config(&settings)?;
        Ok(settings)
    })
    .await?;

    sync_max_level_menu(&app, saved.maximum_source_level);
    let _ = app.emit("max-level-changed", saved.maximum_source_level);

    Ok(())
}

#[tauri::command]
pub async fn wc_get_monitors() -> Result<Vec<Monitor>, String> {
    blocking(monitors::get_monitors).await
}

#[tauri::command]
pub async fn wc_apply() -> Result<String, String> {
    blocking(|| service::apply(false)).await
}

#[tauri::command]
pub async fn wc_change_now() -> Result<String, String> {
    blocking(|| service::apply(false)).await
}

#[tauri::command]
pub async fn wc_start_service() -> Result<(), String> {
    blocking(|| service::set_running(true)).await
}

#[tauri::command]
pub async fn wc_stop_service() -> Result<(), String> {
    blocking(|| service::set_running(false)).await
}

#[tauri::command]
pub async fn wc_toggle_service() -> Result<bool, String> {
    blocking(|| {
        let running = !settings::service_running()?;
        service::set_running(running)?;
        Ok(running)
    })
    .await
}

#[tauri::command]
pub async fn wc_get_status() -> Result<serde_json::Value, String> {
    blocking(|| {
        let config = settings::load_config()?;
        Ok(serde_json::json!({
            "running": settings::service_running()?,
            "interval_minutes": config.interval_minutes,
            "maximum_source_level": config.maximum_source_level,
        }))
    })
    .await
}

#[tauri::command]
pub async fn wc_get_queue_preview() -> Result<Vec<QueuePreview>, String> {
    blocking(build_queue_previews).await
}

#[tauri::command]
pub async fn wc_get_monitor_preview() -> Result<Vec<preview::MonitorPreview>, String> {
    blocking(|| {
        let settings = settings::load_config()?;
        preview::build_monitor_previews(&settings)
    })
    .await
}

fn build_queue_previews() -> Result<Vec<QueuePreview>, String> {
    let mut settings = settings::load()?;
    // The preview must stay cheap: only use Wallhaven images that are already downloaded.
    let images = images::load_images(&mut settings, images::NetworkPolicy::CacheOnly)?;
    let monitors = monitors::get_monitors().unwrap_or_default();

    let queues = queue::build_queues(&images, &monitors, &settings);
    let rotation_label = match settings.rotation_mode {
        settings::RotationMode::Random => "Random",
        settings::RotationMode::Sequence => "Sequence",
    };

    let mut previews = Vec::new();

    if !settings.use_separate_monitor_queues || monitors.len() <= 1 {
        if let Some(queue) = queues.get("shared").cloned() {
            previews.push(build_queue_preview(
                &mut settings,
                "shared".to_string(),
                "Shared queue".to_string(),
                &queue,
                rotation_label,
            ));
        }
    } else {
        for (index, monitor) in monitors.iter().enumerate() {
            let key = queue::get_queue_key(&settings, monitors.len(), &monitor.id, false);
            if let Some(queue) = queues.get(&key).cloned() {
                let label = format!(
                    "Monitor {} ({}x{})",
                    index + 1,
                    monitor.width,
                    monitor.height
                );
                previews.push(build_queue_preview(
                    &mut settings,
                    key,
                    label,
                    &queue,
                    rotation_label,
                ));
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
