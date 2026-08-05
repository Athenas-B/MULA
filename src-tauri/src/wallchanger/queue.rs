use super::images::ImageInfo;
use super::monitors::Monitor;
use super::settings::{PictureSizeHandling, QueueState, RotationMode, Settings};
use rand::Rng;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct RankedImage {
    pub image: ImageInfo,
    pub score: f64,
    pub last_resort: bool,
}

pub fn get_queue_key(settings: &Settings, monitor_count: usize, monitor_id: &str, fallback: bool) -> String {
    if !settings.use_separate_monitor_queues || monitor_count <= 1 {
        "shared".to_string()
    } else if fallback {
        format!("fallback:{monitor_id}")
    } else {
        format!("monitor:{monitor_id}")
    }
}

fn too_small(image: &ImageInfo, monitor: &Monitor) -> bool {
    image.width < monitor.width || image.height < monitor.height
}

fn too_large(image: &ImageInfo, monitor: &Monitor) -> bool {
    image.width > monitor.width || image.height > monitor.height
}

fn score_image(image: &ImageInfo, monitor: &Monitor, settings: &Settings) -> (f64, bool) {
    let mut score = 0.0;

    let image_aspect = image.aspect_ratio();
    let monitor_aspect = monitor.aspect_ratio();

    if image_aspect > 0.0 && monitor_aspect > 0.0 {
        score += 1200.0 * (image_aspect / monitor_aspect).ln().abs();
    }

    if monitor.width > 0 && image.width > 0 {
        score += 80.0 * (image.width as f64 / monitor.width as f64).ln().abs();
    }
    if monitor.height > 0 && image.height > 0 {
        score += 80.0 * (image.height as f64 / monitor.height as f64).ln().abs();
    }

    let width_scale = if image.width > 0 {
        monitor.width as f64 / image.width as f64
    } else {
        1.0
    };
    let height_scale = if image.height > 0 {
        monitor.height as f64 / image.height as f64
    } else {
        1.0
    };
    let fill_scale = width_scale.max(height_scale);
    if fill_scale > 1.0 {
        score += 250.0 + 30.0 * (fill_scale - 1.0);
    }

    let image_landscape = image.is_landscape();
    let monitor_landscape = monitor.width >= monitor.height;
    if image_landscape != monitor_landscape {
        score += 800.0;
    }

    let mut last_resort = false;

    if too_small(image, monitor) {
        match settings.too_small_picture_handling {
            PictureSizeHandling::Allow => {}
            PictureSizeHandling::Avoid => score += 5000.0,
            PictureSizeHandling::UseOnlyAsLastResort => last_resort = true,
        }
    }

    if too_large(image, monitor) {
        match settings.too_large_picture_handling {
            PictureSizeHandling::Allow => {}
            PictureSizeHandling::Avoid => score += 5000.0,
            PictureSizeHandling::UseOnlyAsLastResort => last_resort = true,
        }
    }

    (score, last_resort)
}

pub fn rank_images_for_monitor(images: &[ImageInfo], monitor: &Monitor, settings: &Settings) -> Vec<RankedImage> {
    let mut ranked: Vec<RankedImage> = images
        .iter()
        .map(|image| {
            let (score, last_resort) = score_image(image, monitor, settings);
            RankedImage {
                image: image.clone(),
                score,
                last_resort,
            }
        })
        .collect();

    ranked.sort_by(|a, b| {
        a.last_resort
            .cmp(&b.last_resort)
            .then(a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal))
    });

    ranked
}

pub fn build_queues(images: &[ImageInfo], monitors: &[Monitor], settings: &Settings) -> HashMap<String, Vec<RankedImage>> {
    let mut queues = HashMap::new();

    if !settings.use_separate_monitor_queues || monitors.len() <= 1 {
        // Shared queue: rank each image against its best-fitting monitor.
        let mut shared: Vec<RankedImage> = images
            .iter()
            .map(|image| {
                let mut best: Option<RankedImage> = None;
                for monitor in monitors {
                    let (score, last_resort) = score_image(image, monitor, settings);
                    if best.is_none() || score < best.as_ref().unwrap().score {
                        best = Some(RankedImage {
                            image: image.clone(),
                            score,
                            last_resort,
                        });
                    }
                }
                best.unwrap_or_else(|| RankedImage {
                    image: image.clone(),
                    score: f64::MAX,
                    last_resort: true,
                })
            })
            .collect();

        shared.sort_by(|a, b| {
            a.last_resort
                .cmp(&b.last_resort)
                .then(a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal))
        });

        queues.insert("shared".to_string(), shared);
        return queues;
    }

    for monitor in monitors {
        let key = get_queue_key(settings, monitors.len(), &monitor.id, false);
        queues.insert(key, rank_images_for_monitor(images, monitor, settings));
    }

    queues
}

pub fn ensure_queue_state<'a>(
    settings: &'a mut Settings,
    queue_key: &str,
    queue: &[RankedImage],
) -> &'a mut QueueState {
    if let Some(pos) = settings.queue_states.iter().position(|s| s.queue_key == queue_key) {
        return &mut settings.queue_states[pos];
    }

    let paths: Vec<String> = queue.iter().map(|r| r.image.path.clone()).collect();
    let state = QueueState {
        queue_key: queue_key.to_string(),
        ordered_image_paths: paths,
        ..Default::default()
    };
    settings.queue_states.push(state);
    settings.queue_states.last_mut().unwrap()
}

pub fn choose_from_queue(
    queue: &[RankedImage],
    rotation_mode: RotationMode,
    _queue_key: &str,
    state: &mut QueueState,
) -> Option<RankedImage> {
    if queue.is_empty() {
        return None;
    }

    // Rebuild ordered paths if missing or mismatched.
    let available: std::collections::HashSet<String> = queue.iter().map(|r| r.image.path.clone()).collect();
    if state.ordered_image_paths.is_empty() || !state.ordered_image_paths.iter().any(|p| available.contains(p)) {
        state.ordered_image_paths = queue.iter().map(|r| r.image.path.clone()).collect();
        state.next_index = 0;
    } else {
        // Remove missing paths but keep order.
        state.ordered_image_paths.retain(|p| available.contains(p));
    }

    match rotation_mode {
        RotationMode::Random => {
            let candidates: Vec<&RankedImage> = queue.iter().filter(|r| !r.last_resort).collect();
            let pool: Vec<RankedImage> = if candidates.is_empty() {
                queue.to_vec()
            } else {
                candidates.iter().map(|&r| r.clone()).collect()
            };
            if pool.is_empty() {
                return None;
            }
            let mut rng = rand::thread_rng();
            let index = rng.gen_range(0..pool.len());
            Some(pool[index].clone())
        }
        RotationMode::Sequence => {
            if state.ordered_image_paths.is_empty() {
                return None;
            }
            if state.next_index >= state.ordered_image_paths.len() as i32 {
                state.next_index = 0;
            }
            let path = state.ordered_image_paths[state.next_index as usize].clone();
            state.next_index += 1;
            queue.iter().find(|r| r.image.path == path).cloned()
        }
    }
}
