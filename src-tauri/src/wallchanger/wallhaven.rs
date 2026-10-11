//! Fetches wallpapers from Wallhaven search/API URLs, caches them locally, and
//! exposes the cached images to the rest of the Wall Changer pipeline.
//!
//! Search results are cached per source in a `manifest.json` and reused for [`RESULTS_TTL`],
//! so a wallpaper change normally does no network I/O at all. When a refresh fails (offline,
//! rate limited, ...) the previously downloaded images keep being used.

use super::images::NetworkPolicy;
use super::settings::{Settings, Source};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_WALLHAVEN_PURITY: &str = "110";
/// How long fetched search results are reused before Wallhaven is queried again.
const RESULTS_TTL: Duration = Duration::from_secs(6 * 60 * 60);
/// Wallhaven allows 45 API requests per minute.
const API_REQUEST_SPACING: Duration = Duration::from_millis(1500);
const MANIFEST_FILE: &str = "manifest.json";
const PARTIAL_SUFFIX: &str = ".part";

#[derive(Deserialize)]
struct SearchResponse {
    data: Vec<Wallpaper>,
    #[serde(default)]
    meta: Option<SearchMeta>,
}

#[derive(Deserialize)]
struct SearchMeta {
    #[serde(default)]
    last_page: Option<u32>,
}

#[derive(Deserialize)]
struct Wallpaper {
    #[serde(rename = "id")]
    id: String,
    path: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    /// Everything that influences the result set; a change invalidates the cache.
    query: String,
    /// Unix seconds.
    fetched_at: u64,
    files: Vec<String>,
}

pub fn is_wallhaven_url(path: &str) -> bool {
    let p = path.trim().to_lowercase();
    (p.starts_with("http://") || p.starts_with("https://")) && p.contains("wallhaven.cc")
}

