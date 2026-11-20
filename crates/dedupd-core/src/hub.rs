//! Read-only client for the Hugging Face Hub API: just enough to learn what a
//! repository revision contains.

use crate::fetch::Expect;
use crate::governor::{self, Class, Governor};
use dedupd_api::library::ModelMeta;
use serde::Deserialize;
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

/// Whether trying again later may get another answer: the Hub was busy, out
/// of reach, or failed on its side.
pub fn is_temporary(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<HubError>(),
            Some(HubError::Busy { .. } | HubError::Transport(_))
        ) || matches!(
            cause.downcast_ref::<HubError>(),
            Some(HubError::Status { status, .. }) if *status >= 500 || *status == 429
        )
    })
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

/// What the Hub's model info says about a model, in the terms the owner
/// thinks in. Read from a search result, the live model info or an archived
/// one alike; what an answer does not carry stays empty.
pub fn model_meta(info: &serde_json::Value) -> ModelMeta {
    let text = |v: &serde_json::Value| v.as_str().map(str::to_string);
    let field = |name: &str| info.get(name).and_then(text);
    let number = |name: &str| info.get(name).and_then(|v| v.as_u64());
    // A card field that is one value or a list of them.
    let card = |name: &str| -> Vec<String> {
        match info.get("cardData").and_then(|c| c.get(name)) {
            Some(serde_json::Value::String(s)) => vec![s.clone()],
            Some(serde_json::Value::Array(a)) => a.iter().filter_map(text).collect(),
            _ => Vec::new(),
        }
    };
    let tags: Vec<String> = info
        .get("tags")
        .and_then(|t| t.as_array())
        .map(|a| a.iter().filter_map(text).collect())
        .unwrap_or_default();
    let tagged = |prefix: &str| {
        tags.iter()
            .filter_map(|t| t.strip_prefix(prefix))
            .find(|rest| !rest.contains(':'))
            .map(str::to_string)
    };
    let library_name = field("library_name");
    let mut formats = Vec::new();
    for format in ["gguf", "mlx", "safetensors", "onnx"] {
        if tags.iter().any(|t| t == format) || library_name.as_deref() == Some(format) {
            formats.push(format.to_string());
        }
    }
    let gguf = info.get("gguf");
    ModelMeta {
        author: field("author").or_else(|| {
            let id = field("id").or_else(|| field("modelId"))?;
            id.split_once('/').map(|(org, _)| org.to_string())
        }),
        pipeline_tag: field("pipeline_tag"),
        license: card("license")
            .into_iter()
            .next()
            .or_else(|| tagged("license:")),
        base_model: card("base_model")
            .into_iter()
            .next()
            .or_else(|| tagged("base_model:")),
        parameters: info
            .get("safetensors")
            .and_then(|s| s.get("total"))
            .and_then(|v| v.as_u64())
            .or_else(|| gguf.and_then(|g| g.get("total")).and_then(|v| v.as_u64())),
        architecture: gguf
            .and_then(|g| g.get("architecture"))
            .and_then(text)
            .or_else(|| {
                info.get("config")
                    .and_then(|c| c.get("model_type"))
                    .and_then(text)
            }),
        context_length: gguf
            .and_then(|g| g.get("context_length"))
            .and_then(|v| v.as_u64()),
        // `false`, or the kind of approval: "auto" or "manual".
        gated: info
            .get("gated")
            .is_some_and(|g| g.as_bool().unwrap_or(!g.is_null())),
        private: info.get("private").and_then(|v| v.as_bool()) == Some(true),
        formats,
        languages: card("language"),
        library_name,
        tags,
        downloads: number("downloads"),
        likes: number("likes"),
        last_modified: field("lastModified"),
        created_at: field("createdAt"),
        used_storage: number("usedStorage"),
    }
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

pub use crate::token::Token;

pub struct HubClient {
    http: reqwest::Client,
    endpoint: String,
    token: Arc<Token>,
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
            token: Token::fixed(token),
            governor: Governor::new(governor::DEFAULT_CONNECTIONS, 0),
        })
    }

    /// Send `token`, shared with the fetcher, in place of a fixed one.
    pub fn with_token(mut self, token: Arc<Token>) -> Self {
        self.token = token;
        self
    }

    /// The token this client sends.
    pub fn token(&self) -> &Arc<Token> {
        &self.token
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
        self.token.get().is_some()
    }

    /// Where `token_from_environment` would read the token from: the name
    /// of the variable or the path of the file. `None` when there is none.
    pub fn token_source() -> Option<String> {
        crate::token::source()
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
        crate::token::from_environment()
    }

    async fn get(
        &self,
        url: &str,
        repo: &str,
        auth: Option<&str>,
    ) -> Result<reqwest::Response, HubError> {
        // The token goes to the configured endpoint only, never anywhere a
        // response might point.
        if !crate::token::goes_to(&self.endpoint, url) {
            return Err(HubError::Malformed(format!(
                "refusing to follow a link away from {}: {url}",
                self.endpoint
            )));
        }
        let mut req = self.http.get(url);
        if let Some(value) = auth {
            req = req.header(reqwest::header::AUTHORIZATION, value);
        } else if let Some(t) = self.token.get() {
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
            .filter(|s| is_commit(s))
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

impl HubClient {
    /// The Hugging Face account the token belongs to. `None` when there is
    /// no token, or when it belongs to an organisation, which likes nothing.
    /// Hugging Face limits how often this may be asked; the caller keeps
    /// the answer.
    pub async fn whoami(&self) -> Result<Option<String>, HubError> {
        if !self.has_token() {
            return Ok(None);
        }
        let url = format!("{}/api/whoami-v2", self.endpoint);
        let me: serde_json::Value = self
            .get(&url, "your account", None)
            .await?
            .json()
            .await
            .map_err(|e| HubError::Malformed(format!("account: {e}")))?;
        if me.get("type").and_then(|v| v.as_str()) != Some("user") {
            return Ok(None);
        }
        let name = me.get("name").and_then(|v| v.as_str()).unwrap_or_default();
        if !is_user_name(name) {
            return Err(HubError::Malformed(format!("account name {name:?}")));
        }
        Ok(Some(name.to_string()))
    }

    /// The models `user` has liked, as `org/repo`. Likes are public on
    /// Hugging Face; datasets, Spaces and names that are not a model's
    /// are left out.
    pub async fn liked_models(&self, user: &str) -> Result<Vec<String>, HubError> {
        if !is_user_name(user) {
            return Err(HubError::Malformed(format!("account name {user:?}")));
        }
        let mut models = Vec::new();
        let mut next = Some(format!("{}/api/users/{user}/likes", self.endpoint));
        while let Some(url) = next {
            let resp = self.get(&url, "your likes", None).await?;
            next = resp
                .headers()
                .get(reqwest::header::LINK)
                .and_then(|v| v.to_str().ok())
                .and_then(next_link);
            let page: Vec<serde_json::Value> = resp
                .json()
                .await
                .map_err(|e| HubError::Malformed(format!("likes: {e}")))?;
            for like in &page {
                let repo = &like["repo"];
                if repo["type"].as_str() == Some("model")
                    && let Some(name) = repo["name"].as_str()
                    && crate::names::is_repo_name(name)
                {
                    models.push(name.to_string());
                }
            }
        }
        Ok(models)
    }
}

/// Hugging Face's rule for an account name, which goes into a path.
fn is_user_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 96
        && !name.starts_with(['-', '.'])
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.contains("..")
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

/// True for a full 40-character git commit id.
pub fn is_commit(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The branch or tag name in `revision`; `None` when it is a commit id,
/// which is not a ref.
pub fn ref_name(revision: &str) -> Option<&str> {
    (!is_commit(revision)).then_some(revision)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn reads_what_the_model_info_says() {
        let info = serde_json::json!({
            "id": "bartowski/SmolLM2-135M-Instruct-GGUF",
            "gated": "auto",
            "tags": ["gguf", "text-generation", "license:apache-2.0",
                     "base_model:quantized:HuggingFaceTB/SmolLM2-135M-Instruct",
                     "base_model:HuggingFaceTB/SmolLM2-135M-Instruct"],
            "pipeline_tag": "text-generation",
            "gguf": {"total": 134515008u64, "architecture": "llama", "context_length": 8192},
            "cardData": {"language": ["en"]},
            "downloads": 104143
        });
        let m = model_meta(&info);
        assert_eq!(m.author.as_deref(), Some("bartowski"));
        assert_eq!(m.formats, ["gguf"]);
        assert_eq!(m.license.as_deref(), Some("apache-2.0"));
        assert_eq!(
            m.base_model.as_deref(),
            Some("HuggingFaceTB/SmolLM2-135M-Instruct")
        );
        assert_eq!(
            (m.parameters, m.context_length),
            (Some(134515008), Some(8192))
        );
        assert!(m.gated && !m.private);
        assert_eq!(m.languages, ["en"]);
        assert!(!model_meta(&serde_json::json!({"gated": false})).gated);
    }

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

    #[tokio::test]
    async fn refuses_a_next_link_on_a_host_that_only_begins_like_the_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/models/org/repo/revision/main"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"sha": COMMIT})))
            .mount(&server)
            .await;
        // `http://127.0.0.1:1234` and `http://127.0.0.1:12345` share a prefix.
        let lookalike = format!("{}5/x", server.uri());
        Mock::given(method("GET"))
            .and(path(format!("/api/models/org/repo/tree/{COMMIT}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("link", format!(r#"<{lookalike}>; rel="next""#).as_str())
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

    #[tokio::test]
    async fn reads_whose_token_it_is_and_the_models_they_like() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/whoami-v2"))
            .and(header("authorization", "Bearer secret"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"type": "user", "name": "ove"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/users/ove/likes"))
            .and(query_param("cursor", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"createdAt": "2026-01-01T00:00:00.000Z", "repo": {"name": "org/second", "type": "model"}}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/users/ove/likes"))
            .and(query_param_is_missing("cursor"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header(
                        "link",
                        format!(
                            r#"<{}/api/users/ove/likes?cursor=2>; rel="next""#,
                            server.uri()
                        )
                        .as_str(),
                    )
                    .set_body_json(json!([
                        {"repo": {"name": "org/first", "type": "model"}},
                        {"repo": {"name": "org/data", "type": "dataset"}},
                        {"repo": {"name": "org/app", "type": "space"}},
                        {"repo": {"name": "../../etc", "type": "model"}}
                    ])),
            )
            .mount(&server)
            .await;
        let hub = HubClient::new(server.uri(), Some("secret".into())).unwrap();
        assert_eq!(hub.whoami().await.unwrap().as_deref(), Some("ove"));
        assert_eq!(
            hub.liked_models("ove").await.unwrap(),
            ["org/first", "org/second"]
        );
        assert!(hub.liked_models("../x").await.is_err());
        // No token: nobody to ask about, and nothing is sent.
        let anonymous = HubClient::new(server.uri(), None).unwrap();
        assert_eq!(anonymous.whoami().await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_token_signed_in_later_is_sent_without_a_restart() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/whoami-v2"))
            .and(header("authorization", "Bearer second"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"type": "user", "name": "ove"})),
            )
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("token");
        let read = file.clone();
        let token = Token::read_by(move || {
            std::fs::read_to_string(&read)
                .ok()
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
        });
        let hub = HubClient::new(server.uri(), None)
            .unwrap()
            .with_token(token.clone());
        assert!(!hub.has_token());
        assert!(!token.refresh(), "still none");

        std::fs::write(&file, "second\n").unwrap();
        assert!(token.refresh());
        assert!(!token.refresh(), "the same again");
        assert_eq!(hub.whoami().await.unwrap().as_deref(), Some("ove"));

        std::fs::remove_file(&file).unwrap();
        assert!(token.refresh());
        assert!(!hub.has_token(), "signed out");
        assert!(!Token::fixed(Some("x".into())).refresh());
    }
}
