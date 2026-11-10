//! The Hugging Face Hub API, as `huggingface_hub`, MLX tools and llama.cpp
//! use it.

use crate::proxy::{pass_through, relay, relay_head, unreachable, upstream};
use crate::{Call, Server, files, json, plain};
use axum::body::Body;
use axum::http::{StatusCode, header};
use axum::response::Response;
use dedupd_core::adopt::PROVIDER_HF;
use dedupd_core::db::is_commit;
use dedupd_core::fetch::Expect;
use dedupd_core::hub::{HubError, Snapshot};
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a branch name is trusted to still point at the same commit.
const REF_TTL: Duration = Duration::from_secs(60);

/// A revision dedupd can answer for.
pub(crate) struct Rev {
    pub snap: Snapshot,
    /// Upstream could not be asked, or no longer has it.
    pub from_archive: bool,
}

/// True when the archived model info says access is restricted. Such a
/// repository is checked upstream on every request.
fn restricted(info_json: &str) -> bool {
    let info: Value = serde_json::from_str(info_json).unwrap_or(Value::Null);
    let gated = info
        .get("gated")
        .is_some_and(|g| *g != Value::Bool(false) && !g.is_null());
    let private = info.get("private") == Some(&Value::Bool(true));
    gated || private
}

/// Remove every `xetHash` so clients fetch file bytes from us over plain HTTP
/// instead of from the Xet servers.
fn strip_xet(value: &mut Value) {
    match value {
        Value::Array(items) => items.iter_mut().for_each(strip_xet),
        Value::Object(map) => {
            map.remove("xetHash");
            map.values_mut().for_each(strip_xet);
        }
        _ => {}
    }
}

impl Server {
    fn archived(&self, repo: &str, rev: &str) -> Option<Snapshot> {
        let db = self.engine.db();
        let id = db.repo_id(PROVIDER_HF, repo).ok()??;
        db.archived_snapshot(id, rev).ok()?
    }

    /// Find out what `repo@rev` contains: from upstream when it answers, from
    /// the archive when it cannot or when the repository is gone. `Err` means
    /// dedupd has nothing to say and the request should be passed through.
    pub(crate) async fn revision(
        &self,
        repo: &str,
        rev: &str,
        auth: Option<&str>,
    ) -> Result<Rev, ()> {
        let archived = self.archived(repo, rev);
        let open = archived.as_ref().is_some_and(|s| !restricted(&s.info_json));
        if open {
            // A commit never changes; a branch is trusted for a short while.
            let fresh = is_commit(rev)
                || self
                    .commits
                    .lock()
                    .expect("lock")
                    .get(&(repo.to_string(), rev.to_string()))
                    .is_some_and(|(_, at)| at.elapsed() < REF_TTL);
            if fresh && let Some(snap) = archived.clone() {
                return Ok(Rev {
                    snap,
                    from_archive: false,
                });
            }
        }

        match self.engine.hub.snapshot_as(repo, rev, auth).await {
            Ok(snap) => {
                {
                    let mut db = self.engine.db();
                    if let Ok(id) = db.upsert_repo(PROVIDER_HF, repo) {
                        let name = (!is_commit(rev)).then_some(rev);
                        if let Err(e) = db.record_snapshot(id, name, &snap) {
                            eprintln!("dedupd: archiving {repo}@{rev}: {e:#}");
                        }
                    }
                }
                self.commits.lock().expect("lock").insert(
                    (repo.to_string(), rev.to_string()),
                    (snap.commit.clone(), Instant::now()),
                );
                Ok(Rev {
                    snap,
                    from_archive: false,
                })
            }
            // Gone or hidden upstream: an open repository is answered from
            // the archive; a restricted one is the Hub's decision to make.
            Err(HubError::NotAvailable { status, .. }) => match archived {
                Some(snap) if open => {
                    eprintln!(
                        "dedupd: {repo}@{rev}: Hugging Face answered {status}; using the archived copy"
                    );
                    Ok(Rev {
                        snap,
                        from_archive: true,
                    })
                }
                _ => Err(()),
            },
            // Upstream is down or unreachable: the archive answers.
            Err(e) => archived
                .map(|snap| {
                    eprintln!("dedupd: {repo}@{rev}: {e}; using the archived copy");
                    Rev {
                        snap,
                        from_archive: true,
                    }
                })
                .ok_or(()),
        }
    }
}

