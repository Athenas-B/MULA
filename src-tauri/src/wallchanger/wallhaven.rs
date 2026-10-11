//! Fetches wallpapers from Wallhaven search/API URLs, caches them locally, and
//! exposes the cached images to the rest of the Wall Changer pipeline.
//!
//! Search results are cached per source in a `manifest.json` and reused for [`RESULTS_TTL`],
//! so a wallpaper change normally does no network I/O at all. Stale results are refreshed on a
//! background thread while the cached images keep being used; only a source with nothing
//! cached yet is fetched synchronously. When a refresh fails (offline, rate limited, ...) the
//! previously downloaded images keep being used.

use super::images::NetworkPolicy;
use super::settings::Purity;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Parallel image downloads (the API's rate limit only applies to search requests).
const DOWNLOAD_WORKERS: usize = 4;
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

/// What to do for a source, given the state of its cache.
#[derive(Debug, PartialEq, Eq)]
enum Plan {
    UseCache,
    UseCacheAndRefreshInBackground,
    RefreshNow,
}

fn plan(fresh: bool, network: NetworkPolicy, has_cached_files: bool) -> Plan {
    match (fresh, network, has_cached_files) {
        (true, _, _) | (_, NetworkPolicy::CacheOnly, _) => Plan::UseCache,
        (false, NetworkPolicy::RefreshWhenStale, true) => Plan::UseCacheAndRefreshInBackground,
        (false, NetworkPolicy::RefreshWhenStale, false) => Plan::RefreshNow,
    }
}

/// Cache folders with a background refresh in flight.
static REFRESHING: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Default::default);

struct RefreshingGuard(PathBuf);

impl Drop for RefreshingGuard {
    fn drop(&mut self) {
        REFRESHING.lock().unwrap_or_else(|p| p.into_inner()).remove(&self.0);
    }
}

/// Everything a refresh needs, owned so it can run on another thread.
#[derive(Clone)]
struct RefreshJob {
    url: String,
    page_limit: i32,
    purity: Purity,
    api_key: Option<String>,
    cache_dir: PathBuf,
    query: String,
}

/// Returns the cached image files of a Wallhaven source. Stale search results are refreshed
/// in the background (or right away when nothing is cached yet), if `network` allows it.
pub fn images_for_source(
    url: &str,
    page_limit: i32,
    purity: Purity,
    api_key: Option<&str>,
    network: NetworkPolicy,
) -> Result<Vec<PathBuf>, String> {
    let url = url.trim();
    if url.is_empty() {
        return Ok(Vec::new());
    }

    let cache_dir = source_cache_dir(url)?;
    let query = query_signature(url, page_limit, purity, api_key.is_some());
    let manifest = read_manifest(&cache_dir);
    let fresh = manifest
        .as_ref()
        .is_some_and(|m| m.query == query && now_secs().saturating_sub(m.fetched_at) < RESULTS_TTL.as_secs());
    let cached = cached_files(&cache_dir, manifest.as_ref());

    let job = RefreshJob {
        url: url.to_string(),
        page_limit,
        purity,
        api_key: api_key.map(str::to_string),
        cache_dir,
        query,
    };
    match plan(fresh, network, !cached.is_empty()) {
        Plan::UseCache => Ok(cached),
        Plan::UseCacheAndRefreshInBackground => {
            refresh_in_background(job);
            Ok(cached)
        }
        Plan::RefreshNow => refresh(&job).map_err(|e| redact(&e)),
    }
}

fn refresh_in_background(job: RefreshJob) {
    {
        let mut running = REFRESHING.lock().unwrap_or_else(|p| p.into_inner());
        if !running.insert(job.cache_dir.clone()) {
            return;
        }
    }
    std::thread::spawn(move || {
        // Removes the in-flight mark however the refresh ends, including a panic.
        let _done = RefreshingGuard(job.cache_dir.clone());
        match refresh(&job) {
            Ok(files) => log::info!("Refreshed Wallhaven source ({} image(s))", files.len()),
            Err(e) => log::warn!("Wallhaven refresh failed; keeping cached images: {}", redact(&e)),
        }
    });
}

fn query_signature(url: &str, page_limit: i32, purity: Purity, has_api_key: bool) -> String {
    format!("{}|purity={}|pages={page_limit}|apikey={has_api_key}", url.trim(), purity.as_param())
}

