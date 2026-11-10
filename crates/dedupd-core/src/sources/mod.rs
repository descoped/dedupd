//! Where model files already live on this machine. Each source knows one
//! app's folder layout and reports the files it finds, grouped by repository.

pub mod bionic;

use crate::store::Fingerprint;
use std::path::PathBuf;

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
    /// Repository id on the provider, for example `org/name`.
    pub name: String,
    pub files: Vec<LocalFile>,
}
