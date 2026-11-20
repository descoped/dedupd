//! Where model files already live on this machine. Each source knows one
//! app's folder layout and reports the files it finds, grouped by repository.

pub mod bionic;
pub mod hf_cache;
pub mod ollama;

use crate::store::Fingerprint;
use std::path::{Path, PathBuf};

/// The parts of `path` below `base`. Joined with `/` they are the path as
/// the provider names it. `path` must be under `base`.
/// A walk's entry failed because it is no longer there.
pub(crate) fn gone(e: &walkdir::Error) -> bool {
    e.io_error()
        .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
}

/// An error that says a file or folder is no longer there.
pub(crate) fn not_there(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
    })
}

pub(crate) fn rel_components(path: &Path, base: &Path) -> Vec<String> {
    path.strip_prefix(base)
        .expect("path is under its base folder")
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect()
}

#[derive(Debug, Clone)]
pub struct LocalFile {
    /// Path inside the repository, with `/` separators, as the Hub names it.
    pub rel_path: String,
    pub abs_path: PathBuf,
    pub fingerprint: Fingerprint,
}

#[derive(Debug, Clone)]
pub struct LocalRepo {
    /// Short name of the app that owns the files, for example `bionic`.
    pub app: &'static str,
    /// Where the repository comes from: `hf`, or `ollama` for models from an
    /// Ollama registry, which Hugging Face knows nothing about.
    pub provider: &'static str,
    /// Repository id on the provider, for example `org/name`.
    pub name: String,
    /// The revision the files belong to, when the app's layout records it.
    /// `None` means the files are compared with `main`.
    pub revision: Option<String>,
    pub files: Vec<LocalFile>,
}
