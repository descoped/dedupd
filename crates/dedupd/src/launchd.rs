//! The launchd agent that runs the daemon at login.

use crate::Opened;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command as Process;

/// The launchd label of the daemon. The same name as the signature of the
/// `dedupd` inside the app.
pub const LABEL: &str = "io.descoped.dedupd.daemon";

/// The macOS app's bundle identifier. The agent names it, so that System
/// Settings › General › Login Items lists the daemon under the app's name and
/// icon. Without it macOS shows the name of whoever signed the binary. It has
/// no effect on a Mac that does not have the app.
const APP_BUNDLE_ID: &str = "io.descoped.dedupd.app";

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

/// Whether the text can be a launchd label: letters, digits, `.`, `-` and
/// `_`, and no `..`. The label becomes a file name under `LaunchAgents` and
/// a name launchd is asked about, so nothing in it may leave that folder.
fn is_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 128
        && !label.contains("..")
        && !label.starts_with('.')
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
}

/// Whether an agent file is one dedupd wrote: its program is a `dedupd`
/// and its last argument is `serve`. `install` and `uninstall` touch no
/// other program's agent, whatever label they were given.
fn is_ours(plist: &str) -> bool {
    let Some(arguments) = plist
        .split_once("<key>ProgramArguments</key>")
        .and_then(|(_, rest)| rest.split_once("</array>"))
        .map(|(arguments, _)| arguments)
    else {
        return false;
    };
    let strings: Vec<&str> = arguments
        .split("<string>")
        .skip(1)
        .filter_map(|s| s.split_once("</string>").map(|(text, _)| text))
        .collect();
    let program = strings.first().is_some_and(|p| {
        Path::new(p)
            .file_name()
            .is_some_and(|name| name == "dedupd")
    });
    program && strings.last() == Some(&"serve")
}

