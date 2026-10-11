//! Small helpers shared across the backend.

use std::fs;
use std::path::{Path, PathBuf};

/// MULA's data directory, e.g. `%APPDATA%\mula`.
pub fn mula_config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|p| p.join("mula"))
}

/// Runs blocking work (processes, disk, COM, network) off the UI thread. Tauri runs
/// synchronous commands on the main thread, which would freeze the window.
pub async fn blocking<T: Send + 'static, E: From<String> + Send + 'static>(
    f: impl FnOnce() -> Result<T, E> + Send + 'static,
) -> Result<T, E> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| Err(E::from(format!("Background task failed: {e}"))))
}

/// [`blocking`], logging a failure under `name`.
pub async fn logged_blocking<T: Send + 'static, E: From<String> + std::fmt::Display + Send + 'static>(
    name: &'static str,
    f: impl FnOnce() -> Result<T, E> + Send + 'static,
) -> Result<T, E> {
    let result = blocking(f).await;
    if let Err(e) = &result {
        log::error!("{name} failed: {e}");
    }
    result
}

/// `bytes` random bytes as lowercase hex (for per-session secrets).
pub fn random_hex(bytes: usize) -> String {
    use rand::RngCore;
    let mut buffer = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buffer);
    buffer.iter().map(|b| format!("{b:02x}")).collect()
}

/// SHA-256 of `bytes` as uppercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02X}")).collect()
}

/// Makes `value` safe as a file-name component: ASCII letters and digits, plus any character
/// in `keep`, are kept; everything else becomes `_`.
pub fn safe_file_name(value: &str, keep: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || keep.contains(c) { c } else { '_' })
        .collect()
}

/// 64-bit FNV-1a: a hash that, unlike `DefaultHasher`, is stable across builds, so it can name
/// cache files and folders.
pub fn stable_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Reads a JSON file; a missing file yields `T::default()`. A file that can't be parsed (e.g.
/// from an older format) is moved aside to `<name>.bak` so the app keeps working. Any other
/// read error (e.g. the file is briefly locked by a virus scanner) is returned, so callers that
/// read-modify-write never save defaults over real data.
pub fn read_json_or_default<T: Default + serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(e) => return Err(format!("Failed to read {}: {e}", path.display())),
    };
    match serde_json::from_str(&text) {
        Ok(value) => Ok(value),
        Err(e) => {
            let mut backup = path.as_os_str().to_owned();
            backup.push(".bak");
            log::warn!("Unreadable {} ({e}); keeping it as {:?} and starting fresh", path.display(), backup);
            let _ = fs::rename(path, &backup);
            Ok(T::default())
        }
    }
}

/// Writes `value` as pretty JSON, atomically.
pub fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| format!("Failed to serialize {}: {e}", path.display()))?;
    write_atomic(path, &text)
}

/// Writes via a temporary file and a rename, so a crash mid-write never leaves a truncated file.
pub fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, text).map_err(|e| format!("Failed to write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("Failed to replace {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn atomic_writes_create_folders_replace_files_and_leave_no_temp_file() {
        let dir = TempDir::new("util_atomic");
        let path = dir.join("sub").join("vsd.conf");
        write_atomic(&path, "A=1\n").unwrap();
        write_atomic(&path, "A=2\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "A=2\n");
        assert!(!dir.join("sub").join("vsd.conf.tmp").exists());
    }

    #[test]
    fn file_names_keep_only_safe_characters() {
        assert_eq!(safe_file_name(r"\\.\PHYSICALDRIVE0", "-_"), "____PHYSICALDRIVE0");
        assert_eq!(safe_file_name(r"\\?\DISPLAY#GSM5B7F#5&1a2b", ""), "____DISPLAY_GSM5B7F_5_1a2b");
        assert_eq!(safe_file_name("a b?c/..\\d.png", "._-"), "a_b_c_.._d.png");
        assert_eq!(safe_file_name("žluť", ""), "_lu_", "non-ASCII letters are replaced too");
    }

    #[test]
    fn the_stable_hash_is_fnv1a() {
        assert_eq!(stable_hash(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(stable_hash(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_ne!(stable_hash(b"a"), stable_hash(b"b"));
    }

    #[test]
    fn unreadable_json_files_are_moved_aside() {
        let dir = TempDir::new("util_json");
        let path = dir.join("settings.json");
        fs::write(&path, "{ not json").unwrap();
        let value: std::collections::HashMap<String, i32> = read_json_or_default(&path).unwrap();
        assert!(value.is_empty());
        assert!(!path.exists());
        assert_eq!(fs::read_to_string(dir.join("settings.json.bak")).unwrap(), "{ not json");

        write_json(&path, &std::collections::HashMap::from([("a", 1)])).unwrap();
        let back: std::collections::HashMap<String, i32> = read_json_or_default(&path).unwrap();
        assert_eq!(back.get("a"), Some(&1));
    }

    #[test]
    fn missing_files_give_defaults_but_other_read_errors_are_errors() {
        let dir = TempDir::new("util_json_errors");
        let missing: std::collections::HashMap<String, i32> = read_json_or_default(&dir.join("nope.json")).unwrap();
        assert!(missing.is_empty());
        // A directory can't be read as a file: that must not silently become "defaults".
        assert!(read_json_or_default::<std::collections::HashMap<String, i32>>(dir.path()).is_err());
        assert!(dir.path().exists(), "nothing is moved aside on read errors");
    }

    #[test]
    fn random_hex_and_sha256_hex() {
        let (a, b) = (random_hex(24), random_hex(24));
        assert_eq!(a.len(), 48);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        assert_eq!(sha256_hex(b"abc"), "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD");
    }

    #[test]
    fn the_config_dir_is_named_mula() {
        assert!(mula_config_dir().unwrap().ends_with("mula"));
    }
}
