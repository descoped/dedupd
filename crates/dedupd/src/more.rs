//! The daemon and the maintenance commands.

use crate::{Opened, human};
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use dedupd_core::maintain;
use dedupd_server::Server;
use std::path::PathBuf;
use std::process::Command as Process;
use std::time::Instant;

/// The launchd label of the daemon.
const LABEL: &str = "com.dedupd.daemon";

#[derive(Subcommand)]
pub enum Command {
    /// Run the daemon: the Hugging Face and Ollama front ends and the background work
    Serve {
        /// Address to listen on [default: `listen` in config.toml, else 127.0.0.1:8642]
        #[arg(long, value_name = "ADDR")]
        listen: Option<String>,
    },
    /// Swap app files that duplicate stored content for clones, freeing their space
    Reclaim {
        /// Show what would be swapped, without changing anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Hash every stored file and compare it with its name
    Verify {
        /// Check the copies on the archive disk instead of the local ones
        #[arg(long)]
        archive: bool,
        /// Files to hash at once
        #[arg(long, default_value_t = 4)]
        jobs: usize,
    },
    /// Show stored files nothing refers to; remove them with --apply
    Gc {
        /// Also remove local copies of files that are on the archive disk and in no app
        #[arg(long)]
        offload: bool,
        /// Remove what is listed. Without this, nothing is removed
        #[arg(long)]
        apply: bool,
    },
    /// Drop a model from the archive records, so gc can remove its files
    Forget {
        /// The repository, as org/repo
        repo: String,
    },
    /// Start the daemon at login, as a launchd agent
    Install {
        /// Also point apps started from the Dock or Finder at the daemon (HF_ENDPOINT)
        #[arg(long)]
        env: bool,
        #[arg(long, hide = true, default_value = LABEL)]
        label: String,
    },
    /// Stop the daemon and remove the launchd agent
    Uninstall {
        #[arg(long, hide = true, default_value = LABEL)]
        label: String,
    },
    /// Print the settings that point Hugging Face tools and Ollama at the daemon
    Env,
}

fn launchctl(args: &[&str]) -> Result<std::process::Output> {
    Process::new("/bin/launchctl")
        .args(args)
        .output()
        .context("running launchctl")
}

fn uid() -> u32 {
    // SAFETY: getuid has no failure mode and touches no memory.
    unsafe { libc_getuid() }
}

unsafe extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

fn plist_path(label: &str) -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist")))
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The launchd agent definition: run `dedupd serve` at login and restart it
/// if it stops.
fn plist(label: &str, exe: &str, home: &str, log: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>--home</string>
        <string>{}</string>
        <string>serve</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ProcessType</key>
    <string>Background</string>
    <key>StandardOutPath</key>
    <string>{}</string>
    <key>StandardErrorPath</key>
    <string>{}</string>
</dict>
</plist>
"#,
        xml(label),
        xml(exe),
        xml(home),
        xml(log),
        xml(log)
    )
}

fn endpoint_url(o: &Opened) -> String {
    format!("http://{}", o.config.listen())
}

