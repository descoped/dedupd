# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Status

Phases 1 to 4 of the build order are implemented and committed one phase per commit: store and adoption, fetcher and placers, the HTTP front ends, and the daemon with its background work. The daemon also has a durable download queue with a rate governor, and a control API that covers everything the app shows. The macOS app (`macos-app/`) is built on it: a menu bar icon with a small panel, and a main window with five screens (Library, Discover, Downloads, Storage, Daemon), all through the UniFFI client crate. A release of the app is made by `just app-release` (signed with Developer ID, notarised, in a disk image); 0.1.0 was the first release made this way (2026-11-12, tag `v0.1.0`, the disk image on the GitHub release). Not built: Bionic network interception, Xet for the daemon's own downloads, updates for the app, and Xcode Cloud. `spikes/` holds standalone Python scripts (standard library only, run with `python3`): three that proved the protocol behaviour before the Rust code existed, and one that shows how Ollama can be given an MLX model without a second copy of its weights. App profiles are in: the daemon decides from data which app can be given which model, by which method and at what cost, and the app and `dedupd apps` show its answer. The first method beyond a plain clone is built on them: a 4-bit or 8-bit MLX model is placed into Ollama as clones with a rewritten index. The words of the direction are in the command line and the app: a model is activated in an app and deactivated, and one model can be archived (out of every app and off the internal disk, once the archive disk is known to hold a good copy of each file) and restored.

