//! End-to-end pull and placement against a fake Hub, with scratch app folders.
#![cfg(target_os = "macos")]

mod common;

use common::{COMMIT, REPO, Ranged, engine, git_oid, sha256};
use dedupd_core::fetch::{Expect, Fetcher, Progress, Unwatched};
use dedupd_core::hub::TreeEntry;
use dedupd_core::place::{App, AppDirs};
use dedupd_core::profile::{self, Installed};
use dedupd_core::pull::{self, PullRequest};
use dedupd_core::select::Selection;
use dedupd_core::store::Store;
use dedupd_core::{Home, archive, library, maintain};
use serde_json::json;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Upstream {
    server: MockServer,
    weights: Vec<u8>,
    config: Vec<u8>,
    manifest: String,
}

async fn upstream() -> Upstream {
    let weights: Vec<u8> = (0..400_000u32).map(|i| (i % 251) as u8).collect();
    let config = b"{\"a\":1}\n".to_vec();
    let ollama_config = b"{\"model_format\":\"gguf\"}".to_vec();
    let template = b"{{ .Prompt }}".to_vec();
    let manifest = common::ollama_manifest(&ollama_config, &weights, &template);

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/models/{REPO}/revision/main")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"sha": COMMIT, "id": REPO})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/models/{REPO}/tree/{COMMIT}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"type": "file", "path": "model-Q4_K_M.gguf", "size": weights.len(), "oid": "pointer",
             "lfs": {"oid": sha256(&weights), "size": weights.len(), "pointerSize": 134}},
            {"type": "file", "path": "model-Q8_0.gguf", "size": 9, "oid": "pointer2",
             "lfs": {"oid": "f".repeat(64), "size": 9, "pointerSize": 134}},
            {"type": "file", "path": "sub/config.json", "size": config.len(), "oid": git_oid(&config)}
        ])))
        .mount(&server)
        .await;
    for (file, body) in [
        ("model-Q4_K_M.gguf", &weights),
        ("sub/config.json", &config),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("/{REPO}/resolve/{COMMIT}/{file}")))
            .respond_with(Ranged(body.clone()))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path(format!("/v2/{REPO}/manifests/Q4_K_M")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(
                    "content-type",
                    "application/vnd.docker.distribution.manifest.v2+json",
                )
                .set_body_string(manifest.clone()),
        )
        .mount(&server)
        .await;
    for body in [&ollama_config, &template] {
        Mock::given(method("GET"))
            .and(path(format!("/v2/{REPO}/blobs/sha256:{}", sha256(body))))
            .respond_with(Ranged(body.clone()))
            .mount(&server)
            .await;
    }
    Upstream {
        server,
        weights,
        config,
        manifest,
    }
}

fn request(apps: &[App], offline: bool) -> PullRequest {
    PullRequest {
        repo: REPO.into(),
        revision: "main".into(),
        selection: Selection {
            quant: Some("Q4_K_M".into()),
            include: vec![],
        },
        apps: apps.to_vec(),
        offline,
        jobs: 2,
    }
}

