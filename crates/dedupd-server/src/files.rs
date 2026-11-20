//! Serving stored files with `Range`, and keeping a copy of large files that
//! clients download through dedupd.

use crate::{Call, Server, held, plain};
use axum::body::{Body, Bytes};
use axum::http::{StatusCode, header};
use axum::response::Response;
use dedupd_core::hash::hash_file;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

/// Parse a `Range` header for a file of `size` bytes into an inclusive
/// `(start, end)`. `Ok(None)` means the whole file; `Err` means the range
/// cannot be satisfied.
pub(crate) fn parse_range(value: Option<&str>, size: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(spec) = value.trim().strip_prefix("bytes=") else {
        return Ok(None);
    };
    let (a, b) = spec.split_once('-').ok_or(())?;
    let range = if a.is_empty() {
        let n: u64 = b.parse().map_err(|_| ())?;
        (size.saturating_sub(n), size.saturating_sub(1))
    } else {
        let start: u64 = a.parse().map_err(|_| ())?;
        let end = if b.is_empty() {
            size.saturating_sub(1)
        } else {
            b.parse::<u64>()
                .map_err(|_| ())?
                .min(size.saturating_sub(1))
        };
        (start, end)
    };
    if size == 0 || range.0 > range.1 || range.0 >= size {
        return Err(());
    }
    Ok(Some(range))
}

/// Answer with a file from disk. `extra` headers are added to every answer.
pub(crate) async fn serve_file(
    call: &Call,
    path: &Path,
    size: u64,
    extra: &[(&str, String)],
) -> Response {
    let mut out = Response::builder()
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_TYPE, "application/octet-stream");
    for (k, v) in extra {
        out = out.header(*k, v);
    }
    let range = match parse_range(call.header(header::RANGE), size) {
        Ok(r) => r,
        Err(()) => {
            return out
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{size}"))
                .body(Body::empty())
                .expect("valid response");
        }
    };
    let (start, end) = range.unwrap_or((0, size.saturating_sub(1)));
    let length = if size == 0 { 0 } else { end - start + 1 };
    out = match range {
        Some(_) => out
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}")),
        None => out.status(StatusCode::OK),
    };
    out = out.header(header::CONTENT_LENGTH, length);
    if call.is_head() || length == 0 {
        return out.body(Body::empty()).expect("valid response");
    }
    let mut file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) => return plain(StatusCode::INTERNAL_SERVER_ERROR, format!("store: {e}")),
    };
    if start > 0 && file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        return plain(StatusCode::INTERNAL_SERVER_ERROR, "store: seek failed");
    }
    let stream = tokio_util::io::ReaderStream::with_capacity(file.take(length), 1 << 20);
    out.body(Body::from_stream(stream)).expect("valid response")
}

/// Answer with the stored file of this sha256, copying it back from the
/// archive disk first when it is only there. `None` when dedupd does not
/// hold it. Without `size`, the file's own length is used.
pub(crate) async fn serve_stored(
    server: &Server,
    call: &Call,
    sha256: &str,
    size: Option<u64>,
    extra: &[(&str, String)],
) -> Option<Response> {
    let sha = sha256.to_string();
    // Its length is read in the same step: a file that went away in
    // between (a `gc --offload` at that moment) is not held, and is never
    // answered as an empty one.
    let local = server
        .off(move |e| -> anyhow::Result<Option<u64>> {
            if !e.store.ensure_local(&sha)? {
                return Ok(None);
            }
            Ok(Some(std::fs::metadata(e.store.blob_path(&sha))?.len()))
        })
        .await;
    let on_disk = match local {
        Ok(found) => found?,
        Err(e) => {
            eprintln!("dedupd: {sha256} could not be read from the store: {e:#}");
            return None;
        }
    };
    let file = server.engine.store.blob_path(sha256);
    Some(serve_file(call, &file, size.unwrap_or(on_disk), extra).await)
}

/// A large file being put together from the ranges clients download. When
/// every byte has passed through and no part is still being written, it is
/// hashed and moved into the store.
pub(crate) struct Assembly {
    sha256: String,
    size: u64,
    path: PathBuf,
    file: std::fs::File,
    state: Mutex<Progress>,
}

struct Progress {
    covered: Vec<(u64, u64)>,
    /// The parts that still write through `file`. The file is not hashed
    /// while there is one: a part that is slow, or asked for a second time,
    /// could otherwise write into the file after it was found good and
    /// moved into the store.
    open: usize,
    /// The file is being hashed; no new part is taken.
    finishing: bool,
    /// When a part last ended.
    touched: Instant,
}

