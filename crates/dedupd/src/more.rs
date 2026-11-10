//! The daemon and the maintenance commands.

use crate::Opened;
use anyhow::{Context, Result};
use clap::Subcommand;
use dedupd_server::Server;

#[derive(Subcommand)]
pub enum Command {
    /// Run the daemon: the Hugging Face and Ollama front ends and the background work
    Serve {
        /// Address to listen on [default: `listen` in config.toml, else 127.0.0.1:8642]
        #[arg(long, value_name = "ADDR")]
        listen: Option<String>,
    },
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
                let _ = tokio::signal::ctrl_c().await;
            };
            let result = dedupd_server::serve(server, listener, shutdown).await;
            background.abort();
            result
        }
    }
}
