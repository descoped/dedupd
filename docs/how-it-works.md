---
title: "How it works"
description: "The architecture of dedupd: requirements, the content-addressed store, clones, placement into each app and the archive disk."
group: Guide
order: 1
---

# How it works

## Background

Each application that runs models keeps them in its own directory: Ollama in `~/.ollama/models`, Bionic and LM
Studio in `~/.lmstudio/models`, and Hugging Face tools (`hf`, `huggingface_hub`, `mlx_lm`, llama.cpp) in
`~/.cache/huggingface/hub`. A model used by two applications is stored twice.

dedupd stores each file once and provides it to every application as a copy-on-write clone.

## Architecture

dedupd is one process with four parts:

| Part | Responsibility |
|---|---|
| Store | A content-addressed directory of model files, and a SQLite database that maps repositories, revisions and paths to content and records every file placed in an application |
| Fetcher | Downloads from the model source with resume, verifies each file against its sha256, and resolves branch names to commits |
| Front ends | The Hugging Face Hub API and the Ollama registry API on one local port, served from the store |
| Placers | One per application. A placer knows the application's directory layout and creates the clones, manifests and cache entries the application expects |

## Requirements

dedupd depends on the following; without them it does not operate.

- **Copy-on-write file cloning.** The file system must be able to create a second file that shares the data blocks
  of the first. On macOS this is APFS (`clonefile`).
- **One volume.** The store and every application's model directory must be on the same volume. A clone cannot span
  volumes.

dedupd does not substitute symbolic links, hard links or copies when cloning is unavailable.

## The store

```
~/.dedupd/store/sha256/ab/abcdef…      one file per content, named by its sha256
~/.dedupd/dedupd.sqlite                repositories, revisions, files and placements
~/.dedupd/config.toml                  settings
```

A weights file is identified by its sha256. That value is also the `lfs.oid` Hugging Face publishes for the file,
the layer digest Ollama uses, and the blob name in the Hugging Face cache. One stored file therefore satisfies
every application without being transformed, and stored weights files are never modified.

## Clones

A clone is a separate file that shares all data blocks with its source. To the application it is a regular file of
full size at the expected path. On disk the blocks exist once. When either file is modified, only the modified
blocks are written; the other file is unaffected.

Tools that sum file sizes, such as `du`, count a clone at its full size. The free space reported by the file system
(`df`) and the Storage screen of the app reflect actual usage.

## Placement

| Application | What dedupd writes |
|---|---|
| Ollama | One blob per file under `blobs/`, and a manifest under `manifests/hf.co/…` |
| Bionic and LM Studio | The files under `models/{org}/{repo}/` |
| Hugging Face cache | `models--org--repo/` with `blobs/`, `snapshots/{commit}/` and `refs/main` |

## How models enter the store

**Explicit download.** `dedupd pull org/repo --for ollama,bionic`, or Download in the app. The daemon downloads
each file once, verifies it, stores it and activates the model in the named applications.

**Adoption.** A background pass scans the application directories when they change and every 300 seconds. A new
file that has been unmodified for 20 seconds is hashed and adopted: cloned into the store, with the application's
file left untouched.

**Reclaim.** When an application holds a full copy of content that is already in the store, the copy is replaced by
a clone, which releases its space. Reclaim is the only modification dedupd makes to an application's files without
a request, and it only replaces a file with one of identical content.

**Download through the daemon.** Hugging Face tools and Ollama can use the daemon as their endpoint. It implements
the Hugging Face Hub API and the Ollama registry API on `127.0.0.1:8642`, serves from the store and fetches from
the source what it does not hold. `dedupd env` prints the required settings.

## Retention and integrity

For every repository and revision dedupd retains the weights, the small files (configuration, tokenizer, README,
licence), the source's metadata responses, and the Ollama manifest with its template and parameters. A stored
model can therefore be listed, activated and served when the source is unreachable or the model has been removed
there.

Every file is verified against its sha256 at download. The complete store is re-hashed on a schedule, every 7
days by default (`verify_days`).

## The archive disk

With `archive` set in `config.toml` to a directory on another disk, dedupd maintains a second copy of every stored
file there. This backup is updated at download and adoption, and whenever the disk is attached. A file missing from
the store is restored from the archive on demand.

Archiving a single model is an explicit operation. dedupd reads every copy on the archive disk in full and
rewrites any that is missing or damaged. Only then is the model deactivated in the applications and removed from
the internal disk. Restore copies the files back; activating an archived model restores what the application
needs.

All other functions operate with the archive disk detached.

## Terminology

| Term | Meaning |
|---|---|
| activate | Place a stored model into an application, as clones |
| deactivate | Remove it from an application; the stored copy remains |
| adopt | Take a model an application already holds into the store |
| reclaim | Replace an application's duplicate copy with a clone |
| backup | The second copy on the archive disk |
| archive | Move one model off the internal disk; the archive disk holds the only copy |
| restore | Copy an archived model back |
| forget | Remove a model from the library, so that `gc` can remove its files |

## Scope and guarantees

- **dedupd does not load or run models.** It places files where the applications expect them.
- **dedupd does not convert between formats.** A GGUF edition and an MLX edition of a model are different files and
  do not share disk space.
- **Removal is explicit.** `gc`, `deactivate` and `archive` list what they would remove and act only with `--apply`;
  in the app, only after the list has been confirmed.
- **Local only.** The daemon binds to `127.0.0.1` and `localhost` and uses plain HTTP.
- **Credentials.** A Hugging Face token is sent to Hugging Face only, never to the hosts the files are downloaded
  from.
