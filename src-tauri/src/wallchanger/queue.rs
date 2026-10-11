use super::images::ImageInfo;
use super::monitors::Monitor;
use super::settings::{PictureSizeHandling, QueueState, RotationMode, Settings};
use rand::Rng;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct RankedImage {
    pub image: ImageInfo,
    pub score: f64,
    pub last_resort: bool,
}

/// Whether all monitors draw from one shared queue.
pub fn uses_shared_queue(settings: &Settings, monitor_count: usize) -> bool {
    !settings.use_separate_monitor_queues || monitor_count <= 1
}

pub fn get_queue_key(settings: &Settings, monitor_count: usize, monitor_id: &str, fallback: bool) -> String {
    if uses_shared_queue(settings, monitor_count) {
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
            RankedImage { image: image.clone(), score, last_resort }
        })
        .collect();
    ranked.sort_by(rank_sort);
    ranked
}

/// The monitor an image fits best, with its score (the first monitor wins ties).
fn best_fit<'a>(image: &ImageInfo, monitors: &'a [Monitor], settings: &Settings) -> Option<(&'a Monitor, f64, bool)> {
    monitors
        .iter()
        .map(|monitor| {
            let (score, last_resort) = score_image(image, monitor, settings);
            (monitor, score, last_resort)
        })
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal))
}

pub fn build_queues(images: &[ImageInfo], monitors: &[Monitor], settings: &Settings) -> HashMap<String, Vec<RankedImage>> {
    let mut queues = HashMap::new();

    if uses_shared_queue(settings, monitors.len()) {
        // Shared queue: rank each image against its best-fitting monitor.
        let mut shared: Vec<RankedImage> = images
            .iter()
            .map(|image| match best_fit(image, monitors, settings) {
                Some((_, score, last_resort)) => RankedImage { image: image.clone(), score, last_resort },
                None => RankedImage { image: image.clone(), score: f64::MAX, last_resort: true },
            })
            .collect();
        shared.sort_by(rank_sort);
        queues.insert("shared".to_string(), shared);
        return queues;
    }

    if settings.keep_image_in_single_monitor_queue {
        // Assign each image to exactly one monitor queue (the one it fits best).
        for image in images {
            if let Some((monitor, score, last_resort)) = best_fit(image, monitors, settings) {
                let key = get_queue_key(settings, monitors.len(), &monitor.id, false);
                queues
                    .entry(key)
                    .or_insert_with(Vec::new)
                    .push(RankedImage { image: image.clone(), score, last_resort });
            }
        }
        for queue in queues.values_mut() {
            queue.sort_by(rank_sort);
        }
    } else {
        // Each monitor gets its own queue containing all images.
        for monitor in monitors {
            let key = get_queue_key(settings, monitors.len(), &monitor.id, false);
            queues.insert(key, rank_images_for_monitor(images, monitor, settings));
        }
    }

    queues
}

fn rank_sort(a: &RankedImage, b: &RankedImage) -> std::cmp::Ordering {
    a.last_resort
        .cmp(&b.last_resort)
        .then(a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal))
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

/// Returns the image path that `choose_from_queue` would pick next for `RotationMode::Sequence`,
/// without mutating `state`. Random rotation has no fixed "next" image, so this only applies to
/// Sequence mode; callers should treat a `None` result for Random mode as "picked at random".
pub fn peek_next_sequence_path(queue: &[RankedImage], state: &QueueState) -> Option<String> {
    if queue.is_empty() || state.ordered_image_paths.is_empty() {
        return None;
    }

    let available: HashSet<&String> = queue.iter().map(|r| &r.image.path).collect();
    let ordered: Vec<&String> = state
        .ordered_image_paths
        .iter()
        .filter(|p| available.contains(p))
        .collect();

    if ordered.is_empty() {
        return None;
    }

    let idx = (state.next_index as usize) % ordered.len();
    Some(ordered[idx].clone())
}

