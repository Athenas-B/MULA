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

use crate::error::CmdResult;
use crate::util::blocking;
use serde::{Deserialize, Serialize};

/// The UI lists this many images per queue; the rest is summarized by `image_count`.
const QUEUE_PREVIEW_LIMIT: usize = 30;


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
    pub label: String,
    pub rotation_mode: settings::RotationMode,
    pub image_count: usize,
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

/// Every settings change (UI or tray) ends here: the tray's "Max source level" items are synced
/// and the Wall Changer tab re-renders from the `wc-settings-changed` event.
pub fn on_settings_changed(app: &tauri::AppHandle, versioned: &settings::Versioned) {
    sync_max_level_menu(app, versioned.settings.maximum_source_level);
    service::wake();
    crate::events::emit(crate::events::WC_SETTINGS_CHANGED, versioned);
}

#[tauri::command]
pub async fn wc_get_settings() -> CmdResult<settings::Versioned> {
    Ok(blocking(settings::load_versioned).await?)
}

/// Applies a partial update (`{ setting: value, ... }`) and returns the normalized settings
/// with their revision.
#[tauri::command]
pub async fn wc_update_settings(app: tauri::AppHandle, patch: serde_json::Value) -> CmdResult<settings::Versioned> {
    let saved = blocking(move || settings::update_with_patch(patch)).await?;
    on_settings_changed(&app, &saved);
    Ok(saved)
}

#[tauri::command]
pub async fn wc_get_monitors() -> CmdResult<Vec<Monitor>> {
    Ok(blocking(monitors::get_monitors).await?)
}

#[tauri::command]
pub async fn wc_change_now() -> CmdResult<String> {
    Ok(blocking(|| service::apply(false)).await?)
}

#[tauri::command]
pub async fn wc_toggle_service() -> CmdResult<bool> {
    blocking(|| {
        let running = !settings::service_running();
        service::set_running(running)?;
        Ok(running)
    })
    .await
}

/// Whether the interval service is switched on.
#[tauri::command]
pub async fn wc_get_status() -> CmdResult<bool> {
    blocking(|| Ok(settings::service_running())).await
}

#[tauri::command]
pub async fn wc_get_queue_preview() -> CmdResult<Vec<QueuePreview>> {
    Ok(blocking(build_queue_previews).await?)
}

#[tauri::command]
pub async fn wc_get_monitor_preview() -> CmdResult<Vec<preview::MonitorPreview>> {
    Ok(blocking(|| preview::build_monitor_previews(&settings::load()?)).await?)
}

fn build_queue_previews() -> Result<Vec<QueuePreview>, String> {
    let settings = settings::load()?;
    let mut runtime = settings::load_state()?;
    // The preview must stay cheap: only use Wallhaven images that are already downloaded.
    let images = images::load_images(&settings, &mut runtime.image_cache_entries, images::NetworkPolicy::CacheOnly)?;
    let monitors = monitors::get_monitors().unwrap_or_default();
    let queues = queue::build_queues(&images, &monitors, &settings);

    let labelled: Vec<(String, String)> = if queue::uses_shared_queue(&settings, monitors.len()) {
        vec![("shared".to_string(), "Shared queue".to_string())]
    } else {
        monitors
            .iter()
            .enumerate()
            .map(|(index, monitor)| {
                let key = queue::get_queue_key(&settings, monitors.len(), &monitor.id, false);
                (key, format!("Monitor {} ({}x{})", index + 1, monitor.width, monitor.height))
            })
            .collect()
    };

    Ok(labelled
        .into_iter()
        .filter_map(|(key, label)| {
            let queue = queues.get(&key)?;
            Some(build_queue_preview(&mut runtime, settings.rotation_mode, &key, label, queue))
        })
        .collect())
}

fn build_queue_preview(
    runtime: &mut settings::RuntimeState,
    rotation_mode: settings::RotationMode,
    key: &str,
    label: String,
    queue: &[queue::RankedImage],
) -> QueuePreview {
    let state = queue::ensure_queue_state(runtime, key, queue);
    let next_path = match rotation_mode {
        settings::RotationMode::Sequence => queue::peek_next_sequence_path(queue, state),
        settings::RotationMode::Random => None,
    };
    let last_shown = Some(state.last_image_path.as_str()).filter(|p| !p.is_empty());

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
            is_last_shown: last_shown == Some(ranked.image.path.as_str()),
        })
        .collect();

    QueuePreview { label, rotation_mode, image_count: queue.len(), images }
}
