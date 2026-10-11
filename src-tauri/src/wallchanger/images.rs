use super::settings::{ImageCacheEntry, Settings, Source};
use super::wallhaven;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub const SUPPORTED_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "bmp", "gif", "tif", "tiff"];

#[derive(Debug, Clone)]
pub struct ImageInfo {
    pub path: String,
    pub width: i32,
    pub height: i32,
}

impl ImageInfo {
    pub fn aspect_ratio(&self) -> f64 {
        super::aspect_ratio(self.width, self.height)
    }

    pub fn is_landscape(&self) -> bool {
        self.width >= self.height
    }
}

/// Whether Wallhaven sources may hit the network while images are being collected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkPolicy {
    /// Re-query Wallhaven when the cached search results are stale (used when changing wallpaper).
    RefreshWhenStale,
    /// Only use images that are already downloaded (used by previews).
    CacheOnly,
}

pub fn is_image_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| {
            SUPPORTED_EXTENSIONS
                .iter()
                .any(|&s| s.eq_ignore_ascii_case(ext))
        })
        .unwrap_or(false)
}

pub fn is_eligible_source(source: &Source, settings: &Settings) -> bool {
    source.enabled && source.level <= settings.maximum_source_level && !source.path.is_empty()
}

/// Image files below `root` with the metadata the directory listing already provides
/// (free on Windows, so files don't have to be opened again just to stat them).
fn walkdir(root: &Path) -> Vec<(PathBuf, fs::Metadata)> {
    let mut result = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        for (path, meta) in list_dir(&dir) {
            if meta.is_file() && is_image_file(&path) {
                result.push((path, meta));
            } else if meta.is_dir() {
                stack.push(path);
            }
        }
    }

    result
}

fn list_dir(dir: &Path) -> Vec<(PathBuf, fs::Metadata)> {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter_map(|e| e.metadata().ok().map(|meta| (e.path(), meta)))
                .collect()
        })
        .unwrap_or_default()
}

fn read_image_dimensions(path: &str) -> Option<(i32, i32)> {
    match image::image_dimensions(path) {
        Ok((w, h)) => Some((w as i32, h as i32)),
        Err(e) => {
            log::warn!("Failed to read image dimensions for {}: {}", path, e);
            None
        }
    }
}

/// Collects every image of the eligible sources. A source that fails (missing folder, Wallhaven
/// unreachable, ...) is skipped with a warning instead of failing the whole collection.
pub fn load_images(settings: &mut Settings, network: NetworkPolicy) -> Result<Vec<ImageInfo>, String> {
    let cache: HashMap<String, ImageCacheEntry> = std::mem::take(&mut settings.image_cache_entries)
        .into_iter()
        .map(|e| (e.path.clone(), e))
        .collect();

    let mut images = Vec::new();
    let mut new_cache = Vec::new();
    let mut errors = Vec::new();

    for source in &settings.source_folders {
        if !is_eligible_source(source, settings) {
            continue;
        }

        if wallhaven::is_wallhaven_url(&source.path) {
            match wallhaven::images_for_source(source, settings, network) {
                Ok(paths) => {
                    for path in paths {
                        if let Ok(meta) = fs::metadata(&path) {
                            add_image(path, &meta, &cache, &mut new_cache, &mut images);
                        }
                    }
                }
                Err(e) => {
                    log::warn!("Skipping Wallhaven source {}: {e}", wallhaven::redact(&source.path));
                    errors.push(e);
                }
            }
            continue;
        }

        let expanded = match shellexpand::full(&source.path) {
            Ok(expanded) => expanded,
            Err(e) => {
                log::warn!("Skipping source {}: failed to expand path: {e}", source.path);
                errors.push(format!("Failed to expand source path {}: {e}", source.path));
                continue;
            }
        };
        let root = PathBuf::from(expanded.as_ref());

        if !root.is_dir() {
            log::warn!("Skipping source {}: folder not found", source.path);
            continue;
        }

        let files = if source.include_subfolders {
            walkdir(&root)
        } else {
            list_dir(&root)
                .into_iter()
                .filter(|(path, meta)| meta.is_file() && is_image_file(path))
                .collect()
        };

        for (path, meta) in files {
            add_image(path, &meta, &cache, &mut new_cache, &mut images);
        }
    }

    settings.image_cache_entries = new_cache;
    if images.is_empty() && !errors.is_empty() {
        return Err(errors.join("; "));
    }
    Ok(images)
}