pub async fn run(o: &Opened, cmd: Command) -> Result<()> {
    match cmd {
        Command::Serve { listen } => {
            let addr = listen.unwrap_or_else(|| o.config.listen().to_string());
            let listener = tokio::net::TcpListener::bind(&addr)
                .await
                .with_context(|| format!("listening on {addr}"))?;
            let server = Server::new(o.engine.clone(), o.config.clone(), o.home.clone());
            eprintln!(
                "dedupd listening on http://{addr} (store {}, upstream {})",
                o.home.store_dir().display(),
                o.engine.fetcher.endpoint()
            );
            if o.config.set_env == Some(true) {
                // Apps started from the Dock or Finder take their environment
                // from the login session, not from a shell profile.
                match launchctl(&["setenv", "HF_ENDPOINT", &format!("http://{addr}")]) {
                    Ok(out) if out.status.success() => {
                        eprintln!("dedupd: HF_ENDPOINT set for apps started from now on")
                    }
                    _ => eprintln!("dedupd: could not set HF_ENDPOINT for the login session"),
                }
            }
            // `localhost` may resolve to ::1 first; answer there as well.
            if let Some(port) = addr.strip_prefix("127.0.0.1:")
                && let Ok(v6) = tokio::net::TcpListener::bind(format!("[::1]:{port}")).await
            {
                tokio::spawn(dedupd_server::serve(
                    server.clone(),
                    v6,
                    std::future::pending(),
                ));
            }
            let background = tokio::spawn(dedupd_server::daemon::run(server.clone()));
            let shutdown = async {
                let mut term =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("signal handler");
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            };
            let result = dedupd_server::serve(server, listener, shutdown).await;
            background.abort();
            result?;
        }

        Command::Reclaim { dry_run } => {
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
        }

        Command::Verify { archive, jobs } => {
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
        }

        Command::Gc { offload, apply } => {
            let plan = maintain::gc_plan(&o.engine.db, &o.engine.store, offload)?;
            let total = |v: &[dedupd_core::db::BlobRow]| v.iter().map(|b| b.size).sum::<u64>();
            println!(
                "Not referred to by any archived model or app file: {} files, {}",
                plan.orphans.len(),
                human(total(&plan.orphans))
            );
            for b in &plan.orphans {
                println!("  {:>10}  {}", human(b.size), b.sha256);
            }
            if offload {
                println!(
                    "On the archive disk and in no app (local copy can go): {} files, {}",
                    plan.offload.len(),
                    human(total(&plan.offload))
                );
                for b in &plan.offload {
                    println!("  {:>10}  {}", human(b.size), b.sha256);
                }
            }
            if apply {
                let freed = maintain::gc_apply(&o.engine.db, &o.engine.store, &plan)?;
                println!("Removed. Freed {} on the local disk.", human(freed));
            } else if !plan.orphans.is_empty() || !plan.offload.is_empty() {
                println!("Nothing was removed. Run again with --apply to remove these.");
            }
        }

        Command::Forget { repo } => {
            maintain::forget(&o.engine.db, &repo)?;
            println!(
                "{repo} is no longer in the archive records. `dedupd gc` shows the files this frees."
            );
        }

        Command::Install { env, label } => {
            // The agent runs a copy under the home folder, so rebuilding or
            // cleaning the source tree cannot break the daemon.
            let built = std::env::current_exe().context("finding the dedupd binary")?;
            let exe = o.home.root().join("bin/dedupd");
            if built != exe {
                std::fs::create_dir_all(exe.parent().expect("bin folder"))?;
                let _ = std::fs::remove_file(&exe);
                std::fs::copy(&built, &exe)
                    .with_context(|| format!("copying dedupd to {}", exe.display()))?;
            }
            let path = plist_path(&label)?;
            let log = o.home.root().join("daemon.log");
            std::fs::create_dir_all(path.parent().expect("plist has a parent"))?;
            std::fs::write(
                &path,
                plist(
                    &label,
                    &exe.to_string_lossy(),
                    &o.home.root().to_string_lossy(),
                    &log.to_string_lossy(),
                ),
            )
            .with_context(|| format!("writing {}", path.display()))?;
            if env {
                let config = dedupd_core::config::Config::path(&o.home);
                let mut text = std::fs::read_to_string(&config).unwrap_or_default();
                if !text.contains("set_env") {
                    if !text.is_empty() && !text.ends_with('\n') {
                        text.push('\n');
                    }
                    text.push_str("set_env = true\n");
                    std::fs::write(&config, text)?;
                }
            }
            let domain = format!("gui/{}", uid());
            let _ = launchctl(&["bootout", &format!("{domain}/{label}")]);
            let out = launchctl(&["bootstrap", &domain, &path.to_string_lossy()])?;
            if !out.status.success() {
                bail!(
                    "launchctl could not start the agent: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            println!("Installed {} and started the daemon.", path.display());
            println!("It runs {} and logs to {}.", exe.display(), log.display());
            println!("Run `dedupd env` to see how to point tools at it.");
        }

        Command::Uninstall { label } => {
            let path = plist_path(&label)?;
            let _ = launchctl(&["bootout", &format!("gui/{}/{label}", uid())]);
            if label == LABEL {
                let _ = launchctl(&["unsetenv", "HF_ENDPOINT"]);
            }
            match std::fs::remove_file(&path) {
                Ok(()) => println!("Stopped the daemon and removed {}.", path.display()),
                Err(_) => println!("No agent was installed at {}.", path.display()),
            }
        }

        Command::Env => {
            let url = endpoint_url(o);
            println!(
                "# Hugging Face tools (hf, huggingface_hub, mlx_lm, llama.cpp): add to ~/.zshrc"
            );
            println!("export HF_ENDPOINT={url}");
            println!();
            println!("# Apps started from the Dock or Finder: run once per login, or use");
            println!("# `dedupd install --env` to have the daemon do it.");
            println!("launchctl setenv HF_ENDPOINT {url}");
            println!();
            println!("# Ollama has no endpoint setting. Either let dedupd place the model:");
            println!("dedupd pull org/repo:Q4_K_M --for ollama");
            println!("# or pull through the daemon (the model name then carries its address):");
            println!(
                "ollama pull --insecure {}/org/repo:Q4_K_M",
                url.trim_start_matches("http://")
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_escapes_and_names_the_program() {
        let p = plist("com.x", "/a&b/dedupd", "/h<ome", "/log");
        assert!(p.contains("<string>/a&amp;b/dedupd</string>"));
        assert!(p.contains("<string>/h&lt;ome</string>"));
        assert!(p.contains("<string>serve</string>"));
    }
}
