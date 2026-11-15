use super::images::ImageInfo;
use super::monitors::Monitor;
use super::settings::{PictureSizeHandling, QueueState, RotationMode, RuntimeState, Settings};
use rand::Rng;
use std::cmp::Ordering;
use std::collections::HashMap;

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

/// The remembered place in the queue `queue_key`, created on first use.
pub fn ensure_queue_state<'a>(runtime: &'a mut RuntimeState, queue_key: &str) -> &'a mut QueueState {
    let pos = match runtime.queue_states.iter().position(|s| s.queue_key == queue_key) {
        Some(pos) => pos,
        None => {
            runtime.queue_states.push(QueueState { queue_key: queue_key.to_string(), ..Default::default() });
            runtime.queue_states.len() - 1
        }
    };
    &mut runtime.queue_states[pos]
}

/// Index of the image `RotationMode::Sequence` shows next in a non-empty `queue`: the one after
/// the image shown last, or, when that image is no longer in the queue, the one now in its place.
/// Images added since simply take their ranked position and are reached in turn.
fn next_sequence_index(queue: &[RankedImage], state: &QueueState) -> usize {
    if state.last_image_path.is_empty() {
        return 0;
    }
    match queue.iter().position(|r| r.image.path == state.last_image_path) {
        Some(pos) => (pos + 1) % queue.len(),
        None => state.last_index % queue.len(),
    }
}

/// The image `choose_from_queue` would pick next in `RotationMode::Sequence`, without changing
/// `state`. Random rotation has no fixed "next" image.
pub fn peek_next_sequence_path<'a>(queue: &'a [RankedImage], state: &QueueState) -> Option<&'a str> {
    (!queue.is_empty()).then(|| queue[next_sequence_index(queue, state)].image.path.as_str())
}

