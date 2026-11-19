//! The favorites, and bringing in the likes on Hugging Face: read in the
//! background once they are older than an hour, and on request.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use dedupd_api::favorites::Favorites;
use dedupd_core::favorites::{self, Kept};

use crate::Server;

/// What the daemon holds for the favorites between calls.
#[derive(Default)]
pub(crate) struct State {
    /// Held while the file is read and written, so two changes do not undo
    /// each other.
    file: Mutex<()>,
    /// Set while the likes are being read.
    reading: AtomicBool,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

fn answer(server: &Server, kept: &Kept) -> Favorites {
    kept.answer(server.engine.hub.has_token())
}

/// Read the Hugging Face token again: signing in or out with `hf auth`
/// takes effect without a restart. A new token's likes are read at once,
/// asking Hugging Face whose it is.
pub(crate) fn refresh_token(server: &Arc<Server>) {
    if !server.engine.hub.token().refresh() {
        return;
    }
    let now_has = server.engine.hub.has_token();
    eprintln!(
        "dedupd: the Hugging Face token {}",
        if now_has { "changed" } else { "was removed" }
    );
    server.touch();
    if now_has {
        let server = server.clone();
        tokio::spawn(async move {
            if let Err(e) = sync(&server, true).await {
                eprintln!("dedupd: favorites: {e:#}");
            }
        });
    }
}

/// The favorites as they are.
pub(crate) fn current(server: &Server) -> Favorites {
    let _held = server.favorites.file.lock().expect("lock");
    answer(server, &Kept::load(&server.home))
}

/// Mark a model as a favorite, or no longer.
pub(crate) fn set(server: &Server, repo: &str, favorite: bool) -> anyhow::Result<Favorites> {
    let kept = {
        let _held = server.favorites.file.lock().expect("lock");
        favorites::set(&server.home, repo, favorite, now())?
    };
    server.touch();
    Ok(answer(server, &kept))
}

/// Clears `reading` when the reading ends, however it ends.
struct Reading<'a>(&'a AtomicBool);

impl Drop for Reading<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Read the likes now. `by_hand` also asks Hugging Face again whose token
/// it is. A reading that is already going is not started twice. The
/// reading runs on its own task, so a caller that goes away (a request cut
/// off) does not cut it off.
pub(crate) async fn sync(server: &Arc<Server>, by_hand: bool) -> anyhow::Result<Favorites> {
    let task = server.clone();
    tokio::spawn(async move { read_now(&task, by_hand).await }).await?
}

async fn read_now(server: &Arc<Server>, by_hand: bool) -> anyhow::Result<Favorites> {
    if server.favorites.reading.swap(true, Ordering::SeqCst) {
        return Ok(current(server));
    }
    let _reading = Reading(&server.favorites.reading);
    let known = {
        let _held = server.favorites.file.lock().expect("lock");
        Kept::load(&server.home).user
    };
    let read = favorites::read_likes(&server.engine.hub, known, by_hand).await;
    let kept = read.map(|likes| {
        let _held = server.favorites.file.lock().expect("lock");
        favorites::record_likes(&server.home, likes, now())
    });
    match kept {
        Some(kept) => {
            let kept = kept?;
            server.touch();
            Ok(answer(server, &kept))
        }
        None => Ok(current(server)),
    }
}

/// Read the likes in the background when they are due.
pub(crate) fn sync_if_due(server: &Arc<Server>) {
    let due = server.engine.hub.has_token() && {
        let _held = server.favorites.file.lock().expect("lock");
        Kept::load(&server.home).likes_due(now())
    };
    if due {
        let server = server.clone();
        tokio::spawn(async move {
            if let Err(e) = sync(&server, false).await {
                eprintln!("dedupd: favorites: {e:#}");
            }
        });
    }
}
