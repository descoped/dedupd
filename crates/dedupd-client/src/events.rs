//! Following the daemon: one connection that stays open and delivers a line
//! whenever something changes.
//!
//! The work is one thread that sleeps in a read. It costs nothing while
//! nothing happens, holds one event at a time, and finds the daemon again
//! by itself when it has been away.

use crate::{agent, url};
use dedupd_api::daemon::DaemonEvent;
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
    /// The connection was made (`true`), or it was lost or could not be made
    /// (`false`). After that the subscription keeps trying; `on_event`
    /// follows when it succeeds.
    fn on_connection(&self, connected: bool);
    /// The daemon answers, but this client cannot read what it says: it is
    /// another build of dedupd, with other types. `reason` says what did
    /// not fit. Told once until a line can be read again.
    fn on_unreadable(&self, reason: String);
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
    // What the listener was told last; nothing yet, so that a daemon that
    // is away from the start is reported too.
    let mut connected: Option<bool> = None;
    // Whether the listener was told that the daemon's lines cannot be read.
    let mut unreadable = false;
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
                let event = match serde_json::from_str::<DaemonEvent>(&line) {
                    Ok(event) => event,
                    Err(e) => {
                        if !unreadable {
                            unreadable = true;
                            listener.on_unreadable(e.to_string());
                        }
                        continue;
                    }
                };
                unreadable = false;
                if connected != Some(true) {
                    connected = Some(true);
                    listener.on_connection(true);
                }
                backoff = Duration::from_millis(250);
                listener.on_event(event);
            }
        }
        if done() {
            return;
        }
        if connected != Some(false) {
            connected = Some(false);
            unreadable = false;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Heard {
        events: Mutex<u32>,
        unreadable: Mutex<Vec<String>>,
        connections: Mutex<Vec<bool>>,
    }

    impl EventListener for Heard {
        fn on_event(&self, _event: DaemonEvent) {
            *self.events.lock().unwrap() += 1;
        }
        fn on_connection(&self, connected: bool) {
            self.connections.lock().unwrap().push(connected);
        }
        fn on_unreadable(&self, reason: String) {
            self.unreadable.lock().unwrap().push(reason);
        }
    }

    /// A daemon of another build: its lines carry a job state this build
    /// does not know.
    #[test]
    fn a_daemon_whose_lines_cannot_be_read_is_reported_once() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = server.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            let (mut stream, _) = server.accept().unwrap();
            let line = r#"{"seq":1,"library":0,"checking":false,"downloads":{"jobs":[{"state":"teleporting"}]}}"#;
            let body = format!("{line}\n{line}\n{line}\n");
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            );
            std::thread::sleep(Duration::from_millis(300));
        });
        let heard = Arc::new(Heard::default());
        let subscription = subscribe(address, heard.clone());
        for _ in 0..100 {
            if !heard.unreadable.lock().unwrap().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(50));
        subscription.cancel();
        assert_eq!(heard.unreadable.lock().unwrap().len(), 1);
        assert_eq!(*heard.events.lock().unwrap(), 0);
        assert!(!heard.connections.lock().unwrap().contains(&true));
    }
}
