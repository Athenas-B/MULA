//! Crossfades between the previous and next wallpaper of each monitor by rendering a short
//! sequence of blended frames and stepping through them, since Windows has no built-in
//! transition API for `IDesktopWallpaper::SetWallpaper`. Mirrors the original Wall Changer's
//! fade transition feature.
//!
//! All monitors fade at the same time: every source image is loaded and every frame is rendered
//! in parallel before the first frame is shown (so encoding time doesn't stretch the fade), then
//! the frames of all monitors are stepped through together.

use super::monitors::{get_current_wallpaper, set_wallpaper_for_monitor, Monitor};
use super::settings::Settings;
use crate::util::{parallel_map, IMAGE_WORKERS};
use image::{imageops::FilterType, RgbImage};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// One monitor's wallpaper change.
pub struct WallpaperChange<'a> {
    pub monitor: &'a Monitor,
    pub path: String,
}

/// A monitor's previous and next wallpaper, rendered at its size.
struct FadeImages {
    monitor_id: String,
    from: RgbImage,
    to: RgbImage,
}

fn transition_dir() -> Result<PathBuf, String> {
    Ok(super::settings::config_dir()?.join("rendered").join("transitions"))
}

/// Sets every change's wallpaper, crossfading all monitors together from whatever they show
/// now when fade transitions are enabled. A monitor whose fade can't be prepared or shown
/// simply gets its new wallpaper at the end. Returns the first error of setting a final
/// wallpaper (every monitor is still attempted).
pub fn apply_wallpapers(changes: &[WallpaperChange], settings: &Settings) -> Result<(), String> {
    if settings.enable_wallpaper_fade_transition {
        if let Err(e) = run_fades(changes, settings) {
            log::warn!("Wallpaper fade transition failed, applying directly: {e}");
        }
    }

    // Finish on the real images so Windows applies the configured Fill/Fit mode itself
    // (and keeps doing so if the resolution changes later).
    let mut result = Ok(());
    for change in changes {
        if let Err(e) = set_wallpaper_for_monitor(&change.monitor.id, &change.path) {
            log::error!("Failed to set the wallpaper of monitor {}: {e}", change.monitor.id);
            result = result.and(Err(e));
        }
    }

    if settings.enable_wallpaper_fade_transition {
        if let Ok(dir) = transition_dir() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
    result
}

fn run_fades(changes: &[WallpaperChange], settings: &Settings) -> Result<(), String> {
    // What each monitor shows now (COM, so on this thread); monitors already showing their
    // new wallpaper, or nothing, need no fade.
    let fades: Vec<(&WallpaperChange, String)> = changes
        .iter()
        .filter_map(|change| match get_current_wallpaper(&change.monitor.id) {
            Ok(previous) if !previous.is_empty() && !previous.eq_ignore_ascii_case(&change.path) => Some((change, previous)),
            Ok(_) => None,
            Err(e) => {
                log::warn!("No fade for monitor {}: {e}", change.monitor.id);
                None
            }
        })
        .collect();
    if fades.is_empty() {
        return Ok(());
    }

    // Load both images of every fade in parallel: job 2k is fade k's previous image, 2k+1 its next.
    let mut loaded = parallel_map(fades.len() * 2, IMAGE_WORKERS, |job| {
        let (change, previous) = &fades[job / 2];
        let path = if job % 2 == 0 { previous.as_str() } else { change.path.as_str() };
        load_fitted(path, change.monitor, settings)
    })
    .into_iter();
    let mut images = Vec::new();
    for (change, _) in &fades {
        match (loaded.next().expect("two loads per fade"), loaded.next().expect("two loads per fade")) {
            (Ok(from), Ok(to)) => images.push(FadeImages { monitor_id: change.monitor.id.clone(), from, to }),
            (Err(e), _) | (_, Err(e)) => log::warn!("No fade for monitor {}: {e}", change.monitor.id),
        }
    }

    let steps = settings.wallpaper_fade_steps.max(1) as usize;
    let dir = transition_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create transition frame directory: {e}"))?;
    let frames: Vec<(String, Vec<PathBuf>)> = images
        .iter()
        .zip(render_frames(&images, steps, &dir))
        .filter_map(|(fade, frames)| match frames {
            Ok(frames) => Some((fade.monitor_id.clone(), frames)),
            Err(e) => {
                log::warn!("No fade for monitor {}: {e}", fade.monitor_id);
                None
            }
        })
        .collect();
    drop(images); // full-size images; not needed while the frames play

    let total = Duration::from_millis(settings.wallpaper_fade_duration_ms.max(0) as u64);
    play(&frames, steps, total / steps as u32);
    Ok(())
}

/// Renders frames 1..=`steps` of every fade into `dir`, all in parallel, and returns each
/// fade's frame files in order (or why they couldn't be rendered).
fn render_frames(images: &[FadeImages], steps: usize, dir: &Path) -> Vec<Result<Vec<PathBuf>, String>> {
    let rendered = parallel_map(images.len() * steps, IMAGE_WORKERS, |job| {
        let fade = &images[job / steps];
        let step = job % steps + 1;
        let path = dir.join(format!("{}_{step}.jpg", crate::util::safe_file_name(&fade.monitor_id, "")));
        blend(&fade.from, &fade.to, step as f32 / steps as f32)
            .save(&path)
            .map_err(|e| format!("Failed to save transition frame: {e}"))?;
        Ok(path)
    });
    rendered.chunks(steps.max(1)).map(|frames| frames.iter().cloned().collect()).collect()
}

/// Shows frame `i` of every monitor, waits `step`, and so on. A monitor whose frame can't be set
/// stops fading (it gets its final wallpaper with the others afterwards).
fn play(frames: &[(String, Vec<PathBuf>)], steps: usize, step: Duration) {
    let mut fading = vec![true; frames.len()];
    for i in 0..steps {
        if i > 0 {
            std::thread::sleep(step);
        }
        for ((monitor_id, files), fading) in frames.iter().zip(&mut fading) {
            if !*fading {
                continue;
            }
            if let Err(e) = set_wallpaper_for_monitor(monitor_id, &files[i].to_string_lossy()) {
                log::warn!("Fade on monitor {monitor_id} stopped: {e}");
                *fading = false;
            }
        }
    }
}

/// Loads an image and renders it at the monitor's size using the configured Fill/Fit mode, so
/// the transition frames line up with what Windows shows once the real image is applied.
fn load_fitted(path: &str, monitor: &Monitor, settings: &Settings) -> Result<RgbImage, String> {
    let img = image::open(path)
        .map_err(|e| format!("Failed to open image for transition ({path}): {e}"))?
        .to_rgba8();

    let (width, height) = (monitor.width.max(1) as u32, monitor.height.max(1) as u32);
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

    fn fade(id: &str, from: u8, to: u8) -> FadeImages {
        FadeImages {
            monitor_id: id.to_string(),
            from: RgbImage::from_pixel(16, 8, image::Rgb([from; 3])),
            to: RgbImage::from_pixel(16, 8, image::Rgb([to; 3])),
        }
    }

    #[test]
    fn frames_of_every_monitor_are_rendered_in_step_order() {
        let dir = crate::test_support::TempDir::new("fade_frames");
        let images = [fade(r"\\?\DISPLAY#A", 0, 200), fade(r"\\?\DISPLAY#B", 200, 0)];
        let frames = render_frames(&images, 4, dir.path());
        assert_eq!(frames.len(), 2);

        for (fade, frames) in images.iter().zip(&frames) {
            let frames = frames.as_ref().unwrap();
            assert_eq!(frames.len(), 4);
            let brightness: Vec<i32> = frames
                .iter()
                .map(|f| image::open(f).unwrap().to_rgb8().get_pixel(8, 4).0[0] as i32)
                .collect();
            let start = fade.from.get_pixel(0, 0).0[0] as i32;
            let end = fade.to.get_pixel(0, 0).0[0] as i32;
            // 1/4, 2/4, 3/4 and 4/4 of the way (JPEG is lossy, so roughly).
            for (step, value) in brightness.iter().enumerate() {
                let expected = start + (end - start) * (step as i32 + 1) / 4;
                assert!((value - expected).abs() <= 3, "monitor {} step {step}: {value} vs {expected}", fade.monitor_id);
            }
        }
        assert_ne!(frames[0].as_ref().unwrap()[0], frames[1].as_ref().unwrap()[0], "monitors never share frame files");
    }

    #[test]
    fn a_monitor_whose_frames_cannot_be_written_is_reported_alone() {
        let dir = crate::test_support::TempDir::new("fade_frames_fail");
        // A directory in the place of B's first frame makes that save fail.
        std::fs::create_dir(dir.join("B_1.jpg")).unwrap();
        let frames = render_frames(&[fade("A", 0, 10), fade("B", 0, 10)], 2, dir.path());
        assert!(frames[0].is_ok());
        assert!(frames[1].is_err());
    }
}
