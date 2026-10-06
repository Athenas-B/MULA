//! Fetches wallpapers from Wallhaven search/API URLs, caches them locally, and
//! exposes the cached images to the rest of the Wall Changer pipeline.

use super::images::ImageInfo;
use super::settings::{Settings, Source};
use serde::Deserialize;
use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_WALLHAVEN_PURITY: &str = "110";

#[derive(Deserialize)]
struct SearchResponse {
    data: Vec<Wallpaper>,
}

#[derive(Deserialize)]
struct Wallpaper {
    #[serde(rename = "id")]
    id: String,
    path: String,
}

pub fn is_wallhaven_url(path: &str) -> bool {
    let p = path.trim().to_lowercase();
    (p.starts_with("http://") || p.starts_with("https://")) && p.contains("wallhaven.cc")
}

pub fn fetch_and_cache(source: &Source, settings: &Settings) -> Result<Vec<ImageInfo>, String> {
    let base_url = source.path.trim();
    if base_url.is_empty() {
        return Ok(Vec::new());
    }

    let api_key = if settings.use_wallhaven_api_key && !settings.wallhaven_api_key.is_empty() {
        Some(settings.wallhaven_api_key.as_str())
    } else {
        None
    };

    let page_limit = if source.wallhaven_page_limit == 0 {
        u32::MAX
    } else {
        source.wallhaven_page_limit.max(1) as u32
    };
    let purity = if source.wallhaven_purity.is_empty() {
        DEFAULT_WALLHAVEN_PURITY
    } else {
        &source.wallhaven_purity
    };

    let cache_root = wallhaven_cache_dir()?;
    let cache_dir = cache_root.join(cache_folder_name(base_url));
    fs::create_dir_all(&cache_dir)
        .map_err(|e| format!("Failed to create Wallhaven cache directory: {e}"))?;

    let mut image_urls = Vec::new();
    let mut seen = HashSet::new();

    for page in 1..=page_limit as u32 {
        let url = build_api_url(base_url, api_key, page, purity)?;
        let response = match ureq::AgentBuilder::new()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .get(&url)
            .call()
        {
            Ok(r) => r,
            Err(ureq::Error::Status(code, r)) => {
                let body = r.into_string().unwrap_or_default();
                return Err(format!("Wallhaven returned HTTP {code}: {body}"));
            }
            Err(e) => return Err(format!("Wallhaven request failed: {e}")),
        };

        let text = response
            .into_string()
            .map_err(|e| format!("Failed to read Wallhaven response: {e}"))?;
        let parsed: SearchResponse = serde_json::from_str(&text)
            .map_err(|e| format!("Failed to parse Wallhaven response: {e}"))?;

        if parsed.data.is_empty() {
            break;
        }

        for wallpaper in &parsed.data {
            if seen.insert(wallpaper.id.clone()) {
                image_urls.push(wallpaper.path.clone());
            }
        }
    }

    let mut images = Vec::new();
    let agent = ureq::AgentBuilder::new()
        .timeout(REQUEST_TIMEOUT)
        .build();

    for url in image_urls {
        if let Ok(path) = download_image(&agent, &url, &cache_dir) {
            if let Some((width, height)) = read_image_dimensions(&path) {
                images.push(ImageInfo {
                    path: path.to_string_lossy().to_string(),
                    width,
                    height,
                });
            }
        }
    }

    Ok(images)
}

fn wallhaven_cache_dir() -> Result<PathBuf, String> {
    let dir = super::settings::config_dir()?.join("wallhaven");
    fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create Wallhaven cache directory: {e}"))?;
    Ok(dir)
}

fn cache_folder_name(url: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn build_api_url(input: &str, api_key: Option<&str>, page: u32, purity: &str) -> Result<String, String> {
    let trimmed = input.trim();

    if trimmed.contains("/api/v1/search") {
        let mut url = trimmed.to_string();
        if !url.starts_with("http://") && !url.starts_with("https://") {
            url = format!("https://{url}");
        }
        set_or_append_param(&mut url, "purity", purity);
        set_or_append_param(&mut url, "page", &page.to_string());
        if let Some(key) = api_key {
            set_or_append_param(&mut url, "apikey", key);
        }
        return Ok(url);
    }

    if trimmed.contains("/search?") {
        let query = trimmed
            .splitn(2, '?')
            .nth(1)
            .unwrap_or_default();
        let mut url = if query.is_empty() {
            "https://wallhaven.cc/api/v1/search".to_string()
        } else {
            format!("https://wallhaven.cc/api/v1/search?{query}")
        };
        set_or_append_param(&mut url, "purity", purity);
        set_or_append_param(&mut url, "page", &page.to_string());
        if let Some(key) = api_key {
            set_or_append_param(&mut url, "apikey", key);
        }
        return Ok(url);
    }

    Err("Unsupported Wallhaven URL. Provide a search page (https://wallhaven.cc/search?...) or an /api/v1/search URL.".into())
}

fn set_or_append_param(url: &mut String, key: &str, value: &str) {
    if url.contains(&format!("{key}=")) {
        let prefix = format!("{key}=");
        if let Some(start) = url.find(&prefix) {
            let rest = &url[start + prefix.len()..];
            let end = rest.find('&').map(|i| start + prefix.len() + i).unwrap_or(url.len());
            url.replace_range(start + prefix.len()..end, value);
            return;
        }
    }
    if url.contains('?') {
        url.push('&');
    } else {
        url.push('?');
    }
    url.push_str(key);
    url.push('=');
    url.push_str(value);
}

fn download_image(agent: &ureq::Agent, url: &str, dir: &Path) -> Result<PathBuf, String> {
    let file_name = url
        .rsplit('/')
        .next()
        .unwrap_or("image.jpg")
        .split('?')
        .next()
        .unwrap_or("image.jpg");
    let file_name = sanitize_filename(file_name);
    let path = dir.join(file_name);

    if path.exists() {
        return Ok(path);
    }

    let response = match agent.get(url).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => return Err(format!("Image download returned HTTP {code}")),
        Err(e) => return Err(format!("Image download failed: {e}")),
    };

    let mut reader = response.into_reader();
    let mut file = fs::File::create(&path)
        .map_err(|e| format!("Failed to create cache file: {e}"))?;
    io::copy(&mut reader, &mut file)
        .map_err(|e| format!("Failed to write cache file: {e}"))?;
    file.flush().map_err(|e| format!("Failed to flush cache file: {e}"))?;

    Ok(path)
}

fn sanitize_filename(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || "._-".contains(c) {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push_str("image.jpg");
    }
    out
}

fn read_image_dimensions(path: &Path) -> Option<(i32, i32)> {
    match image::image_dimensions(path) {
        Ok((w, h)) => Some((w as i32, h as i32)),
        Err(e) => {
            log::warn!("Failed to read Wallhaven image dimensions for {}: {}", path.display(), e);
            None
        }
    }
}
