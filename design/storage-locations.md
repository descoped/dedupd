# Storage locations: stores and archives

Internal developer notes. This folder is not part of the documentation site (the site reads `docs/` only).
Status: **design agreed in outline with the owner on 2026-11-21; not built.** Open points are listed at the end.
What is built today is described in `docs/how-it-works.md` and `.claude/CLAUDE.md`.

## Words

| Word | Meaning |
|---|---|
| **Store** | A **warm** location: a content-addressed folder on one APFS volume. The apps on that volume load their models from clones of its files. There can be several stores, at most one per volume. |
| **Archive** | A **cold** location: a folder anywhere (another SSD, a removable disk, a disk kept in a drawer) that holds copies of models. No app loads from it. There can be several archives, each with a name. |
| **Home** | dedupd's own folder on the internal disk: the database, `config.toml`, the log, the activity history. It knows every store and archive and which of them holds which file. |
| **Activate / Deactivate** | Put a model into an app (clones from the store on the app's volume) / take it out (the store keeps the files). Only stores take part. |
| **Archive** (the action) | Move a model to cold storage: the **owner chooses the archive**; the model leaves every app and every store. |
| **Restore** | Bring an archived model back to warm storage: the **owner chooses the store**; the model is in no app afterwards. |

The rule that does not change: **a store and the app folders it serves are on the same APFS volume**, because a
clone cannot cross volumes. Which volume does not matter: the internal disk, or a fast external SSD formatted as
APFS. "The internal disk" in today's texts means "where the store is" and will be reworded.

## The picture

```
 Internal disk (APFS)                         Fast external SSD (APFS), e.g. /Volumes/Envoy
+-----------------------------------------+  +--------------------------------------------+
| Home ~/.dedupd                          |  |                                            |
|   dedupd.sqlite  (every location,       |  |                                            |
|                   every copy of a file) |  |                                            |
|   config.toml, daemon.log, activity     |  |                                            |
|                                         |  |                                            |
| Store "internal"  (warm)                |  | Store "envoy"  (warm)                      |
|   ~/.dedupd/store/sha256/ab/<hex>       |  |   /Volumes/Envoy/dedupd/store/...          |
|        |  clonefile(2)                  |  |        |  clonefile(2)                     |
|        v                                |  |        v                                   |
|   Bionic   ~/.lmstudio/models           |  |   Ollama   /Volumes/Envoy/ollama/models    |
|   HF cache ~/.cache/huggingface/hub     |  |                                            |
+-----------------------------------------+  +--------------------------------------------+
            |   copy, verified by sha256                 |
            +--------------------+-----------------------+
                                 v
     Archive "drawer"  (cold)                   Archive "nas"  (cold)
     /Volumes/Drawer/dedupd                     /Volumes/NAS/dedupd
       files by sha256                            files by sha256
       + each model's own record                  + each model's own record
         (revisions, Hub answers,                   (readable without the
          Ollama manifests)                          database in the home)
```

- An app folder is served by the store on its volume. No store on that volume: the app cannot be given models
  there, and the app says so and offers to create one.
- A store and an archive are never the same thing. A folder on a store's own volume may be set up as an archive
  (for trials), but it is marked "same disk as store X" and never counts as the copy that lets a model leave warm
  storage.

## Sequences

### Archive a model (owner chooses the archive)

```
Owner            App                 Daemon                        Archive "drawer"     Stores / app folders
  |  Archive…     |                    |                                 |                     |
  |-------------->|  archive plan      |                                 |                     |
  |               |  (repo, "drawer")  |                                 |                     |
  |               |------------------->|  what leaves which app,         |                     |
  |               |                    |  which store, what is copied    |                     |
  |               |<-------------------|                                 |                     |
  |  sees the plan, agrees             |                                 |                     |
  |-------------->|  archive apply     |                                 |                     |
  |               |------------------->|  1. copy what "drawer" lacks -->|                     |
  |               |                    |  2. read every copy back past   |                     |
  |               |                    |     the cache, compare sha256 <-|                     |
  |               |                    |  3. write the model's record -->|                     |
  |               |                    |  4. deactivate in every app ------------------------->|
  |               |                    |  5. remove its files from every store (a file another |
  |               |                    |     model or an app still uses stays) --------------->|
  |               |                    |  6. mark archived (in "drawer"), activity entry       |
  |               |  progress (events) |                                 |                     |
  |               |<-------------------|                                 |                     |
```

Nothing is removed before step 2 has passed for every file. A failure at any step leaves the model as it was
(apps and stores untouched).

### Restore a model (owner chooses the store)

```
Owner            App                 Daemon                       Archives                Store "envoy"
  |  Restore…     |                    |                                |                       |
  |  (to "envoy") |                    |                                |                       |
  |-------------->|------------------->|  find an attached archive that |                       |
  |               |                    |  holds every file ------------>|                       |
  |               |                    |  copy, hashing as written ---------------------------->|
  |               |                    |  clear "archived", activity entry                      |
  |               |<-------------------|  the model is in "envoy", in no app                    |
```

None of the archives attached: the restore is refused and names the archives that hold the model.

### Activate a model in an app

```
Owner            App                 Daemon                        Store on the app's volume   App folder
  |  Activate in Ollama                |                                 |                         |
  |-------------->|------------------->|  which store serves Ollama's    |                         |
  |               |                    |  folder? ("envoy")              |                         |
  |               |                    |  files missing there? copy them |                         |
  |               |                    |  in (from another store, an     |                         |
  |               |                    |  archive, or the provider) ---->|                         |
  |               |                    |  clone into the app ------------------------------------->|
  |               |<-------------------|  activated, activity entry      |                         |
```

## What this changes in the code (outline)

- **Database:** a `locations` table (id, kind `store` or `archive`, name, path, the volume's id) and a table of
  copies (sha256, location, verified at). `repos.archived_at` gains the archive(s) it went to. Indexes on the file
  names a revision lists (`revision_files` by sha256 and git id), which also gives an orphan in a clean-up its name.
  The schema changes in place (no migrations): the database in the home is removed and the next scan adopts the
  models again. **Archived models must survive that:** each archive holds a record per model that a scan reads back.
- **Core:** `Store` becomes one of several; every placement asks which store serves the app folder's volume.
  `archive_plan`/`archive_apply` take the archive; `restore_plan`/`restore` take the store.
- **Control API and app:** the Storage screen lists every store and archive; Archive asks "to which archive", Restore
  asks "to which store"; settings add, rename and remove locations.

## Open points

1. **Backup at download.** Today every downloaded file is also copied to the one archive. With several archives:
   no automatic copy, or archives marked to receive every download?
2. **Downloads go to which store:** the store of the apps chosen, or a default store?
3. **Moving a model between stores** (an app folder moved to another volume): one action, or deactivate, archive
   and restore?
4. **Export and import:** is an archive on a removable disk enough (each model's record makes it readable on its
   own), or a single file as well?
5. **A pin per model** (stays on one revision until moved): wanted?