/// Picks the next image and records it in `state` as the last shown one.
pub fn choose_from_queue(queue: &[RankedImage], rotation_mode: RotationMode, state: &mut QueueState) -> Option<RankedImage> {
    if queue.is_empty() {
        return None;
    }

    let index = match rotation_mode {
        RotationMode::Random => {
            // Last-resort images are only used when nothing else exists.
            let candidates: Vec<usize> = (0..queue.len()).filter(|&i| !queue[i].last_resort).collect();
            let mut rng = rand::thread_rng();
            if candidates.is_empty() {
                rng.gen_range(0..queue.len())
            } else {
                candidates[rng.gen_range(0..candidates.len())]
            }
        }
        RotationMode::Sequence => next_sequence_index(queue, state),
    };
    state.last_image_path = queue[index].image.path.clone();
    state.last_index = index;
    Some(queue[index].clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        serde_json::from_str("{}").unwrap()
    }

    fn monitor(id: &str, width: i32, height: i32) -> Monitor {
        Monitor { id: id.into(), width, height }
    }

    fn image(path: &str, width: i32, height: i32) -> ImageInfo {
        ImageInfo { path: path.into(), width, height }
    }

    fn paths(queue: &[RankedImage]) -> Vec<&str> {
        queue.iter().map(|r| r.image.path.as_str()).collect()
    }

    /// A queue of equally ranked images on a 100x100 monitor, in the given order.
    fn queue_of(names: &[&str]) -> Vec<RankedImage> {
        let images: Vec<ImageInfo> = names.iter().map(|name| image(name, 100, 100)).collect();
        rank_images_for_monitor(&images, &monitor("M", 100, 100), &settings())
    }

    fn pick(queue: &[RankedImage], state: &mut QueueState) -> String {
        choose_from_queue(queue, RotationMode::Sequence, state).unwrap().image.path
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
        let mut runtime = RuntimeState::default();
        let queue = queue_of(&["a.jpg", "b.jpg"]);

        let state = ensure_queue_state(&mut runtime, "shared");
        assert_eq!(pick(&queue, state), "a.jpg");

        let again = ensure_queue_state(&mut runtime, "shared");
        assert_eq!(again.last_image_path, "a.jpg", "existing state must not be reset");
        assert_eq!(runtime.queue_states.len(), 1);
        ensure_queue_state(&mut runtime, "monitor:1");
        assert_eq!(runtime.queue_states.len(), 2);
    }

    #[test]
    fn sequence_rotation_walks_the_queue_in_order_and_wraps() {
        let queue = queue_of(&["a.jpg", "b.jpg", "c.jpg"]);
        let mut state = QueueState::default();

        let picked: Vec<String> = (0..4).map(|_| pick(&queue, &mut state)).collect();
        assert_eq!(picked, ["a.jpg", "b.jpg", "c.jpg", "a.jpg"]);
    }

    #[test]
    fn sequence_rotation_picks_up_images_added_later() {
        let mut state = QueueState::default();
        assert_eq!(pick(&queue_of(&["a.jpg", "b.jpg"]), &mut state), "a.jpg");

        // Two images are added to the folder: every image gets its turn, the new ones included.
        let grown = queue_of(&["a.jpg", "b.jpg", "c.jpg", "d.jpg"]);
        let picked: Vec<String> = (0..5).map(|_| pick(&grown, &mut state)).collect();
        assert_eq!(picked, ["b.jpg", "c.jpg", "d.jpg", "a.jpg", "b.jpg"]);

        // An image added before the current position doesn't cause a repeat.
        let mut state = QueueState::default();
        let queue = queue_of(&["b.jpg", "c.jpg"]);
        assert_eq!(pick(&queue, &mut state), "b.jpg");
        assert_eq!(pick(&queue_of(&["a.jpg", "b.jpg", "c.jpg"]), &mut state), "c.jpg");
    }

    #[test]
    fn sequence_rotation_continues_in_place_when_the_last_shown_image_is_gone() {
        let mut state = QueueState::default();
        let all = queue_of(&["a.jpg", "b.jpg", "c.jpg", "d.jpg"]);
        assert_eq!(pick(&all, &mut state), "a.jpg");
        assert_eq!(pick(&all, &mut state), "b.jpg");

        // "b" is deleted: "c" has moved into its place and is next.
        let without_b = queue_of(&["a.jpg", "c.jpg", "d.jpg"]);
        assert_eq!(pick(&without_b, &mut state), "c.jpg");
        assert_eq!(pick(&without_b, &mut state), "d.jpg");

        // The last image of the queue is deleted after being shown: wrap to the start.
        assert_eq!(pick(&queue_of(&["a.jpg", "c.jpg"]), &mut state), "a.jpg");

        // Nothing remembered exists any more: the place still maps into the new queue.
        let mut state = QueueState { last_image_path: "gone.jpg".into(), last_index: 5, ..Default::default() };
        assert_eq!(pick(&queue_of(&["new1.jpg", "new2.jpg"]), &mut state), "new2.jpg");
    }

    #[test]
    fn sequence_rotation_continues_after_a_random_pick() {
        let queue = queue_of(&["a.jpg", "b.jpg", "c.jpg"]);
        let mut state = QueueState::default();
        let random = choose_from_queue(&queue, RotationMode::Random, &mut state).unwrap().image.path;
        assert_eq!(state.last_image_path, random, "every pick is recorded as the last shown image");

        let position = queue.iter().position(|r| r.image.path == random).unwrap();
        assert_eq!(pick(&queue, &mut state), queue[(position + 1) % 3].image.path);
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
        let mut state = QueueState::default();

        for _ in 0..100 {
            let picked = choose_from_queue(&queue, RotationMode::Random, &mut state).unwrap();
            assert_ne!(picked.image.path, "tiny.jpg");
        }

        let only_tiny = rank_images_for_monitor(&[image("tiny.jpg", 100, 50)], &m, &s);
        let picked = choose_from_queue(&only_tiny, RotationMode::Random, &mut QueueState::default()).unwrap();
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
        let all = queue_of(&["a.jpg", "b.jpg", "c.jpg", "d.jpg"]);
        let mut state = QueueState::default();
        assert_eq!(peek_next_sequence_path(&all, &state), Some("a.jpg"));
        pick(&all, &mut state);
        pick(&all, &mut state);

        // "b" (the last shown image) was deleted since.
        let queue = queue_of(&["a.jpg", "c.jpg", "d.jpg"]);
        let peeked = peek_next_sequence_path(&queue, &state).map(str::to_string);
        assert_eq!(state.last_image_path, "b.jpg", "peek must not change the state");
        assert_eq!(peeked.as_deref(), Some(pick(&queue, &mut state).as_str()));
    }
}
