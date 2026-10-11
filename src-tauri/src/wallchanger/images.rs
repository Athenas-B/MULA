use super::settings::{ImageCacheEntry, Settings, Source, SourceKind};
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
        .is_some_and(|ext| SUPPORTED_EXTENSIONS.iter().any(|s| s.eq_ignore_ascii_case(ext)))
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

/// The image files of one source with their metadata.
fn source_files(source: &Source, settings: &Settings, network: NetworkPolicy) -> Result<Vec<(PathBuf, fs::Metadata)>, String> {
    match &source.kind {
        SourceKind::Wallhaven { page_limit, purity } => {
            let api_key = Some(settings.wallhaven_api_key.as_str()).filter(|k| !k.is_empty());
            let paths = wallhaven::images_for_source(&source.path, *page_limit, *purity, api_key, network)?;
            Ok(paths
                .into_iter()
                .filter_map(|path| fs::metadata(&path).ok().map(|meta| (path, meta)))
                .collect())
        }
        SourceKind::Folder { include_subfolders } => {
            let expanded = shellexpand::full(&source.path)
                .map_err(|e| format!("Failed to expand source path {}: {e}", source.path))?;
            let root = PathBuf::from(expanded.as_ref());
            if !root.is_dir() {
                log::warn!("Skipping source {}: folder not found", source.path);
                return Ok(Vec::new());
            }
            Ok(if *include_subfolders {
                walkdir(&root)
            } else {
                list_dir(&root)
                    .into_iter()
                    .filter(|(path, meta)| meta.is_file() && is_image_file(path))
                    .collect()
            })
        }
    }
}

/// Collects every image of the eligible sources, using and refreshing the dimension `cache`.
/// A source that fails (folder can't be expanded, Wallhaven unreachable, ...) is skipped with a
/// warning; only when no source produced any image is the error returned.
pub fn load_images(
    settings: &Settings,
    cache: &mut Vec<ImageCacheEntry>,
    network: NetworkPolicy,
) -> Result<Vec<ImageInfo>, String> {
    let known: HashMap<String, ImageCacheEntry> = std::mem::take(cache).into_iter().map(|e| (e.path.clone(), e)).collect();

    let mut images = Vec::new();
    let mut errors = Vec::new();

    for source in settings.source_folders.iter().filter(|s| is_eligible_source(s, settings)) {
        match source_files(source, settings, network) {
            Ok(files) => {
                for (path, meta) in files {
                    add_image(path, &meta, &known, cache, &mut images);
                }
            }
            Err(e) => {
                log::warn!("Skipping source {}: {e}", wallhaven::redact(&source.path));
                errors.push(e);
            }
        }
    }

    if images.is_empty() && !errors.is_empty() {
        return Err(errors.join("; "));
    }
    Ok(images)
}

