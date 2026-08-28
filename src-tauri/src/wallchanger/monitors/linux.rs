use serde::{Deserialize, Serialize};
use super::settings::ScalingMode;

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
        if self.height == 0 {
            return 1.0;
        }
        self.width as f64 / self.height as f64
    }
}

pub fn initialize_com() -> Result<(), String> {
    Ok(())
}

pub fn create_desktop_wallpaper() -> Result<(), String> {
    Err("Desktop wallpaper API is only available on Windows".to_string())
}

pub fn get_monitors() -> Result<Vec<Monitor>, String> {
    Ok(vec![])
}

pub fn set_wallpaper_for_monitor(_monitor_id: &str, _image_path: &str) -> Result<(), String> {
    Err("Setting wallpapers is only supported on Windows".to_string())
}

pub fn apply_display_settings(
    _scaling_mode: &ScalingMode,
    _background_color_argb: i32,
) -> Result<(), String> {
    Ok(())
}

pub fn is_windows_slideshow_enabled() -> Result<bool, String> {
    Ok(false)
}

pub fn get_current_wallpaper(_monitor_id: &str) -> Result<String, String> {
    Err("Getting current wallpaper is only supported on Windows".to_string())
}

pub fn try_disable_windows_slideshow() -> Result<(), String> {
    Ok(())
}
