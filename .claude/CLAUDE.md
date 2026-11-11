# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Status

Phases 1 to 4 of the build order are implemented and committed one phase per commit: store and adoption, fetcher and placers, the HTTP front ends, and the daemon with its background work. The daemon also has a durable download queue with a rate governor, and a control API that covers everything the app shows. The macOS app (`macos-app/`) is built on it: a menu bar icon with a small panel, and a main window with five screens (Library, Discover, Downloads, Storage, Daemon), all through the UniFFI client crate. A release of the app is made by `just app-release` (signed with Developer ID, notarised, in a disk image); the signing and packaging have been run, the notarisation has not, because no notary credential is stored on this Mac yet. Not built: Bionic network interception, Xet for the daemon's own downloads, updates for the app, and Xcode Cloud. `spikes/` holds three standalone Python scripts (standard library only, run with `python3`) that proved the protocol behaviour before the Rust code existed.

The full research and plan is the Claude Docs artifact "dedupd: research and implementation plan" (https://claude.ai/artifact/CYuT3GEgaBf9EgKLaMPoxd); read it with the Artifact/Claude Docs tools before making design changes, and update it when a decision changes.

## Commands

```sh
cargo build --release                      # binary at target/release/dedupd
cargo test --workspace                     # all tests
cargo test -p dedupd-core --test pull      # one integration test file (adopt, pull, maintain)
cargo test -p dedupd-server --test frontends   # client request sequences against the front ends
cargo test -p dedupd-core hub::tests::parses_link_header   # a single test
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

`just` lists the recipes that wrap these and the app's (`just check` is the Rust gate).

The macOS app (needs Xcode, `brew install xcodegen swiftlint just`):

```sh
just app-check        # SwiftLint strict, build, unit tests (the gate)
just app-uitest       # drive the window with canned daemons; takes pictures and takes over the screen while it runs
just app-screenshots  # export those pictures to macos-app/build/screenshots
just app-run          # build and start the app (menu bar)
just app-preview downloading downloads   # the window on a canned daemon, on one of its screens
just app-live 127.0.0.1:8643   # end to end: find, download and list a model through a running scratch daemon
just app-bindings     # rebuild the Rust client and regenerate the Swift bindings
just fixtures         # re-record the control API answers the app's contract tests read
just app-package      # a release build signed with Developer ID in a disk image, not sent to Apple (to check a release)
just app-release      # the release: signed, notarised by Apple, macos-app/build/release/dedupd-<version>.dmg
```

Tests need macOS on APFS, because they create real clones in a temp directory. They use a mock Hugging Face (wiremock) and scratch app folders; they never touch the real app folders or `~/.dedupd`.

To try the CLI without touching the real store or the real app folders, use a scratch home with a `config.toml` that sets `bionic_models`, `ollama_models`, `hf_cache`, `listen` and `archive` (all optional): `dedupd --home <dir> ...` or `DEDUPD_HOME`. `--endpoint` / `DEDUPD_HF_ENDPOINT` redirects upstream calls. `dedupd install --label <other>` installs the launchd agent under another label for a trial.

In zsh, write `"${repo}:Q4_K_M"`, not `$repo:Q4_K_M`: `:Q` is a zsh modifier and silently eats the tag.

## Code layout

A Cargo workspace (Rust 2024). `.cargo/config.toml` sets the minimum macOS for every Rust build to 26, the same as the app's; keep the two equal.

- `crates/dedupd-core` — library, no HTTP server.
  - `store` (content-addressed folder, clone-in, restore from the archive disk), `db` (SQLite schema and queries), `hash`, `clone`, `paths`, `config` (`<home>/config.toml`).
  - `hub` (read-only Hub API client), `fetch` (resumable verified downloads), `select` (which files of a repository: a quantization, patterns, or everything), `ollama` (manifest format).
  - `queue` (the download queue: every job is a row in the database, survives a restart, and can be paused, continued, retried and cancelled) and `governor` (pacing: `Retry-After` and the `RateLimit` headers first, AIMD on the number of connections where the server gives no number, then the owner's ceilings on connections and bytes per second).
  - `sources/` (what each app already holds: `bionic`, `hf_cache`, `ollama`), `adopt` (hash, verify against the Hub listing, clone into the store).
  - All SQL lives in `db` (except the read-only queries in `status`); the one clone-then-rename is `clone::clone_into_place`; shared test support is `tests/common`, also used by the server tests.
  - `place` (the three placers), `pull` (resolve, fetch, place; also the `Engine` every caller shares), `archive` (second copy on another disk), `maintain` (reclaim, verify, gc, forget), `status`, `library` (the read-only view of the store model by model, and where each app keeps its files).
- `crates/dedupd-server` — the daemon's HTTP side. `hubapi` (Hub API), `registry` (Ollama registry), `files` (Range serving, and assembling large files from what clients download through the daemon), `proxy` (pass-through and header cleaning), `control` (`/dedupd/v1`), `discover` (search and one repository's details on the provider, for the app), `daemon` (the background pass, the folder watcher and the scheduled store check).
- `crates/dedupd-client` — a small blocking client for the control API, exported through UniFFI: one function per call (`lib.rs`), the records they return (`types.rs`), and `subscribe` (`events.rs`), which follows the daemon's event stream on one thread and calls a listener; it reconnects by itself and tells the listener when the daemon is away. It does not depend on the core and links no TLS stack (the daemon is plain HTTP on loopback).
- `crates/dedupd` — the `dedupd` binary: `main.rs` (the command list; pull, place, adopt, status, backup), `serve.rs` (serve, env), `maintenance.rs` (reclaim, verify, gc, forget) and `launchd.rs` (install, uninstall, start, stop, agent).

The command line works on the store and database directly, also while the daemon runs; SQLite in WAL mode and atomic renames make that safe. `pull` and `place` are the exception: when a daemon with the same home answers on the configured address they are sent to it through `dedupd-client`, so one process does the downloading. The daemon's control API under `/dedupd/v1` is what the macOS app talks to, through `dedupd-client`: `status`, `pull`, `scan`, `events` (a stream of JSON lines: one at once, then one after each change once it has settled, and a heartbeat), `downloads` (the queue and its actions), `models`, `locations`, `check`, `forget`, `unplace`, `gc`, `verify`, `config`, `log`, `restart`, and `hub/search` and `hub/models/{org}/{repo}`. A pull that fails through the daemon stays in the queue as a job that needs attention. The app must not embed the core, because the daemon has to keep serving when the app is closed.

## The macOS app

`macos-app/` is a SwiftUI app that lives in the menu bar (`LSUIElement`, macOS 26 or later on Apple silicon, Swift 6), set up like frinkel's `ios-app`: `project.yml` (XcodeGen) is the source of truth and the generated `Dedupd.xcodeproj` is committed; `Developer.xcconfig` carries the signing team and includes `Version.xcconfig` (synced from `Cargo.toml` at build time) and an optional gitignored `Local.xcconfig`; SwiftLint runs after every build. Conventions are in `.claude/rules/macos-app.md`. The design and the decisions behind it are on the page "dedupd App Design" (https://claude.ai/artifact/XEhjuDrTZ1NVNBQ8RuSas2).

What it is: the menu bar icon opens a small panel (state at a glance, a scan, "Open dedupd"); the main window has a sidebar, a list and an inspector. Closing the window leaves the icon and takes the app out of the Dock; quitting the app leaves the daemon.

- `DedupdClient/` — a static library target holding only what `scripts/build-rust-client.sh` generates from `crates/dedupd-client` (the Swift bindings and their C module, committed). The script runs before every Xcode build and also leaves `build/rust/libdedupd_client.a`, which the app links by path. Built for arm64 only.
- `scripts/bundle-daemon.sh` — runs after every build: builds `dedupd` in release mode, puts it at `Contents/Helpers/dedupd` inside the app and signs it. The Daemon screen installs, updates, starts, stops and uninstalls the launch agent by running that command (`AgentService`); the app has no launchd code of its own.
- `scripts/release.sh` — makes the release (`just app-release`): archives a Release build, checks the signatures of the app and of the command inside it, has Apple notarise the app and then the disk image, staples both, and asks macOS whether it would open them. A Debug build is signed with the Apple Development certificate, a Release build with the team's Developer ID Application certificate and a signed time (`project.yml`). It needs that certificate in the keychain and a notary credential stored as the keychain profile `dedupd-notary` (`xcrun notarytool store-credentials`); `--skip-notary` (`just app-package`) stops short of Apple and names the image `-not-notarised`.
- `Dedupd/` — `App` (entry point, launch options, the app-wide objects, the Dock icon rule), `Services` (`DaemonClient` protocol; `LiveDaemonClient` calls the Rust client off the main thread and turns its event listener into an `AsyncStream`; `FixtureDaemonClient` is a canned daemon with a queue that reacts; `AgentService`; `Notifier`), `Stores` (`DaemonStore` holds the connection, the status and the downloads, fed by the one event subscription; `LibraryStore`, `DiscoverStore`, `StorageStore` and `ServiceStore` hold one screen each and reload when `DaemonStore.revision` changes; `AppSettings`), `Models`, `Views` (`Menu`, `Main`, one folder per screen, `Shared`, `Settings`), `Utilities`.
- `DedupdTests/` — Swift Testing. The contract tests run the real Rust client against a loopback server that replays the daemon's answers recorded in `crates/dedupd-server/tests/fixtures/control/`.
- `DedupdUITests/` — XCUITest, one test per screen on a canned daemon, and one live test that `just app-live` runs.

Launch options, always written as a name and a value: `-uiTestFixture running|downloading|notRunning` (a canned daemon, and preferences of its own), `-pane library|discover|downloads|storage|daemon`, `-mainWindow YES`, `-panelWindow YES` (the menu bar panel in an ordinary window, because a menu bar panel cannot be driven or photographed reliably), `-daemonAddress host:port`, and `-daemonHome <dir>` with `-agentLabel <label>` to point the Daemon screen's install at a scratch home under another label.

## Two tiers of storage

- **Store (warm)**: `~/.dedupd/store` on the internal disk. It must be on the same APFS volume as the app folders, because clones do not cross volumes. This is what apps load from.
- **Archive (cold)**: the folder named by `archive` in `config.toml`, on this Mac `/Volumes/Trunk/dedupd` (an external disk). Same layout as a home folder, so `dedupd --home /Volumes/Trunk/dedupd status` reads it. `backup` copies what it lacks, hashing each file as it is written; `pull`, `adopt` and the daemon do the same whenever the disk is attached. A file missing from the store is copied back from the archive on demand (`Store::ensure_local`). `gc --offload` removes local copies of files that are in the archive and in no app.

Everything that touches the archive must tolerate the disk being detached.

## What this project is

`dedupd` is an always-on local daemon for one Mac that lets Ollama, Bionic (LM Studio engine), MLX tools, llama.cpp and any `huggingface_hub` client share one on-disk copy of each Hugging Face model, and keep working when Hugging Face is down or a model is removed upstream.

It is an archive, not a cache, and it is never in the path that loads a model into memory. Its job is to put files, manifests and cache entries where each app looks for them.

A network proxy alone saves no disk, because every app writes its own copy of what it downloads. The saving comes from the store plus clone placement.

## Architecture

Four parts, one process:

1. **Store** — content-addressed folder (`~/.dedupd/store/sha256/ab/<hex>`) on the same APFS volume as the apps. Every file is keyed by its sha256, which for LFS files is the Hub's `lfs.oid`. The git blob id of small non-LFS files is kept as an attribute in the database, because the Hub identifies those files by it. The database (`~/.dedupd/dedupd.sqlite`) archives each revision's Hub answers verbatim, maps `repo@commit/path` to content, and records every app file it knows about.
2. **Fetcher** — downloads from huggingface.co with `Range` resume, verifies sha256, pins branch names to commits.
3. **Front ends** — the Hub API and the Ollama registry API on one localhost port, reading from the store and falling through to the fetcher.
4. **Placers** — one per app; each knows the app's layout and which files it needs, and creates APFS clones (`clonefile(2)`), never copies:
   - Ollama: `~/.ollama/models/blobs/sha256-{hex}` plus a manifest under `manifests/hf.co/{org}/{repo}/{tag}` (0.40 also has a `manifests-v2` tree where the name is a symlink to a manifest blob).
   - Bionic: `~/.lmstudio/models/{org}/{repo}/{file}`.
   - Hugging Face cache: `~/.cache/huggingface/hub/models--org--repo/{blobs/{etag},snapshots/{commit}/,refs/main}`; also serves llama.cpp and MLX tools.

Two ways in: explicit (`dedupd pull org/repo --for ollama,bionic,mlx` fetches once and places clones) and transparent (an app fetches through the proxy, writes its own copy, and a reclaim pass swaps that copy for a clone). Existing models are adopted by cloning from the app folder into the store; adoption never modifies an app's files.

Commands: `pull`, `place`, `adopt`, `status`, `backup`, `serve`, `reclaim`, `verify`, `gc`, `forget`, `install`, `uninstall`, `start`, `stop`, `agent`, `env`. `install` copies the running binary to `<home>/bin/dedupd` and starts it from there, so the agent does not depend on where the app or a build folder is; `agent --json` is what the app reads.

The daemon's background pass (started by a folder watcher when an app folder changes, and by a timer every 300 seconds as a fallback; `scan_interval` in `config.toml`): forget app files that are gone, adopt new or changed files in the three app folders once they have been left alone for 20 seconds, swap duplicate copies for clones (`reclaim`, only files unchanged since hashing and older than a minute), and update the archive disk. The daemon also hashes the whole store on a schedule (`verify_days`) and notes the time in `<home>/last-verify.txt`.

## Invariants

- The Ollama layer digest for a GGUF equals the file's sha256, the Hub `lfs.oid`, and the HF cache blob name. This identity is what makes one store serve every app; do not introduce any transformation of weight files.
- Use clones, not symlinks or hard links, for placement. The store and the app folders must be on one APFS volume; only the archive is on another disk. `du` over-reports clones; `df` is correct.
- Nothing is deleted automatically. `gc` is explicit, lists what it would remove, and removes only with `--apply`. The one automatic change to an app's files is reclaim, which replaces a file with a clone of identical content.
- Archive per repo and commit: weights, small files (configs, tokenizer, README, licence), the Hub tree and model-info JSON, branch/tag to commit mapping, and the Ollama manifest with its config, template and params layers (Hugging Face generates those; they exist nowhere else).
- When upstream errors or returns 404/401/410 for something archived, answer from the archive. For anything not archived, relay the failure.
- Forward `Authorization` to huggingface.co only, never to a CDN. Re-check gated/private repos upstream before serving cached bytes.

## Protocol rules that are easy to get wrong

Hub API (for `huggingface_hub`, llama.cpp):

- Hide Xet, or model bytes bypass the proxy: remove `xetHash` from `/tree` and `/paths-info` entries (keep `lfs.oid`, `lfs.size`), and never emit `X-Xet-Hash`, the Xet `Link` relations or `X-Xet-Refresh-Route`. Still pass `/api/models/{repo}/xet-read-token/{rev}` through for clients with an older cached tree.
- Answer `HEAD /{repo}/resolve/{rev}/{path}` with 200 directly, carrying `X-Repo-Commit`, `ETag` (git sha1 for small files, sha256 for LFS), `Content-Length`, `Accept-Ranges: bytes`. Never redirect to a CDN; the CDN's own ETag is the Xet hash and would produce wrong blob names.
- Rewrite the absolute `Link: rel="next"` pagination header to the proxy origin.
- Relay status, `X-Error-Code`, `X-Error-Message` and `X-Repo-Commit` verbatim, including on 404.
- The `huggingface_hub` client refuses a plain-HTTP download of a single file over 50 GB. Handle those by pre-placing into the HF cache so the client only sends a HEAD.

Ollama registry:

- `GET /v2/.../blobs/sha256:{d}` without `Range` must answer 307 to a URL with a different hostname; a direct 200 fails with `no Location header`, and so does a redirect to the same hostname, because Ollama follows that one itself. The daemon redirects between its two loopback names (`127.0.0.1` and `localhost`) and listens on both. Ollama then sends parallel `Range` parts of 100 MB to 1 GB.
- Remove `WWW-Authenticate` from relayed registry errors; Ollama rejects a token realm on another host.
- Plain HTTP requires `ollama pull --insecure 127.0.0.1:PORT/org/repo:tag`; the model name then carries the proxy address. Direct placement keeps the `hf.co/...` name.
- Manifest responses differ by User-Agent; include it in the cache key.

## Upstream limits (not bugs here)

- Ollama cannot use MLX repos from Hugging Face (registry returns 400; Ollama re-packs safetensors) or split GGUF files.
- Bionic has no endpoint setting. Its downloads go to `search.lmstudio.ai/v1/hf-proxy/...`; the base-URL override exists only in a signed enterprise policy. Bionic is covered by placement and by adopting what it downloads. Avoid folder-level symlinks in its models folder.
- Bionic and LM Studio are separate apps sharing `~/.lmstudio`.

## Harness

`scripts/claude/finish-worktree-removal.sh` removes what a failed `git worktree remove` leaves under `.claude/worktrees/` (on this Mac, Finder can write a `.DS_Store` into the tree while git deletes it; git then stops and the directory is no longer listed). Hooks in `.claude/settings.json` run it at session start, after `git worktree remove|prune`, after `ExitWorktree` and when a subagent stops. Test it with `sh scripts/claude/finish-worktree-removal.test.sh`.

## Spikes

`spikes/` holds the scripts that proved the protocol behaviour. They are evidence and seeds for conformance tests, not product code.

```sh
# Ollama registry served from a folder of sha256-named files (port 8099)
python3 spikes/ollama_registry_from_folder.py <cas-dir> <manifest.json>
ollama pull --insecure localhost:8099/<org>/<repo>:<tag>

# Hub endpoint that hides Xet and streams through the proxy (port 8098)
python3 spikes/hub_endpoint_hide_xet.py
HF_HOME=<scratch-dir> HF_ENDPOINT=http://127.0.0.1:8098 hf download <org>/<repo> <file>

# Logging pass-through, to record what a client requests (port 8098)
python3 spikes/hub_endpoint_logging_passthrough.py
```

Use a scratch `HF_HOME` and a tiny model (for example `bartowski/SmolLM2-135M-Instruct-GGUF`, 105 MB) when testing, and remove test models from `~/.ollama/models` and `~/.lmstudio/models` afterwards. Those folders hold the owner's real models (about 270 GB in Bionic).

## Build order

0. Spikes. Done: a multi-file MLX repository placed into Bionic is indexed and loads; a model placed in Ollama's plain `manifests/` tree lists, runs and is removed normally on 0.40.16 and is still listed after the Ollama server restarts, so `manifests-v2` is not written (Ollama's own pull writes both trees; the v2 entry is a symlink to a manifest blob in `blobs/`). `mlx_lm.generate` loads an MLX repository through the daemon, also with Hugging Face unreachable. Not tested: `HTTPS_PROXY` with Ollama (not needed, since the registry front end works).
1. Done. Store, database, `adopt`, `status`, verification against the Hub tree.
2. Done. Fetcher, `pull`, `place`, the three placers, the archive tier.
3. Done. Hub API and Ollama registry front ends; client request sequences replay as tests.
4. Done. launchd agent, `env`, background pass, reclaim, `verify`, `backup`, `gc`, offline serving.
5. Gated-repository auth for Ollama is done (the daemon adds its own Hugging Face token to registry requests that carry none). Not done: Bionic network interception (its downloads are picked up by the background pass instead) and Xet for the daemon's own upstream fetch (plain HTTP works; this would only be faster).

6. Done, except distribution. The download queue and rate governor in the daemon, the wider control API with its event stream, and the macOS app: Library, Discover, Downloads, Storage and Daemon screens, the menu bar panel, notifications, and install and uninstall of the launch agent through the bundled command. Tried end to end against a scratch daemon with `just app-live`. The release recipe is written and its signing and packaging have been run. Not done: a first notarised release (the notary credential is not stored yet), updates for the app, Xcode Cloud; installing the agent from the app has only been run against the canned agent, not for real.

Next: the first notarised release of the app; ModelScope as a second provider (the `repos.provider` column exists for it).
