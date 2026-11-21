//! Upkeep: swapping duplicate app files for clones, checking the store, and
//! explicit removal. Nothing here runs unless the owner or the daemon asks.

use crate::clone::{clone_into_place, same_volume};
use crate::db::{BlobRow, Db, PROVIDER_HF, lock};
use crate::hash::hash_file;
use crate::place::{AppDirs, hf_repo_dir};
use crate::store::{Fingerprint, Store};
use anyhow::{Context, Result, bail};
use dedupd_api::app::App;
use dedupd_api::moves::{KeptFile, PlannedFile, StoredFile};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

/// How long a duplicate must have been left alone before `reclaim` swaps it
/// for a clone, so a file an app is still writing is not touched.
pub const RECLAIM_MIN_AGE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ReclaimReport {
    /// App files replaced by a clone of the stored file.
    pub swapped: usize,
    /// Disk space those files were using on their own.
    pub bytes: u64,
    /// Files left alone, as `(path, reason)`.
    pub skipped: Vec<(String, String)>,
}

/// Replace app files that are separate copies of stored content with clones
/// of the stored file, which frees the copy's disk space.
///
/// A file is only touched when it is exactly as it was when it was hashed and
/// has not been written for `min_age`. The swap is a rename, so a program
/// that has the old file open keeps reading it undisturbed.
pub fn reclaim(
    db: &Mutex<Db>,
    store: &Store,
    min_age: Duration,
    dry_run: bool,
) -> Result<ReclaimReport> {
    let mut report = ReclaimReport::default();
    let candidates: Vec<_> = lock(db)
        .placements()?
        .into_iter()
        // A derived file holds other bytes than the stored one on purpose.
        .filter(|p| !p.shared && p.derived_sha256.is_none())
        .collect();
    for p in candidates {
        let path = Path::new(&p.path);
        let result = (|| -> Result<Option<String>> {
            let Ok(meta) = fs::symlink_metadata(path) else {
                return Ok(Some("the file is gone".into()));
            };
            if !meta.is_file() {
                return Ok(Some("not a regular file".into()));
            }
            let recorded = p.fingerprint();
            if Fingerprint::of(path)? != recorded {
                return Ok(Some("changed since it was hashed".into()));
            }
            let age = SystemTime::now()
                .duration_since(meta.modified()?)
                .unwrap_or_default();
            if age < min_age {
                return Ok(Some("written too recently".into()));
            }
            if !store.contains(&p.sha256) {
                return Ok(Some("its content is not in the local store".into()));
            }
            let blob = store.blob_path(&p.sha256);
            if !same_volume(path, &blob)? {
                return Ok(Some("on a different volume from the store".into()));
            }
            if dry_run {
                return Ok(None);
            }

            clone_into_place(&blob, path, |tmp| {
                // The clone takes the place of the app's file, so it carries
                // the same permissions and modification time.
                fs::set_permissions(tmp, meta.permissions())?;
                fs::File::open(tmp)?.set_modified(meta.modified()?)?;
                if Fingerprint::of(path)? != recorded {
                    bail!("changed while it was being replaced");
                }
                Ok(())
            })?;
            lock(db).set_placement_shared(&p.path, Fingerprint::of(path)?.mtime_ns)?;
            Ok(None)
        })();
        match result {
            Ok(None) => {
                report.swapped += 1;
                report.bytes += p.size;
            }
            Ok(Some(reason)) => report.skipped.push((p.path, reason)),
            Err(e) => report.skipped.push((p.path, format!("{e:#}"))),
        }
    }
    Ok(report)
}

