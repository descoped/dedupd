//! End-to-end pull and placement against a fake Hub, with scratch app folders.
#![cfg(target_os = "macos")]

mod common;

use common::{COMMIT, REPO, Ranged, engine, git_oid, sha256};
use dedupd_api::app::App;
use dedupd_api::library::{Cost, DecisionState, VersionState};
use dedupd_core::fetch::{Expect, Fetcher, Progress, Unwatched};
use dedupd_core::hub::TreeEntry;
use dedupd_core::place::AppDirs;
use dedupd_core::profile::Installed;
use dedupd_core::pull::{self, PullRequest};
use dedupd_core::select::Selection;
use dedupd_core::store::Store;
use dedupd_core::{Home, adopt, archive, library, maintain};
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
            .all(|p| p.cloned == 0 && p.conflicts.is_empty())
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
    let detail = library::model(&e.db, &e.store, &e.dirs, REPO, &installed)
        .unwrap()
        .unwrap();
    assert_eq!(detail.model.apps.len(), 3);
    assert_eq!(detail.model.meta.formats, ["gguf"]);
    let q4 = detail.variants.iter().find(|v| v.name == "Q4_K_M").unwrap();
    assert_eq!(q4.spec, format!("{REPO}:Q4_K_M"));
    assert_eq!(q4.placed_in, App::ALL);
    assert_eq!(q4.usable_by, App::ALL);
    // Every app answers with a method and what it costs; no app is
    // installed in the scratch folder, so no version is known.
    assert_eq!(q4.decisions.len(), 3);
    for d in &q4.decisions {
        assert_eq!(
            (d.state, d.method.as_deref()),
            (DecisionState::Ready, Some("clone"))
        );
        assert_eq!(d.cost, Some(Cost::None));
        assert_eq!(d.version_state, VersionState::Unknown);
    }
    assert_eq!(q4.stored_bytes, q4.bytes);
    assert!(q4.weights_stored);
    let q8 = detail.variants.iter().find(|v| v.name == "Q8_0").unwrap();
    assert!(q8.placed_in.is_empty());
    assert!(!q8.weights_stored);
    assert_eq!(detail.revisions[0].refs, ["main"]);
    let where_ = library::locations(&e.db, &e.store, &e.dirs).unwrap();
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
        assert!(plan.remove.iter().all(|f| fs::metadata(&f.path).is_ok()));
        maintain::unplace_apply(&e.db, &e.dirs, &plan).unwrap();
        match app {
            App::Bionic => {
                assert_eq!((plan.remove.len(), plan.keep.len()), (1, 1));
                assert_eq!(fs::read(&edited).unwrap(), b"my own edit");
                assert!(!e.dirs.bionic.join(REPO).join("model-Q4_K_M.gguf").exists());
            }
            App::Ollama => {
                assert_eq!((plan.remove.len(), plan.keep.len()), (0, 3));
                assert_eq!(plan.keep[0].why, "another Ollama model uses it");
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
async fn links_and_manifests_stay_with_the_files_that_stay() {
    let up = upstream().await;
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    let e = engine(&home, &up.server.uri(), &dir.path().join("apps"), None);
    pull::pull(&e, &request(&App::ALL, false), &Arc::default())
        .await
        .unwrap();
    let snap = e
        .dirs
        .hf_cache
        .join("models--org--Repo-GGUF/snapshots")
        .join(COMMIT);

    // The owner agrees to remove the weights from the Hugging Face cache but
    // not the config: the config's link stays with it.
    let plan = maintain::unplace_plan(&e.db, &e.store, &e.dirs, REPO, App::Hf).unwrap();
    let weights = plan
        .remove
        .iter()
        .find(|f| f.path.ends_with(&sha256(&up.weights)))
        .unwrap()
        .path
        .clone();
    let plan = plan.only(std::slice::from_ref(&weights));
    assert_eq!(plan.link_paths().len(), 1);
    assert_eq!(maintain::unplace_apply(&e.db, &e.dirs, &plan).unwrap(), 1);
    assert!(fs::symlink_metadata(snap.join("model-Q4_K_M.gguf")).is_err());
    assert_eq!(fs::read(snap.join("sub/config.json")).unwrap(), up.config);

    // A file that changes after the plan was made stays, and so does its
    // link.
    let plan = maintain::unplace_plan(&e.db, &e.store, &e.dirs, REPO, App::Hf).unwrap();
    assert_eq!(plan.link_paths().len(), 1);
    let config = &plan.remove[0].path;
    fs::write(config, b"edited after the plan").unwrap();
    assert_eq!(maintain::unplace_apply(&e.db, &e.dirs, &plan).unwrap(), 0);
    assert!(fs::symlink_metadata(snap.join("sub/config.json")).is_ok());

    // One of Ollama's layers was changed: the model stays whole in Ollama,
    // manifest and all, and only the untouched layers could go.
    let manifest = e
        .dirs
        .ollama
        .join("manifests/hf.co")
        .join(REPO)
        .join("Q4_K_M");
    let layer = e
        .dirs
        .ollama
        .join("blobs")
        .join(format!("sha256-{}", sha256(&up.weights)));
    let blobs = || fs::read_dir(e.dirs.ollama.join("blobs")).unwrap().count();
    assert_eq!(blobs(), 3);

    // Another model that Ollama knows only through its second tree, as its
    // own import writes it (a name that links to a manifest among the
    // blobs), uses the same layers: they are not offered for removal.
    let other = e.dirs.ollama.join("manifests-v2/ollama.com/library/other");
    fs::create_dir_all(&other).unwrap();
    let manifest_blob = e.dirs.ollama.join("blobs/sha256-manifest-of-other");
    fs::copy(&manifest, &manifest_blob).unwrap();
    std::os::unix::fs::symlink(&manifest_blob, other.join("latest")).unwrap();
    let plan = maintain::unplace_plan(&e.db, &e.store, &e.dirs, REPO, App::Ollama).unwrap();
    assert_eq!((plan.remove.len(), plan.keep.len()), (0, 3), "{plan:?}");
    fs::remove_dir_all(e.dirs.ollama.join("manifests-v2")).unwrap();
    fs::remove_file(&manifest_blob).unwrap();

    // The model itself in both trees, as Ollama's own pull leaves one: each
    // of its two manifests is listed once.
    let twin = e
        .dirs
        .ollama
        .join("manifests-v2/hf.co")
        .join(REPO)
        .join("Q4_K_M");
    fs::create_dir_all(twin.parent().unwrap()).unwrap();
    fs::copy(&manifest, &twin).unwrap();
    let plan = maintain::unplace_plan(&e.db, &e.store, &e.dirs, REPO, App::Ollama).unwrap();
    assert_eq!(plan.link_paths().len(), 2, "{:?}", plan.link_paths());
    fs::remove_dir_all(e.dirs.ollama.join("manifests-v2")).unwrap();

    // The owner agrees to some of the layers only: the model stays whole.
    let plan = maintain::unplace_plan(&e.db, &e.store, &e.dirs, REPO, App::Ollama).unwrap();
    assert_eq!((plan.remove.len(), plan.link_paths().len()), (3, 1));
    let some = plan.only(std::slice::from_ref(&layer.to_string_lossy().into_owned()));
    assert!(some.remove.is_empty() && some.link_paths().is_empty());
    assert_eq!(some.keep.len(), 3);

    // A layer changes after the plan was made: nothing is removed.
    let plan = maintain::unplace_plan(&e.db, &e.store, &e.dirs, REPO, App::Ollama).unwrap();
    fs::set_permissions(&layer, std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
    fs::write(&layer, b"changed").unwrap();
    assert_eq!(maintain::unplace_apply(&e.db, &e.dirs, &plan).unwrap(), 0);
    assert!(manifest.is_file());
    assert_eq!(blobs(), 3);

    // Planned with the changed layer: every layer and the manifest stay.
    let plan = maintain::unplace_plan(&e.db, &e.store, &e.dirs, REPO, App::Ollama).unwrap();
    assert_eq!((plan.remove.len(), plan.keep.len()), (0, 3));
    assert!(plan.link_paths().is_empty());
    maintain::unplace_apply(&e.db, &e.dirs, &plan).unwrap();
    assert!(manifest.is_file());
    assert_eq!(blobs(), 3);
}

/// Two archives and no backup: an archive goes where the owner says, and a
/// restore finds the files there.
#[tokio::test]
async fn a_model_is_archived_to_the_archive_named_and_restored_from_it() {
    let up = upstream().await;
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    let apps = dir.path().join("apps");
    let e = engine(&home, &up.server.uri(), &apps, None);
    pull::pull(&e, &request(&App::ALL, false), &Arc::default())
        .await
        .unwrap();
    let drawer = Home::new(dir.path().join("drawer/dedupd"));
    let nas = Home::new(dir.path().join("nas/dedupd"));
    fs::create_dir_all(dir.path().join("drawer")).unwrap();
    let named = |name: &str, home: &Home| dedupd_core::paths::Archive {
        name: name.into(),
        home: home.clone(),
        backup: false,
    };
    e.store
        .set_archives(vec![named("Drawer", &drawer), named("NAS", &nas)]);

    // Two archives and no backup: the owner says which.
    let unsaid = archive::archive_plan(&e.db, &e.store, &e.dirs, REPO, None).unwrap_err();
    assert!(unsaid.to_string().contains("Drawer, NAS"), "{unsaid}");
    // The NAS is not attached (its parent folder is not there).
    let away = archive::archive_plan(&e.db, &e.store, &e.dirs, REPO, Some("NAS")).unwrap_err();
    assert!(away.to_string().contains("not attached"), "{away}");
    let unknown = archive::archive_plan(&e.db, &e.store, &e.dirs, REPO, Some("Attic")).unwrap_err();
    assert!(
        unknown.to_string().contains("no archive named Attic"),
        "{unknown}"
    );

    let plan = archive::archive_plan(&e.db, &e.store, &e.dirs, REPO, Some("drawer")).unwrap();
    assert_eq!(plan.archive_name, "Drawer");
    let report =
        archive::archive_apply(&e.db, &e.store, &e.dirs, REPO, Some("drawer"), &mut |_| {})
            .unwrap();
    assert_eq!(report.archive, "Drawer");
    assert!(report.files > 0 && report.database_copy_failed.is_none());
    // The archive can be read on its own.
    assert!(drawer.db_path().is_file());
    for blob in &plan.remove {
        assert!(drawer.blob_path(&blob.sha256).is_file());
        assert!(!e.store.contains(&blob.sha256));
    }
    let installed = Installed::detect(&e.dirs);
    let detail = library::model(&e.db, &e.store, &e.dirs, REPO, &installed)
        .unwrap()
        .unwrap();
    assert_eq!(detail.archives, ["Drawer"]);
    let places = library::locations(&e.db, &e.store, &e.dirs)
        .unwrap()
        .archives;
    assert_eq!(
        places
            .iter()
            .map(|p| (p.name.as_deref(), p.present))
            .collect::<Vec<_>>(),
        [(Some("Drawer"), true), (Some("NAS"), false)]
    );
    let last = dedupd_core::activity::recent(&home, 1);
    assert!(
        last[0].text.starts_with("Archived to Drawer: "),
        "{}",
        last[0].text
    );

    // The drawer is unplugged, as a cold archive mostly is: a check of the
    // store counts the model's files as not looked for, never as missing.
    fs::rename(dir.path().join("drawer"), dir.path().join("drawer-away")).unwrap();
    let check = dedupd_core::maintain::verify(&e.db, &e.store, false, 1).unwrap();
    assert!(check.missing.is_empty(), "{:?}", check.missing);
    assert_eq!(check.archive_away, plan.remove.len());
    fs::rename(dir.path().join("drawer-away"), dir.path().join("drawer")).unwrap();

    let restore_plan = archive::restore_plan(&e.db, &e.store, REPO, None).unwrap();
    assert_eq!(restore_plan.archives, ["Drawer"]);
    archive::restore(&e.db, &e.store, REPO, None, &mut |_| {}).unwrap();
    for blob in &plan.remove {
        assert!(e.store.contains(&blob.sha256));
    }

    // A model whose files are all in a store needs nothing restored, with
    // or without an archive in sight.
    e.store.set_archives(vec![named("NAS", &nas)]);
    let nothing = archive::restore_plan(&e.db, &e.store, REPO, None).unwrap_err();
    assert!(
        nothing.to_string().contains("nothing to restore"),
        "{nothing}"
    );
    // With files that are in no store and the disk away, a restore names
    // the archives.
    for blob in &plan.remove {
        e.store.remove_everywhere(&blob.sha256).unwrap();
    }
    let none = archive::restore_plan(&e.db, &e.store, REPO, None).unwrap_err();
    assert!(none.to_string().contains("(NAS)"), "{none}");
}

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
    let archived_at = |e: &dedupd_core::engine::Engine| {
        library::model(&e.db, &e.store, &e.dirs, REPO, &installed)
            .unwrap()
            .unwrap()
            .model
            .archived_at
    };
    let plan_of = || archive::archive_plan(&e.db, &e.store, &e.dirs, REPO, None);

    // Without the disk, and for a model dedupd does not have, nothing is planned.
    let refused = plan_of().unwrap_err().to_string();
    assert!(refused.contains("not attached"), "{refused}");
    // A disk that is not attached is counted as holding nothing.
    let on_archive = |e: &dedupd_core::engine::Engine| {
        let place = library::locations(&e.db, &e.store, &e.dirs)
            .unwrap()
            .archives
            .into_iter()
            .next()
            .unwrap();
        (place.present, place.files, place.bytes, place.models)
    };
    assert_eq!(on_archive(&e), (false, 0, 0, 0));
    fs::create_dir_all(&trunk).unwrap();
    assert_eq!(on_archive(&e), (true, 0, 0, 0));
    assert!(archive::archive_plan(&e.db, &e.store, &e.dirs, "org/Nope", None).is_err());

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
        archive::archive_apply(&e.db, &e.store, &e.dirs, REPO, None, &mut |n| checked += n)
            .unwrap();
    assert_eq!((report.files, report.copied), (4, 1));
    assert_eq!((report.freed, checked), (plan.bytes, plan.bytes));
    assert_eq!(report.deactivated.len(), 3);
    assert_eq!(fs::read(&bad).unwrap(), up.config);
    // The archive disk is counted by what it holds: every file, and the
    // model whole. The store holds none of them now.
    assert_eq!(on_archive(&e), (true, 4, plan.bytes, 1));
    let places = library::locations(&e.db, &e.store, &e.dirs).unwrap();
    assert_eq!((places.store.files, places.store.bytes), (0, 0));
    // A model with a file missing there is not counted as held.
    let config = archive_home.blob_path(&sha256(&up.config));
    let kept = fs::read(&config).unwrap();
    fs::remove_file(&config).unwrap();
    assert_eq!(on_archive(&e).3, 0);
    assert_eq!(on_archive(&e).1, 3);
    fs::write(&config, kept).unwrap();
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
    let detail = library::model(&e.db, &e.store, &e.dirs, REPO, &installed)
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
    let plan = archive::restore_plan(&e.db, &e.store, REPO, None).unwrap();
    assert_eq!((plan.copy.len(), plan.missing.len()), (4, 0));
    assert!(!e.store.contains(&all[0]));
    let report = archive::restore(&e.db, &e.store, REPO, None, &mut |_| {}).unwrap();
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
    // A paused download of the model could fetch its files again after the
    // archive, so it holds the model until it is cancelled or finished.
    let job = serde_json::json!({ "repo": REPO }).to_string();
    let id = e.db().insert_download("paused").unwrap();
    e.db().save_download(id, "paused", &job).unwrap();
    let refused =
        archive::archive_apply(&e.db, &e.store, &e.dirs, REPO, None, &mut |_| {}).unwrap_err();
    assert!(
        format!("{refused:#}").contains("not finished"),
        "{refused:#}"
    );
    assert!(archived_at(&e).is_none());
    e.db().save_download(id, "cancelled", &job).unwrap();
    let report = archive::archive_apply(&e.db, &e.store, &e.dirs, REPO, None, &mut |_| {}).unwrap();
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
        library::local_variants(
            &e.db, &e.store, &e.dirs, "org/m", entries, &formats, &installed,
        )
        .unwrap()
        .remove(0)
    };
    let ollama = |entries: &[TreeEntry]| {
        let mut found = variant(entries).decisions.into_iter();
        found.find(|d| d.app == App::Ollama).unwrap()
    };

    // Before the download nothing is known about the widths, so the answer
    // is the hopeful one.
    let five = br#"{"quantization": {"group_size": 64, "bits": 5}}"#;
    let entries = [
        entry("config.json", five, false),
        entry("model.safetensors", b"weights", true),
    ];
    let before = ollama(&entries);
    assert_eq!(before.state, DecisionState::Ready);
    assert_eq!(before.method.as_deref(), Some("clone-with-rewritten-index"));
    assert_eq!(before.cost, Some(Cost::Megabytes));
    assert_eq!(before.version_state, VersionState::Tested);
    assert_eq!(variant(&entries).usable_by, App::ALL);

    // With the config in the store, a width Ollama cannot run is refused.
    let tmp = e.store.tmp_path("config");
    fs::write(&tmp, five).unwrap();
    e.store.add_tmp(&tmp, &sha256(five)).unwrap();
    e.db()
        .upsert_blob(&sha256(five), five.len() as u64, Some(&git_oid(five)))
        .unwrap();
    let after = ollama(&entries);
    assert_eq!(variant(&entries).usable_by, [App::Bionic, App::Hf]);
    assert_eq!(after.state, DecisionState::No);
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
    let placed = &out.placed[0];
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

#[tokio::test]
async fn files_only_the_store_has_are_recorded_again_after_the_database_is_removed() {
    let up = upstream().await;
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    let apps = dir.path().join("apps");
    let e = engine(&home, &up.server.uri(), &apps, None);
    pull::pull(&e, &request(&[App::Bionic], false), &Arc::default())
        .await
        .unwrap();
    let dirs = e.dirs.clone();
    drop(e);

    // The database is removed, and Bionic never had the small file: only
    // the store has it.
    for name in ["dedupd.sqlite", "dedupd.sqlite-wal", "dedupd.sqlite-shm"] {
        let _ = fs::remove_file(home.root().join(name));
    }
    fs::remove_file(dirs.bionic.join(REPO).join("sub/config.json")).unwrap();
    let e = engine(&home, &up.server.uri(), &apps, None);
    let repos = adopt::scan_apps(&e.db, &e.dirs, &[App::Bionic]).unwrap();
    adopt::adopt(
        &e.db,
        &e.store,
        Some(&e.hub),
        repos,
        &adopt::Options::default(),
        &mut |_| {},
    )
    .await
    .unwrap();
    let q4 = || {
        let installed = Installed::detect(&e.dirs);
        library::model(&e.db, &e.store, &e.dirs, REPO, &installed)
            .unwrap()
            .unwrap()
            .variants
            .into_iter()
            .find(|v| v.name == "Q4_K_M")
            .unwrap()
    };
    // The weights are stored, so it can be activated; the small file
    // counts as missing until the scan records it.
    let before = q4();
    assert!(before.weights_stored);
    assert!(before.stored_bytes < before.bytes);

    let mut looked_at = Default::default();
    let found = adopt::recognise_stored(&e.db, &e.store, &mut looked_at).unwrap();
    assert_eq!((found.files, found.damaged, found.unused), (1, 0, 0));
    assert_eq!(q4().stored_bytes, q4().bytes);
    // Recorded once: the next scan finds nothing new.
    let again = adopt::recognise_stored(&e.db, &e.store, &mut looked_at).unwrap();
    assert_eq!(again, adopt::Recognised::default());
}

#[test]
fn a_store_file_that_is_damaged_or_that_nothing_refers_to_is_not_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    let store = Store::open(&home).unwrap();
    let db = std::sync::Mutex::new(dedupd_core::db::Db::open(&home.db_path()).unwrap());
    let put = |name: &str, content: &[u8]| {
        let path = store.blob_path(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
        path
    };
    let damaged = put(&sha256(b"what the name says"), b"something else");
    // Good, but no repository refers to it. Recorded, it would be a file
    // `gc` offers to remove, its copy on the archive disk with it.
    put(&sha256(b"nobody's"), b"nobody's");

    let mut looked_at = Default::default();
    let found = adopt::recognise_stored(&db, &store, &mut looked_at).unwrap();
    assert_eq!((found.files, found.damaged, found.unused), (0, 1, 1));
    assert!(db.lock().unwrap().all_blobs().unwrap().is_empty());
    // Not read again while they look the same; read again once one changes.
    let again = adopt::recognise_stored(&db, &store, &mut looked_at).unwrap();
    assert_eq!(again, adopt::Recognised::default());
    fs::write(&damaged, b"something else again").unwrap();
    let changed = adopt::recognise_stored(&db, &store, &mut looked_at).unwrap();
    assert_eq!((changed.damaged, changed.unused), (1, 0));
}

fn branch(dirs: &AppDirs) -> std::path::PathBuf {
    dedupd_core::place::hf_repo_dir(dirs, REPO).join("refs/main")
}

#[tokio::test]
async fn a_branch_in_the_cache_is_moved_by_the_providers_answer_and_not_by_the_kept_copy() {
    let up = upstream().await;
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    let other = "b".repeat(40);

    // The owner's own tools left the branch at another commit. The
    // provider's answer of now moves it.
    let e = engine(&home, &up.server.uri(), &dir.path().join("apps"), None);
    fs::create_dir_all(branch(&e.dirs).parent().unwrap()).unwrap();
    fs::write(branch(&e.dirs), &other).unwrap();
    pull::pull(&e, &request(&[App::Hf], false), &Arc::default())
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(branch(&e.dirs)).unwrap(), COMMIT);
    // Nothing is left beside it.
    let beside: Vec<_> = fs::read_dir(branch(&e.dirs).parent().unwrap())
        .unwrap()
        .flatten()
        .map(|f| f.file_name())
        .collect();
    assert_eq!(beside, ["main"]);

    // Placed from the kept copy, the same branch stays where it is: the
    // other commit may be the newer one.
    let e = engine(&home, "http://127.0.0.1:1", &dir.path().join("apps2"), None);
    fs::create_dir_all(branch(&e.dirs).parent().unwrap()).unwrap();
    fs::write(branch(&e.dirs), &other).unwrap();
    pull::pull(&e, &request(&[App::Hf], true), &Arc::default())
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(branch(&e.dirs)).unwrap(), other);
    // Where there is no branch yet, the kept copy names it.
    let e = engine(&home, "http://127.0.0.1:1", &dir.path().join("apps3"), None);
    pull::pull(&e, &request(&[App::Hf], true), &Arc::default())
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(branch(&e.dirs)).unwrap(), COMMIT);
}

