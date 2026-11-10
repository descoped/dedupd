//! The Hugging Face cache: `models--org--repo/snapshots/<commit>/<path>` are
//! links to `blobs/<etag>`. The cache records the commit each file belongs to.

use super::{LocalFile, LocalRepo};
use crate::store::Fingerprint;
use anyhow::Result;
use std::collections::HashSet;
use std::path::Path;
use walkdir::WalkDir;

pub const APP: &str = "hf";

fn dirs(dir: &Path) -> Vec<(String, std::path::PathBuf)> {
    let mut out: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    out.sort();
    out
}

/// List the model files in the cache, one group per repository and commit.
/// A missing cache folder is an empty cache.
pub fn scan(hub_dir: &Path) -> Result<Vec<LocalRepo>> {
    let mut repos = Vec::new();
    for (dir_name, repo_dir) in dirs(hub_dir) {
        let Some(name) = dir_name.strip_prefix("models--") else {
            continue;
        };
        let name = name.replacen("--", "/", 1);
        let mut seen = HashSet::new();
        for (commit, snapshot) in dirs(&repo_dir.join("snapshots")) {
            let mut files = Vec::new();
            for entry in WalkDir::new(&snapshot).sort_by_file_name() {
                let entry = entry?;
                if entry.file_type().is_dir() {
                    continue;
                }
                // Follow the link to the blob; a dangling link or a download
                // still in progress has nothing to adopt.
                let Ok(blob) = std::fs::canonicalize(entry.path()) else {
                    continue;
                };
                if !blob.is_file() || !seen.insert(blob.clone()) {
                    continue;
                }
                let rel = entry
                    .path()
                    .strip_prefix(&snapshot)
                    .expect("walked path is under the snapshot");
                files.push(LocalFile {
                    rel_path: rel
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("/"),
                    fingerprint: Fingerprint::of(&blob)?,
                    abs_path: blob,
                });
            }
            if !files.is_empty() {
                repos.push(LocalRepo {
                    app: APP,
                    name: name.clone(),
                    revision: Some(commit),
                    files,
                });
            }
        }
    }
    Ok(repos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn reads_snapshots_through_their_links() {
        let dir = tempfile::tempdir().unwrap();
        let hub = fs::canonicalize(dir.path()).unwrap();
        let repo = hub.join("models--org--Repo-GGUF");
        let commit = "0123456789abcdef0123456789abcdef01234567";
        fs::create_dir_all(repo.join("blobs")).unwrap();
        fs::create_dir_all(repo.join("snapshots").join(commit).join("sub")).unwrap();
        fs::write(repo.join("blobs/aaa"), b"weights").unwrap();
        fs::write(repo.join("blobs/bbb.incomplete"), b"part").unwrap();
        std::os::unix::fs::symlink(
            "../../../blobs/aaa",
            repo.join("snapshots").join(commit).join("sub/m.gguf"),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            "../../blobs/gone",
            repo.join("snapshots").join(commit).join("dangling"),
        )
        .unwrap();
        fs::create_dir_all(hub.join("datasets--x--y/snapshots/c")).unwrap();

        let repos = scan(&hub).unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].name, "org/Repo-GGUF");
        assert_eq!(repos[0].revision.as_deref(), Some(commit));
        assert_eq!(repos[0].files.len(), 1);
        assert_eq!(repos[0].files[0].rel_path, "sub/m.gguf");
        assert_eq!(repos[0].files[0].abs_path, repo.join("blobs/aaa"));
        assert!(scan(&hub.join("missing")).unwrap().is_empty());
    }
}
