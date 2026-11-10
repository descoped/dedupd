//! The launchd agent that runs the daemon at login.

use crate::Opened;
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::Command as Process;

/// The launchd label of the daemon.
pub const LABEL: &str = "com.dedupd.daemon";

pub fn launchctl(args: &[&str]) -> Result<std::process::Output> {
    Process::new("/bin/launchctl")
        .args(args)
        .output()
        .context("running launchctl")
}

fn uid() -> u32 {
    // SAFETY: getuid has no failure mode and touches no memory.
    unsafe { libc_getuid() }
}

unsafe extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

fn plist_path(label: &str) -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist")))
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The launchd agent definition: run `dedupd serve` at login and restart it
/// if it stops.
fn plist(label: &str, exe: &str, home: &str, log: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>--home</string>
        <string>{}</string>
        <string>serve</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ProcessType</key>
    <string>Background</string>
    <key>StandardOutPath</key>
    <string>{}</string>
    <key>StandardErrorPath</key>
    <string>{}</string>
</dict>
</plist>
"#,
        xml(label),
        xml(exe),
        xml(home),
        xml(log),
        xml(log)
    )
}

pub fn cmd_install(o: &Opened, env: bool, label: &str) -> Result<()> {
    // The agent runs a copy under the home folder, so rebuilding or
    // cleaning the source tree cannot break the daemon.
    let built = std::env::current_exe().context("finding the dedupd binary")?;
    let exe = o.home.root().join("bin/dedupd");
    if built != exe {
        std::fs::create_dir_all(exe.parent().expect("bin folder"))?;
        let _ = std::fs::remove_file(&exe);
        std::fs::copy(&built, &exe)
            .with_context(|| format!("copying dedupd to {}", exe.display()))?;
    }
    let path = plist_path(label)?;
    let log = o.home.root().join("daemon.log");
    std::fs::create_dir_all(path.parent().expect("plist has a parent"))?;
    std::fs::write(
        &path,
        plist(
            label,
            &exe.to_string_lossy(),
            &o.home.root().to_string_lossy(),
            &log.to_string_lossy(),
        ),
    )
    .with_context(|| format!("writing {}", path.display()))?;
    if env {
        let config = dedupd_core::config::Config::path(&o.home);
        let mut text = std::fs::read_to_string(&config).unwrap_or_default();
        if !text.contains("set_env") {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str("set_env = true\n");
            std::fs::write(&config, text)?;
        }
    }
    let domain = format!("gui/{}", uid());
    let _ = launchctl(&["bootout", &format!("{domain}/{label}")]);
    let out = launchctl(&["bootstrap", &domain, &path.to_string_lossy()])?;
    if !out.status.success() {
        bail!(
            "launchctl could not start the agent: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    println!("Installed {} and started the daemon.", path.display());
    println!("It runs {} and logs to {}.", exe.display(), log.display());
    println!("Run `dedupd env` to see how to point tools at it.");
    Ok(())
}

pub fn cmd_uninstall(label: &str) -> Result<()> {
    let path = plist_path(label)?;
    let _ = launchctl(&["bootout", &format!("gui/{}/{label}", uid())]);
    if label == LABEL {
        let _ = launchctl(&["unsetenv", "HF_ENDPOINT"]);
    }
    match std::fs::remove_file(&path) {
        Ok(()) => println!("Stopped the daemon and removed {}.", path.display()),
        Err(_) => println!("No agent was installed at {}.", path.display()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_escapes_and_names_the_program() {
        let p = plist("com.x", "/a&b/dedupd", "/h<ome", "/log");
        assert!(p.contains("<string>/a&amp;b/dedupd</string>"));
        assert!(p.contains("<string>/h&lt;ome</string>"));
        assert!(p.contains("<string>serve</string>"));
    }
}
