//! What the store holds, model by model, and where each app keeps its
//! files: the read-only view the owner looks at.

use crate::db::{Db, PROVIDER_HF};
use crate::hub::{ModelMeta, TreeEntry};
use crate::place::{App, AppDirs};
use crate::profile::{self, Decision, Installed, Traits};
use crate::select::{self, Variant};
use crate::store::Store;
use anyhow::{Result, bail};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The most of a README that is handed out.
const README_LIMIT: u64 = 512 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct AppPresence {
    pub app: String,
    pub files: u64,
    pub bytes: u64,
    /// Bytes that share their disk blocks with the store.
    pub shared_bytes: u64,
}

/// One repository in the library.
#[derive(Debug, Clone, Serialize)]
pub struct Model {
    pub provider: String,
    pub repo: String,
    /// The commit `main` pointed at when the provider was last asked.
    pub commit: Option<String>,
    pub meta: ModelMeta,
    /// Archived revisions.
    pub revisions: u64,
    /// When the newest revision was archived, in seconds since the epoch.
    pub fetched_at: Option<i64>,
    /// Files of the archived revisions that are in the store.
    pub stored_files: u64,
    pub stored_bytes: u64,
    pub apps: Vec<AppPresence>,
    pub verified: u64,
    pub mismatch: u64,
    pub not_upstream: u64,
    pub unchecked: u64,
    /// `ok`, `gone`, or `unknown` when the provider has not been asked.
    pub upstream: String,
    pub checked_at: Option<i64>,
    /// The Ollama tags whose manifests are archived.
    pub ollama_tags: Vec<String>,
}

struct RepoRow {
    id: i64,
    provider: String,
    name: String,
    upstream: Option<String>,
    checked_at: Option<i64>,
}

fn repo_rows(db: &Db, only: Option<&str>) -> Result<Vec<RepoRow>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, provider, name, upstream, checked_at FROM repos
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
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn model_of(db: &Db, row: RepoRow) -> Result<Model> {
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
                app: r.get(0)?,
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
        .map(|info| ModelMeta::from_info(&info))
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
    Ok(Model {
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
        upstream: row.upstream.unwrap_or_else(|| "unknown".into()),
        checked_at: row.checked_at,
        ollama_tags,
    })
}

/// Every repository the database knows, by name.
pub fn models(db: &Db) -> Result<Vec<Model>> {
    repo_rows(db, None)?
        .into_iter()
        .map(|row| model_of(db, row))
        .collect()
}

