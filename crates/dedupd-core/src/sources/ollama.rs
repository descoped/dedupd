//! Ollama: `blobs/sha256-<hex>` plus one manifest per model tag. Only models
//! pulled from Hugging Face (`hf.co/...`) are read; those are the ones whose
//! layers other apps can share.

use super::{LocalFile, LocalRepo};
use crate::ollama::{HOST, Manifest};
use crate::store::Fingerprint;
use anyhow::Result;
use std::path::Path;
use walkdir::WalkDir;

pub const APP: &str = "ollama";

/// A manifest found in Ollama's folder.
#[derive(Debug, Clone)]
pub struct FoundManifest {
    pub repo: String,
    pub tag: String,
    pub body: String,
}

/// The Hugging Face manifests in Ollama's models folder.
pub fn manifests(models_dir: &Path) -> Vec<FoundManifest> {
    let base = models_dir.join("manifests").join(HOST);
    let mut out = Vec::new();
    for entry in WalkDir::new(&base)
        .min_depth(3)
        .max_depth(3)
        .sort_by_file_name()
        .into_iter()
        .flatten()
    {
        let name = entry.file_name().to_string_lossy();
        if name.starts_with('.') || !entry.path().is_file() {
            continue;
        }
        let rel: Vec<String> = entry
            .path()
            .strip_prefix(&base)
            .expect("walked path is under the base")
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        if let Ok(body) = std::fs::read_to_string(entry.path())
            && Manifest::parse(&body).is_ok()
        {
            out.push(FoundManifest {
                repo: format!("{}/{}", rel[0], rel[1]),
                tag: rel[2].clone(),
                body,
            });
        }
    }
    out
}

/// The layer files of every Hugging Face model in Ollama's folder.
pub fn scan(models_dir: &Path) -> Result<Vec<LocalRepo>> {
    let mut repos: Vec<LocalRepo> = Vec::new();
    for found in manifests(models_dir) {
        let manifest = Manifest::parse(&found.body)?;
        let mut files = Vec::new();
        for layer in manifest.all() {
            let rel_path = format!("blobs/sha256-{}", layer.sha256()?);
            let abs_path = models_dir.join(&rel_path);
            if abs_path.is_file() {
                files.push(LocalFile {
                    fingerprint: Fingerprint::of(&abs_path)?,
                    rel_path,
                    abs_path,
                });
            }
        }
        match repos.iter_mut().find(|r| r.name == found.repo) {
            Some(repo) => {
                for f in files {
                    if !repo.files.iter().any(|x| x.abs_path == f.abs_path) {
                        repo.files.push(f);
                    }
                }
            }
            None if !files.is_empty() => repos.push(LocalRepo {
                app: APP,
                name: found.repo,
                revision: None,
                files,
            }),
            None => {}
        }
    }
    Ok(repos)
}
