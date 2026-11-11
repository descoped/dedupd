//! Downloads into the store: resumable, verified, and paced by the governor.
//!
//! A large file is fetched in parts with `Range`. Each finished part is
//! written down beside the partial file, so a download that is interrupted
//! (by a pause, a restart, a lost network) continues with the parts it does
//! not have yet. A small file, or one of unknown size, is one stream that
//! continues from the length of its partial file.

use crate::governor::{self, Class, Governor};
use crate::hash::hash_file;
use crate::store::Store;
use anyhow::{Result, anyhow};
use std::collections::{BTreeSet, HashMap};
use std::hash::{BuildHasher, Hasher};
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

/// The size of one part of a large download.
pub const PART_SIZE: u64 = 64 * 1024 * 1024;
/// Failures in a row after which a download gives up, unless it is patient.
const ATTEMPTS: u32 = 6;
/// The longest wait between two attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// What a downloaded file must hash to before it may enter the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    /// The sha256 of the content: the Hub's `lfs.oid`, an Ollama layer digest.
    Sha256(String),
    /// The git blob id, which is how the Hub names files that are not in LFS.
    GitOid(String),
}

impl Expect {
    /// The name of this download's partial file.
    fn key(&self) -> String {
        match self {
            Expect::Sha256(h) => h.clone(),
            Expect::GitOid(o) => format!("git-{o}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Fetched {
    pub sha256: String,
    pub size: u64,
    pub git_oid: Option<String>,
}

/// The server says the file is missing or not visible to this caller.
#[derive(Debug, thiserror::Error)]
#[error("{url} is not available (HTTP {status})")]
pub struct Unavailable {
    pub status: u16,
    pub url: String,
}

/// The volume the store is on cannot hold the download. `needed` is 0 when
/// the disk filled up while writing.
#[derive(Debug)]
pub struct NoSpace {
    pub needed: u64,
    pub free: u64,
}

impl std::error::Error for NoSpace {}

impl std::fmt::Display for NoSpace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.needed == 0 {
            return write!(f, "the disk is full");
        }
        write!(
            f,
            "not enough free space: {} more bytes are needed and {} are free",
            self.needed, self.free
        )
    }
}

/// What was downloaded does not hash to what the provider lists.
#[derive(Debug, thiserror::Error)]
#[error("{url} did not have the expected content; the download was discarded")]
pub struct WrongContent {
    pub url: String,
}

/// Why a download is not moving right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// The server asked for a pause of this many seconds.
    Server { seconds: u64 },
    /// The server cannot be reached; trying again.
    Network,
}

/// Follows one file while it is downloaded.
pub trait Watch: Send + Sync {
    /// The bytes of the file that are on disk changed by `delta`.
    fn bytes(&self, delta: i64);
    /// This many bytes just came over the network.
    fn received(&self, _bytes: u64) {}
    fn waiting(&self, _why: Wait) {}
    /// Bytes are moving again.
    fn running(&self) {}
    /// Every byte is here and the file is being hashed.
    fn verifying(&self) {}
}

/// Running totals a caller can read while downloads are in flight.
#[derive(Debug, Default)]
pub struct Progress {
    pub total: AtomicU64,
    pub done: AtomicU64,
}

impl Watch for Progress {
    fn bytes(&self, delta: i64) {
        if delta >= 0 {
            self.done.fetch_add(delta as u64, Ordering::Relaxed);
        } else {
            self.done.fetch_sub(delta.unsigned_abs(), Ordering::Relaxed);
        }
    }
}

/// For a download nobody follows.
pub struct Unwatched;

impl Watch for Unwatched {
    fn bytes(&self, _delta: i64) {}
}

/// Remembers what one download reported, so a failed one can take it back
/// and leave the caller's count as it found it.
struct Counted<'a> {
    inner: &'a dyn Watch,
    sum: AtomicI64,
}

impl Counted<'_> {
    fn undo(&self) {
        let sum = self.sum.swap(0, Ordering::Relaxed);
        if sum != 0 {
            self.inner.bytes(-sum);
        }
    }
}

impl Watch for Counted<'_> {
    fn bytes(&self, delta: i64) {
        self.sum.fetch_add(delta, Ordering::Relaxed);
        self.inner.bytes(delta);
    }
    fn received(&self, bytes: u64) {
        self.inner.received(bytes);
    }
    fn waiting(&self, why: Wait) {
        self.inner.waiting(why);
    }
    fn running(&self) {
        self.inner.running();
    }
    fn verifying(&self) {
        self.inner.verifying();
    }
}

