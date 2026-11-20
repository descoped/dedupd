//! The Ollama registry API, for `ollama pull --insecure 127.0.0.1:PORT/org/repo:tag`.

use crate::proxy::{unreachable, upstream, upstream_unfollowed};
use crate::{Call, Server, files, plain};
use axum::body::Body;
use axum::http::{StatusCode, header};
use axum::response::Response;
use dedupd_core::db::PROVIDER_HF;
use dedupd_core::ollama::{MANIFEST_TYPE, Manifest};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long the Hub's yes for a layer is taken as still standing. Ollama
/// downloads one layer as many parts in a row; each is a request here.
const PERMIT_FOR: Duration = Duration::from_secs(60);

/// The layers the Hub said yes to lately, by the model named, the layer and
/// the `Authorization` it was asked with.
#[derive(Default)]
pub(crate) struct Permits(Mutex<HashMap<(String, String, String), Instant>>);

impl Permits {
    fn key(call: &Call, name: &str, sha: &str) -> (String, String, String) {
        let auth = call.header(header::AUTHORIZATION).unwrap_or("");
        (name.to_string(), sha.to_string(), auth.to_string())
    }

    fn holds(&self, call: &Call, name: &str, sha: &str) -> bool {
        let map = self.0.lock().unwrap_or_else(|e| e.into_inner());
        map.get(&Self::key(call, name, sha))
            .is_some_and(|at| at.elapsed() < PERMIT_FOR)
    }

    fn note(&self, call: &Call, name: &str, sha: &str) {
        let mut map = self.0.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, at| at.elapsed() < PERMIT_FOR);
        map.insert(Self::key(call, name, sha), Instant::now());
    }

    /// Forget every yes, so the next request asks the Hub again.
    pub(crate) fn clear(&self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

/// Relay a registry answer. The token challenge is removed: it names
/// Hugging Face as the place to get a token, and Ollama refuses a challenge
/// whose host is not the registry it is talking to.
fn relay_or_unreachable(
    server: &Server,
    call: &Call,
    answer: Result<reqwest::Response, reqwest::Error>,
) -> Response {
    let mut out = crate::proxy::relay_or_unreachable(server, call, answer);
    out.headers_mut().remove(header::WWW_AUTHENTICATE);
    out
}

/// `/v2/{name}/manifests/{tag}` and `/v2/{name}/blobs/{digest}`.
pub(crate) async fn handle(server: &Arc<Server>, call: &Call, rest: &[&str]) -> Response {
    // The name and the tag go into the provider's address and into the
    // database, so they are a model's name and a tag, or nothing is asked.
    let named = |name: &[&str]| {
        let name = name.join("/");
        dedupd_core::names::is_repo_name(&name).then_some(name)
    };
    match rest {
        [name @ .., "manifests", tag] => match named(name) {
            Some(name) if dedupd_core::names::is_tag(tag) => {
                manifest(server, call, &name, tag).await
            }
            _ => plain(StatusCode::NOT_FOUND, "that is not a model and a tag"),
        },
        [name @ .., "blobs", digest] => match named(name) {
            Some(name) => blob(server, call, &name, digest).await,
            None => plain(StatusCode::NOT_FOUND, "that is not a model's name"),
        },
        _ => plain(StatusCode::NOT_FOUND, "not a registry path dedupd serves"),
    }
}

fn manifest_response(call: &Call, media_type: &str, body: String) -> Response {
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(body.as_bytes())));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, media_type)
        .header(header::CONTENT_LENGTH, body.len())
        .header("docker-content-digest", digest)
        .body(call.body_unless_head(body))
        .expect("valid response")
}

