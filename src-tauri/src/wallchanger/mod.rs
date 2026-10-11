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

use crate::util::blocking;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// The UI lists this many images per queue; the rest is summarized by `image_count`.
const QUEUE_PREVIEW_LIMIT: usize = 30;

pub use settings::Settings;

/// Width / height, treating a zero height as square.
pub(crate) fn aspect_ratio(width: i32, height: i32) -> f64 {
    if height == 0 {
        1.0
    } else {
        width as f64 / height as f64
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Monitor {
    pub id: String,
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub width: i32,
    pub height: i32,
}

impl Monitor {
    pub fn aspect_ratio(&self) -> f64 {
        aspect_ratio(self.width, self.height)
    }
}

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

/// Keeps the tray's "Max source level" items and the Wall Changer tab in sync after a change.
pub fn on_max_level_changed(app: &tauri::AppHandle, level: i32) {
    use tauri::Emitter;
    sync_max_level_menu(app, level);
    let _ = app.emit("max-level-changed", level);
}

#[tauri::command]
pub async fn wc_get_settings() -> Result<Settings, String> {
    blocking(settings::load_config).await
}

/// Saves the user-editable configuration only; runtime state (queue positions, statistics,
/// image cache, service on/off) lives in a separate file the UI never writes.
#[tauri::command]
pub async fn wc_save_settings(app: tauri::AppHandle, settings: Settings) -> Result<(), String> {
    let saved = blocking(move || {
        let mut settings = settings;
        settings::normalize(&mut settings);
        settings::save_config(&settings)?;
        Ok(settings)
    })
    .await?;

    on_max_level_changed(&app, saved.maximum_source_level);
    Ok(())
}

#[tauri::command]
pub async fn wc_get_monitors() -> Result<Vec<Monitor>, String> {
    blocking(monitors::get_monitors).await
}

#[tauri::command]
pub async fn wc_change_now() -> Result<String, String> {
    blocking(|| service::apply(false)).await
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

    if queue::uses_shared_queue(&settings, monitors.len()) {
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
        .take(QUEUE_PREVIEW_LIMIT)
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
