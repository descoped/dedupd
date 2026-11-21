//! Taking files away: deactivating a model in an app, archiving it to the
//! archive disk and restoring it, and cleaning up stored files. Each is
//! first a plan the owner looks at, then done.

use crate::app::App;
use serde::{Deserialize, Serialize};

/// An app's file that would be removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PlannedFile {
    pub path: String,
    pub size: u64,
}

/// An app's file that would be left alone, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct KeptFile {
    pub path: String,
    pub why: String,
}

/// What taking a model out of one app would do, or did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct UnplacePlan {
    pub repo: String,
    pub app: App,
    pub remove: Vec<PlannedFile>,
    /// Links and manifests that go with the files.
    pub links: Vec<String>,
    pub keep: Vec<KeptFile>,
    pub bytes: u64,
    /// Files removed; `None` when this is only the plan.
    pub removed: Option<u64>,
}

/// What a request to take a model out of an app asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnplaceBody {
    pub repo: String,
    pub app: App,
    /// Remove the files. Without it the answer only says what would go.
    #[serde(default)]
    pub apply: bool,
    /// The files the owner agreed to; nothing else is removed.
    pub paths: Option<Vec<String>>,
}

/// Archive or restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum MoveKind {
    Archive,
    Restore,
}

crate::all_variants!(MoveKind: Archive, Restore);

impl MoveKind {
    /// The kind's word in the control API, which is also its path.
    pub const fn as_str(self) -> &'static str {
        match self {
            MoveKind::Archive => "archive",
            MoveKind::Restore => "restore",
        }
    }
}

/// What a request to archive or restore a model asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoveBody {
    pub repo: String,
    /// For an archive: the name of the archive to move it to. Without it,
    /// the backup archive, or the only archive there is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Do it. Without it the answer only says what would happen.
    #[serde(default)]
    pub apply: bool,
}

/// One model on its way to the archive disk or back: the one that is
/// running, or the last one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct ModelMove {
    pub kind: MoveKind,
    pub repo: String,
    /// For an archive: the name of the archive it goes to.
    #[serde(default)]
    pub archive: Option<String>,
    pub running: bool,
    /// Seconds since the epoch.
    pub started_at: i64,
    pub finished_at: Option<i64>,
    /// The size of the files to move, and of those dealt with so far: for
    /// an archive the copies checked on the archive disk, for a restore the
    /// files copied back.
    pub bytes_total: u64,
    pub bytes_done: u64,
    /// Once finished: the files that left or came back to the internal disk.
    pub files: u64,
    /// Once an archive has finished: stored files that stayed on the
    /// internal disk, because another model or an app still uses them.
    pub kept: u64,
    pub error: Option<String>,
}

/// What archiving would take out of one app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DeactivateStep {
    pub app: App,
    pub files: u64,
    pub bytes: u64,
    /// The app's files that would be left alone, and why.
    pub keep: Vec<KeptFile>,
}

/// A stored file that archiving leaves on the internal disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct KeptStored {
    pub sha256: String,
    pub size: u64,
    pub why: String,
}

/// What archiving a model would do: take it out of every app, then remove
/// its files from the internal disk once the archive disk has a good copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct ArchivePlan {
    pub repo: String,
    /// The archive's folder and its name.
    pub archive: String,
    pub archive_name: String,
    pub deactivate: Vec<DeactivateStep>,
    /// Stored files that would leave the internal disk, and their size.
    pub files: u64,
    pub bytes: u64,
    /// Of those, the files the archive disk lacks; they are copied first.
    pub copy_files: u64,
    pub copy_bytes: u64,
    pub keep: Vec<KeptStored>,
}

/// What restoring a model would copy back from the archive disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct RestorePlan {
    pub repo: String,
    /// The folder of the first archive files come from, and the names of
    /// every archive they come from.
    pub archive: String,
    pub archives: Vec<String>,
    pub files: u64,
    pub bytes: u64,
    /// Free space on the internal disk.
    pub free: u64,
    /// Files that are on neither disk.
    pub missing: u64,
}

/// A stored file, by its content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct StoredFile {
    pub sha256: String,
    pub size: u64,
    pub git_oid: Option<String>,
    /// A model that lists the file and its path there, as `org/repo: path`,
    /// so a person can tell what it is. None for a file no kept model
    /// lists any more.
    #[serde(default)]
    pub known_as: Option<String>,
}

/// Stored files that could be removed, or were.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct GcPlan {
    /// Files nothing refers to.
    pub orphans: Vec<StoredFile>,
    /// Files that are on the archive disk and in no app.
    pub offload: Vec<StoredFile>,
    pub orphan_bytes: u64,
    pub offload_bytes: u64,
    /// Bytes freed; `None` when this is only the plan.
    pub freed: Option<u64>,
}

/// What a request to clean up stored files agrees to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GcBody {
    /// sha256 of the files nothing refers to that the owner agreed to remove.
    #[serde(default)]
    pub orphans: Vec<String>,
    /// sha256 of the files whose local copy the owner agreed to remove.
    #[serde(default)]
    pub offload: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_word_is_spelled_one_way() {
        crate::same_words(&MoveKind::ALL, MoveKind::as_str);
    }

    #[test]
    fn a_request_with_a_misspelt_field_is_refused() {
        // `aply` must not read as "only tell me the plan".
        let misspelt = r#"{"repo": "org/repo", "aply": true}"#;
        assert!(serde_json::from_str::<MoveBody>(misspelt).is_err());
        assert!(serde_json::from_str::<MoveBody>(r#"{"repo": "org/repo"}"#).is_ok());
        assert!(serde_json::from_str::<GcBody>(r#"{"orphan": []}"#).is_err());
        let unplace = r#"{"repo": "org/repo", "app": "ollama", "path": []}"#;
        assert!(serde_json::from_str::<UnplaceBody>(unplace).is_err());
    }
}
