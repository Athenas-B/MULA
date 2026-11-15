//! Crossfades between the previous and next wallpaper of each monitor by rendering a short
//! sequence of blended frames and stepping through them, since Windows has no built-in
//! transition API for `IDesktopWallpaper::SetWallpaper`. Mirrors the original Wall Changer's
//! fade transition feature.
//!
//! All monitors fade at the same time: every source image is loaded and every frame is rendered
//! in parallel before the first frame is shown (so encoding time doesn't stretch the fade), then
//! the frames of all monitors are stepped through together.
//!
//! The last frame of a fade is the new wallpaper at the monitor's size, which is exactly what
//! the next fade starts from. It is kept (one file per monitor, see [`FADE_BASES`]), so the next
//! fade doesn't have to decode and resize the full-size original again.

use super::monitors::{get_current_wallpaper, set_wallpaper_for_monitor, Monitor};
use super::preview::RenderKey;
use super::settings::Settings;
use crate::util::{lock, parallel_map, IMAGE_WORKERS};
use image::{imageops::FilterType, RgbImage};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

/// Workers for blending and encoding frames: one per core, up to 8 (each holds one
/// monitor-sized frame, about 25 MB at 4K).
fn frame_workers() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get()).min(8)
}

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
    /// What `to` shows; recorded with the kept last frame.
    to_key: RenderKey,
}

/// Which wallpaper each monitor's kept last frame ([`base_file`]) shows, by monitor id. Only
/// in memory: after a restart the first fade renders from the original once.
static FADE_BASES: LazyLock<Mutex<HashMap<String, RenderKey>>> = LazyLock::new(Default::default);

fn transition_dir() -> Result<PathBuf, String> {
    Ok(super::settings::config_dir()?.join("rendered").join("transitions"))
}

fn base_dir() -> Result<PathBuf, String> {
    Ok(super::settings::config_dir()?.join("rendered").join("fade_base"))
}

/// Where a monitor's last fade frame is kept.
fn base_file(base_dir: &Path, monitor_id: &str) -> PathBuf {
    base_dir.join(format!("{}.jpg", crate::util::safe_file_name(monitor_id, "")))
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

    let dir = transition_dir()?;
    let base_dir = base_dir()?;
    for dir in [&dir, &base_dir] {
        std::fs::create_dir_all(dir).map_err(|e| format!("Failed to create transition frame directory: {e}"))?;
    }
    // These monitors' kept frames are about to be replaced: take their records out until the
    // new frames are written, so a failed render can't leave a record for the wrong picture.
    let bases: Vec<Option<RenderKey>> = {
        let mut kept = lock(&FADE_BASES);
        fades.iter().map(|(change, _)| kept.remove(&change.monitor.id)).collect()
    };

    // Load both images of every fade in parallel: job 2k is fade k's previous image, 2k+1 its next.
    let mut loaded = parallel_map(fades.len() * 2, IMAGE_WORKERS, |job| {
        let (change, previous) = &fades[job / 2];
        if job % 2 == 0 {
            load_previous(previous, change.monitor, settings, bases[job / 2].as_ref(), &base_file(&base_dir, &change.monitor.id))
        } else {
            load_fitted(&change.path, change.monitor, settings)
        }
    })
    .into_iter();
    let mut images = Vec::new();
    for (change, _) in &fades {
        // Taken before the load: if the file changes meanwhile, the record no longer matches it.
        let to_key = RenderKey::new(&change.path, change.monitor, settings);
        match (loaded.next().expect("two loads per fade"), loaded.next().expect("two loads per fade")) {
            (Ok(from), Ok(to)) => images.push(FadeImages { monitor_id: change.monitor.id.clone(), from, to, to_key }),
            (Err(e), _) | (_, Err(e)) => log::warn!("No fade for monitor {}: {e}", change.monitor.id),
        }
    }

    let steps = settings.wallpaper_fade_steps.max(1) as usize;
    let rendered = render_frames(&images, steps, &dir, &base_dir);
    let mut frames = Vec::new();
    {
        let mut kept = lock(&FADE_BASES);
        for (fade, result) in images.into_iter().zip(rendered) {
            match result {
                Ok(files) => {
                    kept.insert(fade.monitor_id.clone(), fade.to_key);
                    frames.push((fade.monitor_id, files));
                }
                Err(e) => log::warn!("No fade for monitor {}: {e}", fade.monitor_id),
            }
        }
        remove_unrecorded_bases(&base_dir, &kept);
    } // the full-size `images` are gone here; they aren't needed while the frames play

    let total = Duration::from_millis(settings.wallpaper_fade_duration_ms.max(0) as u64);
    play(&frames, steps, total / steps as u32);
    Ok(())
}

