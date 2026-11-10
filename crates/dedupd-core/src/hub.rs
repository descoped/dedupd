//! Read-only client for the Hugging Face Hub API: just enough to learn what a
//! repository revision contains.

use serde::Deserialize;
use std::path::PathBuf;

pub const DEFAULT_ENDPOINT: &str = "https://huggingface.co";

#[derive(Debug, thiserror::Error)]
pub enum HubError {
    /// The Hub says the repository or revision does not exist, or will not
    /// show it to this caller. The Hub answers 401 for both a missing and a
    /// private repository, so the two cannot be told apart.
    #[error("{repo} is not available on the Hub (HTTP {status})")]
    NotAvailable { repo: String, status: u16 },
    #[error("the Hub answered HTTP {status} for {url}")]
    Status { status: u16, url: String },
    #[error("could not reach the Hub: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("unexpected answer from the Hub: {0}")]
    Malformed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub path: String,
    pub size: u64,
    /// Git blob id. For LFS files this is the id of the pointer file, not of
    /// the content.
    pub git_oid: String,
    /// sha256 of the content, present only for files stored in LFS.
    pub lfs_sha256: Option<String>,
}

/// Everything the Hub says about one revision, with the raw answers kept so
/// they can be archived verbatim.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub commit: String,
    pub info_json: String,
    pub tree_json: String,
    pub entries: Vec<TreeEntry>,
}

#[derive(Deserialize)]
struct RawEntry {
    #[serde(rename = "type")]
    kind: String,
    path: String,
    oid: String,
    #[serde(default)]
    size: u64,
    lfs: Option<RawLfs>,
}

#[derive(Deserialize)]
struct RawLfs {
    oid: String,
}

pub struct HubClient {
    http: reqwest::Client,
    endpoint: String,
    token: Option<String>,
}

impl HubClient {
    pub fn new(endpoint: impl Into<String>, token: Option<String>) -> Result<Self, HubError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("dedupd/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(60))
            .build()?;
        Ok(Self {
            http,
            endpoint: endpoint.into().trim_end_matches('/').to_string(),
            token,
        })
    }

    /// The token other Hugging Face tools use: `HF_TOKEN`, else the token file.
    pub fn token_from_environment() -> Option<String> {
        if let Ok(t) = std::env::var("HF_TOKEN")
            && !t.trim().is_empty()
        {
            return Some(t.trim().to_string());
        }
        let file = std::env::var_os("HF_TOKEN_PATH")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HF_HOME").map(|h| PathBuf::from(h).join("token")))
            .or_else(|| {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache/huggingface/token"))
            })?;
        let t = std::fs::read_to_string(file).ok()?;
        (!t.trim().is_empty()).then(|| t.trim().to_string())
    }

    async fn get(&self, url: &str, repo: &str) -> Result<reqwest::Response, HubError> {
        // The token goes to the configured endpoint only, never anywhere a
        // response might point.
        if !url.starts_with(&self.endpoint) {
            return Err(HubError::Malformed(format!(
                "refusing to follow a link away from {}: {url}",
                self.endpoint
            )));
        }
        let mut req = self.http.get(url);
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        let resp = req.send().await?;
        let status = resp.status().as_u16();
        match status {
            200 => Ok(resp),
            401 | 403 | 404 | 410 => Err(HubError::NotAvailable {
                repo: repo.to_string(),
                status,
            }),
            _ => Err(HubError::Status {
                status,
                url: url.to_string(),
            }),
        }
    }

    /// Resolve `revision` (a branch, tag or commit) and list its files.
    pub async fn snapshot(&self, repo: &str, revision: &str) -> Result<Snapshot, HubError> {
        let info_url = format!("{}/api/models/{repo}/revision/{revision}", self.endpoint);
        let info_json = self.get(&info_url, repo).await?.text().await?;
        let info: serde_json::Value = serde_json::from_str(&info_json)
            .map_err(|e| HubError::Malformed(format!("model info: {e}")))?;
        let commit = info
            .get("sha")
            .and_then(|v| v.as_str())
            .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| HubError::Malformed("model info has no commit sha".into()))?
            .to_string();

        let mut pages: Vec<serde_json::Value> = Vec::new();
        let mut next = Some(format!(
            "{}/api/models/{repo}/tree/{commit}?recursive=true&expand=false",
            self.endpoint
        ));
        while let Some(url) = next {
            let resp = self.get(&url, repo).await?;
            next = resp
                .headers()
                .get(reqwest::header::LINK)
                .and_then(|v| v.to_str().ok())
                .and_then(next_link);
            let page: Vec<serde_json::Value> = resp
                .json()
                .await
                .map_err(|e| HubError::Malformed(format!("tree listing: {e}")))?;
            pages.extend(page);
        }