/// Forget app files that no longer exist. Returns how many were forgotten.
/// The stored content stays; only `gc` removes anything from the store.
pub fn prune_placements(db: &Mutex<Db>) -> Result<usize> {
    let gone: Vec<String> = lock(db)
        .placements()?
        .into_iter()
        .filter(|p| fs::symlink_metadata(&p.path).is_err())
        .map(|p| p.path)
        .collect();
    let db = lock(db);
    for path in &gone {
        db.delete_placement(path)?;
    }
    Ok(gone.len())
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct VerifyReport {
    pub checked: usize,
    pub bytes: u64,
    /// Files whose content no longer hashes to their name.
    pub damaged: Vec<String>,
    /// Files that could not be read, each with why. Nothing is known about
    /// their content; they are not counted as damaged.
    pub unreadable: Vec<(String, String)>,
    /// Files the database lists that are in neither the store nor the archive.
    pub missing: Vec<String>,
    /// Files that are not in the store while the archive disk is away, so
    /// nothing says whether it has them. An archived or offloaded file is
    /// one of these until the disk is attached again.
    pub archive_away: usize,
    /// Files that are only on the archive disk.
    pub only_in_archive: usize,
    /// When the archive disk is checked: files that are only on the
    /// internal disk, because the backup has not copied them yet.
    pub not_backed_up: usize,
}

/// Hash every stored file and compare with its name. With `archive`, check
/// the archive disk's copies instead of the local ones.
pub fn verify(db: &Mutex<Db>, store: &Store, archive: bool, jobs: usize) -> Result<VerifyReport> {
    let blobs = lock(db).all_blobs()?;
    let root = match (archive, store.archive()) {
        (true, Some(a)) if !a.attached() => bail!(
            "the archive disk is not attached ({} not found)",
            a.root().display()
        ),
        (true, Some(a)) => a.clone(),
        (true, None) => bail!("no archive folder is configured"),
        (false, _) => store.home().clone(),
    };

    // Taken once: the archives are asked about every file that is not here.
    let attached = store.attached_archives();
    let some_away = store.archives().iter().any(|a| !a.home.attached());
    let next = AtomicUsize::new(0);
    // Set when the archive disk goes away while it is being checked.
    let gone = std::sync::atomic::AtomicBool::new(false);
    let report = Mutex::new(VerifyReport::default());
    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| {
                loop {
                    if gone.load(Ordering::Relaxed) {
                        break;
                    }
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(blob) = blobs.get(i) else { break };
                    let path = root.blob_path(&blob.sha256);
                    if !path.is_file() {
                        let mut r = report.lock().expect("lock");
                        if archive && store.contains(&blob.sha256) {
                            r.not_backed_up += 1;
                        } else if !archive && crate::store::in_any(&attached, &blob.sha256) {
                            r.only_in_archive += 1;
                        } else if !archive && some_away {
                            // Any archive whose disk is away may hold it:
                            // a cold archive is detached most of the time.
                            r.archive_away += 1;
                        } else {
                            r.missing.push(blob.sha256.clone());
                        }
                        continue;
                    }
                    let read = hash_file(&path, false);
                    let mut r = report.lock().expect("lock");
                    match read {
                        Ok(hashes) => {
                            r.checked += 1;
                            r.bytes += blob.size;
                            if hashes.sha256 != blob.sha256 {
                                r.damaged.push(blob.sha256.clone());
                            }
                        }
                        // A file that cannot be read says nothing about
                        // its content. When it is the archive disk that
                        // went away, nothing after this can be told
                        // either: the check stops.
                        Err(e) => {
                            r.unreadable.push((blob.sha256.clone(), e.to_string()));
                            if archive && !root.attached() {
                                gone.store(true, Ordering::Relaxed);
                                break;
                            }
                        }
                    }
                }
            });
        }
    });
    let mut report = report.into_inner().expect("lock");
    if gone.load(Ordering::Relaxed) {
        bail!(
            "the archive disk went away while it was checked, after {} files; nothing is known about the rest",
            report.checked
        );
    }
    report.damaged.sort();
    report.missing.sort();
    report.unreadable.sort();
    Ok(report)
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct GcPlan {
    /// Stored files that no app file refers to, and no model in the library
    /// through a revision, an Ollama manifest or a file it held. A model's
    /// files become orphans only once it is forgotten.
    pub orphans: Vec<BlobRow>,
    /// Stored files that are safely on the archive disk and that no app
    /// uses. Removing the local copy frees space; they come back on demand.
    pub offload: Vec<BlobRow>,
    /// What a kept model calls a listed file, by the file's sha256:
    /// `org/repo: path`. An orphan has no name: no kept model lists it.
    pub names: BTreeMap<String, String>,
}