/// The manifest Hugging Face generates for a GGUF repository. Its content
/// depends on which client asks, so it is archived per User-Agent.
async fn manifest(server: &Arc<Server>, call: &Call, name: &str, tag: &str) -> Response {
    let user_agent = call.header(header::USER_AGENT).unwrap_or("").to_string();
    let url = format!(
        "{}/v2/{name}/manifests/{tag}",
        server.engine.fetcher.endpoint()
    );
    // Always a GET upstream, so a HEAD also archives the manifest.
    let get = Call {
        method: axum::http::Method::GET,
        body: Default::default(),
        ..call.clone()
    };
    let answer = upstream(server, &get, &url, true).await;
    if let Ok(resp) = &answer
        && resp.status().is_success()
    {
        let media_type = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or(MANIFEST_TYPE)
            .to_string();
        let resp = answer.expect("checked above");
        let Ok(body) = resp.text().await else {
            return unreachable("the manifest was cut short");
        };
        if Manifest::parse(&body).is_ok() {
            let kept = (
                name.to_string(),
                tag.to_string(),
                user_agent.clone(),
                media_type.clone(),
                body.clone(),
            );
            let first = server
                .off(move |e| -> anyhow::Result<bool> {
                    let (name, tag, user_agent, media_type, body) = kept;
                    let db = e.db();
                    let id = db.upsert_repo(PROVIDER_HF, &name)?;
                    db.save_manifest(id, &tag, &user_agent, &media_type, &body)?;
                    Ok(db.archived(PROVIDER_HF, &name, "main")?.is_none())
                })
                .await;
            match first {
                Ok(true) => keep_model_info(server, name, call.header(header::AUTHORIZATION)),
                Ok(false) => {}
                // Hugging Face makes this manifest; it exists nowhere else.
                Err(e) => eprintln!("dedupd: the manifest of {name}:{tag} was not kept: {e:#}"),
            }
        }
        return manifest_response(call, &media_type, body);
    }

    // The Hub did not give the manifest (gone, hidden, down, out of
    // reach). A model that is gated or private is the Hub's to answer for,
    // so its answer stands. A model known to be open is answered from the
    // kept manifest, as the Hub API front end does: the Hub says 401 for a
    // repository that no longer exists when the request has no token, so a
    // 401 or 403 for an open model means "gone", not "not for you". The
    // rule is `Server::known_open`, the same for both front ends.
    let archived = if server.known_open(name).await {
        let asked = (name.to_string(), tag.to_string(), user_agent);
        server
            .off(move |e| {
                let (name, tag, user_agent) = asked;
                let db = e.db();
                match db.repo_id(PROVIDER_HF, &name)? {
                    Some(id) => db.manifest(id, &tag, Some(&user_agent)),
                    None => Ok(None),
                }
            })
            .await
            .unwrap_or_else(|e| {
                eprintln!("dedupd: reading the kept manifest of {name}:{tag}: {e:#}");
                None
            })
    } else {
        None
    };
    match archived {
        Some((media_type, body)) => manifest_response(call, &media_type, body),
        None => relay_or_unreachable(server, call, answer),
    }
}

/// Keep the Hub's model info for a model that came in through the
/// registry only, in the background. Whether it is gated or private is
/// read from it, so its kept manifest is not served while the Hub cannot
/// be asked whether the client may have it. It is asked with the client's
/// own `Authorization` when it sent one (the Hub client sends it to the
/// Hub endpoint only), so a gated model the client may have and the
/// daemon's token may not is still read; with none, the daemon's token.
fn keep_model_info(server: &Arc<Server>, name: &str, auth: Option<&str>) {
    if !dedupd_core::names::is_repo_name(name) {
        return;
    }
    let (asking, name) = (server.clone(), name.to_string());
    let auth = auth.map(str::to_string);
    server.background(async move {
        // Until it is kept the model counts as restricted, and the next
        // manifest asks again: said in the log, and nothing more to do.
        let snap = match asking
            .engine
            .hub
            .snapshot_as(&name, "main", auth.as_deref())
            .await
        {
            Ok(snap) => snap,
            Err(e) => {
                eprintln!("dedupd: the info of {name} was not read: {e}");
                return;
            }
        };
        let repo = name.clone();
        let kept = asking
            .off(move |e| e.db().archive_snapshot(PROVIDER_HF, &repo, "main", &snap))
            .await;
        if let Err(e) = kept {
            eprintln!("dedupd: the info of {name} was not kept: {e:#}");
        }
    });
}

/// A layer. Ollama first asks without `Range` and expects a redirect; it then
/// downloads from the redirect target in parallel ranges.
async fn blob(server: &Arc<Server>, call: &Call, name: &str, digest: &str) -> Response {
    let Some(sha) = dedupd_core::ollama::digest_hex(digest) else {
        return plain(StatusCode::BAD_REQUEST, "only sha256 digests are served");
    };
    if call.is_head() {
        let known = match stored(server, sha).await {
            Some(blob) if open_to_all(server, name, sha).await => Some(blob),
            _ => None,
        };
        return match known {
            Some(b) => Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_LENGTH, b.size)
                .header(header::ACCEPT_RANGES, "bytes")
                .header("docker-content-digest", digest)
                .body(Body::empty())
                .expect("valid response"),
            None => {
                let url = server.engine.fetcher.registry_blob_url(name, sha);
                relay_or_unreachable(server, call, upstream(server, call, &url, true).await)
            }
        };
    }
    if call.header(header::RANGE).is_none() {
        return Response::builder()
            .status(StatusCode::TEMPORARY_REDIRECT)
            .header(
                header::LOCATION,
                format!(
                    "{}/dedupd/blob/sha256:{sha}?repo={name}",
                    other_host(&call.origin)
                ),
            )
            .header(header::CONTENT_LENGTH, 0)
            .body(Body::empty())
            .expect("valid response");
    }
    send_blob(server, call, name, sha).await
}

