//! Following the daemon: one connection that stays open and delivers a line
//! whenever something changes.
//!
//! The work is one thread that sleeps in a read. It costs nothing while
//! nothing happens, holds one event at a time, and finds the daemon again
//! by itself when it has been away.

use dedupd_api::daemon::DaemonEvent;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The longest wait between two tries to reach a daemon that is away.
const MAX_BACKOFF: Duration = Duration::from_secs(5);
/// A line longer than this is not an event. No more than this is read
/// into memory for one line.
const MAX_LINE: usize = 8 * 1024 * 1024;
/// The daemon sends a line at least every fifteen seconds. One that sends
/// nothing for this long is not answering: it is stopped, stuck, or
/// something else listens at the address. The connection is then given up
/// and made again, as if it had been closed.
const SILENCE: Duration = Duration::from_secs(45);

/// Receives what the daemon says. Called on a background thread, with one
/// exception: when the subscription's thread cannot be started,
/// `on_connection(false)` is called once on the thread that subscribed,
/// before `subscribe` returns. A listener must take a call from any thread.
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
    /// Stop following. The thread ends at its next step: at most one more
    /// call may reach the listener after this returns, and with a daemon
    /// that sends nothing the thread ends within the silence limit.
    pub fn cancel(&self) {
        // A flag that is only ever set; no other state depends on it.
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
    subscribe_with(address, listener, SILENCE)
}

fn subscribe_with(
    address: String,
    listener: Arc<dyn EventListener>,
    silence: Duration,
) -> Arc<EventSubscription> {
    let stopped = Arc::new(AtomicBool::new(false));
    let (flag, heard) = (stopped.clone(), listener.clone());
    let spawned = std::thread::Builder::new()
        .name("dedupd-events".into())
        .stack_size(256 * 1024)
        .spawn(move || follow(&address, heard.as_ref(), &flag, silence));
    if spawned.is_err() {
        // No thread could be started, so nothing will ever be told. Say
        // that much, so the listener does not wait as if connecting.
        stopped.store(true, Ordering::Relaxed);
        listener.on_connection(false);
    }
    Arc::new(EventSubscription { stopped })
}

