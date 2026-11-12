//! Moving one model between the internal disk and the archive disk: archive
//! and restore, which only the owner asks for. Either can take minutes for a
//! large model, so it runs in the background, one at a time, and the event
//! stream says how far it is.

use crate::Server;
use dedupd_core::archive::{self, ArchivePlan, RestorePlan};
use dedupd_core::queue::State;
use serde_json::{Value, json as j};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Archive,
    Restore,
}

impl Kind {
    const fn as_str(self) -> &'static str {
        match self {
            Kind::Archive => "archive",
            Kind::Restore => "restore",
        }
    }
}

/// The move that is running, or the last one that ran.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct MoveState {
    /// `archive` or `restore`.
    pub kind: String,
    pub repo: String,
    pub running: bool,
    /// Seconds since the epoch.
    pub started_at: i64,
    pub finished_at: Option<i64>,
    /// The size of the files to move, and of those dealt with so far: for
    /// an archive the copies checked on the archive disk, for a restore the
    /// files copied back.
    pub bytes_total: u64,
    pub bytes_done: u64,
    /// Once finished: the files that left or came back to the internal disk.
    pub files: u64,
    /// Once an archive has finished: the stored files that stayed on the
    /// internal disk, because another model or an app still uses them.
    pub kept: u64,
    pub error: Option<String>,
}

pub(crate) fn archive_plan_answer(plan: &ArchivePlan) -> Value {
    j!({
        "repo": plan.repo,
        "archive": plan.archive,
        "deactivate": plan.deactivate.iter().map(|d| j!({
            "app": d.app,
            "files": d.remove.len(),
            "bytes": d.bytes,
            "keep": d.keep.iter()
                .map(|(path, why)| j!({"path": path, "why": why}))
                .collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "files": plan.remove.len(),
        "bytes": plan.bytes,
        "copy_files": plan.copy_files,
        "copy_bytes": plan.copy_bytes,
        "keep": plan.keep,
    })
}

pub(crate) fn restore_plan_answer(plan: &RestorePlan) -> Value {
    j!({
        "repo": plan.repo,
        "archive": plan.archive,
        "files": plan.copy.len(),
        "bytes": plan.bytes,
        "free": plan.free,
        "missing": plan.missing.len(),
    })
}

/// What the move would do, without doing it.
pub(crate) fn plan(server: &Server, kind: Kind, repo: &str) -> anyhow::Result<Value> {
    let e = &server.engine;
    Ok(match kind {
        Kind::Archive => {
            archive_plan_answer(&archive::archive_plan(&e.db, &e.store, &e.dirs, repo)?)
        }
        Kind::Restore => restore_plan_answer(&archive::restore_plan(&e.db, &e.store, repo)?),
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
pub(crate) async fn start(server: &Arc<Server>, kind: Kind, repo: String) -> Result<(), String> {
    let busy = |s: State| {
        !matches!(
            s,
            State::Done | State::Cancelled | State::NeedsAttention | State::Paused
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
        tokio::task::spawn_blocking(move || plan(&server, kind, &repo))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("{e:#}"))?["bytes"]
            .as_u64()
            .unwrap_or(0)
    };
    let mut refused = None;
    server.moving.send_if_modified(|state| {
        if let Some(now) = state.as_ref().filter(|s| s.running) {
            refused = Some(format!(
                "dedupd is still working on {} ({}); try again when that has finished",
                now.repo, now.kind
            ));
            return false;
        }
        *state = Some(MoveState {
            kind: kind.as_str().to_string(),
            repo: repo.clone(),
            running: true,
            started_at: epoch(),
            bytes_total: total,
            ..MoveState::default()
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
                    Kind::Archive => {
                        let r =
                            archive::archive_apply(&e.db, &e.store, &e.dirs, &repo, &mut on_file)?;
                        (r.files, r.freed, r.keep.len() as u64)
                    }
                    Kind::Restore => {
                        let r = archive::restore(&e.db, &e.store, &repo, &mut on_file)?;
                        if let Some(lost) = r.missing.first() {
                            anyhow::bail!(
                                "{} files of {repo} are neither on the internal disk nor on the archive disk (for example {})",
                                r.missing.len(),
                                &lost.sha256[..12]
                            );
                        }
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
