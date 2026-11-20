//! What the store holds, model by model, and where each app keeps its
//! files: the view the owner looks at. It only reads, except
//! `check_upstream`, which notes what the provider answered.

use crate::db::library::RepoRow;
use crate::db::{Db, PROVIDER_HF};
use crate::engine::Engine;
use crate::hub::{HubError, TreeEntry, model_meta};
use crate::place::AppDirs;
use crate::profile::{self, Installed};
use crate::refusals::Refusals;
use crate::select;
use crate::store::Store;
use anyhow::Result;
use dedupd_api::app::App;
use dedupd_api::library::{
    LibraryModel, Location, LocationKind, Locations, ModelDetail, Revision, RevisionFile,
    UpstreamCheck, UpstreamState, Variant, VariantKind,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// The most of a README that is handed out.
const README_LIMIT: u64 = 512 * 1024;

fn model_of(db: &Db, row: RepoRow) -> Result<LibraryModel> {
    let facts = db.repo_facts(row.id)?;
    let mut meta = facts
        .newest
        .as_ref()
        .and_then(|(info, _)| serde_json::from_str::<serde_json::Value>(info).ok())
        .map(|info| model_meta(&info))
        .unwrap_or_default();
    if meta.formats.is_empty() {
        // Nothing from the provider says; the files do.
        let any = |test: &dyn Fn(&str, &str) -> bool| facts.placed.iter().any(|(a, p)| test(a, p));
        // By the files' names, not by the app: Ollama also holds MLX models.
        if any(&|_, path| select::is_gguf(path)) {
            meta.formats.push("gguf".into());
        }
        if any(&|_, path| path.ends_with(".safetensors")) {
            meta.formats.push("safetensors".into());
        }
    }
    if meta.author.is_none() && row.provider == PROVIDER_HF {
        meta.author = row.name.split_once('/').map(|(org, _)| org.to_string());
    }
    Ok(LibraryModel {
        provider: row.provider,
        repo: row.name,
        commit: facts.commit,
        meta,
        revisions: facts.revisions,
        fetched_at: facts.newest.map(|(_, at)| at),
        stored_files: facts.stored_files,
        stored_bytes: facts.stored_bytes,
        apps: facts.apps,
        verified: facts.verified,
        mismatch: facts.mismatch,
        not_upstream: facts.not_upstream,
        unchecked: facts.unchecked,
        upstream: match row.upstream.as_deref() {
            Some(crate::db::UPSTREAM_OK) => UpstreamState::Ok,
            Some(crate::db::UPSTREAM_GONE) => UpstreamState::Gone,
            _ => UpstreamState::Unknown,
        },
        checked_at: row.checked_at,
        ollama_tags: facts.ollama_tags,
        archived_at: row.archived_at,
    })
}

/// Every repository the database knows, by name.
pub fn models(db: &Db) -> Result<Vec<LibraryModel>> {
    db.repo_rows(None)?
        .into_iter()
        .map(|row| model_of(db, row))
        .collect()
}

/// `org/repo:quant` for a variant of a GGUF repository, else `org/repo`.
fn spec_of(repo: &str, variant: &select::Variant) -> String {
    match &variant.quant {
        Some(q) => format!("{repo}:{q}"),
        None => repo.to_string(),
    }
}

/// What the database says about the files of one revision: which stored
/// file each one is, and which apps hold which stored file. Read under the
/// database lock; the disks are looked at afterwards, without it.
struct VariantRows {
    holders: BTreeMap<String, BTreeSet<App>>,
    /// Each file's content, when the store knows it.
    content: BTreeMap<String, Option<String>>,
}

fn variant_rows(db: &Db, repo: &str, entries: &[TreeEntry]) -> Result<VariantRows> {
    let mut content = BTreeMap::new();
    for e in entries {
        content.insert(e.path.clone(), db.blob_for_entry(e)?.map(|b| b.sha256));
    }
    Ok(VariantRows {
        holders: db.holders(repo)?,
        content,
    })
}

/// The ways a revision with these files can be used, and how much of each
/// is in the store and in the apps. The repository need not be archived.
/// The database is locked only while its rows are read: the refusals file,
/// the stored `config.json` and the store's folder are read after that.
pub fn local_variants(
    db: &Mutex<Db>,
    store: &Store,
    repo: &str,
    entries: &[TreeEntry],
    formats: &[String],
    installed: &Installed,
) -> Result<Vec<Variant>> {
    let rows = variant_rows(&crate::db::lock(db), repo, entries)?;
    Ok(variants_of(&rows, store, repo, entries, formats, installed))
}

fn variants_of(
    rows: &VariantRows,
    store: &Store,
    repo: &str,
    entries: &[TreeEntry],
    formats: &[String],
    installed: &Installed,
) -> Vec<Variant> {
    let holders = &rows.holders;
    let sha_of = |path: &str| rows.content.get(path).and_then(|s| s.as_deref());
    // What an app said when it was asked about this model before.
    let refusals = Refusals::load(store.home());
    select::variants(entries)
        .into_iter()
        .filter(|v| !v.files.is_empty())
        .map(|v| {
            let placed_in = App::ALL
                .into_iter()
                .filter(|app| {
                    !v.weights.is_empty()
                        && v.weights.iter().all(|w| {
                            sha_of(w)
                                .and_then(|sha| holders.get(sha))
                                .is_some_and(|apps| apps.contains(app))
                        })
                })
                .collect();
            let ggufs = match v.kind {
                VariantKind::Gguf => v.weights.len(),
                VariantKind::Files => 0,
            };
            let config = v
                .files
                .iter()
                .find(|e| e.path == "config.json")
                .and_then(|e| sha_of(&e.path));
            let judged = profile::decide_for(store, installed, &refusals, ggufs, formats, config);
            Variant {
                spec: spec_of(repo, &v),
                usable_by: judged.ready(),
                format: judged.format,
                placed_in,
                decisions: judged.decisions,
                files: v.files.len() as u64,
                bytes: v.bytes,
                stored_bytes: v
                    .files
                    .iter()
                    .filter(|e| sha_of(&e.path).is_some_and(|sha| store.contains(sha)))
                    .map(|e| e.size)
                    .sum(),
                weights_stored: !v.weights.is_empty()
                    && v.weights
                        .iter()
                        .all(|w| sha_of(w).is_some_and(|sha| store.contains(sha))),
                kind: v.kind,
                name: v.name,
            }
        })
        .collect()
}

/// The Hugging Face repositories the database knows, each with the apps
/// that have files of it.
pub fn presence(db: &Db) -> Result<BTreeMap<String, Vec<App>>> {
    db.presence()
}

/// Everything about one repository. `None` when the database does not
/// know it. The database is locked only while its rows are read; the
/// store, the README and the archive disk (which may be asleep and answer
/// after seconds) are looked at after the lock is given back.
pub fn model(
    db: &Mutex<Db>,
    store: &Store,
    repo: &str,
    installed: &Installed,
) -> Result<Option<ModelDetail>> {
    /// One file of a revision with the stored file it is, when known.
    type Listed = (TreeEntry, Option<String>);
    let (name, model, placements, refs, rows, listed, variant_rows) = {
        let db = crate::db::lock(db);
        let Some(row) = db.repo_rows(Some(repo))?.into_iter().next() else {
            return Ok(None);
        };
        let (repo_id, name) = (row.id, row.name.clone());
        let model = model_of(&db, row)?;
        let placements = db.placements_of(repo_id)?;
        let refs = db.refs(repo_id)?;
        let rows = db.revisions_of(repo_id, model.commit.as_deref())?;
        let mut listed: Vec<Vec<Listed>> = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut files = Vec::new();
            for e in db.revision_entries(row.id)? {
                let sha = db.blob_for_entry(&e)?.map(|b| b.sha256);
                files.push((e, sha));
            }
            listed.push(files);
        }
        let newest: Vec<TreeEntry> = listed
            .first()
            .map(|files| files.iter().map(|(e, _)| e.clone()).collect())
            .unwrap_or_default();
        let variant_rows = variant_rows(&db, &name, &newest)?;
        (name, model, placements, refs, rows, listed, variant_rows)
    };

    let mut holders: BTreeMap<&str, BTreeSet<App>> = BTreeMap::new();
    for p in &placements {
        holders.entry(&p.sha256).or_default().insert(p.app);
    }
    let archive = store.archive().filter(|a| a.attached());
    let mut revisions = Vec::new();
    let mut readme = None;
    for (row, entries) in rows.iter().zip(&listed) {
        let mut files = Vec::with_capacity(entries.len());
        for (e, sha) in entries {
            let stored = sha.as_deref().is_some_and(|s| store.contains(s));
            if readme.is_none() && stored && e.path.eq_ignore_ascii_case("README.md") {
                readme = sha.as_deref().and_then(|s| read_text(&store.blob_path(s)));
            }
            files.push(RevisionFile {
                path: e.path.clone(),
                size: e.size,
                git_oid: e.git_oid.clone(),
                lfs: e.lfs_sha256.is_some(),
                stored,
                archived: match (sha, archive) {
                    (Some(s), Some(a)) => a.blob_path(s).is_file(),
                    _ => false,
                },
                apps: sha
                    .as_deref()
                    .and_then(|s| holders.get(s))
                    .map(|apps| apps.iter().copied().collect())
                    .unwrap_or_default(),
                sha256: sha.clone().or_else(|| e.lfs_sha256.clone()),
            });
        }
        revisions.push(Revision {
            commit: row.commit.clone(),
            fetched_at: row.fetched_at,
            refs: refs
                .iter()
                .filter(|(_, c)| *c == row.commit)
                .map(|(name, _)| name.clone())
                .collect(),
            bytes: entries.iter().map(|(e, _)| e.size).sum(),
            files,
        });
    }

    let newest: Vec<TreeEntry> = listed
        .into_iter()
        .next()
        .map(|files| files.into_iter().map(|(e, _)| e).collect())
        .unwrap_or_default();
    let variants = variants_of(
        &variant_rows,
        store,
        &name,
        &newest,
        &model.meta.formats,
        installed,
    );

    Ok(Some(ModelDetail {
        model,
        revisions,
        placements,
        variants,
        info_json: rows.into_iter().next().map(|r| r.info_json),
        readme,
    }))
}

