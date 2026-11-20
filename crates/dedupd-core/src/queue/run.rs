//! Running one job, and turning what stopped it into words for the owner.

use super::*;

/// Put what an earlier run of the job did into the outcome of a run that
/// tried again: the apps in `took` it placed the model into, what it
/// downloaded, and its notes, each note once. Returns how many of the notes
/// are the earlier run's, which the job's history already holds.
fn carry_over(outcome: &mut PullResult, earlier: PullResult, took: &[App]) -> usize {
    let mut placed: Vec<_> = earlier
        .placed
        .into_iter()
        .filter(|p| took.contains(&p.app))
        .collect();
    placed.append(&mut outcome.placed);
    outcome.placed = placed;
    outcome.downloaded += earlier.downloaded;
    let mut notes = earlier.notes;
    let known = notes.len();
    for note in std::mem::take(&mut outcome.notes) {
        if !notes.contains(&note) {
            notes.push(note);
        }
    }
    outcome.notes = notes;
    known
}

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
        let (mut req, pinned, earlier) = {
            let r = job.record();
            match r.request(jobs) {
                Ok(req) => (req, r.commit.clone(), r.outcome.clone()),
                Err(e) => {
                    drop(r);
                    return self.set(job, JobState::NeedsAttention, Some(format!("{e:#}")));
                }
            }
        };
        // Tried again after some apps took the model and others could not
        // be asked: only those others are asked now, and the apps that took
        // it keep their place in the outcome and are not told about twice.
        let mut took: Vec<App> = earlier
            .as_ref()
            .map(PullResult::activated)
            .unwrap_or_default();
        if req.apps.iter().any(|a| !took.contains(a)) {
            req.apps.retain(|a| !took.contains(a));
        } else {
            // Every app is asked again (a job cut off while placing): the
            // new outcome is the whole answer.
            took.clear();
        }

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
            _alive: job.alive.clone(),
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
                let mut outcome = pull::outcome(&req, plan, files, downloaded, placement);
                // Trying again asked only the apps that had not taken the
                // model; what the earlier run did stays part of the job.
                let known = match earlier.filter(|_| !took.is_empty()) {
                    Some(earlier) => carry_over(&mut outcome, earlier, &took),
                    None => 0,
                };
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
                    for note in &outcome.notes[known..] {
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

#[cfg(test)]
mod tests {
    use super::*;
    use dedupd_api::jobs::PlacedApp;

    fn result(placed: &[App], downloaded: u64, notes: &[&str]) -> PullResult {
        PullResult {
            repo: "org/Repo".into(),
            commit: "c".repeat(40),
            from_archive: false,
            files: 2,
            bytes: 10,
            downloaded,
            placed: placed
                .iter()
                .map(|&app| PlacedApp {
                    app,
                    cloned: 2,
                    present: 0,
                    conflicts: vec![],
                })
                .collect(),
            skipped: vec![],
            notes: notes.iter().map(|n| n.to_string()).collect(),
        }
    }

    #[test]
    fn trying_again_keeps_the_earlier_run_and_tells_only_what_is_new() {
        let earlier = result(
            &[App::Bionic, App::Hf],
            2,
            &["kept from the first run", "both"],
        );
        let mut outcome = result(&[App::Ollama], 0, &["both", "new in this run"]);
        let known = carry_over(&mut outcome, earlier, &[App::Bionic]);
        let apps: Vec<App> = outcome.placed.iter().map(|p| p.app).collect();
        assert_eq!(apps, [App::Bionic, App::Ollama]);
        assert_eq!(outcome.downloaded, 2);
        assert_eq!(
            outcome.notes,
            ["kept from the first run", "both", "new in this run"]
        );
        // Only the note this run added goes into the job's history.
        assert_eq!(outcome.notes[known..], ["new in this run"]);
    }
}
