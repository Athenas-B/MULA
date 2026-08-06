//! Manages launching MULA automatically when the user logs into Windows,
//! mirroring the "Run at Windows startup" option from the original Wall Changer app.

use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
use winreg::RegKey;

const RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE_NAME: &str = "MULA";

/// The argument passed to MULA when it is launched by the Windows startup entry,
/// so the app knows to start minimized to the tray instead of showing its window.
pub const AUTOSTART_ARG: &str = "--minimized";

fn current_exe_command() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("Failed to resolve MULA executable path: {e}"))?;
    let exe = exe.to_string_lossy();
    Ok(format!("\"{exe}\" {AUTOSTART_ARG}"))
}

pub fn is_enabled() -> Result<bool, String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run_key = match hkcu.open_subkey_with_flags(RUN_KEY_PATH, KEY_READ) {
        Ok(key) => key,
        Err(_) => return Ok(false),
    };

    match run_key.get_value::<String, _>(RUN_VALUE_NAME) {
        Ok(_) => Ok(true),
        Err(_) => Ok(false),
    }
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run_key = hkcu
        .open_subkey_with_flags(RUN_KEY_PATH, KEY_WRITE)
        .map_err(|e| format!("Failed to open startup registry key: {e}"))?;

    if enabled {
        let command = current_exe_command()?;
        run_key
            .set_value(RUN_VALUE_NAME, &command)
            .map_err(|e| format!("Failed to add MULA to Windows startup: {e}"))?;
    } else {
        match run_key.delete_value(RUN_VALUE_NAME) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("Failed to remove MULA from Windows startup: {e}")),
        }
    }

    Ok(())
}
