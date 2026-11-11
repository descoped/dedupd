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
    let info = json!({
        "sha": COMMIT, "id": REPO, "gated": false, "private": false,
        "pipeline_tag": "text-generation",
        "tags": ["gguf", "text-generation", "license:apache-2.0", "base_model:org/Base"],
        "downloads": 1200, "likes": 7,
        "lastModified": "2026-01-02T03:04:05.000Z",
        "gguf": {"total": 134515008u64, "architecture": "llama", "context_length": 8192},
        "siblings": [
            {"rfilename": "model.gguf", "blobId": "pointer", "size": weights.len(),
             "lfs": {"sha256": sha256(&weights), "size": weights.len(), "pointerSize": 134}},
            {"rfilename": "sub/config.json", "blobId": git_oid(&config), "size": config.len()}
        ]
    });
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

/// Ask the control API and read the answer as JSON.
async fn control(fx: &Fixture, verb: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let url = format!("{}/dedupd/v1/{path}", fx.base);
    let req = match verb {
        "GET" => fx.http.get(url),
        "PUT" => fx.http.put(url),
        _ => fx.http.post(url),
    };
    let req = match body {
        Some(body) => req.json(&body),
        None => req,
    };
    let resp = req.send().await.unwrap();
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap();
    let value = serde_json::from_str(&text).unwrap_or(Value::String(text));
    (status, value)
}

