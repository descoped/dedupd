# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Status

Phase 1 of the build order is implemented: the store, the database, `dedupd adopt` and `dedupd status`. Phases 2 onward (fetcher, placers, HTTP front ends, daemon, Swift app) are not started. `spikes/` holds three standalone Python scripts (standard library only, run with `python3`) that proved the protocol behaviour.

The full research and plan is the Claude Docs artifact "dedupd: research and implementation plan" (https://claude.ai/artifact/CYuT3GEgaBf9EgKLaMPoxd); read it with the Artifact/Claude Docs tools before making design changes, and update it when a decision changes.

## Commands

```sh
cargo build --release                      # binary at target/release/dedupd
cargo test --workspace                     # all tests
cargo test -p dedupd-core --test adopt     # end-to-end adoption tests (fake Hub, fake models folder)
cargo test -p dedupd-core hub::tests::parses_link_header   # a single test
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

Tests need macOS on APFS, because they create real clones in a temp directory. They never touch the real app folders or `~/.dedupd`.

To try the CLI without touching the real store, point it at scratch folders: `dedupd --home <dir> adopt --models-dir <dir>` (or `DEDUPD_HOME`). `DEDUPD_HF_ENDPOINT` redirects Hub calls, which the tests use with a mock server.

## Code layout

A Cargo workspace (Rust 2024):

- `crates/dedupd-core` — library. `store` (content-addressed folder, clone-in), `db` (SQLite schema and queries), `hub` (read-only Hugging Face API client), `sources/` (one module per app layout; only `bionic` so far), `adopt` (hash, verify against the Hub listing, clone, record), `status`, `hash`, `clone`.
- `crates/dedupd` — the `dedupd` binary. For now it calls the core directly.

Planned, not yet built: the binary gains a `serve` mode run by launchd and the other commands become clients of its local control API; a thin client crate is exposed to Swift through UniFFI for the macOS app. The app must not embed the core, because the daemon has to keep serving when the app is closed.

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

Planned commands: `pull`, `place`, `adopt`, `status`, `verify`, `backup`, `gc`.

## Invariants

- The Ollama layer digest for a GGUF equals the file's sha256, the Hub `lfs.oid`, and the HF cache blob name. This identity is what makes one store serve every app; do not introduce any transformation of weight files.
- Use clones, not symlinks or hard links, for placement. Everything must be on one APFS volume. `du` over-reports clones; `df` is correct.
- Nothing is deleted automatically. Everything fetched or adopted is pinned; `gc` is explicit and shows what it would remove first.
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

0. Spikes still open: multi-file MLX repo placed into Bionic; Ollama `manifests-v2` form surviving restart and prune; `HTTPS_PROXY` with Ollama.
1. Store, database, `adopt`, `status`, verification against the Hub tree.
2. Fetcher, `pull`, the three placers.
3. Hub API and Ollama registry front ends; recorded request traces replay as tests.
4. launchd agent, environment setup, folder watcher, reclaim pass, `verify`, `backup`, offline serving.
5. Optional: Bionic network interception, Xet for the proxy's own upstream fetch, gated-repo auth for Ollama.

Archive behaviour is built into each phase, not added at the end. Phase 4 is done when a model deleted from a test repo upstream can still be placed into all three apps.
