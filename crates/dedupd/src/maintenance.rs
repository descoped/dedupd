//! The maintenance commands: reclaim, verify, gc and forget.

use crate::{Opened, human};
use anyhow::{Result, bail};
use dedupd_core::db::BlobRow;
use dedupd_core::maintain;
use std::time::Instant;

pub fn cmd_reclaim(o: &Opened, dry_run: bool) -> Result<()> {
    // A minute's grace, so a file an app is still writing is left alone.
    let report = maintain::reclaim(
        &o.engine.db,
        &o.engine.store,
        maintain::RECLAIM_MIN_AGE,
        dry_run,
    )?;
    println!(
        "{} {} duplicate files with clones, freeing {}.",
        if dry_run { "Would replace" } else { "Replaced" },
        report.swapped,
        human(report.bytes)
    );
    for (path, reason) in &report.skipped {
        println!("  left alone: {path} ({reason})");
    }
    Ok(())
}

pub fn cmd_verify(o: &Opened, archive: bool, jobs: usize) -> Result<()> {
    let started = Instant::now();
    let report = maintain::verify(&o.engine.db, &o.engine.store, archive, jobs)?;
    println!(
        "Checked {} files ({}) in {:.0?}: {} damaged, {} missing.",
        report.checked,
        human(report.bytes),
        started.elapsed(),
        report.damaged.len(),
        report.missing.len()
    );
    for (sha, why) in &report.unreadable {
        println!("  COULD NOT READ {sha}: {why}");
    }
    if report.only_in_archive > 0 {
        println!(
            "{} files are only on the archive disk.",
            report.only_in_archive
        );
    }
    if report.archive_away > 0 {
        println!(
            "{} files are not on this Mac and the archive disk is away, so they were not looked for.",
            report.archive_away
        );
    }
    if report.not_backed_up > 0 {
        println!(
            "{} files are not on the archive disk yet; `dedupd backup` copies them.",
            report.not_backed_up
        );
    }
    for sha in &report.damaged {
        println!("  DAMAGED {sha}");
    }
    for sha in &report.missing {
        println!("  MISSING {sha}");
    }
    if !report.damaged.is_empty() || !report.missing.is_empty() {
        bail!("the store is not intact");
    }
    Ok(())
}

/// One list of a gc plan: what it is, how much, then each file.
fn print_blobs(title: &str, blobs: &[BlobRow]) {
    let total: u64 = blobs.iter().map(|b| b.size).sum();
    println!("{title}: {} files, {}", blobs.len(), human(total));
    for b in blobs {
        println!("  {:>10}  {}", human(b.size), b.sha256);
    }
}

pub fn cmd_gc(o: &Opened, offload: bool, apply: bool) -> Result<()> {
    let plan = maintain::gc_plan(&o.engine.db, &o.engine.store, offload)?;
    print_blobs(
        "Not referred to by any model in the library or app file",
        &plan.orphans,
    );
    if offload {
        print_blobs(
            "On the archive disk and in no app (local copy can go)",
            &plan.offload,
        );
    }
    if apply {
        let freed = match crate::daemon_here(o) {
            // The daemon removes exactly what was listed here, as far as
            // it is still removable.
            Some(address) => {
                eprintln!("The dedupd daemon is running; it does the work.");
                let shas = |blobs: &[BlobRow]| blobs.iter().map(|b| b.sha256.clone()).collect();
                dedupd_client::gc_apply(address, shas(&plan.orphans), shas(&plan.offload))?
                    .freed
                    .unwrap_or(0)
            }
            None => maintain::gc_apply(&o.engine.db, &o.engine.store, &plan)?,
        };
        println!("Removed. Freed {} on the local disk.", human(freed));
    } else if !plan.orphans.is_empty() || !plan.offload.is_empty() {
        println!("Nothing was removed. Run again with --apply to remove these.");
    }
    Ok(())
}

pub fn cmd_forget(o: &Opened, repo: &str, apply: bool) -> Result<()> {
    let e = &o.engine;
    if !apply {
        maintain::forget_check(&e.db, repo)?;
        let installed = dedupd_core::profile::Installed::detect(&e.dirs);
        let kept = dedupd_core::library::model(&e.db, &e.store, repo, &installed)?;
        let revisions = kept.as_ref().map_or(0, |m| m.revisions.len());
        let files: usize = kept
            .iter()
            .flat_map(|m| &m.revisions)
            .map(|r| r.files.len())
            .sum();
        println!(
            "Forgetting {repo} would drop its records: {revisions} revisions that list {files} files, its branch names, and the Ollama manifests kept for it, which exist nowhere else."
        );
        println!(
            "No file is removed; `dedupd gc` then lists the stored files nothing refers to any more."
        );
        println!("Nothing was changed. Run again with --apply to forget the model.");
        return Ok(());
    }
    match crate::daemon_here(o) {
        Some(address) => {
            eprintln!("The dedupd daemon is running; it does the work.");
            dedupd_client::forget(address, repo.to_string())?;
        }
        None => maintain::forget(&e.db, repo)?,
    }
    println!("{repo} is no longer in the library. `dedupd gc` shows the files this frees.");
    Ok(())
}
