//! Looking around on the provider, for the app: search, and everything
//! about one repository before anything is downloaded.
//!
//! These go through the daemon rather than straight from the app so that
//! they use the daemon's token, share its rate limits, and can say what is
//! already here.

use crate::control::{error, hub_error, ok};
use crate::{Call, Server, percent_decode, percent_encode, plain};
use axum::http::{Method, StatusCode, header};
use axum::response::Response;
use dedupd_core::db::PROVIDER_HF;
use dedupd_core::fetch::encode_path;
use dedupd_core::hub::{HubError, ModelMeta, TreeEntry, next_link};
use dedupd_core::library;
use dedupd_core::profile::Installed;
use serde_json::{Value, json as j};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a search answer is reused.
const SEARCH_TTL: Duration = Duration::from_secs(180);
/// Search answers kept at once.
const SEARCH_KEPT: usize = 64;
/// Results per page unless the caller says.
const PAGE: usize = 40;
/// The most of a README that is fetched.
const README_LIMIT: u64 = 512 * 1024;
/// What a search result carries about each model.
const EXPAND: [&str; 11] = [
    "author",
    "createdAt",
    "downloads",
    "gated",
    "gguf",
    "lastModified",
    "library_name",
    "likes",
    "pipeline_tag",
    "safetensors",
    "tags",
];

/// The provider's query for what the caller asked.
fn search_query(call: &Call) -> String {
    let mut query = Vec::new();
    let mut add = |name: &str, value: &str| {
        if !value.is_empty() {
            query.push(format!("{name}={}", percent_encode(value)));
        }
    };
    let param = |name: &str| call.param(name).unwrap_or_default();
    add("search", param("q").trim());
    add("author", param("author").trim());
    add("pipeline_tag", param("task").trim());
    match param("format").as_str() {
        "" | "any" => {}
        format => add("filter", format),
    }
    let license = param("license");
    if !license.is_empty() {
        add("filter", &format!("license:{license}"));
    }
    let base = param("base_model");
    if !base.is_empty() {
        add("filter", &format!("base_model:{base}"));
    }
    let sort = match param("sort").as_str() {
        "likes" => "likes",
        "modified" => "lastModified",
        "created" => "createdAt",
        "trending" => "trendingScore",
        _ => "downloads",
    };
    add("sort", sort);
    add("direction", "-1");
    let limit = param("limit").parse().unwrap_or(PAGE).clamp(1, 100);
    add("limit", &limit.to_string());
    add("cursor", &param("cursor"));
    query.extend(EXPAND.iter().map(|field| format!("expand[]={field}")));
    query.join("&")
}

/// One model of a search answer: what the provider says, and what is here.
fn hit(info: &Value, here: &BTreeMap<String, Vec<String>>, busy: &[String]) -> Option<Value> {
    let id = info.get("id").and_then(|v| v.as_str())?;
    Some(j!({
        "id": id,
        "meta": ModelMeta::from_info(info),
        "archived": here.contains_key(id),
        "apps": here.get(id).cloned().unwrap_or_default(),
        "downloading": busy.iter().any(|repo| repo == id),
    }))
}

/// The repositories that have a download in the queue.
fn downloading(server: &Server) -> Vec<String> {
    server
        .queue
        .snapshot()
        .jobs
        .into_iter()
        .filter(|job| !job.state.is_finished())
        .map(|job| job.repo)
        .collect()
}

async fn search(server: &Arc<Server>, call: &Call) -> Response {
    let query = search_query(call);
    let cached = {
        let mut kept = server.searches.lock().expect("lock");
        kept.retain(|_, (at, _)| at.elapsed() < SEARCH_TTL);
        kept.get(&query).map(|(_, answer)| answer.clone())
    };
    let answer = match cached {
        Some(answer) => answer,
        None => {
            let resp = match server
                .engine
                .hub
                .api(&format!("/api/models?{query}"), "the search")
                .await
            {
                Ok(resp) => resp,
                Err(e) => return hub_error(e),
            };
            // The next page is named by a cursor; hand that on, not the
            // provider's own address.
            let next = resp
                .headers()
                .get(header::LINK)
                .and_then(|v| v.to_str().ok())
                .and_then(next_link)
                .and_then(|url| {
                    let query = url.split_once('?')?.1.to_string();
                    let cursor = query.split('&').find_map(|p| p.strip_prefix("cursor="))?;
                    Some(percent_decode(cursor))
                });
            let models = match resp.json::<Vec<Value>>().await {
                Ok(models) => models,
                Err(e) => return hub_error(HubError::from(e)),
            };
            let answer = j!({"models": models, "next": next});
            let mut kept = server.searches.lock().expect("lock");
            if kept.len() >= SEARCH_KEPT {
                kept.clear();
            }
            kept.insert(query, (Instant::now(), answer.clone()));
            answer
        }
    };
    // What is here changes while a search answer is kept, so it is added
    // each time.
    let here = match library::presence(&server.engine.db()) {
        Ok(here) => here,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    };
    let busy = downloading(server);
    let models: Vec<Value> = answer["models"]
        .as_array()
        .map(|list| list.iter().filter_map(|m| hit(m, &here, &busy)).collect())
        .unwrap_or_default();
    ok(j!({"models": models, "next": answer["next"]}))
}