/// One failed request, by what to do about it.
enum Failure {
    /// Trying again cannot help.
    Fatal(anyhow::Error),
    /// The server is busy (429, 503) and may have named a pause.
    Busy(Option<Duration>),
    /// The request did not get through, or broke off.
    Network(anyhow::Error),
    /// The server sent the whole file although a range was asked for.
    NoRanges,
}

pub struct Fetcher {
    http: reqwest::Client,
    /// The same client, except that it hands redirects back to the caller.
    http_no_redirect: reqwest::Client,
    endpoint: String,
    token: Option<String>,
    governor: Arc<Governor>,
    /// One download at a time per file, so two requests for the same file
    /// never write the same partial file.
    in_flight: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Keep trying for as long as it takes: the daemon's queue shows the
    /// wait and the owner can stop it. The command line gives up instead.
    patient: AtomicBool,
    part_size: u64,
    backoff: Duration,
}

impl Fetcher {
    pub fn new(endpoint: impl Into<String>, token: Option<String>) -> Result<Self> {
        let builder = || {
            reqwest::Client::builder()
                .user_agent(crate::USER_AGENT)
                .connect_timeout(Duration::from_secs(15))
                .read_timeout(Duration::from_secs(60))
        };
        let http = builder().build()?;
        let http_no_redirect = builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            http,
            http_no_redirect,
            endpoint: endpoint.into().trim_end_matches('/').to_string(),
            token,
            governor: Governor::new(governor::DEFAULT_CONNECTIONS, 0),
            in_flight: Mutex::new(HashMap::new()),
            patient: AtomicBool::new(false),
            part_size: PART_SIZE,
            backoff: Duration::from_secs(1),
        })
    }

    /// Share `governor` with everything else that asks the provider.
    pub fn with_governor(mut self, governor: Arc<Governor>) -> Self {
        self.governor = governor;
        self
    }

    /// For tests: smaller parts and shorter waits between attempts.
    pub fn with_tuning(mut self, part_size: u64, backoff: Duration) -> Self {
        self.part_size = part_size.max(1);
        self.backoff = backoff;
        self
    }

    /// Never give up on a server that is busy or cannot be reached.
    pub fn set_patient(&self, patient: bool) {
        self.patient.store(patient, Ordering::Relaxed);
    }

    pub fn governor(&self) -> &Arc<Governor> {
        &self.governor
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// For relaying an answer as it is: a redirect comes back as a redirect.
    pub fn http_no_redirect(&self) -> &reqwest::Client {
        &self.http_no_redirect
    }

    /// A GET for `url`. The token is attached only for the configured
    /// endpoint; on a redirect to another host the HTTP client drops it.
    pub fn get(&self, url: &str) -> reqwest::RequestBuilder {
        let mut req = self.http.get(url);
        if let Some(t) = &self.token
            && url.starts_with(&self.endpoint)
        {
            req = req.bearer_auth(t);
        }
        req
    }

    /// The `Authorization` value dedupd itself uses for the endpoint.
    pub fn authorization(&self) -> Option<String> {
        self.token.as_ref().map(|t| format!("Bearer {t}"))
    }

    pub fn resolve_url(&self, repo: &str, commit: &str, path: &str) -> String {
        format!(
            "{}/{repo}/resolve/{commit}/{}",
            self.endpoint,
            encode_path(path)
        )
    }

    pub fn registry_blob_url(&self, repo: &str, sha256: &str) -> String {
        format!("{}/v2/{repo}/blobs/sha256:{sha256}", self.endpoint)
    }

    /// Remove what an unfinished download of this file left behind.
    pub fn discard(&self, store: &Store, expect: &Expect) {
        let part = store.tmp_path(&format!("{}.part", expect.key()));
        let _ = std::fs::remove_file(parts_path(&part));
        let _ = std::fs::remove_file(part);
    }

    /// Download `url` into the store. An interrupted download continues from
    /// where it stopped, in this run or a later one. `watch` is told about
    /// every byte; when the download fails, what it was told is taken back.
    pub async fn fetch(
        &self,
        store: &Store,
        url: &str,
        size: Option<u64>,
        expect: &Expect,
        watch: &dyn Watch,
    ) -> Result<Fetched> {
        let key = expect.key();
        let lock = self
            .in_flight
            .lock()
            .expect("lock")
            .entry(key.clone())
            .or_default()
            .clone();
        let _guard = lock.lock().await;

        if let Expect::Sha256(h) = expect
            && store.contains(h)
        {
            let size = std::fs::metadata(store.blob_path(h))?.len();
            watch.bytes(size as i64);
            return Ok(Fetched {
                sha256: h.clone(),
                size,
                git_oid: None,
            });
        }

        let watch = Counted {
            inner: watch,
            sum: AtomicI64::new(0),
        };
        let part = store.tmp_path(&format!("{key}.part"));
        let want_git = matches!(expect, Expect::GitOid(_));
        // A file that arrives damaged is fetched once more before the
        // download is reported as wrong.
        for _round in 0..2 {
            let downloaded = match size {
                Some(size) if size > self.part_size => {
                    match self.in_parts(store, url, &part, size, &watch).await {
                        Err(Failure::NoRanges) => {
                            self.discard(store, expect);
                            self.in_one(store, url, &part, Some(size), &watch).await
                        }
                        other => other,
                    }
                }
                _ => self.in_one(store, url, &part, size, &watch).await,
            };
            if let Err(failure) = downloaded {
                watch.undo();
                let e = match failure {
                    Failure::Fatal(e) | Failure::Network(e) => e,
                    Failure::Busy(_) => anyhow!("the server stayed busy"),
                    Failure::NoRanges => anyhow!("the server does not serve ranges"),
                };
                return Err(e.context(format!("downloading {url}")));
            }

            watch.verifying();
            let path = part.clone();
            let hashes = tokio::task::spawn_blocking(move || hash_file(&path, want_git)).await??;
            let ok = match expect {
                Expect::Sha256(h) => &hashes.sha256 == h,
                Expect::GitOid(o) => hashes.git_oid.as_deref() == Some(o.as_str()),
            };
            if ok {
                store.add_tmp(&part, &hashes.sha256)?;
                let _ = std::fs::remove_file(parts_path(&part));
                return Ok(Fetched {
                    sha256: hashes.sha256,
                    size: hashes.size,
                    git_oid: hashes.git_oid,
                });
            }
            self.discard(store, expect);
            watch.undo();
        }
        Err(WrongContent {
            url: url.to_string(),
        }
        .into())
    }

    /// Refuse a download the store's volume cannot hold.
    fn room(&self, store: &Store, needed: u64) -> Result<(), Failure> {
        let free = store.free_bytes().map_err(Failure::Fatal)?;
        if needed > free {
            return Err(Failure::Fatal(NoSpace { needed, free }.into()));
        }
        Ok(())
    }

    /// A large file, part by part.
    async fn in_parts(
        &self,
        store: &Store,
        url: &str,
        part: &Path,
        size: u64,
        watch: &dyn Watch,
    ) -> Result<(), Failure> {
        let map_path = parts_path(part);
        let count = size.div_ceil(self.part_size);
        let length = |index: u64| (size - index * self.part_size).min(self.part_size);

        let mut done = PartMap::load(&map_path, size, self.part_size);
        let on_disk = tokio::fs::metadata(part).await.map(|m| m.len()).ok();
        if on_disk != Some(size) {
            done.clear();
        }
        let have: u64 = done.iter().map(|i| length(*i)).sum();
        self.room(store, size - have)?;
        if on_disk != Some(size) {
            // The whole length is claimed at once; APFS does not allocate
            // blocks until they are written.
            let file = tokio::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(part)
                .await
                .map_err(|e| Failure::Fatal(io_error(e, part)))?;
            file.set_len(size)
                .await
                .map_err(|e| Failure::Fatal(io_error(e, part)))?;
        }
        watch.bytes(have as i64);

        let todo: Vec<u64> = (0..count).filter(|i| !done.contains(i)).collect();
        let workers = todo.len().min(self.governor.max_connections());
        let shared = Mutex::new((todo.into_iter(), done));
        let worker = || async {
            loop {
                let next = shared.lock().expect("lock").0.next();
                let Some(index) = next else {
                    return Ok::<(), Failure>(());
                };
                let start = index * self.part_size;
                self.range(url, part, start, start + length(index), false, watch)
                    .await?;
                let mut guard = shared.lock().expect("lock");
                guard.1.insert(index);
                PartMap::save(&map_path, size, self.part_size, &guard.1)
                    .map_err(|e| Failure::Fatal(io_error(e, &map_path)))?;
            }
        };
        let running: Vec<_> = (0..workers).map(|_| worker()).collect();
        futures_util::future::try_join_all(running).await?;
        Ok(())
    }

    /// A file as one stream, continuing from the length of its partial file.
    async fn in_one(
        &self,
        store: &Store,
        url: &str,
        part: &Path,
        size: Option<u64>,
        watch: &dyn Watch,
    ) -> Result<(), Failure> {
        let on_disk = tokio::fs::metadata(part)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        let have = match size {
            Some(size) if on_disk > size => 0,
            _ => on_disk,
        };
        if have == 0 {
            let _ = tokio::fs::remove_file(part).await;
        }
        self.room(store, size.unwrap_or(0).saturating_sub(have))?;
        watch.bytes(have as i64);
        let end = size.unwrap_or(u64::MAX);
        match self.range(url, part, have, end, true, watch).await {
            // The server will not continue from there (it ignores `Range`,
            // or the partial file is stale): start over from the first byte.
            Err(Failure::NoRanges) if have > 0 => {
                watch.bytes(-(have as i64));
                let _ = tokio::fs::remove_file(part).await;
                self.range(url, part, 0, end, true, watch).await
            }
            other => other,
        }
    }

    /// Write the bytes from `start` up to `end` at their place in `part`,
    /// trying again until they are all there. With `to_eof` the range runs
    /// to the end of the file, whose length need not be known (`end` is then
    /// `u64::MAX`).
    async fn range(
        &self,
        url: &str,
        part: &Path,
        start: u64,
        end: u64,
        to_eof: bool,
        watch: &dyn Watch,
    ) -> Result<(), Failure> {
        let mut at = start;
        let mut failures = 0u32;
        loop {
            let permit = self.governor.permit(Class::Files).await;
            watch.running();
            let before = at;
            let failure = match self.attempt(url, part, &mut at, end, to_eof, watch).await {
                Ok(()) => {
                    self.governor.succeeded();
                    return Ok(());
                }
                Err(failure) => failure,
            };
            drop(permit);
            if at > before {
                // It got somewhere, so the server is not refusing us.
                failures = 0;
            }
            failures += 1;
            let patient = self.patient.load(Ordering::Relaxed);
            match failure {
                Failure::Fatal(_) | Failure::NoRanges => return Err(failure),
                _ if !patient && failures >= ATTEMPTS => return Err(failure),
                Failure::Busy(retry_after) => {
                    let pause = retry_after.unwrap_or_else(|| self.pause(failures));
                    let held = self.governor.congested(Some(pause));
                    watch.waiting(Wait::Server {
                        seconds: held.as_secs().max(1),
                    });
                }
                Failure::Network(_) => {
                    self.governor.congested(None);
                    watch.waiting(Wait::Network);
                    tokio::time::sleep(self.pause(failures)).await;
                }
            }
        }
    }

    /// One request for the bytes from `*at` on, written at their place.
    async fn attempt(
        &self,
        url: &str,
        part: &Path,
        at: &mut u64,
        end: u64,
        to_eof: bool,
        watch: &dyn Watch,
    ) -> Result<(), Failure> {
        let known_end = end != u64::MAX;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(part)
            .await
            .map_err(|e| Failure::Fatal(io_error(e, part)))?;
        if known_end && *at >= end {
            return Ok(());
        }
        let range = match (*at, to_eof) {
            (0, true) => None,
            (from, true) => Some(format!("bytes={from}-")),
            (from, false) => Some(format!("bytes={from}-{}", end - 1)),
        };
        let resp = self.open(url, range.as_deref()).await?;
        // 200 is the whole file: right only when that is what was asked for.
        if resp.status().as_u16() == 200 && range.is_some() {
            return Err(Failure::NoRanges);
        }
        let mut resp = resp;

        file.seek(SeekFrom::Start(*at))
            .await
            .map_err(|e| Failure::Fatal(io_error(e, part)))?;
        loop {
            let chunk = match resp.chunk().await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(e) => {
                    let _ = file.flush().await;
                    return Err(Failure::Network(e.into()));
                }
            };
            if chunk.len() as u64 > end - *at {
                return Err(Failure::Fatal(anyhow!(
                    "the server sent more than the {} bytes that were left",
                    end - *at
                )));
            }
            file.write_all(&chunk)
                .await
                .map_err(|e| Failure::Fatal(io_error(e, part)))?;
            *at += chunk.len() as u64;
            watch.bytes(chunk.len() as i64);
            watch.received(chunk.len() as u64);
            self.governor.throttle(chunk.len()).await;
        }
        file.flush()
            .await
            .map_err(|e| Failure::Fatal(io_error(e, part)))?;
        if known_end && *at != end {
            return Err(Failure::Network(anyhow!(
                "the download ended {} bytes early",
                end - *at
            )));
        }
        Ok(())
    }

    /// Send the request, following redirects by hand: the provider's own
    /// answer carries its rate limits, and the token must not travel on to
    /// the download server it points at.
    async fn open(&self, url: &str, range: Option<&str>) -> Result<reqwest::Response, Failure> {
        let mut url = reqwest::Url::parse(url).map_err(|e| Failure::Fatal(e.into()))?;
        for _ in 0..8 {
            let ours = url.as_str().starts_with(&self.endpoint);
            let mut req = self.http_no_redirect.get(url.clone());
            if let Some(t) = &self.token
                && ours
            {
                req = req.bearer_auth(t);
            }
            if let Some(range) = range {
                req = req.header(reqwest::header::RANGE, range);
            }
            let resp = req.send().await.map_err(|e| Failure::Network(e.into()))?;
            if ours {
                self.governor.observe(resp.headers());
            }
            let status = resp.status().as_u16();
            if resp.status().is_redirection() {
                let target = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|l| url.join(l).ok());
                match target {
                    Some(next) => url = next,
                    None => {
                        return Err(Failure::Fatal(anyhow!(
                            "HTTP {status} without a usable Location"
                        )));
                    }
                }
                continue;
            }
            return match status {
                200 | 206 => Ok(resp),
                401 | 403 | 404 | 410 => Err(Failure::Fatal(
                    Unavailable {
                        status,
                        url: url.to_string(),
                    }
                    .into(),
                )),
                // The position asked for is not in the file: the partial
                // file is stale.
                416 => Err(Failure::NoRanges),
                429 | 503 => Err(Failure::Busy(governor::retry_after(resp.headers()))),
                408 | 500..=599 => Err(Failure::Network(anyhow!("HTTP {status}"))),
                _ => Err(Failure::Fatal(anyhow!("HTTP {status}"))),
            };
        }
        Err(Failure::Fatal(anyhow!("too many redirects")))
    }

    /// How long to wait after `failures` in a row: doubling up to a minute,
    /// with a random share taken off so many downloads do not retry at once.
    fn pause(&self, failures: u32) -> Duration {
        let doubled = self.backoff * 2u32.pow(failures.saturating_sub(1).min(6));
        let ceiling = doubled.min(MAX_BACKOFF.max(self.backoff));
        let random = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish();
        let share = 0.5 + (random % 1000) as f64 / 2000.0;
        ceiling.mul_f64(share)
    }
}

/// Which parts of a large download are complete, kept beside its partial file.
struct PartMap;

impl PartMap {
    fn load(path: &Path, size: u64, part_size: u64) -> BTreeSet<u64> {
        let read = || -> Option<BTreeSet<u64>> {
            let value: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
            if value["size"].as_u64()? != size || value["part"].as_u64()? != part_size {
                return None;
            }
            value["done"]
                .as_array()?
                .iter()
                .map(|v| v.as_u64())
                .collect()
        };
        read().unwrap_or_default()
    }

    fn save(path: &Path, size: u64, part_size: u64, done: &BTreeSet<u64>) -> std::io::Result<()> {
        let text = serde_json::json!({"size": size, "part": part_size, "done": done}).to_string();
        let tmp = path.with_extension("parts.new");
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, path)
    }
}

fn parts_path(part: &Path) -> PathBuf {
    part.with_extension("parts")
}

fn io_error(e: std::io::Error, path: &Path) -> anyhow::Error {
    if e.kind() == std::io::ErrorKind::StorageFull {
        return NoSpace { needed: 0, free: 0 }.into();
    }
    anyhow::Error::new(e).context(format!("writing {}", path.display()))
}

/// Percent-encode a repository path for a URL, keeping `/`.
pub fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