fn assert_placed(up: &Upstream, dirs: &AppDirs) {
    // Bionic: plain files under org/repo, writable, same bytes.
    let b = dirs.bionic.join(REPO);
    assert_eq!(fs::read(b.join("model-Q4_K_M.gguf")).unwrap(), up.weights);
    assert_eq!(fs::read(b.join("sub/config.json")).unwrap(), up.config);
    assert_ne!(
        fs::metadata(b.join("sub/config.json")).unwrap().mode() & 0o200,
        0
    );
    assert!(!b.join("model-Q8_0.gguf").exists());

    // Hugging Face cache: blobs named by etag, snapshot links, the branch ref.
    let h = dirs.hf_cache.join("models--org--Repo-GGUF");
    assert_eq!(
        fs::read(h.join("blobs").join(sha256(&up.weights))).unwrap(),
        up.weights
    );
    assert_eq!(
        fs::read(h.join("blobs").join(git_oid(&up.config))).unwrap(),
        up.config
    );
    let snap = h.join("snapshots").join(COMMIT);
    assert!(
        fs::symlink_metadata(snap.join("sub/config.json"))
            .unwrap()
            .is_symlink()
    );
    assert_eq!(fs::read(snap.join("sub/config.json")).unwrap(), up.config);
    assert_eq!(
        fs::read(snap.join("model-Q4_K_M.gguf")).unwrap(),
        up.weights
    );
    assert_eq!(fs::read_to_string(h.join("refs/main")).unwrap(), COMMIT);

    // Ollama: every layer by digest, and the manifest byte for byte.
    let o = &dirs.ollama;
    assert_eq!(
        fs::read(
            o.join("blobs")
                .join(format!("sha256-{}", sha256(&up.weights)))
        )
        .unwrap(),
        up.weights
    );
    assert_eq!(
        fs::read_to_string(o.join("manifests/hf.co").join(REPO).join("Q4_K_M")).unwrap(),
        up.manifest
    );
    assert_eq!(fs::read_dir(o.join("blobs")).unwrap().count(), 3);
}

#[tokio::test]
async fn pull_places_into_three_apps_and_survives_upstream_removal() {
    let up = upstream().await;
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    let progress = Arc::new(Progress::default());

    let e = engine(&home, &up.server.uri(), &dir.path().join("apps1"), None);
    let out = pull::pull(&e, &request(&App::ALL, false), &progress)
        .await
        .unwrap();
    assert_eq!((out.files, out.downloaded, out.from_archive), (2, 2, false));
    assert_eq!(out.commit, COMMIT);
    assert!(out.notes.is_empty(), "{:?}", out.notes);
    assert_placed(&up, &e.dirs);
    // The store's own files stay read-only.
    let blob = e.store.blob_path(&sha256(&up.weights));
    assert_eq!(fs::metadata(&blob).unwrap().mode() & 0o222, 0);

    // A second pull downloads nothing and changes nothing.
    let out = pull::pull(&e, &request(&App::ALL, false), &progress)
        .await
        .unwrap();
    assert_eq!(out.downloaded, 0);
    assert!(
        out.placed
            .iter()
            .all(|(_, p)| p.cloned == 0 && p.conflicts.is_empty())
    );
    let weight_fetches = up
        .server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().ends_with("model-Q4_K_M.gguf"))
        .count();
    assert_eq!(weight_fetches, 4, "four parts, fetched once and not again");

    // Copy to the archive disk, then lose the local copy of the weights.
    let archive_home = Home::new(dir.path().join("trunk/dedupd"));
    assert!(archive::sync(&e.db, &e.store, &archive_home, &mut |_, _| {}).is_err());
    fs::create_dir_all(dir.path().join("trunk")).unwrap();
    let report = archive::sync(&e.db, &e.store, &archive_home, &mut |_, _| {}).unwrap();
    assert_eq!((report.copied, report.failed.len()), (4, 0));
    assert!(archive_home.db_path().is_file());
    fs::remove_file(&blob).unwrap();

    // The repository disappears upstream. The model can still be placed into
    // all three apps, from the archive.
    up.server.reset().await;
    Mock::given(path_regex(".*"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&up.server)
        .await;
    let e2 = engine(
        &home,
        &up.server.uri(),
        &dir.path().join("apps2"),
        Some(archive_home.clone()),
    );
    let out = pull::pull(&e2, &request(&App::ALL, false), &progress)
        .await
        .unwrap();
    assert!(out.from_archive);
    assert_eq!(out.downloaded, 0);
    assert_placed(&up, &e2.dirs);
    assert!(blob.is_file());

    // `place` never asks upstream at all.
    let e3 = engine(&home, "http://127.0.0.1:1", &dir.path().join("apps3"), None);
    pull::pull(&e3, &request(&App::ALL, true), &progress)
        .await
        .unwrap();
    assert_placed(&up, &e3.dirs);
}