fn follow(address: &str, listener: &dyn EventListener, stopped: &AtomicBool, silence: Duration) {
    let done = || stopped.load(Ordering::Relaxed);
    // What the listener was told last; nothing yet, so that a daemon that
    // is away from the start is reported too.
    let mut connected: Option<bool> = None;
    // Whether the listener was told that the daemon's lines cannot be read.
    let mut unreadable = false;
    let mut backoff = Duration::from_millis(250);
    while !done() {
        if let Some(lines) = open(address, silence) {
            let mut lines = BufReader::with_capacity(16 * 1024, lines);
            let mut line = String::new();
            loop {
                line.clear();
                // Through a bound, so a peer that never ends its line cannot
                // fill the memory. A line that reaches the bound, a closed
                // connection and a daemon gone silent all end this
                // connection.
                let mut bounded = (&mut lines).take(MAX_LINE as u64 + 1);
                match bounded.read_line(&mut line) {
                    Ok(n) if n > 0 && line.len() <= MAX_LINE && line.ends_with('\n') => {}
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
///
/// The connection is made by hand, not through the HTTP client the other
/// calls use: this one stays open for as long as the app runs, and it
/// needs a limit on each read (how long the daemon may be silent), which
/// that client only has for a whole answer. The daemon is on this Mac and
/// speaks plain HTTP/1.1.
fn open(address: &str, silence: Duration) -> Option<Body> {
    crate::url(address, "events").ok()?;
    // Every address the name stands for is tried, as the other calls do:
    // a daemon may listen on only one of them.
    let stream = address
        .to_socket_addrs()
        .ok()?
        .find_map(|to| TcpStream::connect_timeout(&to, Duration::from_secs(2)).ok())?;
    stream.set_read_timeout(Some(silence)).ok()?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .ok()?;
    let mut stream = BufReader::with_capacity(16 * 1024, stream);
    write!(
        stream.get_mut(),
        "GET /dedupd/v1/events HTTP/1.1\r\nhost: {address}\r\naccept: application/x-ndjson\r\nconnection: close\r\n\r\n"
    )
    .ok()?;
    let mut head = String::new();
    let mut chunked = false;
    let mut first = true;
    loop {
        head.clear();
        let n = (&mut stream)
            .take(HEAD_LINE + 1)
            .read_line(&mut head)
            .ok()?;
        if n == 0 || n as u64 > HEAD_LINE {
            return None;
        }
        let line = head.trim_end();
        if first {
            // "HTTP/1.1 200 OK"
            let mut words = line.split(' ');
            if !words.next()?.starts_with("HTTP/1.") || words.next()? != "200" {
                return None;
            }
            first = false;
        } else if line.is_empty() {
            break;
        } else if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("transfer-encoding")
        {
            chunked = value.trim().eq_ignore_ascii_case("chunked");
        }
    }
    Some(Body {
        stream,
        chunked,
        left: 0,
        ended: false,
    })
}

/// The longest line of an answer's head that is read.
const HEAD_LINE: u64 = 16 * 1024;

/// The bytes of an answer's body: as they come, or out of their chunks.
struct Body {
    stream: BufReader<TcpStream>,
    chunked: bool,
    /// Bytes left of the chunk being read.
    left: u64,
    ended: bool,
}

impl Read for Body {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if !self.chunked {
            return self.stream.read(buf);
        }
        if self.ended || buf.is_empty() {
            return Ok(0);
        }
        if self.left == 0 {
            // "<length in hex>[;extension]\r\n"
            let mut size = String::new();
            let n = (&mut self.stream).take(64).read_line(&mut size)?;
            let digits = size.trim_end().split(';').next().unwrap_or("").trim();
            let length = u64::from_str_radix(digits, 16).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "not a chunk length")
            });
            if n == 0 {
                return Ok(0);
            }
            self.left = length?;
            if self.left == 0 {
                self.ended = true;
                return Ok(0);
            }
        }
        let want = buf
            .len()
            .min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let n = self.stream.read(&mut buf[..want])?;
        if n == 0 {
            return Ok(0);
        }
        self.left -= n as u64;
        if self.left == 0 {
            // The line end after the chunk's bytes.
            let mut end = [0u8; 2];
            self.stream.read_exact(&mut end)?;
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn wait_until(what: impl Fn() -> bool) -> bool {
        for _ in 0..500 {
            if what() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// A stand-in daemon that answers the first connection with `answer`
    /// and then does `then` with the open connection.
    fn stand_in(
        answer: &'static str,
        then: impl FnOnce(std::net::TcpStream) + Send + 'static,
    ) -> String {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = server.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            let (mut stream, _) = server.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(answer.as_bytes());
            then(stream);
        });
        address
    }

    const EVENT: &str = r#"{"checking":false,"downloads":{"governor":{"budgets":[],"connections":0,"hold_seconds":0,"max_connections":8,"speed_limit":0,"window":2},"jobs":[],"speed":0},"library":0,"moving":null,"seq":1}"#;

    #[test]
    fn lines_are_read_out_of_their_chunks() {
        let line = format!("{EVENT}\n");
        // One line in two chunks, as a server may cut it.
        let (a, b) = line.split_at(20);
        let answer = format!(
            "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n{:x}\r\n{a}\r\n{:x}\r\n{b}\r\n",
            a.len(),
            b.len()
        );
        let address = stand_in(Box::leak(answer.into_boxed_str()), |stream| {
            std::thread::sleep(Duration::from_millis(500));
            drop(stream);
        });
        let heard = Arc::new(Heard::default());
        let subscription = subscribe(address, heard.clone());
        assert!(wait_until(|| *heard.events.lock().unwrap() == 1));
        assert_eq!(heard.connections.lock().unwrap().first(), Some(&true));
        subscription.cancel();
    }

    #[test]
    fn a_daemon_that_goes_silent_is_reported_as_away() {
        // It answers, sends one line, and then holds the connection open
        // and says nothing: stopped, or stuck.
        let answer = Box::leak(
            format!("HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\n\r\n{EVENT}\n")
                .into_boxed_str(),
        );
        let address = stand_in(answer, |stream| {
            std::thread::sleep(Duration::from_secs(5));
            drop(stream);
        });
        let heard = Arc::new(Heard::default());
        let subscription = subscribe_with(address, heard.clone(), Duration::from_millis(200));
        assert!(wait_until(
            || *heard.connections.lock().unwrap() == [true, false]
        ));
        subscription.cancel();
    }

    #[test]
    fn a_line_that_never_ends_is_not_read_into_memory() {
        // A body with no line end at all, longer than a line may be.
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = server.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            let (mut stream, _) = server.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n");
            let block = vec![b'x'; 1024 * 1024];
            for _ in 0..(MAX_LINE / block.len() + 2) {
                if stream.write_all(&block).is_err() {
                    return;
                }
            }
            std::thread::sleep(Duration::from_secs(2));
        });
        let heard = Arc::new(Heard::default());
        let subscription = subscribe(address, heard.clone());
        // The connection is given up at the bound and the daemon is "away".
        assert!(wait_until(|| heard
            .connections
            .lock()
            .unwrap()
            .contains(&false)));
        assert_eq!(*heard.events.lock().unwrap(), 0);
        subscription.cancel();
    }

    #[test]
    fn a_daemon_that_was_away_is_found_again() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = server.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            // The first connection is closed at once; the second answers.
            drop(server.accept().unwrap());
            let (mut stream, _) = server.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            let _ = write!(stream, "HTTP/1.1 200 OK\r\n\r\n{EVENT}\n");
            std::thread::sleep(Duration::from_secs(2));
        });
        let heard = Arc::new(Heard::default());
        let subscription = subscribe(address, heard.clone());
        assert!(wait_until(
            || *heard.connections.lock().unwrap() == [false, true]
        ));
        assert_eq!(*heard.events.lock().unwrap(), 1);
        subscription.cancel();
    }

    #[test]
    fn an_address_that_is_not_one_is_never_connected_to() {
        let heard = Arc::new(Heard::default());
        let subscription = subscribe("http://127.0.0.1:1/x".into(), heard.clone());
        assert!(wait_until(|| *heard.connections.lock().unwrap() == [false]));
        subscription.cancel();
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