/// The agent file for `label`, refused when the label is none, or when a
/// file is there that dedupd did not write.
fn our_plist(label: &str) -> Result<PathBuf> {
    let path = plist_path(label)?;
    match std::fs::read_to_string(&path) {
        Ok(text) if !is_ours(&text) => bail!(
            "{} is not an agent of dedupd; it is left as it is",
            path.display()
        ),
        Ok(_) => Ok(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(path),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Take `HF_ENDPOINT` out of the login session when it still names this
/// daemon's address. One the owner set himself, or another daemon's, stays.
/// True when it was taken out.
fn unset_endpoint(home: &Path) -> bool {
    let Ok(config) = dedupd_core::config::Config::load(&dedupd_core::Home::new(home)) else {
        return false;
    };
    let ours = format!("http://{}", config.listen());
    let set = launchctl(&["getenv", "HF_ENDPOINT"])
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == ours);
    set && launchctl(&["unsetenv", "HF_ENDPOINT"]).is_ok_and(|out| out.status.success())
}

const ENDPOINT_UNSET: &str =
    "HF_ENDPOINT no longer points apps started from the Dock or Finder at the daemon.";

fn plist_path(label: &str) -> Result<PathBuf> {
    if !is_label(label) {
        bail!("{label:?} is not a launchd label (letters, digits, '.', '-' and '_')");
    }
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
    <key>AssociatedBundleIdentifiers</key>
    <string>{}</string>
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
        xml(APP_BUNDLE_ID),
        xml(log),
        xml(log)
    )
}

/// Put the program `built` at `exe`, unless the two are one file under two
/// names (a symlink to the installed program, a relative `--home`): then
/// there is nothing to copy, and removing one would remove both. The copy
/// is made beside `exe` and renamed onto it, so a copy that fails part of
/// the way (a full disk) leaves the installed program as it was.
fn install_program(built: &Path, exe: &Path) -> Result<()> {
    let same = match (std::fs::canonicalize(built), std::fs::canonicalize(exe)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if same {
        return Ok(());
    }
    let folder = exe.parent().expect("bin folder");
    std::fs::create_dir_all(folder)?;
    let tmp = folder.join(format!(".dedupd-{}.tmp", std::process::id()));
    let copied = std::fs::copy(built, &tmp)
        .and_then(|_| std::fs::rename(&tmp, exe))
        .with_context(|| format!("copying dedupd to {}", exe.display()));
    if copied.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    copied
}

/// Write `text` to `path` through a temporary name beside it.
fn write_whole(path: &Path, text: &str) -> Result<()> {
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let written = std::fs::write(&tmp, text)
        .and_then(|()| std::fs::rename(&tmp, path))
        .with_context(|| format!("writing {}", path.display()));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

pub fn cmd_install(o: &Opened, env: bool, label: &str) -> Result<()> {
    // The agent runs a copy under the home folder, so rebuilding or
    // cleaning the source tree cannot break the daemon.
    let built = std::env::current_exe().context("finding the dedupd binary")?;
    let exe = o.home.program_path();
    // Before anything is written: the label is one, and no other
    // program's agent is at its name.
    let path = our_plist(label)?;
    install_program(&built, &exe)?;
    let log = o.home.log_path();
    std::fs::create_dir_all(path.parent().expect("plist has a parent"))?;
    write_whole(
        &path,
        &plist(
            label,
            &exe.to_string_lossy(),
            &o.home.root().to_string_lossy(),
            &log.to_string_lossy(),
        ),
    )?;
    if env {
        // Asked for in so many words, so an existing `set_env = false` is
        // changed too; the rest of the file stays as it is.
        let changes = std::collections::BTreeMap::from([(
            dedupd_api::daemon::Setting::SetEnv,
            serde_json::Value::Bool(true),
        )]);
        dedupd_core::config::Config::update(&o.home, &changes)?;
    }
    let domain = format!("gui/{}", uid());
    let _ = launchctl(&["bootout", &format!("{domain}/{label}")]);
    // `bootout` returns before launchd has let go of a daemon that was
    // running, and a `bootstrap` in that moment fails ("Bootstrap failed: 5:
    // Input/output error"). So it is tried again for a few seconds.
    let bootstrap = || launchctl(&["bootstrap", &domain, &path.to_string_lossy()]);
    let mut out = bootstrap()?;
    for _ in 0..40 {
        if out.status.success() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
        out = bootstrap()?;
    }
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

pub fn cmd_uninstall(home: &Path, label: &str) -> Result<()> {
    let path = our_plist(label)?;
    // launchd is asked to let go also when the file is gone already.
    let _ = launchctl(&["bootout", &format!("gui/{}/{label}", uid())]);
    match std::fs::remove_file(&path) {
        Ok(()) => println!("Stopped the daemon and removed {}.", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("No agent was installed at {}.", path.display());
        }
        Err(e) => return Err(e).with_context(|| format!("removing {}", path.display())),
    }
    if unset_endpoint(home) {
        println!("{ENDPOINT_UNSET}");
    }
    Ok(())
}

fn loaded(label: &str) -> bool {
    launchctl(&["print", &format!("gui/{}/{label}", uid())]).is_ok_and(|out| out.status.success())
}

/// Start the installed agent's daemon.
pub fn cmd_start(label: &str) -> Result<()> {
    let path = our_plist(label)?;
    if !path.is_file() {
        bail!("no agent is installed; run `dedupd install` first");
    }
    if loaded(label) {
        println!("The daemon is already running.");
        return Ok(());
    }
    let out = launchctl(&[
        "bootstrap",
        &format!("gui/{}", uid()),
        &path.to_string_lossy(),
    ])?;
    if !out.status.success() {
        bail!(
            "launchctl could not start the agent: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    println!("Started the daemon.");
    Ok(())
}

/// Stop the daemon and leave the agent installed, so it starts again at the
/// next login.
pub fn cmd_stop(home: &Path, label: &str) -> Result<()> {
    our_plist(label)?;
    if !loaded(label) {
        println!("The daemon is not running.");
        return Ok(());
    }
    let out = launchctl(&["bootout", &format!("gui/{}/{label}", uid())])?;
    if !out.status.success() {
        bail!(
            "launchctl could not stop the agent: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    println!("Stopped the daemon. It starts again at the next login, or with `dedupd start`.");
    // Apps would otherwise be pointed at an address nobody answers on;
    // the daemon sets it again when it starts.
    if unset_endpoint(home) {
        println!("{ENDPOINT_UNSET}");
    }
    Ok(())
}

/// The version an installed copy of dedupd reports.
fn version_of(exe: &Path) -> Option<String> {
    let out = Process::new(exe).arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace().last().map(str::to_string)
}

/// Whether two files hold the same bytes. False when either cannot be read.
/// Read a piece at a time: the two are programs of some tens of megabytes.
fn same_file(a: &Path, b: &Path) -> bool {
    use std::io::Read;
    let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).ok();
    if size(a).is_none() || size(a) != size(b) {
        return false;
    }
    let (Ok(mut a), Ok(mut b)) = (std::fs::File::open(a), std::fs::File::open(b)) else {
        return false;
    };
    let (mut x, mut y) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
    loop {
        let Ok(n) = a.read(&mut x) else {
            return false;
        };
        if n == 0 {
            // The lengths were equal, so the other has ended too.
            return true;
        }
        if b.read_exact(&mut y[..n]).is_err() || x[..n] != y[..n] {
            return false;
        }
    }
}

/// Say whether the agent is installed and running, and which dedupd it runs.
pub fn cmd_agent(home: &Path, label: &str, json: bool) -> Result<()> {
    let plist = plist_path(label)?;
    let home = dedupd_core::Home::new(home);
    let program = home.program_path();
    let installed = plist.is_file();
    let running = loaded(label);
    let installed_version = program.is_file().then(|| version_of(&program)).flatten();
    // Two builds can carry one version number, so the files are compared.
    let same_build = std::env::current_exe()
        .ok()
        .is_some_and(|me| same_file(&me, &program));
    if json {
        let text = |path: &Path| path.to_string_lossy().into_owned();
        let state = dedupd_api::daemon::AgentState {
            label: label.to_string(),
            plist: text(&plist),
            installed,
            running,
            home: text(home.root()),
            program: text(&program),
            installed_version,
            same_build,
            version: env!("CARGO_PKG_VERSION").to_string(),
            log: text(&home.log_path()),
        };
        println!("{}", serde_json::to_string_pretty(&state)?);
        return Ok(());
    }
    match (installed, running) {
        (true, true) => println!("The agent is installed and the daemon is running."),
        (true, false) => println!("The agent is installed; the daemon is stopped."),
        (false, true) => println!("The daemon is running, but its agent file is gone."),
        (false, false) => println!("No agent is installed."),
    }
    println!("  agent    {}", plist.display());
    match installed_version {
        Some(v) => println!("  program  {} (version {v})", program.display()),
        None => println!("  program  {} (not there)", program.display()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installing_through_another_name_for_the_program_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("bin/dedupd");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"the program").unwrap();
        let link = dir.path().join("dedupd-link");
        std::os::unix::fs::symlink(&exe, &link).unwrap();

        // Started through the link: the installed program is the same file.
        install_program(&link, &exe).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"the program");

        // Another build replaces it whole, and leaves nothing beside it.
        let built = dir.path().join("built");
        std::fs::write(&built, b"a newer program").unwrap();
        install_program(&built, &exe).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"a newer program");
        assert_eq!(std::fs::read_dir(exe.parent().unwrap()).unwrap().count(), 1);

        // A program that cannot be read leaves the installed one alone.
        assert!(install_program(&dir.path().join("missing"), &exe).is_err());
        assert_eq!(std::fs::read(&exe).unwrap(), b"a newer program");
    }

    #[test]
    fn same_file_compares_the_bytes() {
        let dir = std::env::temp_dir().join(format!("dedupd-same-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b, c) = (dir.join("a"), dir.join("b"), dir.join("c"));
        std::fs::write(&a, b"one build").unwrap();
        std::fs::write(&b, b"one build").unwrap();
        std::fs::write(&c, b"two build").unwrap();
        assert!(same_file(&a, &b));
        assert!(!same_file(&a, &c));
        assert!(!same_file(&a, &dir.join("missing")));
        // Longer than one piece, and different only at the end.
        let (d, e) = (dir.join("d"), dir.join("e"));
        let mut long = vec![7u8; 200_000];
        std::fs::write(&d, &long).unwrap();
        std::fs::write(&e, &long).unwrap();
        assert!(same_file(&d, &e));
        *long.last_mut().unwrap() = 8;
        std::fs::write(&e, &long).unwrap();
        assert!(!same_file(&d, &e));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_label_is_a_name_and_an_agent_file_is_dedupds_own() {
        for label in [LABEL, "io.descoped.dedupd.trial", "a_b-c.1"] {
            assert!(is_label(label), "{label}");
        }
        for label in ["", "../x", "a/b", "a b", ".hidden", "a..b", "x;y"] {
            assert!(!is_label(label), "{label}");
            assert!(plist_path(label).is_err(), "{label}");
        }
        assert!(is_ours(&plist(
            LABEL,
            "/h/bin/dedupd",
            "/h",
            "/h/daemon.log"
        )));
        // Another program's agent, and a file that is no agent at all.
        let other =
            plist(LABEL, "/h/bin/dedupd", "/h", "/l").replace("/h/bin/dedupd", "/usr/bin/other");
        assert!(!is_ours(&other));
        let no_serve =
            plist(LABEL, "/h/bin/dedupd", "/h", "/l").replace("<string>serve</string>", "");
        assert!(!is_ours(&no_serve));
        assert!(!is_ours("not a plist"));
    }

    #[test]
    fn plist_escapes_and_names_the_program() {
        let p = plist("com.x", "/a&b/dedupd", "/h<ome", "/log");
        assert!(p.contains("<string>/a&amp;b/dedupd</string>"));
        assert!(p.contains("<string>/h&lt;ome</string>"));
        assert!(p.contains("<string>serve</string>"));
    }

    #[test]
    fn plist_ties_the_agent_to_the_app_and_is_well_formed() {
        let p = plist(LABEL, "/h/bin/dedupd", "/h", "/h/daemon.log");
        assert!(p.contains(
            "<key>AssociatedBundleIdentifiers</key>\n    <string>io.descoped.dedupd.app</string>"
        ));
        assert!(p.contains("<string>/h/daemon.log</string>"));
        // The log path fills both log keys, and nothing else.
        assert_eq!(p.matches("/h/daemon.log").count(), 2);
        let file = std::env::temp_dir().join(format!("dedupd-plist-{}.plist", std::process::id()));
        std::fs::write(&file, &p).unwrap();
        let lint = Process::new("/usr/bin/plutil")
            .arg("-lint")
            .arg(&file)
            .output()
            .unwrap();
        let _ = std::fs::remove_file(&file);
        assert!(
            lint.status.success(),
            "{}",
            String::from_utf8_lossy(&lint.stdout)
        );
    }
}
