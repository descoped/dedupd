//! Downloads into the store, with resume and verification.

use crate::hash::hash_file;
use crate::store::Store;
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::AsyncWriteExt;

/// What a downloaded file must hash to before it may enter the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    /// The sha256 of the content: the Hub's `lfs.oid`, an Ollama layer digest.
    Sha256(String),
    /// The git blob id, which is how the Hub names files that are not in LFS.
    GitOid(String),
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

/// Running totals a caller can read while downloads are in flight.
#[derive(Debug, Default)]
pub struct Progress {
    pub total: AtomicU64,
    pub done: AtomicU64,
}

pub struct Fetcher {
    http: reqwest::Client,
    endpoint: String,
    token: Option<String>,
    /// One download at a time per file, so two requests for the same file
    /// never write the same partial file.
    in_flight: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

const ATTEMPTS: usize = 6;

impl Fetcher {
    pub fn new(endpoint: impl Into<String>, token: Option<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("dedupd/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(std::time::Duration::from_secs(15))
            .read_timeout(std::time::Duration::from_secs(60))
            .build()?;
        Ok(Self {
            http,
            endpoint: endpoint.into().trim_end_matches('/').to_string(),
            token,
            in_flight: Mutex::new(HashMap::new()),
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
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

    /// Download `url` into the store. An interrupted download continues from
    /// where it stopped, in this run or a later one.
    pub async fn fetch(
        &self,
        store: &Store,
        url: &str,
        size: Option<u64>,
        expect: &Expect,
        progress: Option<&Progress>,
    ) -> Result<Fetched> {
        let key = match expect {
            Expect::Sha256(h) => h.clone(),
            Expect::GitOid(o) => format!("git-{o}"),
        };
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
            return Ok(Fetched {
                sha256: h.clone(),
                size,
                git_oid: None,
            });
        }

        let part = store.tmp_path(&format!("{key}.part"));
        let mut last_error = None;
        for attempt in 0..ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(1 << attempt.min(4))).await;
            }
            match self.download(url, &part, size, progress).await {
                Ok(()) => {
                    last_error = None;
                    break;
                }
                Err(e) if e.downcast_ref::<Unavailable>().is_some() => return Err(e),
                Err(e) => last_error = Some(e),
            }
        }
        if let Some(e) = last_error {
            return Err(e.context(format!("downloading {url}")));
        }

        let want_git = matches!(expect, Expect::GitOid(_));
        let path = part.clone();
        let hashes = tokio::task::spawn_blocking(move || hash_file(&path, want_git)).await??;
        let ok = match expect {
            Expect::Sha256(h) => &hashes.sha256 == h,
            Expect::GitOid(o) => hashes.git_oid.as_deref() == Some(o.as_str()),
        };
        if !ok {
            let _ = std::fs::remove_file(&part);
            bail!("{url} did not have the expected content; the download was discarded");
        }
        store.add_tmp(&part, &hashes.sha256)?;
        Ok(Fetched {
            sha256: hashes.sha256,
            size: hashes.size,
            git_oid: hashes.git_oid,
        })
    }

    /// One attempt: continue `part` from its current length to the end.
    async fn download(
        &self,
        url: &str,
        part: &std::path::Path,
        size: Option<u64>,
        progress: Option<&Progress>,
    ) -> Result<()> {
        let mut have = tokio::fs::metadata(part)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        if let Some(size) = size {
            if have == size && size > 0 {
                return Ok(());
            }
            if have > size {
                tokio::fs::remove_file(part).await?;
                have = 0;
            }
        }

        let mut req = self.get(url);
        if have > 0 {
            req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
        }
        let mut resp = req.send().await?;
        let status = resp.status().as_u16();
        match status {
            200 => have = 0,
            206 => {}
            416 => {
                // The partial file is longer than the real one, or stale.
                tokio::fs::remove_file(part).await.ok();
                bail!("the server rejected the resume position");
            }
            401 | 403 | 404 | 410 => {
                return Err(Unavailable {
                    status,
                    url: url.to_string(),
                }
                .into());
            }
            _ => bail!("HTTP {status}"),
        }

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(have == 0)
            .append(have > 0)
            .open(part)
            .await
            .with_context(|| format!("opening {}", part.display()))?;
        if let Some(p) = progress
            && have > 0
        {
            p.done.fetch_add(have, Ordering::Relaxed);
        }
        let mut written = 0u64;
        let result: Result<()> = async {
            while let Some(chunk) = resp.chunk().await? {
                file.write_all(&chunk).await?;
                written += chunk.len() as u64;
                if let Some(p) = progress {
                    p.done.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                }
            }
            file.flush().await?;
            Ok(())
        }
        .await;
        let total = have + written;
        let short = size.is_some_and(|s| total != s);
        if result.is_err() || short {
            // The next attempt counts the bytes on disk again.
            if let Some(p) = progress {
                p.done.fetch_sub(total, Ordering::Relaxed);
            }
            result?;
            bail!("the download ended after {total} bytes");
        }
        Ok(())
    }
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
