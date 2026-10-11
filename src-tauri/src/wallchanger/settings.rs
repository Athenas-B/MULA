//! Wall Changer configuration (`settings.json`, edited by the user) and runtime state
//! (`state.json`, written only by the wallpaper service). They are separate types and files, so
//! saving settings can never roll back queue positions, statistics or the image cache.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::util::{JsonFormat, JsonStore};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum RotationMode {
    #[default]
    Random,
    Sequence,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum PictureSizeHandling {
    #[default]
    Allow,
    Avoid,
    UseOnlyAsLastResort,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
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

/// Which Wallhaven content ratings a source asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Purity {
    #[serde(default)]
    pub sfw: bool,
    #[serde(default)]
    pub sketchy: bool,
    #[serde(default)]
    pub nsfw: bool,
}

impl Default for Purity {
    fn default() -> Self {
        Self { sfw: true, sketchy: true, nsfw: false }
    }
}

impl Purity {
    /// Wallhaven's `purity` query parameter, e.g. `"110"`.
    pub fn as_param(&self) -> String {
        [self.sfw, self.sketchy, self.nsfw].iter().map(|on| if *on { '1' } else { '0' }).collect()
    }

    /// At least one rating must be selected; fall back to SFW.
    fn normalized(self) -> Self {
        if self.sfw || self.sketchy || self.nsfw {
            self
        } else {
            Self { sfw: true, ..self }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceKind {
    /// A local folder of images.
    Folder {
        #[serde(default = "default_true")]
        include_subfolders: bool,
    },
    /// A Wallhaven search page or `/api/v1/search` URL.
    Wallhaven {
        /// Result pages to fetch; 0 means all of them.
        #[serde(default = "default_wallhaven_page_limit")]
        page_limit: i32,
        #[serde(default)]
        purity: Purity,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    #[serde(default)]
    pub path: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_level")]
    pub level: i32,
    #[serde(flatten)]
    pub kind: SourceKind,
}

#[cfg(test)]
impl Source {
    pub fn folder(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            enabled: true,
            level: default_level(),
            kind: SourceKind::Folder { include_subfolders: true },
        }
    }

    pub fn wallhaven(url: impl Into<String>) -> Self {
        Self {
            path: url.into(),
            enabled: true,
            level: default_level(),
            kind: SourceKind::Wallhaven { page_limit: default_wallhaven_page_limit(), purity: Purity::default() },
        }
    }
}

/// User-editable configuration, stored in `settings.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub source_folders: Vec<Source>,
    #[serde(default)]
    pub use_random_source_order: bool,
    #[serde(default = "default_interval")]
    pub interval_minutes: i32,
    #[serde(default)]
    pub rotation_mode: RotationMode,
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
    pub change_on_start: bool,
    #[serde(default)]
    pub change_one_monitor_per_interval: bool,
    #[serde(default = "default_true")]
    pub enable_wallpaper_fade_transition: bool,
    #[serde(default = "default_fade_duration")]
    pub wallpaper_fade_duration_ms: i32,
    #[serde(default = "default_fade_steps")]
    pub wallpaper_fade_steps: i32,
    #[serde(default = "default_true")]
    pub show_wallpaper_change_notifications: bool,
    /// Sent to Wallhaven when non-empty (required for NSFW results).
    #[serde(default)]
    pub wallhaven_api_key: String,
}

impl Default for Settings {
    fn default() -> Self {
        // Deserializing "{}" applies every per-field `#[serde(default = "...")]`.
        serde_json::from_str("{}").expect("all settings fields have defaults")
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueueState {
    #[serde(default)]
    pub queue_key: String,
    #[serde(default)]
    pub last_image_path: String,
    #[serde(default)]
    pub next_index: i32,
    #[serde(default)]
    pub ordered_image_paths: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImageCacheEntry {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub length: i64,
    /// Last modification time, in seconds since the Unix epoch.
    #[serde(default)]
    pub modified_secs: i64,
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

/// State the wallpaper service maintains, stored in `state.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeState {
    #[serde(default)]
    pub next_monitor_change_index: i32,
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

static SETTINGS: JsonStore<Settings> = JsonStore::new("wallchanger/settings.json", JsonFormat::Pretty, normalize);
/// Compact: it can hold thousands of paths.
static STATE: JsonStore<RuntimeState> = JsonStore::new("wallchanger/state.json", JsonFormat::Compact, keep::<RuntimeState>);
static SERVICE: JsonStore<ServiceFlag> = JsonStore::new("wallchanger/service.json", JsonFormat::Pretty, keep::<ServiceFlag>);

fn keep<T>(_: &mut T) {}

/// Bumped on every settings change (while the store is locked), so the UI can ignore responses
/// and events that are older than what it already shows.
static REVISION: AtomicU64 = AtomicU64::new(0);

/// Settings together with the revision they belong to.
#[derive(Debug, Clone, Serialize)]
pub struct Versioned {
    pub revision: u64,
    pub settings: Settings,
}

pub fn config_dir() -> Result<PathBuf, String> {
    crate::util::mula_path("wallchanger")
}

pub fn load() -> Result<Settings, String> {
    SETTINGS.get()
}

/// The current settings and their revision.
pub fn load_versioned() -> Result<Versioned, String> {
    SETTINGS.read(|settings| Versioned { revision: REVISION.load(Ordering::SeqCst), settings: settings.clone() })
}

/// Changes the settings through `f`, normalizes and saves them, without racing other writers.
pub fn update(f: impl FnOnce(&mut Settings)) -> Result<Versioned, String> {
    SETTINGS.update(|settings| {
        f(settings);
        normalize(settings);
        Ok(versioned(settings))
    })
}

/// Applies a partial update: each top-level key of `patch` replaces that setting.
pub fn update_with_patch(patch: serde_json::Value) -> Result<Versioned, String> {
    SETTINGS.update(|settings| {
        *settings = apply_patch(settings, patch)?;
        Ok(versioned(settings))
    })
}

fn versioned(settings: &Settings) -> Versioned {
    Versioned { revision: REVISION.fetch_add(1, Ordering::SeqCst) + 1, settings: settings.clone() }
}

fn apply_patch(settings: &Settings, patch: serde_json::Value) -> Result<Settings, String> {
    let serde_json::Value::Object(patch) = patch else {
        return Err("A settings update must be a JSON object".to_string());
    };
    let mut value = serde_json::to_value(settings).map_err(|e| e.to_string())?;
    let fields = value.as_object_mut().expect("settings serialize to an object");
    for (key, field) in patch {
        if !fields.contains_key(&key) {
            return Err(format!("Unknown setting: {key}"));
        }
        fields.insert(key, field);
    }
    let mut next: Settings = serde_json::from_value(value).map_err(|e| format!("Invalid settings: {e}"))?;
    normalize(&mut next);
    Ok(next)
}

pub fn load_state() -> Result<RuntimeState, String> {
    STATE.get()
}

/// Stores the state a wallpaper change produced.
pub fn save_state(state: RuntimeState) -> Result<(), String> {
    STATE.set(state)
}

/// Whether the interval service is switched on; stored on its own in `service.json`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct ServiceFlag {
    #[serde(default)]
    running: bool,
}

pub fn service_running() -> bool {
    SERVICE.read(|flag| flag.running).unwrap_or(false)
}

pub fn set_service_running(running: bool) -> Result<(), String> {
    SERVICE.set(ServiceFlag { running })
}

pub fn normalize(settings: &mut Settings) {
    settings.interval_minutes = settings.interval_minutes.clamp(1, 1440);
    settings.maximum_source_level = settings.maximum_source_level.clamp(1, 10);
    settings.wallhaven_api_key = settings.wallhaven_api_key.trim().to_string();

    for source in &mut settings.source_folders {
        source.path = source.path.trim().to_string();
        source.level = source.level.clamp(1, 10);
        if let SourceKind::Wallhaven { page_limit, purity } = &mut source.kind {
            *page_limit = (*page_limit).max(0);
            *purity = purity.normalized();
        }
    }
    settings.source_folders.retain(|s| !s.path.is_empty());

    settings.picture_name_overlay_font_size = settings.picture_name_overlay_font_size.max(1);
    settings.wallpaper_fade_duration_ms = settings.wallpaper_fade_duration_ms.max(0);
    settings.wallpaper_fade_steps = settings.wallpaper_fade_steps.max(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_documented_values() {
        let s = Settings::default();
        assert_eq!(s.interval_minutes, 30);
        assert_eq!(s.maximum_source_level, 10);
        assert_eq!(s.picture_name_overlay_font_size, 28);
        assert_eq!(s.wallpaper_fade_duration_ms, 600);
        assert_eq!(s.wallpaper_fade_steps, 12);
        assert_eq!(s.background_color_argb as u32, 0xFF00_0000);
        assert_eq!(s.picture_name_overlay_text_color_argb as u32, 0xFFFF_FFFF);
        assert!(s.use_separate_monitor_queues && s.enable_wallpaper_fade_transition);
        assert!(s.disable_windows_slideshow_when_running);
        assert_eq!(s.rotation_mode, RotationMode::Random);
        assert_eq!(s.scaling_mode, ScalingMode::Fill);
        assert!(s.source_folders.is_empty());
    }

    #[test]
    fn sources_are_tagged_by_kind() {
        let folder = serde_json::to_value(Source::folder("C:/Pics")).unwrap();
        assert_eq!(folder["kind"], "folder");
        assert_eq!(folder["include_subfolders"], true);
        assert_eq!(folder["level"], 5);

        let wallhaven = serde_json::to_value(Source::wallhaven("https://wallhaven.cc/search?q=x")).unwrap();
        assert_eq!(wallhaven["kind"], "wallhaven");
        assert_eq!(wallhaven["page_limit"], 1);
        assert_eq!(wallhaven["purity"], serde_json::json!({ "sfw": true, "sketchy": true, "nsfw": false }));

        let parsed: Source = serde_json::from_value(serde_json::json!({ "path": "x", "kind": "wallhaven" })).unwrap();
        assert_eq!(parsed.kind, SourceKind::Wallhaven { page_limit: 1, purity: Purity::default() });
        assert!(serde_json::from_value::<Source>(serde_json::json!({ "path": "x", "kind": "ftp" })).is_err());
    }

    #[test]
    fn purity_maps_to_the_wallhaven_parameter() {
        assert_eq!(Purity::default().as_param(), "110");
        assert_eq!(Purity { sfw: false, sketchy: false, nsfw: true }.as_param(), "001");
        assert_eq!(Purity { sfw: false, sketchy: false, nsfw: false }.normalized().as_param(), "100");
    }

    #[test]
    fn enums_use_the_pascal_case_names_the_ui_sends() {
        let s: Settings = serde_json::from_str(r#"{"rotation_mode": "Sequence", "scaling_mode": "FitInsideScreen"}"#).unwrap();
        assert_eq!(s.rotation_mode, RotationMode::Sequence);
        assert_eq!(serde_json::to_value(&s).unwrap()["scaling_mode"], "FitInsideScreen");
        assert!(serde_json::from_str::<Settings>(r#"{"rotation_mode": "sequence"}"#).is_err());
    }

    #[test]
    fn numeric_settings_are_clamped_into_range() {
        let mut s = Settings {
            interval_minutes: 0,
            maximum_source_level: 42,
            picture_name_overlay_font_size: -3,
            wallpaper_fade_duration_ms: -1,
            wallpaper_fade_steps: 0,
            ..Settings::default()
        };
        normalize(&mut s);
        assert_eq!(
            (s.interval_minutes, s.maximum_source_level, s.picture_name_overlay_font_size),
            (1, 10, 1)
        );
        assert_eq!((s.wallpaper_fade_duration_ms, s.wallpaper_fade_steps), (0, 1));

        s.interval_minutes = 99_999;
        s.maximum_source_level = 0;
        normalize(&mut s);
        assert_eq!((s.interval_minutes, s.maximum_source_level), (1440, 1));
    }

    #[test]
    fn sources_are_trimmed_clamped_and_empty_ones_dropped() {
        let mut url = Source::wallhaven("  https://wallhaven.cc/search?q=x  ");
        url.level = 99;
        url.kind = SourceKind::Wallhaven { page_limit: -4, purity: Purity { sfw: false, sketchy: false, nsfw: false } };
        let mut s = Settings { source_folders: vec![url, Source::folder("   ")], ..Settings::default() };
        normalize(&mut s);

        assert_eq!(s.source_folders.len(), 1);
        let source = &s.source_folders[0];
        assert_eq!((source.path.as_str(), source.level), ("https://wallhaven.cc/search?q=x", 10));
        assert_eq!(source.kind, SourceKind::Wallhaven { page_limit: 0, purity: Purity { sfw: true, sketchy: false, nsfw: false } });
    }

    #[test]
    fn the_api_key_is_trimmed() {
        let mut s = Settings { wallhaven_api_key: "  KEY  ".into(), ..Settings::default() };
        normalize(&mut s);
        assert_eq!(s.wallhaven_api_key, "KEY");
    }

    #[test]
    fn patches_replace_only_the_given_settings_and_are_normalized() {
        let base = Settings { interval_minutes: 15, ..Settings::default() };
        let next = apply_patch(&base, serde_json::json!({ "maximum_source_level": 99, "rotation_mode": "Sequence" })).unwrap();
        assert_eq!(next.interval_minutes, 15, "untouched settings keep their value");
        assert_eq!(next.maximum_source_level, 10, "patched values are normalized");
        assert_eq!(next.rotation_mode, RotationMode::Sequence);

        let sources = apply_patch(&base, serde_json::json!({ "source_folders": [{ "path": "C:/x", "kind": "folder" }] })).unwrap();
        assert_eq!(sources.source_folders, vec![Source::folder("C:/x")]);
    }

    #[test]
    fn invalid_patches_are_rejected() {
        let base = Settings::default();
        assert!(apply_patch(&base, serde_json::json!({ "no_such_setting": 1 })).unwrap_err().contains("Unknown setting"));
        assert!(apply_patch(&base, serde_json::json!({ "interval_minutes": "soon" })).is_err());
        assert!(apply_patch(&base, serde_json::json!([1, 2])).is_err());
    }

    #[test]
    fn the_service_flag_defaults_to_off() {
        let flag: ServiceFlag = serde_json::from_str("{}").unwrap();
        assert!(!flag.running);
        assert_eq!(serde_json::to_value(ServiceFlag { running: true }).unwrap(), serde_json::json!({ "running": true }));
    }
}
