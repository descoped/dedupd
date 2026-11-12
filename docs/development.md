# Development

## What you need

- macOS 26 on Apple silicon, with the working folder on APFS. The tests create real clones in a temp folder.
- A current Rust toolchain (the workspace uses the 2024 edition).
- For the app: Xcode, and `brew install xcodegen swiftlint just`.

## The gates

```sh
just check        # Rust: formatting, clippy with warnings as errors, every test
just app-check    # the app: SwiftLint strict, build, unit tests
```

`just` alone lists every recipe. Single tests:

```sh
cargo test -p dedupd-core --test pull          # adopt, pull, maintain
cargo test -p dedupd-server --test frontends   # what clients ask the front ends
cargo test -p dedupd-core hub::tests::parses_link_header
```

The tests use a mock Hugging Face and scratch app folders. They never touch `~/.dedupd` or your apps' folders.

`just app-uitest` drives the app's window on canned daemons and takes the pictures used in these pages. It takes
over the screen while it runs. `just app-screenshots` exports the pictures to `macos-app/build/screenshots`.

## Trying a build safely

Never point a build from this folder at your real home. Give it a scratch home with a `config.toml` that sets
`bionic_models`, `ollama_models`, `hf_cache`, `listen` and `archive` to scratch places:

```sh
target/release/dedupd --home <scratch> status
target/release/dedupd --home <scratch> serve
```

`--endpoint` (or `DEDUPD_HF_ENDPOINT`) sends upstream calls somewhere else. `dedupd install --label <other>`
installs the agent under another label for a trial. A small model for testing:
`bartowski/SmolLM2-135M-Instruct-GGUF` (105 MB).

## Layout

A Cargo workspace and a SwiftUI app.

| Path | What is there |
|---|---|
| `crates/dedupd-core` | The library: the store, the database, the Hugging Face client, downloads and the queue, the three placers, app profiles, the archive disk, maintenance. No HTTP server. |
| `crates/dedupd-server` | The daemon's HTTP side: the Hugging Face Hub API, the Ollama registry API, the control API under `/dedupd/v1`, and the background pass. |
| `crates/dedupd-client` | A small blocking client for the control API, exported to Swift through UniFFI. |
| `crates/dedupd` | The `dedupd` command. |
| `macos-app` | The menu bar app. `project.yml` (XcodeGen) is the source of truth; the Xcode project is generated. |
| `spikes` | Standalone Python scripts that proved the protocol behaviour before the Rust code existed. Evidence, not product code. |

The app talks to the daemon only through `crates/dedupd-client`. It never opens the store or the database.

`.claude/CLAUDE.md` is the long form: the invariants, the protocol rules that are easy to get wrong, and what
each module does.

## Rules that must hold

- A weights file in the store is never changed. Its sha256 is the Hub's `lfs.oid`, the Ollama layer digest and the
  Hugging Face cache's blob name, and that identity is what lets one store serve every app.
- Placement is by clones. No symlinks, no hard links, no copies.
- Nothing is deleted without an explicit `--apply`.
- A Hugging Face token goes to Hugging Face only, never to a download server.
- Text a person types or pastes is not trusted. The app reads it in one place (`ModelSpec`), and the daemon checks
  a model's name again before it asks Hugging Face.
- The database has one flat schema and no migrations. When the schema changes, the number is raised and the old
  database is removed; the next scan adopts the models again from the app folders.

## Releases

`just app-release` makes the app's release: a Release build signed with Developer ID, notarised by Apple, in a
disk image. It needs the maintainer's certificate and notary credential. `just app-package` does everything short
of sending it to Apple, for checking.

## Continuous integration

`.github/workflows/ci.yml` runs `just check` on macOS for pull requests and pushes to `main`. The app is checked
on the developer's Mac, because its debug build is signed with a development certificate.
