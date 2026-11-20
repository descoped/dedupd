//! Ollama: `blobs/sha256-<hex>` plus one manifest per model tag, under
//! `manifests/<registry host>/<namespace>/<model>/<tag>`.
//!
//! Models pulled from Hugging Face (`hf.co/...`) are Hugging Face
//! repositories, and their layers are what other apps can share. Models from
//! Ollama's own registry are archived too, under their full name.

use super::{LocalFile, LocalRepo, rel_components};
use crate::db::{PROVIDER_HF, PROVIDER_OLLAMA};
use crate::ollama::{HOST, Manifest};
use crate::store::Fingerprint;
use anyhow::Result;
use dedupd_api::app::App;
use std::path::Path;
use walkdir::WalkDir;

pub const APP: &str = App::Ollama.as_str();

/// A manifest found in Ollama's folder.
#[derive(Debug, Clone)]
pub struct FoundManifest {
    pub provider: &'static str,
    /// `org/repo` for Hugging Face, `host/namespace/model` otherwise.
    pub repo: String,
    pub tag: String,
    pub body: String,
}

/// Every model manifest in Ollama's models folder, as its `manifests` tree
/// lists them: the models dedupd adopts and places.
pub fn manifests(models_dir: &Path) -> Vec<FoundManifest> {
    manifests_under(models_dir, "manifests")
}

/// Every manifest Ollama may read a model from: the `manifests` tree and the
/// `manifests-v2` tree, whose entries are links to manifest files among the
/// blobs. Ollama's own import writes only the second on a fresh folder, so
/// what a model still needs is asked of both.
pub fn manifests_in_use(models_dir: &Path) -> Vec<FoundManifest> {
    let mut out = manifests_under(models_dir, "manifests");
    out.extend(manifests_under(models_dir, "manifests-v2"));
    out
}

fn manifests_under(models_dir: &Path, tree: &str) -> Vec<FoundManifest> {
    let base = models_dir.join(tree);
    let mut out = Vec::new();
    for entry in WalkDir::new(&base)
        .min_depth(4)
        .max_depth(4)
        .sort_by_file_name()
        .into_iter()
        .flatten()
    {
        let name = entry.file_name().to_string_lossy();
        if name.starts_with('.') || !entry.path().is_file() {
            continue;
        }
        let rel = rel_components(entry.path(), &base);
        if let Ok(body) = std::fs::read_to_string(entry.path())
            && Manifest::parse(&body).is_ok()
        {
            let (provider, repo) = if rel[0] == HOST {
                (PROVIDER_HF, format!("{}/{}", rel[1], rel[2]))
            } else {
                (PROVIDER_OLLAMA, format!("{}/{}/{}", rel[0], rel[1], rel[2]))
            };
            out.push(FoundManifest {
                provider,
                repo,
                tag: rel[3].clone(),
                body,
            });
        }
    }
    out
}

/// The layer files of every model in Ollama's folder.
pub fn scan(models_dir: &Path) -> Result<Vec<LocalRepo>> {
    let mut repos: Vec<LocalRepo> = Vec::new();
    for found in manifests(models_dir) {
        let manifest = Manifest::parse(&found.body)?;
        let mut files = Vec::new();
        for layer in manifest.all() {
            let rel_path = format!("blobs/sha256-{}", layer.sha256()?);
            let abs_path = models_dir.join(&rel_path);
            if abs_path.is_file()
                && let Some(fingerprint) = Fingerprint::if_there(&abs_path)?
            {
                files.push(LocalFile {
                    fingerprint,
                    rel_path,
                    abs_path,
                });
            }
        }
        match repos
            .iter_mut()
            .find(|r| r.name == found.repo && r.provider == found.provider)
        {
            Some(repo) => {
                for f in files {
                    if !repo.files.iter().any(|x| x.abs_path == f.abs_path) {
                        repo.files.push(f);
                    }
                }
            }
            None if !files.is_empty() => repos.push(LocalRepo {
                app: APP,
                provider: found.provider,
                name: found.repo,
                revision: None,
                files,
            }),
            None => {}
        }
    }
    Ok(repos)
}
