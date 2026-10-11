//! The error commands return to the frontend: a tagged object (`{ "kind": ..., "message"?: ... }`)
//! the UI can switch on, instead of looking for words in error text.

use serde::Serialize;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandError {
    /// smartmontools isn't installed; the UI offers to install it.
    SmartctlMissing,
    /// The operation needs administrator rights; the UI offers to retry elevated.
    NeedsElevation { message: String },
    /// The user declined the UAC prompt.
    ElevationDeclined,
    Other { message: String },
}

/// What every Tauri command returns.
pub type CmdResult<T> = Result<T, CommandError>;

impl From<String> for CommandError {
    fn from(message: String) -> Self {
        Self::Other { message }
    }
}

impl From<&str> for CommandError {
    fn from(message: &str) -> Self {
        Self::Other { message: message.to_string() }
    }
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SmartctlMissing => f.write_str("smartmontools (smartctl) is not installed"),
            Self::NeedsElevation { message } | Self::Other { message } => f.write_str(message),
            Self::ElevationDeclined => f.write_str("Administrator rights were declined"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_serialize_as_tagged_objects() {
        assert_eq!(serde_json::to_value(CommandError::SmartctlMissing).unwrap(), serde_json::json!({ "kind": "smartctl_missing" }));
        assert_eq!(
            serde_json::to_value(CommandError::NeedsElevation { message: "open failed".into() }).unwrap(),
            serde_json::json!({ "kind": "needs_elevation", "message": "open failed" })
        );
        assert_eq!(serde_json::to_value(CommandError::from("boom")).unwrap(), serde_json::json!({ "kind": "other", "message": "boom" }));
    }

    #[test]
    fn errors_display_their_message() {
        assert_eq!(CommandError::from("boom").to_string(), "boom");
        assert_eq!(CommandError::ElevationDeclined.to_string(), "Administrator rights were declined");
    }
}
