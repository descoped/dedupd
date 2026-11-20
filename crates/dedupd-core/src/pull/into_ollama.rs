//! The Ollama side of a pull: the manifest Hugging Face's registry
//! generates, and putting the model into Ollama.

use super::dest;
use super::{PullRequest, ollama_mlx, profile};
use crate::engine::{Engine, blocking};
use crate::fetch::{Expect, Unwatched};
use crate::hub::{HubError, Snapshot, TreeEntry, model_meta};
use crate::ollama;
use crate::place::{self, Item, Placed};
use crate::profile::Installed;
use crate::refusals::{Refusal, Refusals};
use crate::select;
use anyhow::{Context, Result, anyhow, bail};
use dedupd_api::app::App;
use dedupd_api::jobs::NotActivated;
use dedupd_api::library::DecisionState;
use std::path::Path;
use std::sync::Arc;

/// Get the manifest Hugging Face's registry generates for `repo:tag`, archive
/// it, and make sure every layer it names is in the store. Returns the
/// manifest with its body as the registry sent it.
async fn ollama_manifest(
    engine: &Arc<Engine>,
    repo_id: i64,
    repo: &str,
    tag: &str,
    tree: &[TreeEntry],
    commit: &str,
    offline: bool,
) -> Result<(ollama::Manifest, String)> {
    let mut upstream_error = None;
    let mut fetched = None;
    if !offline {
        let url = format!("{}/v2/{repo}/manifests/{tag}", engine.fetcher.endpoint());
        let sent = engine
            .fetcher
            .get(&url)
            .header(reqwest::header::USER_AGENT, ollama::USER_AGENT)
            .header(reqwest::header::ACCEPT, ollama::MANIFEST_TYPE)
            .send()
            .await;
        match sent {
            Ok(resp) if resp.status().is_success() => {
                let media_type = resp
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or(ollama::MANIFEST_TYPE)
                    .to_string();
                // A connection lost while the answer arrives is the same
                // trouble as one that never opened: the kept manifest is
                // tried, and the failure passes.
                match resp.text().await {
                    Ok(text) => {
                        let manifest = ollama::Manifest::parse(&text)?;
                        engine.db().save_manifest(
                            repo_id,
                            tag,
                            ollama::USER_AGENT,
                            &media_type,
                            &text,
                        )?;
                        fetched = Some((manifest, text));
                    }
                    Err(e) => {
                        upstream_error = Some(
                            anyhow::Error::new(HubError::Transport(e))
                                .context("the answer from Hugging Face was cut off"),
                        )
                    }
                }
            }
            // Typed, so a caller can tell a passing trouble from an answer.
            Ok(resp) => {
                let status = resp.status().as_u16();
                upstream_error = Some(
                    anyhow::Error::new(HubError::Status { status, url }).context(format!(
                        "Hugging Face has no Ollama manifest for {repo}:{tag} (HTTP {status})"
                    )),
                )
            }
            Err(e) => {
                upstream_error = Some(
                    anyhow::Error::new(HubError::Transport(e))
                        .context("could not reach Hugging Face"),
                )
            }
        }
    }
    let (manifest, body) = match fetched {
        Some(found) => found,
        None => {
            let archived = engine
                .db()
                .manifest(repo_id, tag, Some(ollama::USER_AGENT))?;
            match archived {
                Some((_, body)) => (ollama::Manifest::parse(&body)?, body),
                None => {
                    return Err(upstream_error.unwrap_or_else(|| {
                        anyhow!("no Ollama manifest for {repo}:{tag} is archived")
                    }));
                }
            }
        }
    };

    for layer in manifest.all() {
        let sha = layer.sha256()?;
        let owned = sha.to_string();
        // In the store, or brought back from the archive disk; recorded
        // when the database does not know it yet.
        let local = blocking(engine, move |engine| {
            if !engine.store.ensure_local(&owned)? {
                return Ok(false);
            }
            if engine.db().blob(&owned)?.is_none() {
                let size = std::fs::metadata(engine.store.blob_path(&owned))?.len();
                engine.db().upsert_blob(&owned, size, None)?;
            }
            Ok(true)
        })
        .await?;
        if local {
            continue;
        }
        if offline {
            bail!("layer {} of {repo}:{tag} is not in the store", layer.digest);
        }
        // A layer that is a file of the repository comes from the repository;
        // the layers Hugging Face generates exist only in its registry.
        let url = match tree.iter().find(|e| e.lfs_sha256.as_deref() == Some(sha)) {
            Some(e) => engine.fetcher.resolve_url(repo, commit, &e.path),
            None => engine.fetcher.registry_blob_url(repo, sha),
        };
        let size = (layer.size > 0).then_some(layer.size);
        let got = engine
            .fetcher
            .fetch(
                &engine.store,
                &url,
                size,
                &Expect::Sha256(sha.to_string()),
                &Unwatched,
            )
            .await
            .with_context(|| format!("fetching layer {}", layer.digest))?;
        engine.db().upsert_blob(&got.sha256, got.size, None)?;
    }
    Ok((manifest, body))
}

