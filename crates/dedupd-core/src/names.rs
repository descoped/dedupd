//! What a model name may be: Hugging Face's own rule for a repository name,
//! and the way a model is asked for, `org/repo[:quant][@revision]`. Text a
//! person typed reaches the provider only through these.

use anyhow::{Result, bail};

/// One half of `org/name`, by Hugging Face's own rule: up to 96 letters,
/// digits, `-`, `_` and `.`; it starts and ends with a letter, a digit or
/// `_`; no `--`, no `..`, and it does not end in `.git`.
fn is_name_part(part: &str) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    !part.is_empty()
        && part.len() <= 96
        && part.starts_with(word)
        && part.ends_with(word)
        && part.chars().all(|c| word(c) || c == '-' || c == '.')
        && !part.contains("--")
        && !part.contains("..")
        && !part.ends_with(".git")
}

/// Whether the text is the name of a repository, `org/name`, and nothing
/// else. A name that came from outside (a request, a pasted address) is
/// checked with this before it is put into a path or sent to the provider:
/// such a name can hold nothing that climbs out of a folder or changes the
/// meaning of an address.
pub fn is_repo_name(repo: &str) -> bool {
    let mut parts = repo.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(org), Some(name), None) if is_name_part(org) && is_name_part(name)
    )
}

/// Split `org/repo[:quant][@revision]` as typed on the command line. Every
/// part is checked: a quantization is a short word, and a revision is a
/// branch, tag or commit with nothing in it that climbs out of a path.
pub fn parse_model(spec: &str) -> Result<(String, Option<String>, String)> {
    let spec = spec
        .trim()
        .trim_start_matches("https://huggingface.co/")
        .trim_start_matches("hf.co/");
    let (rest, revision) = match spec.split_once('@') {
        Some((r, rev)) => (r, rev.to_string()),
        None => (spec, "main".to_string()),
    };
    let (repo, quant) = match rest.split_once(':') {
        Some((r, q)) => (r, Some(q.to_string())),
        None => (rest, None),
    };
    let quant_ok = quant.as_deref().is_none_or(|q| {
        !q.is_empty()
            && q.len() <= 64
            && !q.contains("..")
            && q.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    });
    let revision_ok = !revision.is_empty()
        && revision.len() <= 200
        && !revision.contains("..")
        && !revision.contains("//")
        && !revision.starts_with(['/', '-', '.'])
        && !revision.ends_with('/')
        && revision
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c));
    if !is_repo_name(repo) || !quant_ok || !revision_ok {
        bail!("{spec} is not a model name; use org/repo, org/repo:Q4_K_M or org/repo@revision");
    }
    Ok((repo.to_string(), quant, revision))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_name_is_read_and_checked() {
        let ok = |spec: &str| parse_model(spec).unwrap();
        assert_eq!(ok("org/repo"), ("org/repo".into(), None, "main".into()));
        assert_eq!(
            ok(" https://huggingface.co/org_x/Repo-1.5_b:Q4_K_M@refs/pr/3 "),
            (
                "org_x/Repo-1.5_b".into(),
                Some("Q4_K_M".into()),
                "refs/pr/3".into()
            )
        );
        assert_eq!(ok("hf.co/org/repo@v1.0").2, "v1.0");

        for hostile in [
            "",
            "org",
            "org/repo/extra",
            "org/..",
            "../etc/passwd",
            "org/re..po",
            "org/re--po",
            "org/-repo",
            "org/repo.",
            "org/repo.git",
            "org/re po",
            "org/repo;rm",
            "org/$(id)",
            "org/re%2Fpo",
            "org/repo:../../x",
            "org/repo:Q4/../x",
            "org/repo:",
            "org/repo@",
            "org/repo@../../main",
            "org/repo@/etc",
            "org/repo@main//x",
            "org/repo@-x",
            "org/repo@main?x=1",
            "https://evil.example/org/repo",
            "org/rep\u{43e}",
        ] {
            assert!(parse_model(hostile).is_err(), "{hostile:?} was taken");
        }
        assert!(is_repo_name(&format!("org/{}", "a".repeat(96))));
        assert!(!is_repo_name(&format!("org/{}", "a".repeat(97))));
        assert!(!is_repo_name("org/repo:Q4_K_M"));
        assert!(!is_repo_name("org/repo@main"));
    }
}
