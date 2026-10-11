//! Small helpers shared across the backend.

use std::fs;
use std::path::{Path, PathBuf};

/// MULA's data directory, e.g. `%APPDATA%\mula`.
pub fn mula_config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|p| p.join("mula"))
}

/// Runs blocking work (processes, disk, COM, network) off the UI thread. Tauri runs
/// synchronous commands on the main thread, which would freeze the window.
pub async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("Background task failed: {e}"))?
}

/// [`blocking`], logging a failure under `name`.
pub async fn logged_blocking<T: Send + 'static>(
    name: &'static str,
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let result = blocking(f).await;
    if let Err(e) = &result {
        log::error!("{name} failed: {e}");
    }
    result
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
    fn the_config_dir_is_named_mula() {
        assert!(mula_config_dir().unwrap().ends_with("mula"));
    }
}
