//! The library: the models dedupd keeps, where their files are, and what
//! each app can do with them.

use crate::app::App;
use serde::{Deserialize, Serialize};

/// What the provider says about a model.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct ModelMeta {
    pub author: Option<String>,
    pub pipeline_tag: Option<String>,
    pub library_name: Option<String>,
    pub license: Option<String>,
    pub base_model: Option<String>,
    /// Number of parameters.
    pub parameters: Option<u64>,
    pub architecture: Option<String>,
    pub context_length: Option<u64>,
    /// The owner must accept terms on the provider's site before downloading.
    pub gated: bool,
    pub private: bool,
    /// `gguf`, `mlx`, `safetensors`, `onnx`: the provider's own tags, an
    /// open set.
    pub formats: Vec<String>,
    pub languages: Vec<String>,
    pub tags: Vec<String>,
    pub downloads: Option<u64>,
    pub likes: Option<u64>,
    /// ISO 8601, as the provider sends it.
    pub last_modified: Option<String>,
    pub created_at: Option<String>,
    /// Bytes the repository uses on the provider, all revisions together.
    pub used_storage: Option<u64>,
}

/// Whether the provider still has a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum UpstreamState {
    Ok,
    /// The provider answered that it no longer has the model.
    Gone,
    /// The provider has not been asked.
    Unknown,
}

/// What an app holds of one model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct AppPresence {
    pub app: App,
    pub files: u64,
    pub bytes: u64,
    /// Bytes that share their disk blocks with the store.
    pub shared_bytes: u64,
}

/// One model in the library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct LibraryModel {
    pub provider: String,
    pub repo: String,
    /// The commit `main` pointed at when the provider was last asked.
    pub commit: Option<String>,
    pub meta: ModelMeta,
    /// Revisions kept.
    pub revisions: u64,
    /// When the newest revision was kept, in seconds since the epoch.
    pub fetched_at: Option<i64>,
    /// Files of the kept revisions that are in the store.
    pub stored_files: u64,
    pub stored_bytes: u64,
    pub apps: Vec<AppPresence>,
    pub verified: u64,
    pub mismatch: u64,
    pub not_upstream: u64,
    pub unchecked: u64,
    pub upstream: UpstreamState,
    pub checked_at: Option<i64>,
    /// The Ollama tags whose manifests are kept.
    pub ollama_tags: Vec<String>,
    /// When the owner archived the model, in seconds since the epoch: it is
    /// in no app and its files are on the archive disk only.
    pub archived_at: Option<i64>,
}

/// The library's models.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelList {
    pub models: Vec<LibraryModel>,
}

/// One file of a kept revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct RevisionFile {
    pub path: String,
    pub size: u64,
    /// sha256 of the content, when it is known.
    pub sha256: Option<String>,
    pub git_oid: String,
    /// Kept in LFS by the provider: a weight file rather than a small one.
    pub lfs: bool,
    /// On the internal disk.
    pub stored: bool,
    /// On the archive disk; `false` also when it is not attached.
    pub archived: bool,
    /// The apps that have this file.
    pub apps: Vec<App>,
}

/// A commit of a model that dedupd has kept the provider's listing for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Revision {
    pub commit: String,
    pub fetched_at: i64,
    /// Branch and tag names that point here.
    pub refs: Vec<String>,
    pub bytes: u64,
    pub files: Vec<RevisionFile>,
}

/// How an app's file compares with the provider's listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum VerifyState {
    /// Same bytes as the provider lists at the recorded commit.
    Verified,
    /// The provider lists this path with different content. The local file
    /// may come from an older revision, or may be damaged.
    Mismatch,
    /// The provider's listing has no file at this path.
    NotUpstream,
    /// The provider could not be asked.
    Unchecked,
}

crate::all_variants!(VerifyState: Verified, Mismatch, NotUpstream, Unchecked);

impl VerifyState {
    /// The state's word in the control API and in the database.
    pub const fn as_str(self) -> &'static str {
        match self {
            VerifyState::Verified => "verified",
            VerifyState::Mismatch => "mismatch",
            VerifyState::NotUpstream => "not_upstream",
            VerifyState::Unchecked => "unchecked",
        }
    }

    /// The inverse of `as_str`. Anything unknown reads as unchecked.
    pub fn parse(s: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|v| v.as_str() == s)
            .unwrap_or(VerifyState::Unchecked)
    }
}

/// How a file came to be in an app's folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum Origin {
    /// The app downloaded it, and dedupd took it into the store.
    Adopted,
    /// dedupd put it there.
    Placed,
}

crate::all_variants!(Origin: Adopted, Placed);

impl Origin {
    /// The origin's word in the control API and in the database.
    pub const fn as_str(self) -> &'static str {
        match self {
            Origin::Adopted => "adopted",
            Origin::Placed => "placed",
        }
    }

    /// The origin a database row's word names.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|o| o.as_str() == s)
    }
}

/// A file in an app's folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Placement {
    pub app: App,
    pub path: String,
    pub rel_path: String,
    pub size: u64,
    pub sha256: String,
    pub shared: bool,
    pub verify_state: VerifyState,
    pub origin: Origin,
}

/// The form a model's weights are in. App profiles name the formats each
/// method takes, and the app remembers per format where new models go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum), uniffi(name = "ModelFormat"))]
pub enum Format {
    Gguf,
    Mlx,
    /// Anything else: plain safetensors, ONNX and so on.
    Other,
}

crate::all_variants!(Format: Gguf, Mlx, Other);

impl Format {
    /// The format's name in a sentence.
    pub const fn label(self) -> &'static str {
        match self {
            Format::Gguf => "GGUF",
            Format::Mlx => "MLX",
            Format::Other => "other",
        }
    }
}

