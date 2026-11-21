//! Moving one model between the internal disk and the archive disk: archive
//! and restore, which only the owner asks for. Either can take minutes for a
//! large model, so it runs in the background, one at a time, and the event
//! stream says how far it is.

use crate::{Server, epoch, held};
use dedupd_api::activity::ActivityAction;
use dedupd_api::moves::{ModelMove, MoveKind};
use dedupd_core::{activity, archive};
use std::sync::Arc;

/// The size of the files a move would deal with, and for an archive the
/// name of the archive it goes to.
fn bytes(
    server: &Server,
    kind: MoveKind,
    repo: &str,
    to: Option<&str>,
) -> anyhow::Result<(u64, Option<String>)> {
    let e = &server.engine;
    Ok(match kind {
        MoveKind::Archive => {
            let plan = archive::archive_plan(&e.db, &e.store, &e.dirs, repo, to)?;
            (plan.bytes, Some(plan.archive_name))
        }
        MoveKind::Restore => (archive::restore_plan(&e.db, &e.store, repo)?.bytes, None),
    })
}

/// Why another move cannot start while `now` runs.
fn still_working(now: &ModelMove) -> String {
    format!(
        "dedupd is still working on {} ({}); try again when that has finished",
        now.repo,
        now.kind.as_str()
    )
}

/// Why a job for `model` (a spec as the queue takes it) is not queued while
/// `moving` is the move the daemon is doing or did last: the job would
/// fetch or place the files the move is taking away or bringing back.
pub(crate) fn refuses_job(moving: Option<&ModelMove>, model: &str) -> Option<String> {
    let now = moving.filter(|m| m.running)?;
    let (repo, ..) = dedupd_core::names::parse_model(model).ok()?;
    (repo == now.repo).then(|| still_working(now))
}

/// Start the move in the background. Refused, with the reason, when another
/// move is running, when the model is being downloaded, or when the plan
/// cannot be made (no archive disk, a model dedupd does not have). A
/// refusal goes into the activity list too, as the move's failure, so the
/// list tells every archive and restore the owner asked for.
pub(crate) async fn start(
    server: &Arc<Server>,
    kind: MoveKind,
    repo: String,
    to: Option<String>,
) -> Result<(), String> {
    let started = begin(server, kind, repo.clone(), to).await;
    if let Err(why) = &started
        && dedupd_core::names::is_repo_name(&repo)
    {
        let action = match kind {
            MoveKind::Archive => ActivityAction::Archive,
            MoveKind::Restore => ActivityAction::Restore,
        };
        let (home, why) = (server.engine.store.home().clone(), why.clone());
        let _ = tokio::task::spawn_blocking(move || {
            activity::record(
                &home,
                activity::Happened {
                    action,
                    repo,
                    app: None,
                    result: Err(why),
                },
            );
        })
        .await;
        server.touch();
    }
    started
}

async fn begin(
    server: &Arc<Server>,
    kind: MoveKind,
    repo: String,
    to: Option<String>,
) -> Result<(), String> {
    let (total, archive_name) = {
        let (server, repo, to) = (server.clone(), repo.clone(), to.clone());
        tokio::task::spawn_blocking(move || bytes(&server, kind, &repo, to.as_deref()))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("{e:#}"))?
    };
    let _starting = held(&server.starting);
    // The same rule archive_apply and restore keep, asked first so the
    // owner hears it at once rather than as a failed move. A job added
    // after this waits for the move: the queue asks `moving` first.
    // One short question to the database, on this thread because the lock
    // above must cover it and the start below.
    if server
        .engine
        .db()
        .has_unfinished_job(&repo)
        .map_err(|e| format!("{e:#}"))?
    {
        return Err(archive::busy(&repo));
    }
    let mut refused = None;
    server.moving.send_if_modified(|state| {
        if let Some(now) = state.as_ref().filter(|s| s.running) {
            refused = Some(still_working(now));
            return false;
        }
        *state = Some(ModelMove {
            kind,
            repo: repo.clone(),
            archive: archive_name.clone(),
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
                        let r = archive::archive_apply(
                            &e.db,
                            &e.store,
                            &e.dirs,
                            &repo,
                            archive_name.as_deref(),
                            &mut on_file,
                        )?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn moving(repo: &str, running: bool) -> ModelMove {
        ModelMove {
            kind: MoveKind::Archive,
            repo: repo.into(),
            archive: None,
            running,
            started_at: 0,
            finished_at: None,
            bytes_total: 0,
            bytes_done: 0,
            files: 0,
            kept: 0,
            error: None,
        }
    }

    #[test]
    fn no_job_is_queued_for_a_model_that_is_moving() {
        let now = moving("org/Repo", true);
        for spec in ["org/Repo", "org/Repo:Q4_K_M", "hf.co/org/Repo@main"] {
            let why = refuses_job(Some(&now), spec).unwrap();
            assert!(why.contains("org/Repo (archive)"), "{why}");
        }
        assert_eq!(refuses_job(Some(&now), "org/Other:Q4_K_M"), None);
        assert_eq!(
            refuses_job(Some(&moving("org/Repo", false)), "org/Repo"),
            None
        );
        assert_eq!(refuses_job(None, "org/Repo"), None);
    }
}
