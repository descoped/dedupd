//! Core of dedupd: one content-addressed store of model files, shared with
//! local runtimes through APFS clones.

pub mod activity;
pub mod adopt;
pub mod archive;
pub mod clone;
pub mod config;
pub mod db;
pub mod engine;
pub mod fetch;
pub mod governor;
pub mod hash;
pub mod hub;
pub mod library;
pub mod maintain;
pub mod names;
pub mod ollama;
pub mod ollama_mlx;
pub mod paths;
pub mod place;
pub mod profile;
pub mod pull;
pub mod queue;
pub mod refusals;
pub mod safetensors;
pub mod select;
pub mod sources;
pub mod status;
pub mod store;

pub use paths::Home;

/// What dedupd calls itself when it asks the provider for something.
pub(crate) const USER_AGENT: &str = concat!("dedupd/", env!("CARGO_PKG_VERSION"));

/// A size as people read it: `6.86 GB`, in powers of 1000. The app's
/// `ByteFormat` writes the same, so every place shows one figure.
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// Seconds since the Unix epoch.
pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