fn refresh(job: &RefreshJob) -> Result<Vec<PathBuf>, String> {
    let page_limit = if job.page_limit == 0 { u32::MAX } else { job.page_limit.max(1) as u32 };
    let purity = job.purity.as_param();

    let agent = ureq::AgentBuilder::new().timeout(REQUEST_TIMEOUT).build();
    let mut image_urls = Vec::new();
    let mut seen = HashSet::new();

    for page in 1..=page_limit {
        if page > 1 {
            std::thread::sleep(API_REQUEST_SPACING);
        }

        let url = build_api_url(&job.url, page, &purity)?;
        let mut request = agent.get(&url);
        if let Some(key) = &job.api_key {
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

    let files = download_all(&agent, &image_urls, &job.cache_dir);
    record_refresh(&job.cache_dir, job.query.clone(), image_urls.len(), files)
}

/// Downloads `urls` with a few workers, returning the files in the order of `urls`.
fn download_all(agent: &ureq::Agent, urls: &[String], dir: &Path) -> Vec<PathBuf> {
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<PathBuf>>> = Mutex::new(vec![None; urls.len()]);
    std::thread::scope(|scope| {
        for _ in 0..DOWNLOAD_WORKERS.min(urls.len()) {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(url) = urls.get(i) else { break };
                match download_image(agent, url, dir) {
                    Ok(path) => results.lock().unwrap_or_else(|p| p.into_inner())[i] = Some(path),
                    Err(e) => log::warn!("Skipping Wallhaven image {url}: {e}"),
                }
            });
        }
    });
    results.into_inner().unwrap_or_else(|p| p.into_inner()).into_iter().flatten().collect()
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
    let dir = super::settings::config_dir()?.join("wallhaven").join(cache_folder_name(url));
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create Wallhaven cache directory: {e}"))?;
    Ok(dir)
}

fn cache_folder_name(url: &str) -> String {
    format!("fnv{:016x}", crate::util::stable_hash(url.as_bytes()))
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
    match crate::util::safe_file_name(name, "._-") {
        safe if safe.is_empty() => "image.jpg".to_string(),
        safe => safe,
    }
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

    #[test]
    fn stale_results_are_refreshed_in_the_background_unless_nothing_is_cached() {
        use NetworkPolicy::*;
        assert_eq!(plan(true, RefreshWhenStale, true), Plan::UseCache);
        assert_eq!(plan(false, CacheOnly, false), Plan::UseCache);
        assert_eq!(plan(false, RefreshWhenStale, true), Plan::UseCacheAndRefreshInBackground);
        assert_eq!(plan(false, RefreshWhenStale, false), Plan::RefreshNow);
    }

    #[test]
    fn the_refresh_mark_is_cleared_even_when_a_refresh_panics() {
        let dir = PathBuf::from("mula-test-refresh-guard");
        REFRESHING.lock().unwrap().insert(dir.clone());
        let joined = std::thread::spawn({
            let dir = dir.clone();
            move || {
                let _done = RefreshingGuard(dir);
                panic!("simulated refresh failure");
            }
        })
        .join();
        assert!(joined.is_err());
        assert!(!REFRESHING.lock().unwrap_or_else(|p| p.into_inner()).contains(&dir));
    }

    #[test]
    fn parallel_downloads_keep_the_url_order_and_skip_failures() {
        let dir = crate::test_support::TempDir::new("wh_download_all");
        // Already-downloaded valid files are reused without any network access.
        for name in ["a.png", "b.png", "c.png"] {
            crate::test_support::write_png(&dir.join(name), 2, 2);
        }
        let urls: Vec<String> = ["c.png", "a.png", "b.png"].iter().map(|n| format!("https://w.wallhaven.cc/full/{n}")).collect();
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_millis(1)).build();
        let files = download_all(&agent, &urls, dir.path());
        assert_eq!(files, vec![dir.join("c.png"), dir.join("a.png"), dir.join("b.png")]);

        // An unreachable URL is skipped rather than failing the batch.
        let mut with_bad = urls.clone();
        with_bad.insert(1, "http://127.0.0.1:9/missing.png".to_string());
        assert_eq!(download_all(&agent, &with_bad, dir.path()).len(), 3);
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
        let url = "https://wallhaven.cc/search?q=x";
        let sig = query_signature(url, 1, Purity::default(), false);
        assert_eq!(sig, query_signature("  https://wallhaven.cc/search?q=x  ", 1, Purity::default(), false));
        assert_ne!(query_signature(url, 1, Purity { nsfw: true, ..Purity::default() }, false), sig);
        assert_ne!(query_signature(url, 0, Purity::default(), false), sig);
        let keyed = query_signature(url, 1, Purity::default(), true);
        assert_ne!(keyed, sig);
        assert!(keyed.ends_with("apikey=true"), "only whether a key is used is recorded, never the key");
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
        let url = "https://wallhaven.cc/search?q=mula-unit-test-cache-only";
        let files = images_for_source(url, 1, Purity::default(), None, NetworkPolicy::CacheOnly).unwrap();
        assert!(files.is_empty());
        let dir = super::super::settings::config_dir().unwrap().join("wallhaven").join(cache_folder_name(url));
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
