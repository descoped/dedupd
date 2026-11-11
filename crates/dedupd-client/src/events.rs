//! Following the daemon: one connection that stays open and delivers a line
//! whenever something changes.
//!
//! The work is one thread that sleeps in a read. It costs nothing while
//! nothing happens, holds one event at a time, and finds the daemon again
//! by itself when it has been away.

use crate::types::DaemonEvent;
use crate::{agent, url};
use std::io::{BufRead, BufReader};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The longest wait between two tries to reach a daemon that is away.
const MAX_BACKOFF: Duration = Duration::from_secs(5);
/// A line longer than this is not an event.
const MAX_LINE: usize = 8 * 1024 * 1024;

/// Receives what the daemon says. Called on a background thread.
#[uniffi::export(with_foreign)]
pub trait EventListener: Send + Sync {
    /// The daemon's state as of now. The first one arrives as soon as the
    /// connection is made.
    fn on_event(&self, event: DaemonEvent);
    /// The connection was made (`true`) or lost (`false`). After a loss the
    /// subscription keeps trying; `on_event` follows when it succeeds.
    fn on_connection(&self, connected: bool);
}

/// A running subscription. It ends when `cancel` is called or the last
/// reference is dropped.
#[derive(uniffi::Object)]
pub struct EventSubscription {
    stopped: Arc<AtomicBool>,
}

#[uniffi::export]
impl EventSubscription {
    /// Stop following. Nothing is delivered after this returns.
    pub fn cancel(&self) {
        self.stopped.store(true, Ordering::Relaxed);
    }
}

impl Drop for EventSubscription {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Follow the daemon at `address`. Events go to `listener` until the
/// returned subscription is cancelled.
#[uniffi::export]
pub fn subscribe(address: String, listener: Arc<dyn EventListener>) -> Arc<EventSubscription> {
    let stopped = Arc::new(AtomicBool::new(false));
    let flag = stopped.clone();
    let spawned = std::thread::Builder::new()
        .name("dedupd-events".into())
        .stack_size(256 * 1024)
        .spawn(move || follow(&address, listener.as_ref(), &flag));
    if spawned.is_err() {
        stopped.store(true, Ordering::Relaxed);
    }
    Arc::new(EventSubscription { stopped })
}

fn follow(address: &str, listener: &dyn EventListener, stopped: &AtomicBool) {
    let done = || stopped.load(Ordering::Relaxed);
    let mut connected = false;
    let mut backoff = Duration::from_millis(250);
    while !done() {
        if let Some(lines) = open(address) {
            let mut lines = BufReader::with_capacity(16 * 1024, lines);
            let mut line = String::new();
            loop {
                line.clear();
                match lines.read_line(&mut line) {
                    Ok(n) if n > 0 && line.len() <= MAX_LINE => {}
                    _ => break,
                }
                if done() {
                    return;
                }
                let Ok(event) = serde_json::from_str::<DaemonEvent>(&line) else {
                    continue;
                };
                if !connected {
                    connected = true;
                    listener.on_connection(true);
                }
                backoff = Duration::from_millis(250);
                listener.on_event(event);
            }
        }
        if done() {
            return;
        }
        if connected {
            connected = false;
            listener.on_connection(false);
        }
        // Wait in short steps, so a cancel does not have to wait it out.
        let mut waited = Duration::ZERO;
        while waited < backoff && !done() {
            std::thread::sleep(Duration::from_millis(50));
            waited += Duration::from_millis(50);
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// The event stream's body, when the daemon answers.
fn open(address: &str) -> Option<impl std::io::Read> {
    // No time limit on the body: the daemon sends a line at least every
    // fifteen seconds, and a daemon that dies closes the connection.
    let resp = agent(None).get(url(address, "events")).call().ok()?;
    (resp.status().as_u16() == 200).then(|| resp.into_body().into_reader())
}
