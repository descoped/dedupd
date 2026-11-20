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
//!
//! [`url`] is the one module that is not data: the percent-encoding both
//! ends of the control API use. It is here because the client must not
//! depend on the core, and this is the only crate the two share.
//!
//! A request body refuses a field it does not know (`deny_unknown_fields`),
//! so a misspelt `apply` is an error and not a quiet "only tell me the
//! plan". An answer does not: an older client reads a newer daemon's.

/// Gives an enum its `ALL`: every variant, in the order written. A variant
/// left out of the list does not compile, so a new case cannot be missed
/// by the code that walks `ALL` (parsing a word, scanning every app).
macro_rules! all_variants {
    ($ty:ident: $($variant:ident),+ $(,)?) => {
        impl $ty {
            /// Every variant, in the order it is shown and tried.
            pub const ALL: [$ty; [$($ty::$variant),+].len()] = [$($ty::$variant),+];
        }
        const _: fn($ty) = |variant| match variant {
            $($ty::$variant => ()),+
        };
    };
}
pub(crate) use all_variants;

/// For the tests of this crate: every variant's word on the wire (serde)
/// is its `as_str`.
#[cfg(test)]
pub(crate) fn same_words<T: serde::Serialize + Copy>(all: &[T], word: impl Fn(T) -> &'static str) {
    for variant in all {
        assert_eq!(
            serde_json::to_string(variant).unwrap(),
            format!("\"{}\"", word(*variant))
        );
    }
}

pub mod activity;
pub mod app;
pub mod daemon;
pub mod favorites;
pub mod hub;
pub mod jobs;
pub mod library;
pub mod moves;
pub mod url;

#[cfg(feature = "ffi")]
uniffi::setup_scaffolding!();