#[tokio::test]
async fn a_model_is_removed_from_an_app_and_placed_again() {
    let up = upstream().await;
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    let e = engine(&home, &up.server.uri(), &dir.path().join("apps"), None);
    pull::pull(&e, &request(&App::ALL, false), &Arc::default())
        .await
        .unwrap();

    // The library says where the model is and what it can be used as.
    let installed = Installed::detect(&e.dirs);
    let detail = library::model(&e.db(), &e.store, REPO, &installed)
        .unwrap()
        .unwrap();
    assert_eq!(detail.model.apps.len(), 3);
    assert_eq!(detail.model.meta.formats, ["gguf"]);
    let q4 = detail.variants.iter().find(|v| v.name == "Q4_K_M").unwrap();
    assert_eq!(q4.spec, format!("{REPO}:Q4_K_M"));
    assert_eq!(q4.placed_in, ["bionic", "ollama", "hf"]);
    assert_eq!(q4.usable_by, ["bionic", "ollama", "hf"]);
    // Every app answers with a method and what it costs; no app is
    // installed in the scratch folder, so no version is known.
    assert_eq!(q4.decisions.len(), 3);
    for d in &q4.decisions {
        assert_eq!(
            (d.state.as_str(), d.method.as_deref()),
            ("ready", Some("clone"))
        );
        assert_eq!(d.cost, Some(profile::Cost::None));
        assert_eq!(d.version_state, "unknown");
    }
    assert_eq!(q4.stored_bytes, q4.bytes);
    let q8 = detail.variants.iter().find(|v| v.name == "Q8_0").unwrap();
    assert!(q8.placed_in.is_empty());
    assert_eq!(detail.revisions[0].refs, ["main"]);
    let where_ = library::locations(&e.db(), &e.store, &e.dirs).unwrap();
    assert!(where_.apps.iter().all(|a| a.present && a.models == 1));
    assert!(where_.volume_free > 0 && where_.volume_total > where_.volume_free);

    // Another Ollama model uses the same weights; they must stay for it.
    let other = e.dirs.ollama.join("manifests/hf.co/org/Other/latest");
    fs::create_dir_all(other.parent().unwrap()).unwrap();
    fs::write(&other, &up.manifest).unwrap();
    // The owner edited one of Bionic's files.
    let edited = e.dirs.bionic.join(REPO).join("sub/config.json");
    fs::write(&edited, b"my own edit").unwrap();

    for app in App::ALL {
        let plan = maintain::unplace_plan(&e.db, &e.store, &e.dirs, REPO, app).unwrap();
        // Planning removes nothing.
        assert!(plan.remove.iter().all(|(p, _)| fs::metadata(p).is_ok()));
        maintain::unplace_apply(&e.db, &e.dirs, &plan).unwrap();
        match app {
            App::Bionic => {
                assert_eq!((plan.remove.len(), plan.keep.len()), (1, 1));
                assert_eq!(fs::read(&edited).unwrap(), b"my own edit");
                assert!(!e.dirs.bionic.join(REPO).join("model-Q4_K_M.gguf").exists());
            }
            App::Ollama => {
                assert_eq!((plan.remove.len(), plan.keep.len()), (0, 3));
                assert_eq!(plan.keep[0].1, "another Ollama model uses it");
                assert_eq!(plan.links.len(), 1);
                assert!(!e.dirs.ollama.join("manifests/hf.co").join(REPO).exists());
                assert!(other.is_file());
                assert_eq!(
                    fs::read_dir(e.dirs.ollama.join("blobs")).unwrap().count(),
                    3
                );
            }
            App::Hf => {
                assert_eq!((plan.remove.len(), plan.links.len()), (2, 2));
                assert!(!e.dirs.hf_cache.join("models--org--Repo-GGUF").exists());
                assert!(e.dirs.hf_cache.is_dir());
            }
        }
    }
    // The store still has everything, so the model goes back without the
    // provider.
    assert!(e.store.contains(&sha256(&up.weights)));
    fs::remove_file(&edited).unwrap();
    let e2 = engine(&home, "http://127.0.0.1:1", &dir.path().join("apps"), None);
    pull::pull(&e2, &request(&App::ALL, true), &Arc::default())
        .await
        .unwrap();
    assert_placed(&up, &e2.dirs);
}