impl GcPlan {
    /// The plan as the control API shows it; `freed` once it was done.
    pub fn answer(&self, freed: Option<u64>) -> dedupd_api::moves::GcPlan {
        let files = |list: &[BlobRow]| -> Vec<StoredFile> {
            list.iter()
                .map(|b| StoredFile {
                    sha256: b.sha256.clone(),
                    size: b.size,
                    git_oid: b.git_oid.clone(),
                    known_as: self.names.get(&b.sha256).cloned(),
                })
                .collect()
        };
        let bytes = |list: &[BlobRow]| list.iter().map(|b| b.size).sum();
        dedupd_api::moves::GcPlan {
            orphans: files(&self.orphans),
            offload: files(&self.offload),
            orphan_bytes: bytes(&self.orphans),
            offload_bytes: bytes(&self.offload),
            freed,
        }
    }
}

/// Work out what `gc` would remove. Removes nothing.
pub fn gc_plan(db: &Mutex<Db>, store: &Store, offload: bool) -> Result<GcPlan> {
    // The rows are read under the database lock; the disks, of which the
    // archive may be slow to answer, are asked after it is given back.
    let (orphans, unplaced, listed) = {
        let db = lock(db);
        let (unplaced, listed) = match offload && store.archive().is_some() {
            true => (db.unplaced_blobs()?, db.listed_names()?),
            false => (Vec::new(), BTreeMap::new()),
        };
        (db.unreferenced_blobs()?, unplaced, listed)
    };
    let mut plan = GcPlan {
        orphans,
        offload: Vec::new(),
        names: BTreeMap::new(),
    };
    if offload && let Some(archive) = store.archive() {
        let orphans: HashSet<&str> = plan.orphans.iter().map(|o| o.sha256.as_str()).collect();
        plan.offload = unplaced
            .into_iter()
            .filter(|b| !orphans.contains(b.sha256.as_str()))
            .filter(|b| store.contains(&b.sha256))
            .filter(|b| fs::metadata(archive.blob_path(&b.sha256)).is_ok_and(|m| m.len() == b.size))
            .collect();
        // A revision lists a large file by its sha256 and a small one by
        // its git id.
        for blob in &plan.offload {
            let name = listed
                .get(&blob.sha256)
                .or_else(|| blob.git_oid.as_ref().and_then(|oid| listed.get(oid)));
            if let Some(name) = name {
                plan.names.insert(blob.sha256.clone(), name.clone());
            }
        }
    }
    Ok(plan)
}

/// Carry out a plan. Returns the bytes freed on the local disk.
pub fn gc_apply(db: &Mutex<Db>, store: &Store, plan: &GcPlan) -> Result<u64> {
    // Remove the local copy; the bytes that freed.
    let remove_local = |blob: &BlobRow| -> Result<u64> {
        let path = store.blob_path(&blob.sha256);
        if !path.is_file() {
            return Ok(0);
        }
        fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        Ok(blob.size)
    };
    let mut freed = 0;
    // An archive whose disk is away: the orphans' local copies go, and
    // their records stay, so a later `gc` with every disk attached still
    // knows of the copies there and removes them.
    let archives = store.archives();
    let archive_away = archives.iter().any(|a| !a.home.attached());
    for blob in &plan.orphans {
        {
            // Asked again, and removed, under the database lock: a model
            // that came to name the file since the plan was made (a pull,
            // an adoption) records its listing under the same lock before
            // it looks in the store, so it either keeps the file here or
            // finds it gone and fetches it.
            let db = lock(db);
            if !db.is_unreferenced(&blob.sha256)? {
                continue;
            }
            freed += remove_local(blob)?;
            if !archive_away {
                db.delete_blob(&blob.sha256)?;
            }
        }
        if !archive_away {
            for archive in &archives {
                let _ = fs::remove_file(archive.home.blob_path(&blob.sha256));
            }
        }
    }
    // The local copy of a file to offload goes only once the archive disk's
    // copy has been read in full and is good; a damaged copy or a detached
    // disk leaves it.
    for blob in &plan.offload {
        let good = store.archive().is_some_and(|archive| {
            hash_file(&archive.blob_path(&blob.sha256), false)
                .is_ok_and(|h| h.sha256 == blob.sha256)
        });
        if good {
            freed += remove_local(blob)?;
        }
    }
    Ok(freed)
}

