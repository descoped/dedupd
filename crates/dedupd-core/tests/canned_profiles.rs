//! The app's canned daemon answers from the real app profiles: this test
//! writes what the profiles decide for the kinds of model the canned
//! daemon holds as Swift, and fails when the profiles changed and the Swift
//! was not written again (`just fixtures`).

use dedupd_api::app::{App, AppInfo};
use dedupd_api::library::{Cost, Decision, DecisionState, Format, VersionState};
use dedupd_core::profile::{self, Installed, Traits};
use std::fmt::Write;
use std::path::Path;

/// The versions the canned daemon finds installed.
fn installed() -> Installed {
    Installed::unknown().with(App::Ollama, "0.40.16")
}

/// The kinds of model the canned daemon shows: a name for Swift, and the
/// traits the profiles judge.
fn cases() -> Vec<(&'static str, Traits)> {
    let traits = |format, split, bits: Option<Vec<u32>>| Traits {
        format,
        split,
        bits,
    };
    vec![
        ("gguf", traits(Format::Gguf, false, None)),
        ("splitGGUF", traits(Format::Gguf, true, None)),
        ("mlxUnknownBits", traits(Format::Mlx, false, None)),
        ("mlx4", traits(Format::Mlx, false, Some(vec![4]))),
        ("mlx6", traits(Format::Mlx, false, Some(vec![6]))),
        ("mlx8", traits(Format::Mlx, false, Some(vec![8]))),
        ("other", traits(Format::Other, false, None)),
    ]
}

fn swift_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn swift_optional(s: Option<&str>) -> String {
    s.map_or_else(|| "nil".to_string(), swift_string)
}

fn swift_app(app: App) -> &'static str {
    match app {
        App::Bionic => ".bionic",
        App::Ollama => ".ollama",
        App::Hf => ".huggingFace",
    }
}

fn swift_format(format: Format) -> &'static str {
    match format {
        Format::Gguf => ".gguf",
        Format::Mlx => ".mlx",
        Format::Other => ".other",
    }
}

fn swift_decision(d: &Decision) -> String {
    let state = match d.state {
        DecisionState::Ready => ".ready",
        DecisionState::Planned => ".planned",
        DecisionState::No => ".no",
    };
    let cost = swift_cost(d.cost);
    let version_state = match d.version_state {
        VersionState::Tested => ".tested",
        VersionState::Untested => ".untested",
        VersionState::Broken => ".broken",
        VersionState::Unknown => ".unknown",
    };
    format!(
        "Decision(\n                app: {}, state: {state}, method: {}, cost: {cost},\n                reason: {},\n                version: {}, versionState: {version_state}\n            )",
        swift_app(d.app),
        swift_optional(d.method.as_deref()),
        swift_string(&d.reason),
        swift_optional(d.version.as_deref()),
    )
}

fn swift_cost(cost: Option<Cost>) -> &'static str {
    match cost {
        None => "nil",
        Some(Cost::None) => ".nothing",
        Some(Cost::Megabytes) => ".megabytes",
        Some(Cost::Full) => ".full",
    }
}

fn swift_app_info(a: &AppInfo) -> String {
    let takes: Vec<String> = a
        .takes
        .iter()
        .map(|t| {
            format!(
                "Taken(format: {}, cost: {})",
                swift_format(t.format),
                swift_cost(Some(t.cost))
            )
        })
        .collect();
    format!(
        "AppInfo(\n            app: {}, name: {}, version: {},\n            takes: [\n{}            ]\n        )",
        swift_app(a.app),
        swift_string(&a.name),
        swift_optional(a.version.as_deref()),
        takes
            .iter()
            .map(|t| format!("                {t},\n"))
            .collect::<String>()
    )
}

fn generate() -> String {
    let installed = installed();
    let mut out = String::from(
        "// Written by `just fixtures` from the daemon's app profiles\n\
         // (crates/dedupd-core/tests/canned_profiles.rs). Do not edit.\n\n\
         import DedupdClient\n\n\
         /// What the app profiles answer for the kinds of model the canned\n\
         /// daemon holds, with Ollama 0.40.16 installed.\n\
         enum RecordedProfiles {\n",
    );
    out.push_str("    static let apps: [AppInfo] = [\n");
    for app in profile::apps(&installed) {
        writeln!(out, "        {},", swift_app_info(&app)).unwrap();
    }
    out.push_str("    ]\n");
    for (name, traits) in cases() {
        writeln!(out, "\n    static let {name}: [Decision] = [").unwrap();
        // Each profile is asked directly, not through `decide_for`: the
        // canned daemon keeps no refusals and has no store to read a
        // model's config from, so the profiles' own answers are what it
        // gives.
        for p in profile::profiles() {
            let d = p.decide(&traits, installed.version(p.app));
            writeln!(
                out,
                "        {},",
                swift_decision(&d).replace("\n    ", "\n")
            )
            .unwrap();
        }
        out.push_str("    ]\n");
    }
    out.push_str("}\n");
    out
}

#[test]
fn canned_profiles_match_the_recorded_ones() {
    let file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../macos-app/Dedupd/Services/RecordedProfiles.swift");
    let now = generate();
    if std::env::var_os("DEDUPD_UPDATE_FIXTURES").is_some() {
        std::fs::write(&file, &now).unwrap();
        return;
    }
    let recorded = std::fs::read_to_string(&file).unwrap_or_else(|_| {
        panic!(
            "{} is missing; record it with just fixtures",
            file.display()
        )
    });
    assert_eq!(
        recorded, now,
        "the app profiles changed; re-record with just fixtures"
    );
}
