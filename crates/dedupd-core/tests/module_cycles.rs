//! The core's modules depend on each other in one direction only, so the
//! core can later be split into crates. This reads the sources and fails
//! when two modules reach each other, directly or around a circle.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The core's top-level modules and the ones each uses, outside its tests.
fn uses() -> BTreeMap<String, BTreeSet<String>> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut uses: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut files = vec![src.clone()];
    while let Some(path) = files.pop() {
        if path.is_dir() {
            files.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
            continue;
        }
        if path.extension().is_none_or(|e| e != "rs") || path == src.join("lib.rs") {
            continue;
        }
        let top = path
            .strip_prefix(&src)
            .unwrap()
            .components()
            .next()
            .unwrap();
        let module = top
            .as_os_str()
            .to_string_lossy()
            .trim_end_matches(".rs")
            .to_string();
        let text = std::fs::read_to_string(&path).unwrap();
        let code = without_test_modules(&text);
        let entry = uses.entry(module.clone()).or_default();
        for (i, _) in code.match_indices("crate::") {
            let names = used_names(&code[i + "crate::".len()..]);
            entry.extend(names.into_iter().filter(|n| *n != module));
        }
    }
    let known: BTreeSet<String> = uses.keys().cloned().collect();
    for used in uses.values_mut() {
        used.retain(|m| known.contains(m));
    }
    uses
}

/// The code without its `#[cfg(test)] mod … { … }` blocks: what a test
/// module uses is not a dependency of the module.
fn without_test_modules(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("#[cfg(test)]") {
        out.push_str(&rest[..at]);
        let after = &rest[at + "#[cfg(test)]".len()..];
        let item = after.trim_start();
        if item.starts_with("mod ")
            && let Some(open) = item.find('{')
            && let Some(len) = block_len(&item[open..])
        {
            rest = &item[open + len..];
        } else {
            // Another test-only item: kept, so its uses count too.
            out.push_str("#[cfg(test)]");
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// The length of the block that `text` opens with `{`, through its
/// matching `}`.
fn block_len(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in text.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// The top-level modules named right after a `crate::`: one name, or each
/// of a `{…}` group, nested groups included.
fn used_names(rest: &str) -> Vec<String> {
    let first = |item: &str| -> String {
        item.trim()
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .next()
            .unwrap_or_default()
            .to_string()
    };
    if !rest.starts_with('{') {
        return vec![first(rest)];
    }
    let Some(len) = block_len(rest) else {
        return Vec::new();
    };
    let group = &rest[1..len - 1];
    // Split at the commas that are not inside a nested group.
    let (mut names, mut depth, mut start) = (Vec::new(), 0usize, 0);
    for (i, c) in group.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                names.push(first(&group[start..i]));
                start = i + 1;
            }
            _ => {}
        }
    }
    names.push(first(&group[start..]));
    names.retain(|n| !n.is_empty());
    names
}

#[test]
fn imports_are_read_whole_and_only_test_modules_are_left_out() {
    assert_eq!(used_names("db::{Db, lock};"), ["db"]);
    assert_eq!(used_names("{a::{b, c}, d, e::f::{g}};"), ["a", "d", "e"]);
    let code = "use crate::x;\n#[cfg(test)]\nmod tests {\n use crate::y;\n fn f() { {} }\n}\nuse crate::z;\n";
    let kept = without_test_modules(code);
    assert!(
        kept.contains("crate::x") && kept.contains("crate::z"),
        "{kept}"
    );
    assert!(!kept.contains("crate::y"), "{kept}");
}

/// A path from `from` back to itself, when there is one.
fn circle(uses: &BTreeMap<String, BTreeSet<String>>, from: &str) -> Option<Vec<String>> {
    let mut stack = vec![vec![from.to_string()]];
    let mut seen = BTreeSet::new();
    while let Some(path) = stack.pop() {
        let last = path.last().unwrap();
        for next in &uses[last] {
            if next == from {
                let mut found = path.clone();
                found.push(next.clone());
                return Some(found);
            }
            if seen.insert(next.clone()) {
                let mut longer = path.clone();
                longer.push(next.clone());
                stack.push(longer);
            }
        }
    }
    None
}

#[test]
fn no_two_core_modules_depend_on_each_other() {
    let uses = uses();
    assert!(uses.len() > 10, "the sources were not found");
    let circles: Vec<String> = uses
        .keys()
        .filter_map(|m| circle(&uses, m))
        .map(|c| c.join(" -> "))
        .collect();
    assert!(circles.is_empty(), "modules in a circle: {circles:#?}");
}