        let mut entries = Vec::new();
        for value in &pages {
            let raw: RawEntry = serde_json::from_value(value.clone())
                .map_err(|e| HubError::Malformed(format!("tree entry: {e}")))?;
            if raw.kind == "file" {
                entries.push(TreeEntry {
                    path: raw.path,
                    size: raw.size,
                    git_oid: raw.oid,
                    lfs_sha256: raw.lfs.map(|l| l.oid),
                });
            }
        }

        Ok(Snapshot {
            commit,
            info_json,
            tree_json: serde_json::Value::Array(pages).to_string(),
            entries,
        })
    }
}

/// Pull the `rel="next"` target out of a `Link` header.
fn next_link(header: &str) -> Option<String> {
    header.split(',').find_map(|part| {
        let (target, params) = part.split_once(';')?;
        let is_next = params
            .split(';')
            .any(|p| p.trim().replace('"', "") == "rel=next");
        let url = target.trim().strip_prefix('<')?.strip_suffix('>')?;
        is_next.then(|| url.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn parses_link_header() {
        assert_eq!(
            next_link(r#"<https://h/api/x?cursor=abc>; rel="next""#).as_deref(),
            Some("https://h/api/x?cursor=abc")
        );
        assert_eq!(
            next_link(r#"<https://h/a>; rel="prev", <https://h/b>; rel="next""#).as_deref(),
            Some("https://h/b")
        );
        assert_eq!(next_link(r#"<https://h/a>; rel="xet-auth""#), None);
    }

    #[tokio::test]
    async fn snapshot_follows_pagination_and_sends_the_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/models/org/repo/revision/main"))
            .and(header("authorization", "Bearer secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"sha": COMMIT})))
            .mount(&server)
            .await;
        let tree = format!("/api/models/org/repo/tree/{COMMIT}");
        Mock::given(method("GET"))
            .and(path(tree.clone()))
            .and(query_param_is_missing("cursor"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header(
                        "link",
                        format!(
                            r#"<{}{tree}?recursive=true&cursor=p2>; rel="next""#,
                            server.uri()
                        )
                        .as_str(),
                    )
                    .set_body_json(json!([
                        {"type": "directory", "oid": "d", "size": 0, "path": "sub"},
                        {"type": "file", "oid": "g1", "size": 5, "path": "config.json"}
                    ])),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(tree))
            .and(query_param("cursor", "p2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"type": "file", "oid": "g2", "size": 9, "path": "sub/model.gguf",
                 "lfs": {"oid": "sha", "size": 9, "pointerSize": 134}, "xetHash": "x"}
            ])))
            .mount(&server)
            .await;

        let hub = HubClient::new(server.uri(), Some("secret".into())).unwrap();
        let snap = hub.snapshot("org/repo", "main").await.unwrap();
        assert_eq!(snap.commit, COMMIT);
        assert_eq!(
            snap.entries,
            vec![
                TreeEntry {
                    path: "config.json".into(),
                    size: 5,
                    git_oid: "g1".into(),
                    lfs_sha256: None
                },
                TreeEntry {
                    path: "sub/model.gguf".into(),
                    size: 9,
                    git_oid: "g2".into(),
                    lfs_sha256: Some("sha".into())
                },
            ]
        );
        // The archived tree keeps every field the Hub sent, across both pages.
        let archived: serde_json::Value = serde_json::from_str(&snap.tree_json).unwrap();
        assert_eq!(archived.as_array().unwrap().len(), 3);
        assert_eq!(archived[2]["xetHash"], "x");
    }

    #[tokio::test]
    async fn missing_repo_is_reported_as_not_available() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let hub = HubClient::new(server.uri(), None).unwrap();
        let err = hub.snapshot("org/gone", "main").await.unwrap_err();
        assert!(matches!(err, HubError::NotAvailable { status: 401, .. }));
    }

    #[tokio::test]
    async fn refuses_a_next_link_on_another_host() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/models/org/repo/revision/main"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"sha": COMMIT})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/api/models/org/repo/tree/{COMMIT}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("link", r#"<https://elsewhere.example/x>; rel="next""#)
                    .set_body_json(json!([])),
            )
            .mount(&server)
            .await;
        let hub = HubClient::new(server.uri(), Some("secret".into())).unwrap();
        assert!(matches!(
            hub.snapshot("org/repo", "main").await,
            Err(HubError::Malformed(_))
        ));
    }
}
