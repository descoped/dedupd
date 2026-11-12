//! The `apps` command: what each app's profile says, for the apps as they
//! are installed and for one model.

use crate::{Opened, human};
use anyhow::{Context, Result};
use dedupd_core::library;
use dedupd_core::profile::{self, Installed, Method};
use serde_json::json;

/// "GGUF and MLX", with the bit widths when the method is limited to some.
fn takes(method: &Method) -> String {
    let formats: Vec<&str> = method.formats.iter().map(|f| f.label()).collect();
    let widths: Vec<String> = method.bits.iter().map(|b| format!("{b}-bit")).collect();
    match widths.is_empty() {
        true => formats.join(", "),
        false => format!("{} ({})", formats.join(", "), widths.join(", ")),
    }
}

fn tried(state: &str) -> &'static str {
    match state {
        "tested" => "dedupd was tested with this version",
        "untested" => "dedupd was not tested with this version",
        "broken" => "known not to work with this version",
        _ => "not found on this Mac",
    }
}

pub fn cmd_apps(o: &Opened, repo: Option<String>, as_json: bool) -> Result<()> {
    let installed = Installed::detect(&o.engine.dirs);
    let Some(repo) = repo else {
        return overview(o, &installed, as_json);
    };
    let detail = library::model(&o.engine.db(), &o.engine.store, &repo, &installed)?
        .with_context(|| format!("{repo} is not in the store"))?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&detail.variants)?);
        return Ok(());
    }
    for v in &detail.variants {
        println!("{} ({})", v.spec, human(v.bytes));
        for d in &v.decisions {
            let answer = match d.state.as_str() {
                "ready" if v.placed_in.contains(&d.app) => "in it",
                "ready" => "yes",
                "planned" => "not yet",
                _ => "no",
            };
            println!("  {:<8} {:<8} {}", d.app, answer, d.reason);
        }
    }
    Ok(())
}

fn overview(o: &Opened, installed: &Installed, as_json: bool) -> Result<()> {
    if as_json {
        let apps: Vec<_> = profile::profiles()
            .iter()
            .map(|p| {
                let version = installed.version(p.app);
                json!({
                    "app": p.app.as_str(),
                    "name": p.name,
                    "models": o.engine.dirs.models(p.app),
                    "version": version,
                    "version_state": p.version_state(None, version),
                    "methods": p.methods,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&apps)?);
        return Ok(());
    }
    for p in profile::profiles() {
        let version = installed.version(p.app);
        println!(
            "{}{} ({})",
            p.name,
            version.map(|v| format!(" {v}")).unwrap_or_default(),
            tried(p.version_state(None, version))
        );
        println!("  models in {}", o.engine.dirs.models(p.app).display());
        for m in &p.methods {
            println!(
                "  {:<22} {}, {}{}",
                takes(m),
                m.id,
                m.cost.label(),
                if m.built { "" } else { "; not built yet" }
            );
        }
    }
    Ok(())
}
