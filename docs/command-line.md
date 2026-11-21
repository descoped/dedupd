---
title: "The command line"
description: "Reference for the dedupd command: every command, its options and the settings in config.toml."
group: Guide
order: 3
---

# The command line

`dedupd` is one command. `cargo build --release` leaves it at `target/release/dedupd`; `dedupd install` copies it to
`~/.dedupd/bin/dedupd` and runs the daemon from there. The macOS app carries the same command inside it.

The command line works on the store directly, also while the daemon runs. `pull` and `activate` are handed to the
daemon when one is running, so one process does the downloading. So are `archive --apply` and `restore`: the daemon
keeps its downloads away from the model while its files move.

## A model's name

| You write | It means |
|---|---|
| `org/repo` | the whole repository |
| `org/repo:Q4_K_M` | one quantization of a GGUF repository |
| `org/repo:Model-Q4_K_M` | one GGUF file by its name without `.gguf`, for a repository where two files carry the same quantization |
| `org/repo@revision` | a branch, tag or commit |

In zsh, put the name in quotes when it comes from a variable: `"${repo}:Q4_K_M"`. Without them `:Q` is read as a
zsh modifier and the tag is lost.

The apps are named `bionic`, `ollama` and `hf` (the Hugging Face cache; `mlx` and `llama.cpp` mean the same).

## Getting and placing models

| Command | What it does |
|---|---|
| `dedupd pull <model> --for <apps>` | Download a model into the store once and activate it in the apps, as clones. `--include '*Q8_0*.gguf'` takes only matching files. |
| `dedupd activate <model> --for <apps>` | Activate a stored model in apps, without contacting Hugging Face. |
| `dedupd deactivate <repo> [--from <apps>]` | Take a model out of apps; the stored copy stays. Lists what it would remove; `--apply` removes it. |
| `dedupd adopt` | Take the models the apps already hold into the store, without changing the apps' files. `--dry-run` writes nothing. |
| `dedupd reclaim` | Swap app files that duplicate stored content for clones, which frees their space. `--dry-run` only shows. |

## Looking

| Command | What it does |
|---|---|
| `dedupd status` | What the store holds and which app files it covers. `--json` for scripts. |
| `dedupd apps` | How each app can be given a model. With `org/repo`: each app's answer for that stored model. |
| `dedupd verify` | Hash every stored file and compare it with its name. `--archive` checks the copies in the backup archive. A file that is not on this Mac while its archive's disk is away is reported as not looked for, not as missing. |

## Archives

| Command | What it does |
|---|---|
| `dedupd backup [dir]` | Copy the store and the database to the backup archive. |
| `dedupd archive <repo> [--to <name>]` | Move one model to an archive: out of every app and off the internal disk. Without `--to`, the backup archive, or the only archive there is. Lists what would happen; `--apply` does it. |
| `dedupd restore <repo> [--to <name>]` | Copy an archived model back into the store named, from another store or any attached archive that holds it. Without `--to`, the store in dedupd's home. |

## Cleaning up

