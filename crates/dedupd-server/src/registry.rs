//! The Ollama registry API, for `ollama pull --insecure 127.0.0.1:PORT/org/repo:tag`.

use crate::proxy::{unreachable, upstream};
use crate::{Call, Server, files, plain};
use axum::body::Body;
use axum::http::{StatusCode, header};
use axum::response::Response;
use dedupd_core::db::PROVIDER_HF;
use dedupd_core::ollama::{MANIFEST_TYPE, Manifest};
use sha2::{Digest, Sha256};
use std::sync::Arc;

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
    match rest {
        [name @ .., "manifests", tag] if !name.is_empty() => {
            manifest(server, call, &name.join("/"), tag).await
        }
        [name @ .., "blobs", digest] if !name.is_empty() => {
            blob(server, call, &name.join("/"), digest).await
        }
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
            let first = {
                let db = server.engine.db();
                if let Ok(id) = db.upsert_repo(PROVIDER_HF, name) {
                    let _ = db.save_manifest(id, tag, &user_agent, &media_type, &body);
                }
                matches!(db.archived(PROVIDER_HF, name, "main"), Ok(None))
            };
            if first {
                keep_model_info(server, name, call.header(header::AUTHORIZATION));
            }
        }
        return manifest_response(call, &media_type, body);
    }

    // The Hub said no to this client, or the repository is gated or
    // private: its answer stands, as for the Hub API. Otherwise (gone,
    // down, out of reach) the archived manifest answers. Whether the model
    // is gated or private is read from the Hub's info kept for it; a model
    // with no info kept counts as restricted, since nothing says this
    // client may have it.
    let refused = answer
        .as_ref()
        .is_ok_and(|r| matches!(r.status().as_u16(), 401 | 403));
    let archived = {
        let db = server.engine.db();
        let restricted = db
            .archived(PROVIDER_HF, name, "main")
            .ok()
            .flatten()
            .is_none_or(|(_, snap)| crate::hubapi::restricted(&snap.info_json));
        db.repo_id(PROVIDER_HF, name)
            .ok()
            .flatten()
            .filter(|_| !refused && !restricted)
            .and_then(|id| db.manifest(id, tag, Some(&user_agent)).ok().flatten())
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
    let (server, name) = (server.clone(), name.to_string());
    let auth = auth.map(str::to_string);
    tokio::spawn(async move {
        let hub = &server.engine.hub;
        if let Ok(snap) = hub.snapshot_as(&name, "main", auth.as_deref()).await {
            let _ = server
                .engine
                .db()
                .archive_snapshot(PROVIDER_HF, &name, "main", &snap);
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
        let known = server.engine.db().blob(sha).ok().flatten();
        return match known.filter(|_| open_to_all(server, name, sha)) {
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
    let name = call.query_params("repo").next().unwrap_or("");
    send_blob(server, call, name, sha).await
}

/// Whether a stored layer may be answered without asking the Hub: the kept
/// info of the model the client names says it is open, and that model is
/// known to hold the layer. A model with no info kept, and a layer asked for
/// under another model's name, count as restricted.
fn open_to_all(server: &Server, name: &str, sha: &str) -> bool {
    let db = server.engine.db();
    let open = db
        .archived(PROVIDER_HF, name, "main")
        .ok()
        .flatten()
        .is_some_and(|(_, snap)| !crate::hubapi::restricted(&snap.info_json));
    open && db
        .repo_id(PROVIDER_HF, name)
        .ok()
        .flatten()
        .is_some_and(|id| db.repo_refers_to(id, sha).unwrap_or(false))
}

/// For a layer that is not open to all: ask the Hub, as this client, whether
/// it may have it. `Some` is the answer that says no.
async fn refused(server: &Arc<Server>, call: &Call, name: &str, sha: &str) -> Option<Response> {
    if open_to_all(server, name, sha) {
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
    match upstream(server, &ask, &url, true).await {
        Ok(resp) if resp.status().is_success() => None,
        other => Some(relay_or_unreachable(server, call, other)),
    }
}

async fn send_blob(server: &Arc<Server>, call: &Call, name: &str, sha: &str) -> Response {
    let extra = [("docker-content-digest", format!("sha256:{sha}"))];
    // A gated or private model's layer is the Hub's to hand out: its sha256
    // is no secret, so holding the bytes is not reason enough to serve them.
    let held = server.engine.db().blob(sha).ok().flatten().is_some();
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
            files::tee_response(server, resp, sha, None, &extra)
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