/// What a variant is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum VariantKind {
    /// One quantization of a GGUF repository.
    Gguf,
    /// A repository that is taken whole.
    Files,
}

/// Whether an app can take a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum DecisionState {
    /// dedupd can put the model into the app.
    Ready,
    /// The app could take the model, and dedupd cannot do it yet.
    Planned,
    /// The app cannot take the model.
    No,
}

/// Whether the installed version of an app was tried with a method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum VersionState {
    Tested,
    Untested,
    /// Known not to work.
    Broken,
    /// The version was not found.
    Unknown,
}

/// The new disk space a method takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum Cost {
    /// A clone: nothing.
    #[cfg_attr(feature = "ffi", uniffi(name = "nothing"))]
    None,
    /// A clone with a part rewritten.
    Megabytes,
    /// A second copy.
    Full,
}

/// What the daemon decided for one app and one variant of a model, from the
/// app's profile and the version of the app that is installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Decision {
    pub app: App,
    pub state: DecisionState,
    /// The method, unless the state is `no`: `clone`, or
    /// `clone-with-rewritten-index`. Methods are named in the profiles,
    /// which are data.
    pub method: Option<String>,
    pub cost: Option<Cost>,
    /// Why, in one sentence.
    pub reason: String,
    /// The installed version of the app, when the daemon found it.
    pub version: Option<String>,
    pub version_state: VersionState,
}

/// A way a repository can be downloaded and used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Variant {
    pub kind: VariantKind,
    /// The form its weights are in, as the app profiles judge it.
    pub format: Format,
    pub name: String,
    /// What to ask for to get it: `org/repo:Q4_K_M`, or `org/repo`.
    pub spec: String,
    pub files: u64,
    pub bytes: u64,
    pub stored_bytes: u64,
    /// Every weights file of it is stored. The small files it may lack (a
    /// README, or a settings file the app it came from did not download)
    /// are fetched when it is activated.
    #[serde(default)]
    pub weights_stored: bool,
    /// The apps dedupd can put it into: those whose decision is `ready`.
    pub usable_by: Vec<App>,
    /// The apps that have all its weight files.
    pub placed_in: Vec<App>,
    /// For every app: whether it can take this, how, at what cost, and why.
    pub decisions: Vec<Decision>,
    /// For every app: the store on the disk of its folder, and what of this
    /// variant that store lacks and Activate copies in from another store.
    #[serde(default)]
    pub copies: Vec<StoreCopy>,
}

/// What activating a variant in one app copies first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct StoreCopy {
    pub app: App,
    /// The store on the disk of the app's folder; none when that disk has
    /// no store, and the app cannot be given the model.
    pub store: Option<String>,
    /// The files another store on this Mac has and this one lacks.
    pub files: u64,
    pub bytes: u64,
}

/// Everything about one model in the library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct ModelDetail {
    pub model: LibraryModel,
    pub revisions: Vec<Revision>,
    pub placements: Vec<Placement>,
    pub variants: Vec<Variant>,
    /// The provider's own description of the newest revision, as it sent it.
    pub info_json: Option<String>,
    pub readme: Option<String>,
    /// The names of the attached archives that hold every file of the
    /// model.
    #[serde(default)]
    pub archives: Vec<String>,
}

/// What a folder is to dedupd.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum LocationKind {
    /// A store: the one in dedupd's home, or another on its own disk.
    Store,
    /// The copy on the archive disk.
    Archive,
    /// An app's folder; `app` names it.
    App,
}

/// A folder on the disk and what dedupd knows is in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Location {
    pub kind: LocationKind,
    /// The app, for an app's folder.
    pub app: Option<App>,
    /// The archive's name, for an archive.
    #[serde(default)]
    pub name: Option<String>,
    /// For an archive: it is the backup archive, which gets a copy of
    /// every stored file.
    #[serde(default)]
    pub backup: bool,
    pub path: String,
    /// The folder is there. For the archive: the disk is attached.
    pub present: bool,
    pub files: u64,
    pub bytes: u64,
    /// Bytes that share their disk blocks with the store.
    pub shared_bytes: u64,
    pub models: u64,
}

/// Where the files are: the store, the archive disk and each app's folder,
/// with the room on the volume they share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Locations {
    /// The volume the store and the app folders are on.
    pub volume_total: u64,
    pub volume_free: u64,
    pub store: Location,
    /// The stores besides the one in dedupd's home.
    #[serde(default)]
    pub stores: Vec<Location>,
    /// Every archive, the backup archive first.
    pub archives: Vec<Location>,
    pub apps: Vec<Location>,
}

/// What the provider said when it was asked about a model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct UpstreamCheck {
    pub upstream: UpstreamState,
    /// The commit `main` pointed at before.
    pub commit: Option<String>,
    /// The commit `main` points at now.
    pub latest: Option<String>,
    /// `main` has moved since the model was kept.
    pub changed: bool,
}

/// A model by its name: the request body of `check` and `forget`, and the
/// answer that names the model something was done to. Because it is also
/// an answer, it does not refuse unknown fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoBody {
    pub repo: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_states_read_back() {
        for v in VerifyState::ALL {
            assert_eq!(VerifyState::parse(v.as_str()), v);
            assert_eq!(
                serde_json::to_string(&v).unwrap(),
                format!("\"{}\"", v.as_str())
            );
        }
        assert_eq!(VerifyState::parse("what"), VerifyState::Unchecked);
    }

    #[test]
    fn origins_read_back() {
        for o in Origin::ALL {
            assert_eq!(Origin::parse(o.as_str()), Some(o));
            assert_eq!(
                serde_json::to_string(&o).unwrap(),
                format!("\"{}\"", o.as_str())
            );
        }
    }
}