/// Drop a repository from the library records, so that `gc` sees its files as
/// unreferenced. Refused while an app still has files of the repository.
pub fn forget(db: &Mutex<Db>, repo: &str) -> Result<()> {
    prune_placements(db)?;
    let mut db = lock(db);
    let id = forgettable(&db, repo, |_| true)?;
    db.delete_repo(id)
}

/// Whether `repo` could be forgotten now: it is in the library and no app
/// holds a file of it. The error says which of the two is in the way.
/// Nothing is changed: the record of an app file that is gone is passed
/// over here, where `forget` removes it first.
pub fn forget_check(db: &Mutex<Db>, repo: &str) -> Result<()> {
    forgettable(&lock(db), repo, |path| {
        std::fs::symlink_metadata(path).is_ok()
    })
    .map(|_| ())
}

/// The repository's id when no app holds a file of it; `counts` says
/// which recorded app files do.
fn forgettable(db: &Db, repo: &str, counts: impl Fn(&str) -> bool) -> Result<i64> {
    let id = db.known_repo(repo)?;
    let in_use: Vec<String> = db
        .placements()?
        .into_iter()
        .filter(|p| p.repo == repo && counts(&p.path))
        .map(|p| p.path)
        .collect();
    if let Some(first) = in_use.first() {
        bail!(
            "{repo} still has {} files in app folders (for example {first}). Remove the model from the apps first.",
            in_use.len()
        );
    }
    Ok(id)
}

/// What removing a repository from one app would do. Nothing in it is
/// removed until `unplace_apply` is given the plan.
#[derive(Debug, Clone)]
pub struct UnplacePlan {
    pub repo: String,
    pub app: App,
    /// The app's files that would be removed. Each is exactly as it was
    /// when it was hashed, and its content stays in the store.
    pub remove: Vec<PlannedFile>,
    /// Links and manifests that go with them.
    pub links: Vec<Link>,
    /// Files that would be left alone, and why.
    pub keep: Vec<KeptFile>,
    pub bytes: u64,
}

/// A link or manifest in an app's folder that names app files: a snapshot
/// link in the Hugging Face cache, or an Ollama manifest. It goes only with
/// every file it names that would otherwise stay.
#[derive(Debug, Clone)]
pub struct Link {
    pub path: String,
    /// The files of `remove` that must all go for this link to go.
    pub goes_with: Vec<String>,
}

impl UnplacePlan {
    /// Only the files the owner agreed to, out of this plan, and the links
    /// that name nothing else.
    /// A model leaves Ollama whole or not at all, so there the owner agrees
    /// to every layer or none is removed.
    pub fn only(mut self, agreed: &[String]) -> Self {
        let agreed: HashSet<&str> = agreed.iter().map(String::as_str).collect();
        if self.app == App::Ollama && !self.remove.iter().all(|f| agreed.contains(f.path.as_str()))
        {
            self.keep_all(Kept::RestOfModelStays);
            return self;
        }
        self.remove.retain(|f| agreed.contains(f.path.as_str()));
        self.links
            .retain(|l| l.goes_with.iter().all(|p| agreed.contains(p.as_str())));
        self.bytes = self.remove.iter().map(|f| f.size).sum();
        self
    }

    /// Keep every file the plan would remove, and every link.
    fn keep_all(&mut self, why: Kept) {
        self.keep.extend(self.remove.drain(..).map(|f| KeptFile {
            path: f.path,
            why: why.sentence().to_string(),
        }));
        self.links.clear();
        self.bytes = 0;
    }

    /// The paths of the links and manifests that would be removed.
    pub fn link_paths(&self) -> Vec<String> {
        self.links.iter().map(|l| l.path.clone()).collect()
    }

    /// The plan as the control API shows it; `removed` once it was done.
    pub fn answer(self, removed: Option<usize>) -> dedupd_api::moves::UnplacePlan {
        let links = self.link_paths();
        dedupd_api::moves::UnplacePlan {
            repo: self.repo,
            app: self.app,
            links,
            remove: self.remove,
            keep: self.keep,
            bytes: self.bytes,
            removed: removed.map(|n| n as u64),
        }
    }
}