#[tokio::test]
async fn a_branch_is_not_named_while_a_file_of_the_snapshot_is_missing() {
    let up = upstream().await;
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    let e = engine(&home, &up.server.uri(), &dir.path().join("apps"), None);
    // Something else is where the weights would go.
    let blob = dedupd_core::place::hf_repo_dir(&e.dirs, REPO)
        .join("blobs")
        .join(sha256(&up.weights));
    fs::create_dir_all(blob.parent().unwrap()).unwrap();
    fs::write(&blob, b"other bytes").unwrap();

    let out = pull::pull(&e, &request(&[App::Hf], false), &Arc::default())
        .await
        .unwrap();
    assert_eq!(out.placed[0].conflicts.len(), 1);
    assert!(!out.skipped[0].temporary);
    assert_eq!(fs::read(&blob).unwrap(), b"other bytes");
    assert!(!branch(&e.dirs).exists());
}

#[tokio::test]
async fn a_manifest_of_another_model_under_the_same_name_is_left_alone() {
    let up = upstream().await;
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    let e = engine(&home, &up.server.uri(), &dir.path().join("apps"), None);
    let manifest = e
        .dirs
        .ollama
        .join("manifests/hf.co")
        .join(REPO)
        .join("Q4_K_M");
    // A model the owner made under the name: other weights.
    let theirs = common::ollama_manifest(b"{}", b"their own weights", b"{{ .Prompt }}");
    fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    fs::write(&manifest, &theirs).unwrap();

    let out = pull::pull(&e, &request(&[App::Ollama], false), &Arc::default())
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(&manifest).unwrap(), theirs);
    assert_eq!(out.placed[0].conflicts.len(), 1, "{:?}", out.placed);
    assert!(out.placed[0].conflicts[0].contains("names another model"));
    assert!(out.activated().is_empty());

    // The same layers written another way are this model: replaced.
    let spaced = up.manifest.replace("\"layers\":", "\"layers\": ");
    assert_ne!(spaced, up.manifest);
    fs::write(&manifest, &spaced).unwrap();
    let out = pull::pull(&e, &request(&[App::Ollama], false), &Arc::default())
        .await
        .unwrap();
    assert!(out.placed[0].conflicts.is_empty(), "{:?}", out.placed);
    assert_eq!(fs::read_to_string(&manifest).unwrap(), up.manifest);
}

