//! What the store holds, model by model, and where each app keeps its
//! files: the read-only view the owner looks at.

use crate::db::{Db, PROVIDER_HF, app_column};
use crate::hub::{TreeEntry, model_meta};
use crate::place::AppDirs;
use crate::profile::{self, Installed};
use crate::refusals::Refusals;
use crate::select;
use crate::store::Store;
use anyhow::{Result, bail};
use dedupd_api::app::App;
use dedupd_api::library::{
    AppPresence, LibraryModel, Location, LocationKind, Locations, ModelDetail, Origin, Placement,
    Revision, RevisionFile, UpstreamState, Variant, VariantKind, VerifyState,
};
use rusqlite::{OptionalExtension, params};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The most of a README that is handed out.
const README_LIMIT: u64 = 512 * 1024;

struct RepoRow {
    id: i64,
    provider: String,
    name: String,
    upstream: Option<String>,
    checked_at: Option<i64>,
    archived_at: Option<i64>,
}

fn repo_rows(db: &Db, only: Option<&str>) -> Result<Vec<RepoRow>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, provider, name, upstream, checked_at, archived_at FROM repos
         WHERE ?1 IS NULL OR name = ?1 ORDER BY name COLLATE NOCASE, provider",
    )?;
    Ok(stmt
        .query_map([only], |r| {
            Ok(RepoRow {
                id: r.get(0)?,
                provider: r.get(1)?,
                name: r.get(2)?,
                upstream: r.get(3)?,
                checked_at: r.get(4)?,
                archived_at: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn model_of(db: &Db, row: RepoRow) -> Result<LibraryModel> {
    let conn = db.conn();
    let commit: Option<String> = conn
        .query_row(
            "SELECT commit_sha FROM refs WHERE repo_id = ?1 AND name = 'main'",
            [row.id],
            |r| r.get(0),
        )
        .optional()?;
    let revisions: i64 = conn.query_row(
        "SELECT count(*) FROM revisions WHERE repo_id = ?1",
        [row.id],
        |r| r.get(0),
    )?;
    let newest: Option<(String, i64)> = conn
        .query_row(
            "SELECT info_json, fetched_at FROM revisions WHERE repo_id = ?1
             ORDER BY (commit_sha = ?2) DESC, fetched_at DESC, id DESC LIMIT 1",
            params![row.id, commit],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    // Small files are known to the Hub by their git blob id, weights by sha256.
    let (stored_files, stored_bytes): (i64, i64) = conn.query_row(
        "SELECT count(*), COALESCE(sum(size), 0) FROM blobs WHERE sha256 IN (
             SELECT COALESCE(f.lfs_sha256,
                             (SELECT sha256 FROM blobs WHERE git_oid = f.git_oid LIMIT 1))
             FROM revision_files f JOIN revisions v ON v.id = f.revision_id
             WHERE v.repo_id = ?1)",
        [row.id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let mut stmt = conn.prepare(
        "SELECT app, count(*), COALESCE(sum(size), 0),
                COALESCE(sum(CASE WHEN shared THEN size ELSE 0 END), 0)
         FROM placements WHERE repo_id = ?1 GROUP BY app ORDER BY app",
    )?;
    let apps = stmt
        .query_map([row.id], |r| {
            Ok(AppPresence {
                app: app_column(r, 0)?,
                files: r.get::<_, i64>(1)? as u64,
                bytes: r.get::<_, i64>(2)? as u64,
                shared_bytes: r.get::<_, i64>(3)? as u64,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let counts: (i64, i64, i64, i64) = conn.query_row(
        "SELECT COALESCE(sum(verify_state = 'verified'), 0),
                COALESCE(sum(verify_state = 'mismatch'), 0),
                COALESCE(sum(verify_state = 'not_upstream'), 0),
                COALESCE(sum(verify_state = 'unchecked'), 0)
         FROM placements WHERE repo_id = ?1",
        [row.id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let mut stmt =
        conn.prepare("SELECT DISTINCT tag FROM ollama_manifests WHERE repo_id = ?1 ORDER BY tag")?;
    let ollama_tags = stmt
        .query_map([row.id], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;

    let mut meta = newest
        .as_ref()
        .and_then(|(info, _)| serde_json::from_str::<serde_json::Value>(info).ok())
        .map(|info| model_meta(&info))
        .unwrap_or_default();
    if meta.formats.is_empty() {
        // Nothing from the provider says; the files do.
        let mut stmt =
            conn.prepare("SELECT DISTINCT app, rel_path FROM placements WHERE repo_id = ?1")?;
        let files = stmt
            .query_map([row.id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let any = |test: &dyn Fn(&str, &str) -> bool| files.iter().any(|(a, p)| test(a, p));
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
        commit,
        meta,
        revisions: revisions as u64,
        fetched_at: newest.map(|(_, at)| at),
        stored_files: stored_files as u64,
        stored_bytes: stored_bytes as u64,
        apps,
        verified: counts.0 as u64,
        mismatch: counts.1 as u64,
        not_upstream: counts.2 as u64,
        unchecked: counts.3 as u64,
        upstream: match row.upstream.as_deref() {
            Some("ok") => UpstreamState::Ok,
            Some("gone") => UpstreamState::Gone,
            _ => UpstreamState::Unknown,
        },
        checked_at: row.checked_at,
        ollama_tags,
        archived_at: row.archived_at,
    })
}

/// Every repository the database knows, by name.
pub fn models(db: &Db) -> Result<Vec<LibraryModel>> {
    repo_rows(db, None)?
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
    let mut stmt = db.conn().prepare(
        "SELECT DISTINCT p.sha256, p.app FROM placements p JOIN repos r ON r.id = p.repo_id
         WHERE r.name = ?1",
    )?;
    let mut holders: BTreeMap<String, BTreeSet<App>> = BTreeMap::new();
    for row in stmt.query_map([repo], |r| Ok((r.get::<_, String>(0)?, app_column(r, 1)?)))? {
        let (sha, app) = row?;
        holders.entry(sha).or_default().insert(app);
    }
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
    let mut stmt = db.conn().prepare(
        "SELECT r.name, p.app FROM repos r LEFT JOIN placements p ON p.repo_id = r.id
         WHERE r.provider = ?1 GROUP BY r.id, p.app ORDER BY r.name, p.app",
    )?;
    let mut out: BTreeMap<String, Vec<App>> = BTreeMap::new();
    for row in stmt.query_map([PROVIDER_HF], |r| {
        let app = match r.get::<_, Option<String>>(1)? {
            Some(_) => Some(app_column(r, 1)?),
            None => None,
        };
        Ok((r.get::<_, String>(0)?, app))
    })? {
        let (repo, app) = row?;
        out.entry(repo).or_default().extend(app);
    }
    Ok(out)
}

/// Everything about one repository. `None` when the database does not
/// know it.
pub fn model(
    db: &Db,
    store: &Store,
    repo: &str,
    installed: &Installed,
) -> Result<Option<ModelDetail>> {
    let Some(row) = repo_rows(db, Some(repo))?.into_iter().next() else {
        return Ok(None);
    };
    let (repo_id, name) = (row.id, row.name.clone());
    let model = model_of(db, row)?;
    let conn = db.conn();

    let mut stmt = conn.prepare(
        "SELECT app, path, rel_path, size, sha256, shared, verify_state, origin
         FROM placements WHERE repo_id = ?1 ORDER BY app, rel_path, path",
    )?;
    let placements = stmt
        .query_map([repo_id], |r| {
            let origin: String = r.get(7)?;
            Ok(Placement {
                app: app_column(r, 0)?,
                path: r.get(1)?,
                rel_path: r.get(2)?,
                size: r.get::<_, i64>(3)? as u64,
                sha256: r.get(4)?,
                shared: r.get(5)?,
                verify_state: VerifyState::parse(&r.get::<_, String>(6)?),
                // Anything else was written by dedupd itself.
                origin: Origin::parse(&origin).unwrap_or(Origin::Placed),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut holders: BTreeMap<&str, BTreeSet<App>> = BTreeMap::new();
    for p in &placements {
        holders.entry(&p.sha256).or_default().insert(p.app);
    }

    let archive = store.archive().filter(|a| crate::archive::attached(a));
    let refs = db.refs(repo_id)?;
    let mut stmt = conn.prepare(
        "SELECT id, commit_sha, fetched_at, info_json FROM revisions WHERE repo_id = ?1
         ORDER BY (commit_sha = ?2) DESC, fetched_at DESC, id DESC",
    )?;
    let rows = stmt
        .query_map(params![repo_id, model.commit], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut files_of = conn.prepare(
        "SELECT path, size, git_oid, lfs_sha256 FROM revision_files
         WHERE revision_id = ?1 ORDER BY path",
    )?;
    let mut revisions = Vec::new();
    let mut newest: Option<Vec<TreeEntry>> = None;
    let mut readme = None;
    for (rev_id, commit, fetched_at, _) in &rows {
        let entries = files_of
            .query_map([rev_id], |r| {
                Ok(TreeEntry {
                    path: r.get(0)?,
                    size: r.get::<_, i64>(1)? as u64,
                    git_oid: r.get(2)?,
                    lfs_sha256: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
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
            commit: commit.clone(),
            fetched_at: *fetched_at,
            refs: refs
                .iter()
                .filter(|(_, c)| c == commit)
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
        info_json: rows.into_iter().next().map(|r| r.3),
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
    let conn = db.conn();
    let (files, bytes): (i64, i64) = conn.query_row(
        "SELECT count(*), COALESCE(sum(size), 0) FROM blobs",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let repos: i64 = conn.query_row("SELECT count(*) FROM repos", [], |r| r.get(0))?;
    let (volume_total, volume_free) = store.volume()?;
    let mut apps = Vec::new();
    for app in App::ALL {
        let path = match app {
            App::Bionic => &dirs.bionic,
            App::Ollama => &dirs.ollama,
            App::Hf => &dirs.hf_cache,
        };
        let row: (i64, i64, i64, i64) = conn.query_row(
            "SELECT count(*), COALESCE(sum(size), 0),
                    COALESCE(sum(CASE WHEN shared THEN size ELSE 0 END), 0),
                    count(DISTINCT repo_id)
             FROM placements WHERE app = ?1",
            [app.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        apps.push(Location {
            kind: LocationKind::App,
            app: Some(app),
            path: path.to_string_lossy().into_owned(),
            present: path.is_dir(),
            files: row.0 as u64,
            bytes: row.1 as u64,
            shared_bytes: row.2 as u64,
            models: row.3 as u64,
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
            files: files as u64,
            bytes: bytes as u64,
            shared_bytes: 0,
            models: repos as u64,
        },
        archive: store.archive().map(|a| Location {
            kind: LocationKind::Archive,
            app: None,
            path: a.root().to_string_lossy().into_owned(),
            present: crate::archive::attached(a),
            files: 0,
            bytes: 0,
            shared_bytes: 0,
            models: 0,
        }),
        apps,
    })
}

/// The repository's row id, or an error that names it.
pub fn repo_id(db: &Db, repo: &str) -> Result<(i64, String)> {
    match repo_rows(db, Some(repo))?.into_iter().next() {
        Some(row) => Ok((row.id, row.provider)),
        None => bail!("{repo} is not in the library"),
    }
}
