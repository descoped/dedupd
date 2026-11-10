//! Core of dedupd: one content-addressed store of model files, shared with
//! local runtimes through APFS clones.

pub mod adopt;
pub mod clone;
pub mod db;
pub mod hash;
pub mod hub;
pub mod paths;
pub mod sources;
pub mod status;
pub mod store;

pub use paths::Home;

/// Seconds since the Unix epoch.
pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
