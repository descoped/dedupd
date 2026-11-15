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
        let code = text.split("#[cfg(test)]").next().unwrap_or_default();
        let entry = uses.entry(module.clone()).or_default();
        for (i, _) in code.match_indices("crate::") {
            let rest = &code[i + "crate::".len()..];
            let names: Vec<&str> = match rest.strip_prefix('{') {
                Some(group) => group
                    .split('}')
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .map(|n| n.trim().split("::").next().unwrap_or_default())
                    .collect(),
                None => vec![
                    rest.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                        .next()
                        .unwrap_or_default(),
                ],
            };
            entry.extend(
                names
                    .into_iter()
                    .filter(|n| *n != module)
                    .map(str::to_string),
            );
        }
    }
    let known: BTreeSet<String> = uses.keys().cloned().collect();
    for used in uses.values_mut() {
        used.retain(|m| known.contains(m));
    }
    uses
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
