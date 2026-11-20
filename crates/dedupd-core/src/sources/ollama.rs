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
use anyhow::{Context, Result};
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
/// lists them: the models dedupd adopts and places. A file there that is
/// not a manifest (one Ollama is still writing, or something else) is left
/// out: there is nothing in it to adopt. A folder that cannot be read is
/// an error, with its name; a folder that is not there is an empty one.
pub fn manifests(models_dir: &Path) -> Result<Vec<FoundManifest>> {
    manifests_under(models_dir, "manifests", false)
}

/// Every manifest Ollama may read a model from: the `manifests` tree and the
/// `manifests-v2` tree, whose entries are links to manifest files among the
/// blobs. Ollama's own import writes only the second on a fresh folder, so
/// what a model still needs is asked of both.
///
/// The answer decides which layers may be removed, so here a manifest that
/// cannot be read or understood is an error and not a model with no
/// layers.
pub fn manifests_in_use(models_dir: &Path) -> Result<Vec<FoundManifest>> {
    let mut out = manifests_under(models_dir, "manifests", true)?;
    out.extend(manifests_under(models_dir, "manifests-v2", true)?);
    Ok(out)
}

fn manifests_under(models_dir: &Path, tree: &str, every_one: bool) -> Result<Vec<FoundManifest>> {
    let base = models_dir.join(tree);
    let mut out = Vec::new();
    for entry in WalkDir::new(&base)
        .min_depth(4)
        .max_depth(4)
        .sort_by_file_name()
    {
        let entry = match entry {
            Ok(entry) => entry,
            // Not there (no such tree, or removed while it was read).
            Err(e) if super::gone(&e) => continue,
            Err(e) => {
                return Err(e).with_context(|| format!("reading {}", base.display()));
            }
        };
        let name = entry.file_name().to_string_lossy();
        if name.starts_with('.') || !entry.path().is_file() {
            continue;
        }
        let rel = rel_components(entry.path(), &base);
        let body = match std::fs::read_to_string(entry.path()) {
            Ok(body) if Manifest::parse(&body).is_ok() => body,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            _ if !every_one => continue,
            Ok(_) => anyhow::bail!(
                "{} is in Ollama's manifests and is not a manifest",
                entry.path().display()
            ),
            Err(e) => {
                return Err(e).with_context(|| format!("reading {}", entry.path().display()));
            }
        };
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
    Ok(out)
}

/// The layer files of every model in Ollama's folder. A layer two models
/// share (the same template or licence text has the same digest) is listed
/// for each of them; the database keeps one record per path, so it is
/// recorded as being in Ollama for one of the two. What Ollama still needs
/// is never read from those records but from its manifests
/// (`manifests_in_use`).
pub fn scan(models_dir: &Path) -> Result<Vec<LocalRepo>> {
    let mut repos: Vec<LocalRepo> = Vec::new();
    for found in manifests(models_dir)? {
        let manifest = Manifest::parse(&found.body)?;
        let mut files = Vec::new();
        for layer in manifest.all() {
            let rel_path = format!("blobs/sha256-{}", layer.sha256()?);
            let abs_path = models_dir.join(&rel_path);
            // A manifest can name one file twice; it is listed once.
            if files.iter().any(|f: &LocalFile| f.abs_path == abs_path) {
                continue;
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    const WEIGHTS: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const TEMPLATE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const CONFIG: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    fn manifest(weights: &str) -> String {
        let layer = |kind: &str, sha: &str| {
            format!(r#"{{"mediaType":"{kind}","digest":"sha256:{sha}","size":1}}"#)
        };
        format!(
            r#"{{"schemaVersion":2,"config":{},"layers":[{},{}]}}"#,
            layer("application/vnd.docker.container.image.v1+json", CONFIG),
            layer("application/vnd.ollama.image.model", weights),
            layer("application/vnd.ollama.image.template", TEMPLATE),
        )
    }

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// An Ollama folder with a Hugging Face model, one of Ollama's own
    /// (they share the template and config layers), and a hidden file.
    fn folder() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let at = |rel: &str| dir.path().join(rel);
        write(&at("manifests/hf.co/org/repo/Q4_K_M"), &manifest(WEIGHTS));
        write(
            &at("manifests/registry.ollama.ai/library/tiny/latest"),
            &manifest(CONFIG),
        );
        write(&at("manifests/hf.co/org/repo/.DS_Store"), "x");
        for sha in [WEIGHTS, TEMPLATE, CONFIG] {
            write(&at(&format!("blobs/sha256-{sha}")), "x");
        }
        dir
    }

    #[test]
    fn models_are_named_by_where_their_manifest_is() {
        let dir = folder();
        let found = manifests(dir.path()).unwrap();
        let names: Vec<_> = found
            .iter()
            .map(|m| (m.provider, m.repo.as_str(), m.tag.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                (PROVIDER_HF, "org/repo", "Q4_K_M"),
                (PROVIDER_OLLAMA, "registry.ollama.ai/library/tiny", "latest"),
            ]
        );
        // No folder at all is an empty one.
        assert!(manifests(&dir.path().join("nowhere")).unwrap().is_empty());
        assert!(
            manifests_in_use(&dir.path().join("nowhere"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_layer_two_models_share_is_listed_for_each_and_a_missing_one_for_none() {
        let dir = folder();
        std::fs::remove_file(dir.path().join(format!("blobs/sha256-{WEIGHTS}"))).unwrap();
        let repos = scan(dir.path()).unwrap();
        let files = |name: &str| -> Vec<String> {
            let repo = repos.iter().find(|r| r.name == name).unwrap();
            repo.files.iter().map(|f| f.rel_path.clone()).collect()
        };
        let shared = [
            format!("blobs/sha256-{CONFIG}"),
            format!("blobs/sha256-{TEMPLATE}"),
        ];
        // The weights of org/repo are gone; what is left is still listed.
        assert_eq!(files("org/repo"), shared);
        // Ollama's own model names the config blob twice; it is listed once.
        assert_eq!(files("registry.ollama.ai/library/tiny"), shared);
    }

    #[test]
    fn what_is_not_a_manifest_is_left_out_of_adoption_and_stops_a_removal() {
        let dir = folder();
        write(
            &dir.path().join("manifests/hf.co/org/half/latest"),
            "{\"layers\":[",
        );
        // Nothing to adopt in it.
        assert_eq!(manifests(dir.path()).unwrap().len(), 2);
        // What may be removed cannot be told while it is there.
        let said = manifests_in_use(dir.path()).unwrap_err();
        assert!(said.to_string().contains("is not a manifest"), "{said}");
    }
}
