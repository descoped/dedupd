//! Serving stored files with `Range`, and keeping a copy of large files that
//! clients download through dedupd.

use crate::{Call, Server, plain};
use axum::body::{Body, Bytes};
use axum::http::{StatusCode, header};
use axum::response::Response;
use dedupd_core::hash::hash_file;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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
    let (engine, sha) = (server.engine.clone(), sha256.to_string());
    let local = tokio::task::spawn_blocking(move || engine.store.ensure_local(&sha)).await;
    if !matches!(local, Ok(Ok(true))) {
        return None;
    }
    let file = server.engine.store.blob_path(sha256);
    let size = size.unwrap_or_else(|| std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0));
    Some(serve_file(call, &file, size, extra).await)
}

/// A large file being put together from the ranges clients download. When
/// every byte has passed through, it is hashed and moved into the store.
pub(crate) struct Assembly {
    sha256: String,
    size: u64,
    path: PathBuf,
    file: std::fs::File,
    covered: Mutex<Vec<(u64, u64)>>,
    finishing: AtomicBool,
}

impl Assembly {
    /// Record that `[start, end)` is on disk. True when the file is complete.
    fn add(&self, start: u64, end: u64) -> bool {
        let mut covered = self.covered.lock().expect("lock");
        if end > start {
            covered.push((start, end));
            covered.sort();
            let mut merged: Vec<(u64, u64)> = Vec::new();
            for &(s, e) in covered.iter() {
                match merged.last_mut() {
                    Some(last) if s <= last.1 => last.1 = last.1.max(e),
                    _ => merged.push((s, e)),
                }
            }
            *covered = merged;
        }
        covered.as_slice() == [(0, self.size)]
    }
}

impl Server {
    fn assembly(&self, sha256: &str, size: u64) -> Option<Arc<Assembly>> {
        let mut map = self.assemblies.lock().expect("lock");
        if let Some(a) = map.get(sha256) {
            return (a.size == size).then(|| a.clone());
        }
        let path = self.engine.store.tmp_path(&format!("{sha256}.asm"));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .ok()?;
        let a = Arc::new(Assembly {
            sha256: sha256.to_string(),
            size,
            path,
            file,
            covered: Mutex::new(Vec::new()),
            finishing: AtomicBool::new(false),
        });
        map.insert(sha256.to_string(), a.clone());
        Some(a)
    }

    /// A part finished (or was cut short). If the file is now whole, check
    /// it and move it into the store.
    fn part_done(self: &Arc<Self>, asm: &Arc<Assembly>, start: u64, end: u64) {
        if !asm.add(start, end) || asm.finishing.swap(true, Ordering::SeqCst) {
            return;
        }
        let (server, asm) = (self.clone(), asm.clone());
        tokio::task::spawn_blocking(move || {
            let ok = hash_file(&asm.path, false).is_ok_and(|h| h.sha256 == asm.sha256);
            if ok
                && server.engine.store.add_tmp(&asm.path, &asm.sha256).is_ok()
                && let Err(e) = server.engine.db().upsert_blob(&asm.sha256, asm.size, None)
            {
                eprintln!("dedupd: recording {}: {e:#}", asm.sha256);
            }
            if !ok {
                let _ = std::fs::remove_file(&asm.path);
            }
            server.assemblies.lock().expect("lock").remove(&asm.sha256);
        });
    }
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
            self.server.part_done(&self.asm, self.start, self.pos);
        }
    }
}

/// Relay an upstream download of the file with this sha256 to the client,
/// writing the same bytes into the file's assembly on the way.
fn tee(server: &Arc<Server>, resp: reqwest::Response, sha256: &str) -> Body {
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
    let part = placed.and_then(|(start, size)| {
        server.assembly(sha256, size).map(|asm| Part {
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
                        if p.asm.file.write_all_at(&chunk, p.pos).is_ok() {
                            p.pos += chunk.len() as u64;
                        } else {
                            // Keep serving the client; stop keeping a copy
                            // past the bytes that did reach the disk.
                            p.server.part_done(&p.asm, p.start, p.pos);
                            p.broken = true;
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
pub(crate) fn tee_response(
    server: &Arc<Server>,
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
    out.body(tee(server, resp, sha256)).expect("valid response")
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