/// `/api/models/...`
pub(crate) async fn api(server: &Arc<Server>, call: &Call, rest: &[&str]) -> Response {
    let keyword = |s: &&str| matches!(*s, "revision" | "tree" | "paths-info" | "refs");
    let Some(at) = rest
        .iter()
        .position(keyword)
        .filter(|i| (1..=2).contains(i))
    else {
        // `/api/models/org/repo` is the model info for the default branch.
        if (1..=2).contains(&rest.len()) && call.method == axum::http::Method::GET {
            return info(server, call, &rest.join("/"), "main").await;
        }
        return pass_through(server, call).await;
    };
    let repo = rest[..at].join("/");
    let args = &rest[at + 1..];
    match (rest[at], args) {
        ("revision", [rev]) => info(server, call, &repo, rev).await,
        ("tree", [rev, path @ ..]) => {
            listing(server, call, &repo, rev, Some(&path.join("/"))).await
        }
        ("paths-info", [rev]) => listing(server, call, &repo, rev, None).await,
        ("refs", []) => refs(server, call, &repo).await,
        _ => pass_through(server, call).await,
    }
}

/// True when upstream's answer means "ask the archive instead".
fn failed(status: u16) -> bool {
    matches!(status, 401 | 404 | 410) || status >= 500
}

/// Branch and tag names, which llama.cpp asks for first. Upstream's answer is
/// relayed; when it cannot answer, the names archived for the repository are.
async fn refs(server: &Arc<Server>, call: &Call, repo: &str) -> Response {
    let url = format!(
        "{}{}",
        server.engine.fetcher.endpoint(),
        call.path_and_query()
    );
    let answer = upstream(server, call, &url, false).await;
    let down = match &answer {
        Ok(resp) => failed(resp.status().as_u16()),
        Err(_) => true,
    };
    if down {
        let archived = {
            let db = server.engine.db();
            db.repo_id(PROVIDER_HF, repo)
                .ok()
                .flatten()
                .and_then(|id| db.refs(id).ok())
                .unwrap_or_default()
        };
        // A restricted repository is the Hub's to answer for.
        let open = server
            .archived(repo, "main")
            .is_none_or(|s| !restricted(&s.info_json));
        if !archived.is_empty() && open {
            eprintln!("dedupd: {repo}: branch names answered from the archive");
            let branches: Vec<Value> = archived
                .iter()
                .map(|(name, commit)| {
                    serde_json::json!({
                        "name": name,
                        "ref": format!("refs/heads/{name}"),
                        "targetCommit": commit,
                    })
                })
                .collect();
            return json(
                StatusCode::OK,
                &serde_json::json!({"branches": branches, "converts": [], "tags": []}),
            );
        }
    }
    match answer {
        Ok(resp) => relay(server, call, resp),
        Err(e) => unreachable(e),
    }
}

/// Model info. Upstream's answer is relayed as it is; the archive answers
/// when upstream cannot.
async fn info(server: &Arc<Server>, call: &Call, repo: &str, rev: &str) -> Response {
    let url = format!(
        "{}{}",
        server.engine.fetcher.endpoint(),
        call.path_and_query()
    );
    let answer = upstream(server, call, &url, false).await;
    let down = match &answer {
        Ok(resp) => failed(resp.status().as_u16()),
        Err(_) => true,
    };
    if down
        && let Ok(found) = server
            .revision(repo, rev, call.header(header::AUTHORIZATION))
            .await
    {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
            .header("x-repo-commit", &found.snap.commit)
            .body(if call.is_head() {
                Body::empty()
            } else {
                Body::from(found.snap.info_json)
            })
            .expect("valid response");
    }
    match answer {
        Ok(resp) => {
            if resp.status().is_success() && !call.is_head() {
                // Clients ask for the model info first and then fetch by
                // commit. Archive the revision now, with the branch name, so
                // the name still resolves when upstream cannot be asked.
                let (server, repo, rev) = (server.clone(), repo.to_string(), rev.to_string());
                let auth = call.header(header::AUTHORIZATION).map(str::to_string);
                tokio::spawn(async move {
                    let _ = server.revision(&repo, &rev, auth.as_deref()).await;
                });
            }
            relay(server, call, resp)
        }
        Err(e) => unreachable(e),
    }
}

