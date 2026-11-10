use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use dedupd_core::adopt::{self, Action, Event, Upstream, Verify};
use dedupd_core::db::Db;
use dedupd_core::hub::{DEFAULT_ENDPOINT, HubClient};
use dedupd_core::sources::bionic;
use dedupd_core::store::Store;
use dedupd_core::{Home, status};
use std::path::PathBuf;
use std::time::Instant;

#[derive(Parser)]
#[command(
    name = "dedupd",
    version,
    about = "One shared local store for AI models"
)]
struct Cli {
    /// Where dedupd keeps its store and database [default: ~/.dedupd]
    #[arg(long, global = true, env = "DEDUPD_HOME", value_name = "DIR")]
    home: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Bring models an app already holds into the store, without changing the app's files
    Adopt {
        /// The app's models folder [default: ~/.lmstudio/models]
        #[arg(long, value_name = "DIR")]
        models_dir: Option<PathBuf>,
        /// Do not ask Hugging Face what each repository contains
        #[arg(long)]
        offline: bool,
        /// Hash and verify, but write nothing
        #[arg(long)]
        dry_run: bool,
        /// Hash every file again, including ones already recorded
        #[arg(long)]
        rehash: bool,
        /// Files to hash at once
        #[arg(long, default_value_t = 4)]
        jobs: usize,
        /// Hugging Face endpoint
        #[arg(long, env = "DEDUPD_HF_ENDPOINT", default_value = DEFAULT_ENDPOINT, value_name = "URL")]
        endpoint: String,
    },
    /// Show what the store holds and which app files it covers
    Status {
        /// Print machine-readable JSON
        #[arg(long)]
        json: bool,
    },
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

fn home(cli_home: Option<PathBuf>) -> Result<Home> {
    let root = cli_home
        .or_else(Home::default_root)
        .context("HOME is not set; pass --home")?;
    Ok(Home::new(root))
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let home = home(cli.home)?;

    match cli.command {
        Command::Adopt {
            models_dir,
            offline,
            dry_run,
            rehash,
            jobs,
            endpoint,
        } => {
            let models_dir = models_dir
                .or_else(bionic::default_models_dir)
                .context("HOME is not set; pass --models-dir")?;
            if !models_dir.is_dir() {
                bail!("{} is not a folder", models_dir.display());
            }
            let repos = bionic::scan(&models_dir)?;
            let total_files: usize = repos.iter().map(|r| r.files.len()).sum();
            let total_bytes: u64 = repos
                .iter()
                .flat_map(|r| &r.files)
                .map(|f| f.fingerprint.size)
                .sum();
            eprintln!(
                "Found {} repositories, {total_files} files, {} in {}",
                repos.len(),
                human(total_bytes),
                models_dir.display()
            );

            let store = Store::open(&home)?;
            let mut db = Db::open(&home.db_path())?;
            let hub = if offline {
                None
            } else {
                Some(HubClient::new(
                    endpoint,
                    HubClient::token_from_environment(),
                )?)
            };
            let opts = adopt::Options {
                jobs,
                dry_run,
                rehash,
            };

            let started = Instant::now();
            let mut on_event = |event: Event<'_>| match event {
                Event::Repo {
                    name,
                    upstream,
                    files,
                } => {
                    let note = match upstream {
                        Upstream::Commit(c) => format!("main is at {}", &c[..12]),
                        Upstream::NotAvailable(_) => "not available on Hugging Face".to_string(),
                        Upstream::Failed(e) => format!("could not check: {e}"),
                        Upstream::NotAsked => "not checked (offline)".to_string(),
                    };
                    eprintln!("{name}: {files} files, {note}");
                }
                Event::File(f) if f.action != Action::Unchanged => {
                    eprintln!(
                        "  {:<12} {:>10}  {}/{}",
                        f.verify.as_str(),
                        human(f.size),
                        f.repo,
                        f.rel_path
                    );
                }
                Event::File(_) => {}
                Event::Error { path, reason } => eprintln!("  FAILED {path}: {reason}"),
            };
            let outcomes =
                adopt::adopt(&mut db, &store, hub.as_ref(), repos, &opts, &mut on_event).await?;

            let files = || outcomes.iter().flat_map(|o| &o.files);
            let count = |a: Action| files().filter(|f| f.action == a).count();
            let state = |v: Verify| files().filter(|f| f.verify == v).count();
            let hashed: u64 = files()
                .filter(|f| f.action != Action::Unchanged)
                .map(|f| f.size)
                .sum();
            let errors: usize = outcomes.iter().map(|o| o.errors.len()).sum();

            println!();
            if dry_run {
                println!("Dry run: nothing was written.");
            }
            println!(
                "Hashed {} in {:.0?}. Cloned into store: {}. Already in store: {}. Unchanged since last run: {}.",
                human(hashed),
                started.elapsed(),
                count(Action::Cloned),
                count(Action::AlreadyInStore),
                count(Action::Unchanged),
            );
            println!(
                "Verified against Hugging Face: {}. Different from upstream: {}. Not listed upstream: {}. Not checked: {}.",
                state(Verify::Verified),
                state(Verify::Mismatch),
                state(Verify::NotUpstream),
                state(Verify::Unchecked),
            );
            for f in files().filter(|f| matches!(f.verify, Verify::Mismatch | Verify::NotUpstream))
            {
                println!("  {}: {}/{}", f.verify.as_str(), f.repo, f.rel_path);
            }
            if errors > 0 {
                bail!("{errors} files could not be adopted; see the FAILED lines above");
            }
        }

        Command::Status { json } => {
            if !home.db_path().exists() {
                bail!(
                    "nothing here yet: {} has no database. Run `dedupd adopt` first.",
                    home.root().display()
                );
            }
            let db = Db::open(&home.db_path())?;
            let s = status::status(&db)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&s)?);
                return Ok(());
            }
            println!("Store: {}", home.store_dir().display());
            println!("  {} files, {}", s.store_files, human(s.store_bytes));
            println!();
            println!("App files covered by the store:");
            for a in &s.apps {
                println!("  {:<10} {} files, {}", a.app, a.files, human(a.bytes));
            }
            println!();
            println!("Repositories:");
            for r in &s.repos {
                let commit = r.commit.as_deref().map(|c| &c[..12]).unwrap_or("unknown");
                let mut notes = vec![format!("{} verified", r.verified)];
                for (n, label) in [
                    (r.mismatch, "different from upstream"),
                    (r.not_upstream, "not listed upstream"),
                    (r.unchecked, "not checked"),
                ] {
                    if n > 0 {
                        notes.push(format!("{n} {label}"));
                    }
                }
                println!(
                    "  {:<58} {:>10}  {} files  main@{commit}  {}",
                    r.name,
                    human(r.bytes),
                    r.files,
                    notes.join(", ")
                );
            }
        }
    }
    Ok(())
}
