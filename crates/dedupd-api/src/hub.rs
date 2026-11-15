//! Looking around on the provider: search, and one repository before
//! anything is downloaded.

use crate::app::App;
use crate::library::{AppPresence, ModelMeta, Variant};
use serde::{Deserialize, Serialize};

/// One model of a search answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SearchHit {
    pub id: String,
    pub meta: ModelMeta,
    /// dedupd keeps this model.
    pub archived: bool,
    /// The apps that have it.
    pub apps: Vec<App>,
    pub downloading: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SearchPage {
    pub models: Vec<SearchHit>,
    /// Hand this back as `cursor` to get the next page.
    pub next: Option<String>,
}

/// What to search the provider for. Empty fields are not asked about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SearchQuery {
    pub text: String,
    /// `gguf`, `mlx`, `safetensors`, or empty for any.
    pub format: String,
    /// The provider's task name, for example `text-generation`.
    pub task: String,
    pub license: String,
    pub author: String,
    /// `downloads`, `likes`, `modified`, `created` or `trending`.
    pub sort: String,
    pub cursor: Option<String>,
}

/// A file of a repository on the provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct HubFile {
    pub path: String,
    pub size: u64,
    pub sha256: Option<String>,
    pub lfs: bool,
    /// On the internal disk.
    pub stored: bool,
}

/// Everything about one repository on the provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct HubModel {
    pub id: String,
    pub commit: Option<String>,
    /// The provider could not be asked; this is what dedupd kept.
    pub from_archive: bool,
    pub meta: ModelMeta,
    pub files: Vec<HubFile>,
    pub variants: Vec<Variant>,
    /// The same model in other formats and quantizations.
    pub related: Vec<SearchHit>,
    pub readme: Option<String>,
    /// dedupd keeps this model.
    pub archived: bool,
    pub apps: Vec<AppPresence>,
    pub downloading: bool,
    /// The provider's own description, as it sent it.
    pub info_json: String,
}