/// The start of a text file; `None` when it cannot be read as text.
fn read_text(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut text = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(README_LIMIT)
        .read_to_end(&mut text)
        .ok()?;
    Some(String::from_utf8_lossy(&text).into_owned())
}

/// Where everything is: the store, the archive and each app's folder.
pub fn locations(db: &Db, store: &Store, dirs: &AppDirs) -> Result<Locations> {
    let (files, bytes) = crate::status::store_totals(db, store)?;
    let repos = db.repo_count()?;
    let (volume_total, volume_free) = store.volume()?;
    let mut apps = Vec::new();
    for app in App::ALL {
        let path = match app {
            App::Bionic => &dirs.bionic,
            App::Ollama => &dirs.ollama,
            App::Hf => &dirs.hf_cache,
        };
        let totals = db.app_totals(app)?;
        apps.push(Location {
            kind: LocationKind::App,
            app: Some(app),
            path: path.to_string_lossy().into_owned(),
            present: path.is_dir(),
            files: totals.files,
            bytes: totals.bytes,
            shared_bytes: totals.shared_bytes,
            models: totals.models,
        });
    }
    Ok(Locations {
        volume_total,
        volume_free,
        store: Location {
            kind: LocationKind::Store,
            app: None,
            path: store.home().store_dir().to_string_lossy().into_owned(),
            present: true,
            files,
            bytes,
            shared_bytes: 0,
            models: repos,
        },
        archive: store.archive().map(|a| Location {
            kind: LocationKind::Archive,
            app: None,
            path: a.root().to_string_lossy().into_owned(),
            present: a.attached(),
            files: 0,
            bytes: 0,
            shared_bytes: 0,
            models: 0,
        }),
        apps,
    })
}