/// Removes API keys from text that may end up in logs or the UI.
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(idx) = rest.to_ascii_lowercase().find("apikey=") {
        let value_start = idx + "apikey=".len();
        out.push_str(&rest[..value_start]);
        out.push_str("***");
        let tail = &rest[value_start..];
        let end = tail.find(|c: char| c == '&' || c.is_whitespace() || c == '"').unwrap_or(tail.len());
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// Returns the cached image files for a Wallhaven source, refreshing the search results first
/// when they are older than [`RESULTS_TTL`] and `network` allows it.
pub fn images_for_source(source: &Source, settings: &Settings, network: NetworkPolicy) -> Result<Vec<PathBuf>, String> {
    let base_url = source.path.trim();
    if base_url.is_empty() {
        return Ok(Vec::new());
    }

    let cache_dir = source_cache_dir(base_url)?;
    let query = query_signature(source, settings);
    let manifest = read_manifest(&cache_dir);
    let fresh = manifest
        .as_ref()
        .is_some_and(|m| m.query == query && now_secs().saturating_sub(m.fetched_at) < RESULTS_TTL.as_secs());

    if fresh || network == NetworkPolicy::CacheOnly {
        return Ok(cached_files(&cache_dir, manifest.as_ref()));
    }

    match refresh(source, settings, &cache_dir, query) {
        Ok(files) => Ok(files),
        Err(e) => {
            let cached = cached_files(&cache_dir, manifest.as_ref());
            if cached.is_empty() {
                Err(redact(&e))
            } else {
                log::warn!(
                    "Wallhaven refresh failed, using {} cached image(s): {}",
                    cached.len(),
                    redact(&e)
                );
                Ok(cached)
            }
        }
    }
}

fn query_signature(source: &Source, settings: &Settings) -> String {
    format!(
        "{}|purity={}|pages={}|apikey={}",
        source.path.trim(),
        effective_purity(source),
        source.wallhaven_page_limit,
        settings.use_wallhaven_api_key && !settings.wallhaven_api_key.is_empty()
    )
}

fn effective_purity(source: &Source) -> &str {
    if source.wallhaven_purity.is_empty() {
        DEFAULT_WALLHAVEN_PURITY
    } else {
        &source.wallhaven_purity
    }
}

fn refresh(source: &Source, settings: &Settings, cache_dir: &Path, query: String) -> Result<Vec<PathBuf>, String> {
    let base_url = source.path.trim();
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
    let purity = effective_purity(source);

    let agent = ureq::AgentBuilder::new().timeout(REQUEST_TIMEOUT).build();
    let mut image_urls = Vec::new();
    let mut seen = HashSet::new();

    for page in 1..=page_limit {
        if page > 1 {
            std::thread::sleep(API_REQUEST_SPACING);
        }

        let url = build_api_url(base_url, page, purity)?;
        let mut request = agent.get(&url);
        if let Some(key) = api_key {
            // A header keeps the key out of URLs, and so out of logs and error messages.
            request = request.set("X-API-Key", key);
        }
        let response = match request.call() {
            Ok(r) => r,
            Err(ureq::Error::Status(429, _)) if !image_urls.is_empty() => {
                log::warn!("Wallhaven rate limit reached after {} page(s); using the results so far", page - 1);
                break;
            }
            Err(ureq::Error::Status(code, r)) => {
                let body = r.into_string().unwrap_or_default();
                return Err(format!("Wallhaven returned HTTP {code}: {}", body.trim()));
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
        if parsed.meta.and_then(|m| m.last_page).is_some_and(|last| page >= last) {
            break;
        }
    }

    let mut files = Vec::new();
    for url in &image_urls {
        match download_image(&agent, url, cache_dir) {
            Ok(path) => files.push(path),
            Err(e) => log::warn!("Skipping Wallhaven image {url}: {e}"),
        }
    }

    record_refresh(cache_dir, query, image_urls.len(), files)
}

/// Stores the result of a refresh. If images were found but none could be downloaded (e.g.
/// the image CDN is unreachable), this is an error, so the caller keeps using the cache
/// instead of treating the source as freshly fetched and empty for the next few hours.
fn record_refresh(cache_dir: &Path, query: String, attempted: usize, files: Vec<PathBuf>) -> Result<Vec<PathBuf>, String> {
    if attempted > 0 && files.is_empty() {
        return Err(format!("None of the {attempted} Wallhaven image(s) could be downloaded"));
    }

    let manifest = Manifest {
        query,
        fetched_at: now_secs(),
        files: files
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect(),
    };
    if let Err(e) = write_manifest(cache_dir, &manifest) {
        log::warn!("Failed to write Wallhaven manifest: {e}");
    }

    Ok(files)
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn read_manifest(cache_dir: &Path) -> Option<Manifest> {
    let text = fs::read_to_string(cache_dir.join(MANIFEST_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_manifest(cache_dir: &Path, manifest: &Manifest) -> Result<(), String> {
    let text = serde_json::to_string(manifest).map_err(|e| e.to_string())?;
    crate::util::write_atomic(&cache_dir.join(MANIFEST_FILE), &text)
}

/// Files of the last successful fetch, or (for caches made before manifests existed) every
/// image in the folder.
fn cached_files(cache_dir: &Path, manifest: Option<&Manifest>) -> Vec<PathBuf> {
    match manifest {
        Some(m) => m.files.iter().map(|f| cache_dir.join(f)).filter(|p| p.is_file()).collect(),
        None => fs::read_dir(cache_dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.is_file() && super::images::is_image_file(p))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn source_cache_dir(url: &str) -> Result<PathBuf, String> {
    let root = super::settings::config_dir()?.join("wallhaven");
    let dir = root.join(cache_folder_name(url));

    // Folders used to be named with `DefaultHasher`, whose output is not stable across Rust
    // releases. Adopt such a folder instead of downloading everything again.
    let legacy = root.join(legacy_cache_folder_name(url));
    if !dir.exists() && legacy.is_dir() {
        if let Err(e) = fs::rename(&legacy, &dir) {
            log::warn!("Could not migrate Wallhaven cache folder {}: {e}", legacy.display());
        }
    }

    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create Wallhaven cache directory: {e}"))?;
    Ok(dir)
}

/// 64-bit FNV-1a: stable across builds, unlike `DefaultHasher`.
fn cache_folder_name(url: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in url.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("fnv{hash:016x}")
}

fn legacy_cache_folder_name(url: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn build_api_url(input: &str, page: u32, purity: &str) -> Result<String, String> {
    let trimmed = input.trim();

    if trimmed.contains("/api/v1/search") {
        let mut url = trimmed.to_string();
        if !url.starts_with("http://") && !url.starts_with("https://") {
            url = format!("https://{url}");
        }
        set_or_append_param(&mut url, "purity", purity);
        set_or_append_param(&mut url, "page", &page.to_string());
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
        return Ok(url);
    }

    Err("Unsupported Wallhaven URL. Provide a search page (https://wallhaven.cc/search?...) or an /api/v1/search URL.".into())
}

fn set_or_append_param(url: &mut String, key: &str, value: &str) {
    let (base, query) = match url.split_once('?') {
        Some((base, query)) => (base.to_string(), query.to_string()),
        None => (url.clone(), String::new()),
    };

    let mut found = false;
    let mut params: Vec<String> = query
        .split('&')
        .filter(|param| !param.is_empty())
        .map(|param| {
            let name = param.split('=').next().unwrap_or_default();
            if !found && name == key {
                found = true;
                format!("{key}={value}")
            } else {
                param.to_string()
            }
        })
        .collect();
    if !found {
        params.push(format!("{key}={value}"));
    }

    *url = format!("{base}?{}", params.join("&"));
}

/// Downloads `url` into `dir` unless a valid copy is already there. Downloads go to a `.part`
/// file first, so an interrupted transfer never leaves a truncated image behind.
fn download_image(agent: &ureq::Agent, url: &str, dir: &Path) -> Result<PathBuf, String> {
    let file_name = url
        .rsplit('/')
        .next()
        .unwrap_or("image.jpg")
        .split('?')
        .next()
        .unwrap_or("image.jpg");
    let file_name = sanitize_filename(file_name);
    let path = dir.join(&file_name);

    if path.exists() {
        if is_valid_image(&path) {
            return Ok(path);
        }
        // A truncated or corrupt file from an older version: fetch it again.
        let _ = fs::remove_file(&path);
    }

    let response = match agent.get(url).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => return Err(format!("Image download returned HTTP {code}")),
        Err(e) => return Err(format!("Image download failed: {e}")),
    };

    let partial = dir.join(format!("{file_name}{PARTIAL_SUFFIX}"));
    let result = (|| {
        let mut reader = response.into_reader();
        let mut file = fs::File::create(&partial).map_err(|e| format!("Failed to create cache file: {e}"))?;
        io::copy(&mut reader, &mut file).map_err(|e| format!("Failed to write cache file: {e}"))?;
        file.flush().map_err(|e| format!("Failed to flush cache file: {e}"))?;
        drop(file);
        if !is_valid_image(&partial) {
            return Err("Downloaded file is not a readable image".to_string());
        }
        fs::rename(&partial, &path).map_err(|e| format!("Failed to finalize cache file: {e}"))
    })();

    if let Err(e) = result {
        let _ = fs::remove_file(&partial);
        return Err(e);
    }
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

fn is_valid_image(path: &Path) -> bool {
    matches!(image::image_dimensions(path), Ok((w, h)) if w > 0 && h > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_hides_api_keys() {
        assert_eq!(
            redact("https://wallhaven.cc/api/v1/search?q=x&apikey=SECRET123&page=2: timed out"),
            "https://wallhaven.cc/api/v1/search?q=x&apikey=***&page=2: timed out"
        );
        assert_eq!(redact("GET ...?APIKEY=abc"), "GET ...?APIKEY=***");
        assert_eq!(redact("no key here"), "no key here");
    }

    #[test]
    fn cache_folder_name_is_stable() {
        // Pinned value: changing the hash would orphan every user's download cache.
        assert_eq!(cache_folder_name("https://wallhaven.cc/search?q=forest"), cache_folder_name("https://wallhaven.cc/search?q=forest"));
        assert_eq!(cache_folder_name(""), "fnvcbf29ce484222325");
        assert_ne!(cache_folder_name("a"), cache_folder_name("b"));
    }

    #[test]
    fn api_url_is_built_from_search_page_urls() {
        let url = build_api_url("https://wallhaven.cc/search?q=forest&purity=100", 3, "110").unwrap();
        assert_eq!(url, "https://wallhaven.cc/api/v1/search?q=forest&purity=110&page=3");
        assert!(!url.contains("apikey"), "the API key is sent as a header, never in the URL");
        assert!(build_api_url("https://wallhaven.cc/w/abc", 1, "100").is_err());
    }

    #[test]
    fn cached_files_follow_the_manifest_and_skip_missing_files() {
        let tmp = crate::test_support::TempDir::new("wh_cached");
        let dir = tmp.path().to_path_buf();
        fs::write(dir.join("a.jpg"), b"x").unwrap();
        fs::write(dir.join("b.jpg.part"), b"x").unwrap();

        let manifest = Manifest { query: "q".into(), fetched_at: 0, files: vec!["a.jpg".into(), "gone.jpg".into()] };
        assert_eq!(cached_files(&dir, Some(&manifest)), vec![dir.join("a.jpg")]);
        // Without a manifest, partial downloads are never picked up.
        assert_eq!(cached_files(&dir, None), vec![dir.join("a.jpg")]);
    }
}

#[cfg(test)]
mod helper_tests {
    use super::*;

    fn source(path: &str) -> Source {
        serde_json::from_value(serde_json::json!({ "path": path })).unwrap()
    }

    fn settings() -> Settings {
        serde_json::from_str("{}").unwrap()
    }

    #[test]
    fn only_http_urls_on_wallhaven_count_as_wallhaven_sources() {
        assert!(is_wallhaven_url("https://wallhaven.cc/search?q=forest"));
        assert!(is_wallhaven_url("  HTTP://WALLHAVEN.CC/api/v1/search  "));
        assert!(!is_wallhaven_url(r"C:\Pictures\wallhaven.cc"));
        assert!(!is_wallhaven_url("https://example.com/search?q=x"));
        assert!(!is_wallhaven_url(""));
    }

    #[test]
    fn query_parameters_are_replaced_or_appended() {
        let mut url = "https://w/api?q=a&page=1".to_string();
        set_or_append_param(&mut url, "page", "2");
        assert_eq!(url, "https://w/api?q=a&page=2");

        let mut url = "https://w/api?page=1&q=a".to_string();
        set_or_append_param(&mut url, "page", "3");
        assert_eq!(url, "https://w/api?page=3&q=a");

        let mut url = "https://w/api?q=a".to_string();
        set_or_append_param(&mut url, "purity", "100");
        assert_eq!(url, "https://w/api?q=a&purity=100");

        let mut url = "https://w/api".to_string();
        set_or_append_param(&mut url, "page", "1");
        assert_eq!(url, "https://w/api?page=1");
    }

    #[test]
    fn query_parameters_match_whole_names_only() {
        let mut url = "https://w/api?per_page=24".to_string();
        set_or_append_param(&mut url, "page", "2");
        assert_eq!(url, "https://w/api?per_page=24&page=2");
    }

    #[test]
    fn api_urls_are_completed_with_scheme_purity_page_and_key() {
        assert_eq!(
            build_api_url("wallhaven.cc/api/v1/search?q=a", 1, "100").unwrap(),
            "https://wallhaven.cc/api/v1/search?q=a&purity=100&page=1"
        );
        assert_eq!(
            build_api_url("https://wallhaven.cc/search?", 2, "110").unwrap(),
            "https://wallhaven.cc/api/v1/search?purity=110&page=2"
        );
        assert_eq!(
            build_api_url("https://wallhaven.cc/api/v1/search?apikey=pasted&page=9", 1, "111").unwrap(),
            "https://wallhaven.cc/api/v1/search?apikey=pasted&page=1&purity=111"
        );
    }

    #[test]
    fn downloaded_file_names_are_sanitized() {
        assert_eq!(sanitize_filename("wallhaven-abc123.jpg"), "wallhaven-abc123.jpg");
        assert_eq!(sanitize_filename("a b?c/..\\d.png"), "a_b_c_.._d.png");
        assert_eq!(sanitize_filename(""), "image.jpg");
    }

    #[test]
    fn the_query_signature_changes_with_everything_that_affects_results() {
        let s = settings();
        let base = source("https://wallhaven.cc/search?q=x");
        let sig = query_signature(&base, &s);
        assert_eq!(sig, query_signature(&source("  https://wallhaven.cc/search?q=x  "), &s), "whitespace is ignored");

        let mut purity = base.clone();
        purity.wallhaven_purity = "111".into();
        let mut pages = base.clone();
        pages.wallhaven_page_limit = 0;
        let mut keyed = s.clone();
        keyed.use_wallhaven_api_key = true;
        keyed.wallhaven_api_key = "k".into();

        assert_ne!(query_signature(&purity, &s), sig);
        assert_ne!(query_signature(&pages, &s), sig);
        assert_ne!(query_signature(&base, &keyed), sig);
        assert!(!query_signature(&base, &keyed).contains("=k|") && !query_signature(&base, &keyed).ends_with("=k"),
            "the API key itself must not be stored in the manifest");
    }

    #[test]
    fn empty_purity_falls_back_to_sfw_and_sketchy() {
        assert_eq!(effective_purity(&source("x")), "110");
        let mut s = source("x");
        s.wallhaven_purity = "100".into();
        assert_eq!(effective_purity(&s), "100");
    }

    #[test]
    fn legacy_folder_names_are_sixteen_hex_digits() {
        let name = legacy_cache_folder_name("https://wallhaven.cc/search?q=x");
        assert_eq!(name.len(), 16);
        assert!(name.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(name, cache_folder_name("https://wallhaven.cc/search?q=x"));
    }

    #[test]
    fn redact_handles_multiple_keys_and_non_ascii_text() {
        assert_eq!(redact("a apikey=1&b apikey=2"), "a apikey=***&b apikey=***");
        assert_eq!(redact("žluťoučký apikey=abc \"x\""), "žluťoučký apikey=*** \"x\"");
        assert_eq!(redact("apikey="), "apikey=***");
    }

    #[test]
    fn cache_only_never_touches_the_network() {
        // An unreachable host would make a refresh fail; CacheOnly must not even try.
        let src = source("https://wallhaven.cc/search?q=mula-unit-test-cache-only");
        let files = images_for_source(&src, &settings(), NetworkPolicy::CacheOnly).unwrap();
        assert!(files.is_empty());
        let dir = super::super::settings::config_dir().unwrap().join("wallhaven").join(cache_folder_name(src.path.trim()));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_refresh_without_any_successful_download_keeps_the_old_manifest() {
        let dir = crate::test_support::TempDir::new("refresh_fail");
        let old = Manifest { query: "q".into(), fetched_at: 1, files: vec!["old.jpg".into()] };
        write_manifest(dir.path(), &old).unwrap();

        assert!(record_refresh(dir.path(), "q".into(), 3, Vec::new()).is_err());
        let kept = read_manifest(dir.path()).unwrap();
        assert_eq!((kept.fetched_at, kept.files), (1, vec!["old.jpg".to_string()]));
    }

    #[test]
    fn a_successful_or_empty_search_refresh_is_recorded() {
        let dir = crate::test_support::TempDir::new("refresh_ok");
        let files = record_refresh(dir.path(), "q".into(), 2, vec![dir.join("a.jpg")]).unwrap();
        assert_eq!(files, vec![dir.join("a.jpg")]);
        assert_eq!(read_manifest(dir.path()).unwrap().files, vec!["a.jpg".to_string()]);

        // A search that genuinely has no results is a valid (empty) result.
        assert!(record_refresh(dir.path(), "q2".into(), 0, Vec::new()).unwrap().is_empty());
        assert_eq!(read_manifest(dir.path()).unwrap().query, "q2");
    }

    #[test]
    fn manifests_round_trip() {
        let dir = crate::test_support::TempDir::new("manifest");
        assert!(read_manifest(dir.path()).is_none());
        let manifest = Manifest { query: "q".into(), fetched_at: 42, files: vec!["a.jpg".into()] };
        write_manifest(dir.path(), &manifest).unwrap();
        let back = read_manifest(dir.path()).unwrap();
        assert_eq!((back.query.as_str(), back.fetched_at, back.files), ("q", 42, vec!["a.jpg".to_string()]));
        assert!(!dir.join("manifest.json.tmp").exists());
    }

    #[test]
    fn only_decodable_files_count_as_valid_images() {
        let dir = crate::test_support::TempDir::new("valid_image");
        let good = crate::test_support::write_png(&dir.join("good.png"), 3, 2);
        std::fs::write(dir.join("bad.jpg"), b"<html>rate limited</html>").unwrap();
        assert!(is_valid_image(&good));
        assert!(!is_valid_image(&dir.join("bad.jpg")));
        assert!(!is_valid_image(&dir.join("missing.jpg")));
    }
}