fn add_image(
    path: PathBuf,
    meta: &fs::Metadata,
    known: &HashMap<String, ImageCacheEntry>,
    cache: &mut Vec<ImageCacheEntry>,
    images: &mut Vec<ImageInfo>,
) {
    let path_str = path.to_string_lossy().to_string();
    let length = meta.len() as i64;
    let modified_secs = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let (width, height) = known
        .get(&path_str)
        .filter(|e| e.length == length && e.modified_secs == modified_secs)
        .map(|e| (e.width, e.height))
        .unwrap_or_else(|| read_image_dimensions(&path_str).unwrap_or((0, 0)));

    // Unreadable files are cached as 0x0 too, so they aren't decoded (and logged) on every pass.
    cache.push(ImageCacheEntry { path: path_str.clone(), length, modified_secs, width, height });
    if width > 0 && height > 0 {
        images.push(ImageInfo { path: path_str, width, height });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{write_png, TempDir};

    fn settings_with(sources: Vec<Source>) -> Settings {
        Settings { source_folders: sources, ..Settings::default() }
    }

    fn source(path: &Path) -> Source {
        Source::folder(path.to_string_lossy())
    }

    fn names(images: &[ImageInfo]) -> Vec<String> {
        let mut names: Vec<String> = images
            .iter()
            .map(|i| Path::new(&i.path).file_name().unwrap().to_string_lossy().to_string())
            .collect();
        names.sort();
        names
    }

    fn load(settings: &Settings, cache: &mut Vec<ImageCacheEntry>) -> Result<Vec<ImageInfo>, String> {
        load_images(settings, cache, NetworkPolicy::CacheOnly)
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
        let s = Settings { maximum_source_level: 5, ..Settings::default() };
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
        let mut cache = Vec::new();
        let images = load(&s, &mut cache).unwrap();
        assert_eq!(names(&images), ["deep.png", "top.png"], "unreadable and non-image files are skipped");
        let top = images.iter().find(|i| i.path.ends_with("top.png")).unwrap();
        assert_eq!((top.width, top.height), (40, 20));
        assert_eq!(cache.len(), 3, "dimensions are cached for every image file, readable or not");

        s.source_folders[0].kind = SourceKind::Folder { include_subfolders: false };
        assert_eq!(names(&load(&s, &mut cache).unwrap()), ["top.png"]);
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

        let s = Settings { maximum_source_level: 5, ..settings_with(vec![disabled, too_high, missing]) };
        assert!(load(&s, &mut Vec::new()).unwrap().is_empty());
    }

    #[test]
    fn cached_dimensions_are_reused_while_the_file_is_unchanged() {
        let dir = TempDir::new("images_cache");
        let file = write_png(&dir.join("a.png"), 64, 32);
        let meta = std::fs::metadata(&file).unwrap();
        let modified_secs = meta.modified().unwrap().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;

        let s = settings_with(vec![source(dir.path())]);
        let mut cache = vec![ImageCacheEntry {
            path: file.to_string_lossy().to_string(),
            length: meta.len() as i64,
            modified_secs,
            width: 999,
            height: 555,
        }];
        let images = load(&s, &mut cache).unwrap();
        assert_eq!((images[0].width, images[0].height), (999, 555), "cache hit must skip decoding");

        // A size mismatch means the file changed: the real dimensions are read again.
        cache[0].length += 1;
        let images = load(&s, &mut cache).unwrap();
        assert_eq!((images[0].width, images[0].height), (64, 32));
        assert_eq!(cache[0].width, 64, "the cache is refreshed");
    }

    #[test]
    fn unreadable_files_are_cached_but_not_offered() {
        let dir = TempDir::new("images_broken");
        std::fs::write(dir.join("broken.png"), b"not a png").unwrap();
        let s = settings_with(vec![source(dir.path())]);
        let mut cache = Vec::new();

        assert!(load(&s, &mut cache).unwrap().is_empty());
        assert_eq!(cache.len(), 1);
        assert_eq!((cache[0].width, cache[0].height), (0, 0));
    }

    #[test]
    fn cache_entries_for_deleted_files_are_dropped() {
        let dir = TempDir::new("images_prune");
        write_png(&dir.join("a.png"), 8, 8);
        let s = settings_with(vec![source(dir.path())]);
        let mut cache = vec![ImageCacheEntry { path: dir.join("gone.png").to_string_lossy().to_string(), ..Default::default() }];

        load(&s, &mut cache).unwrap();
        assert_eq!(cache.len(), 1);
        assert!(cache[0].path.ends_with("a.png"));
    }

    #[test]
    fn a_failing_source_is_reported_only_when_nothing_else_produced_images() {
        let dir = TempDir::new("images_errors");
        write_png(&dir.join("a.png"), 8, 8);
        let unexpandable = source(Path::new("$MULA_SURELY_UNDEFINED_VAR/pics"));

        let err = load(&settings_with(vec![unexpandable.clone()]), &mut Vec::new()).unwrap_err();
        assert!(err.contains("Failed to expand"), "{err}");

        let mixed = settings_with(vec![unexpandable, source(dir.path())]);
        assert_eq!(load(&mixed, &mut Vec::new()).unwrap().len(), 1);
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
