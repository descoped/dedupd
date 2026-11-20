//! The commands that take a model out of use: deactivate (out of an app),
//! archive (out of every app and off the internal disk) and restore.

use crate::{Opened, human};
use anyhow::{Result, bail};
use dedupd_api::app::App;
use dedupd_api::moves::{ModelMove, MoveKind};
use dedupd_core::archive;
use dedupd_core::maintain::{self, UnplacePlan};
use std::time::Instant;

/// Says how far a move is, one line per file, and nothing twice.
struct Progress {
    verb: &'static str,
    total: u64,
    done: u64,
    last: String,
}

impl Progress {
    fn new(verb: &'static str, total: u64) -> Self {
        Self {
            verb,
            total,
            done: 0,
            last: String::new(),
        }
    }

    fn add(&mut self, size: u64) {
        self.done += size;
        let line = format!(
            "  {} {} of {}",
            self.verb,
            human(self.done),
            human(self.total)
        );
        if line != self.last {
            eprintln!("{line}");
            self.last = line;
        }
    }
}

/// What taking a model out of one app removes and what it leaves.
fn print_unplace(plan: &UnplacePlan, done: Option<usize>) {
    let files = plan.remove.len();
    match done {
        Some(removed) => println!(
            "{}: removed {removed} files ({}).",
            plan.app,
            human(plan.bytes)
        ),
        None => println!("{}: {files} files ({})", plan.app, human(plan.bytes)),
    }
    if done.is_none() {
        for file in &plan.remove {
            println!("  {:>10}  {}", human(file.size), file.path);
        }
    }
    for kept in &plan.keep {
        println!("  left in place: {} ({})", kept.path, kept.why);
    }
}

pub fn cmd_deactivate(o: &Opened, repo: &str, apps: Vec<App>, apply: bool) -> Result<()> {
    let e = &o.engine;
    maintain::prune_placements(&e.db)?;
    let holding: Vec<App> = {
        let placed = e.db().placements()?;
        App::ALL
            .into_iter()
            .filter(|app| {
                placed
                    .iter()
                    .any(|p| p.repo == repo && p.app == app.as_str())
            })
            .collect()
    };
    let apps: Vec<App> = match apps.is_empty() {
        true => holding,
        false => apps.into_iter().filter(|a| holding.contains(a)).collect(),
    };
    if apps.is_empty() {
        println!("{repo} is in none of these apps. Nothing to do.");
        return Ok(());
    }
    let mut pending = false;
    for app in apps {
        let plan = maintain::unplace_plan(&e.db, &e.store, &e.dirs, repo, app)?;
        let done = match apply {
            true => Some(maintain::deactivate(&e.db, &e.store, &e.dirs, &plan)?),
            false => None,
        };
        pending |= !plan.remove.is_empty() || !plan.links.is_empty();
        print_unplace(&plan, done);
    }
    if apply {
        println!("The stored copy stays, so the model can be activated again without downloading.");
    } else if pending {
        println!(
            "Nothing was removed. Run again with --apply to take the model out of these apps."
        );
    }
    Ok(())
}

/// Have the daemon archive or restore `repo`, when one runs on this home:
/// it keeps its download queue away from the model meanwhile, which a
/// command working beside it cannot. Follows the move to its end and
/// returns it; `None` when no daemon answers for this home, and the
/// command then does the work itself.
fn moved_by_daemon(o: &Opened, repo: &str, kind: MoveKind) -> Result<Option<ModelMove>> {
    let address = o.config.listen().to_string();
    let runs_here = dedupd_client::status(address.clone())
        .is_ok_and(|s| crate::same_folder(std::path::Path::new(&s.home), o.home.root()));
    if !runs_here {
        return Ok(None);
    }
    eprintln!("The dedupd daemon is running; it does the work.");
    match kind {
        MoveKind::Archive => dedupd_client::archive_model(address.clone(), repo.to_string())?,
        MoveKind::Restore => dedupd_client::restore_model(address.clone(), repo.to_string())?,
    }
    let mut said = String::new();
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let Some(now) = dedupd_client::model_move(address.clone())?
            .filter(|m| m.kind == kind && m.repo == repo)
        else {
            bail!("the daemon no longer tells how the move of {repo} goes; see `dedupd status`");
        };
        if !now.running {
            return match now.error {
                Some(why) => bail!("{why}"),
                None => Ok(Some(now)),
            };
        }
        let line = format!("  {} of {}", human(now.bytes_done), human(now.bytes_total));
        if line != said {
            eprintln!("{line}");
            said = line;
        }
    }
}