pub fn choose_from_queue(queue: &[RankedImage], rotation_mode: RotationMode, state: &mut QueueState) -> Option<RankedImage> {
    if queue.is_empty() {
        return None;
    }

    // Rebuild ordered paths if missing or mismatched; otherwise drop missing paths but keep order.
    let available: HashSet<&str> = queue.iter().map(|r| r.image.path.as_str()).collect();
    if !state.ordered_image_paths.iter().any(|p| available.contains(p.as_str())) {
        state.ordered_image_paths = queue.iter().map(|r| r.image.path.clone()).collect();
        state.next_index = 0;
    } else {
        state.ordered_image_paths.retain(|p| available.contains(p.as_str()));
    }

    match rotation_mode {
        RotationMode::Random => {
            let candidates: Vec<&RankedImage> = queue.iter().filter(|r| !r.last_resort).collect();
            let pool = if candidates.is_empty() { queue.iter().collect() } else { candidates };
            let index = rand::thread_rng().gen_range(0..pool.len());
            Some(pool[index].clone())
        }
        RotationMode::Sequence => {
            if state.next_index >= state.ordered_image_paths.len() as i32 {
                state.next_index = 0;
            }
            let path = state.ordered_image_paths[state.next_index as usize].clone();
            state.next_index += 1;
            queue.iter().find(|r| r.image.path == path).cloned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        serde_json::from_str("{}").unwrap()
    }

    fn monitor(id: &str, width: i32, height: i32) -> Monitor {
        Monitor { id: id.into(), width, height, right: width, bottom: height, ..Default::default() }
    }

    fn image(path: &str, width: i32, height: i32) -> ImageInfo {
        ImageInfo { path: path.into(), width, height }
    }

    fn paths(queue: &[RankedImage]) -> Vec<&str> {
        queue.iter().map(|r| r.image.path.as_str()).collect()
    }

    fn sequence_state(queue: &[RankedImage]) -> QueueState {
        QueueState { ordered_image_paths: queue.iter().map(|r| r.image.path.clone()).collect(), ..Default::default() }
    }

    #[test]
    fn queue_keys_depend_on_monitor_count_and_queue_mode() {
        let mut s = settings();
        assert_eq!(get_queue_key(&s, 1, "M1", false), "shared");
        assert_eq!(get_queue_key(&s, 2, "M1", false), "monitor:M1");
        assert_eq!(get_queue_key(&s, 2, "M1", true), "fallback:M1");

        s.use_separate_monitor_queues = false;
        assert_eq!(get_queue_key(&s, 3, "M1", true), "shared");
    }

    #[test]
    fn best_fitting_images_rank_first() {
        let images = [
            image("portrait.jpg", 1080, 1920),
            image("small.jpg", 960, 540),
            image("exact.jpg", 1920, 1080),
            image("ultrawide.jpg", 3440, 1440),
        ];
        let ranked = rank_images_for_monitor(&images, &monitor("M", 1920, 1080), &settings());
        assert_eq!(paths(&ranked)[0], "exact.jpg");
        assert_eq!(*paths(&ranked).last().unwrap(), "portrait.jpg", "orientation mismatch is penalized most");
        assert!(ranked.windows(2).all(|w| w[0].score <= w[1].score));
    }

    #[test]
    fn too_small_images_can_be_avoided_or_used_only_as_last_resort() {
        let images = [image("small.jpg", 1280, 720), image("big_wrong_aspect.jpg", 2560, 1600)];
        let m = monitor("M", 1920, 1080);

        let mut s = settings();
        s.too_small_picture_handling = PictureSizeHandling::Allow;
        let allowed = rank_images_for_monitor(&images, &m, &s);
        assert!(allowed.iter().all(|r| !r.last_resort));

        s.too_small_picture_handling = PictureSizeHandling::Avoid;
        let avoided = rank_images_for_monitor(&images, &m, &s);
        assert_eq!(paths(&avoided), ["big_wrong_aspect.jpg", "small.jpg"]);
        assert!(avoided[1].score >= 5000.0);

        s.too_small_picture_handling = PictureSizeHandling::UseOnlyAsLastResort;
        let last_resort = rank_images_for_monitor(&images, &m, &s);
        assert_eq!(paths(&last_resort), ["big_wrong_aspect.jpg", "small.jpg"]);
        assert!(last_resort[1].last_resort && !last_resort[0].last_resort);
    }

    #[test]
    fn too_large_images_can_be_marked_last_resort() {
        let mut s = settings();
        s.too_large_picture_handling = PictureSizeHandling::UseOnlyAsLastResort;
        let ranked = rank_images_for_monitor(
            &[image("4k.jpg", 3840, 2160), image("hd.jpg", 1920, 1080)],
            &monitor("M", 1920, 1080),
            &s,
        );
        assert_eq!(paths(&ranked), ["hd.jpg", "4k.jpg"]);
        assert!(ranked[1].last_resort);
    }

    #[test]
    fn a_single_shared_queue_holds_every_image() {
        let images = [image("a.jpg", 1920, 1080), image("b.jpg", 1080, 1920)];
        let monitors = [monitor("L", 1920, 1080), monitor("P", 1080, 1920)];
        let mut s = settings();
        s.use_separate_monitor_queues = false;

        let queues = build_queues(&images, &monitors, &s);
        assert_eq!(queues.len(), 1);
        let shared = &queues["shared"];
        assert_eq!(shared.len(), 2);
        // Each image is scored against the monitor it fits best, so both fit perfectly.
        assert!(shared.iter().all(|r| r.score < 1.0), "{:?}", shared.iter().map(|r| r.score).collect::<Vec<_>>());
    }

    #[test]
    fn a_single_monitor_always_uses_the_shared_queue() {
        let queues = build_queues(&[image("a.jpg", 10, 10)], &[monitor("only", 1920, 1080)], &settings());
        assert_eq!(queues.keys().collect::<Vec<_>>(), ["shared"]);
    }

    #[test]
    fn separate_queues_give_every_monitor_all_images() {
        let images = [image("land.jpg", 1920, 1080), image("port.jpg", 1080, 1920)];
        let monitors = [monitor("L", 1920, 1080), monitor("P", 1080, 1920)];
        let queues = build_queues(&images, &monitors, &settings());

        assert_eq!(queues.len(), 2);
        assert_eq!(paths(&queues["monitor:L"]), ["land.jpg", "port.jpg"]);
        assert_eq!(paths(&queues["monitor:P"]), ["port.jpg", "land.jpg"]);
    }

    #[test]
    fn exclusive_queues_assign_each_image_to_its_best_monitor_only() {
        let images = [image("land1.jpg", 1920, 1080), image("port.jpg", 1080, 1920), image("land2.jpg", 2560, 1440)];
        let monitors = [monitor("L", 1920, 1080), monitor("P", 1080, 1920)];
        let mut s = settings();
        s.keep_image_in_single_monitor_queue = true;

        let queues = build_queues(&images, &monitors, &s);
        assert_eq!(paths(&queues["monitor:L"]), ["land1.jpg", "land2.jpg"]);
        assert_eq!(paths(&queues["monitor:P"]), ["port.jpg"]);
        let total: usize = queues.values().map(Vec::len).sum();
        assert_eq!(total, images.len(), "no image may appear in two queues");
    }

    #[test]
    fn queue_state_is_created_once_and_then_reused() {
        let mut s = settings();
        let queue = rank_images_for_monitor(&[image("a.jpg", 1, 1), image("b.jpg", 1, 1)], &monitor("M", 1, 1), &s);

        let state = ensure_queue_state(&mut s, "shared", &queue);
        assert_eq!(state.ordered_image_paths, ["a.jpg", "b.jpg"]);
        state.next_index = 1;

        let again = ensure_queue_state(&mut s, "shared", &[]);
        assert_eq!(again.next_index, 1, "existing state must not be reset");
        assert_eq!(s.queue_states.len(), 1);
    }

    #[test]
    fn sequence_rotation_walks_the_queue_in_order_and_wraps() {
        let queue = rank_images_for_monitor(
            &[image("a.jpg", 100, 100), image("b.jpg", 100, 100), image("c.jpg", 100, 100)],
            &monitor("M", 100, 100),
            &settings(),
        );
        let mut state = sequence_state(&queue);

        let picked: Vec<String> = (0..4)
            .map(|_| choose_from_queue(&queue, RotationMode::Sequence, &mut state).unwrap().image.path)
            .collect();
        assert_eq!(picked, ["a.jpg", "b.jpg", "c.jpg", "a.jpg"]);
    }

    #[test]
    fn sequence_rotation_drops_deleted_images_but_keeps_the_order() {
        let all = [image("a.jpg", 1, 1), image("b.jpg", 1, 1), image("c.jpg", 1, 1)];
        let m = monitor("M", 1, 1);
        let mut state = sequence_state(&rank_images_for_monitor(&all, &m, &settings()));
        state.next_index = 1;

        let without_b = rank_images_for_monitor(&[all[0].clone(), all[2].clone()], &m, &settings());
        let next = choose_from_queue(&without_b, RotationMode::Sequence, &mut state).unwrap();
        assert_eq!(next.image.path, "c.jpg");
        assert_eq!(state.ordered_image_paths, ["a.jpg", "c.jpg"]);
    }

    #[test]
    fn sequence_rotation_restarts_when_no_remembered_image_exists_anymore() {
        let m = monitor("M", 1, 1);
        let mut state = QueueState { ordered_image_paths: vec!["gone.jpg".into()], next_index: 5, ..Default::default() };
        let queue = rank_images_for_monitor(&[image("new1.jpg", 1, 1), image("new2.jpg", 1, 1)], &m, &settings());

        let next = choose_from_queue(&queue, RotationMode::Sequence, &mut state).unwrap();
        assert_eq!(next.image.path, "new1.jpg");
        assert_eq!(state.next_index, 1);
    }

    #[test]
    fn random_rotation_avoids_last_resort_images_while_others_exist() {
        let mut s = settings();
        s.too_small_picture_handling = PictureSizeHandling::UseOnlyAsLastResort;
        let m = monitor("M", 1920, 1080);
        let queue = rank_images_for_monitor(
            &[image("ok1.jpg", 1920, 1080), image("ok2.jpg", 2560, 1440), image("tiny.jpg", 100, 50)],
            &m,
            &s,
        );
        let mut state = sequence_state(&queue);

        for _ in 0..100 {
            let picked = choose_from_queue(&queue, RotationMode::Random, &mut state).unwrap();
            assert_ne!(picked.image.path, "tiny.jpg");
        }

        let only_tiny = rank_images_for_monitor(&[image("tiny.jpg", 100, 50)], &m, &s);
        let picked = choose_from_queue(&only_tiny, RotationMode::Random, &mut sequence_state(&only_tiny)).unwrap();
        assert_eq!(picked.image.path, "tiny.jpg", "last resort images are used when nothing else exists");
    }

    #[test]
    fn empty_queues_yield_nothing() {
        let mut state = QueueState::default();
        assert!(choose_from_queue(&[], RotationMode::Sequence, &mut state).is_none());
        assert!(choose_from_queue(&[], RotationMode::Random, &mut state).is_none());
        assert!(peek_next_sequence_path(&[], &state).is_none());
    }

    #[test]
    fn peek_predicts_the_next_sequence_pick_without_advancing() {
        let m = monitor("M", 1, 1);
        let all = [image("a.jpg", 1, 1), image("b.jpg", 1, 1), image("c.jpg", 1, 1), image("d.jpg", 1, 1)];
        let mut state = sequence_state(&rank_images_for_monitor(&all, &m, &settings()));
        state.next_index = 3;
        // "b" was deleted since the order was recorded.
        let queue = rank_images_for_monitor(&[all[0].clone(), all[2].clone(), all[3].clone()], &m, &settings());

        let peeked = peek_next_sequence_path(&queue, &state);
        assert_eq!(state.next_index, 3, "peek must not mutate");
        let chosen = choose_from_queue(&queue, RotationMode::Sequence, &mut state).unwrap();
        assert_eq!(peeked.as_deref(), Some(chosen.image.path.as_str()));
    }
}