/// The same server under its other loopback name.
///
/// Ollama follows a redirect to the same hostname by itself and then fails
/// with "no Location header"; it only takes the redirect target as the
/// download URL when the hostname differs. `localhost` and `127.0.0.1` are
/// different hostnames for the same daemon.
fn other_host(origin: &str) -> String {
    let rest = origin.strip_prefix("http://").unwrap_or(origin);
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) if p.bytes().all(|b| b.is_ascii_digit()) => (h, format!(":{p}")),
        _ => (rest, String::new()),
    };
    let other = if host == "localhost" {
        "127.0.0.1"
    } else {
        "localhost"
    };
    format!("http://{other}{port}")
}

/// The redirect target: `/dedupd/blob/sha256:{hex}?repo={name}`.
pub(crate) async fn blob_direct(server: &Arc<Server>, call: &Call, digest: &str) -> Response {
    let Some(sha) = dedupd_core::ollama::digest_hex(digest) else {
        return plain(StatusCode::BAD_REQUEST, "only sha256 digests are served");
    };
    // Without a name the layer is served only when it is stored and open.
    let name = call.query_params("repo").next().unwrap_or("");
    if !name.is_empty() && !dedupd_core::names::is_repo_name(name) {
        return plain(StatusCode::NOT_FOUND, "that is not a model's name");
    }
    send_blob(server, call, name, sha).await
}

/// Whether a stored layer may be answered without asking the Hub: the kept
/// info of the model the client names says it is open, and that model is
/// known to hold the layer. A model with no info kept, and a layer asked for
/// under another model's name, count as restricted.
async fn open_to_all(server: &Server, name: &str, sha: &str) -> bool {
    if !server.known_open(name).await {
        return false;
    }
    let asked = (name.to_string(), sha.to_string());
    let refers = server
        .off(move |e| {
            let db = e.db();
            match db.repo_id(PROVIDER_HF, &asked.0)? {
                Some(id) => db.repo_refers_to(id, &asked.1),
                None => Ok(false),
            }
        })
        .await;
    // What cannot be read does not say the layer is open.
    refers.unwrap_or_else(|e| {
        eprintln!("dedupd: looking up layer {sha} of {name}: {e:#}");
        false
    })
}

/// The stored file with this sha256, as the database knows it.
async fn stored(server: &Server, sha: &str) -> Option<dedupd_core::db::BlobRow> {
    let asked = sha.to_string();
    let found = server.off(move |e| e.db().blob(&asked)).await;
    found.unwrap_or_else(|e| {
        eprintln!("dedupd: looking for {sha} in the store: {e:#}");
        None
    })
}

/// For a layer that is not open to all: ask the Hub whether it may be had,
/// with the client's own `Authorization`, or with dedupd's token when the
/// client has none (as for every read of a model). `Some` is the answer
/// that says no. The Hub's own answer decides: it is a redirect to the
/// download server, which is not followed, since that address is made for
/// the download and says nothing more about who may have the layer. A yes
/// stands for a minute.
async fn refused(server: &Arc<Server>, call: &Call, name: &str, sha: &str) -> Option<Response> {
    if open_to_all(server, name, sha).await || server.permits.holds(call, name, sha) {
        return None;
    }
    if !dedupd_core::names::is_repo_name(name) {
        return Some(plain(
            StatusCode::NOT_FOUND,
            "that layer is not served without its model's name",
        ));
    }
    let ask = Call {
        method: axum::http::Method::HEAD,
        body: Default::default(),
        ..call.clone()
    };
    let url = server.engine.fetcher.registry_blob_url(name, sha);
    match upstream_unfollowed(server, &ask, &url).await {
        Ok(resp) if resp.status().is_success() || resp.status().is_redirection() => {
            server.permits.note(call, name, sha);
            None
        }
        other => Some(relay_or_unreachable(server, call, other)),
    }
}

async fn send_blob(server: &Arc<Server>, call: &Call, name: &str, sha: &str) -> Response {
    let extra = [("docker-content-digest", format!("sha256:{sha}"))];
    // A gated or private model's layer is the Hub's to hand out: its sha256
    // is no secret, so holding the bytes is not reason enough to serve them.
    let held = stored(server, sha).await.is_some();
    if held && let Some(no) = refused(server, call, name, sha).await {
        return no;
    }
    if let Some(served) = files::serve_stored(server, call, sha, None, &extra).await {
        return served;
    }
    if name.is_empty() {
        return plain(StatusCode::NOT_FOUND, "that layer is not in the store");
    }
    let url = server.engine.fetcher.registry_blob_url(name, sha);
    match upstream(server, call, &url, true).await {
        Ok(resp) if matches!(resp.status().as_u16(), 200 | 206) => {
            files::tee_response(server, call, resp, sha, None, &extra)
        }
        other => relay_or_unreachable(server, call, other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_goes_to_the_other_loopback_name() {
        assert_eq!(other_host("http://127.0.0.1:8642"), "http://localhost:8642");
        assert_eq!(other_host("http://localhost:8642"), "http://127.0.0.1:8642");
        assert_eq!(other_host("http://[::1]:8642"), "http://localhost:8642");
    }
}