/// Archiving a model takes it out of every app and off the internal disk,
/// once the archive disk is known to hold a good copy of each file; restoring
/// copies the files back and leaves the apps alone.
#[tokio::test]
async fn a_model_is_archived_and_restored() {
    let up = upstream().await;
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    let trunk = dir.path().join("trunk");
    let archive_home = Home::new(trunk.join("dedupd"));
    let apps = dir.path().join("apps");
    let e = engine(&home, &up.server.uri(), &apps, Some(archive_home.clone()));
    pull::pull(&e, &request(&App::ALL, false), &Arc::default())
        .await
        .unwrap();
    let installed = Installed::detect(&e.dirs);
    let archived_at = |e: &pull::Engine| {
        library::model(&e.db(), &e.store, REPO, &installed)
            .unwrap()
            .unwrap()
            .model
            .archived_at
    };
    let plan_of = || archive::archive_plan(&e.db, &e.store, &e.dirs, REPO);

    // Without the disk, and for a model dedupd does not have, nothing is planned.
    let refused = plan_of().unwrap_err().to_string();
    assert!(refused.contains("not attached"), "{refused}");
    fs::create_dir_all(&trunk).unwrap();
    assert!(archive::archive_plan(&e.db, &e.store, &e.dirs, "org/Nope").is_err());

    // The weights, the config, and the two small layers Ollama needs.
    let all: Vec<String> = [&up.weights, &up.config]
        .into_iter()
        .map(|b| sha256(b))
        .chain(
            dedupd_core::ollama::Manifest::parse(&up.manifest)
                .unwrap()
                .all()
                .map(|l| l.sha256().unwrap().to_string()),
        )
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    assert_eq!(all.len(), 4);

    // Nothing is on the archive disk yet, so every file would be copied
    // first. Planning changes nothing.
    let plan = plan_of().unwrap();
    assert_eq!(plan.deactivate.len(), 3);
    assert_eq!((plan.remove.len(), plan.copy_files), (4, 4));
    assert!(plan.keep.is_empty());
    assert_placed(&up, &e.dirs);
    assert!(archived_at(&e).is_none());

    // The backup is made, and then one copy on the archive disk goes bad.
    archive::sync(&e.db, &e.store, &archive_home, &mut |_, _| {}).unwrap();
    let bad = archive_home.blob_path(&sha256(&up.config));
    fs::remove_file(&bad).unwrap();
    fs::write(&bad, vec![b'x'; up.config.len()]).unwrap();
    let plan = plan_of().unwrap();
    assert_eq!(plan.copy_files, 0);

    let mut checked = 0;
    let report =
        archive::archive_apply(&e.db, &e.store, &e.dirs, REPO, &mut |n| checked += n).unwrap();
    assert_eq!((report.files, report.copied), (4, 1));
    assert_eq!((report.freed, checked), (plan.bytes, plan.bytes));
    assert_eq!(report.deactivated.len(), 3);
    assert_eq!(fs::read(&bad).unwrap(), up.config);
    // In no app, and off the internal disk; the archive disk has it all.
    assert!(!e.dirs.bionic.join(REPO).exists());
    assert!(!e.dirs.hf_cache.join("models--org--Repo-GGUF").exists());
    assert!(!e.dirs.ollama.join("manifests/hf.co").join(REPO).exists());
    assert_eq!(
        fs::read_dir(e.dirs.ollama.join("blobs")).unwrap().count(),
        0
    );
    for sha in &all {
        assert!(!e.store.contains(sha));
        assert!(archive_home.blob_path(sha).is_file());
    }
    let detail = library::model(&e.db(), &e.store, REPO, &installed)
        .unwrap()
        .unwrap();
    assert!(detail.model.archived_at.is_some());
    assert!(detail.model.apps.is_empty());
    let weights = detail.revisions[0]
        .files
        .iter()
        .find(|f| f.path == "model-Q4_K_M.gguf")
        .unwrap();
    assert!(!weights.stored && weights.archived);
    // Archiving again finds nothing left to do.
    let again = plan_of().unwrap();
    assert!(again.deactivate.is_empty() && again.remove.is_empty());

    // Restoring copies the files back and puts the model into no app.
    let plan = archive::restore_plan(&e.db, &e.store, REPO).unwrap();
    assert_eq!((plan.copy.len(), plan.missing.len()), (4, 0));
    assert!(!e.store.contains(&all[0]));
    let report = archive::restore(&e.db, &e.store, REPO, &mut |_| {}).unwrap();
    assert_eq!((report.files, report.bytes), (4, plan.bytes));
    for sha in &all {
        assert!(e.store.contains(sha));
        assert!(archive_home.blob_path(sha).is_file());
    }
    assert!(archived_at(&e).is_none());
    assert!(!e.dirs.bionic.join(REPO).exists());
    // Activating is then the usual step, without the provider.
    let offline = engine(
        &home,
        "http://127.0.0.1:1",
        &apps,
        Some(archive_home.clone()),
    );
    pull::pull(&offline, &request(&App::ALL, true), &Arc::default())
        .await
        .unwrap();
    assert_placed(&up, &offline.dirs);

    // A file that something else still needs stays on the internal disk:
    // another model has the same weights, and the owner edited one of
    // Bionic's files, so Bionic keeps it.
    offline
        .db()
        .archive_snapshot(
            "hf",
            "org/Other",
            "main",
            &dedupd_core::hub::Snapshot {
                commit: "1".repeat(40),
                info_json: "{}".into(),
                tree_json: "[]".into(),
                entries: vec![TreeEntry {
                    path: "other.gguf".into(),
                    size: up.weights.len() as u64,
                    git_oid: "pointer".into(),
                    lfs_sha256: Some(sha256(&up.weights)),
                }],
            },
        )
        .unwrap();
    let edited = offline.dirs.bionic.join(REPO).join("sub/config.json");
    fs::write(&edited, b"my own edit").unwrap();
    let e = offline;
    let report = archive::archive_apply(&e.db, &e.store, &e.dirs, REPO, &mut |_| {}).unwrap();
    let mut kept: Vec<(&str, &str)> = report
        .keep
        .iter()
        .map(|k| (k.sha256.as_str(), k.why.as_str()))
        .collect();
    kept.sort_by_key(|(_, why)| *why);
    assert_eq!(
        kept,
        [
            (sha256(&up.config).as_str(), "an app still has it"),
            (sha256(&up.weights).as_str(), "another model uses it"),
        ]
    );
    assert_eq!(report.files, 2);
    assert!(e.store.contains(&sha256(&up.weights)));
    assert!(e.store.contains(&sha256(&up.config)));
    assert_eq!(fs::read(&edited).unwrap(), b"my own edit");
    assert!(archived_at(&e).is_some());

    // Activating an archived model brings back what it needs from the
    // archive disk, and the model is no longer archived.
    fs::remove_file(&edited).unwrap();
    pull::pull(&e, &request(&App::ALL, true), &Arc::default())
        .await
        .unwrap();
    assert_placed(&up, &e.dirs);
    assert!(archived_at(&e).is_none());
}

