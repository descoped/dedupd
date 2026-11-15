//! Moving one model between the internal disk and the archive disk: archive
//! and restore, which only the owner asks for. Either can take minutes for a
//! large model, so it runs in the background, one at a time, and the event
//! stream says how far it is.

use crate::Server;
use dedupd_api::jobs::JobState;
use dedupd_api::moves::{ModelMove, MoveKind};
use dedupd_core::archive;
use std::sync::Arc;

/// The size of the files a move would deal with.
fn bytes(server: &Server, kind: MoveKind, repo: &str) -> anyhow::Result<u64> {
    let e = &server.engine;
    Ok(match kind {
        MoveKind::Archive => archive::archive_plan(&e.db, &e.store, &e.dirs, repo)?.bytes,
        MoveKind::Restore => archive::restore_plan(&e.db, &e.store, repo)?.bytes,
    })
}

fn epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Start the move in the background. Refused, with the reason, when another
/// move is running, when the model is being downloaded, or when the plan
/// cannot be made (no archive disk, a model dedupd does not have).
pub(crate) async fn start(
    server: &Arc<Server>,
    kind: MoveKind,
    repo: String,
) -> Result<(), String> {
    let busy = |s: JobState| {
        !matches!(
            s,
            JobState::Done | JobState::Cancelled | JobState::NeedsAttention | JobState::Paused
        )
    };
    if server
        .queue
        .snapshot()
        .jobs
        .iter()
        .any(|job| job.repo == repo && busy(job.state))
    {
        return Err(format!(
            "{repo} is being downloaded; wait for that to finish or cancel it first"
        ));
    }
    let total = {
        let (server, repo) = (server.clone(), repo.clone());
        tokio::task::spawn_blocking(move || bytes(&server, kind, &repo))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("{e:#}"))?
    };
    let mut refused = None;
    server.moving.send_if_modified(|state| {
        if let Some(now) = state.as_ref().filter(|s| s.running) {
            refused = Some(format!(
                "dedupd is still working on {} ({}); try again when that has finished",
                now.repo,
                now.kind.as_str()
            ));
            return false;
        }
        *state = Some(ModelMove {
            kind,
            repo: repo.clone(),
            running: true,
            started_at: epoch(),
            finished_at: None,
            bytes_total: total,
            bytes_done: 0,
            files: 0,
            kept: 0,
            error: None,
        });
        true
    });
    if let Some(why) = refused {
        return Err(why);
    }

    let server = server.clone();
    tokio::spawn(async move {
        let work = {
            let server = server.clone();
            let repo = repo.clone();
            tokio::task::spawn_blocking(move || -> anyhow::Result<(u64, u64, u64)> {
                let e = &server.engine;
                let mut on_file = |size: u64| {
                    server.moving.send_modify(|state| {
                        if let Some(s) = state {
                            s.bytes_done += size;
                        }
                    });
                };
                Ok(match kind {
                    MoveKind::Archive => {
                        let r =
                            archive::archive_apply(&e.db, &e.store, &e.dirs, &repo, &mut on_file)?;
                        (r.files, r.freed, r.keep.len() as u64)
                    }
                    MoveKind::Restore => {
                        // A restore with files that are nowhere is an error.
                        let r = archive::restore(&e.db, &e.store, &repo, &mut on_file)?;
                        (r.files, r.bytes, 0)
                    }
                })
            })
            .await
        };
        let done = match work {
            Ok(done) => done.map_err(|e| format!("{e:#}")),
            Err(e) => Err(e.to_string()),
        };
        if let Err(why) = &done {
            eprintln!("dedupd: {} of {repo} failed: {why}", kind.as_str());
        }
        server.moving.send_modify(|state| {
            if let Some(s) = state {
                s.running = false;
                s.finished_at = Some(epoch());
                match done {
                    Ok((files, bytes, kept)) => {
                        (s.files, s.bytes_done, s.kept) = (files, bytes, kept);
                    }
                    Err(why) => s.error = Some(why),
                }
            }
        });
        // The apps and the store hold something else now, also after a
        // failure part of the way.
        server.touch();
    });
    Ok(())
}
