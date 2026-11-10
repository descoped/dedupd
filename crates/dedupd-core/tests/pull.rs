//! End-to-end pull and placement against a fake Hub, with scratch app folders.
#![cfg(target_os = "macos")]

use dedupd_core::db::Db;
use dedupd_core::fetch::{Expect, Fetcher, Progress};
use dedupd_core::hub::HubClient;
use dedupd_core::place::{App, AppDirs};
use dedupd_core::pull::{self, Engine, PullRequest};
use dedupd_core::select::Selection;
use dedupd_core::store::Store;
use dedupd_core::{Home, archive};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const REPO: &str = "org/Repo-GGUF";

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn git_oid(bytes: &[u8]) -> String {
    use sha1::Sha1;
    let mut h = Sha1::new();
    h.update(format!("blob {}\0", bytes.len()).as_bytes());
    h.update(bytes);
    hex::encode(h.finalize())
}

/// Serves a body and honours `Range: bytes=N-`, as the Hub's CDN does.
struct Ranged(Vec<u8>);

impl Respond for Ranged {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let range = req
            .headers
            .get("range")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("bytes="))
            .and_then(|v| v.strip_suffix('-'))
            .and_then(|v| v.parse::<usize>().ok());
        match range {
            Some(start) => ResponseTemplate::new(206)
                .insert_header(
                    "content-range",
                    format!("bytes {start}-{}/{}", self.0.len() - 1, self.0.len()).as_str(),
                )
                .set_body_bytes(self.0[start..].to_vec()),
            None => ResponseTemplate::new(200).set_body_bytes(self.0.clone()),
        }
    }
}

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
    let manifest = json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
        "config": {"mediaType": "application/vnd.docker.container.image.v1+json",
                   "digest": format!("sha256:{}", sha256(&ollama_config)), "size": ollama_config.len()},
        "layers": [
            {"mediaType": "application/vnd.ollama.image.model",
             "digest": format!("sha256:{}", sha256(&weights)), "size": weights.len()},
            {"mediaType": "application/vnd.ollama.image.template",
             "digest": format!("sha256:{}", sha256(&template)), "size": template.len()}
        ]
    })
    .to_string();

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

fn engine(home: &Home, endpoint: &str, apps: &Path, archive: Option<Home>) -> Arc<Engine> {
    Arc::new(Engine {
        db: Mutex::new(Db::open(&home.db_path()).unwrap()),
        store: Store::open(home).unwrap().with_archive(archive),
        hub: HubClient::new(endpoint, None).unwrap(),
        fetcher: Fetcher::new(endpoint, None).unwrap(),
        dirs: AppDirs::under(apps),
    })
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
    assert_eq!(weight_fetches, 1);

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
            None,
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
        .fetch(&store, &url, None, &Expect::Sha256(wrong.clone()), None)
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
