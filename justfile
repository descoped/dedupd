# dedupd development recipes. Run `just` to list them.

# Where xcodebuild puts what it builds for the app (not committed).
derived := "build/DerivedData"
# Every xcodebuild call for the app starts like this.
xcb := "xcodebuild -scheme Dedupd -destination 'platform=macOS,arch=arm64' -derivedDataPath " + derived

default:
    @just --list --unsorted

# ---------------------------------------------------------------- Rust

# Build the dedupd binary (target/release/dedupd)
build:
    cargo build --release

# Run all Rust tests
test:
    cargo test --workspace

# Clippy with warnings as errors
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Format the Rust code
fmt:
    cargo fmt --all

# The Rust gate: formatting, clippy and tests
check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace

# Re-record the control API answers the app's contract tests read
fixtures:
    DEDUPD_UPDATE_FIXTURES=1 cargo test -p dedupd-server --test frontends control_api_answers_match_the_recorded_ones

# Copy the version in Cargo.toml to the app
sync-version:
    #!/usr/bin/env bash
    set -euo pipefail
    VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
    printf '// Synced from Cargo.toml\nMARKETING_VERSION = %s\nCURRENT_PROJECT_VERSION = 1\n' "$VERSION" > macos-app/Version.xcconfig
    echo "Updated: macos-app/Version.xcconfig ($VERSION)"

# ---------------------------------------------------------------- macOS app

# Build the Rust client for the app and refresh its Swift bindings
app-bindings:
    sh macos-app/scripts/build-rust-client.sh

# Generate macos-app/Dedupd.xcodeproj from project.yml
app-gen:
    cd macos-app && xcodegen generate

# Lint the Swift code
app-lint:
    cd macos-app && swiftlint lint --config .swiftlint.yml --strict

# Build the app
app-build: app-gen
    cd macos-app && {{xcb}} -quiet build

# Run the app's unit tests, the control API contract tests among them
app-unit: app-gen
    cd macos-app && {{xcb}} -only-testing:DedupdTests test

# Run the app's UI tests against canned daemons (takes over the screen briefly)
app-uitest: app-gen
    cd macos-app && rm -rf build/UITestResults.xcresult && {{xcb}} -only-testing:DedupdUITests -resultBundlePath build/UITestResults.xcresult test

# Run all the app's tests: unit and UI
app-test: app-gen
    cd macos-app && rm -rf build/UITestResults.xcresult && {{xcb}} -resultBundlePath build/UITestResults.xcresult test

# The app gate: lint, build and unit tests
app-check: app-lint app-build app-unit

# Export the pictures the UI tests took to macos-app/build/screenshots
app-screenshots:
    #!/usr/bin/env bash
    set -euo pipefail
    cd macos-app
    RESULT_BUNDLE="build/UITestResults.xcresult"
    OUT_DIR="build/screenshots"
    if [ ! -d "$RESULT_BUNDLE" ]; then
        echo "No test results found. Run 'just app-uitest' first."
        exit 1
    fi
    rm -rf "$OUT_DIR" && mkdir -p "$OUT_DIR"
    xcrun xcresulttool export attachments --path "$RESULT_BUNDLE" --output-path "$OUT_DIR" > /dev/null
    python3 - "$OUT_DIR" <<'PY'
    import json, os, shutil, sys
    out = sys.argv[1]
    with open(os.path.join(out, "manifest.json")) as f:
        tests = json.load(f)
    for test in tests:
        for attachment in test.get("attachments", []):
            source = os.path.join(out, attachment.get("exportedFileName", ""))
            name = attachment.get("suggestedHumanReadableName", "")
            # The pictures the tests name start with a number; the rest is
            # what Xcode records by itself.
            if os.path.exists(source) and name[:1].isdigit():
                shutil.move(source, os.path.join(out, name.split("_")[0] + ".png"))
            elif os.path.isfile(source):
                os.remove(source)
    PY
    echo "Screenshots saved to macos-app/$OUT_DIR"

# End to end against a running daemon: the app pulls a model through it. Use a daemon with a scratch home.
app-live address model="bartowski/SmolLM2-135M-Instruct-GGUF:Q4_K_M": app-gen
    cd macos-app && rm -rf build/LiveResults.xcresult && TEST_RUNNER_DEDUPD_LIVE_ADDRESS={{address}} TEST_RUNNER_DEDUPD_LIVE_MODEL={{model}} {{xcb}} -only-testing:DedupdUITests/DedupdUITests/testLiveDaemonPullsAModel -resultBundlePath build/LiveResults.xcresult test

# Build the app and start it; it appears in the menu bar
app-run: app-build
    open macos-app/{{derived}}/Build/Products/Debug/Dedupd.app

# Start the app with its panel in a window, on a canned daemon: running, downloading or notRunning
app-preview fixture="running": app-build
    open -n macos-app/{{derived}}/Build/Products/Debug/Dedupd.app --args -uiTestFixture {{fixture}}

# Archive the app for release (requires code signing)
app-archive: app-gen
    cd macos-app && xcodebuild -scheme Dedupd -destination 'generic/platform=macOS' -configuration Release -archivePath build/Dedupd.xcarchive archive

# Redraw the app icon
app-icon:
    swift macos-app/scripts/make-icon.swift macos-app/Dedupd/Assets.xcassets/AppIcon.appiconset
