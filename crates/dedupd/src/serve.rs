//! The daemon itself, and the settings that point tools at it.

use crate::Opened;
use crate::launchd::launchctl;
use anyhow::{Context, Result, bail};
use dedupd_core::Home;
use dedupd_core::config::{is_loopback, not_loopback};
use dedupd_server::Server;

/// A log larger than this is cut back when the daemon starts.
const LOG_LIMIT: u64 = 16 * 1024 * 1024;
/// How much of its end is kept then.
const LOG_KEPT: u64 = 1024 * 1024;

/// Cut the daemon's log back to its end when it has grown past the limit.
/// launchd appends to the file for as long as the agent is installed, and
/// nothing else shortens it. The file is cut in place: launchd's writes go
/// to its end, wherever that is.
fn trim_log(home: &Home, limit: u64, kept: u64) -> std::io::Result<bool> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let path = home.log_path();
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let size = file.metadata()?.len();
    if size <= limit {
        return Ok(false);
    }
    let mut tail = Vec::new();
    file.seek(SeekFrom::Start(size.saturating_sub(kept)))?;
    file.read_to_end(&mut tail)?;
    // From the first whole line on.
    let from = tail.iter().position(|b| *b == b'\n').map_or(0, |at| at + 1);
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(b"dedupd: the log was cut back to its end here\n")?;
    file.write_all(&tail[from..])?;
    Ok(true)
}

/// Take the home for this daemon. A second daemon on the same home (another
/// port, a build from the source folder) would clear the files the first
/// is putting together and take up the same downloads. The lock is the
/// system's and goes when the process ends, however it ends.
fn only_daemon_here(home: &Home) -> Result<std::fs::File> {
    let path = home.daemon_lock_path();
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(std::fs::TryLockError::WouldBlock) => bail!(
            "a dedupd daemon already runs on {}; stop it first, or use another --home",
            home.root().display()
        ),
        Err(std::fs::TryLockError::Error(e)) => {
            Err(e).with_context(|| format!("locking {}", path.display()))
        }
    }
}

pub async fn cmd_serve(o: &Opened, listen: Option<String>) -> Result<()> {
    let addr = listen.unwrap_or_else(|| o.config.listen().to_string());
    if !is_loopback(&addr) {
        bail!("{}", not_loopback(&addr));
    }
    // Held until the process ends or becomes the next daemon.
    let only = only_daemon_here(&o.home)?;
    if let Err(e) = trim_log(&o.home, LOG_LIMIT, LOG_KEPT) {
        eprintln!("dedupd: the log could not be cut back: {e}");
    }
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("listening on {addr}"))?;
    // The daemon has no one waiting at a prompt: a download that meets a
    // busy or unreachable server waits and goes on, however long it takes.
    o.engine.fetcher.set_patient(true);
    let server = Server::new(o.engine.clone(), o.config.clone(), o.home.clone())?;
    eprintln!(
        "dedupd listening on http://{addr} (store {}, upstream {})",
        o.home.store_dir().display(),
        o.engine.fetcher.endpoint()
    );
    if o.config.set_env() {
        // Apps started from the Dock or Finder take their environment
        // from the login session, not from a shell profile. `stop` and
        // `uninstall` take it away again when it is still this address.
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
        let (also, port) = (server.clone(), port.to_string());
        tokio::spawn(async move {
            if let Err(e) = dedupd_server::serve(also, v6, std::future::pending()).await {
                eprintln!("dedupd: no longer answering on [::1]:{port}: {e:#}");
            }
        });
    }
    let background = tokio::spawn(dedupd_server::daemon::run(server.clone()));
    let shutdown = async {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("signal handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    };
    let result = dedupd_server::serve(server.clone(), listener, shutdown).await;
    background.abort();
    result?;
    if server.restart_asked() {
        // Become a new daemon in this process: launchd sees no change, and
        // a daemon started by hand stays in its terminal.
        use std::os::unix::process::CommandExt;
        // The next daemon takes the home itself.
        drop(only);
        let exe = std::env::current_exe().context("finding the dedupd binary")?;
        eprintln!("dedupd: starting again");
        let failed = std::process::Command::new(&exe)
            .args(std::env::args_os().skip(1))
            .exec();
        return Err(failed).with_context(|| format!("starting {}", exe.display()));
    }
    // Asked to stop: leave now. Returning would drop the runtime, which
    // waits without limit for work on its blocking threads (a store check
    // takes hours), so the process would stay and a second Ctrl-C would do
    // nothing. The work is cut as a kill would cut it, which every such
    // operation already has to survive.
    drop(only);
    std::process::exit(0)
}

pub fn cmd_env(o: &Opened) -> Result<()> {
    let url = format!("http://{}", o.config.listen());
    println!("# Hugging Face tools (hf, huggingface_hub, mlx_lm, llama.cpp): add to ~/.zshrc");
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_that_has_grown_is_cut_back_to_its_last_whole_lines() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path());
        assert!(!trim_log(&home, 100, 30).unwrap(), "no log yet");
        let lines: String = (0..20).map(|n| format!("line {n:02}\n")).collect();
        std::fs::write(home.log_path(), &lines).unwrap();
        assert!(!trim_log(&home, 1000, 30).unwrap(), "under the limit");
        assert!(trim_log(&home, 100, 30).unwrap());
        let kept = std::fs::read_to_string(home.log_path()).unwrap();
        assert_eq!(
            kept,
            "dedupd: the log was cut back to its end here\nline 17\nline 18\nline 19\n"
        );
    }

    #[test]
    fn a_home_is_one_daemons() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path());
        let first = only_daemon_here(&home).unwrap();
        let second = only_daemon_here(&home).unwrap_err();
        assert!(second.to_string().contains("already runs"), "{second:#}");
        drop(first);
        only_daemon_here(&home).unwrap();
    }
}
