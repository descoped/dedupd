use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use dedupd_core::adopt::{self, Action, Event, Upstream, Verify};
use dedupd_core::config::Config;
use dedupd_core::db::Db;
use dedupd_core::fetch::{Fetcher, Progress};
use dedupd_core::hub::{DEFAULT_ENDPOINT, HubClient};
use dedupd_core::place::{App, AppDirs};
use dedupd_core::pull::{self, Engine, PullRequest};
use dedupd_core::select::Selection;
use dedupd_core::store::Store;
use dedupd_core::{Home, archive, status};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod more;

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

    /// Hugging Face endpoint [default: https://huggingface.co]
    #[arg(long, global = true, env = "DEDUPD_HF_ENDPOINT", value_name = "URL")]
    endpoint: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args)]
struct ModelArgs {
    /// The model: org/repo, org/repo:Q4_K_M or org/repo@revision
    model: String,
    /// Apps to place the model into: bionic, ollama, hf (also: mlx, llama.cpp)
    #[arg(long = "for", value_delimiter = ',', value_name = "APPS")]
    apps: Vec<App>,
    /// Only files matching these patterns, for example '*Q8_0*.gguf'
    #[arg(long, value_delimiter = ',', value_name = "PATTERNS")]
    include: Vec<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Fetch a model into the store once and place it into apps as clones
    Pull {
        #[command(flatten)]
        model: ModelArgs,
        /// Downloads to run at once
        #[arg(long, default_value_t = 4)]
        jobs: usize,
    },
    /// Place an archived model into apps without contacting Hugging Face
    Place {
        #[command(flatten)]
        model: ModelArgs,
    },
    /// Bring models the apps already hold into the store, without changing the apps' files
    Adopt {
        /// Apps to read: bionic, ollama, hf [default: all]
        #[arg(long = "app", value_delimiter = ',', value_name = "APPS")]
        apps: Vec<App>,
        /// Read this folder as a Bionic models folder instead of the apps' own folders
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
    },
    /// Show what the store holds and which app files it covers
    Status {
        /// Print machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Copy the store and database to the archive disk
    Backup {
        /// The archive folder [default: `archive` in config.toml]
        dir: Option<PathBuf>,
    },
    #[command(flatten)]
    More(more::Command),
}

pub fn human(bytes: u64) -> String {
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

/// The opened store, database and clients, plus the settings they came from.
pub struct Opened {
    pub home: Home,
    pub config: Config,
    pub engine: Arc<Engine>,
}

pub fn open(home: Option<PathBuf>, endpoint: Option<String>) -> Result<Opened> {
    let root = home
        .or_else(Home::default_root)
        .context("HOME is not set; pass --home")?;
    let home = Home::new(root);
    let config = Config::load(&home)?;
    let endpoint = endpoint
        .or_else(|| config.hf_endpoint.clone())
        .unwrap_or_else(|| DEFAULT_ENDPOINT.to_string());
    let token = HubClient::token_from_environment();
    let engine = Engine {
        store: Store::open(&home)?.with_archive(config.archive_home()),
        db: Mutex::new(Db::open(&home.db_path())?),
        hub: HubClient::new(endpoint.clone(), token.clone())?,
        fetcher: Fetcher::new(endpoint, token)?,
        dirs: AppDirs::resolve(&config)?,
    };
    Ok(Opened {
        home,
        config,
        engine: Arc::new(engine),
    })
}

/// Copy new store files to the archive disk when one is configured and
/// attached. Quiet when there is nothing to do.
pub fn backup_if_attached(o: &Opened) {
    let Some(archive_home) = o.config.archive_home() else {
        return;
    };
    if !archive::attached(&archive_home) {
        eprintln!(
            "Archive disk not attached; {} will be updated by the next `dedupd backup`.",
            archive_home.root().display()
        );
        return;
    }
    match archive::sync(&o.engine.db, &o.engine.store, &archive_home, &mut |_, _| {}) {
        Ok(r) if r.copied > 0 => eprintln!(
            "Archived {} files ({}) to {}.",
            r.copied,
            human(r.copied_bytes),
            archive_home.root().display()
        ),
        Ok(_) => {}
        Err(e) => eprintln!("Could not update the archive: {e:#}"),
    }
}

async fn run_pull(o: &Opened, model: ModelArgs, jobs: usize, offline: bool) -> Result<()> {
    let model_spec = model.model.clone();
    let (repo, quant, revision) = pull::parse_model(&model.model)?;
    let req = PullRequest {
        repo,
        revision,
        selection: Selection {
            quant,
            include: model.include,
        },
        apps: model.apps,
        offline,
        jobs,
    };
    // When the daemon is running on this store it does the work, so one
    // process downloads and the daemon's own requests share the result.
    let address = o.config.listen().to_string();
    let home = o.home.root().to_string_lossy().into_owned();
    let via_daemon = {
        let address = address.clone();
        tokio::task::spawn_blocking(move || dedupd_client::status(address))
            .await?
            .is_ok_and(|s| s.home == home)
    };

    let progress = Arc::new(Progress::default());
    let ticker = {
        let (progress, address) = (progress.clone(), address.clone());
        tokio::spawn(async move {
            let started = Instant::now();
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let (done, total) = if via_daemon {
                    let address = address.clone();
                    match tokio::task::spawn_blocking(move || dedupd_client::status(address)).await
                    {
                        Ok(Ok(s)) => (s.downloading_done, s.downloading_total),
                        _ => continue,
                    }
                } else {
                    (
                        progress.done.load(Ordering::Relaxed),
                        progress.total.load(Ordering::Relaxed),
                    )
                };
                if total > 0 && done < total {
                    eprintln!(
                        "  {} of {} ({:.0}%), {:.0?}",
                        human(done),
                        human(total),
                        done as f64 * 100.0 / total as f64,
                        started.elapsed()
                    );
                }
            }
        })
    };
    let result = if via_daemon {
        eprintln!("The dedupd daemon is running; it does the work.");
        let apps = req.apps.iter().map(|a| a.as_str().to_string()).collect();
        let (model, include) = (model_spec, req.selection.include.clone());
        tokio::task::spawn_blocking(move || {
            dedupd_client::pull(address, model, apps, include, offline)
        })
        .await?
        .map_err(anyhow::Error::from)
    } else {
        pull::pull(&o.engine, &req, &progress)
            .await
            .map(|out| dedupd_client::PullResult {
                repo: out.repo,
                commit: out.commit,
                from_archive: out.from_archive,
                files: out.files as u64,
                bytes: out.bytes,
                downloaded: out.downloaded as u64,
                placed: out
                    .placed
                    .into_iter()
                    .map(|(app, p)| dedupd_client::PlacedApp {
                        app: app.as_str().to_string(),
                        cloned: p.cloned as u64,
                        present: p.present as u64,
                        conflicts: p.conflicts,
                    })
                    .collect(),
                notes: out.notes,
            })
    };
    ticker.abort();
    let out = result?;

    println!(
        "{}@{}: {} files, {}{}",
        out.repo,
        &out.commit[..12],
        out.files,
        human(out.bytes),
        if out.from_archive {
            " (from the archive; Hugging Face was not used)"
        } else {
            ""
        }
    );
    println!(
        "Downloaded {} files; {} were already in the store.",
        out.downloaded,
        out.files - out.downloaded
    );
    for placed in &out.placed {
        println!(
            "  {:<7} {} placed as clones, {} already there",
            placed.app, placed.cloned, placed.present
        );
    }
    for note in &out.notes {
        println!("  note: {note}");
    }
    if !via_daemon {
        backup_if_attached(o);
    }
    if out.placed.iter().any(|p| !p.conflicts.is_empty()) {
        bail!("some files exist in an app folder with different content and were left alone");
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let o = open(cli.home, cli.endpoint)?;

    match cli.command {
        Command::Pull { model, jobs } => run_pull(&o, model, jobs, false).await?,
        Command::Place { model } => run_pull(&o, model, 1, true).await?,

        Command::Adopt {
            apps,
            models_dir,
            offline,
            dry_run,
            rehash,
            jobs,
        } => {
            let mut dirs = o.engine.dirs.clone();
            let mut apps = if apps.is_empty() {
                App::ALL.to_vec()
            } else {
                apps
            };
            if let Some(dir) = models_dir {
                if !dir.is_dir() {
                    bail!("{} is not a folder", dir.display());
                }
                dirs.bionic = dir;
                apps = vec![App::Bionic];
            }
            let repos = adopt::scan_apps(&o.engine.db, &dirs, &apps)?;
            let total_files: usize = repos.iter().map(|r| r.files.len()).sum();
            let total_bytes: u64 = repos
                .iter()
                .flat_map(|r| &r.files)
                .map(|f| f.fingerprint.size)
                .sum();
            eprintln!(
                "Found {} repositories, {total_files} files, {}",
                repos.len(),
                human(total_bytes),
            );

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
                        Upstream::Commit(c) => format!("upstream is at {}", &c[..12]),
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
            let hub = (!offline).then_some(&o.engine.hub);
            let outcomes = adopt::adopt(
                &o.engine.db,
                &o.engine.store,
                hub,
                repos,
                &opts,
                &mut on_event,
            )
            .await?;

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
            if !dry_run {
                backup_if_attached(&o);
            }
            if errors > 0 {
                bail!("{errors} files could not be adopted; see the FAILED lines above");
            }
        }

        Command::Status { json } => {
            let s = status::status(&o.engine.db())?;
            if json {
                println!("{}", serde_json::to_string_pretty(&s)?);
                return Ok(());
            }
            println!("Store: {}", o.home.store_dir().display());
            println!("  {} files, {}", s.store_files, human(s.store_bytes));
            println!(
                "  {} of app files share these blocks instead of being copies",
                human(s.shared_bytes)
            );
            if let Some(a) = o.config.archive_home() {
                println!(
                    "Archive: {}{}",
                    a.root().display(),
                    if archive::attached(&a) {
                        ""
                    } else {
                        " (disk not attached)"
                    }
                );
            }
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
                    "  {:<58} {:>10}  {} stored, {} in apps  main@{commit}  {}",
                    r.name,
                    human(r.stored_bytes),
                    r.stored_files,
                    r.files,
                    notes.join(", ")
                );
            }
        }

        Command::Backup { dir } => {
            let archive_home = dir
                .map(Home::new)
                .or_else(|| o.config.archive_home())
                .context("no archive folder: pass one, or set `archive` in config.toml")?;
            let started = Instant::now();
            let report = archive::sync(
                &o.engine.db,
                &o.engine.store,
                &archive_home,
                &mut |sha, size| eprintln!("  copied {:>10}  {}", human(size), &sha[..16]),
            )?;
            println!(
                "Archive at {}: copied {} files ({}) in {:.0?}, {} were already there.",
                archive_home.root().display(),
                report.copied,
                human(report.copied_bytes),
                started.elapsed(),
                report.present
            );
            if report.only_in_archive > 0 {
                println!(
                    "{} files are only in the archive; they are copied back when needed.",
                    report.only_in_archive
                );
            }
            for (sha, reason) in &report.failed {
                println!("  FAILED {sha}: {reason}");
            }
            if !report.failed.is_empty() {
                bail!("{} files could not be archived", report.failed.len());
            }
        }

        Command::More(cmd) => more::run(&o, cmd).await?,
    }
    Ok(())
}
