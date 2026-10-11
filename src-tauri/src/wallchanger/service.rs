use super::images::{is_eligible_source, load_images, NetworkPolicy};
use super::monitors::{apply_display_settings, get_monitors, is_windows_slideshow_enabled, try_disable_windows_slideshow};
use super::overlay::{cleanup_rendered_cache, resolve_wallpaper_path};
use super::queue::{build_queues, choose_from_queue, ensure_queue_state, get_queue_key, rank_images_for_monitor};
use super::transition::apply_wallpaper;
use super::settings::{self, Settings};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Bumped on every start/stop; a worker thread exits as soon as it no longer matches, so a quick
/// stop + start can never leave two workers running.
static GENERATION: AtomicU64 = AtomicU64::new(0);
static WORKER_ACTIVE: Mutex<bool> = Mutex::new(false);
/// Wallpaper changes are serialized: the service, the UI and the tray can all trigger one.
static APPLY_LOCK: Mutex<()> = Mutex::new(());

pub fn start() -> Result<(), String> {
    let mut active = WORKER_ACTIVE.lock().map_err(|e| e.to_string())?;
    if *active {
        return Ok(());
    }
    *active = true;
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;

    std::thread::Builder::new()
        .name("wallchanger-service".into())
        .spawn(move || run_worker(generation))
        .map_err(|e| {
            *active = false;
            format!("Failed to start wallpaper service: {e}")
        })?;
    Ok(())
}

pub fn stop() -> Result<(), String> {
    let mut active = WORKER_ACTIVE.lock().map_err(|e| e.to_string())?;
    GENERATION.fetch_add(1, Ordering::SeqCst);
    *active = false;
    Ok(())
}

/// Turns the interval service on or off and remembers the choice across restarts.
pub fn set_running(running: bool) -> Result<(), String> {
    settings::set_service_running(running)?;
    if running {
        start()?;
    } else {
        stop()?;
    }
    super::emit("wc-service-changed", running);
    Ok(())
}

fn run_worker(generation: u64) {
    if let Err(e) = super::monitors::initialize_com() {
        log::error!("Wallchanger COM init failed: {}", e);
        if GENERATION.load(Ordering::SeqCst) == generation {
            if let Ok(mut active) = WORKER_ACTIVE.lock() {
                *active = false;
            }
        }
        return;
    }

    let mut last_apply = Instant::now();
    while GENERATION.load(Ordering::SeqCst) == generation {
        match settings::load_config() {
            Ok(config) => {
                let interval = Duration::from_secs(config.interval_minutes.max(1) as u64 * 60);
                if last_apply.elapsed() >= interval {
                    if let Err(e) = apply(config.change_one_monitor_per_interval) {
                        log::error!("Wallchanger apply failed: {}", e);
                    }
                    last_apply = Instant::now();
                }
            }
            Err(e) => log::error!("Wallchanger failed to load settings: {}", e),
        }
        std::thread::sleep(Duration::from_secs(1));
    }

    #[cfg(target_os = "windows")]
    unsafe { windows::Win32::System::Com::CoUninitialize() };
}

/// Picks and sets the next wallpaper(s), then persists the resulting runtime state.
/// Blocking (image decoding, Wallhaven downloads, fade transitions): never call it on the UI thread.
pub fn apply(change_one_monitor_only: bool) -> Result<String, String> {
    let _apply_guard = APPLY_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    super::monitors::initialize_com()?;

    let mut settings = settings::load()?;
    let result = apply_with(&mut settings, change_one_monitor_only)?;
    settings::save_apply_state(&settings)?;

    if let Err(e) = cleanup_rendered_cache() {
        log::warn!("Failed to clean up rendered wallpaper cache: {e}");
    }

    super::emit("wc-wallpaper-changed", &result);
    Ok(result)
}

fn apply_with(settings: &mut Settings, change_one_monitor_only: bool) -> Result<String, String> {
    if !settings.source_folders.iter().any(|s| is_eligible_source(s, settings)) {
        return Err("No eligible wallpaper sources found".to_string());
    }

    let images = load_images(settings, NetworkPolicy::RefreshWhenStale)?;
    if images.is_empty() {
        return Err("No wallpaper images found in enabled sources".to_string());
    }

    let monitors = get_monitors()?;
    apply_display_settings(&settings.scaling_mode, settings.background_color_argb)?;
    handle_windows_slideshow_conflict(settings);

    if monitors.is_empty() {
        return Err("No monitors found".to_string());
    }

    let queues = build_queues(&images, &monitors, settings);

    let (targets, next_index) = select_target_monitors(&monitors, settings, change_one_monitor_only);
    if let Some(idx) = next_index {
        settings.next_monitor_change_index = idx;
    }

    let rotation_mode = settings.rotation_mode;
    let mut applied = Vec::new();

    for monitor in targets {
        let key = get_queue_key(settings, monitors.len(), &monitor.id, false);
        let mut queue = queues.get(&key).cloned().unwrap_or_default();
        let mut fallback = false;

        if queue.is_empty() {
            fallback = true;
            queue = rank_images_for_monitor(&images, monitor, settings);
        }

        let fallback_key = get_queue_key(settings, monitors.len(), &monitor.id, fallback);
        let selected = {
            let state = ensure_queue_state(settings, &fallback_key, &queue);
            let chosen = choose_from_queue(&queue, rotation_mode, &fallback_key, state)
                .ok_or_else(|| "No suitable wallpaper found".to_string())?;
            state.last_image_path = chosen.image.path.clone();
            chosen
        };

        let wallpaper_path = resolve_wallpaper_path(&selected.image.path, settings);
        apply_wallpaper(monitor, &wallpaper_path, settings)?;

        record_image_shown(settings, &selected.image.path);

        applied.push(selected.image.path.clone());
    }

    Ok(format!(
        "Applied {} wallpaper(s): {}",
        applied.len(),
        applied
            .iter()
            .map(|p| std::path::Path::new(p).file_name().unwrap_or_default().to_string_lossy())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn select_target_monitors<'a>(
    monitors: &'a [super::monitors::Monitor],
    settings: &Settings,
    change_one_monitor_only: bool,
) -> (Vec<&'a super::monitors::Monitor>, Option<i32>) {
    if change_one_monitor_only && monitors.len() > 1 {
        let index = (settings.next_monitor_change_index as usize) % monitors.len();
        let next = ((index + 1) % monitors.len()) as i32;
        (vec![&monitors[index]], Some(next))
    } else {
        (monitors.iter().collect(), None)
    }
}

fn handle_windows_slideshow_conflict(settings: &Settings) {
    if !settings.disable_windows_slideshow_when_running {
        return;
    }

    match is_windows_slideshow_enabled() {
        Ok(false) => {}
        Ok(true) => {
            if let Err(e) = try_disable_windows_slideshow() {
                log::warn!("Windows slideshow is enabled but could not be disabled: {e}");
            }
        }
        Err(e) => {
            log::warn!("Could not detect Windows slideshow state: {e}");
        }
    }
}

fn record_image_shown(settings: &mut Settings, path: &str) {
    if let Some(stat) = settings.image_show_stats.iter_mut().find(|s| s.path == path) {
        stat.shown_count += 1;
    } else {
        settings.image_show_stats.push(super::settings::ImageShowStat {
            path: path.to_string(),
            shown_count: 1,
        });
    }
}
