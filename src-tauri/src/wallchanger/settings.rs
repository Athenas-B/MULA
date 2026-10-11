use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum RotationMode {
    #[default]
    Random,
    Sequence,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum PictureSizeHandling {
    #[default]
    Allow,
    Avoid,
    UseOnlyAsLastResort,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum ScalingMode {
    #[default]
    Fill,
    FitInsideScreen,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum PictureNameOverlayTextMode {
    #[default]
    FileNameWithoutExtension,
    FileName,
    FolderAndFileName,
    FullPath,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Source {
    #[serde(default)]
    pub path: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub include_subfolders: bool,
    #[serde(default = "default_level")]
    pub level: i32,
    #[serde(default = "default_wallhaven_page_limit")]
    pub wallhaven_page_limit: i32,
    #[serde(default)]
    pub wallhaven_purity: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueueState {
    #[serde(default)]
    pub queue_key: String,
    #[serde(default)]
    pub last_image_path: String,
    #[serde(default)]
    pub next_image_path: String,
    #[serde(default)]
    pub next_index: i32,
    #[serde(default)]
    pub rotation_mode: RotationMode,
    #[serde(default)]
    pub ordered_image_paths: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImageCacheEntry {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub length: i64,
    #[serde(default)]
    pub last_write_utc: String,
    #[serde(default)]
    pub width: i32,
    #[serde(default)]
    pub height: i32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImageShowStat {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub shown_count: i64,
}

/// The Wall Changer's configuration plus its runtime state.
///
/// Only the configuration half is (de)serialized to and from `settings.json` and the UI. The
/// fields marked `skip_serializing` are runtime state owned by the wallpaper service; they live
/// in `state.json` (see [`RuntimeState`]) so that saving settings from the UI can never roll back
/// queue positions, statistics or the image cache. They are still deserialized so that old
/// combined `settings.json` files can be migrated.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub source_folders: Vec<Source>,
    #[serde(default)]
    pub use_random_source_order: bool,
    #[serde(default = "default_interval")]
    pub interval_minutes: i32,
    #[serde(default)]
    pub rotation_mode: RotationMode,
    #[serde(default = "default_true")]
    pub include_subfolders: bool,
    #[serde(default = "default_max_level")]
    pub maximum_source_level: i32,
    #[serde(default = "default_true")]
    pub use_separate_monitor_queues: bool,
    #[serde(default)]
    pub keep_image_in_single_monitor_queue: bool,
    #[serde(default)]
    pub too_small_picture_handling: PictureSizeHandling,
    #[serde(default)]
    pub too_large_picture_handling: PictureSizeHandling,
    #[serde(default)]
    pub scaling_mode: ScalingMode,
    #[serde(default = "default_black")]
    pub background_color_argb: i32,
    #[serde(default)]
    pub show_picture_name_overlay: bool,
    #[serde(default)]
    pub picture_name_overlay_text_mode: PictureNameOverlayTextMode,
    #[serde(default = "default_font_size")]
    pub picture_name_overlay_font_size: i32,
    #[serde(default = "default_white")]
    pub picture_name_overlay_text_color_argb: i32,
    #[serde(default)]
    pub picture_name_overlay_offset_x: i32,
    #[serde(default)]
    pub picture_name_overlay_offset_y: i32,
    #[serde(default = "default_true")]
    pub picture_name_overlay_use_backdrop: bool,
    #[serde(default = "default_true")]
    pub disable_windows_slideshow_when_running: bool,
    #[serde(default, skip_serializing)]
    pub change_service_running: bool,
    #[serde(default)]
    pub change_on_start: bool,
    #[serde(default)]
    pub change_one_monitor_per_interval: bool,
    #[serde(default, skip_serializing)]
    pub next_monitor_change_index: i32,
    #[serde(default = "default_true")]
    pub enable_wallpaper_fade_transition: bool,
    #[serde(default = "default_fade_duration")]
    pub wallpaper_fade_duration_ms: i32,
    #[serde(default = "default_fade_steps")]
    pub wallpaper_fade_steps: i32,
    #[serde(default = "default_true")]
    pub show_wallpaper_change_notifications: bool,
    #[serde(default)]
    pub use_wallhaven_api_key: bool,
    #[serde(default)]
    pub wallhaven_api_key: String,
    #[serde(default)]
    pub sequence_offset: i32,
    #[serde(default, skip_serializing)]
    pub queue_states: Vec<QueueState>,
    #[serde(default, skip_serializing)]
    pub image_cache_entries: Vec<ImageCacheEntry>,
    #[serde(default, skip_serializing)]
    pub image_show_stats: Vec<ImageShowStat>,
}

fn default_true() -> bool { true }
fn default_level() -> i32 { 5 }
fn default_interval() -> i32 { 30 }
fn default_max_level() -> i32 { 10 }
fn default_wallhaven_page_limit() -> i32 { 1 }

fn normalize_purity(purity: &str) -> String {
    let s = purity.trim();
    if s.len() == 3 && s.chars().all(|c| c == '0' || c == '1') {
        if s == "000" { "100".to_string() } else { s.to_string() }
    } else if s.is_empty() {
        "110".to_string()
    } else {
        "110".to_string()
    }
}
fn default_black() -> i32 { 0xFF000000u32 as i32 }
fn default_white() -> i32 { 0xFFFFFFFFu32 as i32 }
fn default_font_size() -> i32 { 28 }
fn default_fade_duration() -> i32 { 600 }
fn default_fade_steps() -> i32 { 12 }

/// Runtime state persisted in `state.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeState {
    #[serde(default)]
    pub change_service_running: bool,
    #[serde(default)]
    pub next_monitor_change_index: i32,
    #[serde(default)]
    pub queue_states: Vec<QueueState>,
    #[serde(default)]
    pub image_cache_entries: Vec<ImageCacheEntry>,
    #[serde(default)]
    pub image_show_stats: Vec<ImageShowStat>,
}

impl RuntimeState {
    fn from_settings(settings: &Settings) -> Self {
        Self {
            change_service_running: settings.change_service_running,
            next_monitor_change_index: settings.next_monitor_change_index,
            queue_states: settings.queue_states.clone(),
            image_cache_entries: settings.image_cache_entries.clone(),
            image_show_stats: settings.image_show_stats.clone(),
        }
    }

    fn apply_to(self, settings: &mut Settings) {
        settings.change_service_running = self.change_service_running;
        settings.next_monitor_change_index = self.next_monitor_change_index;
        settings.queue_states = self.queue_states;
        settings.image_cache_entries = self.image_cache_entries;
        settings.image_show_stats = self.image_show_stats;
    }
}

/// Serializes read-modify-write cycles on `settings.json`.
static CONFIG_LOCK: Mutex<()> = Mutex::new(());
/// Serializes read-modify-write cycles on `state.json`.
static STATE_LOCK: Mutex<()> = Mutex::new(());

pub fn config_dir() -> Result<PathBuf, String> {
    dirs::config_dir()
        .map(|p| p.join("mula").join("wallchanger"))
        .ok_or_else(|| "Could not determine config directory".to_string())
}

pub fn settings_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("settings.json"))
}

pub fn state_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("state.json"))
}

/// Writes via a temporary file and a rename, so a crash mid-write never leaves a truncated file.
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create wallchanger config directory: {e}"))?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, text).map_err(|e| format!("Failed to write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("Failed to replace {}: {e}", path.display()))
}