/// Whether the provider's refusal says the model is gone, and not only
/// that this caller may not see it. 404 and 410 say gone. 401 is what the
/// provider answers for a missing model when no token is sent, so it says
/// gone only then: with a token, 401 is the token's own trouble (expired,
/// taken back). 403 is a model whose terms were not accepted.
fn says_gone(status: u16, has_token: bool) -> bool {
    matches!(status, 404 | 410) || (status == 401 && !has_token)
}

/// Ask the provider whether it still has the repository and where `main`
/// is now, and note the answer. Nothing is downloaded. Fails with
/// [`crate::db::NotInLibrary`] for a model the database does not know, with
/// the provider's [`HubError`] when it could not be asked or would not
/// show the model to this caller (what is known about the model then
/// stays as it was), and when the answer cannot be noted, so a check that
/// says it is done was kept.
pub async fn check_upstream(engine: &Arc<Engine>, repo: &str) -> Result<UpstreamCheck> {
    // The database is asked off the runtime: its lock may be held by a
    // caller that is reading a disk.
    let known = {
        let repo = repo.to_string();
        crate::engine::blocking(engine, move |engine| {
            let db = engine.db();
            let id = db.known_repo(&repo)?;
            db.ref_commit(id, "main")
        })
        .await?
    };
    let note = |gone: bool| {
        let repo = repo.to_string();
        crate::engine::blocking(engine, move |engine| {
            engine.db().set_upstream(PROVIDER_HF, &repo, gone)
        })
    };
    let path = format!(
        "/api/models/{}/revision/main",
        dedupd_api::url::encode_path(repo)
    );
    let answer = match engine.hub.api(&path, repo).await {
        Ok(resp) => resp
            .json::<serde_json::Value>()
            .await
            .map_err(HubError::from),
        Err(e) => Err(e),
    };
    match answer {
        Ok(info) => {
            let latest = info.get("sha").and_then(|v| v.as_str()).map(str::to_string);
            note(false).await?;
            Ok(UpstreamCheck {
                upstream: UpstreamState::Ok,
                changed: latest.is_some() && latest != known,
                commit: known,
                latest,
            })
        }
        Err(HubError::NotAvailable { status, .. }) if says_gone(status, engine.hub.has_token()) => {
            note(true).await?;
            Ok(UpstreamCheck {
                upstream: UpstreamState::Gone,
                commit: known,
                latest: None,
                changed: false,
            })
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_is_not_a_model_that_is_gone() {
        // Not there, with or without a token.
        assert!(says_gone(404, true) && says_gone(404, false) && says_gone(410, true));
        // What the provider answers for a missing model when no token is sent.
        assert!(says_gone(401, false));
        // A token that expired, and terms that were not accepted.
        assert!(!says_gone(401, true));
        assert!(!says_gone(403, true) && !says_gone(403, false));
    }
}