#[tokio::test]
async fn one_apps_failure_does_not_stop_the_others() {
    let up = upstream().await;
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    let e = engine(&home, &up.server.uri(), &dir.path().join("apps"), None);
    // Bionic's folder for the organisation cannot be made: a file is there.
    let org = e.dirs.bionic.join(REPO.split('/').next().unwrap());
    fs::create_dir_all(&e.dirs.bionic).unwrap();
    fs::write(&org, b"in the way").unwrap();

    let out = pull::pull(&e, &request(&App::ALL, false), &Arc::default())
        .await
        .unwrap();
    assert_eq!(out.activated(), [App::Ollama, App::Hf]);
    let skipped: Vec<_> = out.skipped.iter().map(|s| (s.app, s.temporary)).collect();
    // Worth another try once the owner has looked.
    assert_eq!(skipped, [(App::Bionic, true)]);
    assert!(
        out.skipped[0].reason.contains("creating"),
        "{:?}",
        out.skipped
    );
    assert_eq!(fs::read(&org).unwrap(), b"in the way");
    // The history is told of all three.
    let told = dedupd_core::activity::recent(&home, 10);
    assert_eq!(told.len(), 3, "{told:?}");
}

/// A scratch APFS volume, a disk image attached for one test and detached
/// when it ends. `None` where `hdiutil` cannot make one.
struct ScratchVolume {
    mount: std::path::PathBuf,
}

