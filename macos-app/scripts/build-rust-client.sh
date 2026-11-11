#!/bin/sh
# Build the Rust control API client for the app and refresh its Swift bindings.
#
# The app links crates/dedupd-client as a static library and calls it through
# the Swift file UniFFI generates from it. Xcode runs this before it compiles
# the DedupdClient target; `just app-bindings` runs it by hand.
#
# Writes:
#   macos-app/build/rust/libdedupd_client.a                 (not committed)
#   macos-app/DedupdClient/Generated/dedupd_client.swift    (committed)
#   macos-app/DedupdClient/Generated/FFI/dedupd_clientFFI.h (committed)
#   macos-app/DedupdClient/Generated/FFI/module.modulemap   (committed)
#
# The generated files are committed so that `xcodegen generate` finds them in
# a clean clone. A file is replaced only when its content changed, so an
# unchanged client does not make Xcode recompile.
set -eu

app=$(cd "$(dirname "$0")/.." && pwd)
root=$(cd "$app/.." && pwd)

# Xcode runs scripts with a minimal PATH that has neither rustup nor Homebrew.
PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/opt/homebrew/opt/rustup/bin:/usr/local/bin:$PATH"
export PATH
if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found. Install Rust from https://rustup.rs and build again." >&2
  exit 1
fi

target=${CARGO_TARGET_DIR:-$root/target}
out=$app/build/rust
generated=$app/DedupdClient/Generated
tmp=$out/bindings
mkdir -p "$out" "$generated/FFI"
rm -rf "$tmp"
mkdir -p "$tmp"

cd "$root"

# Replace a file only when its content differs.
install_if_changed() {
  if ! cmp -s "$1" "$2"; then
    cp "$1" "$2"
    echo "updated ${2#"$root"/}"
  fi
}

# The library first, without the generator: the app links this one. The build
# also leaves the dynamic library the generator reads the interface from;
# running the generator alone does not.
cargo build --quiet --release -p dedupd-client
install_if_changed "$target/release/libdedupd_client.a" "$out/libdedupd_client.a"

# The generator is a feature of the client crate, so that its dependencies
# stay out of the library built above.
cargo run --quiet --release -p dedupd-client --features bindgen --bin uniffi-bindgen -- \
  generate --library "$target/release/libdedupd_client.dylib" --language swift --out-dir "$tmp"

install_if_changed "$tmp/dedupd_client.swift" "$generated/dedupd_client.swift"
install_if_changed "$tmp/dedupd_clientFFI.h" "$generated/FFI/dedupd_clientFFI.h"
# clang finds a module map only under this name.
install_if_changed "$tmp/dedupd_clientFFI.modulemap" "$generated/FFI/module.modulemap"
rm -rf "$tmp"
