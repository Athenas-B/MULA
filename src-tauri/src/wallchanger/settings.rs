use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

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

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
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
    #[serde(default)]
    pub change_service_running: bool,
    #[serde(default)]
    pub change_on_start: bool,
    #[serde(default)]
    pub change_one_monitor_per_interval: bool,
    #[serde(default)]
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
    #[serde(default)]
    pub queue_states: Vec<QueueState>,
    #[serde(default)]
    pub image_cache_entries: Vec<ImageCacheEntry>,
    #[serde(default)]
    pub image_show_stats: Vec<ImageShowStat>,
}

fn default_true() -> bool { true }
fn default_level() -> i32 { 5 }
fn default_interval() -> i32 { 30 }
fn default_max_level() -> i32 { 10 }
fn default_wallhaven_page_limit() -> i32 { 1 }
fn default_black() -> i32 { 0xFF000000u32 as i32 }
fn default_white() -> i32 { 0xFFFFFFFFu32 as i32 }
fn default_font_size() -> i32 { 28 }
fn default_fade_duration() -> i32 { 600 }
fn default_fade_steps() -> i32 { 12 }

pub fn config_dir() -> Result<PathBuf, String> {
    dirs::config_dir()
        .map(|p| p.join("mula").join("wallchanger"))
        .ok_or_else(|| "Could not determine config directory".to_string())
}

pub fn settings_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("settings.json"))
}

pub fn load() -> Result<Settings, String> {
    let path = settings_path()?;
    if !path.exists() {
        return Ok(Settings::default());
    }

    let text = fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read wallchanger settings: {e}"))?;
    let mut settings: Settings = serde_json::from_str(&text)
        .map_err(|e| format!("Failed to parse wallchanger settings: {e}"))?;
    normalize(&mut settings);
    Ok(settings)
}

pub fn save(settings: &Settings) -> Result<(), String> {
    let path = settings_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create wallchanger config directory: {e}"))?;
    }

    let text = serde_json::to_string_pretty(settings)
        .map_err(|e| format!("Failed to serialize wallchanger settings: {e}"))?;
    fs::write(&path, text)
        .map_err(|e| format!("Failed to write wallchanger settings: {e}"))?;
    Ok(())
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
        source.wallhaven_page_limit = source.wallhaven_page_limit.max(1);
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