| Command | What it does |
|---|---|
| `dedupd forget <repo>` | Say what dropping a model from the library would drop; `--apply` drops its records, so `gc` can remove its files. Refused while an app holds the model. |
| `dedupd gc` | Show stored files nothing refers to; `--apply` removes them, here and in every archive (while an archive's disk is away, their records stay so a later `gc` removes the copies there). `--offload` also lists local copies of files that are in the backup archive and in no app, each with the model and path it is kept under where a kept model lists it. |

## The daemon

| Command | What it does |
|---|---|
| `dedupd serve` | Run the daemon in the foreground. One daemon runs on a home; a second one is refused. |
| `dedupd install` | Run the daemon at login, as a launchd agent. `--env` also points apps started from the Dock or Finder at it. |
| `dedupd uninstall` | Stop the daemon and remove the agent. The store stays. |
| `dedupd stop` / `dedupd start` | Stop the daemon until the next login, and start it again. `stop` and `uninstall` also take `HF_ENDPOINT` out of the login session when it names the daemon's address. |
| `dedupd agent` | Whether the agent is installed and running. `--json` for scripts. |
| `dedupd env` | Print the settings that point Hugging Face tools and Ollama at the daemon. |

When the daemon runs, `pull`, `activate`, `deactivate --apply`, `archive --apply`, `restore`, `forget --apply` and
`gc --apply` are sent to it, so one process downloads and removes. Without a daemon the command does the work itself.

The daemon cuts its log (`~/.dedupd/daemon.log`) back to the last megabyte when it starts and finds it larger than
16 MB.

`dedupd env` prints:

```sh
# Hugging Face tools (hf, huggingface_hub, mlx_lm, llama.cpp): add to ~/.zshrc
export HF_ENDPOINT=http://127.0.0.1:8642

# Apps started from the Dock or Finder: run once per login, or use
# `dedupd install --env` to have the daemon do it.
launchctl setenv HF_ENDPOINT http://127.0.0.1:8642

# Ollama has no endpoint setting. Either let dedupd place the model:
dedupd pull org/repo:Q4_K_M --for ollama
# or pull through the daemon (the model name then carries its address):
ollama pull --insecure 127.0.0.1:8642/org/repo:Q4_K_M
```

## Options for every command

| Option | What it does |
|---|---|
| `--home <dir>` (or `DEDUPD_HOME`) | Where dedupd keeps its store and database. Default `~/.dedupd`. |
| `--endpoint <url>` (or `DEDUPD_HF_ENDPOINT`) | The Hugging Face endpoint. Default `https://huggingface.co`. |

## Settings

`~/.dedupd/config.toml`. Every line is optional; the app's Daemon screen edits the same file.

```toml
# The backup archive: a folder on another disk that gets a copy of every stored file.
archive = "/Volumes/Archive/dedupd"

# More archives, each with a name. They hold only the models archived to them.
# The backup archive may be listed too, which gives it its name. The app's Storage
# screen edits this list, and the daemon uses a change at once.
[[archives]]
name = "Drawer"
path = "/Volumes/Drawer/dedupd"

# More stores, each on an APFS disk of its own that holds app folders: the apps
# whose folders are on that disk are given models from it. The store in dedupd's
# home serves the internal disk. The app's Storage screen edits this list.
[[stores]]
name = "Envoy"
path = "/Volumes/Envoy/dedupd"

# The address the daemon listens on. It must be an address of this Mac itself
# (127.0.0.1, [::1] or localhost); any other is refused.
listen = "127.0.0.1:8642"

# The Hugging Face endpoint the daemon fetches from. Default https://huggingface.co.
# --endpoint and DEDUPD_HF_ENDPOINT take precedence.
hf_endpoint = "https://huggingface.co"

# Set HF_ENDPOINT to the daemon's address for apps started from the Dock or Finder,
# through launchctl setenv when the daemon starts. Default false; `dedupd install --env` sets it to true.
set_env = false

# The app folders, when they are not in the usual place.
bionic_models = "/Users/you/.lmstudio/models"
ollama_models = "/Users/you/.ollama/models"
hf_cache = "/Users/you/.cache/huggingface/hub"

# Swap duplicate app files for clones in the background. Default true.
reclaim = true

# Days between full checks of the store; 0 turns them off. Default 7.
verify_days = 7

# Seconds between looks at the app folders when nothing was seen to change. Default 300.
scan_interval = 300

# The most downloads at once, and bytes per second for all of them (0 is no limit).
max_connections = 8
speed_limit = 0
```

For gated repositories dedupd uses the same Hugging Face token as other tools: `HF_TOKEN`, or the token file written
by `hf auth login`. A client that reads a model through the daemon without a token of its own is served with that
token. Other requests to Hugging Face through the daemon (the account, datasets, spaces, uploads) need the client's
own token.

## Running against a scratch home

For evaluation without affecting existing models, use a separate home whose `config.toml` points the three
application directories at scratch directories:

```sh
dedupd --home /tmp/dedupd-trial status
```
