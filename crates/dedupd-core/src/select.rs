//! Choosing which files of a repository to fetch and place.

use crate::hub::TreeEntry;
use anyhow::{Result, bail};
use std::collections::BTreeMap;

/// The quantization Hugging Face serves to Ollama when no tag is given.
const DEFAULT_QUANT: &str = "Q4_K_M";

#[derive(Debug, Clone, Default)]
pub struct Selection {
    /// A quantization name such as `Q4_K_M`, matched against GGUF file names.
    pub quant: Option<String>,
    /// Glob patterns (`*`, `?`). When given, exactly the matching files.
    pub include: Vec<String>,
}

/// Match `text` against a pattern where `*` is any run of characters and `?`
/// is any one character. Case-insensitive.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub fn is_gguf(path: &str) -> bool {
    path.to_lowercase().ends_with(".gguf")
}

fn is_projector(path: &str) -> bool {
    file_name(path).to_lowercase().starts_with("mmproj")
}

/// The name of a GGUF without the `-00001-of-00004` part of a split file.
fn gguf_group(path: &str) -> String {
    let stem = path.strip_suffix(".gguf").unwrap_or(path);
    let parts: Vec<&str> = stem.rsplitn(4, '-').collect();
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if parts.len() == 4 && digits(parts[0]) && parts[1] == "of" && digits(parts[2]) {
        parts[3].to_string()
    } else {
        stem.to_string()
    }
}

/// The weight files (GGUF, not projectors) among `entries`.
pub fn weights(entries: &[TreeEntry]) -> Vec<&TreeEntry> {
    entries
        .iter()
        .filter(|e| is_gguf(&e.path) && !is_projector(&e.path))
        .collect()
}

/// Pick the files to fetch.
///
/// With `include` patterns, exactly the files that match. Otherwise every
/// file of a repository that has no GGUF weights (an MLX or safetensors
/// repository), or one quantization of a GGUF repository plus its small files.
pub fn select(entries: &[TreeEntry], sel: &Selection) -> Result<Vec<TreeEntry>> {
    if !sel.include.is_empty() {
        let picked: Vec<TreeEntry> = entries
            .iter()
            .filter(|e| {
                sel.include
                    .iter()
                    .any(|p| glob_match(p, &e.path) || glob_match(p, file_name(&e.path)))
            })
            .cloned()
            .collect();
        if picked.is_empty() {
            bail!("no file matches {}", sel.include.join(", "));
        }
        return Ok(picked);
    }

    let ggufs = weights(entries);
    if ggufs.is_empty() {
        if let Some(q) = &sel.quant {
            bail!("the repository has no GGUF files, so there is no quantization {q} to pick");
        }
        return Ok(entries.to_vec());
    }

    let mut groups: BTreeMap<String, Vec<&TreeEntry>> = BTreeMap::new();
    for e in ggufs {
        groups.entry(gguf_group(&e.path)).or_default().push(e);
    }
    let wanted = |q: &str| -> Vec<&String> {
        let q = q.to_lowercase();
        groups
            .keys()
            .filter(|g| file_name(g).to_lowercase().contains(&q))
            .collect()
    };
    let choices = || {
        groups
            .keys()
            .map(|g| file_name(g).to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let group = match &sel.quant {
        Some(q) => match wanted(q).as_slice() {
            [one] => (*one).clone(),
            [] => bail!(
                "no GGUF file matches {q}. The repository has: {}",
                choices()
            ),
            _ => bail!(
                "{q} matches more than one GGUF. Be more specific; the repository has: {}",
                choices()
            ),
        },
        None if groups.len() == 1 => groups.keys().next().expect("one group").clone(),
        None => match wanted(DEFAULT_QUANT).as_slice() {
            [one] => (*one).clone(),
            _ => bail!(
                "the repository has several GGUF files; name one, for example org/repo:Q8_0. It has: {}",
                choices()
            ),
        },
    };

    let mut picked: Vec<TreeEntry> = entries
        .iter()
        .filter(|e| !is_gguf(&e.path))
        .cloned()
        .collect();
    picked.extend(groups[&group].iter().map(|e| (*e).clone()));
    picked.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(picked)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str) -> TreeEntry {
        TreeEntry {
            path: path.into(),
            size: 1,
            git_oid: "g".into(),
            lfs_sha256: is_gguf(path).then(|| "s".into()),
        }
    }

    fn paths(v: &[TreeEntry]) -> Vec<&str> {
        v.iter().map(|e| e.path.as_str()).collect()
    }

    #[test]
    fn glob() {
        assert!(glob_match("*.gguf", "Model-Q4_K_M.GGUF"));
        assert!(glob_match(
            "model-0000?-of-*.safetensors",
            "model-00001-of-00004.safetensors"
        ));
        assert!(!glob_match("*.gguf", "config.json"));
        assert!(glob_match("*", ""));
    }

    #[test]
    fn repository_without_gguf_is_taken_whole() {
        let e = [entry("config.json"), entry("model.safetensors")];
        assert_eq!(select(&e, &Selection::default()).unwrap().len(), 2);
    }

    #[test]
    fn picks_one_quantization_and_keeps_small_files() {
        let e = [
            entry("README.md"),
            entry("m-Q4_K_M.gguf"),
            entry("m-Q8_0.gguf"),
            entry("mmproj-f16.gguf"),
        ];
        let default = select(&e, &Selection::default()).unwrap();
        assert_eq!(paths(&default), ["README.md", "m-Q4_K_M.gguf"]);
        let q8 = select(
            &e,
            &Selection {
                quant: Some("q8_0".into()),
                include: vec![],
            },
        )
        .unwrap();
        assert_eq!(paths(&q8), ["README.md", "m-Q8_0.gguf"]);
        assert!(
            select(
                &e,
                &Selection {
                    quant: Some("Q2".into()),
                    include: vec![]
                }
            )
            .is_err()
        );
    }

    #[test]
    fn split_gguf_is_one_choice() {
        let e = [
            entry("m-MXFP4-00001-of-00002.gguf"),
            entry("m-MXFP4-00002-of-00002.gguf"),
        ];
        assert_eq!(select(&e, &Selection::default()).unwrap().len(), 2);
    }

    #[test]
    fn several_quantizations_without_a_default_is_an_error() {
        let e = [entry("m-Q5_K_M.gguf"), entry("m-Q8_0.gguf")];
        let err = select(&e, &Selection::default()).unwrap_err().to_string();
        assert!(err.contains("m-Q5_K_M") && err.contains("m-Q8_0"));
    }

    #[test]
    fn include_patterns_win() {
        let e = [entry("a/b.gguf"), entry("c.json")];
        let sel = Selection {
            quant: None,
            include: vec!["*.json".into()],
        };
        assert_eq!(paths(&select(&e, &sel).unwrap()), ["c.json"]);
    }
}