/// Work out what removing `repo` from `app` would delete. The store keeps
/// every file, so the model can be placed again later.
pub fn unplace_plan(
    db: &Mutex<Db>,
    store: &Store,
    dirs: &AppDirs,
    repo: &str,
    app: App,
) -> Result<UnplacePlan> {
    let mut plan = UnplacePlan {
        repo: repo.to_string(),
        app,
        remove: Vec::new(),
        links: Vec::new(),
        keep: Vec::new(),
        bytes: 0,
    };
    let placed: Vec<_> = lock(db)
        .placements()?
        .into_iter()
        .filter(|p| p.repo == repo && p.app == app.as_str())
        .collect();
    // Ollama keeps one file per layer, and models share layers.
    let (manifests, used_elsewhere) = match app {
        App::Ollama => ollama_use(dirs, repo)?,
        _ => Default::default(),
    };
    // Whether the model can leave Ollama whole: every file of it goes, is
    // gone already, or stays only for another model.
    let mut whole = true;
    let attached = store.attached_archives();
    for p in placed {
        let reason = match Fingerprint::of(Path::new(&p.path)) {
            Err(_) => Some(Kept::Gone),
            Ok(now) if now != p.fingerprint() => Some(Kept::Changed),
            // Ollama names a layer by the bytes of the file itself.
            Ok(_) if used_elsewhere.contains(p.derived_sha256.as_ref().unwrap_or(&p.sha256)) => {
                Some(Kept::UsedByAnotherModel)
            }
            Ok(_) if !store.contains(&p.sha256) && !crate::store::in_any(&attached, &p.sha256) => {
                Some(Kept::NoStoredCopy)
            }
            Ok(_) => None,
        };
        whole &= reason.is_none_or(Kept::lets_the_model_go);
        match reason {
            Some(why) => plan.keep.push(KeptFile {
                path: p.path,
                why: why.sentence().to_string(),
            }),
            None => {
                plan.bytes += p.size;
                plan.remove.push(PlannedFile {
                    path: p.path,
                    size: p.size,
                });
            }
        }
    }
    // A manifest names every layer of the model: while one layer has to
    // stay, the model stays in Ollama whole, every layer and the manifest.
    if app == App::Ollama && !whole {
        plan.keep_all(Kept::RestOfModelStays);
        return Ok(plan);
    }
    let going: Vec<String> = plan.remove.iter().map(|f| f.path.clone()).collect();
    plan.links = match app {
        App::Ollama => manifests
            .into_iter()
            .map(|path| Link {
                path,
                goes_with: going.clone(),
            })
            .collect(),
        App::Hf => hf_links(&hf_repo_dir(dirs, repo), &going),
        App::Bionic => Vec::new(),
    };
    Ok(plan)
}

/// Why a file of the model stays in the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kept {
    Gone,
    Changed,
    UsedByAnotherModel,
    NoStoredCopy,
    /// Ollama: another layer of the model has to stay, so all of it does.
    RestOfModelStays,
}

impl Kept {
    fn sentence(self) -> &'static str {
        match self {
            Kept::Gone => "it is no longer there",
            Kept::Changed => "it has changed since dedupd looked at it",
            Kept::UsedByAnotherModel => "another Ollama model uses it",
            Kept::NoStoredCopy => "the store has no copy of it",
            Kept::RestOfModelStays => {
                "another file of the model stays, and Ollama needs all of them"
            }
        }
    }

    /// Whether the model can still leave the app with this file kept.
    fn lets_the_model_go(self) -> bool {
        matches!(self, Kept::Gone | Kept::UsedByAnotherModel)
    }
}