fn add_image(
    path: PathBuf,
    meta: &fs::Metadata,
    cache: &HashMap<String, ImageCacheEntry>,
    new_cache: &mut Vec<ImageCacheEntry>,
    images: &mut Vec<ImageInfo>,
) {
    let path_str = path.to_string_lossy().to_string();
    let length = meta.len() as i64;
    let last_write = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let (width, height) = cache
        .get(&path_str)
        .filter(|e| {
            e.length == length && {
                let e_last = chrono::DateTime::parse_from_rfc3339(&e.last_write_utc)
                    .map(|d| d.timestamp())
                    .unwrap_or(0);
                e_last == last_write
            }
        })
        .map(|e| (e.width, e.height))
        .unwrap_or_else(|| read_image_dimensions(&path_str).unwrap_or((0, 0)));

    if width == 0 || height == 0 {
        return;
    }

    new_cache.push(ImageCacheEntry {
        path: path_str.clone(),
        length,
        last_write_utc: format_utc(last_write),
        width,
        height,
    });

    images.push(ImageInfo {
        path: path_str,
        width,
        height,
    });
}

fn format_utc(epoch: i64) -> String {
    let dt =
        chrono::DateTime::from_timestamp(epoch, 0).unwrap_or_else(|| chrono::DateTime::UNIX_EPOCH);
    dt.to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{write_png, TempDir};

    fn settings_with(sources: Vec<Source>) -> Settings {
        let mut s: Settings = serde_json::from_str("{}").unwrap();
        s.source_folders = sources;
        s
    }

    fn source(path: &Path) -> Source {
        serde_json::from_value(serde_json::json!({ "path": path.to_string_lossy() })).unwrap()
    }

    fn names(images: &[ImageInfo]) -> Vec<String> {
        let mut names: Vec<String> = images
            .iter()
            .map(|i| Path::new(&i.path).file_name().unwrap().to_string_lossy().to_string())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn image_files_are_recognized_by_extension_case_insensitively() {
        for name in ["a.jpg", "b.JPEG", "c.Png", "d.bmp", "e.gif", "f.tif", "g.TIFF"] {
            assert!(is_image_file(Path::new(name)), "{name}");
        }
        for name in ["a.webp", "b.txt", "jpg", "c.jpg.part", "noext"] {
            assert!(!is_image_file(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn sources_must_be_enabled_non_empty_and_within_the_level_limit() {
        let mut s = settings_with(vec![]);
        s.maximum_source_level = 5;
        let mut src = source(Path::new("C:/Pics"));
        assert!(is_eligible_source(&src, &s));

        src.level = 6;
        assert!(!is_eligible_source(&src, &s));
        src.level = 5;
        src.enabled = false;
        assert!(!is_eligible_source(&src, &s));
        src.enabled = true;
        src.path.clear();
        assert!(!is_eligible_source(&src, &s));
    }

    #[test]
    fn local_folders_are_scanned_with_or_without_subfolders() {
        let dir = TempDir::new("images_scan");
        write_png(&dir.join("top.png"), 40, 20);
        write_png(&dir.join("nested").join("deeper").join("deep.png"), 30, 30);
        std::fs::write(dir.join("notes.txt"), b"not an image").unwrap();
        std::fs::write(dir.join("broken.jpg"), b"not really a jpeg").unwrap();

        let mut s = settings_with(vec![source(dir.path())]);
        let images = load_images(&mut s, NetworkPolicy::CacheOnly).unwrap();
        assert_eq!(names(&images), ["deep.png", "top.png"], "unreadable and non-image files are skipped");
        let top = images.iter().find(|i| i.path.ends_with("top.png")).unwrap();
        assert_eq!((top.width, top.height), (40, 20));
        assert_eq!(s.image_cache_entries.len(), 2, "dimensions are cached for readable images");

        s.source_folders[0].include_subfolders = false;
        let images = load_images(&mut s, NetworkPolicy::CacheOnly).unwrap();
        assert_eq!(names(&images), ["top.png"]);
    }

    #[test]
    fn disabled_missing_and_over_level_sources_are_skipped() {
        let dir = TempDir::new("images_skip");
        write_png(&dir.join("a.png"), 10, 10);

        let mut disabled = source(dir.path());
        disabled.enabled = false;
        let mut too_high = source(dir.path());
        too_high.level = 9;
        let missing = source(&dir.join("does-not-exist"));

        let mut s = settings_with(vec![disabled, too_high, missing]);
        s.maximum_source_level = 5;
        assert!(load_images(&mut s, NetworkPolicy::CacheOnly).unwrap().is_empty());
    }

    #[test]
    fn cached_dimensions_are_reused_while_the_file_is_unchanged() {
        let dir = TempDir::new("images_cache");
        let file = write_png(&dir.join("a.png"), 64, 32);
        let meta = std::fs::metadata(&file).unwrap();
        let mtime = meta.modified().unwrap().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        let path_str = file.to_string_lossy().to_string();

        let mut s = settings_with(vec![source(dir.path())]);
        s.image_cache_entries = vec![ImageCacheEntry {
            path: path_str.clone(),
            length: meta.len() as i64,
            last_write_utc: format_utc(mtime),
            width: 999,
            height: 555,
        }];
        let images = load_images(&mut s, NetworkPolicy::CacheOnly).unwrap();
        assert_eq!((images[0].width, images[0].height), (999, 555), "cache hit must skip decoding");

        // A size mismatch means the file changed: the real dimensions are read again.
        s.image_cache_entries[0].length += 1;
        let images = load_images(&mut s, NetworkPolicy::CacheOnly).unwrap();
        assert_eq!((images[0].width, images[0].height), (64, 32));
        assert_eq!(s.image_cache_entries[0].width, 64, "the cache is refreshed");
    }

    #[test]
    fn cache_entries_for_deleted_files_are_dropped() {
        let dir = TempDir::new("images_prune");
        write_png(&dir.join("a.png"), 8, 8);
        let mut s = settings_with(vec![source(dir.path())]);
        s.image_cache_entries = vec![ImageCacheEntry { path: dir.join("gone.png").to_string_lossy().to_string(), ..Default::default() }];

        load_images(&mut s, NetworkPolicy::CacheOnly).unwrap();
        assert_eq!(s.image_cache_entries.len(), 1);
        assert!(s.image_cache_entries[0].path.ends_with("a.png"));
    }

    #[test]
    fn a_failing_source_is_reported_only_when_nothing_else_produced_images() {
        let dir = TempDir::new("images_errors");
        write_png(&dir.join("a.png"), 8, 8);
        let unexpandable = source(Path::new("$MULA_SURELY_UNDEFINED_VAR/pics"));

        let mut only_bad = settings_with(vec![unexpandable.clone()]);
        let err = load_images(&mut only_bad, NetworkPolicy::CacheOnly).unwrap_err();
        assert!(err.contains("Failed to expand"), "{err}");

        let mut mixed = settings_with(vec![unexpandable, source(dir.path())]);
        assert_eq!(load_images(&mut mixed, NetworkPolicy::CacheOnly).unwrap().len(), 1);
    }

    #[test]
    fn utc_timestamps_round_trip() {
        let formatted = format_utc(1_700_000_000);
        assert_eq!(chrono::DateTime::parse_from_rfc3339(&formatted).unwrap().timestamp(), 1_700_000_000);
    }

    #[test]
    fn landscape_and_aspect_helpers() {
        let wide = ImageInfo { path: String::new(), width: 200, height: 100 };
        let square = ImageInfo { path: String::new(), width: 50, height: 50 };
        let degenerate = ImageInfo { path: String::new(), width: 10, height: 0 };
        assert!(wide.is_landscape() && square.is_landscape());
        assert_eq!(wide.aspect_ratio(), 2.0);
        assert_eq!(degenerate.aspect_ratio(), 1.0);
    }
}
