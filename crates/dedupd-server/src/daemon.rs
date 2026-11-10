//! The daemon's background work.

use crate::Server;
use std::sync::Arc;

#[derive(Debug, Default, serde::Serialize)]
pub struct PassReport {}

/// One round of background work.
pub async fn pass(server: &Arc<Server>) -> anyhow::Result<PassReport> {
    let _guard = server.pass_lock.lock().await;
    Ok(PassReport::default())
}

/// Run the background work until the task is aborted.
pub async fn run(server: Arc<Server>) {
    let every = std::time::Duration::from_secs(server.config.scan_interval.unwrap_or(60).max(5));
    loop {
        if let Err(e) = pass(&server).await {
            eprintln!("dedupd: background pass failed: {e:#}");
        }
        tokio::time::sleep(every).await;
    }
}
