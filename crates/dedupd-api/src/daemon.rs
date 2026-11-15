//! The daemon itself: what it holds, its settings, its background work and
//! the event stream.

use crate::app::App;
use crate::jobs::Downloads;
use crate::moves::ModelMove;
use serde::{Deserialize, Serialize, Serializer};

/// Where the daemon listens unless `listen` in its `config.toml` says
/// otherwise.
pub const DEFAULT_ADDRESS: &str = "127.0.0.1:8642";

/// What an app holds altogether.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct AppUsage {
    pub app: App,
    pub files: u64,
    pub bytes: u64,
}

/// One model in the totals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Repository {
    pub provider: String,
    pub name: String,
    /// The commit `main` pointed at when the provider was last asked.
    pub commit: Option<String>,
    /// Files of the kept revisions that are in the store.
    pub stored_files: u64,
    pub stored_bytes: u64,
    /// App files that hold this model's content.
    pub files: u64,
    pub bytes: u64,
    pub verified: u64,
    pub mismatch: u64,
    pub not_upstream: u64,
    pub unchecked: u64,
    /// The owner archived the model: its files are on the archive disk only.
    pub archived: bool,
}

/// The daemon as a process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DaemonInfo {
    pub pid: u32,
    pub started_at: i64,
    pub listen: String,
    pub executable: Option<String>,
    /// The launchd label it runs under; `None` when it was started by hand.
    pub managed_by: Option<String>,
    /// A Hugging Face token is sent with requests.
    pub token: bool,
    /// Where the token comes from: a variable's name or a file's path.
    pub token_source: Option<String>,
    pub max_connections: u64,
    pub speed_limit: u64,
    /// The whole store is being checked.
    pub checking: bool,
    pub last_check_at: Option<i64>,
}

/// What the daemon holds and what it is doing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DaemonStatus {
    pub version: String,
    pub home: String,
    pub endpoint: String,
    /// The archive folder, when one is configured.
    pub archive_path: Option<String>,
    pub archive_attached: bool,
    pub store_files: u64,
    pub store_bytes: u64,
    /// Bytes of app files that share disk blocks with the store.
    pub shared_bytes: u64,
    /// Bytes of the downloads in flight, and how many have arrived.
    pub downloading_total: u64,
    pub downloading_done: u64,
    pub apps: Vec<AppUsage>,
    pub repositories: Vec<Repository>,
    pub daemon: DaemonInfo,
}

/// What one pass over the app folders did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct ScanResult {
    /// App files that no longer exist and were dropped from the records.
    pub forgotten: u64,
    /// App files brought into the store.
    pub adopted: u64,
    pub adopted_bytes: u64,
    /// Duplicate app files swapped for clones.
    pub reclaimed: u64,
    pub reclaimed_bytes: u64,
    /// Files copied to the archive disk.
    pub archived: u64,
    pub errors: Vec<String>,
}

/// The check of the whole store: the one that is running, or what the last
/// one found.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct StoreCheck {
    pub running: bool,
    /// Seconds since the epoch.
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub checked: u64,
    pub bytes: u64,
    /// sha256 of the files whose content no longer matches their name.
    pub damaged: Vec<String>,
    /// sha256 of the files that are in neither the store nor the archive.
    pub missing: Vec<String>,
    pub only_in_archive: u64,
    pub error: Option<String>,
}

/// The settings in the daemon's config file, defaults filled in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DaemonSettings {
    pub path: String,
    pub listen: String,
    pub hf_endpoint: String,
    pub archive: Option<String>,
    pub bionic_models: String,
    pub ollama_models: String,
    pub hf_cache: String,
    pub reclaim: bool,
    pub set_env: bool,
    pub verify_days: u64,
    pub scan_interval: u64,
    pub max_connections: u64,
    pub speed_limit: u64,
    /// The running daemon still uses older settings.
    pub restart_needed: bool,
}

/// A setting, by its name in `config.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum Setting {
    Listen,
    HfEndpoint,
    Archive,
    BionicModels,
    OllamaModels,
    HfCache,
    Reclaim,
    SetEnv,
    VerifyDays,
    ScanInterval,
    MaxConnections,
    SpeedLimit,
}

impl Setting {
    /// The name in `config.toml`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Setting::Listen => "listen",
            Setting::HfEndpoint => "hf_endpoint",
            Setting::Archive => "archive",
            Setting::BionicModels => "bionic_models",
            Setting::OllamaModels => "ollama_models",
            Setting::HfCache => "hf_cache",
            Setting::Reclaim => "reclaim",
            Setting::SetEnv => "set_env",
            Setting::VerifyDays => "verify_days",
            Setting::ScanInterval => "scan_interval",
            Setting::MaxConnections => "max_connections",
            Setting::SpeedLimit => "speed_limit",
        }
    }
}

/// A setting's new value. On the wire it is the plain JSON value, and
/// `null` for `Unset`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum SettingValue {
    Text {
        value: String,
    },
    Number {
        value: u64,
    },
    Flag {
        value: bool,
    },
    /// Remove the setting, so its default applies.
    Unset,
}

impl Serialize for SettingValue {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            SettingValue::Text { value } => s.serialize_str(value),
            SettingValue::Number { value } => s.serialize_u64(*value),
            SettingValue::Flag { value } => s.serialize_bool(*value),
            SettingValue::Unset => s.serialize_none(),
        }
    }
}

/// A setting to change.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SettingChange {
    pub key: Setting,
    pub value: SettingValue,
}

/// The last lines of the daemon's log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct LogTail {
    pub path: String,
    pub lines: Vec<String>,
}

/// One line of the daemon's event stream: everything that changes often.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct DaemonEvent {
    pub seq: u64,
    /// Goes up when what the store or the apps hold has changed.
    pub library: u64,
    /// The whole store is being checked.
    pub checking: bool,
    /// The archive or restore of one model that is running, or the last one.
    pub moving: Option<ModelMove>,
    pub downloads: Downloads,
}

/// The answer to a request that failed, with why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub error: String,
}

/// The answer to a request that starts something in the background.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Started {
    pub started: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn a_change_is_sent_as_the_plain_value() {
        let changes: BTreeMap<Setting, SettingValue> = [
            (Setting::Listen, SettingValue::Text { value: "a".into() }),
            (Setting::VerifyDays, SettingValue::Number { value: 3 }),
            (Setting::Reclaim, SettingValue::Flag { value: false }),
            (Setting::Archive, SettingValue::Unset),
        ]
        .into();
        assert_eq!(
            serde_json::to_string(&changes).unwrap(),
            r#"{"listen":"a","archive":null,"reclaim":false,"verify_days":3}"#
        );
    }

    #[test]
    fn a_setting_is_named_as_in_the_config_file() {
        let read: Setting = serde_json::from_str("\"max_connections\"").unwrap();
        assert_eq!(read.as_str(), "max_connections");
    }
}
