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
    if report.only_in_archive > 0 {
        println!(
            "{} files are only on the archive disk.",
            report.only_in_archive
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
        "Not referred to by any archived model or app file",
        &plan.orphans,
    );
    if offload {
        print_blobs(
            "On the archive disk and in no app (local copy can go)",
            &plan.offload,
        );
    }
    if apply {
        let freed = maintain::gc_apply(&o.engine.db, &o.engine.store, &plan)?;
        println!("Removed. Freed {} on the local disk.", human(freed));
    } else if !plan.orphans.is_empty() || !plan.offload.is_empty() {
        println!("Nothing was removed. Run again with --apply to remove these.");
    }
    Ok(())
}

pub fn cmd_forget(o: &Opened, repo: &str) -> Result<()> {
    maintain::forget(&o.engine.db, repo)?;
    println!("{repo} is no longer in the archive records. `dedupd gc` shows the files this frees.");
    Ok(())
}