impl ScratchVolume {
    fn attach(dir: &std::path::Path) -> Option<Self> {
        let name = format!("dedupd-test-{}", std::process::id());
        let image = dir.join("volume.dmg");
        let made = std::process::Command::new("hdiutil")
            .args([
                "create", "-quiet", "-size", "64m", "-fs", "APFS", "-volname", &name,
            ])
            .arg(&image)
            .status()
            .ok()?;
        if !made.success() {
            return None;
        }
        let mount = dir.join("mount");
        fs::create_dir_all(&mount).ok()?;
        let attached = std::process::Command::new("hdiutil")
            .args(["attach", "-quiet", "-nobrowse", "-mountpoint"])
            .arg(&mount)
            .arg(&image)
            .status()
            .ok()?;
        attached.success().then_some(Self { mount })
    }
}

impl Drop for ScratchVolume {
    fn drop(&mut self) {
        let _ = std::process::Command::new("hdiutil")
            .args(["detach", "-quiet", "-force"])
            .arg(&self.mount)
            .status();
    }
}

#[tokio::test]
async fn a_model_goes_to_the_store_on_the_disk_of_the_app_and_is_copied_between_stores() {
    let up = upstream().await;
    let dir = tempfile::tempdir().unwrap();
    let Some(volume) = ScratchVolume::attach(dir.path()) else {
        eprintln!("skipped: hdiutil could not attach a scratch APFS volume");
        return;
    };
    let home = Home::new(dir.path().join("home"));
    let engine = engine(&home, &up.server.uri(), &dir.path().join("apps"), None);
    // Bionic keeps its models on the other disk; the cache stays here.
    let mut owned = Arc::try_unwrap(engine).ok().unwrap();
    owned.dirs.bionic = volume.mount.join("bionic");
    let e = Arc::new(owned);
    let envoy = Home::new(volume.mount.join("dedupd"));
    fs::create_dir_all(envoy.root()).unwrap();
    e.store.set_stores(vec![dedupd_core::paths::StoreRoot {
        name: "Envoy".into(),
        home: envoy.clone(),
        primary: false,
    }]);
    let weights = sha256(&up.weights);

    // For Bionic alone, the download goes to the store on its disk, and
    // the internal disk holds none of it.
    let progress = Arc::new(Progress::default());
    let out = pull::pull(&e, &request(&[App::Bionic], false), &progress)
        .await
        .unwrap();
    assert_eq!(out.downloaded, 2);
    assert_eq!(out.placed.len(), 1, "{:?}", out.skipped);
    assert!(envoy.blob_path(&weights).is_file());
    assert!(!home.blob_path(&weights).is_file());
    let placed = e.dirs.bionic.join(REPO).join("model-Q4_K_M.gguf");
    assert_eq!(fs::read(&placed).unwrap(), up.weights);
    assert!(e.store.contains_anywhere(&weights));

    // Before the cache is given it, the library says what activating it
    // there copies: the files, from the other store.
    let installed = Installed::default();
    let detail = library::model(&e.db, &e.store, &e.dirs, REPO, &installed)
        .unwrap()
        .unwrap();
    let variant = detail
        .variants
        .iter()
        .find(|v| v.weights_stored)
        .expect("a stored variant");
    let to_cache = variant.copies.iter().find(|c| c.app == App::Hf).unwrap();
    assert_eq!(to_cache.store.as_deref(), Some("Internal"));
    assert_eq!(to_cache.bytes, variant.bytes);
    let to_bionic = variant
        .copies
        .iter()
        .find(|c| c.app == App::Bionic)
        .unwrap();
    assert_eq!(
        (to_bionic.store.as_deref(), to_bionic.bytes),
        (Some("Envoy"), 0)
    );

    // The cache is on the internal disk: nothing is downloaded again, the
    // files are copied from the other store into this one.
    let out = pull::pull(&e, &request(&[App::Hf], false), &progress)
        .await
        .unwrap();
    assert_eq!(out.downloaded, 0);
    assert_eq!(out.placed.len(), 1, "{:?}", out.skipped);
    assert!(home.blob_path(&weights).is_file());
    let blob = e
        .dirs
        .hf_cache
        .join("models--org--Repo-GGUF/blobs")
        .join(&weights);
    assert_eq!(fs::read(blob).unwrap(), up.weights);

    // An app folder on a disk with no store is not given the model, and
    // trying again does not help until a store is added there.
    e.store.set_stores(vec![]);
    fs::remove_dir_all(e.dirs.bionic.join(REPO)).unwrap();
    let out = pull::pull(&e, &request(&[App::Bionic], false), &progress)
        .await
        .unwrap();
    assert!(out.placed.is_empty());
    let skipped = &out.skipped[0];
    assert!(!skipped.temporary, "{skipped:?}");
    assert!(skipped.reason.contains("no store"), "{}", skipped.reason);
    drop(e);
    drop(volume);
}
