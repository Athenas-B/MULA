//! Crossfades between the previous and next wallpaper for a monitor by rendering a
//! short sequence of blended frames and stepping through them, since Windows has no
//! built-in transition API for `IDesktopWallpaper::SetWallpaper`. Mirrors the original
//! Wall Changer's fade transition feature.

use super::monitors::{get_current_wallpaper, set_wallpaper_for_monitor, Monitor};
use super::settings::Settings;
use image::{imageops::FilterType, RgbImage};
use std::path::PathBuf;
use std::time::Duration;

fn transition_dir() -> Result<PathBuf, String> {
    let dir = super::settings::config_dir()?.join("rendered").join("transitions");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create transition frame directory: {e}"))?;
    Ok(dir)
}

/// Sets `new_wallpaper_path` on the given monitor, crossfading from whatever is
/// currently displayed if fade transitions are enabled and a previous wallpaper can be
/// found; otherwise falls back to setting it directly.
pub fn apply_wallpaper(monitor: &Monitor, new_wallpaper_path: &str, settings: &Settings) -> Result<(), String> {
    if !settings.enable_wallpaper_fade_transition {
        return set_wallpaper_for_monitor(&monitor.id, new_wallpaper_path);
    }

    match run_fade(monitor, new_wallpaper_path, settings) {
        Ok(()) => Ok(()),
        Err(e) => {
            log::warn!("Wallpaper fade transition failed, applying directly: {e}");
            set_wallpaper_for_monitor(&monitor.id, new_wallpaper_path)
        }
    }
}

fn run_fade(monitor: &Monitor, new_wallpaper_path: &str, settings: &Settings) -> Result<(), String> {
    let previous_path = get_current_wallpaper(&monitor.id)?;
    if previous_path.is_empty() || previous_path.eq_ignore_ascii_case(new_wallpaper_path) {
        return set_wallpaper_for_monitor(&monitor.id, new_wallpaper_path);
    }

    let width = monitor.width.max(1) as u32;
    let height = monitor.height.max(1) as u32;

    let from = load_fitted(&previous_path, width, height, settings)?;
    let to = load_fitted(new_wallpaper_path, width, height, settings)?;

    let steps = settings.wallpaper_fade_steps.max(1);
    let total_duration = Duration::from_millis(settings.wallpaper_fade_duration_ms.max(0) as u64);
    let step_duration = total_duration / steps as u32;

    let dir = transition_dir()?;
    let monitor_key = sanitize_for_filename(&monitor.id);

    // Render every frame before showing any, so encoding time doesn't stretch the fade.
    let mut frames = Vec::with_capacity(steps as usize);
    for step in 1..=steps {
        let frame_path = dir.join(format!("{monitor_key}_{step}.jpg"));
        blend(&from, &to, step as f32 / steps as f32)
            .save(&frame_path)
            .map_err(|e| format!("Failed to save transition frame: {e}"))?;
        frames.push(frame_path);
    }

    for (i, frame_path) in frames.iter().enumerate() {
        if i > 0 {
            std::thread::sleep(step_duration);
        }
        set_wallpaper_for_monitor(&monitor.id, &frame_path.to_string_lossy())?;
    }

    // Finish on the real image so Windows applies the configured Fill/Fit mode itself
    // (and keeps doing so if the resolution changes later).
    set_wallpaper_for_monitor(&monitor.id, new_wallpaper_path)?;

    let _ = std::fs::remove_dir_all(&dir);

    Ok(())
}

fn sanitize_for_filename(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Loads an image and renders it at the monitor's size using the configured Fill/Fit mode, so
/// the transition frames line up with what Windows shows once the real image is applied.
fn load_fitted(path: &str, width: u32, height: u32, settings: &Settings) -> Result<RgbImage, String> {
    let img = image::open(path)
        .map_err(|e| format!("Failed to open image for transition ({path}): {e}"))?
        .to_rgba8();

    let background = super::preview::argb_to_rgba(settings.background_color_argb);
    let fitted = super::preview::fit_to_canvas(&img, width, height, &settings.scaling_mode, background, FilterType::Triangle);
    Ok(image::DynamicImage::ImageRgba8(fitted).to_rgb8())
}

fn blend(from: &RgbImage, to: &RgbImage, alpha: f32) -> RgbImage {
    let (width, height) = from.dimensions();
    let mixed = from
        .as_raw()
        .iter()
        .zip(to.as_raw())
        .map(|(&a, &b)| lerp(a, b, alpha))
        .collect();
    RgbImage::from_raw(width, height, mixed).expect("both frames have the monitor's size")
}

fn lerp(a: u8, b: u8, alpha: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * alpha).round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lerp_interpolates_between_endpoints() {
        assert_eq!(lerp(0, 100, 0.0), 0);
        assert_eq!(lerp(0, 100, 1.0), 100);
        assert_eq!(lerp(0, 100, 0.5), 50);
    }

    #[test]
    fn blend_mixes_two_images_by_alpha() {
        let from = RgbImage::from_pixel(4, 4, image::Rgb([0, 0, 0]));
        let to = RgbImage::from_pixel(4, 4, image::Rgb([200, 200, 200]));

        let half = blend(&from, &to, 0.5);
        assert_eq!(half.get_pixel(0, 0).0, [100, 100, 100]);

        let start = blend(&from, &to, 0.0);
        assert_eq!(start.get_pixel(0, 0).0, [0, 0, 0]);

        let end = blend(&from, &to, 1.0);
        assert_eq!(end.get_pixel(0, 0).0, [200, 200, 200]);
    }
}

#[cfg(test)]
mod name_tests {
    use super::*;

    #[test]
    fn monitor_ids_become_safe_file_name_parts() {
        assert_eq!(sanitize_for_filename(r"\\?\DISPLAY#GSM5B7F#5&1a2b"), "____DISPLAY_GSM5B7F_5_1a2b");
        assert_eq!(sanitize_for_filename("abc123"), "abc123");
    }
}
