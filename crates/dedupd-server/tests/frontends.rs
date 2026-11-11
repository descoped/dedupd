//! The request sequences `huggingface_hub` and Ollama send, replayed against
//! the front ends with a fake Hugging Face behind them.
#![cfg(target_os = "macos")]

#[path = "../../dedupd-core/tests/common/mod.rs"]
mod common;

use common::{COMMIT, REPO, Ranged, git_oid, sha256};
use dedupd_core::Home;
use dedupd_core::config::Config;
use dedupd_server::Server;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const OLLAMA_UA: &str = "ollama/0.40.16 (arm64 darwin) Go/go1.24.0";

struct Fixture {
    _dir: tempfile::TempDir,
    upstream: MockServer,
    server: Arc<Server>,
    base: String,
    http: reqwest::Client,
    weights: Vec<u8>,
    config: Vec<u8>,
    template: Vec<u8>,
    manifest: String,
}

async fn fixture() -> Fixture {
    let weights: Vec<u8> = (0..300_000u32).map(|i| (i % 241) as u8).collect();
    let config = b"{\"a\":1}\n".to_vec();
    let ollama_config = b"{\"model_format\":\"gguf\"}".to_vec();
    let template: Vec<u8> = (0..5000u32).map(|i| (i % 97) as u8).collect();
    let manifest = common::ollama_manifest(&ollama_config, &weights, &template);

    let upstream = MockServer::start().await;
    let info = json!({"sha": COMMIT, "id": REPO, "gated": false, "private": false});
    for p in [
        format!("/api/models/{REPO}/revision/main"),
        format!("/api/models/{REPO}"),
    ] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_json(info.clone()))
            .mount(&upstream)
            .await;
    }
    let tree = json!([
        {"type": "directory", "path": "sub", "oid": "d", "size": 0},
        {"type": "file", "path": "model.gguf", "size": weights.len(), "oid": "pointer",
         "lfs": {"oid": sha256(&weights), "size": weights.len(), "pointerSize": 134},
         "xetHash": "feedface"},
        {"type": "file", "path": "sub/config.json", "size": config.len(), "oid": git_oid(&config)}
    ]);
    // The first page of a listing names a next page; the cursor page is empty.
    Mock::given(method("GET"))
        .and(path_regex(format!(
            "^/api/models/{REPO}/tree/(main|{COMMIT})$"
        )))
        .and(wiremock::matchers::query_param("cursor", "zzz"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&upstream)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(format!(
            "^/api/models/{REPO}/tree/(main|{COMMIT})$"
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(
                    "link",
                    format!(
                        r#"<{}/api/models/{REPO}/tree/{COMMIT}?cursor=zzz>; rel="next", <https://cas.example/t>; rel="xet-auth""#,
                        upstream.uri()
                    )
                    .as_str(),
                )
                .insert_header("x-xet-hash", "feedface")
                .set_body_json(tree),
        )
        .mount(&upstream)
        .await;
    for (file, body) in [("model.gguf", &weights), ("sub/config.json", &config)] {
        Mock::given(method("GET"))
            .and(path(format!("/{REPO}/resolve/{COMMIT}/{file}")))
            .respond_with(Ranged(body.clone()))
            .mount(&upstream)
            .await;
    }
    Mock::given(method("GET"))
        .and(path(format!("/v2/{REPO}/manifests/latest")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(
                    "content-type",
                    "application/vnd.docker.distribution.manifest.v2+json",
                )
                .set_body_string(manifest.clone()),
        )
        .mount(&upstream)
        .await;
    for body in [&ollama_config, &template, &weights] {
        Mock::given(method("GET"))
            .and(path(format!("/v2/{REPO}/blobs/sha256:{}", sha256(body))))
            .respond_with(Ranged(body.clone()))
            .mount(&upstream)
            .await;
    }

    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    let engine = common::engine(&home, &upstream.uri(), &dir.path().join("apps"), None);
    let server = Server::new(engine, Config::default(), home).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(dedupd_server::serve(
        server.clone(),
        listener,
        std::future::pending(),
    ));
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    Fixture {
        _dir: dir,
        upstream,
        server,
        base,
        http,
        weights,
        config,
        template,
        manifest,
    }
}

async fn wait_for_store(fx: &Fixture, sha: &str) {
    for _ in 0..200 {
        if fx.server.engine.store.contains(sha) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{sha} never reached the store");
}

async fn upstream_hits(fx: &Fixture, suffix: &str) -> usize {
    fx.upstream
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().ends_with(suffix))
        .count()
}

/// Take Hugging Face away: every request now gets the Hub's "not found".
async fn remove_upstream(fx: &Fixture) {
    fx.upstream.reset().await;
    Mock::given(path_regex(".*"))
        .respond_with(
            ResponseTemplate::new(404)
                .insert_header("x-error-code", "RepoNotFound")
                .insert_header("x-error-message", "Repository not found"),
        )
        .mount(&fx.upstream)
        .await;
    fx.server.forget_refs();
}

#[tokio::test]
async fn hub_client_download_sequence_then_offline() {
    let fx = fixture().await;
    let weights_sha = sha256(&fx.weights);
    let url = |p: &str| format!("{}{p}", fx.base);

    // The listing loses its Xet hashes, and pagination points back at us.
    let resp = fx
        .http
        .get(url(&format!("/api/models/{REPO}/tree/main?recursive=true")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.headers().get("x-xet-hash").is_none());
    assert_eq!(
        resp.headers().get("link").unwrap().to_str().unwrap(),
        format!(
            r#"<{}/api/models/{REPO}/tree/{COMMIT}?cursor=zzz>; rel="next""#,
            fx.base
        )
    );
    let tree: Value = resp.json().await.unwrap();
    assert_eq!(tree.as_array().unwrap().len(), 3);
    assert!(!tree.to_string().contains("xetHash"));
    assert_eq!(tree[1]["lfs"]["oid"], weights_sha);

    // HEAD is answered here, with the names the client's cache needs.
    let head = fx
        .http
        .head(url(&format!("/{REPO}/resolve/main/model.gguf")))
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), 200);
    assert!(head.headers().get("location").is_none());
    assert_eq!(head.headers()["x-repo-commit"], COMMIT);
    assert_eq!(
        head.headers()["etag"],
        format!("\"{weights_sha}\"").as_str()
    );
    assert_eq!(
        head.headers()["content-length"],
        fx.weights.len().to_string().as_str()
    );

    // A small file is fetched, checked and served; its etag is the git id.
    let small = fx
        .http
        .get(url(&format!("/{REPO}/resolve/{COMMIT}/sub/config.json")))
        .send()
        .await
        .unwrap();
    assert_eq!(
        small.headers()["etag"],
        format!("\"{}\"", git_oid(&fx.config)).as_str()
    );
    assert_eq!(small.bytes().await.unwrap(), fx.config);

    // The weights stream through, and the store keeps them.
    let big = fx
        .http
        .get(url(&format!("/{REPO}/resolve/{COMMIT}/model.gguf")))
        .send()
        .await
        .unwrap();
    assert_eq!(big.status(), 200);
    assert_eq!(big.bytes().await.unwrap(), fx.weights);
    wait_for_store(&fx, &weights_sha).await;
    assert_eq!(upstream_hits(&fx, "/model.gguf").await, 1);

    // From now on the store answers, ranges included.
    let part = fx
        .http
        .get(url(&format!("/{REPO}/resolve/main/model.gguf")))
        .header("range", "bytes=10-19")
        .send()
        .await
        .unwrap();
    assert_eq!(part.status(), 206);
    assert_eq!(
        part.headers()["content-range"],
        format!("bytes 10-19/{}", fx.weights.len()).as_str()
    );
    assert_eq!(part.bytes().await.unwrap(), fx.weights[10..20]);
    assert_eq!(upstream_hits(&fx, "/model.gguf").await, 1);

    // The repository is removed upstream. Everything a client needs for the
    // archived model still works; anything else gets the Hub's own error.
    remove_upstream(&fx).await;
    let info: Value = fx
        .http
        .get(url(&format!("/api/models/{REPO}/revision/main")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info["sha"], COMMIT);
    let tree: Value = fx
        .http
        .get(url(&format!("/api/models/{REPO}/tree/main?recursive=true")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(tree.as_array().unwrap().len(), 3);
    assert!(!tree.to_string().contains("xetHash"));
    let top: Value = fx
        .http
        .get(url(&format!("/api/models/{REPO}/tree/main")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(top.as_array().unwrap().len(), 2);
    let paths: Value = fx
        .http
        .post(url(&format!("/api/models/{REPO}/paths-info/{COMMIT}")))
        .header("content-type", "application/x-www-form-urlencoded")
        .body("paths=sub%2Fconfig.json&expand=false")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(paths.as_array().unwrap().len(), 1);
    assert_eq!(paths[0]["path"], "sub/config.json");

    let head = fx
        .http
        .head(url(&format!("/{REPO}/resolve/main/model.gguf")))
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), 200);
    assert_eq!(head.headers()["x-repo-commit"], COMMIT);
    let again = fx
        .http
        .get(url(&format!("/{REPO}/resolve/main/model.gguf")))
        .send()
        .await
        .unwrap();
    assert_eq!(again.bytes().await.unwrap(), fx.weights);
    let missing = fx
        .http
        .head(url(&format!("/{REPO}/resolve/main/nope.bin")))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
    assert_eq!(missing.headers()["x-error-code"], "EntryNotFound");

    // A repository that was never archived: the Hub's answer, verbatim.
    let other = fx
        .http
        .head(url("/org/never-seen/resolve/main/model.gguf"))
        .send()
        .await
        .unwrap();
    assert_eq!(other.status(), 404);
    assert_eq!(other.headers()["x-error-code"], "RepoNotFound");
    assert_eq!(other.headers()["x-error-message"], "Repository not found");
}

#[tokio::test]
async fn ollama_pull_sequence_then_offline() {
    let fx = fixture().await;
    let url = |p: &str| format!("{}{p}", fx.base);
    let template_sha = sha256(&fx.template);

    assert_eq!(fx.http.get(url("/v2/")).send().await.unwrap().status(), 200);

    let m = fx
        .http
        .get(url(&format!("/v2/{REPO}/manifests/latest")))
        .header("user-agent", OLLAMA_UA)
        .send()
        .await
        .unwrap();
    assert_eq!(m.status(), 200);
    assert_eq!(
        m.headers()["docker-content-digest"],
        format!("sha256:{}", sha256(fx.manifest.as_bytes())).as_str()
    );
    assert_eq!(m.text().await.unwrap(), fx.manifest);

    // Without Range, Ollama must be redirected to a URL on the same host.
    let blob = format!("/v2/{REPO}/blobs/sha256:{template_sha}");
    let first = fx.http.get(url(&blob)).send().await.unwrap();
    assert_eq!(first.status(), 307);
    let location = first.headers()["location"].to_str().unwrap().to_string();
    // Ollama only treats a redirect to another hostname as the download
    // URL, so the target is this server under its other loopback name.
    assert_eq!(
        location,
        format!(
            "{}/dedupd/blob/sha256:{template_sha}?repo={REPO}",
            fx.base.replace("127.0.0.1", "localhost")
        )
    );
    let location = location.replace("localhost", "127.0.0.1");

    // It then downloads parts in parallel. Once every byte has passed
    // through, the layer is in the store.
    let size = fx.template.len();
    let (a, b) = tokio::join!(
        fx.http
            .get(&location)
            .header("range", format!("bytes=2000-{}", size - 1))
            .send(),
        fx.http
            .get(&location)
            .header("range", "bytes=0-1999")
            .send(),
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!((a.status().as_u16(), b.status().as_u16()), (206, 206));
    assert_eq!(a.bytes().await.unwrap(), fx.template[2000..]);
    assert_eq!(b.bytes().await.unwrap(), fx.template[..2000]);
    wait_for_store(&fx, &template_sha).await;

    // Hugging Face goes away: the manifest and the layer are still served.
    remove_upstream(&fx).await;
    let m = fx
        .http
        .get(url(&format!("/v2/{REPO}/manifests/latest")))
        .header("user-agent", OLLAMA_UA)
        .send()
        .await
        .unwrap();
    assert_eq!(m.text().await.unwrap(), fx.manifest);
    let head = fx.http.head(url(&blob)).send().await.unwrap();
    assert_eq!(head.status(), 200);
    assert_eq!(head.headers()["content-length"], size.to_string().as_str());
    let whole = fx.http.get(&location).send().await.unwrap();
    assert_eq!(whole.status(), 200);
    assert_eq!(whole.bytes().await.unwrap(), fx.template);
    let unknown = fx
        .http
        .get(url("/v2/org/never-seen/manifests/latest"))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), 404);
}

#[tokio::test]
async fn control_api_is_closed_to_web_pages() {
    let fx = fixture().await;
    let status = format!("{}/dedupd/v1/status", fx.base);
    let ok: Value = fx
        .http
        .get(&status)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ok["status"]["store_files"], 0);
    // The client the command line and the app use speaks the same API.
    let address = fx.base.trim_start_matches("http://").to_string();
    let (a, b) = (address.clone(), address.clone());
    let seen = tokio::task::spawn_blocking(move || dedupd_client::status(a))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(seen.store_files, 0);
    assert_eq!(seen.version, env!("CARGO_PKG_VERSION"));
    let pulled = tokio::task::spawn_blocking(move || {
        dedupd_client::pull(b, REPO.into(), vec!["bionic".into()], vec![], false)
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!((pulled.files, pulled.downloaded), (2, 2));
    assert_eq!(pulled.placed[0].app, "bionic");
    assert_eq!(pulled.placed[0].cloned, 2);
    let nobody = tokio::task::spawn_blocking(|| dedupd_client::status("127.0.0.1:1".into()))
        .await
        .unwrap();
    assert!(matches!(
        nobody,
        Err(dedupd_client::ClientError::NotRunning { .. })
    ));

    let blocked = fx
        .http
        .get(&status)
        .header("origin", "https://evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), 403);
}

#[tokio::test]
async fn a_model_an_app_downloaded_itself_is_shared_by_the_next_pass() {
    use dedupd_server::daemon;
    let fx = fixture().await;
    let dirs = fx.server.engine.dirs.clone();
    let zero = Duration::ZERO;

    // Bionic downloads the repository on its own, past the daemon.
    let bionic = dirs.bionic.join(REPO);
    std::fs::create_dir_all(bionic.join("sub")).unwrap();
    std::fs::write(bionic.join("model.gguf"), &fx.weights).unwrap();
    std::fs::write(bionic.join("sub/config.json"), &fx.config).unwrap();

    // Not yet settled: a download may still be writing.
    let early = daemon::pass_with(&fx.server, Duration::from_secs(3600), zero)
        .await
        .unwrap();
    assert_eq!(early.adopted, 0);

    let first = daemon::pass_with(&fx.server, zero, zero).await.unwrap();
    assert_eq!((first.adopted, first.errors.len()), (2, 0));
    assert!(fx.server.engine.store.contains(&sha256(&fx.weights)));

    // The Hugging Face client can now get the model from the daemon without
    // any download from upstream.
    let got = fx
        .http
        .get(format!("{}/{REPO}/resolve/main/model.gguf", fx.base))
        .send()
        .await
        .unwrap();
    assert_eq!(got.bytes().await.unwrap(), fx.weights);
    assert_eq!(upstream_hits(&fx, "/model.gguf").await, 0);

    // Ollama then pulls the same model itself and writes a second copy of
    // the weights. The next pass replaces that copy with a clone.
    let blob = dirs
        .ollama
        .join(format!("blobs/sha256-{}", sha256(&fx.weights)));
    std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
    std::fs::write(&blob, &fx.weights).unwrap();
    let manifest = dirs
        .ollama
        .join("manifests/hf.co")
        .join(REPO)
        .join("latest");
    std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    std::fs::write(&manifest, &fx.manifest).unwrap();
    let ino = std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&blob).unwrap());

    let second = daemon::pass_with(&fx.server, zero, zero).await.unwrap();
    assert_eq!((second.adopted, second.reclaimed), (1, 1));
    assert_eq!(second.reclaimed_bytes, fx.weights.len() as u64);
    assert_ne!(
        std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&blob).unwrap()),
        ino
    );
    assert_eq!(std::fs::read(&blob).unwrap(), fx.weights);

    // The manifest Ollama wrote is archived, and an idle pass does nothing.
    let quiet = daemon::pass_with(&fx.server, zero, zero).await.unwrap();
    assert_eq!((quiet.adopted, quiet.reclaimed, quiet.forgotten), (0, 0, 0));
    remove_upstream(&fx).await;
    let m = fx
        .http
        .get(format!("{}/v2/{REPO}/manifests/latest", fx.base))
        .send()
        .await
        .unwrap();
    assert_eq!(m.text().await.unwrap(), fx.manifest);

    // The model is deleted in Bionic: forgotten there, kept in the store.
    std::fs::remove_dir_all(&bionic).unwrap();
    let after = daemon::pass_with(&fx.server, zero, zero).await.unwrap();
    assert_eq!(after.forgotten, 2);
    assert!(fx.server.engine.store.contains(&sha256(&fx.weights)));
}

/// The control API's answers, as the macOS app's tests decode them through
/// the client. Paths, the upstream address and the version differ from run to
/// run and are replaced, so the files change only when the answers do.
///
/// Re-record with `DEDUPD_UPDATE_FIXTURES=1 cargo test -p dedupd-server --test
/// frontends control_api_answers_match_the_recorded_ones` (`just fixtures`).
#[tokio::test]
async fn control_api_answers_match_the_recorded_ones() {
    let fx = fixture().await;
    let root = fx._dir.path().to_string_lossy().into_owned();
    let real = std::fs::canonicalize(fx._dir.path())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let upstream = fx.upstream.uri();
    let normalise = |body: String| -> String {
        let body = body
            .replace(&real, "/Users/example")
            .replace(&root, "/Users/example")
            .replace(&upstream, "https://huggingface.co");
        let mut value: Value = serde_json::from_str(&body).unwrap();
        if value.get("version").is_some() {
            value["version"] = json!("0.0.0");
        }
        serde_json::to_string_pretty(&value).unwrap() + "\n"
    };
    let post = |path: &'static str, body: Value| {
        let (http, base) = (fx.http.clone(), fx.base.clone());
        async move {
            http.post(format!("{base}/dedupd/v1/{path}"))
                .json(&body)
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap()
        }
    };

    let pull = post("pull", json!({"model": REPO, "apps": ["bionic"]})).await;
    let scan = post("scan", json!({})).await;
    let error = post("pull", json!({"model": "not a model", "apps": []})).await;
    let status = fx
        .http
        .get(format!("{}/dedupd/v1/status", fx.base))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/control");
    let update = std::env::var_os("DEDUPD_UPDATE_FIXTURES").is_some();
    for (name, body) in [
        ("status.json", status),
        ("pull.json", pull),
        ("scan.json", scan),
        ("error.json", error),
    ] {
        let now = normalise(body);
        let file = dir.join(name);
        if update {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&file, &now).unwrap();
            continue;
        }
        let recorded = std::fs::read_to_string(&file)
            .unwrap_or_else(|_| panic!("{} is missing; record it", file.display()));
        assert_eq!(
            recorded, now,
            "{name} changed; re-record with DEDUPD_UPDATE_FIXTURES=1 and check the app still reads it"
        );
    }
}