/// An MLX repository's answer for Ollama depends on the bit widths in its
/// `config.json`, which is read from the store.
#[test]
fn an_mlx_model_tells_each_app_what_it_can_do() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    let e = engine(&home, "http://127.0.0.1:9", &dir.path().join("apps"), None);
    let entry = |path: &str, bytes: &[u8], lfs: bool| TreeEntry {
        path: path.to_string(),
        size: bytes.len() as u64,
        git_oid: git_oid(bytes),
        lfs_sha256: lfs.then(|| sha256(bytes)),
    };
    let installed = Installed::unknown().with(App::Ollama, "0.40.16");
    let formats = ["mlx".to_string(), "safetensors".to_string()];
    let variant = |entries: &[TreeEntry]| {
        library::local_variants(&e.db(), &e.store, "org/m", entries, &formats, &installed)
            .unwrap()
            .remove(0)
    };
    let ollama = |entries: &[TreeEntry]| {
        let mut found = variant(entries).decisions.into_iter();
        found.find(|d| d.app == "ollama").unwrap()
    };

    // Before the download nothing is known about the widths, so the answer
    // is the hopeful one.
    let five = br#"{"quantization": {"group_size": 64, "bits": 5}}"#;
    let entries = [
        entry("config.json", five, false),
        entry("model.safetensors", b"weights", true),
    ];
    let before = ollama(&entries);
    assert_eq!(before.state, "ready");
    assert_eq!(before.method.as_deref(), Some("clone-with-rewritten-index"));
    assert_eq!(before.cost, Some(profile::Cost::Megabytes));
    assert_eq!(before.version_state, "tested");
    assert_eq!(variant(&entries).usable_by, ["bionic", "ollama", "hf"]);

    // With the config in the store, a width Ollama cannot run is refused.
    let tmp = e.store.tmp_path("config");
    fs::write(&tmp, five).unwrap();
    e.store.add_tmp(&tmp, &sha256(five)).unwrap();
    e.db()
        .upsert_blob(&sha256(five), five.len() as u64, Some(&git_oid(five)))
        .unwrap();
    let after = ollama(&entries);
    assert_eq!(variant(&entries).usable_by, ["bionic", "hf"]);
    assert_eq!(after.state, "no");
    assert_eq!(after.method, None);
    assert_eq!(
        after.reason,
        "Ollama 0.40.16 cannot run 5-bit MLX models; it runs 4-bit and 8-bit ones"
    );
}

