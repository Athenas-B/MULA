//! Backend → UI events: one app handle, set at startup, and the event names in one place.

use serde::Serialize;
use std::sync::OnceLock;
use tauri::{AppHandle, Emitter};

pub const WC_SETTINGS_CHANGED: &str = "wc-settings-changed";
pub const WC_SERVICE_CHANGED: &str = "wc-service-changed";
pub const WC_WALLPAPER_CHANGED: &str = "wc-wallpaper-changed";
pub const VSD_LOG: &str = "vsd-log";
pub const VSD_STATE_CHANGED: &str = "vsd-state-changed";
pub const AUTOSTART_CHANGED: &str = "autostart-changed";

static APP: OnceLock<AppHandle> = OnceLock::new();

pub fn init(app: AppHandle) {
    let _ = APP.set(app);
}

/// Sends `event` to the UI; a no-op before [`init`] (e.g. in unit tests).
pub fn emit<S: Serialize + Clone>(event: &str, payload: S) {
    if let Some(app) = APP.get() {
        let _ = app.emit(event, payload);
    }
}
