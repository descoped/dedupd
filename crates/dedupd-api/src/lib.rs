//! The records and words of the dedupd daemon's control API (`/dedupd/v1`).
//!
//! The daemon builds its answers from these types and the client reads them
//! back into the same types, so the two cannot drift: a field that is
//! renamed on one side is renamed on the other, or the build fails. Words
//! that the app acts on (a job's state, the actions it allows, an app's
//! name) are enums here, never strings to be spelled out again.
//!
//! The crate holds data and the rules that belong to the data alone. With
//! the `ffi` feature every type is also a UniFFI record or enum, which is
//! how the macOS app sees them.

pub mod activity;
pub mod app;
pub mod daemon;
pub mod hub;
pub mod jobs;
pub mod library;
pub mod moves;

#[cfg(feature = "ffi")]
uniffi::setup_scaffolding!();
