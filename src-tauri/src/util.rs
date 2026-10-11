//! Small helpers shared across the backend.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// MULA's data directory, e.g. `%APPDATA%\mula`.
pub fn mula_config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|p| p.join("mula"))
}

/// A path inside MULA's data directory.
pub fn mula_path(relative: &str) -> Result<PathBuf, String> {
    Ok(mula_config_dir().ok_or("Could not determine config directory")?.join(relative))
}

/// Locks `m`, recovering the data if another thread panicked while holding it: MULA's locks
/// guard plain data that stays consistent between operations.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Runs `command` and returns its stdout. A failure to start or a non-zero exit becomes an
/// error naming `what`, with the command's stderr (or stdout, which some tools use for errors).
pub fn run_command(command: &mut Command, what: &str) -> Result<String, String> {
    let output = command.output().map_err(|e| format!("Failed to run {what}: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = if stderr.trim().is_empty() { stdout.trim() } else { stderr.trim() };
        return Err(format!("{what} failed: {detail}"));
    }
    Ok(stdout)
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

/// How a [`JsonStore`] writes its file.
#[derive(Clone, Copy)]
pub enum JsonFormat {
    /// Indented, for files people may read.
    Pretty,
    /// One line, for large machine-only files.
    Compact,
}

/// A JSON file under MULA's data directory, kept in memory: it is read on first use (missing:
/// default; unreadable: moved aside, default) and then served from memory. A change is written
/// to disk first and only adopted when the write succeeded, so memory never runs ahead of the
/// file. Edits made to the file by hand while MULA runs are not picked up.
pub struct JsonStore<T> {
    relative_path: &'static str,
    format: JsonFormat,
    /// Fixes up freshly read values (e.g. clamping); not applied to values the app stores.
    on_load: fn(&mut T),
    path_override: Option<PathBuf>,
    value: Mutex<Option<T>>,
}

impl<T: Default + Clone + serde::Serialize + serde::de::DeserializeOwned> JsonStore<T> {
    pub const fn new(relative_path: &'static str, format: JsonFormat, on_load: fn(&mut T)) -> Self {
        Self { relative_path, format, on_load, path_override: None, value: Mutex::new(None) }
    }

    /// A store for a file at an explicit path (tests).
    #[cfg(test)]
    pub fn at(path: PathBuf, format: JsonFormat, on_load: fn(&mut T)) -> Self {
        Self { relative_path: "", format, on_load, path_override: Some(path), value: Mutex::new(None) }
    }

    fn path(&self) -> Result<PathBuf, String> {
        match &self.path_override {
            Some(path) => Ok(path.clone()),
            None => mula_path(self.relative_path),
        }
    }

    fn loaded<'a>(&self, slot: &'a mut Option<T>) -> Result<&'a mut T, String> {
        if slot.is_none() {
            let mut value = read_json_or_default(&self.path()?)?;
            (self.on_load)(&mut value);
            *slot = Some(value);
        }
        Ok(slot.as_mut().expect("just loaded"))
    }

    fn write(&self, value: &T) -> Result<(), String> {
        let path = self.path()?;
        let text = match self.format {
            JsonFormat::Pretty => serde_json::to_string_pretty(value),
            JsonFormat::Compact => serde_json::to_string(value),
        }
        .map_err(|e| format!("Failed to serialize {}: {e}", path.display()))?;
        write_atomic(&path, &text)
    }

    /// Runs `f` on the current value.
    pub fn read<R>(&self, f: impl FnOnce(&T) -> R) -> Result<R, String> {
        let mut slot = lock(&self.value);
        Ok(f(self.loaded(&mut slot)?))
    }

    /// A copy of the current value.
    pub fn get(&self) -> Result<T, String> {
        self.read(T::clone)
    }

    /// Changes the value through `f` and saves it; nothing changes when `f` fails or the file
    /// can't be written. Other readers and writers wait until it is done.
    pub fn update<R>(&self, f: impl FnOnce(&mut T) -> Result<R, String>) -> Result<R, String> {
        let mut slot = lock(&self.value);
        let current = self.loaded(&mut slot)?;
        let mut next = current.clone();
        let result = f(&mut next)?;
        self.write(&next)?;
        *current = next;
        Ok(result)
    }

    /// Replaces the value and saves it (kept only when the write succeeded).
    pub fn set(&self, value: T) -> Result<(), String> {
        let mut slot = lock(&self.value);
        self.write(&value)?;
        *slot = Some(value);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Doc {
        n: i32,
    }

    fn clamp_doc(doc: &mut Doc) {
        doc.n = doc.n.min(10);
    }

    #[test]
    fn stores_read_the_file_once_and_serve_later_reads_from_memory() {
        let dir = TempDir::new("store_read");
        let path = dir.join("doc.json");
        fs::write(&path, r#"{"n":42}"#).unwrap();
        let store = JsonStore::<Doc>::at(path.clone(), JsonFormat::Pretty, clamp_doc);

        assert_eq!(store.get().unwrap(), Doc { n: 10 }, "freshly read values are fixed up");
        fs::write(&path, r#"{"n":1}"#).unwrap();
        assert_eq!(store.read(|d| d.n).unwrap(), 10, "the file is not read again");
    }

    #[test]
    fn missing_files_start_from_the_default() {
        let dir = TempDir::new("store_missing");
        let store = JsonStore::<Doc>::at(dir.join("none.json"), JsonFormat::Pretty, clamp_doc);
        assert_eq!(store.get().unwrap(), Doc::default());
        assert!(!dir.join("none.json").exists(), "reading never creates the file");
    }

    #[test]
    fn updates_and_sets_are_written_through() {
        let dir = TempDir::new("store_write");
        let path = dir.join("sub").join("doc.json");
        let store = JsonStore::<Doc>::at(path.clone(), JsonFormat::Compact, clamp_doc);

        assert_eq!(store.update(|d| { d.n = 50; Ok(d.n * 2) }).unwrap(), 100);
        assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"n":50}"#, "stored values are not re-clamped");
        assert_eq!(store.get().unwrap().n, 50);

        store.set(Doc { n: 7 }).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"n":7}"#);
        assert_eq!(store.get().unwrap().n, 7);
    }

    #[test]
    fn failed_changes_leave_memory_and_file_untouched() {
        let dir = TempDir::new("store_fail");
        let path = dir.join("doc.json");
        let store = JsonStore::<Doc>::at(path.clone(), JsonFormat::Compact, clamp_doc);
        store.set(Doc { n: 3 }).unwrap();

        assert!(store.update(|d| { d.n = 9; Err::<(), _>("nope".to_string()) }).is_err());
        assert_eq!(store.get().unwrap().n, 3);

        // A directory in the file's place makes the write fail.
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(store.update(|d| { d.n = 9; Ok(()) }).is_err());
        assert!(store.set(Doc { n: 9 }).is_err());
        assert_eq!(store.get().unwrap().n, 3, "memory never runs ahead of the file");
    }

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
    fn commands_return_stdout_or_a_named_error() {
        let ok = run_command(Command::new("cmd").args(["/C", "echo hello"]), "echo").unwrap();
        assert_eq!(ok.trim(), "hello");
        let err = run_command(Command::new("cmd").args(["/C", "echo broken 1>&2 & exit 3"]), "the tool").unwrap_err();
        assert_eq!(err, "the tool failed: broken");
        assert!(run_command(&mut Command::new("mula-no-such-program"), "x").unwrap_err().starts_with("Failed to run x"));
    }

    #[test]
    fn poisoned_locks_are_recovered() {
        let m = std::sync::Arc::new(Mutex::new(1));
        let poisoner = m.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("poison");
        })
        .join();
        assert!(m.is_poisoned());
        *lock(&m) += 1;
        assert_eq!(*lock(&m), 2);
    }

    #[test]
    fn the_config_dir_is_named_mula() {
        assert!(mula_config_dir().unwrap().ends_with("mula"));
    }
}
