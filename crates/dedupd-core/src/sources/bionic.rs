//! Bionic and LM Studio keep models as plain files under
//! `<models dir>/<publisher>/<repo>/<file>`.

use super::{LocalFile, LocalRepo};
use crate::store::Fingerprint;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub const APP: &str = "bionic";

/// `~/.lmstudio/models`, or `None` when `HOME` is not set.
pub fn default_models_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".lmstudio/models"))
}

fn visible_dirs(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        // `file_type` does not follow symlinks, so linked folders are skipped.
        if !name.starts_with('.') && entry.file_type()?.is_dir() {
            out.push((name, entry.path()));
        }
    }
    out.sort();
    Ok(out)
}

/// List every model file under `models_dir`. Hidden files, symlinks and
/// anything that is not a regular file are left out.
pub fn scan(models_dir: &Path) -> Result<Vec<LocalRepo>> {
    let mut repos = Vec::new();
    for (publisher, publisher_dir) in visible_dirs(models_dir)? {
        for (repo, repo_dir) in visible_dirs(&publisher_dir)? {
            let mut files = Vec::new();
            let walker = WalkDir::new(&repo_dir)
                .follow_links(false)
                .sort_by_file_name()
                .into_iter()
                .filter_entry(|e| {
                    e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.')
                });
            for entry in walker {
                let entry = entry?;
                if !entry.file_type().is_file() {
                    continue;
                }
                let rel = entry
                    .path()
                    .strip_prefix(&repo_dir)
                    .expect("walked path is under the repo dir");
                let rel_path = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                files.push(LocalFile {
                    rel_path,
                    abs_path: entry.path().to_path_buf(),
                    fingerprint: Fingerprint::of(entry.path())?,
                });
            }
            if !files.is_empty() {
                repos.push(LocalRepo {
                    app: APP,
                    name: format!("{publisher}/{repo}"),
                    revision: None,
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
    fn finds_regular_files_grouped_by_repo() {
        let dir = tempfile::tempdir().unwrap();
        let m = dir.path();
        fs::create_dir_all(m.join("org-a/Repo-1/sub")).unwrap();
        fs::create_dir_all(m.join("org-a/empty")).unwrap();
        fs::create_dir_all(m.join(".hidden/repo")).unwrap();
        fs::write(m.join("org-a/Repo-1/config.json"), b"{}").unwrap();
        fs::write(m.join("org-a/Repo-1/sub/weights.safetensors"), b"abc").unwrap();
        fs::write(m.join("org-a/Repo-1/.DS_Store"), b"x").unwrap();
        fs::write(m.join(".hidden/repo/f"), b"x").unwrap();
        fs::write(m.join(".DS_Store"), b"x").unwrap();
        std::os::unix::fs::symlink(
            m.join("org-a/Repo-1/config.json"),
            m.join("org-a/Repo-1/link.json"),
        )
        .unwrap();

        let repos = scan(m).unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].name, "org-a/Repo-1");
        let rels: Vec<_> = repos[0].files.iter().map(|f| f.rel_path.as_str()).collect();
        assert_eq!(rels, ["config.json", "sub/weights.safetensors"]);
        assert_eq!(repos[0].files[1].fingerprint.size, 3);
    }
}