The full research and plan is the Claude Docs artifact "dedupd: research and implementation plan" (https://claude.ai/artifact/CYuT3GEgaBf9EgKLaMPoxd); read it with the Artifact/Claude Docs tools before making design changes, and update it when a decision changes.

## Licence and public documentation

The project is MIT licensed (`LICENSE`, copyright Descoped); `license = "MIT"` in the workspace manifest, and the app carries the licence in `Contents/Resources/LICENSE` and names it in `NSHumanReadableCopyright`. `README.md` and `docs/` are what people outside read: `how-it-works.md`, `app.md`, `command-line.md`, `apps.md`, `development.md`, `roadmap.md` (the public form of the plan document's roadmap: it says what is decided and marks what is open, and promises nothing that is not tested). The pages in `docs/` are also the documentation site: `site/` is a krantho static site (Next.js) that reads them from `../docs`, and `.github/workflows/pages.yml` publishes it to GitHub Pages, at https://dedupd.com, when `docs/` or `site/` changes on `main`. Each page starts with frontmatter (`title`, `description`, `group`, `order`, values quoted when they hold `: `) and then its own `# Title`; pages link to each other by file name (`apps.md#limitations`) and to pictures as `images/<name>.png`, so they read the same on GitHub and on the site. `docs/README.md` is the site's home page and repeats the introduction, features, status, requirements and install of the root `README.md`; change the two together. The logo is `docs/images/logo.svg` (with `monogram.svg` and `wordmark.svg`); the site's header draws it in `site/components/brand.tsx` and its tab icon is `site/app/icon.svg`, a copy of the monogram. The home page's download button links to the latest release's page in `docs/README.md`; before the build `site/scripts/download-link.mjs` asks GitHub for the latest release and writes its `.dmg` to `site/generated/download.json`, and the docs page component (`site/lib/download.ts`) points the button at it while the page is rendered, never in the built files, whose data Next records with byte lengths (it leaves the link when GitHub cannot be asked), and the Pages workflow builds again on `main` when a release is published. `site/README.md` says how to build it (`cd site && pnpm install && pnpm dev`). They are written in a professional register: what dedupd has today and what it plans, in plain words, with no story telling (no origin stories, no "not X, but Y" lines); what is planned is called planned and what is untested is called untested. Keep them true when behaviour changes; a command, a setting or a default that changes in the code changes there too. The pictures in `docs/images/` are the UI tests' own (`just app-uitest`, then `just app-screenshots`, then copy the ones the pages use), taken on the canned daemon. The canned data must hold nothing personal: paths are under `/Users/example` and the archive disk is `/Volumes/Archive`. Nothing about the owner's Mac (folder names, disk names, what is on it) goes into the repository; it goes into the dossier.

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
just app-install      # the fast round trip on this Mac: build, replace the app in /Applications, bring the installed daemon to the same build, open the app
just app-preview downloading downloads   # the window on a canned daemon, on one of its screens
just app-live 127.0.0.1:8643   # end to end: find, download and list a model through a running scratch daemon
just app-bindings     # rebuild the Rust client and regenerate the Swift bindings
just fixtures         # re-record the control API answers the app's contract tests read
just app-package      # a release build signed with Developer ID in a disk image, not sent to Apple (to check a release)
just app-release      # the release: signed, notarised by Apple, macos-app/build/release/dedupd-<version>.dmg
```

Tests need macOS on APFS, because they create real clones in a temp directory. They use a mock Hugging Face (wiremock) and scratch app folders; they never touch the real app folders or `~/.dedupd`. `cargo test -p dedupd-core --test ollama_mlx` asks the Ollama installed in `/Applications` (only its command line tool, with scratch folders) and is skipped where there is none.

To try the CLI without touching the real store or the real app folders, use a scratch home with a `config.toml` that sets `bionic_models`, `ollama_models`, `hf_cache`, `listen` and `archive` (all optional): `dedupd --home <dir> ...` or `DEDUPD_HOME`. `--endpoint` / `DEDUPD_HF_ENDPOINT` redirects upstream calls. `dedupd install --label <other>` installs the launchd agent under another label for a trial.

Never run a build from this folder against the real home (`~/.dedupd`); always pass `--home <scratch>`. The real home belongs to the installed daemon. The one way a build from here reaches it is `just app-install`, which has the app's own `dedupd install` replace the installed daemon when it is another build (`agent --json` says so in `same_build`, because two builds can carry one version number).

The database has one flat schema and no migrations (`SCHEMA` and `SCHEMA_VERSION` in `db.rs`). A database with another schema number is refused with a message that says to remove it. This is greenfield: when the schema changes, change `SCHEMA` in place, raise the number, and remove the old database (`dedupd.sqlite` with its `-wal` and `-shm` files, in the home and on the archive disk); the next scan adopts the models again from the app folders.

In zsh, write `"${repo}:Q4_K_M"`, not `$repo:Q4_K_M`: `:Q` is a zsh modifier and silently eats the tag.

## Code layout

A Cargo workspace (Rust 2024). `.cargo/config.toml` sets the minimum macOS for every Rust build to 26, the same as the app's; keep the two equal.

- `crates/dedupd-core` — library, no HTTP server.
  - `store` (content-addressed folder, clone-in, restore from the archive disk), `db` (SQLite schema and queries), `hash`, `clone`, `paths`, `config` (`<home>/config.toml`).
  - `hub` (read-only Hub API client), `fetch` (resumable verified downloads), `select` (which files of a repository: a quantization, patterns, or everything), `ollama` (manifest format).
  - `queue` (the download queue: every job is a row in the database, survives a restart, and can be paused, continued, retried and cancelled) and `governor` (pacing: `Retry-After` and the `RateLimit` headers first, AIMD on the number of connections where the server gives no number, then the owner's ceilings on connections and bytes per second).
  - `sources/` (what each app already holds: `bionic`, `hf_cache`, `ollama`), `adopt` (hash, verify against the Hub listing, clone into the store; a file dedupd derived from a stored one, and a manifest that names such a file, are skipped).
  - All SQL lives in `db` (except the read-only queries in `status`); the one clone-then-rename is `clone::clone_into_place`; shared test support is `tests/common`, also used by the server tests.
  - `place` (the three placers), `pull` (resolve, fetch, place; also the `Engine` every caller shares), `archive` (the second copy on another disk: the backup `sync` keeps of every stored file, and archiving one model on request, `archive_plan` and `archive_apply`, with `restore_plan` and `restore` to bring it back; a model that is archived has `repos.archived_at` set, which a later pull or activation clears), `maintain` (reclaim, verify, gc, forget), `status`, `library` (the read-only view of the store model by model, and where each app keeps its files).
  - `refusals` (`<home>/refusals.json`: what an app said when it was asked about a model and said no, by the app's version, the method and the sha256 of the model's `config.json`; `Decision::unless_refused` turns a `ready` answer into `no` with that reason. Ollama's refusal of an MLX stand-in is the one kept so far. A file and not a table, so the schema did not change).
  - `profile` (app profiles). `profiles/*.toml`, compiled in, hold for each app the methods a model can be put into it by, cheapest first: the formats and bit widths a method takes, what rules it out, its cost in new disk space, whether dedupd can do it yet (`built`), and the versions of the app it was tested with. `Profile::decide` gives one answer for an app, a model and the installed version: the state (`ready`, `planned` when the app could take the model and dedupd cannot do it yet, `no`), the method, the cost and a reason in a sentence. `Installed::detect` reads each app's version (the bundle's `Info.plist` under `AppDirs.apps`, or `version.txt` in the Hugging Face cache). `library` puts these decisions on every variant; `usable_by` is the apps whose state is `ready`. A rule about which app takes what belongs in a profile, not in code or in the Swift app.
  - `safetensors` (the index of a safetensors file, and `rewritten_clone`: a clone that lists the same numbers under other names, where a longer index is given room by moving the first tensors to the end of the file) and `ollama_mlx` (the method `clone-with-rewritten-index`: asks the installed Ollama what it would write by having its command line tool import a stand-in of a few hundred KB into a scratch folder, puts the settings it gives into the store, makes one clone per weights file and way of quantizing, and writes the manifest last). A file made this way is a derived file: its row in `placements` names the stored file it was made from in `sha256`, and carries `method` and its own `derived_sha256`. Reclaim and adoption leave derived files alone.
- `crates/dedupd-server` — the daemon's HTTP side. `hubapi` (Hub API), `registry` (Ollama registry), `files` (Range serving, and assembling large files from what clients download through the daemon), `proxy` (pass-through and header cleaning), `control` (`/dedupd/v1`), `discover` (search and one repository's details on the provider, for the app), `moves` (the archive or restore of one model: it runs in the background, one at a time, and the event stream says how far it is), `daemon` (the background pass, the folder watcher and the scheduled store check).
- `crates/dedupd-client` — a small blocking client for the control API, exported through UniFFI: one function per call (`lib.rs`), the records they return (`types.rs`), and `subscribe` (`events.rs`), which follows the daemon's event stream on one thread and calls a listener; it reconnects by itself and tells the listener when the daemon is away. It does not depend on the core and links no TLS stack (the daemon is plain HTTP on loopback).
- `crates/dedupd` — the `dedupd` binary: `main.rs` (the command list; pull, activate, adopt, status, backup), `apps.rs` (apps), `serve.rs` (serve, env), `lifecycle.rs` (deactivate, archive, restore), `maintenance.rs` (reclaim, verify, gc, forget) and `launchd.rs` (install, uninstall, start, stop, agent).

The command line works on the store and database directly, also while the daemon runs; SQLite in WAL mode and atomic renames make that safe. `pull` and `activate` are the exception: when a daemon with the same home answers on the configured address they are sent to it through `dedupd-client`, so one process does the downloading. The daemon's control API under `/dedupd/v1` is what the macOS app talks to, through `dedupd-client`: `status`, `pull`, `scan`, `events` (a stream of JSON lines: one at once, then one after each change once it has settled, and a heartbeat; a line also carries `moving`, the archive or restore that is running or the last one), `downloads` (the queue and its actions), `models`, `locations`, `check`, `forget`, `unplace`, `archive` and `restore` (without `apply` the plan, with it the start of the move), `move`, `gc`, `verify`, `config`, `log`, `restart`, and `hub/search` and `hub/models/{org}/{repo}`. A pull that fails through the daemon stays in the queue as a job that needs attention, and so does one that asked for apps and got into none of them (its notes say why); when it got into some, the notes of the others are in its history. The app must not embed the core, because the daemon has to keep serving when the app is closed.

## The macOS app

`macos-app/` is a SwiftUI app that lives in the menu bar (`LSUIElement`, macOS 26 or later on Apple silicon, Swift 6): `project.yml` (XcodeGen) is the source of truth and the generated `Dedupd.xcodeproj` is committed; `Developer.xcconfig` carries the signing team and includes `Version.xcconfig` (synced from `Cargo.toml` at build time) and an optional gitignored `Local.xcconfig`; SwiftLint runs after every build. Conventions are in `.claude/rules/macos-app.md`. The design and the decisions behind it are on the page "dedupd App Design" (https://claude.ai/artifact/XEhjuDrTZ1NVNBQ8RuSas2).

What it is: the menu bar icon opens a small panel (state at a glance, a scan, a field that takes a model's address or name and opens it in Discover, "Open dedupd"); the main window has a sidebar, a list and an inspector. Closing the window leaves the icon and takes the app out of the Dock; quitting the app leaves the daemon.

- `Dedupd/AppIcon.icon` — the app icon, an Icon Composer document (macOS 26): the monogram as a glass layer and an instrument dial behind it, both SVG in `Assets/`, on a dark blue gradient. macOS makes the dark, clear and tinted versions from it. `just app-icon` opens it in Icon Composer; Xcode compiles it.
- `DedupdClient/` — a static library target holding only what `scripts/build-rust-client.sh` generates from `crates/dedupd-client` (the Swift bindings and their C module, committed). The script runs before every Xcode build and also leaves `build/rust/libdedupd_client.a`, which the app links by path. Built for arm64 only.
- `scripts/bundle-daemon.sh` — runs after every build: builds `dedupd` in release mode, puts it at `Contents/Helpers/dedupd` inside the app and signs it. The Daemon screen installs, updates, starts, stops and uninstalls the launch agent by running that command (`AgentService`); the app has no launchd code of its own.
- `scripts/release.sh` — makes the release (`just app-release`): archives a Release build, checks the signatures of the app and of the command inside it, has Apple notarise the app and then the disk image, staples both, and asks macOS whether it would open them. A Debug build is signed with the Apple Development certificate, a Release build with the team's Developer ID Application certificate and a signed time (`project.yml`). It needs that certificate in the keychain and a notary credential stored as the keychain profile `dedupd-notary` (`xcrun notarytool store-credentials`); `--skip-notary` (`just app-package`) stops short of Apple and names the image `-not-notarised`. A release is tagged `v<version>` on the commit it was built from, and the disk image is attached to a GitHub release (`gh release create`) with its sha256 in the notes. Nothing is sent to Apple or published without the owner's word.
- `Dedupd/` — `App` (entry point, launch options, the app-wide objects, the Dock icon rule), `Services` (`DaemonClient` protocol; `LiveDaemonClient` calls the Rust client off the main thread and turns its event listener into an `AsyncStream`; `FixtureDaemonClient` is a canned daemon with a queue that reacts; `AgentService`; `Notifier`), `Stores` (`DaemonStore` holds the connection, the status and the downloads, fed by the one event subscription; `LibraryStore`, `DiscoverStore`, `StorageStore` and `ServiceStore` hold one screen each and reload when `DaemonStore.revision` changes; `AppSettings`), `Models`, `Views` (`Menu`, `Main`, one folder per screen, `Shared`, `Settings`), `Utilities`.
- Text a person types or pastes is not trusted. `ModelSpec` (`Models/PullRequest.swift`) is the one place that reads it: out of a name or an address on `huggingface.co` or `hf.co` comes only a checked `org/name`, plain words are searched for, and anything else that looks like an address (another host, another scheme, a name before the host, a port, an escaped path) is refused with a sentence and sent nowhere. A ModelScope address is recognised only to say it is not supported yet. The daemon checks again: `pull::is_repo_name` is Hugging Face's own rule for a name, and `parse_model` and `hub/models/{org}/{repo}` refuse what does not pass it before the provider is asked. A new field or a new source goes through these two, not around them.
- `DedupdTests/` — Swift Testing. The contract tests run the real Rust client against a loopback server that replays the daemon's answers recorded in `crates/dedupd-server/tests/fixtures/control/`.
- `DedupdUITests/` — XCUITest, one test per screen on a canned daemon, and one live test that `just app-live` runs.

Launch options, always written as a name and a value: `-uiTestFixture running|downloading|notRunning` (a canned daemon, and preferences of its own), `-pane library|discover|downloads|storage|daemon`, `-mainWindow YES`, `-panelWindow YES` (the menu bar panel in an ordinary window, because a menu bar panel cannot be driven or photographed reliably), `-daemonAddress host:port`, and `-daemonHome <dir>` with `-agentLabel <label>` to point the Daemon screen's install at a scratch home under another label.

## Two tiers of storage

- **Store (warm)**: `~/.dedupd/store` on the internal disk. It must be on the same APFS volume as the app folders, because clones do not cross volumes. This is what apps load from.
- **Archive (cold)**: the folder named by `archive` in `config.toml`, on an external disk (for example `/Volumes/Archive/dedupd`). Same layout as a home folder, so `dedupd --home <archive folder> status` reads it. `backup` copies what it lacks, hashing each file as it is written; `pull`, `adopt` and the daemon do the same whenever the disk is attached. A file missing from the store is copied back from the archive on demand (`Store::ensure_local`). `gc --offload` removes local copies of files that are in the archive and in no app. `archive <org/repo>` does that for one model on purpose: it reads every copy on the archive disk in full (and writes a missing or damaged one again), takes the model out of every app, removes its files from the store and marks it archived. A file another model that is not archived refers to, or an app still has, stays. `restore` copies the files back and puts the model into no app; activating an archived model restores what it needs by itself.

Everything that touches the archive must tolerate the disk being detached.

## What this project is

`dedupd` is an always-on local daemon for one Mac that lets Ollama, Bionic (LM Studio engine), MLX tools, llama.cpp and any `huggingface_hub` client share one on-disk copy of each Hugging Face model, and keep working when Hugging Face is down or a model is removed upstream.

It is a smart proxy and a shared store for models: it sits between the apps and the model sources, so a model is downloaded once and kept, and every file is stored once and shared with each app that can use it. It keeps what it has downloaded (it is not a cache), and it is never in the path that loads a model into memory. Its job is to put files, manifests and cache entries where each app looks for them.

The disk saving comes from the store and from placement, because an app writes whatever it downloads itself: dedupd places a model before the app asks, or swaps the app's copy for a clone right after the download.

### Direction (decided 2026-11-12)

The aim is the least disk space for every pair of app and model. Do not build narrow: more apps, more model sources (ModelScope next) and more platforms (Ubuntu with NVIDIA GPUs) are expected, and each is added as data or as a plug-in, one at a time, with real tests. The plan document's "Direction and roadmap" section has the detail. What was decided:

- Different editions of a model (GGUF, MLX, AWQ and so on) do not share disk; the catalogue groups them as one family.
- dedupd does not convert between formats. It downloads the edition an app needs.
- A file system that can clone files is a prerequisite (APFS; on Linux Btrfs and XFS with reflink). There is no fallback to hard links, symlinks or copies, so ext4 is not supported.
- The words: activate and deactivate (place into an app, take out of it), backup (the copy on the archive disk, made at download), archive (a deliberate act for one model: deactivate it everywhere and move it out of warm storage), restore.
- The roadmap, in order: app profiles; the words and archive/restore per model; the catalogue of families, editions and placements, with everything fetched at download; ModelScope; Linux (command line only); more apps.

## Architecture

Four parts, one process:

1. **Store** — content-addressed folder (`~/.dedupd/store/sha256/ab/<hex>`) on the same APFS volume as the apps. Every file is keyed by its sha256, which for LFS files is the Hub's `lfs.oid`. The git blob id of small non-LFS files is kept as an attribute in the database, because the Hub identifies those files by it. The database (`~/.dedupd/dedupd.sqlite`) archives each revision's Hub answers verbatim, maps `repo@commit/path` to content, and records every app file it knows about.
2. **Fetcher** — downloads from huggingface.co with `Range` resume, verifies sha256, pins branch names to commits.
3. **Front ends** — the Hub API and the Ollama registry API on one localhost port, reading from the store and falling through to the fetcher.
4. **Placers** — one per app; each knows the app's layout and which files it needs, and creates APFS clones (`clonefile(2)`), never copies:
   - Ollama: `~/.ollama/models/blobs/sha256-{hex}` plus a manifest under `manifests/hf.co/{org}/{repo}/{tag}` (0.40 also has a `manifests-v2` tree where the name is a symlink to a manifest blob). A GGUF is one blob, a clone of the stored file. An MLX model is one blob per weights file, a clone with a rewritten index, plus the settings Ollama itself gives for the model; its tag is `latest`.
   - Bionic: `~/.lmstudio/models/{org}/{repo}/{file}`.
   - Hugging Face cache: `~/.cache/huggingface/hub/models--org--repo/{blobs/{etag},snapshots/{commit}/,refs/main}`; also serves llama.cpp and MLX tools.

Two ways in: explicit (`dedupd pull org/repo --for ollama,bionic,mlx` fetches once and places clones) and transparent (an app fetches through the daemon, writes its own copy, and a reclaim pass swaps that copy for a clone). Existing models are adopted by cloning from the app folder into the store; adoption never modifies an app's files.

Commands: `pull`, `activate` (`place` still works as another name for it), `deactivate` (lists what it would remove from the apps; `--apply` removes it), `archive` (lists what it would do; `--apply` does it), `restore`, `adopt`, `status`, `apps` (the profiles and the installed versions; with `org/repo`, each app's answer for a stored model), `backup`, `serve`, `reclaim`, `verify`, `gc`, `forget`, `install`, `uninstall`, `start`, `stop`, `agent`, `env`. `install` copies the running binary to `<home>/bin/dedupd` and starts it from there, so the agent does not depend on where the app or a build folder is; `agent --json` is what the app reads.

The daemon's background pass (started by a folder watcher when an app folder changes, and by a timer every 300 seconds as a fallback; `scan_interval` in `config.toml`): forget app files that are gone, adopt new or changed files in the three app folders once they have been left alone for 20 seconds, swap duplicate copies for clones (`reclaim`, only files unchanged since hashing and older than a minute), and update the archive disk. The daemon also hashes the whole store on a schedule (`verify_days`) and notes the time in `<home>/last-verify.txt`.

## Invariants

- The Ollama layer digest for a GGUF equals the file's sha256, the Hub `lfs.oid`, and the HF cache blob name. This identity is what makes one store serve every app; do not introduce any transformation of weight files in the store. The one exception is on the app's side and is recorded as such: a derived file (an MLX weights file in Ollama) is a clone with a rewritten index and a sha256 of its own, and the store keeps the file as Hugging Face has it.
- Use clones, not symlinks or hard links, for placement. The store and the app folders must be on one APFS volume; only the archive is on another disk. `du` over-reports clones; `df` is correct.
- Nothing is deleted automatically. `gc`, `deactivate` and `archive` are explicit: each lists what it would remove, and removes only with `--apply` (in the app, after the owner has seen the list and agreed). The one automatic change to an app's files is reclaim, which replaces a file with a clone of identical content.
- Archive per repo and commit: weights, small files (configs, tokenizer, README, licence), the Hub tree and model-info JSON, branch/tag to commit mapping, and the Ollama manifest with its config, template and params layers (Hugging Face generates those; they exist nowhere else).
- When upstream errors or returns 404/401/410 for something archived, answer from the archive. For anything not archived, relay the failure.
- Forward `Authorization` to huggingface.co only, never to a CDN. Re-check gated/private repos upstream before serving cached bytes.

## Protocol rules that are easy to get wrong

Hub API (for `huggingface_hub`, llama.cpp):

- Hide Xet, or model bytes bypass the daemon: remove `xetHash` from `/tree` and `/paths-info` entries (keep `lfs.oid`, `lfs.size`), and never emit `X-Xet-Hash`, the Xet `Link` relations or `X-Xet-Refresh-Route`. Still pass `/api/models/{repo}/xet-read-token/{rev}` through for clients with an older cached tree.
- Answer `HEAD /{repo}/resolve/{rev}/{path}` with 200 directly, carrying `X-Repo-Commit`, `ETag` (git sha1 for small files, sha256 for LFS), `Content-Length`, `Accept-Ranges: bytes`. Never redirect to a CDN; the CDN's own ETag is the Xet hash and would produce wrong blob names.
- Rewrite the absolute `Link: rel="next"` pagination header to the daemon's origin.
- Relay status, `X-Error-Code`, `X-Error-Message` and `X-Repo-Commit` verbatim, including on 404.
- The `huggingface_hub` client refuses a plain-HTTP download of a single file over 50 GB. Handle those by pre-placing into the HF cache so the client only sends a HEAD.

Ollama registry:

- `GET /v2/.../blobs/sha256:{d}` without `Range` must answer 307 to a URL with a different hostname; a direct 200 fails with `no Location header`, and so does a redirect to the same hostname, because Ollama follows that one itself. The daemon redirects between its two loopback names (`127.0.0.1` and `localhost`) and listens on both. Ollama then sends parallel `Range` parts of 100 MB to 1 GB.
- Remove `WWW-Authenticate` from relayed registry errors; Ollama rejects a token realm on another host.
- Plain HTTP requires `ollama pull --insecure 127.0.0.1:PORT/org/repo:tag`; the model name then carries the daemon's address. Direct placement keeps the `hf.co/...` name.
- Manifest responses differ by User-Agent; include it in the cache key.

## Upstream limits (not bugs here)

- Ollama cannot pull MLX repos from Hugging Face (the registry returns 400), and its own import (`ollama create`) re-packs safetensors into one file per tensor, a full second copy. dedupd places a 4-bit or 8-bit MLX model as clones with a rewritten index instead (`ollama_mlx`; the findings are under "MLX models in Ollama"). Ollama 0.40.16 cannot run other bit widths, and an MLX model that is not quantized has not been tried. Ollama cannot use split GGUF files.
- Bionic has no endpoint setting. Its downloads go to `search.lmstudio.ai/v1/hf-proxy/...`; the base-URL override exists only in a signed enterprise policy. Bionic is covered by placement and by adopting what it downloads. Avoid folder-level symlinks in its models folder.
- Bionic and LM Studio are separate apps sharing `~/.lmstudio`.

## GitHub

`main` is the default branch. `.github/workflows/ci.yml` runs `just check` on macOS 26 for pull requests into `main` and pushes to it (not for changes that touch only the app, the docs or the scripts). `.github/workflows/claude.yml` has Claude review a pull request once when it is opened or reopened, and answer an `@claude` mention from the owner or a collaborator; that second job runs on macOS, may run the Rust gate, and can push a fix. It needs the secret `CLAUDE_CODE_OAUTH_TOKEN` and the Claude GitHub app on the repository. `.github/renovate.json`: Renovate opens no pull request by itself; the owner ticks what he wants on the dependency dashboard, once a week.

## Harness

`scripts/claude/finish-worktree-removal.sh` removes what a failed `git worktree remove` leaves under `.claude/worktrees/` (on this Mac, Finder can write a `.DS_Store` into the tree while git deletes it; git then stops and the directory is no longer listed). Hooks in `.claude/settings.json` run it at session start, after `git worktree remove|prune`, after `ExitWorktree` and when a subagent stops. Test it with `sh scripts/claude/finish-worktree-removal.test.sh`.

## Spikes

`spikes/` holds the scripts that proved the protocol behaviour. They are evidence and seeds for conformance tests, not product code.

```sh
# Ollama registry served from a folder of sha256-named files (port 8099)
python3 spikes/ollama_registry_from_folder.py <cas-dir> <manifest.json>
ollama pull --insecure localhost:8099/<org>/<repo>:<tag>

# Hub endpoint that hides Xet and streams the files itself (port 8098)
python3 spikes/hub_endpoint_hide_xet.py
HF_HOME=<scratch-dir> HF_ENDPOINT=http://127.0.0.1:8098 hf download <org>/<repo> <file>

# Logging pass-through, to record what a client requests (port 8098)
python3 spikes/hub_endpoint_logging_passthrough.py
```

### MLX models in Ollama

```sh
# A scratch Ollama server and folder; OLLAMA_MODELS must be set for the client too,
# because `ollama create` writes into the default folder otherwise.
OLLAMA_HOST=127.0.0.1:11999 OLLAMA_MODELS=<scratch-dir> ollama serve
OLLAMA_HOST=127.0.0.1:11999 OLLAMA_MODELS=<scratch-dir> \
  python3 spikes/ollama_mlx_shared_clone.py <mlx-model-folder> hf.co/<org>/<repo> --measure
```

What it showed on Ollama 0.40.16 (2026-11-12), with `mlx-community/Qwen3.5-0.8B-4bit` and the owner's `Qwen3.8-27B-MLX-4bit` (three weights files, 16 GB):

- Ollama's import keeps every tensor's bytes, dtype and shape. It renames `X.scales` to `X.weight.scale` and `X.biases` to `X.weight.bias`, and adds `{"quant_type": "int4", "group_size": "64"}` to the file's metadata. Its loader accepts a file that holds many tensors.
- So a weights file can be given to Ollama as a clone whose index (the safetensors header) is rewritten in place. The 16 GB model cost about 130 MB of new disk; it lists, runs with the same answers as Ollama's own import, is still there after the server restarts, and `ollama rm` removes it.
- The settings (the config layer with renderer, parser and capabilities) and the metadata note come from Ollama itself: it imports a stand-in of a few hundred KB (the model's small files and one quantized layer) and gives the same config layer as for the whole model.
- The stand-in also tells what Ollama cannot run. It knows `int4` and `int8` (and `nvfp4`, `mxfp4`, `mxfp8`, not tried). For a 5-bit or 6-bit MLX model it writes no note; its own import of the owner's 5-bit model succeeds and the model then fails to load.
- A model that mixes 4-bit and 8-bit layers fails to load after Ollama's own import (it labels every layer with the default). As one clone per bit width, each listing only its own tensors, it runs.
- The blob's sha256 is that of the rewritten clone, not the Hub's `lfs.oid`, so the first invariant above does not hold for such a placement. This relies on behaviour Ollama does not document.

Built into the product on 2026-11-12 as the `ollama_mlx` module, with two more findings. Ollama's import of a safetensors model runs inside its command line tool: no server takes part, so the daemon runs `ollama create` with a scratch `OLLAMA_MODELS` and a scratch `HOME` and never touches the owner's server or folder while asking. On a fresh folder that import writes its manifest under `manifests-v2/ollama.com/library/`, and it puts a layer's weight, scale and bias into one blob. Measured through the Rust code on a scratch home and a scratch Ollama: the 16 GB `Qwen3.8-27B-MLX-4bit` was placed in 6 seconds for 108 MB of new disk and answered at 44 tokens a second; `mlx-community/Qwen3.5-0.8B-4bit` likewise. Not tried: placing into the owner's real Ollama folder, and an Ollama upgrade with such a model in place.

Use a scratch `HF_HOME` and a tiny model (for example `bartowski/SmolLM2-135M-Instruct-GGUF`, 105 MB) when testing, and remove test models from `~/.ollama/models` and `~/.lmstudio/models` afterwards. Those folders hold the owner's real models (about 270 GB in Bionic).

## Build order

0. Spikes. Done: a multi-file MLX repository placed into Bionic is indexed and loads; a model placed in Ollama's plain `manifests/` tree lists, runs and is removed normally on 0.40.16 and is still listed after the Ollama server restarts, so `manifests-v2` is not written (Ollama's own pull writes both trees; the v2 entry is a symlink to a manifest blob in `blobs/`). `mlx_lm.generate` loads an MLX repository through the daemon, also with Hugging Face unreachable. Not tested: `HTTPS_PROXY` with Ollama (not needed, since the registry front end works).
1. Done. Store, database, `adopt`, `status`, verification against the Hub tree.
2. Done. Fetcher, `pull`, `place`, the three placers, the archive tier.
3. Done. Hub API and Ollama registry front ends; client request sequences replay as tests.
4. Done. launchd agent, `env`, background pass, reclaim, `verify`, `backup`, `gc`, offline serving.
5. Gated-repository auth for Ollama is done (the daemon adds its own Hugging Face token to registry requests that carry none). Not done: Bionic network interception (its downloads are picked up by the background pass instead) and Xet for the daemon's own upstream fetch (plain HTTP works; this would only be faster).

6. Done. The download queue and rate governor in the daemon, the wider control API with its event stream, and the macOS app: Library, Discover, Downloads, Storage and Daemon screens, the menu bar panel, notifications, and install and uninstall of the launch agent through the bundled command. Tried end to end against a scratch daemon with `just app-live`. The first notarised release, 0.1.0, was made with `just app-release` and published as a GitHub release. Not done: updates for the app, Xcode Cloud; installing the agent from the app has only been run against the canned agent, not for real.

Roadmap step 1, app profiles, is done: the profiles as data with the decision shown by the control API, the app and `dedupd apps`, and the placement of MLX models into Ollama as clones with a rewritten index. The copy as a last resort (the proposal's third build step) is left out. Roadmap step 2 is done too: the words (activate, deactivate, backup, archive, restore) in the command line and the app, and archive and restore per model, in the core, the control API, the client and the Library screen. It was tried on a scratch home with a real 105 MB model, through the command line and through a scratch daemon. Not tried: a model of many gigabytes on the real archive disk (a move tells its progress file by file, so one large file shows no progress until it is done). In the names of code and of the control API, "archived" still often means "kept by dedupd" (`from_archive`, `archived_snapshot`, the `archived` field of a search result); the texts people read say "kept". Next: step 3 of the roadmap, the catalogue of families, editions and placements. Waiting on the owner: a first placement of an MLX model into the real Ollama.
