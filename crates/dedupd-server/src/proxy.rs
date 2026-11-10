//! Passing requests through to Hugging Face, and relaying its answers.

use crate::{Call, Server, plain};
use axum::body::Body;
use axum::http::{HeaderName, StatusCode, header};
use axum::response::Response;

/// Request headers that belong to the connection, not the request.
const HOP: [&str; 6] = [
    "host",
    "connection",
    "content-length",
    "transfer-encoding",
    "keep-alive",
    "upgrade",
];

/// Response headers that must not reach the client. The Xet ones would send
/// it to fetch model bytes from another server, past dedupd.
const DROP: [&str; 8] = [
    "connection",
    "transfer-encoding",
    "keep-alive",
    "strict-transport-security",
    "alt-svc",
    "x-xet-hash",
    "x-xet-refresh-route",
    "x-xet-cas-url",
];

/// Send the client's request to `url` upstream. With `identity`, ask for an
/// uncompressed body, for answers dedupd has to read.
pub(crate) async fn upstream(
    server: &Server,
    call: &Call,
    url: &str,
    identity: bool,
) -> Result<reqwest::Response, reqwest::Error> {
    send(server.engine.fetcher.http(), server, call, url, identity).await
}

async fn send(
    http: &reqwest::Client,
    server: &Server,
    call: &Call,
    url: &str,
    identity: bool,
) -> Result<reqwest::Response, reqwest::Error> {
    let fetcher = &server.engine.fetcher;
    let mut req = http.request(call.method.clone(), url);
    let mut has_auth = false;
    for (name, value) in &call.headers {
        let n = name.as_str();
        if HOP.contains(&n) || (identity && n == "accept-encoding") {
            continue;
        }
        if n == "authorization" {
            // Credentials go to the configured endpoint only.
            if !url.starts_with(fetcher.endpoint()) {
                continue;
            }
            has_auth = true;
        }
        req = req.header(name, value);
    }
    if !has_auth
        && url.starts_with(fetcher.endpoint())
        && let Some(auth) = fetcher.authorization()
    {
        req = req.header(header::AUTHORIZATION, auth);
    }
    if !call.body.is_empty() {
        req = req.body(call.body.clone());
    }
    req.send().await
}

/// Rewrite a `Link` header: drop the Xet relations and point pagination at us.
fn rewrite_link(value: &str, endpoint: &str, origin: &str) -> Option<String> {
    let kept: Vec<String> = value
        .split(',')
        .map(str::trim)
        // Only the relation names Xet; a repository's own name may contain it.
        .filter(|part| {
            let params = part.split_once('>').map_or(*part, |(_, params)| params);
            !params.to_lowercase().contains("xet")
        })
        .map(|part| part.replace(endpoint, origin))
        .collect();
    (!kept.is_empty()).then(|| kept.join(", "))
}

/// Start a response that carries upstream's status and headers, cleaned.
pub(crate) fn relay_head(
    server: &Server,
    call: &Call,
    resp: &reqwest::Response,
    keep_length: bool,
) -> axum::http::response::Builder {
    let mut out = Response::builder().status(resp.status().as_u16());
    for (name, value) in resp.headers() {
        let n = name.as_str();
        if DROP.contains(&n) || (!keep_length && (n == "content-length" || n == "content-encoding"))
        {
            continue;
        }
        if n == "link" {
            if let Some(v) = value
                .to_str()
                .ok()
                .and_then(|v| rewrite_link(v, server.engine.fetcher.endpoint(), &call.origin))
            {
                out = out.header(HeaderName::from_static("link"), v);
            }
            continue;
        }
        if n == "location" {
            // A redirect within the endpoint stays with us; one to a CDN is
            // the client's to follow.
            let endpoint = server.engine.fetcher.endpoint();
            match value.to_str().ok().and_then(|v| v.strip_prefix(endpoint)) {
                Some(rest) if rest.starts_with('/') => {
                    out = out.header(header::LOCATION, format!("{}{rest}", call.origin));
                }
                _ => out = out.header(name, value),
            }
            continue;
        }
        out = out.header(name, value);
    }
    out
}

/// Relay upstream's answer unchanged, streaming the body.
pub(crate) fn relay(server: &Server, call: &Call, resp: reqwest::Response) -> Response {
    let head = relay_head(server, call, &resp, true);
    let body = if call.is_head() {
        Body::empty()
    } else {
        Body::from_stream(resp.bytes_stream())
    };
    head.body(body).expect("valid response")
}

pub(crate) fn unreachable(e: impl std::fmt::Display) -> Response {
    plain(
        StatusCode::BAD_GATEWAY,
        format!(
            "dedupd could not reach Hugging Face, and has no archived copy to answer with: {e}"
        ),
    )
}

/// Anything dedupd has no special handling for.
///
/// Redirects are relayed, not followed: the client reads the commit, the
/// linked ETag and the size from the redirect itself, and the place it points
/// to (a CDN) has none of them.
pub(crate) async fn pass_through(server: &Server, call: &Call) -> Response {
    let url = format!(
        "{}{}",
        server.engine.fetcher.endpoint(),
        call.path_and_query()
    );
    let http = server.engine.fetcher.http_no_redirect();
    match send(http, server, call, &url, false).await {
        Ok(resp) => relay(server, call, resp),
        Err(e) => unreachable(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_header_is_pointed_at_the_proxy_and_loses_xet() {
        let v = r#"<https://huggingface.co/api/models/o/r/tree/c?cursor=x>; rel="next", <https://cas.example/auth>; rel="xet-auth""#;
        assert_eq!(
            rewrite_link(v, "https://huggingface.co", "http://127.0.0.1:8642").unwrap(),
            r#"<http://127.0.0.1:8642/api/models/o/r/tree/c?cursor=x>; rel="next""#
        );
        assert_eq!(
            rewrite_link(r#"<https://c/a>; rel="xet-auth""#, "e", "o"),
            None
        );
        let v = r#"<https://huggingface.co/api/models/xet-team/r/tree/c?cursor=x>; rel="next""#;
        assert_eq!(
            rewrite_link(v, "https://huggingface.co", "http://127.0.0.1:8642").unwrap(),
            r#"<http://127.0.0.1:8642/api/models/xet-team/r/tree/c?cursor=x>; rel="next""#
        );
    }
}
