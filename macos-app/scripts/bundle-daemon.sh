#!/bin/sh
# Put the dedupd command inside the app.
#
# The app installs, updates, starts and stops the daemon by running the
# `dedupd` it carries (`dedupd install` copies that binary to the daemon's
# home folder and registers the launchd agent). Xcode runs this after it has
# compiled the app and before it signs the bundle, so the command is sealed
# into the app's signature.
#
# Writes: <app>/Contents/Helpers/dedupd
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

cd "$root"
cargo build --quiet --release -p dedupd

target=${CARGO_TARGET_DIR:-$root/target}
helpers="$TARGET_BUILD_DIR/$CONTENTS_FOLDER_PATH/Helpers"
mkdir -p "$helpers"
cp "$target/release/dedupd" "$helpers/dedupd"

# A command inside a signed app needs a signature of its own, with the
# hardened runtime so that the app can be notarised. Without a signing
# identity (a build with signing turned off) it gets an ad-hoc one.
identity=${EXPANDED_CODE_SIGN_IDENTITY:-}
[ -n "$identity" ] || identity=-
codesign --force --sign "$identity" --options runtime --timestamp=none \
  --identifier io.descoped.dedupd.daemon "$helpers/dedupd"