/// The manifest files of `repo` in Ollama's folder, and the layers that
/// other models' manifests name.
fn ollama_use(dirs: &AppDirs, repo: &str) -> Result<(Vec<String>, HashSet<String>)> {
    let mut own = Vec::new();
    let mut elsewhere = HashSet::new();
    // A manifest that cannot be read is an error: its layers would read
    // as used by nothing.
    for found in crate::sources::ollama::manifests_in_use(&dirs.ollama)? {
        if found.repo == repo {
            let rel = match found.provider {
                PROVIDER_HF => Path::new(crate::ollama::HOST).join(repo),
                _ => PathBuf::from(repo),
            }
            .join(&found.tag);
            for tree in ["manifests", "manifests-v2"] {
                let path = dirs.ollama.join(tree).join(&rel);
                if fs::symlink_metadata(&path).is_ok() {
                    own.push(path.to_string_lossy().into_owned());
                }
            }
        } else if let Ok(manifest) = crate::ollama::Manifest::parse(&found.body) {
            elsewhere.extend(
                manifest
                    .all()
                    .filter_map(|l| l.sha256().ok())
                    .map(str::to_string),
            );
        }
    }
    // A model Ollama pulled itself is in both trees, and was found twice.
    own.sort();
    own.dedup();
    Ok((own, elsewhere))
}

/// The snapshot links in a Hugging Face cache folder that point at one of
/// the blobs in `going`, each with the blob it points at.
fn hf_links(repo_dir: &Path, going: &[String]) -> Vec<Link> {
    let blobs = repo_dir.join("blobs");
    walkdir::WalkDir::new(repo_dir.join("snapshots"))
        .sort_by_file_name()
        .into_iter()
        .flatten()
        .filter(|e| e.path_is_symlink())
        .filter_map(|e| {
            let target = fs::read_link(e.path()).ok()?;
            let blob = blobs.join(target.file_name()?);
            let blob = blob.to_string_lossy();
            going.iter().any(|g| *g == blob).then(|| Link {
                path: e.path().to_string_lossy().into_owned(),
                goes_with: vec![blob.into_owned()],
            })
        })
        .collect()
}

/// Take a model out of an app as the owner agreed (`unplace_apply`), and
/// say so in the activity list: deactivated, partly, or why not. Nothing is
/// told when nothing was removed.
pub fn deactivate(
    db: &Mutex<Db>,
    store: &Store,
    dirs: &AppDirs,
    plan: &UnplacePlan,
) -> Result<usize> {
    let app = plan.app;
    let done = unplace_apply(db, dirs, plan);
    // How many files of the model the app still has; `None` when the
    // database cannot say, which is not "none".
    let left = || -> Option<usize> {
        let all = lock(db).placements().ok()?;
        Some(
            all.iter()
                .filter(|p| p.repo == plan.repo && p.app == app.as_str())
                .count(),
        )
    };
    crate::activity::record_result(
        store.home(),
        dedupd_api::activity::ActivityAction::Deactivate,
        &plan.repo,
        Some(app),
        &done,
        |removed| match (*removed, left()) {
            (0, _) => None,
            (_, Some(0)) => Some(crate::activity::deactivated(app)),
            (removed, Some(left)) => Some(crate::activity::partly_deactivated(app, removed, left)),
            (removed, None) => Some(crate::activity::removed_from(app, removed)),
        },
    );
    done
}

