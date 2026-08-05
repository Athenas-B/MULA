use super::settings::{ImageCacheEntry, Settings, Source};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "bmp", "gif", "tif", "tiff",
];

#[derive(Debug, Clone)]
pub struct ImageInfo {
    pub path: String,
    pub width: i32,
    pub height: i32,
}

impl ImageInfo {
    pub fn aspect_ratio(&self) -> f64 {
        if self.height == 0 {
            return 1.0;
        }
        self.width as f64 / self.height as f64
    }

    pub fn is_landscape(&self) -> bool {
        self.width >= self.height
    }
}

fn is_image_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| SUPPORTED_EXTENSIONS.iter().any(|&s| s.eq_ignore_ascii_case(ext)))
        .unwrap_or(false)
}

pub fn is_eligible_source(source: &Source, settings: &Settings) -> bool {
    source.enabled && source.level <= settings.maximum_source_level && !source.path.is_empty()
}

fn walkdir(root: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if let Ok(meta) = entry.metadata() {
                    if meta.is_file() && is_image_file(&path) {
                        result.push(path);
                    } else if meta.is_dir() {
                        stack.push(path);
                    }
                }
            }
        }
    }

    result
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

pub fn load_images(settings: &mut Settings) -> Result<Vec<ImageInfo>, String> {
    let cache: HashMap<String, ImageCacheEntry> = settings
        .image_cache_entries
        .iter()
        .map(|e| (e.path.clone(), e.clone()))
        .collect();

    let mut images = Vec::new();
    let mut new_cache = Vec::new();

    for source in &settings.source_folders {
        if !is_eligible_source(source, settings) {
            continue;
        }

        let expanded = shellexpand::full(&source.path)
            .map_err(|e| format!("Failed to expand source path: {e}"))?;
        let root = PathBuf::from(expanded.as_ref());

        if !root.is_dir() {
            continue;
        }

        let paths = if source.include_subfolders {
            walkdir(&root)
        } else {
            fs::read_dir(&root)
                .map(|entries| {
                    entries
                        .filter_map(|e| e.ok())
                        .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
                        .filter(|e| is_image_file(&e.path()))
                        .map(|e| e.path())
                        .collect()
                })
                .unwrap_or_default()
        };

        for path in paths {
            let path_str = path.to_string_lossy().to_string();
            let meta = match fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };

            let length = meta.len() as i64;
            let last_write = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);

            let (width, height) = cache
                .get(&path_str)
                .filter(|e| e.length == length && {
                    let e_last = chrono::DateTime::parse_from_rfc3339(&e.last_write_utc)
                        .map(|d| d.timestamp())
                        .unwrap_or(0);
                    e_last == last_write
                })
                .map(|e| (e.width, e.height))
                .unwrap_or_else(|| {
                    read_image_dimensions(&path_str).unwrap_or((0, 0))
                });

            if width == 0 || height == 0 {
                continue;
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
    }

    settings.image_cache_entries = new_cache;
    Ok(images)
}

fn format_utc(epoch: i64) -> String {
    let dt = chrono::DateTime::from_timestamp(epoch, 0)
        .unwrap_or_else(|| chrono::DateTime::UNIX_EPOCH);
    dt.to_rfc3339()
}
