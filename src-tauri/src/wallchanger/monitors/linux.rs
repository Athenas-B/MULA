pub use super::Monitor;
use super::settings::ScalingMode;

pub fn initialize_com() -> Result<(), String> {
    Ok(())
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