pub fn cmd_archive(o: &Opened, repo: &str, apply: bool) -> Result<()> {
    let e = &o.engine;
    let plan = archive::archive_plan(&e.db, &e.store, &e.dirs, repo)?;
    if !apply {
        println!("Archiving {repo} to {} would:", plan.archive);
        if plan.deactivate.is_empty() {
            println!("Take it out of no app: it is in none.");
        }
        for step in &plan.deactivate {
            print_unplace(step, None);
        }
        if plan.copy_files > 0 {
            println!(
                "Copy {} files ({}) to the archive disk first, because it does not have them yet.",
                plan.copy_files,
                human(plan.copy_bytes)
            );
        }
        println!(
            "Remove {} stored files ({}) from the internal disk, after reading each copy on the archive disk in full.",
            plan.remove.len(),
            human(plan.bytes)
        );
        for kept in &plan.keep {
            println!(
                "  stays: {} {} ({})",
                human(kept.size),
                &kept.sha256[..12],
                kept.why
            );
        }
        println!("Nothing was changed. Run again with --apply to archive the model.");
        return Ok(());
    }
    let started = Instant::now();
    if let Some(done) = moved_by_daemon(o, repo, MoveKind::Archive)? {
        println!(
            "Archived {repo} in {:.0?}: {} files left the internal disk{}.",
            started.elapsed(),
            done.files,
            match done.kept {
                0 => String::new(),
                n => format!("; {n} stayed, because another model or an app still uses them"),
            }
        );
        println!("`dedupd restore {repo}` copies it back.");
        return Ok(());
    }
    let mut said = Progress::new("checked", plan.bytes);
    let report =
        archive::archive_apply(&e.db, &e.store, &e.dirs, repo, &mut |size| said.add(size))?;
    for (app, files) in &report.deactivated {
        println!("{app}: removed {files} files.");
    }
    println!(
        "Archived {repo} in {:.0?}: {} files left the internal disk, freeing {}{}.",
        started.elapsed(),
        report.files,
        human(report.freed),
        match report.copied {
            0 => String::new(),
            n => format!("; {n} were copied to the archive disk first"),
        }
    );
    for kept in &report.keep {
        println!(
            "  stays: {} {} ({})",
            human(kept.size),
            &kept.sha256[..12],
            kept.why
        );
    }
    println!("`dedupd restore {repo}` copies it back.");
    Ok(())
}

pub fn cmd_restore(o: &Opened, repo: &str) -> Result<()> {
    let e = &o.engine;
    let plan = archive::restore_plan(&e.db, &e.store, repo)?;
    if plan.copy.is_empty() && plan.missing.is_empty() {
        archive::restore(&e.db, &e.store, repo, &mut |_| {})?;
        println!("{repo} is on the internal disk already. Nothing to copy.");
        return Ok(());
    }
    eprintln!(
        "Copying {} files ({}) back from {}.",
        plan.copy.len(),
        human(plan.bytes),
        plan.archive
    );
    let started = Instant::now();
    if let Some(done) = moved_by_daemon(o, repo, MoveKind::Restore)? {
        println!(
            "Restored {repo} in {:.0?}: {} files. It is in no app yet; `dedupd activate <model> --for <apps>` puts it into one.",
            started.elapsed(),
            done.files
        );
        return Ok(());
    }
    let mut said = Progress::new("copied", plan.bytes);
    let report = match archive::restore(&e.db, &e.store, repo, &mut |size| said.add(size)) {
        Ok(report) => report,
        Err(e) => {
            // Say which files are nowhere before giving up.
            if let Some(missing) = e.downcast_ref::<archive::Missing>() {
                for blob in &missing.report.missing {
                    println!(
                        "  MISSING {} ({}): neither on the internal disk nor on the archive disk",
                        blob.sha256,
                        human(blob.size)
                    );
                }
            }
            return Err(e.context(format!("some files of {repo} could not be restored")));
        }
    };
    println!(
        "Restored {repo} in {:.0?}: {} files, {}. It is in no app yet; `dedupd activate <model> --for <apps>` puts it into one.",
        started.elapsed(),
        report.files,
        human(report.bytes)
    );
    Ok(())
}
