//! End-to-end adoption against a fake Hub and a fake Bionic models folder.
#![cfg(target_os = "macos")]

use dedupd_core::Home;
use dedupd_core::adopt::{self, Action, Options, Upstream, Verify};
use dedupd_core::db::Db;
use dedupd_core::hash::hash_file;
use dedupd_core::hub::HubClient;
use dedupd_core::sources::bionic;
use dedupd_core::status::status;
use dedupd_core::store::Store;
use serde_json::json;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

struct Fixture {
    _dir: tempfile::TempDir,
    models: PathBuf,
    home: Home,
}

/// A models folder with one repository: a "weights" file (LFS upstream), a
/// config (plain git upstream), a file edited locally, and one the Hub does
/// not list.
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let models = dir.path().join("models");
    let repo = models.join("org/Repo-GGUF");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("model.gguf"), vec![7u8; 300_000]).unwrap();
    fs::write(repo.join("config.json"), b"{\"a\":1}\n").unwrap();
    fs::write(repo.join("tokenizer.json"), b"edited locally").unwrap();
    fs::write(repo.join("notes.txt"), b"mine").unwrap();
    let home = Home::new(dir.path().join("home"));
    Fixture {
        _dir: dir,
        models,
        home,
    }
}

async fn fake_hub(models: &Path) -> MockServer {
    let repo = models.join("org/Repo-GGUF");
    let weights = hash_file(&repo.join("model.gguf"), false).unwrap();
    let config = hash_file(&repo.join("config.json"), true).unwrap();

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/models/org/Repo-GGUF/revision/main"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"sha": COMMIT, "id": "org/Repo-GGUF"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/models/org/Repo-GGUF/tree/{COMMIT}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"type": "file", "path": "model.gguf", "size": weights.size, "oid": "pointer-oid",
             "lfs": {"oid": weights.sha256, "size": weights.size, "pointerSize": 134},
             "xetHash": "feed"},
            {"type": "file", "path": "config.json", "size": config.size, "oid": config.git_oid},
            {"type": "file", "path": "tokenizer.json", "size": 99, "oid": "some-other-oid"},
            {"type": "file", "path": "README.md", "size": 10, "oid": "readme-oid"}
        ])))
        .mount(&server)
        .await;
    server
}

fn snapshot_of(dir: &Path) -> Vec<(PathBuf, u64, i64, u64, Vec<u8>)> {
    let mut out = Vec::new();
    for e in walkdir::WalkDir::new(dir).sort_by_file_name() {
        let e = e.unwrap();
        if e.file_type().is_file() {
            let m = e.metadata().unwrap();
            out.push((
                e.path().to_path_buf(),
                m.ino(),
                m.mtime() * 1_000_000_000 + m.mtime_nsec(),
                m.mode() as u64,
                fs::read(e.path()).unwrap(),
            ));
        }
    }
    out
}

fn verify_of<'a>(o: &'a adopt::RepoOutcome, rel: &str) -> (Verify, Action, &'a str) {
    let f = o.files.iter().find(|f| f.rel_path == rel).unwrap();
    (f.verify, f.action, &f.sha256)
}

