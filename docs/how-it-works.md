---
title: "How it works"
description: "The architecture of dedupd: requirements, the content-addressed store, clones, placement into each app and the archives."
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

A placement does not replace what an application already has. A file with other content is left alone and reported.
So is an Ollama manifest under the same name that belongs to another model. In the Hugging Face cache, the branch
file is written last and only when every file was placed; a branch that names another commit is moved by a new
answer from the source, and left alone when the model is placed from the kept copy. When one application cannot be
given a model, the others are still given it, and the job says which took it and why the other did not.

## How models enter the store

**Explicit download.** `dedupd pull org/repo --for ollama,bionic`, or Download in the app. The daemon downloads
each file once, verifies it, stores it and activates the model in the named applications.

**Adoption.** A background pass scans the application directories when they change and every 300 seconds. A new
file that has been unmodified for 20 seconds is hashed and adopted: cloned into the store, with the application's
file left untouched. The same pass records files that are in the store but not in the database, such as a model's
README that no application downloaded; each is hashed first and recorded only when its content matches its name
and a model dedupd knows refers to it. Any other file in the store is left out of the records, so `gc` never lists
it.

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
there. The exception is a gated or private model, and a model whose source metadata dedupd does not hold: a client
gets it through the daemon only while the source can confirm that the client has access.

Every file is verified against its sha256 at download. The complete store is re-hashed on a schedule, every 7
days by default (`verify_days`).

## Stores

A store is a content-addressed folder on one APFS volume; the applications on that volume are given clones of its
files. The store in dedupd's home is always there. More can be added, at most one per volume, each with a name, for
example on an external SSD that holds Ollama's models while Bionic's stay on the internal disk. A download goes to the
store that serves every application it is for, otherwise to the store in dedupd's home. Activating a model in an
application whose store lacks it copies the files into that store from another store or an archive first, with the
size shown before. An application on a volume with no store cannot be given models. Removing a store from the list
deletes nothing.

## Archives

An archive is a folder on another disk (an external SSD, a removable disk, a network volume) that holds copies of
models. No application loads from an archive. There can be several, each with a name; the default name is the
disk's. Each archive has the layout of a home folder, database included, so `dedupd --home <archive folder> status`
reads it on any Mac.

One archive can be the backup archive: it receives a copy of every stored file, at download and adoption and
whenever its disk is attached. The other archives receive only the models archived to them.

Archiving a single model is an explicit operation, and the owner chooses the archive. dedupd reads every copy in
that archive in full and rewrites any that is missing or damaged. Only then is the model deactivated in the
applications and removed from every store. Restore copies the files into the store the owner chooses (the one in
dedupd's home by default), from another store or any attached archive that holds them; activating an archived model
restores what the application needs.

All other functions operate with the archives' disks detached.

## Terminology

| Term | Meaning |
|---|---|
| activate | Place a stored model into an application, as clones |
| deactivate | Remove it from an application; the stored copy remains |
| adopt | Take a model an application already holds into the store |
| reclaim | Replace an application's duplicate copy with a clone |
| backup | The copy of every stored file in the backup archive |
| store | A folder on one APFS volume whose files the applications on that volume are given as clones |
| archive | Move one model out of every store to an archive the owner chooses |
| restore | Copy an archived model back into a store the owner chooses |
| forget | Remove a model from the library, so that `gc` can remove its files |

## Scope and guarantees

- **dedupd does not load or run models.** It places files where the applications expect them.
- **dedupd does not convert between formats.** A GGUF edition and an MLX edition of a model are different files and
  do not share disk space.
- **Removal is explicit.** `gc`, `deactivate` and `archive` list what they would remove and act only with `--apply`;
  in the app, only after the list has been confirmed.
- **Local only.** The daemon binds to `127.0.0.1` and `localhost` and uses plain HTTP. It answers no request from a
  web page: one that carries an `Origin` header, or that addresses the daemon by another name than a loopback one,
  is refused on every route.
- **Credentials.** A Hugging Face token is sent to Hugging Face only, never to the hosts the files are downloaded
  from. dedupd only reads with it: the models, and the account's likes for the favorites. A client that asks the
  daemon for a model without a token of its own (Ollama, for a gated model) is served with dedupd's token; any other
  request the daemon passes on to Hugging Face carries the client's own token or none.
- **Gated and private models.** A stored file of a gated or private model is served to a client only after Hugging
  Face has been asked whether it may be had, with the client's token or with dedupd's when the client has none, so it
  is not served while Hugging Face is unreachable.