#[tokio::test]
async fn existing_app_files_are_never_overwritten() {
    let up = upstream().await;
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    let e = engine(&home, &up.server.uri(), &dir.path().join("apps"), None);
    let mine = e.dirs.bionic.join(REPO).join("sub/config.json");
    fs::create_dir_all(mine.parent().unwrap()).unwrap();
    fs::write(&mine, b"my own edit").unwrap();
    // The same bytes as upstream, written by the app itself.
    let same = e.dirs.bionic.join(REPO).join("model-Q4_K_M.gguf");
    fs::write(&same, &up.weights).unwrap();
    let ino = fs::metadata(&same).unwrap().ino();

    let out = pull::pull(&e, &request(&[App::Bionic], false), &Arc::default())
        .await
        .unwrap();
    let placed = &out.placed[0].1;
    assert_eq!(
        (placed.cloned, placed.present, placed.conflicts.len()),
        (0, 1, 1)
    );
    assert_eq!(fs::read(&mine).unwrap(), b"my own edit");
    assert_eq!(fs::metadata(&same).unwrap().ino(), ino);
}

#[tokio::test]
async fn interrupted_download_resumes_and_bad_content_is_refused() {
    let up = upstream().await;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&Home::new(dir.path().join("home"))).unwrap();
    let fetcher = Fetcher::new(up.server.uri(), None).unwrap();
    let sha = sha256(&up.weights);
    let url = fetcher.resolve_url(REPO, COMMIT, "model-Q4_K_M.gguf");

    fs::write(
        store.tmp_path(&format!("{sha}.part")),
        &up.weights[..150_000],
    )
    .unwrap();
    let got = fetcher
        .fetch(
            &store,
            &url,
            Some(up.weights.len() as u64),
            &Expect::Sha256(sha.clone()),
            &Unwatched,
        )
        .await
        .unwrap();
    assert_eq!(got.sha256, sha);
    assert_eq!(fs::read(store.blob_path(&sha)).unwrap(), up.weights);
    let requests = up.server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].headers.get("range").unwrap(), "bytes=150000-");

    let wrong = "a".repeat(64);
    let err = fetcher
        .fetch(
            &store,
            &url,
            None,
            &Expect::Sha256(wrong.clone()),
            &Unwatched,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("expected content"));
    assert!(!store.contains(&wrong));
    assert!(
        fs::read_dir(store.home().tmp_dir())
            .unwrap()
            .next()
            .is_none()
    );
}
