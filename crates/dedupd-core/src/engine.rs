//! What every operation needs: the database, the store, the provider and
//! the app folders. Shared by the command line, the daemon and its queue.

use crate::db::{self, Db};
use crate::fetch::Fetcher;
use crate::governor::Governor;
use crate::hub::HubClient;
use crate::place::AppDirs;
use crate::store::Store;
use anyhow::Result;
use std::sync::{Arc, Mutex};

/// Everything an operation needs.
pub struct Engine {
    pub db: Mutex<Db>,
    pub store: Store,
    pub hub: HubClient,
    pub fetcher: Fetcher,
    pub dirs: AppDirs,
    /// Paces every request to the provider; shared by `hub` and `fetcher`.
    pub governor: Arc<Governor>,
}

impl Engine {
    pub fn db(&self) -> std::sync::MutexGuard<'_, Db> {
        db::lock(&self.db)
    }
}

/// Run blocking store work (a file may be copied back from the archive disk)
/// off the async runtime.
pub(crate) async fn blocking<T: Send + 'static>(
    engine: &Arc<Engine>,
    work: impl FnOnce(&Engine) -> Result<T> + Send + 'static,
) -> Result<T> {
    let engine = engine.clone();
    tokio::task::spawn_blocking(move || work(&engine)).await?
}