impl Progress {
    /// Record that `[start, end)` is on disk. True when the file is complete.
    fn add(&mut self, start: u64, end: u64, size: u64) -> bool {
        if end > start {
            self.covered.push((start, end));
            self.covered.sort();
            let mut merged: Vec<(u64, u64)> = Vec::new();
            for &(s, e) in &self.covered {
                match merged.last_mut() {
                    Some(last) if s <= last.1 => last.1 = last.1.max(e),
                    _ => merged.push((s, e)),
                }
            }
            self.covered = merged;
        }
        self.covered.as_slice() == [(0, size)]
    }
}

/// How long an assembly no part writes to is kept. A client that stopped
/// part of the way (`ollama pull` cancelled) leaves one behind, up to the
/// size of the model.
pub(crate) const ASSEMBLY_IDLE: Duration = Duration::from_secs(15 * 60);

/// The files being put together now, by sha256.
#[derive(Default)]
pub(crate) struct Assemblies(Mutex<std::collections::HashMap<String, Arc<Assembly>>>);

impl Assemblies {
    /// The assembly for this file with one more part open on it, or `None`
    /// when no copy is kept of this answer. A new one's file is made under
    /// the lock, so two first parts do not make two: one `open`, short.
    fn open_part(&self, at: PathBuf, sha256: &str, size: u64) -> Option<Arc<Assembly>> {
        let mut map = held(&self.0);
        if let Some(a) = map.get(sha256) {
            let mut state = held(&a.state);
            if a.size != size || state.finishing {
                return None;
            }
            state.open += 1;
            return Some(a.clone());
        }
        let file = match std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&at)
        {
            Ok(file) => file,
            Err(e) => {
                eprintln!("dedupd: no copy is kept of {sha256}: {e}");
                return None;
            }
        };
        let a = Arc::new(Assembly {
            sha256: sha256.to_string(),
            size,
            path: at,
            file,
            state: Mutex::new(Progress {
                covered: Vec::new(),
                open: 1,
                finishing: false,
                touched: Instant::now(),
            }),
        });
        map.insert(sha256.to_string(), a.clone());
        Some(a)
    }

    /// Drop the assemblies no part has written to for `idle`, with their
    /// files. Returns how many went.
    pub(crate) fn drop_idle(&self, idle: Duration) -> usize {
        let mut map = held(&self.0);
        let gone: Vec<String> = map
            .iter()
            .filter(|(_, a)| {
                let state = held(&a.state);
                state.open == 0 && !state.finishing && state.touched.elapsed() >= idle
            })
            .map(|(sha, _)| sha.clone())
            .collect();
        let dropped: Vec<_> = gone.iter().filter_map(|sha| map.remove(sha)).collect();
        // The files go after the lock is given back; no request waits on
        // an unlink.
        drop(map);
        for assembly in &dropped {
            let _ = std::fs::remove_file(&assembly.path);
        }
        dropped.len()
    }

    /// How many files are being put together now.
    pub(crate) fn open(&self) -> usize {
        held(&self.0).len()
    }
}

/// A part finished (or was cut short), and writes no more. If the file is
/// now whole and no other part is open, check it and move it into the
/// store.
fn part_done(server: &Arc<Server>, asm: &Arc<Assembly>, start: u64, end: u64) {
    {
        let mut state = held(&asm.state);
        let whole = state.add(start, end, asm.size);
        state.open = state.open.saturating_sub(1);
        state.touched = Instant::now();
        if !whole || state.open > 0 || state.finishing {
            return;
        }
        state.finishing = true;
    }
    let (server, asm) = (server.clone(), asm.clone());
    tokio::task::spawn_blocking(move || {
        let ok = match hash_file(&asm.path, false) {
            Ok(h) if h.sha256 == asm.sha256 => true,
            Ok(h) => {
                eprintln!(
                    "dedupd: what clients downloaded as {} is {}; no copy is kept",
                    asm.sha256, h.sha256
                );
                false
            }
            Err(e) => {
                eprintln!("dedupd: checking the copy of {}: {e:#}", asm.sha256);
                false
            }
        };
        let stored = ok
            && match server.engine.store.add_tmp(&asm.path, &asm.sha256) {
                Ok(_) => true,
                Err(e) => {
                    eprintln!("dedupd: storing {}: {e:#}", asm.sha256);
                    false
                }
            };
        if stored && let Err(e) = server.engine.db().upsert_blob(&asm.sha256, asm.size, None) {
            eprintln!("dedupd: recording {}: {e:#}", asm.sha256);
        }
        // Whatever kept the file out of the store, its copy goes.
        if !stored {
            let _ = std::fs::remove_file(&asm.path);
        }
        held(&server.assemblies.0).remove(&asm.sha256);
    });
}

