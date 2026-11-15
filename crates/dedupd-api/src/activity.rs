//! The history of what happened to the models in the apps.

use crate::app::App;
use serde::{Deserialize, Serialize};

/// What was done to a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum ActivityAction {
    /// Put into an app.
    Activate,
    /// Taken out of an app.
    Deactivate,
    /// Taken out of every app and off the internal disk.
    Archive,
    /// Copied back from the archive disk.
    Restore,
}

/// One thing that happened to a model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct ActivityEntry {
    /// Grows with every entry, so a reader can tell which ones it has shown.
    pub id: i64,
    /// Seconds since the Unix epoch.
    pub at: i64,
    pub action: ActivityAction,
    /// `org/repo`.
    pub repo: String,
    /// The app, for an activation or a deactivation.
    pub app: Option<App>,
    pub ok: bool,
    /// What happened, in a sentence; for a failure, why.
    pub text: String,
}

/// The history, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityList {
    pub entries: Vec<ActivityEntry>,
}