/// Queue a download through the control API and wait until it has ended.
async fn download(fx: &Fixture, body: Value) -> Value {
    let (status, queued) = control(fx, "POST", "downloads", Some(body)).await;
    assert_eq!(status, 200, "{queued}");
    let id = queued["id"].as_i64().unwrap();
    for _ in 0..500 {
        let (_, job) = control(fx, "GET", &format!("downloads/{id}"), None).await;
        if matches!(job["state"].as_str(), Some("done" | "needs_attention")) {
            return job;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("download {id} never ended");
}

/// The provider's search and the model the fixture serves, as search results.
async fn mount_search(fx: &Fixture) {
    let hit = |id: &str, tags: Value| {
        json!({"id": id, "author": id.split('/').next().unwrap(), "downloads": 1200, "likes": 7,
               "gated": false, "tags": tags, "pipeline_tag": "text-generation",
               "lastModified": "2026-01-02T03:04:05.000Z", "createdAt": "2025-12-01T00:00:00.000Z"})
    };
    Mock::given(method("GET"))
        .and(path("/api/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(
                    "link",
                    format!(
                        r#"<{}/api/models?search=repo&limit=40&cursor=abc%3D>; rel="next""#,
                        fx.upstream.uri()
                    )
                    .as_str(),
                )
                .insert_header("ratelimit", r#""api";r=498;t=120"#)
                .insert_header("ratelimit-policy", r#""fixed window";"api";q=500;w=300"#)
                .set_body_json(json!([
                    hit(
                        REPO,
                        json!(["gguf", "license:apache-2.0", "base_model:org/Base"])
                    ),
                    hit(
                        "mlx-community/Repo-4bit",
                        json!([
                            "mlx",
                            "safetensors",
                            "license:apache-2.0",
                            "base_model:org/Base"
                        ])
                    ),
                ])),
        )
        .mount(&fx.upstream)
        .await;
}

#[tokio::test]
async fn the_app_downloads_looks_removes_and_cleans_up() {
    let fx = fixture().await;
    let job = download(&fx, json!({"model": REPO, "apps": ["bionic", "hf"]})).await;
    assert_eq!(job["state"], "done", "{job}");
    assert_eq!(job["files"].as_array().unwrap().len(), 2);
    assert_eq!(job["bytes_done"], job["bytes_total"]);
    // Asking for the same thing again starts nothing new once it is done,
    // and names the running download while it is not.
    let (_, list) = control(&fx, "GET", "downloads", None).await;
    assert_eq!(list["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(list["jobs"][0]["files_done"], 2);
    assert!(list["jobs"][0].get("files").is_none());
    assert_eq!(list["governor"]["max_connections"], 8);

    let (_, models) = control(&fx, "GET", "models", None).await;
    let model = &models["models"][0];
    assert_eq!(model["repo"], REPO);
    assert_eq!(model["meta"]["formats"], json!(["gguf"]));
    assert_eq!(model["meta"]["license"], "apache-2.0");
    assert_eq!(model["upstream"], "ok");
    assert_eq!(model["apps"].as_array().unwrap().len(), 2);

    let (status, detail) = control(&fx, "GET", &format!("models/{REPO}"), None).await;
    assert_eq!(status, 200);
    assert_eq!(detail["variants"][0]["spec"], format!("{REPO}:model"));
    assert_eq!(detail["variants"][0]["placed_in"], json!(["bionic", "hf"]));
    assert_eq!(
        detail["variants"][0]["usable_by"],
        json!(["bionic", "ollama", "hf"])
    );
    assert_eq!(
        detail["revisions"][0]["files"][0]["apps"],
        json!(["bionic", "hf"])
    );
    assert_eq!(detail["placements"].as_array().unwrap().len(), 4);
    let (status, _) = control(&fx, "GET", "models/org/Nothing", None).await;
    assert_eq!(status, 404);

    let (_, places) = control(&fx, "GET", "locations", None).await;
    assert_eq!(places["store"]["files"], 2);
    assert_eq!(places["apps"][0]["id"], "bionic");
    assert_eq!(places["apps"][0]["models"], 1);
    assert_eq!(places["apps"][1]["models"], 0);

    // Removing from an app: first what would go, then exactly that.
    let ask = json!({"repo": REPO, "app": "bionic"});
    let (_, plan) = control(&fx, "POST", "unplace", Some(ask.clone())).await;
    assert_eq!(plan["remove"].as_array().unwrap().len(), 2);
    assert_eq!(plan["removed"], Value::Null);
    let bionic = fx.server.engine.dirs.bionic.join(REPO);
    assert!(bionic.join("model.gguf").is_file());
    let agreed = json!({"repo": REPO, "app": "bionic", "apply": true,
                        "paths": [plan["remove"][0]["path"]]});
    let (_, done) = control(&fx, "POST", "unplace", Some(agreed)).await;
    assert_eq!(done["removed"], 1);
    assert!(!bionic.join("model.gguf").exists());
    assert!(bionic.join("sub/config.json").is_file());

    // A model that apps still hold cannot be forgotten.
    let (status, refused) = control(&fx, "POST", "forget", Some(json!({"repo": REPO}))).await;
    assert_eq!(status, 409, "{refused}");
    for app in ["bionic", "hf"] {
        let all = json!({"repo": REPO, "app": app, "apply": true});
        control(&fx, "POST", "unplace", Some(all)).await;
    }
    let (_, nothing) = control(&fx, "GET", "gc", None).await;
    assert_eq!(nothing["orphans"], json!([]));
    let (status, _) = control(&fx, "POST", "forget", Some(json!({"repo": REPO}))).await;
    assert_eq!(status, 200);
    let (_, plan) = control(&fx, "GET", "gc", None).await;
    let orphans: Vec<Value> = plan["orphans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["sha256"].clone())
        .collect();
    assert_eq!(orphans.len(), 2);
    // Only what was agreed to is removed.
    let (_, freed) = control(&fx, "POST", "gc", Some(json!({"orphans": [orphans[0]]}))).await;
    assert_eq!(freed["orphans"].as_array().unwrap().len(), 1);
    let (_, left) = control(&fx, "GET", "gc", None).await;
    assert_eq!(left["orphans"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn the_event_stream_follows_a_download() {
    use futures_util::StreamExt;
    let fx = fixture().await;
    let resp = fx
        .http
        .get(format!("{}/dedupd/v1/events", fx.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.headers()["content-type"], "application/x-ndjson");
    let mut bytes = resp.bytes_stream();
    let mut pending = String::new();
    let mut lines: Vec<Value> = Vec::new();
    let mut started = false;
    let seen_done = |lines: &[Value]| {
        lines
            .iter()
            .any(|l| l["downloads"]["jobs"][0]["state"] == "done")
    };
    while !seen_done(&lines) {
        let chunk = tokio::time::timeout(Duration::from_secs(10), bytes.next())
            .await
            .expect("the stream went quiet")
            .expect("the stream ended")
            .unwrap();
        pending.push_str(std::str::from_utf8(&chunk).unwrap());
        while let Some(end) = pending.find('\n') {
            lines.push(serde_json::from_str(&pending[..end]).unwrap());
            pending.drain(..=end);
        }
        // The first line arrives at once and says nothing is going on.
        if !started {
            assert_eq!(lines[0]["seq"], 1);
            assert_eq!(lines[0]["downloads"]["jobs"], json!([]));
            control(
                &fx,
                "POST",
                "downloads",
                Some(json!({"model": REPO, "apps": []})),
            )
            .await;
            started = true;
        }
    }
    let seqs: Vec<u64> = lines.iter().map(|l| l["seq"].as_u64().unwrap()).collect();
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");
    // The stream ends when the daemon stops, so stopping does not hang.
    fx.server.shutdown.cancel();
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(Ok(_)) = bytes.next().await {}
    })
    .await;
    assert!(ended.is_ok(), "the stream outlived the daemon");
}

#[tokio::test]
async fn search_and_model_info_say_what_is_already_here() {
    let fx = fixture().await;
    mount_search(&fx).await;
    let find = "hub/search?q=repo&format=gguf&license=apache-2.0&sort=likes";
    let (status, found) = control(&fx, "GET", find, None).await;
    assert_eq!(status, 200, "{found}");
    assert_eq!(found["next"], "abc=");
    assert_eq!(found["models"][0]["id"], REPO);
    assert_eq!(found["models"][0]["archived"], false);
    assert_eq!(
        found["models"][1]["meta"]["formats"],
        json!(["mlx", "safetensors"])
    );
    let asked = fx.upstream.received_requests().await.unwrap();
    let query = asked.last().unwrap().url.query().unwrap().to_string();
    for part in [
        "search=repo",
        "filter=gguf",
        "filter=license%3Aapache-2.0",
        "sort=likes",
        "expand[]=downloads",
    ] {
        assert!(query.contains(part), "{part} is not in {query}");
    }
    // The provider's limits are followed for searches too.
    let (_, list) = control(&fx, "GET", "downloads", None).await;
    assert_eq!(list["governor"]["budgets"][0]["remaining"], 498);

    let (status, info) = control(&fx, "GET", &format!("hub/models/{REPO}"), None).await;
    assert_eq!(status, 200, "{info}");
    assert_eq!(info["commit"], COMMIT);
    assert_eq!(info["meta"]["context_length"], 8192);
    assert_eq!(info["files"].as_array().unwrap().len(), 2);
    assert_eq!(info["variants"][0]["stored_bytes"], 0);
    assert_eq!(info["related"][0]["id"], "mlx-community/Repo-4bit");
    assert_eq!(info["archived"], false);

    // Once it is downloaded, both say so; the search answer is reused.
    download(&fx, json!({"model": REPO, "apps": ["ollama"]})).await;
    let before = fx.upstream.received_requests().await.unwrap().len();
    let (_, found) = control(&fx, "GET", find, None).await;
    assert_eq!(fx.upstream.received_requests().await.unwrap().len(), before);
    assert_eq!(found["models"][0]["archived"], true);
    assert_eq!(found["models"][0]["apps"], json!(["ollama"]));
    let (_, info) = control(&fx, "GET", &format!("hub/models/{REPO}"), None).await;
    assert_eq!(info["variants"][0]["placed_in"], json!(["ollama"]));
    assert_eq!(info["files"][0]["stored"], true);

    // With the provider gone, what is archived still answers.
    remove_upstream(&fx).await;
    let (status, info) = control(&fx, "GET", &format!("hub/models/{REPO}"), None).await;
    assert_eq!(status, 200, "{info}");
    assert_eq!(info["from_archive"], true);
    assert_eq!(info["files"].as_array().unwrap().len(), 2);
    let (status, _) = control(&fx, "GET", "hub/models/org/Unknown", None).await;
    assert_eq!(status, 404);
    let (_, checked) = control(&fx, "POST", "check", Some(json!({"repo": REPO}))).await;
    assert_eq!(checked["upstream"], "gone");
    let (_, models) = control(&fx, "GET", "models", None).await;
    assert_eq!(models["models"][0]["upstream"], "gone");
}

#[tokio::test]
async fn settings_are_changed_through_the_daemon() {
    let fx = fixture().await;
    let (_, before) = control(&fx, "GET", "config", None).await;
    assert_eq!(before["max_connections"], 8);
    assert_eq!(before["restart_needed"], false);
    // The download limits apply at once.
    let change = json!({"max_connections": 3, "speed_limit": 1_000_000});
    let (status, after) = control(&fx, "PUT", "config", Some(change)).await;
    assert_eq!(status, 200, "{after}");
    assert_eq!(after["restart_needed"], false);
    let (_, state) = control(&fx, "GET", "status", None).await;
    assert_eq!(state["daemon"]["max_connections"], 3);
    assert_eq!(state["daemon"]["speed_limit"], 1_000_000);
    // Other settings wait for the next start.
    let (_, after) = control(&fx, "PUT", "config", Some(json!({"verify_days": 30}))).await;
    assert_eq!(
        (&after["verify_days"], &after["restart_needed"]),
        (&json!(30), &json!(true))
    );
    let (status, refused) = control(&fx, "PUT", "config", Some(json!({"nonsense": 1}))).await;
    assert_eq!(status, 400, "{refused}");

    // A check of the store runs in the background and leaves its result.
    control(&fx, "POST", "verify", None).await;
    for _ in 0..200 {
        let (_, check) = control(&fx, "GET", "verify", None).await;
        if check["running"] == false && check["finished_at"].is_number() {
            assert_eq!(check["damaged"], json!([]));
            let (_, log) = control(&fx, "GET", "log?lines=5", None).await;
            assert_eq!(log["lines"], json!([]));
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the store check never finished");
}

/// Everything the app does, through the client it links: each call reads
/// what the daemon really sends.
#[tokio::test(flavor = "multi_thread")]
async fn the_client_reads_everything_the_daemon_sends() {
    use dedupd_client as client;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Seen {
        events: Mutex<Vec<client::DaemonEvent>>,
        connections: Mutex<Vec<bool>>,
    }
    impl client::EventListener for Seen {
        fn on_event(&self, event: client::DaemonEvent) {
            self.events.lock().unwrap().push(event);
        }
        fn on_connection(&self, connected: bool) {
            self.connections.lock().unwrap().push(connected);
        }
    }

    let fx = fixture().await;
    mount_search(&fx).await;
    let address = fx.base.trim_start_matches("http://").to_string();
    let seen = Arc::new(Seen::default());
    let subscription = client::subscribe(address.clone(), seen.clone());

    let a = address.clone();
    let done = tokio::task::spawn_blocking(move || -> Result<(), client::ClientError> {
        let query = client::SearchQuery {
            text: "repo".into(),
            format: "gguf".into(),
            sort: "likes".into(),
            ..Default::default()
        };
        let page = client::search(a.clone(), query)?;
        assert_eq!(page.next.as_deref(), Some("abc="));
        assert_eq!(page.models[1].meta.formats, ["mlx", "safetensors"]);
        let hub = client::hub_model(a.clone(), REPO.into())?;
        assert_eq!(hub.variants[0].usable_by, ["bionic", "ollama", "hf"]);
        assert_eq!(hub.related.len(), 1);

        let spec = hub.variants[0].spec.clone();
        let queued = client::enqueue(a.clone(), spec, vec!["bionic".into()], vec![], false)?;
        assert!(!queued.existing);
        let job = loop {
            let job = client::download(a.clone(), queued.id)?;
            if job.job.state == "done" {
                break job;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!((job.job.files_total, job.job.files_done), (2, 2));
        assert_eq!(job.files[0].stage, "stored");
        assert!(job.events.iter().any(|e| e.text == "done"));
        assert_eq!(client::downloads(a.clone())?.jobs.len(), 1);

        let library = client::models(a.clone())?;
        assert_eq!(library[0].apps[0].app, "bionic");
        let detail = client::model(a.clone(), REPO.into())?;
        assert_eq!(detail.placements.len(), 2);
        assert_eq!(detail.revisions[0].files.len(), 2);
        let places = client::locations(a.clone())?;
        assert_eq!(places.store.files, 2);
        assert!(places.volume_free > 0);
        assert_eq!(
            client::check_upstream(a.clone(), REPO.into())?.upstream,
            "ok"
        );
        let status = client::status(a.clone())?;
        assert_eq!(status.daemon.pid, std::process::id());
        assert!(!status.daemon.token);

        let plan = client::unplace(a.clone(), REPO.into(), "bionic".into(), false, None)?;
        assert_eq!((plan.remove.len(), plan.removed), (2, None));
        let gone = client::unplace(a.clone(), REPO.into(), "bionic".into(), true, None)?;
        assert_eq!(gone.removed, Some(2));
        client::forget(a.clone(), REPO.into())?;
        let plan = client::gc_plan(a.clone(), false)?;
        assert_eq!(plan.orphans.len(), 2);
        let shas = plan.orphans.iter().map(|b| b.sha256.clone()).collect();
        let freed = client::gc_apply(a.clone(), shas, vec![])?;
        assert_eq!(freed.freed, Some(300_008));

        let change = client::SettingChange {
            key: "speed_limit".into(),
            value: client::SettingValue::Number { value: 5_000_000 },
        };
        let settings = client::update_settings(a.clone(), vec![change])?;
        assert_eq!(settings.speed_limit, 5_000_000);
        assert!(!client::settings(a.clone())?.restart_needed);
        client::start_store_check(a.clone())?;
        while client::store_check(a.clone())?.finished_at.is_none() {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(client::log_tail(a.clone(), 10)?.lines.is_empty());
        let refused = client::download_action(a, 99, "pause".into());
        assert!(matches!(refused, Err(client::ClientError::Failed { .. })));
        Ok(())
    })
    .await
    .unwrap();
    done.unwrap();

    // The subscription was told how the download ended and that what the
    // library holds has changed. Quick changes arrive as one event.
    let finished = |e: &client::DaemonEvent| {
        e.library > 0 && e.downloads.jobs.first().is_some_and(|j| j.state == "done")
    };
    for _ in 0..300 {
        if seen.events.lock().unwrap().last().is_some_and(finished) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    {
        let events = seen.events.lock().unwrap();
        assert_eq!(events[0].seq, 1);
        assert!(events.last().is_some_and(finished), "{events:?}");
        assert_eq!(*seen.connections.lock().unwrap(), [true]);
    }
    let a = address.clone();
    let cleared = tokio::task::spawn_blocking(move || client::clear_downloads(a))
        .await
        .unwrap();
    assert_eq!(cleared.unwrap(), 1);
    for _ in 0..300 {
        let emptied = |e: &client::DaemonEvent| e.downloads.jobs.is_empty() && e.seq > 1;
        if seen.events.lock().unwrap().last().is_some_and(emptied) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        seen.events
            .lock()
            .unwrap()
            .last()
            .is_some_and(|e| e.downloads.jobs.is_empty())
    );
    // When the daemon goes away the listener is told, once.
    fx.server.shutdown.cancel();
    for _ in 0..300 {
        if seen.connections.lock().unwrap().len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(*seen.connections.lock().unwrap(), [true, false]);
    subscription.cancel();
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
    mount_search(&fx).await;
    let root = fx._dir.path().to_string_lossy().into_owned();
    let real = std::fs::canonicalize(fx._dir.path())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let upstream = fx.upstream.uri();
    // What differs from run to run gets a fixed value.
    fn steady(value: &mut Value) {
        match value {
            Value::Array(items) => items.iter_mut().for_each(steady),
            Value::Object(fields) => {
                for (key, field) in fields.iter_mut() {
                    let fixed = match key.as_str() {
                        "version" => json!("0.0.0"),
                        "executable" => json!("/Users/example/bin/dedupd"),
                        "pid" => json!(1),
                        "volume_total" => json!(1_000_000_000_000u64),
                        "volume_free" => json!(400_000_000_000u64),
                        "speed" => json!(0),
                        "resets_in" => json!(100),
                        "window" => json!(2),
                        "started_at" | "created_at" | "updated_at" | "finished_at"
                        | "fetched_at" | "checked_at" | "at"
                            if field.is_number() =>
                        {
                            json!(1_700_000_000)
                        }
                        _ => {
                            steady(field);
                            continue;
                        }
                    };
                    *field = fixed;
                }
            }
            _ => {}
        }
    }
    let normalise = |value: &Value| -> String {
        let body = value
            .to_string()
            .replace(&real, "/Users/example")
            .replace(&root, "/Users/example")
            .replace(&upstream, "https://huggingface.co");
        let mut value: Value = serde_json::from_str(&body).unwrap();
        steady(&mut value);
        serde_json::to_string_pretty(&value).unwrap() + "\n"
    };

    let first_event = {
        use futures_util::StreamExt;
        let resp = fx
            .http
            .get(format!("{}/dedupd/v1/events", fx.base))
            .send()
            .await
            .unwrap();
        let chunk = resp.bytes_stream().next().await.unwrap().unwrap();
        serde_json::from_slice::<Value>(&chunk).unwrap()
    };
    let (_, search) = control(&fx, "GET", "hub/search?q=repo", None).await;
    let (_, hub_model) = control(&fx, "GET", &format!("hub/models/{REPO}"), None).await;
    let (_, pull) = control(
        &fx,
        "POST",
        "pull",
        Some(json!({"model": REPO, "apps": ["bionic"]})),
    )
    .await;
    let (_, scan) = control(&fx, "POST", "scan", Some(json!({}))).await;
    let bad = json!({"model": "not a model", "apps": []});
    let (_, error) = control(&fx, "POST", "pull", Some(bad)).await;
    let (_, status) = control(&fx, "GET", "status", None).await;
    let again = json!({"model": format!("{REPO}:model"), "apps": ["hf"], "offline": true});
    let (_, enqueue) = control(&fx, "POST", "downloads", Some(again.clone())).await;
    download(&fx, again).await;
    let (_, downloads) = control(&fx, "GET", "downloads", None).await;
    let (_, job) = control(&fx, "GET", "downloads/2", None).await;
    let (_, models) = control(&fx, "GET", "models", None).await;
    let (_, model) = control(&fx, "GET", &format!("models/{REPO}"), None).await;
    let (_, locations) = control(&fx, "GET", "locations", None).await;
    let (_, config) = control(&fx, "GET", "config", None).await;
    let (_, check) = control(&fx, "POST", "check", Some(json!({"repo": REPO}))).await;
    let ask = json!({"repo": REPO, "app": "bionic"});
    let (_, unplace) = control(&fx, "POST", "unplace", Some(ask)).await;
    let (_, gc) = control(&fx, "GET", "gc?offload=true", None).await;
    let (_, verify) = control(&fx, "GET", "verify", None).await;
    let (_, log) = control(&fx, "GET", "log", None).await;

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/control");
    let update = std::env::var_os("DEDUPD_UPDATE_FIXTURES").is_some();
    for (name, body) in [
        ("status.json", status),
        ("pull.json", pull),
        ("scan.json", scan),
        ("error.json", error),
        ("event.json", first_event),
        ("search.json", search),
        ("hub-model.json", hub_model),
        ("enqueue.json", enqueue),
        ("downloads.json", downloads),
        ("download.json", job),
        ("models.json", models),
        ("model.json", model),
        ("locations.json", locations),
        ("config.json", config),
        ("check.json", check),
        ("unplace.json", unplace),
        ("gc.json", gc),
        ("verify.json", verify),
        ("log.json", log),
    ] {
        let now = normalise(&body);
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
