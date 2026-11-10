//! Support shared by the integration tests of this crate and of the server
//! crate, which includes this file by path.
#![allow(dead_code)]

use dedupd_core::Home;
use dedupd_core::db::Db;
use dedupd_core::fetch::Fetcher;
use dedupd_core::hub::HubClient;
use dedupd_core::place::AppDirs;
use dedupd_core::pull::Engine;
use dedupd_core::store::Store;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::{Arc, Mutex};
use wiremock::{Request, Respond, ResponseTemplate};

pub const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
pub const REPO: &str = "org/Repo-GGUF";

pub fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The id git gives `bytes` as a blob.
pub fn git_oid(bytes: &[u8]) -> String {
    use sha1::Sha1;
    let mut h = Sha1::new();
    h.update(format!("blob {}\0", bytes.len()).as_bytes());
    h.update(bytes);
    hex::encode(h.finalize())
}

/// Serves a body and honours `Range` (`bytes=N-` and `bytes=N-M`), as the
/// Hub's CDN does.
pub struct Ranged(pub Vec<u8>);

impl Respond for Ranged {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let spec = req
            .headers
            .get("range")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("bytes="))
            .and_then(|v| v.split_once('-'));
        match spec {
            Some((a, b)) => {
                let start: usize = a.parse().unwrap();
                let end: usize = b.parse().unwrap_or(self.0.len() - 1);
                ResponseTemplate::new(206)
                    .insert_header(
                        "content-range",
                        format!("bytes {start}-{end}/{}", self.0.len()).as_str(),
                    )
                    .set_body_bytes(self.0[start..=end].to_vec())
            }
            None => ResponseTemplate::new(200).set_body_bytes(self.0.clone()),
        }
    }
}

/// An engine on `home` that asks `endpoint`, with scratch app folders under
/// `apps`.
pub fn engine(home: &Home, endpoint: &str, apps: &Path, archive: Option<Home>) -> Arc<Engine> {
    Arc::new(Engine {
        db: Mutex::new(Db::open(&home.db_path()).unwrap()),
        store: Store::open(home).unwrap().with_archive(archive),
        hub: HubClient::new(endpoint, None).unwrap(),
        fetcher: Fetcher::new(endpoint, None).unwrap(),
        dirs: AppDirs::under(apps),
    })
}

/// The manifest Hugging Face's registry would generate for a model with
/// this config layer, weights and template.
pub fn ollama_manifest(config: &[u8], weights: &[u8], template: &[u8]) -> String {
    let layer = |media_type: &str, body: &[u8]| {
        json!({
            "mediaType": media_type,
            "digest": format!("sha256:{}", sha256(body)),
            "size": body.len(),
        })
    };
    json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
        "config": layer("application/vnd.docker.container.image.v1+json", config),
        "layers": [
            layer("application/vnd.ollama.image.model", weights),
            layer("application/vnd.ollama.image.template", template),
        ]
    })
    .to_string()
}