/// The files the model info lists, as the rest of dedupd knows files.
fn siblings(info: &Value) -> Vec<TreeEntry> {
    let list = info.get("siblings").and_then(|s| s.as_array());
    list.into_iter()
        .flatten()
        .filter_map(|file| {
            Some(TreeEntry {
                path: file.get("rfilename")?.as_str()?.to_string(),
                size: file.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
                git_oid: file
                    .get("blobId")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                lfs_sha256: file
                    .get("lfs")
                    .and_then(|l| l.get("sha256"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            })
        })
        .collect()
}

/// Other repositories made from the same base model: the same model in
/// other formats and quantizations.
async fn related(server: &Arc<Server>, repo: &str, meta: &ModelMeta) -> Vec<Value> {
    let base = meta.base_model.as_deref().unwrap_or(repo);
    let mut query = format!(
        "filter={}&sort=downloads&direction=-1&limit=30",
        percent_encode(&format!("base_model:{base}"))
    );
    for field in EXPAND {
        query.push_str(&format!("&expand[]={field}"));
    }
    let api = &server.engine.hub;
    let Ok(resp) = api
        .api(&format!("/api/models?{query}"), "related models")
        .await
    else {
        return Vec::new();
    };
    let Ok(models) = resp.json::<Vec<Value>>().await else {
        return Vec::new();
    };
    let here = library::presence(&server.engine.db()).unwrap_or_default();
    let busy = downloading(server);
    models
        .iter()
        .filter(|m| m.get("id").and_then(|v| v.as_str()) != Some(repo))
        .filter_map(|m| hit(m, &here, &busy))
        .collect()
}

async fn readme(
    server: &Arc<Server>,
    repo: &str,
    commit: &str,
    files: &[TreeEntry],
) -> Option<String> {
    let file = files
        .iter()
        .find(|f| f.path.eq_ignore_ascii_case("README.md"))?;
    if file.size > README_LIMIT {
        return None;
    }
    let path = format!(
        "/{}/resolve/{commit}/{}",
        encode_path(repo),
        encode_path(&file.path)
    );
    server
        .engine
        .hub
        .api(&path, repo)
        .await
        .ok()?
        .text()
        .await
        .ok()
}

async fn model(server: &Arc<Server>, repo: &str) -> Response {
    let engine = &server.engine;
    let path = format!("/api/models/{}?blobs=true", encode_path(repo));
    let live = match engine.hub.api(&path, repo).await {
        Ok(resp) => resp.text().await.map_err(HubError::from),
        Err(e) => Err(e),
    };
    let (info_json, files, commit, from_archive) = match live {
        Ok(text) => {
            let info: Value = serde_json::from_str(&text).unwrap_or_default();
            let commit = info.get("sha").and_then(|v| v.as_str()).map(str::to_string);
            let _ = engine.db().set_upstream(PROVIDER_HF, repo, false);
            (text, siblings(&info), commit, false)
        }
        Err(why) => {
            if matches!(why, HubError::NotAvailable { .. }) {
                let _ = engine.db().set_upstream(PROVIDER_HF, repo, true);
            }
            // What is archived answers when the provider cannot.
            let snapshot = {
                let db = engine.db();
                db.repo_id(PROVIDER_HF, repo)
                    .ok()
                    .flatten()
                    .and_then(|id| db.archived_snapshot(id, "main").ok().flatten())
            };
            match snapshot {
                Some(s) => (s.info_json, s.entries, Some(s.commit), true),
                None => return hub_error(why),
            }
        }
    };
    let info: Value = serde_json::from_str(&info_json).unwrap_or_default();
    let meta = ModelMeta::from_info(&info);

    let (related, readme) = match (&commit, from_archive) {
        (Some(commit), false) => tokio::join!(
            related(server, repo, &meta),
            readme(server, repo, commit, &files)
        ),
        _ => (Vec::new(), None),
    };
    let local = {
        let db = engine.db();
        let installed = Installed::detect(&engine.dirs);
        let variants =
            library::local_variants(&db, &engine.store, repo, &files, &meta.formats, &installed);
        let detail = library::model(&db, &engine.store, repo, &installed);
        variants.and_then(|v| Ok((v, detail?)))
    };
    let (variants, detail) = match local {
        Ok(local) => local,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    };
    let stored: BTreeMap<&str, bool> = detail
        .iter()
        .flat_map(|d| d.revisions.iter())
        .filter(|r| Some(&r.commit) == commit.as_ref())
        .flat_map(|r| r.files.iter().map(|f| (f.path.as_str(), f.stored)))
        .collect();
    let busy = downloading(server);
    ok(j!({
        "id": repo,
        "commit": commit,
        "from_archive": from_archive,
        "meta": meta,
        "files": files.iter().map(|f| j!({
            "path": f.path,
            "size": f.size,
            "sha256": f.lfs_sha256,
            "lfs": f.lfs_sha256.is_some(),
            "stored": stored.get(f.path.as_str()).copied().unwrap_or(false),
        })).collect::<Vec<_>>(),
        "variants": variants,
        "related": related,
        "readme": readme.or_else(|| detail.as_ref().and_then(|d| d.readme.clone())),
        "archived": detail.is_some(),
        "apps": detail.as_ref().map(|d| d.model.apps.clone()).unwrap_or_default(),
        "downloading": busy.iter().any(|r| r == repo),
        "info_json": info_json,
    }))
}

pub(crate) async fn handle(server: &Arc<Server>, call: &Call, rest: &[&str]) -> Response {
    if call.header(header::ORIGIN).is_some() {
        return plain(
            StatusCode::FORBIDDEN,
            "the control API is not for web pages",
        );
    }
    match (&call.method, rest) {
        (&Method::GET, ["search"]) => search(server, call).await,
        (&Method::GET, ["models", org, name]) => {
            // The name goes into the provider's address and into the
            // database, so it is a repository's name or nothing: no part of
            // it can climb to another path on the provider.
            let repo = percent_decode(&format!("{org}/{name}"));
            if !dedupd_core::pull::is_repo_name(&repo) {
                return plain(StatusCode::BAD_REQUEST, "that is not a model's name");
            }
            model(server, &repo).await
        }
        _ => plain(StatusCode::NOT_FOUND, "no such control endpoint"),
    }
}
