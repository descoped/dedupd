//! Read-only client for the Hugging Face Hub API: just enough to learn what a
//! repository revision contains.

use crate::fetch::Expect;
use crate::governor::{self, Class, Governor};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

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
    /// The Hub is busy or this caller has used up its requests for now.
    #[error("the Hub asks to wait {seconds} seconds before the next request")]
    Busy { seconds: u64 },
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

impl TreeEntry {
    /// The name the Hub and the Hugging Face cache give the file's content:
    /// the sha256 for an LFS file, the git blob id otherwise.
    pub fn etag(&self) -> &str {
        self.lfs_sha256.as_deref().unwrap_or(&self.git_oid)
    }

    /// What a download of the file must hash to.
    pub fn expect(&self) -> Expect {
        match &self.lfs_sha256 {
            Some(sha) => Expect::Sha256(sha.clone()),
            None => Expect::GitOid(self.git_oid.clone()),
        }
    }
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
    governor: Arc<Governor>,
}

impl HubClient {
    pub fn new(endpoint: impl Into<String>, token: Option<String>) -> Result<Self, HubError> {
        let http = reqwest::Client::builder()
            .user_agent(crate::USER_AGENT)
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(60))
            .build()?;
        Ok(Self {
            http,
            endpoint: endpoint.into().trim_end_matches('/').to_string(),
            token,
            governor: Governor::new(governor::DEFAULT_CONNECTIONS, 0),
        })
    }

    /// Share `governor` with everything else that asks the provider.
    pub fn with_governor(mut self, governor: Arc<Governor>) -> Self {
        self.governor = governor;
        self
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Whether a token is sent with requests to the Hub.
    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    /// Where `token_from_environment` would read the token from: the name
    /// of the variable or the path of the file. `None` when there is none.
    pub fn token_source() -> Option<String> {
        if std::env::var("HF_TOKEN").is_ok_and(|t| !t.trim().is_empty()) {
            return Some("HF_TOKEN".into());
        }
        let file = token_file()?;
        let text = std::fs::read_to_string(&file).ok()?;
        (!text.trim().is_empty()).then(|| file.to_string_lossy().into_owned())
    }

    /// A GET under the endpoint, for the parts of the Hub API this client
    /// has no method of its own for. `what` names the thing asked about in
    /// the error.
    pub async fn api(
        &self,
        path_and_query: &str,
        what: &str,
    ) -> Result<reqwest::Response, HubError> {
        let url = format!("{}{path_and_query}", self.endpoint);
        self.get(&url, what, None).await
    }

    /// The token other Hugging Face tools use: `HF_TOKEN`, else the token file.
    pub fn token_from_environment() -> Option<String> {
        if let Ok(t) = std::env::var("HF_TOKEN")
            && !t.trim().is_empty()
        {
            return Some(t.trim().to_string());
        }
        let t = std::fs::read_to_string(token_file()?).ok()?;
        (!t.trim().is_empty()).then(|| t.trim().to_string())
    }

    async fn get(
        &self,
        url: &str,
        repo: &str,
        auth: Option<&str>,
    ) -> Result<reqwest::Response, HubError> {
        // The token goes to the configured endpoint only, never anywhere a
        // response might point.
        if !url.starts_with(&self.endpoint) {
            return Err(HubError::Malformed(format!(
                "refusing to follow a link away from {}: {url}",
                self.endpoint
            )));
        }
        let mut req = self.http.get(url);
        if let Some(value) = auth {
            req = req.header(reqwest::header::AUTHORIZATION, value);
        } else if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        let _permit = self.governor.permit(Class::Api).await;
        let resp = req.send().await?;
        self.governor.observe(resp.headers());
        let status = resp.status().as_u16();
        match status {
            200 => Ok(resp),
            429 | 503 => {
                let pause = governor::retry_after(resp.headers())
                    .unwrap_or(std::time::Duration::from_secs(30));
                let held = self.governor.congested(Some(pause));
                Err(HubError::Busy {
                    seconds: held.as_secs().max(1),
                })
            }
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
        self.snapshot_as(repo, revision, None).await
    }

    /// Like `snapshot`, but asking with the caller's own `Authorization`
    /// value when it has one, so the Hub decides what that caller may see.
    pub async fn snapshot_as(
        &self,
        repo: &str,
        revision: &str,
        auth: Option<&str>,
    ) -> Result<Snapshot, HubError> {
        let info_url = format!("{}/api/models/{repo}/revision/{revision}", self.endpoint);
        let info_json = self.get(&info_url, repo, auth).await?.text().await?;
        let info: serde_json::Value = serde_json::from_str(&info_json)
            .map_err(|e| HubError::Malformed(format!("model info: {e}")))?;
        let commit = info
            .get("sha")
            .and_then(|v| v.as_str())
            .filter(|s| crate::db::is_commit(s))
            .ok_or_else(|| HubError::Malformed("model info has no commit sha".into()))?
            .to_string();

        let mut pages: Vec<serde_json::Value> = Vec::new();
        let mut next = Some(format!(
            "{}/api/models/{repo}/tree/{commit}?recursive=true&expand=false",
            self.endpoint
        ));
        while let Some(url) = next {
            let resp = self.get(&url, repo, auth).await?;
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
            let raw = RawEntry::deserialize(value)
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

/// The file Hugging Face's own tools keep the token in.
fn token_file() -> Option<PathBuf> {
    std::env::var_os("HF_TOKEN_PATH")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HF_HOME").map(|h| PathBuf::from(h).join("token")))
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache/huggingface/token"))
        })
}

/// Pull the `rel="next"` target out of a `Link` header.
pub fn next_link(header: &str) -> Option<String> {
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
