//! What a model name may be: Hugging Face's own rule for a repository name,
//! and the way a model is asked for, `org/repo[:quant][@revision]`. Text a
//! person typed reaches the provider only through these.

use anyhow::{Result, bail};

/// One half of `org/name`, by Hugging Face's own rule: up to 96 letters,
/// digits, `-`, `_` and `.`; it starts and ends with a letter, a digit or
/// `_`; no `--`, no `..`, and it does not end in `.git`.
pub(crate) fn is_name_part(part: &str) -> bool {
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

/// Whether the text names a model as the provider's own API takes it:
/// `org/name`, or a single name from before Hugging Face had organisations
/// (`gpt2`). The check for a name that goes into an address of the API;
/// what a person types or a client names in a path is `is_repo_name`.
pub fn is_model_name(repo: &str) -> bool {
    is_repo_name(repo) || is_name_part(repo)
}

/// Whether the text is a revision: a branch, a tag, a commit or a ref such
/// as `refs/pr/3`, with nothing in it that climbs out of a path or changes
/// the meaning of an address.
pub fn is_revision(revision: &str) -> bool {
    !revision.is_empty()
        && revision.len() <= 200
        && !revision.contains("..")
        && !revision.contains("//")
        && !revision.starts_with(['/', '-', '.'])
        && !revision.ends_with('/')
        && revision
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c))
}

/// Whether the text is a quantization or a registry tag (`Q4_K_M`,
/// `latest`): one word of up to 64 letters, digits, `-`, `_` and `.`, with
/// no `..` in it. A tag from a request is checked with this before it goes
/// into an address or the database.
pub fn is_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 64
        && !tag.contains("..")
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
}

/// Whether the text is a sha256 as dedupd names files by it: 64 lower-case
/// hex digits. A hash from outside (the provider's listing, a request) is
/// checked with this before it names a file.
pub fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether the text is a git object id: 40 lower-case hex digits.
pub fn is_git_oid(text: &str) -> bool {
    text.len() == 40 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether the text is the path of a file inside a repository, as a git tree
/// can hold it: relative, with `/` between its parts, and no part that is
/// empty, `.` or `..`. Joined to a folder, such a path stays inside it.
pub fn is_file_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.contains(['\\', '\0'])
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Split `org/repo[:quant][@revision]` as typed on the command line. Every
/// part is checked: a quantization is a short word, and a revision is a
/// branch, tag or commit with nothing in it that climbs out of a path.
pub fn parse_model(spec: &str) -> Result<(String, Option<String>, String)> {
    // Each prefix once: `hf.co` is also a name an organisation can have.
    let spec = spec.trim();
    let spec = spec.strip_prefix("https://huggingface.co/").unwrap_or(spec);
    let spec = spec.strip_prefix("hf.co/").unwrap_or(spec);
    let (rest, revision) = match spec.split_once('@') {
        Some((r, rev)) => (r, rev.to_string()),
        None => (spec, "main".to_string()),
    };
    let (repo, quant) = match rest.split_once(':') {
        Some((r, q)) => (r, Some(q.to_string())),
        None => (rest, None),
    };
    let quant_ok = quant.as_deref().is_none_or(is_tag);
    if !is_repo_name(repo) || !quant_ok || !is_revision(&revision) {
        bail!("{spec} is not a model name; use org/repo, org/repo:Q4_K_M or org/repo@revision");
    }
    Ok((repo.to_string(), quant, revision))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_paths_and_hashes_from_outside_are_checked() {
        for good in [
            "model.gguf",
            "sub/config.json",
            "a/b/c.d",
            ".gitattributes",
            "a..b/c",
        ] {
            assert!(is_file_path(good), "{good}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../../x",
            "a/./b",
            "a//b",
            "a/",
            "a\\b",
            "a\0b",
            "..",
        ] {
            assert!(!is_file_path(bad), "{bad:?}");
        }
        let sha = "ab".repeat(32);
        assert!(is_sha256(&sha));
        assert!(is_git_oid(&sha[..40]));
        for bad in [
            "",
            "../../x",
            &sha[..63],
            &sha.to_uppercase(),
            &format!("/{}", &sha[1..]),
        ] {
            assert!(!is_sha256(bad), "{bad:?}");
            assert!(!is_git_oid(bad), "{bad:?}");
        }
    }

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
        // A prefix is taken off once: `hf.co` is also a name.
        assert_eq!(ok("hf.co/hf.co/repo").0, "hf.co/repo");
        assert!(parse_model("hf.co/hf.co/org/repo").is_err());
        assert!(is_model_name("gpt2") && is_model_name("org/repo"));
        assert!(!is_model_name("a/b/c") && !is_model_name("..") && !is_model_name(""));

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