/// Place the selection into Ollama. The inner `Err` is why it was not
/// placed, which does not fail the pull.
pub(super) async fn place_into_ollama(
    engine: &Arc<Engine>,
    req: &PullRequest,
    repo_id: i64,
    snap: &Snapshot,
    entries: &[TreeEntry],
    items: &Arc<Vec<Item>>,
) -> Result<Result<Placed, NotActivated>> {
    let skip = |reason: String| NotActivated::lasting(App::Ollama, reason);
    // The app's profile says whether this selection can go in, and why not.
    let weights = select::weights(entries);
    let info: serde_json::Value = serde_json::from_str(&snap.info_json).unwrap_or_default();
    let config = items
        .iter()
        .find(|i| i.rel_path == "config.json")
        .map(|i| i.sha256.clone());
    // The decision reads files (the apps' versions, the refusals kept,
    // the model's config.json), so it is made off the async runtime.
    let (ggufs, formats, asked) = (weights.len(), model_meta(&info).formats, config.clone());
    let (installed, decision) = blocking(engine, move |engine| {
        let installed = Installed::detect(&engine.dirs);
        let decision = profile::decide_for(
            &engine.store,
            &installed,
            &Refusals::load(engine.store.home()),
            ggufs,
            &formats,
            asked.as_deref(),
        )
        .of(App::Ollama)
        .clone();
        Ok((installed, decision))
    })
    .await?;
    if decision.state != DecisionState::Ready {
        return Ok(Err(skip(decision.reason)));
    }
    if decision.method.as_deref() == Some(ollama_mlx::METHOD) {
        let Some(command) = installed.command(App::Ollama).map(Path::to_path_buf) else {
            return Ok(Err(skip("Ollama's command line tool was not found".into())));
        };
        let (repo, items) = (req.repo.clone(), items.clone());
        let placed = blocking(engine, move |engine| {
            ollama_mlx::place(
                &dest(engine, repo_id, &repo),
                &command,
                ollama::DEFAULT_TAG,
                &items,
            )
        })
        .await?;
        let version = installed.version(App::Ollama);
        if let (Err(ollama_mlx::NotPlaced::Ollama(said)), Some(version), Some(config)) =
            (&placed, version, config)
        {
            // Ollama's answer holds for this model and this version; keep it,
            // so the model is not offered to it again.
            let refusal = Refusal {
                app: App::Ollama,
                version: version.to_string(),
                method: ollama_mlx::METHOD.to_string(),
                config,
                reason: said.clone(),
                lasting: true,
                at: crate::now(),
            };
            let kept = blocking(engine, move |engine| {
                Refusals::record(engine.store.home(), refusal)
            })
            .await;
            if let Err(e) = kept {
                eprintln!("dedupd: could not keep Ollama's answer: {e:#}");
            }
        }
        return Ok(placed.map_err(|why| match (&why, version) {
            (ollama_mlx::NotPlaced::Ollama(said), Some(version)) => {
                skip(crate::refusals::sentence("Ollama", version, said))
            }
            (ollama_mlx::NotPlaced::Trouble(_), _) => NotActivated {
                temporary: true,
                ..skip(why.to_string())
            },
            _ => skip(why.to_string()),
        }));
    }
    let tag = req
        .selection
        .quant
        .as_deref()
        .unwrap_or(ollama::DEFAULT_TAG);
    let found = ollama_manifest(
        engine,
        repo_id,
        &req.repo,
        tag,
        &snap.entries,
        &snap.commit,
        req.offline,
    )
    .await;
    let (manifest, body) = match found {
        Ok(m) => m,
        Err(e) => {
            // The sentence on top says it; the causes under it repeat it.
            return Ok(Err(NotActivated {
                temporary: crate::hub::is_temporary(&e),
                ..skip(e.to_string())
            }));
        }
    };
    // Without a named quantization the tag is Hugging Face's choice, which
    // need not be the file the patterns selected.
    // The profile gives Ollama a GGUF only when there is a weights file;
    // that rule is in another file, so it is not leaned on here.
    let Some(first) = weights.first() else {
        return Ok(Err(skip("the selection has no weights file".into())));
    };
    let wanted = first.lfs_sha256.as_deref();
    if !manifest.all().any(|l| l.sha256().ok() == wanted) {
        return Ok(Err(skip(format!(
            "its tag {tag} is a different file than {}; name the quantization, as in {}:Q8_0",
            first.path, req.repo
        ))));
    }
    let (repo, tag) = (req.repo.clone(), tag.to_string());
    let placed = blocking(engine, move |engine| {
        place::place_ollama(&dest(engine, repo_id, &repo), &tag, &manifest, &body)
    })
    .await?;
    Ok(Ok(placed))
}
