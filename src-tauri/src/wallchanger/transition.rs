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

    let from = load_and_cover(&previous_path, width, height)?;
    let to = load_and_cover(new_wallpaper_path, width, height)?;

    let steps = settings.wallpaper_fade_steps.max(1);
    let total_duration = Duration::from_millis(settings.wallpaper_fade_duration_ms.max(0) as u64);
    let step_duration = total_duration / steps as u32;

    let dir = transition_dir()?;
    let monitor_key = sanitize_for_filename(&monitor.id);

    for step in 1..=steps {
        let alpha = step as f32 / steps as f32;
        let frame = blend(&from, &to, alpha);

        let frame_path = dir.join(format!("{monitor_key}_{step}.jpg"));
        frame
            .save(&frame_path)
            .map_err(|e| format!("Failed to save transition frame: {e}"))?;

        set_wallpaper_for_monitor(&monitor.id, &frame_path.to_string_lossy())?;

        if step < steps {
            std::thread::sleep(step_duration);
        }
    }

    // Finish on the real (unstretched) image so Windows applies the configured
    // Fill/Fit scaling mode normally, rather than leaving the last stretched frame set.
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

/// Loads an image and resizes it to exactly `width` x `height`, cropping to cover the
/// target area (so the transition frames line up with the final Fill/Fit result).
fn load_and_cover(path: &str, width: u32, height: u32) -> Result<RgbImage, String> {
    let img = image::open(path)
        .map_err(|e| format!("Failed to open image for transition ({path}): {e}"))?
        .to_rgb8();

    Ok(image::imageops::resize(&img, width, height, FilterType::Triangle))
}

fn blend(from: &RgbImage, to: &RgbImage, alpha: f32) -> RgbImage {
    let (width, height) = from.dimensions();
    let mut out = RgbImage::new(width, height);

    for y in 0..height {
        for x in 0..width {
            let a = from.get_pixel(x, y).0;
            let b = to.get_pixel(x, y).0;
            let mixed = [
                lerp(a[0], b[0], alpha),
                lerp(a[1], b[1], alpha),
                lerp(a[2], b[2], alpha),
            ];
            out.put_pixel(x, y, image::Rgb(mixed));
        }
    }

    out
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