struct Part {
    server: Arc<Server>,
    asm: Arc<Assembly>,
    start: u64,
    pos: u64,
    broken: bool,
}

impl Drop for Part {
    fn drop(&mut self) {
        if !self.broken {
            part_done(&self.server, &self.asm, self.start, self.pos);
        }
    }
}

/// Relay an upstream download of the file with this sha256 to the client,
/// writing the same bytes into the file's assembly on the way when `keep`.
fn tee(server: &Arc<Server>, resp: reqwest::Response, sha256: &str, keep: bool) -> Body {
    let status = resp.status().as_u16();
    let header = |name: header::HeaderName| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    // Where this answer starts in the file, and how long the whole file is.
    let placed = match status {
        200 => header(header::CONTENT_LENGTH)
            .and_then(|v| v.parse::<u64>().ok())
            .map(|size| (0, size)),
        206 => header(header::CONTENT_RANGE).and_then(|v| {
            let (range, total) = v.strip_prefix("bytes ")?.split_once('/')?;
            let start = range.split_once('-')?.0.parse::<u64>().ok()?;
            Some((start, total.parse::<u64>().ok()?))
        }),
        _ => None,
    };
    let part = placed.filter(|_| keep).and_then(|(start, size)| {
        let at = server.engine.store.tmp_path(&format!("{sha256}.asm"));
        server
            .assemblies
            .open_part(at, sha256, size)
            .map(|asm| Part {
                server: server.clone(),
                asm,
                start,
                pos: start,
                broken: false,
            })
    });

    let stream = futures_util::stream::unfold(
        (resp, part, false),
        |(mut resp, mut part, ended)| async move {
            if ended {
                return None;
            }
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    if let Some(p) = part.as_mut()
                        && !p.broken
                    {
                        // One chunk into the system's cache, on this thread:
                        // short, and in the order the bytes arrive.
                        match p.asm.file.write_all_at(&chunk, p.pos) {
                            Ok(()) => p.pos += chunk.len() as u64,
                            Err(e) => {
                                // Keep serving the client; stop keeping a
                                // copy past the bytes that did reach the
                                // disk.
                                eprintln!(
                                    "dedupd: the copy of {} stops at {}: {e}",
                                    p.asm.sha256, p.pos
                                );
                                part_done(&p.server, &p.asm, p.start, p.pos);
                                p.broken = true;
                            }
                        }
                    }
                    Some((Ok::<Bytes, std::io::Error>(chunk), (resp, part, false)))
                }
                Ok(None) => None,
                Err(e) => Some((Err(std::io::Error::other(e)), (resp, part, true))),
            }
        },
    );
    Body::from_stream(stream)
}

/// Relay an upstream 200 or 206 for the file with this sha256, keeping a
/// copy of the bytes (see `tee`). `extra` headers are added to the answer.
/// The answer to a `HEAD` carries no bytes, so no copy is started for it.
pub(crate) fn tee_response(
    server: &Arc<Server>,
    call: &Call,
    resp: reqwest::Response,
    sha256: &str,
    content_type: Option<&str>,
    extra: &[(&str, String)],
) -> Response {
    let mut out = Response::builder().status(resp.status().as_u16());
    for name in [header::CONTENT_LENGTH, header::CONTENT_RANGE] {
        if let Some(v) = resp.headers().get(&name) {
            out = out.header(name, v);
        }
    }
    out = out.header(header::ACCEPT_RANGES, "bytes");
    if let Some(content_type) = content_type {
        out = out.header(header::CONTENT_TYPE, content_type);
    }
    for (k, v) in extra {
        out = out.header(*k, v);
    }
    let keep = call.method != axum::http::Method::HEAD;
    out.body(tee(server, resp, sha256, keep))
        .expect("valid response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_range(None, 10), Ok(None));
        assert_eq!(parse_range(Some("bytes=0-3"), 10), Ok(Some((0, 3))));
        assert_eq!(parse_range(Some("bytes=4-"), 10), Ok(Some((4, 9))));
        assert_eq!(parse_range(Some("bytes=-3"), 10), Ok(Some((7, 9))));
        assert_eq!(parse_range(Some("bytes=5-99"), 10), Ok(Some((5, 9))));
        assert_eq!(parse_range(Some("bytes=10-"), 10), Err(()));
        assert_eq!(parse_range(Some("bytes=0-"), 0), Err(()));
    }
}