#[tokio::test]
async fn adopts_verifies_and_leaves_app_files_alone() {
    let fx = fixture();
    let server = fake_hub(&fx.models).await;
    let hub = HubClient::new(server.uri(), None).unwrap();
    let store = Store::open(&fx.home).unwrap();
    let mut db = Db::open(&fx.home.db_path()).unwrap();
    let before = snapshot_of(&fx.models);

    let repos = bionic::scan(&fx.models).unwrap();
    let out = adopt::adopt(
        &mut db,
        &store,
        Some(&hub),
        repos,
        &Options::default(),
        &mut |_| {},
    )
    .await
    .unwrap();

    assert_eq!(out.len(), 1);
    let repo = &out[0];
    assert_eq!(repo.upstream, Upstream::Commit(COMMIT.into()));
    assert!(repo.errors.is_empty());
    assert_eq!(verify_of(repo, "model.gguf").0, Verify::Verified);
    assert_eq!(verify_of(repo, "config.json").0, Verify::Verified);
    assert_eq!(verify_of(repo, "tokenizer.json").0, Verify::Mismatch);
    assert_eq!(verify_of(repo, "notes.txt").0, Verify::NotUpstream);
    assert!(repo.files.iter().all(|f| f.action == Action::Cloned));

    // Every file is in the store under its own sha256, read-only, same bytes.
    for f in &repo.files {
        let blob = store.blob_path(&f.sha256);
        assert_eq!(hash_file(&blob, false).unwrap().sha256, f.sha256);
        assert_eq!(fs::metadata(&blob).unwrap().mode() & 0o222, 0);
    }

    // The app's folder is byte-for-byte and inode-for-inode what it was.
    assert_eq!(snapshot_of(&fx.models), before);

    // The Hub's answers are archived verbatim, including fields dedupd does
    // not itself use.
    let (info, tree): (String, String) = db
        .conn()
        .query_row(
            "SELECT info_json, tree_json FROM revisions WHERE commit_sha = ?1",
            [COMMIT],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(info.contains("org/Repo-GGUF"));
    assert!(tree.contains("xetHash") && tree.contains("README.md"));

    let s = status(&db).unwrap();
    assert_eq!(s.store_files, 4);
    assert_eq!(s.apps.len(), 1);
    assert_eq!((s.apps[0].app.as_str(), s.apps[0].files), ("bionic", 4));
    let r = &s.repos[0];
    assert_eq!(
        (r.verified, r.mismatch, r.not_upstream, r.unchecked),
        (2, 1, 1, 0)
    );
    assert_eq!(r.commit.as_deref(), Some(COMMIT));
}

#[tokio::test]
async fn second_run_does_not_rehash_and_offline_keeps_earlier_verification() {
    let fx = fixture();
    let server = fake_hub(&fx.models).await;
    let hub = HubClient::new(server.uri(), None).unwrap();
    let store = Store::open(&fx.home).unwrap();
    let mut db = Db::open(&fx.home.db_path()).unwrap();
    let opts = Options::default();

    let scan = || bionic::scan(&fx.models).unwrap();
    adopt::adopt(&mut db, &store, Some(&hub), scan(), &opts, &mut |_| {})
        .await
        .unwrap();

    // Run again with the Hub gone: nothing is hashed, earlier results stand.
    let out = adopt::adopt(&mut db, &store, None, scan(), &opts, &mut |_| {})
        .await
        .unwrap();
    assert_eq!(out[0].upstream, Upstream::NotAsked);
    assert!(out[0].files.iter().all(|f| f.action == Action::Unchanged));
    assert_eq!(verify_of(&out[0], "model.gguf").0, Verify::Verified);
    assert_eq!(status(&db).unwrap().repos[0].verified, 2);

    // A file the app replaced is hashed again and gets a new store entry;
    // the old one stays, because nothing is deleted automatically.
    let old_sha = verify_of(&out[0], "config.json").2.to_string();
    let config = fx.models.join("org/Repo-GGUF/config.json");
    fs::write(&config, b"{\"a\":2,\"b\":3}\n").unwrap();
    let out = adopt::adopt(&mut db, &store, Some(&hub), scan(), &opts, &mut |_| {})
        .await
        .unwrap();
    let (v, action, new_sha) = verify_of(&out[0], "config.json");
    assert_eq!((v, action), (Verify::Mismatch, Action::Cloned));
    assert_ne!(new_sha, old_sha);
    assert!(store.contains(&old_sha) && store.contains(new_sha));
    assert_eq!(status(&db).unwrap().store_files, 5);
}

#[tokio::test]
async fn offline_first_run_is_unchecked_then_verified_without_rehash() {
    let fx = fixture();
    let store = Store::open(&fx.home).unwrap();
    let mut db = Db::open(&fx.home.db_path()).unwrap();
    let opts = Options::default();
    let scan = || bionic::scan(&fx.models).unwrap();

    let out = adopt::adopt(&mut db, &store, None, scan(), &opts, &mut |_| {})
        .await
        .unwrap();
    assert!(
        out[0]
            .files
            .iter()
            .all(|f| f.verify == Verify::Unchecked && f.action == Action::Cloned)
    );

    let server = fake_hub(&fx.models).await;
    let hub = HubClient::new(server.uri(), None).unwrap();
    let out = adopt::adopt(&mut db, &store, Some(&hub), scan(), &opts, &mut |_| {})
        .await
        .unwrap();
    assert!(out[0].files.iter().all(|f| f.action == Action::Unchanged));
    assert_eq!(verify_of(&out[0], "model.gguf").0, Verify::Verified);
    assert_eq!(verify_of(&out[0], "config.json").0, Verify::Verified);
    assert_eq!(verify_of(&out[0], "tokenizer.json").0, Verify::Mismatch);
}

#[tokio::test]
async fn dry_run_writes_nothing() {
    let fx = fixture();
    let server = fake_hub(&fx.models).await;
    let hub = HubClient::new(server.uri(), None).unwrap();
    let store = Store::open(&fx.home).unwrap();
    let mut db = Db::open(&fx.home.db_path()).unwrap();
    let opts = Options {
        dry_run: true,
        ..Options::default()
    };

    let out = adopt::adopt(
        &mut db,
        &store,
        Some(&hub),
        bionic::scan(&fx.models).unwrap(),
        &opts,
        &mut |_| {},
    )
    .await
    .unwrap();
    assert!(out[0].files.iter().all(|f| f.action == Action::DryRun));
    assert_eq!(verify_of(&out[0], "model.gguf").0, Verify::Verified);
    let s = status(&db).unwrap();
    assert_eq!((s.store_files, s.repos.len()), (0, 0));
    assert!(!store.contains(verify_of(&out[0], "model.gguf").2));
}

#[tokio::test]
async fn repo_missing_upstream_is_still_adopted() {
    let fx = fixture();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let hub = HubClient::new(server.uri(), None).unwrap();
    let store = Store::open(&fx.home).unwrap();
    let mut db = Db::open(&fx.home.db_path()).unwrap();

    let out = adopt::adopt(
        &mut db,
        &store,
        Some(&hub),
        bionic::scan(&fx.models).unwrap(),
        &Options::default(),
        &mut |_| {},
    )
    .await
    .unwrap();
    assert!(matches!(out[0].upstream, Upstream::NotAvailable(_)));
    assert!(
        out[0]
            .files
            .iter()
            .all(|f| f.verify == Verify::Unchecked && f.action == Action::Cloned)
    );
    assert_eq!(status(&db).unwrap().store_files, 4);
}