/// The paths a `paths-info` request asks about: a form or a JSON body.
fn requested_paths(call: &Call) -> Vec<String> {
    let body = String::from_utf8_lossy(&call.body);
    if let Ok(v) = serde_json::from_str::<Value>(&body) {
        return v
            .get("paths")
            .and_then(|p| p.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
    }
    body.split('&')
        .filter_map(|pair| pair.strip_prefix("paths="))
        .map(percent_decode)
        .collect()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if s.is_char_boundary(i + 1) && s.get(i + 1..i + 3).is_some() => {
                match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `tree` (with `tree_path`) and `paths-info` (without). The Xet hashes are
/// removed from upstream's answer; the archive answers when upstream cannot.
async fn listing(
    server: &Arc<Server>,
    call: &Call,
    repo: &str,
    rev: &str,
    tree_path: Option<&str>,
) -> Response {
    let url = format!(
        "{}{}",
        server.engine.fetcher.endpoint(),
        call.path_and_query()
    );
    let answer = upstream(server, call, &url, true).await;
    match answer {
        Ok(resp) if resp.status().is_success() => {
            let head = relay_head(server, call, &resp, false);
            let Ok(text) = resp.text().await else {
                return unreachable("the answer was cut short");
            };
            let body = match serde_json::from_str::<Value>(&text) {
                Ok(mut v) => {
                    strip_xet(&mut v);
                    v.to_string()
                }
                Err(_) => text,
            };
            head.header(header::CONTENT_LENGTH, body.len())
                .body(if call.is_head() {
                    Body::empty()
                } else {
                    Body::from(body)
                })
                .expect("valid response")
        }
        other => {
            let down = match &other {
                Ok(resp) => failed(resp.status().as_u16()),
                Err(_) => true,
            };
            if down
                && let Ok(found) = server
                    .revision(repo, rev, call.header(header::AUTHORIZATION))
                    .await
                && let Some(listed) = archived_listing(call, &found.snap, tree_path)
            {
                return listed;
            }
            match other {
                Ok(resp) => relay(server, call, resp),
                Err(e) => unreachable(e),
            }
        }
    }
}

/// Answer a listing from the archived tree, which was fetched in full.
///
/// `None` when a folder was asked for that the revision does not have; the
/// caller then relays upstream's own answer.
fn archived_listing(call: &Call, snap: &Snapshot, tree_path: Option<&str>) -> Option<Response> {
    let mut tree: Value = serde_json::from_str(&snap.tree_json).unwrap_or(Value::Array(vec![]));
    strip_xet(&mut tree);
    let entries = tree.as_array().cloned().unwrap_or_default();
    let path_of = |e: &Value| {
        e.get("path")
            .and_then(|p| p.as_str())
            .unwrap_or("")
            .to_string()
    };
    let picked: Vec<Value> = match tree_path {
        None => {
            let wanted = requested_paths(call);
            entries
                .into_iter()
                .filter(|e| wanted.contains(&path_of(e)))
                .collect()
        }
        Some(prefix) => {
            let recursive = call.query.as_deref().is_some_and(|q| {
                q.split('&')
                    .any(|p| matches!(p, "recursive=true" | "recursive=1" | "recursive=True"))
            });
            let prefix = prefix.trim_matches('/');
            entries
                .into_iter()
                .filter(|e| {
                    let path = path_of(e);
                    let inside = if prefix.is_empty() {
                        Some(path.as_str())
                    } else {
                        path.strip_prefix(prefix).and_then(|r| r.strip_prefix('/'))
                    };
                    inside.is_some_and(|r| !r.is_empty() && (recursive || !r.contains('/')))
                })
                .collect()
        }
    };
    if picked.is_empty() && tree_path.is_some_and(|p| !p.trim_matches('/').is_empty()) {
        return None;
    }
    let mut out = json(StatusCode::OK, &Value::Array(picked));
    out.headers_mut().insert(
        "x-repo-commit",
        snap.commit.parse().expect("commit is a valid header value"),
    );
    Some(out)
}

fn entry_not_found(repo: &str, rev: &Rev, path: &str) -> Response {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header("x-error-code", "EntryNotFound")
        .header("x-error-message", "Entry not found")
        .header("x-repo-commit", &rev.snap.commit)
        .body(Body::from(format!("{path} is not in {repo}")))
        .expect("valid response")
}

/// `HEAD` and `GET /{repo}/resolve/{rev}/{path}`: metadata and file bytes.
///
/// Answered directly, never with a redirect: the CDN upstream would redirect
/// to names files by their Xet hash, which would give the client's cache the
/// wrong blob names.
pub(crate) async fn resolve(
    server: &Arc<Server>,
    call: &Call,
    repo: &str,
    rev: &str,
    path: &str,
) -> Response {
    let path = percent_decode(path);
    let auth = call.header(header::AUTHORIZATION);
    let Ok(found) = server.revision(repo, rev, auth).await else {
        // Not something dedupd knows: relay upstream's own answer, error
        // headers included.
        return pass_through(server, call).await;
    };
    let Some(entry) = found.snap.entries.iter().find(|e| e.path == path) else {
        if found.from_archive {
            return entry_not_found(repo, &found, &path);
        }
        return pass_through(server, call).await;
    };
    let etag = entry.lfs_sha256.clone().unwrap_or(entry.git_oid.clone());
    let extra = [
        ("x-repo-commit", found.snap.commit.clone()),
        ("etag", format!("\"{etag}\"")),
    ];

    if call.is_head() {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_LENGTH, entry.size)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(extra[0].0, &extra[0].1)
            .header(extra[1].0, &extra[1].1)
            .body(Body::empty())
            .expect("valid response");
    }

    // In the store, or on the archive disk?
    let known = server.engine.db().blob_for_entry(entry).ok().flatten();
    if let Some(blob) = known {
        let (engine, sha) = (server.engine.clone(), blob.sha256.clone());
        let local = tokio::task::spawn_blocking(move || engine.store.ensure_local(&sha)).await;
        if matches!(local, Ok(Ok(true))) {
            let file = server.engine.store.blob_path(&blob.sha256);
            return files::serve_file(call, &file, blob.size, &extra).await;
        }
    }
    if found.from_archive {
        return plain(
            StatusCode::NOT_FOUND,
            format!("{repo} is not available upstream and {path} was never archived"),
        );
    }

    let url = server
        .engine
        .fetcher
        .resolve_url(repo, &found.snap.commit, &path);
    match &entry.lfs_sha256 {
        // A small file: fetch it whole, check it, then serve it.
        None => {
            let fetched = server
                .engine
                .fetcher
                .fetch(
                    &server.engine.store,
                    &url,
                    Some(entry.size),
                    &Expect::GitOid(entry.git_oid.clone()),
                    None,
                )
                .await;
            match fetched {
                Ok(got) => {
                    let _ = server.engine.db().upsert_blob(
                        &got.sha256,
                        got.size,
                        got.git_oid.as_deref(),
                    );
                    let file = server.engine.store.blob_path(&got.sha256);
                    files::serve_file(call, &file, got.size, &extra).await
                }
                Err(_) => pass_through(server, call).await,
            }
        }
        // A large file: stream it to the client and keep the bytes.
        Some(sha) => match upstream(server, call, &url, true).await {
            Ok(resp) if matches!(resp.status().as_u16(), 200 | 206) => {
                let mut out = Response::builder().status(resp.status().as_u16());
                for name in [header::CONTENT_LENGTH, header::CONTENT_RANGE] {
                    if let Some(v) = resp.headers().get(&name) {
                        out = out.header(name, v);
                    }
                }
                out.header(header::ACCEPT_RANGES, "bytes")
                    .header(header::CONTENT_TYPE, "application/octet-stream")
                    .header(extra[0].0, &extra[0].1)
                    .header(extra[1].0, &extra[1].1)
                    .body(files::tee(server, resp, sha))
                    .expect("valid response")
            }
            Ok(resp) => relay(server, call, resp),
            Err(e) => unreachable(e),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xet_hashes_are_removed_everywhere() {
        let mut v =
            serde_json::json!([{"path": "a", "xetHash": "x", "lfs": {"oid": "o", "xetHash": "y"}}]);
        strip_xet(&mut v);
        assert_eq!(v, serde_json::json!([{"path": "a", "lfs": {"oid": "o"}}]));
    }

    #[test]
    fn restricted_repositories_are_recognised() {
        assert!(!restricted(r#"{"gated": false, "private": false}"#));
        assert!(!restricted(r#"{}"#));
        assert!(restricted(r#"{"gated": "auto"}"#));
        assert!(restricted(r#"{"private": true}"#));
    }

    #[test]
    fn decodes_form_values() {
        assert_eq!(percent_decode("a%2Fb+c.gguf"), "a/b c.gguf");
        assert_eq!(percent_decode("100%"), "100%");
    }
}
