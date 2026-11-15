//! Running one job, and turning what stopped it into words for the owner.

use super::*;

/// What stopped an attempt, by what to do about it.
pub(super) enum Problem {
    Network(String),
    Busy(u64),
    /// Only the owner can do something about it. `gone` when the provider
    /// said it does not have the repository, or will not show it.
    Owner {
        why: String,
        gone: bool,
    },
}

impl Queue {
    pub(super) async fn run(self: &Arc<Self>, job: &Arc<Job>) {
        let engine = &self.engine;
        let jobs = engine.governor.max_connections();
        let (req, pinned) = {
            let r = job.record();
            match r.request(jobs) {
                Ok(req) => (req, r.commit.clone()),
                Err(e) => {
                    drop(r);
                    return self.set(job, JobState::NeedsAttention, Some(format!("{e:#}")));
                }
            }
        };

        // Settle the revision and the files, waiting out a provider that is
        // busy or cannot be reached.
        let mut failures = 0u32;
        let plan = loop {
            self.set(job, JobState::Resolving, None);
            let problem = match pull::plan(engine, &req, pinned.as_deref()).await {
                Ok(plan) => break plan,
                Err(e) => problem_of(&e, &req),
            };
            failures += 1;
            match problem {
                Problem::Owner { why, gone } => {
                    if gone {
                        let _ = engine.db().set_upstream(PROVIDER_HF, &req.repo, true);
                    }
                    return self.needs_owner(job, &req, why);
                }
                Problem::Busy(seconds) => {
                    job.record().wait_until = Some(now() + seconds as i64);
                    self.set(job, JobState::WaitingServer, Some(wait_text(seconds)));
                    tokio::time::sleep(Duration::from_secs(seconds)).await;
                }
                Problem::Network(why) => {
                    self.set(job, JobState::WaitingNetwork, Some(why));
                    let pause = Duration::from_secs(2u64.pow(failures.min(5)).min(30));
                    tokio::time::sleep(pause).await;
                }
            }
        };
        if !plan.from_archive {
            let _ = engine.db().set_upstream(PROVIDER_HF, &req.repo, false);
        }
        {
            let mut r = job.record();
            r.commit = Some(plan.snap.commit.clone());
            r.bytes_total = plan.bytes;
            r.bytes_done = 0;
            r.files = plan.entries.iter().map(file_state).collect();
            let from = if plan.from_archive {
                ", from the kept copy"
            } else {
                ""
            };
            r.note(format!(
                "settled on {}@{}, {} files{from}",
                req.revision,
                &plan.snap.commit[..12.min(plan.snap.commit.len())],
                plan.entries.len()
            ));
        }
        self.set(job, JobState::Downloading, None);

        let watch = Arc::new(JobWatch {
            queue: self.clone(),
            job: job.clone(),
        });
        let fetched = pull::fetch(engine, &req, &plan, watch).await;
        let (items, downloaded) = match fetched {
            Ok(done) => done,
            Err(e) => {
                let why = attention_text(&pull::gone_upstream(e, &req, &plan), &req);
                return self.needs_owner(job, &req, why);
            }
        };

        self.set(job, JobState::Placing, None);
        let files = items.len();
        match pull::place(engine, &req, &plan, items).await {
            Ok(placement) => {
                // Every file is stored. An app that did not take the model
                // for good (it cannot run it, files of its own in the way)
                // would answer the same again, so the job is done; one that
                // could not be asked just now (the provider out of reach, a
                // full disk) waits for the owner to try again. The outcome
                // says which apps took it and why the others did not.
                let outcome = pull::outcome(&req, plan, files, downloaded, placement);
                let retry: Vec<String> = outcome
                    .skipped
                    .iter()
                    .filter(|s| s.temporary)
                    .map(|s| format!("not activated in {}: {}", s.app.title(), s.reason))
                    .collect();
                {
                    let mut r = job.record();
                    r.bytes_done = r.bytes_total;
                    for skipped in &outcome.skipped {
                        r.note(format!(
                            "not activated in {}: {}",
                            skipped.app.title(),
                            skipped.reason
                        ));
                    }
                    for note in &outcome.notes {
                        r.note(note.clone());
                    }
                    r.outcome = Some(outcome);
                }
                if !retry.is_empty() {
                    // `place` has told the history already.
                    return self.set(job, JobState::NeedsAttention, Some(retry.join("; ")));
                }
                self.set(job, JobState::Done, None);
                self.forget_old();
                let call = self.on_done.lock().expect("lock").clone();
                if let Some(call) = call {
                    call();
                }
            }
            Err(e) => self.needs_owner(job, &req, format!("{e:#}")),
        }
    }

    /// Stop a job until the owner acts. An activation that stops before
    /// anything was placed is told in the history too, app by app.
    fn needs_owner(&self, job: &Job, req: &PullRequest, why: String) {
        if job.record().kind == JobKind::Activation {
            // A file write, kept off the async runtime.
            let (home, repo, apps, text) = (
                self.engine.store.home().clone(),
                req.repo.clone(),
                req.apps.clone(),
                why.clone(),
            );
            tokio::task::spawn_blocking(move || {
                crate::activity::record_stopped_activation(&home, &repo, &apps, &text);
            });
        }
        self.set(job, JobState::NeedsAttention, Some(why));
    }
}

pub(super) fn wait_text(seconds: u64) -> String {
    format!("Hugging Face asked for a pause of {seconds} s")
}

/// Why the revision could not be settled.
fn problem_of(e: &anyhow::Error, req: &PullRequest) -> Problem {
    match e.downcast_ref::<HubError>() {
        Some(HubError::Busy { seconds }) => Problem::Busy(*seconds),
        Some(HubError::Transport(_)) => {
            Problem::Network("Hugging Face cannot be reached".to_string())
        }
        Some(HubError::Status { status, .. }) if *status >= 500 => {
            Problem::Network(format!("Hugging Face answered HTTP {status}"))
        }
        Some(HubError::NotAvailable { status, .. }) => Problem::Owner {
            why: not_available(*status, req),
            gone: true,
        },
        _ => Problem::Owner {
            why: format!("{e:#}"),
            gone: false,
        },
    }
}

/// What to tell the owner when the provider will not hand something over.
fn not_available(status: u16, req: &PullRequest) -> String {
    match status {
        401 | 403 => format!(
            "Hugging Face will not show {} to this Mac (HTTP {status}). If the model is \
             gated, accept its terms on huggingface.co and make sure a token is set, then retry.",
            req.repo
        ),
        _ => format!(
            "{} is not on Hugging Face (HTTP {status}), and it is not kept here.",
            req.repo
        ),
    }
}

/// Why the files could not be fetched, for the owner.
fn attention_text(e: &anyhow::Error, req: &PullRequest) -> String {
    if let Some(unavailable) = e.downcast_ref::<Unavailable>() {
        return not_available(unavailable.status, req);
    }
    if let Some(space) = e.downcast_ref::<NoSpace>() {
        return format!("{space}. Free some space, then retry.");
    }
    if e.downcast_ref::<WrongContent>().is_some() {
        return "A file arrived damaged twice. Retry later; if it keeps happening the file \
                on Hugging Face does not match its listing."
            .to_string();
    }
    format!("{e:#}")
}