/// Carry out a plan made by `unplace_plan`. A file that has changed since
/// the plan was made is left alone, and in Ollama the whole model with it.
/// Returns the files removed.
pub fn unplace_apply(db: &Mutex<Db>, dirs: &AppDirs, plan: &UnplacePlan) -> Result<usize> {
    let app = plan.app;
    let recorded: std::collections::HashMap<String, Fingerprint> = lock(db)
        .placements()?
        .into_iter()
        .filter(|p| p.repo == plan.repo && p.app == app.as_str())
        .map(|p| (p.path.clone(), p.fingerprint()))
        .collect();
    let unchanged = |path: &str| {
        recorded
            .get(path)
            .is_some_and(|fp| Fingerprint::of(Path::new(path)).is_ok_and(|now| now == *fp))
    };
    // An Ollama model leaves whole: one layer that changed after the plan
    // was made keeps all of them.
    if app == App::Ollama && !plan.remove.iter().all(|f| unchanged(&f.path)) {
        return Ok(0);
    }
    // A file that changed after the plan was made stays, and so does every
    // link that names it.
    let going: HashSet<&str> = plan
        .remove
        .iter()
        .map(|f| f.path.as_str())
        .filter(|path| unchanged(path))
        .collect();
    let links: Vec<&str> = plan
        .links
        .iter()
        .filter(|l| l.goes_with.iter().all(|p| going.contains(p.as_str())))
        .map(|l| l.path.as_str())
        .collect();
    // The names go before the files they name: Ollama's manifests, the
    // cache's snapshot links. A stop part of the way then leaves files no
    // name points at, which the next deactivation removes, and never a
    // model that is listed and cannot load. A name that cannot be removed
    // stops the work before any file has gone.
    for link in &links {
        match fs::remove_file(link) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing {link}")),
        }
    }
    let mut removed: HashSet<&str> = HashSet::new();
    for PlannedFile { path, .. } in &plan.remove {
        if !going.contains(path.as_str()) {
            continue;
        }
        fs::remove_file(path).with_context(|| format!("removing {path}"))?;
        // And what a placement that was cut off left beside it.
        crate::clone::clear_leftovers_beside(Path::new(path));
        lock(db).delete_placement(path)?;
        removed.insert(path);
    }
    if app == App::Hf && !removed.is_empty() {
        let repo_dir = hf_repo_dir(dirs, &plan.repo);
        // Only names may be left: which commit a branch was at (`refs`),
        // and which files the provider said do not exist (`.no_exist`).
        // Any file elsewhere in the folder, also one in `snapshots` that
        // is no link of the cache's, keeps the folder.
        let only_names_left = repo_dir.is_dir()
            && is_empty_tree(&repo_dir.join("blobs"))
            && is_empty_tree(&repo_dir.join("snapshots"))
            && fs::read_dir(&repo_dir).is_ok_and(|entries| {
                entries.flatten().all(|e| {
                    let name = e.file_name();
                    ["blobs", "snapshots", "refs", ".no_exist", ".DS_Store"]
                        .iter()
                        .any(|known| name == *known)
                })
            });
        if only_names_left {
            let _ = fs::remove_dir_all(&repo_dir);
        }
    }
    // Leave no empty folders behind. The app's own folder stays, and so do
    // the folders Ollama keeps directly in it.
    let (stop, keep_top) = match app {
        App::Bionic => (dirs.bionic.as_path(), false),
        App::Hf => (dirs.hf_cache.as_path(), false),
        App::Ollama => (dirs.ollama.as_path(), true),
    };
    let mut folders: Vec<PathBuf> = removed
        .iter()
        .chain(&links)
        .filter_map(|p| Path::new(p).parent().map(Path::to_path_buf))
        .collect();
    folders.sort();
    folders.dedup();
    for folder in folders.iter().rev() {
        let mut dir = folder.as_path();
        while dir != stop && dir.starts_with(stop) {
            if keep_top && dir.parent() == Some(stop) {
                break;
            }
            if !remove_if_empty(dir) {
                break;
            }
            match dir.parent() {
                Some(parent) => dir = parent,
                None => break,
            }
        }
    }
    Ok(removed.len())
}

/// Remove `dir` when nothing is in it but what Finder leaves behind.
fn remove_if_empty(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    let names: Vec<_> = entries.flatten().map(|e| e.file_name()).collect();
    if names.iter().any(|n| n != ".DS_Store") {
        return false;
    }
    let _ = fs::remove_file(dir.join(".DS_Store"));
    fs::remove_dir(dir).is_ok()
}

/// No file anywhere under `dir`; also true when `dir` is not there.
fn is_empty_tree(dir: &Path) -> bool {
    !walkdir::WalkDir::new(dir)
        .into_iter()
        .flatten()
        .any(|e| !e.file_type().is_dir())
}

/// Deactivate as the owner asked: the plan for `repo` in `app`, cut to the
/// files the owner agreed to when `agreed` names them, and carried out when
/// `apply`. Without `apply` only the plan is answered.
pub fn unplace(
    db: &Mutex<Db>,
    store: &Store,
    dirs: &AppDirs,
    repo: &str,
    app: App,
    agreed: Option<&[String]>,
    apply: bool,
) -> Result<dedupd_api::moves::UnplacePlan> {
    let mut plan = unplace_plan(db, store, dirs, repo, app)?;
    if let Some(agreed) = agreed {
        plan = plan.only(agreed);
    }
    let removed = match apply {
        true => Some(deactivate(db, store, dirs, &plan)?),
        false => None,
    };
    Ok(plan.answer(removed))
}