/// One file of an archived revision.
#[derive(Debug, Clone, Serialize)]
pub struct RevisionFile {
    pub path: String,
    pub size: u64,
    /// sha256 of the content, when it is known.
    pub sha256: Option<String>,
    pub git_oid: String,
    /// Kept in LFS by the provider: a weight file rather than a small one.
    pub lfs: bool,
    /// On the internal disk.
    pub stored: bool,
    /// On the archive disk. `false` also when the disk is not attached.
    pub archived: bool,
    /// The apps that have this file.
    pub apps: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Revision {
    pub commit: String,
    pub fetched_at: i64,
    /// Branch and tag names that point here.
    pub refs: Vec<String>,
    pub bytes: u64,
    pub files: Vec<RevisionFile>,
}

/// A file in an app's folder.
#[derive(Debug, Clone, Serialize)]
pub struct Placement {
    pub app: String,
    pub path: String,
    pub rel_path: String,
    pub size: u64,
    pub sha256: String,
    pub shared: bool,
    pub verify_state: String,
    /// `adopted` when the app downloaded it, `placed` when dedupd put it there.
    pub origin: String,
}

/// A way the repository can be used, and how much of it is here.
#[derive(Debug, Clone, Serialize)]
pub struct LocalVariant {
    pub kind: String,
    pub name: String,
    /// What to pull to get it: `org/repo:Q4_K_M`, or `org/repo`.
    pub spec: String,
    pub files: u64,
    pub bytes: u64,
    pub stored_bytes: u64,
    /// The apps dedupd can put it into: those whose decision is `ready`.
    pub usable_by: Vec<String>,
    /// The apps that have all its weight files.
    pub placed_in: Vec<String>,
    /// For every app: whether it can take this variant, by which method,
    /// at what cost in disk space, and why.
    pub decisions: Vec<Decision>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelDetail {
    pub model: Model,
    pub revisions: Vec<Revision>,
    pub placements: Vec<Placement>,
    pub variants: Vec<LocalVariant>,
    /// The provider's own description of the newest revision, as it sent it.
    pub info_json: Option<String>,
    pub readme: Option<String>,
}

/// The quantization widths of a model that is not GGUF, from its
/// `config.json` when the store has it. Empty when it is not known.
fn widths(
    store: &Store,
    entries: &[TreeEntry],
    sha_of: impl Fn(&str) -> Option<String>,
) -> Vec<u32> {
    entries
        .iter()
        .find(|e| e.path == "config.json")
        .and_then(|e| sha_of(&e.path))
        .filter(|sha| store.contains(sha))
        .and_then(|sha| std::fs::read(store.blob_path(&sha)).ok())
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .map(|config| profile::quantization_bits(&config))
        .unwrap_or_default()
}

/// `org/repo:quant` for a variant of a GGUF repository, else `org/repo`.
pub fn spec_of(repo: &str, variant: &Variant) -> String {
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
) -> Result<Vec<LocalVariant>> {
    let mut stmt = db.conn().prepare(
        "SELECT DISTINCT p.sha256, p.app FROM placements p JOIN repos r ON r.id = p.repo_id
         WHERE r.name = ?1",
    )?;
    let mut holders: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for row in stmt.query_map([repo], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })? {
        let (sha, app) = row?;
        holders.entry(sha).or_default().insert(app);
    }
    // Each file's content, when the store knows it.
    let mut content: BTreeMap<&str, Option<String>> = BTreeMap::new();
    for e in entries {
        content.insert(&e.path, db.blob_for_entry(e)?.map(|b| b.sha256));
    }
    let sha_of = |path: &str| content.get(path).and_then(|s| s.as_deref());
    Ok(select::variants(entries)
        .into_iter()
        .filter(|v| !v.files.is_empty())
        .map(|v| {
            let placed_in = App::ALL
                .iter()
                .filter(|app| {
                    !v.weights.is_empty()
                        && v.weights.iter().all(|w| {
                            sha_of(w)
                                .and_then(|sha| holders.get(sha))
                                .is_some_and(|apps| apps.contains(app.as_str()))
                        })
                })
                .map(|a| a.as_str().to_string())
                .collect();
            let mut traits = Traits::of(&v, formats);
            if v.kind != "gguf" {
                traits =
                    traits.with_bits(widths(store, &v.files, |p| sha_of(p).map(str::to_string)));
            }
            let decisions = profile::decisions(&traits, installed);
            LocalVariant {
                spec: spec_of(repo, &v),
                usable_by: decisions
                    .iter()
                    .filter(|d| d.state == "ready")
                    .map(|d| d.app.clone())
                    .collect(),
                placed_in,
                decisions,
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
pub fn presence(db: &Db) -> Result<BTreeMap<String, Vec<String>>> {
    let mut stmt = db.conn().prepare(
        "SELECT r.name, p.app FROM repos r LEFT JOIN placements p ON p.repo_id = r.id
         WHERE r.provider = ?1 GROUP BY r.id, p.app ORDER BY r.name, p.app",
    )?;
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in stmt.query_map([PROVIDER_HF], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
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
            Ok(Placement {
                app: r.get(0)?,
                path: r.get(1)?,
                rel_path: r.get(2)?,
                size: r.get::<_, i64>(3)? as u64,
                sha256: r.get(4)?,
                shared: r.get(5)?,
                verify_state: r.get(6)?,
                origin: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut holders: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for p in &placements {
        holders.entry(&p.sha256).or_default().insert(&p.app);
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
                    .map(|apps| apps.iter().map(|a| a.to_string()).collect())
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

/// A folder on the disk and what dedupd knows is in it.
#[derive(Debug, Clone, Serialize)]
pub struct Location {
    /// `store`, `archive`, or an app's short name.
    pub id: String,
    pub path: String,
    /// The folder is there. For the archive: the disk is attached.
    pub present: bool,
    pub files: u64,
    pub bytes: u64,
    /// Bytes that share their disk blocks with the store.
    pub shared_bytes: u64,
    pub models: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Locations {
    /// The volume the store and the app folders are on.
    pub volume_total: u64,
    pub volume_free: u64,
    pub store: Location,
    pub archive: Option<Location>,
    pub apps: Vec<Location>,
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
            id: app.as_str().into(),
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
            id: "store".into(),
            path: store.home().store_dir().to_string_lossy().into_owned(),
            present: true,
            files: files as u64,
            bytes: bytes as u64,
            shared_bytes: 0,
            models: repos as u64,
        },
        archive: store.archive().map(|a| Location {
            id: "archive".into(),
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
        None => bail!("{repo} is not in the archive"),
    }
}