/// Deletes kept frames nothing records any more (monitors that are gone, or files left by an
/// earlier run of MULA).
fn remove_unrecorded_bases(base_dir: &Path, kept: &HashMap<String, RenderKey>) {
    let recorded: Vec<PathBuf> = kept.keys().map(|monitor_id| base_file(base_dir, monitor_id)).collect();
    for entry in std::fs::read_dir(base_dir).into_iter().flatten().flatten() {
        if !recorded.contains(&entry.path()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Renders frames 1..=`steps` of every fade, all in parallel, and returns each fade's frame
/// files in order (or why they couldn't be rendered). The frames go into `dir`, except the last
/// one: it is the new image itself and is written to the monitor's kept file in `base_dir`.
fn render_frames(images: &[FadeImages], steps: usize, dir: &Path, base_dir: &Path) -> Vec<Result<Vec<PathBuf>, String>> {
    let mut rendered = parallel_map(images.len() * steps, frame_workers(), |job| {
        let fade = &images[job / steps];
        let step = job % steps + 1;
        let blended;
        let (frame, path) = if step == steps {
            (&fade.to, base_file(base_dir, &fade.monitor_id))
        } else {
            blended = blend(&fade.from, &fade.to, step as f32 / steps as f32);
            (&blended, dir.join(format!("{}_{step}.jpg", crate::util::safe_file_name(&fade.monitor_id, ""))))
        };
        frame.save(&path).map_err(|e| format!("Failed to save transition frame: {e}"))?;
        Ok(path)
    })
    .into_iter();
    images.iter().map(|_| rendered.by_ref().take(steps).collect()).collect()
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

/// The wallpaper a monitor shows now, at the monitor's size: its kept last fade frame when
/// `base` says that frame shows exactly this wallpaper (same file, monitor size and fit), else
/// rendered from the file like any other image.
fn load_previous(path: &str, monitor: &Monitor, settings: &Settings, base: Option<&RenderKey>, base_file: &Path) -> Result<RgbImage, String> {
    if base == Some(&RenderKey::new(path, monitor, settings)) {
        match image::open(base_file) {
            Ok(frame) if (frame.width() as i32, frame.height() as i32) == (monitor.width.max(1), monitor.height.max(1)) => {
                return Ok(frame.into_rgb8());
            }
            Ok(_) => {}
            Err(e) => log::warn!("Can't reuse the last fade frame of monitor {}: {e}", monitor.id),
        }
    }
    load_fitted(path, monitor, settings)
}

/// Loads an image and renders it at the monitor's size using the configured Fill/Fit mode, so
/// the transition frames line up with what Windows shows once the real image is applied.
fn load_fitted(path: &str, monitor: &Monitor, settings: &Settings) -> Result<RgbImage, String> {
    let img = super::preview::open_rgba(path)?;

    let (width, height) = (monitor.width.max(1) as u32, monitor.height.max(1) as u32);
    let background = super::preview::argb_to_rgba(settings.background_color_argb);
    let fitted = super::preview::fit_to_canvas(&img, width, height, &settings.scaling_mode, background, FilterType::Triangle);
    Ok(image::DynamicImage::ImageRgba8(fitted).to_rgb8())
}

/// Mixes two same-sized images: `alpha` 0 gives `from`, 1 gives `to`.
fn blend(from: &RgbImage, to: &RgbImage, alpha: f32) -> RgbImage {
    let weight = (alpha.clamp(0.0, 1.0) * 256.0).round() as u32;
    let (width, height) = from.dimensions();
    let mixed = from
        .as_raw()
        .iter()
        .zip(to.as_raw())
        .map(|(&a, &b)| lerp(a, b, weight))
        .collect();
    RgbImage::from_raw(width, height, mixed).expect("both frames have the monitor's size")
}

/// `a` moved towards `b` by `weight`/256. Integer math: this runs for every byte of every
/// frame, where per-byte float rounding was ten times slower.
fn lerp(a: u8, b: u8, weight: u32) -> u8 {
    ((a as u32 * (256 - weight) + b as u32 * weight + 128) >> 8) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lerp_interpolates_between_endpoints() {
        assert_eq!(lerp(0, 100, 0), 0);
        assert_eq!(lerp(0, 100, 256), 100);
        assert_eq!(lerp(0, 100, 128), 50);
        assert_eq!(lerp(255, 255, 77), 255);
        assert_eq!(lerp(255, 0, 256), 0);
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
            to_key: RenderKey::default(),
        }
    }

    #[test]
    fn frames_of_every_monitor_are_rendered_in_step_order() {
        let dir = crate::test_support::TempDir::new("fade_frames");
        let images = [fade(r"\\?\DISPLAY#A", 0, 200), fade(r"\\?\DISPLAY#B", 200, 0)];
        let base_dir = dir.join("base");
        std::fs::create_dir(&base_dir).unwrap();
        let frames = render_frames(&images, 4, dir.path(), &base_dir);
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

        // The last frame of each monitor is its kept file; the others are throwaway frames.
        for (fade, frames) in images.iter().zip(&frames) {
            let frames = frames.as_ref().unwrap();
            assert_eq!(frames[3], base_file(&base_dir, &fade.monitor_id));
            assert!(frames[..3].iter().all(|f| f.parent() == Some(dir.path())));
        }
    }

    fn monitor(width: i32, height: i32) -> Monitor {
        Monitor { id: "M1".into(), width, height }
    }

    /// Red channel of the centre pixel (clear of any letterbox): enough to tell the test
    /// pictures apart.
    fn red(image: &RgbImage) -> u8 {
        image.get_pixel(image.width() / 2, image.height() / 2).0[0]
    }

    #[test]
    fn the_kept_last_frame_is_reused_while_it_shows_the_current_wallpaper() {
        let dir = crate::test_support::TempDir::new("fade_base");
        let settings = Settings::default();
        let m = monitor(32, 16);
        // The wallpaper file is red; the kept frame is drawn dark so the two can be told apart.
        let wallpaper = dir.join("current.png");
        image::RgbImage::from_pixel(64, 64, image::Rgb([250, 0, 0])).save(&wallpaper).unwrap();
        let wallpaper = wallpaper.to_string_lossy().to_string();
        let kept = dir.join("kept.jpg");
        RgbImage::from_pixel(32, 16, image::Rgb([10, 10, 10])).save(&kept).unwrap();
        let key = RenderKey::new(&wallpaper, &m, &settings);

        let load = |base: Option<&RenderKey>, monitor: &Monitor, settings: &Settings| {
            load_previous(&wallpaper, monitor, settings, base, &kept).unwrap()
        };
        assert!(red(&load(Some(&key), &m, &settings)) < 50, "the kept frame is used");
        assert!(red(&load(None, &m, &settings)) > 200, "nothing kept: rendered from the file");

        // Anything the picture depends on changing means rendering from the file again.
        let resized = monitor(16, 16);
        assert!(red(&load(Some(&key), &resized, &settings)) > 200, "monitor size changed");
        let mut fit = settings.clone();
        fit.scaling_mode = super::super::settings::ScalingMode::FitInsideScreen;
        assert!(red(&load(Some(&key), &m, &fit)) > 200, "fit mode changed");
        let other = RenderKey::new(&dir.join("other.png").to_string_lossy(), &m, &settings);
        assert!(red(&load(Some(&other), &m, &settings)) > 200, "the kept frame shows another wallpaper");

        // The wallpaper file was replaced by a different picture.
        image::RgbImage::from_pixel(80, 80, image::Rgb([250, 0, 0])).save(&wallpaper).unwrap();
        assert!(red(&load(Some(&key), &m, &settings)) > 200, "file changed");
    }

    #[test]
    fn a_missing_or_wrongly_sized_kept_frame_falls_back_to_the_file() {
        let dir = crate::test_support::TempDir::new("fade_base_bad");
        let settings = Settings::default();
        let m = monitor(32, 16);
        let wallpaper = dir.join("current.png");
        image::RgbImage::from_pixel(64, 64, image::Rgb([250, 0, 0])).save(&wallpaper).unwrap();
        let wallpaper = wallpaper.to_string_lossy().to_string();
        let key = RenderKey::new(&wallpaper, &m, &settings);

        let missing = dir.join("missing.jpg");
        let image = load_previous(&wallpaper, &m, &settings, Some(&key), &missing).unwrap();
        assert_eq!((image.dimensions(), red(&image) > 200), ((32, 16), true));

        let wrong_size = dir.join("wrong.jpg");
        RgbImage::from_pixel(8, 8, image::Rgb([10, 10, 10])).save(&wrong_size).unwrap();
        let image = load_previous(&wallpaper, &m, &settings, Some(&key), &wrong_size).unwrap();
        assert_eq!((image.dimensions(), red(&image) > 200), ((32, 16), true));
    }

    #[test]
    fn kept_frames_nothing_records_are_deleted() {
        let dir = crate::test_support::TempDir::new("fade_base_prune");
        let current = base_file(dir.path(), "M1");
        let stale = base_file(dir.path(), "GONE");
        std::fs::write(&current, b"x").unwrap();
        std::fs::write(&stale, b"x").unwrap();

        let kept = HashMap::from([("M1".to_string(), RenderKey::default())]);
        remove_unrecorded_bases(dir.path(), &kept);
        assert!(current.exists());
        assert!(!stale.exists());
    }

    #[test]
    fn a_monitor_whose_frames_cannot_be_written_is_reported_alone() {
        let dir = crate::test_support::TempDir::new("fade_frames_fail");
        // A directory in the place of B's first frame makes that save fail.
        std::fs::create_dir(dir.join("B_1.jpg")).unwrap();
        let frames = render_frames(&[fade("A", 0, 10), fade("B", 0, 10)], 2, dir.path(), dir.path());
        assert!(frames[0].is_ok());
        assert!(frames[1].is_err());
    }
}