fn lock(m: &'static Mutex<()>) -> MutexGuard<'static, ()> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn read_config_file() -> Result<Settings, String> {
    let path = settings_path()?;
    // Deserialize from "{}" rather than using `Settings::default()` directly, so that
    // the per-field `#[serde(default = "...")]` values (e.g. font size, colors, interval)
    // are applied instead of the derived `Default` impl's zeroed-out values.
    let text = if path.exists() {
        fs::read_to_string(&path).map_err(|e| format!("Failed to read wallchanger settings: {e}"))?
    } else {
        "{}".to_string()
    };

    serde_json::from_str(&text).map_err(|e| format!("Failed to parse wallchanger settings: {e}"))
}

fn read_state_file() -> Result<Option<RuntimeState>, String> {
    let path = state_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(|e| format!("Failed to read wallchanger state: {e}"))?;
    match serde_json::from_str(&text) {
        Ok(state) => Ok(Some(state)),
        Err(e) => {
            // Runtime state is rebuildable, so a damaged file must not take the app down.
            log::warn!("Ignoring unreadable wallchanger state ({e}); starting with fresh state");
            Ok(Some(RuntimeState::default()))
        }
    }
}

fn write_state_file(state: &RuntimeState) -> Result<(), String> {
    let text = serde_json::to_string(state)
        .map_err(|e| format!("Failed to serialize wallchanger state: {e}"))?;
    write_atomic(&state_path()?, &text)
}

/// Loads only the configuration (`settings.json`); runtime-state fields are left empty.
pub fn load_config() -> Result<Settings, String> {
    let mut settings = read_config_file()?;
    RuntimeState::default().apply_to(&mut settings);
    normalize(&mut settings);
    Ok(settings)
}

/// Loads the configuration together with the runtime state.
pub fn load() -> Result<Settings, String> {
    let mut settings = read_config_file()?;

    let state = {
        let _guard = lock(&STATE_LOCK);
        match read_state_file()? {
            Some(state) => state,
            None => {
                // First run after the split: move the state out of the old combined file.
                let migrated = RuntimeState::from_settings(&settings);
                write_state_file(&migrated)?;
                let _config_guard = lock(&CONFIG_LOCK);
                save_config_unlocked(&settings)?;
                log::info!("Migrated wallchanger runtime state to state.json");
                migrated
            }
        }
    };
    state.apply_to(&mut settings);

    normalize(&mut settings);
    Ok(settings)
}

fn save_config_unlocked(settings: &Settings) -> Result<(), String> {
    let text = serde_json::to_string_pretty(settings)
        .map_err(|e| format!("Failed to serialize wallchanger settings: {e}"))?;
    write_atomic(&settings_path()?, &text)
}

/// Saves the configuration half of `settings`. Runtime state is never written here.
pub fn save_config(settings: &Settings) -> Result<(), String> {
    let _guard = lock(&CONFIG_LOCK);
    save_config_unlocked(settings)
}

/// Loads the configuration, lets `f` change it, and saves it, without racing other writers.
pub fn update_config(f: impl FnOnce(&mut Settings)) -> Result<Settings, String> {
    let _guard = lock(&CONFIG_LOCK);
    let mut settings = load_config()?;
    f(&mut settings);
    normalize(&mut settings);
    save_config_unlocked(&settings)?;
    Ok(settings)
}

/// Saves the state a wallpaper change produces (queues, cache, statistics, next monitor).
/// The service on/off flag is owned by [`set_service_running`] and is preserved as-is.
pub fn save_apply_state(settings: &Settings) -> Result<(), String> {
    let _guard = lock(&STATE_LOCK);
    let mut state = RuntimeState::from_settings(settings);
    state.change_service_running = read_state_file()?
        .map(|s| s.change_service_running)
        .unwrap_or(settings.change_service_running);
    write_state_file(&state)
}

pub fn service_running() -> Result<bool, String> {
    Ok(load()?.change_service_running)
}

pub fn set_service_running(running: bool) -> Result<(), String> {
    let _guard = lock(&STATE_LOCK);
    let mut state = match read_state_file()? {
        Some(state) => state,
        None => RuntimeState::from_settings(&read_config_file()?),
    };
    state.change_service_running = running;
    write_state_file(&state)
}

pub fn normalize(settings: &mut Settings) {
    settings.interval_minutes = settings.interval_minutes.clamp(1, 1440);
    settings.maximum_source_level = settings.maximum_source_level.clamp(1, 10);

    settings.wallhaven_api_key = settings.wallhaven_api_key.trim().to_string();
    if settings.wallhaven_api_key.is_empty() {
        settings.use_wallhaven_api_key = false;
    }

    for source in &mut settings.source_folders {
        source.path = source.path.trim().to_string();
        source.level = source.level.clamp(1, 10);
        source.wallhaven_page_limit = source.wallhaven_page_limit.max(0);
        source.wallhaven_purity = normalize_purity(&source.wallhaven_purity);
    }

    settings.source_folders.retain(|s| !s.path.is_empty());

    if settings.picture_name_overlay_font_size < 1 {
        settings.picture_name_overlay_font_size = 1;
    }
    if settings.wallpaper_fade_duration_ms < 0 {
        settings.wallpaper_fade_duration_ms = 0;
    }
    if settings.wallpaper_fade_steps < 1 {
        settings.wallpaper_fade_steps = 1;
    }
    if settings.sequence_offset < 0 {
        settings.sequence_offset = 0;
    }
    if settings.next_monitor_change_index < 0 {
        settings.next_monitor_change_index = 0;
    }

    // Dedupe and prune queue state paths.
    for state in &mut settings.queue_states {
        state.next_index = state.next_index.max(0);
        state.ordered_image_paths.retain(|p| !p.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_state_is_not_serialized_with_the_configuration() {
        let mut settings: Settings = serde_json::from_str("{}").unwrap();
        settings.change_service_running = true;
        settings.next_monitor_change_index = 3;
        settings.queue_states.push(QueueState { queue_key: "shared".into(), next_index: 7, ..Default::default() });
        settings.image_show_stats.push(ImageShowStat { path: "a.jpg".into(), shown_count: 2 });

        let json = serde_json::to_value(&settings).unwrap();
        for key in ["change_service_running", "next_monitor_change_index", "queue_states", "image_cache_entries", "image_show_stats"] {
            assert!(json.get(key).is_none(), "{key} must not be part of the saved configuration");
        }
        assert!(json.get("interval_minutes").is_some());
    }

    #[test]
    fn old_combined_settings_still_yield_their_state_for_migration() {
        let old = r#"{"interval_minutes": 15, "change_service_running": true,
                      "queue_states": [{"queue_key": "shared", "next_index": 4}],
                      "image_show_stats": [{"path": "a.jpg", "shown_count": 9}]}"#;
        let settings: Settings = serde_json::from_str(old).unwrap();
        let state = RuntimeState::from_settings(&settings);
        assert!(state.change_service_running);
        assert_eq!(state.queue_states[0].next_index, 4);
        assert_eq!(state.image_show_stats[0].shown_count, 9);
        assert_eq!(settings.interval_minutes, 15);
    }

    #[test]
    fn a_ui_save_cannot_touch_runtime_state() {
        // What the frontend sends back: configuration only, state fields absent.
        let from_ui: Settings = serde_json::from_str(r#"{"interval_minutes": 5}"#).unwrap();
        let mut combined: Settings = serde_json::from_str("{}").unwrap();
        RuntimeState {
            queue_states: vec![QueueState { queue_key: "shared".into(), next_index: 12, ..Default::default() }],
            ..Default::default()
        }
        .apply_to(&mut combined);

        // Saving `from_ui` only ever produces configuration JSON.
        let saved: serde_json::Value = serde_json::to_value(&from_ui).unwrap();
        assert!(saved.get("queue_states").is_none());
        assert_eq!(combined.queue_states[0].next_index, 12);
    }
}
