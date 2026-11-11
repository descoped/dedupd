---
paths:
  - "macos-app/**"
  - "crates/dedupd-client/**"
---

# macOS app conventions

**Applies to:** the SwiftUI menu bar app in `macos-app/` and the Rust client it links.

- **`project.yml` is the source of truth** (XcodeGen); `Dedupd.xcodeproj` is generated, committed, and never edited by hand. Run `just app-gen` after adding, moving or removing a file. The version comes from `Version.xcconfig`, synced from `Cargo.toml` by `just sync-version` and by the pre-build script.
- **The app holds no model data and never opens the store.** Everything it shows comes from the daemon's control API through `crates/dedupd-client`, so the daemon keeps working when the app is closed. Do not add a dependency on `dedupd-core` to the client or reach for the database from Swift.
- **The Rust client reaches Swift through UniFFI.** `macos-app/scripts/build-rust-client.sh` builds the static library and regenerates `DedupdClient/Generated/`; Xcode runs it before every build and `just app-bindings` runs it by hand. The generated files are committed and never edited. A change to the client's exported functions or records therefore shows up as a diff there: commit it together with the Rust change.
- **A change to the control API** (`crates/dedupd-server/src/control.rs`) needs `just fixtures` to re-record `crates/dedupd-server/tests/fixtures/control/`, which the app's `ContractTests` read through the real client.
- **The minimum is macOS 26 on Apple silicon**, chosen to avoid fallback code: use the current APIs directly, and add an `if #available` branch only for something newer than 26.
- **Swift 6 strict concurrency**, `@Observable` + `@MainActor` stores, folders by role (`App`, `Models`, `Services`, `Stores`, `Views`, `Utilities`). No force unwraps or `try!` / `as!` without an inline SwiftLint disable comment stating why. Calls into the Rust client block, so they go through `LiveDaemonClient`, which runs them off the main thread.
- **Sizes are written as the command line writes them** (`ByteFormat`, the twin of `human()` in `crates/dedupd/src/main.rs`), so the app and the terminal show the same figure.
- **Gate:** `just app-check` (SwiftLint strict, build, unit tests). `just app-uitest` drives the panel with canned daemons (`-uiTestFixture running|downloading|notRunning`, which also shows the panel in a window) and takes pictures; `just app-screenshots` exports them. `just app-live <address>` pulls a real model through a running daemon: point it at a daemon with a scratch home, never at the real one.
- **Accessibility text is spelled out** on elements a UI test reads (`.accessibilityElement(children: .ignore)` plus a label): combining children that carry tooltips reads as nothing on macOS. A text's words are its accessibility value there, not its label.
- **Machine config** lives in `macos-app/Local.xcconfig` (gitignored, example alongside). The signing team in `Developer.xcconfig` is committed on purpose; it is not a secret.
