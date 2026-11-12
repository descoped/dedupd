//! The Hugging Face Hub API, as `huggingface_hub`, MLX tools and llama.cpp
//! use it.

use crate::proxy::{
    is_down, mirror_url, pass_through, relay, relay_head, relay_or_unreachable, unreachable,
    upstream,
};
use crate::{Call, Server, files, json, plain};
use axum::body::Body;
use axum::http::{StatusCode, header};
use axum::response::Response;
use dedupd_core::adopt::PROVIDER_HF;
use dedupd_core::db::is_commit;
use dedupd_core::fetch::Unwatched;
use dedupd_core::hub::{HubError, Snapshot, TreeEntry};
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
        let found = self.engine.db().archived(PROVIDER_HF, repo, rev);
        found.ok()?.map(|(_, snap)| snap)
    }

    /// Find out what `repo@rev` contains: from upstream when it answers, from
    /// the archive when it cannot or when the repository is gone. `None` means
    /// dedupd has nothing to say and the request should be passed through.
    pub(crate) async fn revision(&self, repo: &str, rev: &str, auth: Option<&str>) -> Option<Rev> {
        let archived = self.archived(repo, rev);
        let open = archived.as_ref().is_some_and(|s| !restricted(&s.info_json));
        // A commit never changes; a branch is trusted for a short while.
        let fresh = open
            && (is_commit(rev)
                || self
                    .commits
                    .lock()
                    .expect("lock")
                    .get(&(repo.to_string(), rev.to_string()))
                    .is_some_and(|(_, at)| at.elapsed() < REF_TTL));
        let archived = match archived {
            Some(snap) if fresh => {
                return Some(Rev {
                    snap,
                    from_archive: false,
                });
            }
            other => other,
        };

        match self.engine.hub.snapshot_as(repo, rev, auth).await {
            Ok(snap) => {
                let archived = self
                    .engine
                    .db()
                    .archive_snapshot(PROVIDER_HF, repo, rev, &snap);
                if let Err(e) = archived {
                    eprintln!("dedupd: archiving {repo}@{rev}: {e:#}");
                }
                self.commits.lock().expect("lock").insert(
                    (repo.to_string(), rev.to_string()),
                    (snap.commit.clone(), Instant::now()),
                );
                Some(Rev {
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
                    Some(Rev {
                        snap,
                        from_archive: true,
                    })
                }
                _ => None,
            },
            // Upstream is down or unreachable: the archive answers.
            Err(e) => archived.map(|snap| {
                eprintln!("dedupd: {repo}@{rev}: {e}; using the archived copy");
                Rev {
                    snap,
                    from_archive: true,
                }
            }),
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

/// Branch and tag names, which llama.cpp asks for first. Upstream's answer is
/// relayed; when it cannot answer, the names archived for the repository are.
async fn refs(server: &Arc<Server>, call: &Call, repo: &str) -> Response {
    let answer = upstream(server, call, &mirror_url(server, call), false).await;
    if is_down(&answer) {
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
            eprintln!("dedupd: {repo}: branch names answered from the kept copy");
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
    relay_or_unreachable(server, call, answer)
}

/// Model info. Upstream's answer is relayed as it is; the archive answers
/// when upstream cannot.
async fn info(server: &Arc<Server>, call: &Call, repo: &str, rev: &str) -> Response {
    let answer = upstream(server, call, &mirror_url(server, call), false).await;
    if is_down(&answer)
        && let Some(found) = server
            .revision(repo, rev, call.header(header::AUTHORIZATION))
            .await
    {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
            .header("x-repo-commit", &found.snap.commit)
            .body(call.body_unless_head(found.snap.info_json))
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
        .map(form_decode)
        .collect()
}

/// Decode a form value, where `+` is a space and `%2B` a plus sign.
fn form_decode(value: &str) -> String {
    percent_decode(&value.replace('+', " "))
}

/// Decode `%XX`. A `+` stays a plus sign, as in a URL path.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
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
    let answer = upstream(server, call, &mirror_url(server, call), true).await;
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
                .body(call.body_unless_head(body))
                .expect("valid response")
        }
        other => {
            if is_down(&other)
                && let Some(found) = server
                    .revision(repo, rev, call.header(header::AUTHORIZATION))
                    .await
                && let Some(listed) = archived_listing(call, &found.snap, tree_path)
            {
                return listed;
            }
            relay_or_unreachable(server, call, other)
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
            let recursive = call
                .query_params("recursive")
                .any(|v| matches!(v, "true" | "1" | "True"));
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

/// The answer to a `HEAD`: what the file is, without its bytes.
fn head_response(entry: &TreeEntry, extra: &[(&str, String)]) -> Response {
    let mut out = Response::builder()
        .status(StatusCode::OK)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, entry.size)
        .header(header::CONTENT_TYPE, "application/octet-stream");
    for (k, v) in extra {
        out = out.header(*k, v);
    }
    out.body(Body::empty()).expect("valid response")
}

/// A small file: fetch it whole, check it, then serve it.
async fn fetch_small(
    server: &Server,
    call: &Call,
    entry: &TreeEntry,
    url: &str,
    extra: &[(&str, String)],
) -> Response {
    let engine = &server.engine;
    let fetched = engine
        .fetcher
        .fetch(
            &engine.store,
            url,
            Some(entry.size),
            &entry.expect(),
            &Unwatched,
        )
        .await;
    match fetched {
        Ok(got) => {
            let _ = engine
                .db()
                .upsert_blob(&got.sha256, got.size, got.git_oid.as_deref());
            let file = engine.store.blob_path(&got.sha256);
            files::serve_file(call, &file, got.size, extra).await
        }
        Err(_) => pass_through(server, call).await,
    }
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
    let Some(found) = server.revision(repo, rev, auth).await else {
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
    let extra = [
        ("x-repo-commit", found.snap.commit.clone()),
        ("etag", format!("\"{}\"", entry.etag())),
    ];
    if call.is_head() {
        return head_response(entry, &extra);
    }

    // In the store, or on the archive disk?
    let known = server.engine.db().blob_for_entry(entry).ok().flatten();
    if let Some(blob) = known
        && let Some(served) =
            files::serve_stored(server, call, &blob.sha256, Some(blob.size), &extra).await
    {
        return served;
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
    let Some(sha) = &entry.lfs_sha256 else {
        return fetch_small(server, call, entry, &url, &extra).await;
    };
    // A large file: stream it to the client and keep the bytes.
    match upstream(server, call, &url, true).await {
        Ok(resp) if matches!(resp.status().as_u16(), 200 | 206) => {
            files::tee_response(server, resp, sha, Some("application/octet-stream"), &extra)
        }
        other => relay_or_unreachable(server, call, other),
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
        assert_eq!(form_decode("a%2Fb+c.gguf"), "a/b c.gguf");
        assert_eq!(form_decode("100%"), "100%");
        // In a URL path a plus sign is part of the file name.
        assert_eq!(percent_decode("a%20b+c.gguf"), "a b+c.gguf");
    }
}
