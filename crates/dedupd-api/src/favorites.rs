//! The models the owner marked as favorites, and the likes on Hugging Face
//! that are brought in among them.

use serde::{Deserialize, Serialize};

/// Where a favorite came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum FavoriteSource {
    /// Starred in dedupd.
    Dedupd,
    /// Liked on Hugging Face, and brought in from there.
    HuggingFace,
}

/// One favorite model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Favorite {
    /// `org/repo`.
    pub repo: String,
    /// Seconds since the Unix epoch.
    pub added_at: i64,
    pub source: FavoriteSource,
}

/// How the likes on Hugging Face are brought in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct LikesSync {
    /// Whether the daemon has a Hugging Face token; without one it does not
    /// know whose likes to read.
    pub has_token: bool,
    /// The Hugging Face account the token belongs to, once known.
    pub user: Option<String>,
    /// When the likes were last asked for, in seconds since the Unix epoch,
    /// whether or not that worked; `problem` says when it did not.
    pub synced_at: Option<i64>,
    /// Why the last reading failed, in a sentence.
    pub problem: Option<String>,
}

/// The favorites, newest first, and how the likes are brought in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Favorites {
    pub favorites: Vec<Favorite>,
    pub likes: LikesSync,
}

/// Mark a model as a favorite, or no longer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct FavoriteBody {
    pub repo: String,
    pub favorite: bool,
}
