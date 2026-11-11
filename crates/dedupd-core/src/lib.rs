//! Core of dedupd: one content-addressed store of model files, shared with
//! local runtimes through APFS clones.

pub mod adopt;
pub mod archive;
pub mod clone;
pub mod config;
pub mod db;
pub mod fetch;
pub mod governor;
pub mod hash;
pub mod hub;
pub mod maintain;
pub mod ollama;
pub mod paths;
pub mod place;
pub mod pull;
pub mod queue;
pub mod select;
pub mod sources;
pub mod status;
pub mod store;

pub use paths::Home;

/// What dedupd calls itself when it asks the provider for something.
pub(crate) const USER_AGENT: &str = concat!("dedupd/", env!("CARGO_PKG_VERSION"));

/// Seconds since the Unix epoch.
pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
