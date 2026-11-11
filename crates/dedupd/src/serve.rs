//! The daemon itself, and the settings that point tools at it.

use crate::Opened;
use crate::launchd::launchctl;
use anyhow::{Context, Result};
use dedupd_server::Server;

pub async fn cmd_serve(o: &Opened, listen: Option<String>) -> Result<()> {
    let addr = listen.unwrap_or_else(|| o.config.listen().to_string());
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
        let exe = std::env::current_exe().context("finding the dedupd binary")?;
        eprintln!("dedupd: starting again");
        let failed = std::process::Command::new(&exe)
            .args(std::env::args_os().skip(1))
            .exec();
        return Err(failed).with_context(|| format!("starting {}", exe.display()));
    }
    Ok(())
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
