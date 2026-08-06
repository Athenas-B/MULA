use super::images::{is_eligible_source, load_images};
use super::monitors::{apply_display_settings, get_monitors, is_windows_slideshow_enabled, set_wallpaper_for_monitor, try_disable_windows_slideshow};
use super::overlay::resolve_wallpaper_path;
use super::queue::{build_queues, choose_from_queue, ensure_queue_state, get_queue_key, rank_images_for_monitor};
use super::settings::{load, save, Settings};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::async_runtime::{spawn_blocking, JoinHandle};

static SERVICE_RUNNING: AtomicBool = AtomicBool::new(false);
static SERVICE_HANDLE: OnceLock<Mutex<Option<JoinHandle<()>>>> = OnceLock::new();

pub fn start() -> Result<(), String> {
    if SERVICE_RUNNING.load(Ordering::SeqCst) {
        return Ok(());
    }

    SERVICE_RUNNING.store(true, Ordering::SeqCst);

    let handle = spawn_blocking(|| {
        if let Err(e) = super::monitors::initialize_com() {
            log::error!("Wallchanger COM init failed: {}", e);
            SERVICE_RUNNING.store(false, Ordering::SeqCst);
            return;
        }

        let mut last_apply = Instant::now();
        let mut interval: Duration;

        loop {
            if !SERVICE_RUNNING.load(Ordering::SeqCst) {
                break;
            }

            let mut settings = match load() {
                Ok(s) => s,
                Err(e) => {
                    log::error!("Wallchanger failed to load settings: {}", e);
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
            };

            interval = Duration::from_secs(settings.interval_minutes.max(1) as u64 * 60);

            if settings.change_service_running && last_apply.elapsed() >= interval {
                let one_monitor = settings.change_one_monitor_per_interval;
                if let Err(e) = apply(&mut settings, one_monitor) {
                    log::error!("Wallchanger apply failed: {}", e);
                } else if let Err(e) = save(&settings) {
                    log::error!("Wallchanger failed to save settings: {}", e);
                }
                last_apply = Instant::now();
            }

            std::thread::sleep(Duration::from_secs(1));
        }

        unsafe { windows::Win32::System::Com::CoUninitialize() };
    });

    let guard = SERVICE_HANDLE.get_or_init(|| Mutex::new(None));
    if let Ok(mut opt) = guard.lock() {
        if let Some(old) = opt.take() {
            old.abort();
        }
        *opt = Some(handle);
    }

    Ok(())
}

pub fn stop() -> Result<(), String> {
    SERVICE_RUNNING.store(false, Ordering::SeqCst);

    if let Some(guard) = SERVICE_HANDLE.get() {
        if let Ok(mut opt) = guard.lock() {
            if let Some(handle) = opt.take() {
                handle.abort();
            }
        }
    }

    Ok(())
}

pub fn apply(settings: &mut Settings, change_one_monitor_only: bool) -> Result<String, String> {
    if !settings.source_folders.iter().any(|s| is_eligible_source(s, settings)) {
        return Err("No eligible wallpaper sources found".to_string());
    }

    let images = load_images(settings)?;
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
        set_wallpaper_for_monitor(&monitor.id, &wallpaper_path)?;

        record_image_shown(settings, &selected.image.path);

        applied.push(selected.image.path.clone());
    }

    save(settings)?;

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

pub fn toggle_service_running() -> Result<bool, String> {
    let mut settings = load()?;
    settings.change_service_running = !settings.change_service_running;
    save(&settings)?;

    if settings.change_service_running {
        start()?;
    }

    Ok(settings.change_service_running)
}
