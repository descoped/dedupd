//! What the store holds, model by model, and where each app keeps its
//! files: the read-only view the owner looks at.

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
        if any(&|app, path| select::is_gguf(path) || app == App::Ollama.as_str()) {
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
            Some("ok") => UpstreamState::Ok,
            Some("gone") => UpstreamState::Gone,
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
pub fn spec_of(repo: &str, variant: &select::Variant) -> String {
    match &variant.quant {
        Some(q) => format!("{repo}:{q}"),
        None => repo.to_string(),
    }
}

/// The ways a revision with these files can be used, and how much of each
/// is in the store and in the apps. The repository need not be archived.
pub fn local_variants(
    db: &Db,
    store: &Store,
    repo: &str,
    entries: &[TreeEntry],
    formats: &[String],
    installed: &Installed,
) -> Result<Vec<Variant>> {
    let holders = db.holders(repo)?;
    // Each file's content, when the store knows it.
    let mut content: BTreeMap<&str, Option<String>> = BTreeMap::new();
    for e in entries {
        content.insert(&e.path, db.blob_for_entry(e)?.map(|b| b.sha256));
    }
    let sha_of = |path: &str| content.get(path).and_then(|s| s.as_deref());
    // What an app said when it was asked about this model before.
    let refusals = Refusals::load(store.home());
    Ok(select::variants(entries)
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
                kind: v.kind,
                name: v.name,
            }
        })
        .collect())
}

/// The Hugging Face repositories the database knows, each with the apps
/// that have files of it.
pub fn presence(db: &Db) -> Result<BTreeMap<String, Vec<App>>> {
    db.presence()
}

/// Everything about one repository. `None` when the database does not
/// know it.
pub fn model(
    db: &Db,
    store: &Store,
    repo: &str,
    installed: &Installed,
) -> Result<Option<ModelDetail>> {
    let Some(row) = db.repo_rows(Some(repo))?.into_iter().next() else {
        return Ok(None);
    };
    let (repo_id, name) = (row.id, row.name.clone());
    let model = model_of(db, row)?;
    let placements = db.placements_of(repo_id)?;
    let mut holders: BTreeMap<&str, BTreeSet<App>> = BTreeMap::new();
    for p in &placements {
        holders.entry(&p.sha256).or_default().insert(p.app);
    }

    let archive = store.archive().filter(|a| a.attached());
    let refs = db.refs(repo_id)?;
    let rows = db.revisions_of(repo_id, model.commit.as_deref())?;
    let mut revisions = Vec::new();
    let mut newest: Option<Vec<TreeEntry>> = None;
    let mut readme = None;
    for row in &rows {
        let entries = db.revision_entries(row.id)?;
        let mut files = Vec::with_capacity(entries.len());
        for e in &entries {
            let sha = db.blob_for_entry(e)?.map(|b| b.sha256);
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
                archived: match (&sha, archive) {
                    (Some(s), Some(a)) => a.blob_path(s).is_file(),
                    _ => false,
                },
                apps: sha
                    .as_deref()
                    .and_then(|s| holders.get(s))
                    .map(|apps| apps.iter().copied().collect())
                    .unwrap_or_default(),
                sha256: sha.or_else(|| e.lfs_sha256.clone()),
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
            bytes: entries.iter().map(|e| e.size).sum(),
            files,
        });
        newest.get_or_insert(entries);
    }

    let variants = local_variants(
        db,
        store,
        &name,
        &newest.unwrap_or_default(),
        &model.meta.formats,
        installed,
    )?;

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
    let (files, bytes) = db.store_totals()?;
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

/// Ask the provider whether it still has the repository and where `main`
/// is now, and note the answer. Nothing is downloaded. Fails with
/// [`crate::db::NotInLibrary`] for a model the database does not know, and
/// with the provider's [`HubError`] when it could not be asked.
pub async fn check_upstream(engine: &Engine, repo: &str) -> Result<UpstreamCheck> {
    let known = {
        let db = engine.db();
        let id = db.known_repo(repo)?;
        db.ref_commit(id, "main")?
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
            engine.db().set_upstream(PROVIDER_HF, repo, false)?;
            Ok(UpstreamCheck {
                upstream: UpstreamState::Ok,
                changed: latest.is_some() && latest != known,
                commit: known,
                latest,
            })
        }
        Err(HubError::NotAvailable { .. }) => {
            engine.db().set_upstream(PROVIDER_HF, repo, true)?;
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
