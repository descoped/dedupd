//! The commands as a person runs them: the built program, a scratch home
//! and scratch app folders. What removes lists first and removes only with
//! `--apply`; what a flag must not reach is refused before anything is
//! touched.
#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::process::Command;

const REPO: &str = "org/Repo-GGUF";

struct Scratch {
    dir: tempfile::TempDir,
}

impl Scratch {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let at = |name: &str| dir.path().join(name).to_string_lossy().into_owned();
        // An address nothing listens on: the commands work by themselves.
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let listen = free.local_addr().unwrap();
        drop(free);
        std::fs::create_dir_all(dir.path().join("home")).unwrap();
        std::fs::write(
            dir.path().join("home/config.toml"),
            format!(
                "bionic_models = {:?}\nollama_models = {:?}\nhf_cache = {:?}\nlisten = \"{listen}\"\n",
                at("apps/bionic"),
                at("apps/ollama"),
                at("apps/hf"),
            ),
        )
        .unwrap();
        Self { dir }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    /// Run the program on the scratch home. Returns whether it succeeded
    /// and everything it printed.
    fn run(&self, args: &[&str]) -> (bool, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_dedupd"))
            .arg("--home")
            .arg(self.home())
            .args(["--endpoint", "http://127.0.0.1:1"])
            .args(args)
            .env_remove("HF_TOKEN")
            .env_remove("DEDUPD_HOME")
            .output()
            .unwrap();
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), said)
    }

    fn ok(&self, args: &[&str]) -> String {
        let (ok, said) = self.run(args);
        assert!(ok, "dedupd {args:?} failed: {said}");
        said
    }
}

fn stored_files(home: &Path) -> usize {
    walk(&home.join("store/sha256"))
}

fn walk(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.path().is_dir() {
            true => walk(&e.path()),
            false => 1,
        })
        .sum()
}

#[test]
fn what_removes_lists_first_and_removes_only_with_apply() {
    let s = Scratch::new();
    let model = s
        .dir
        .path()
        .join("apps/bionic")
        .join(REPO)
        .join("model.gguf");
    std::fs::create_dir_all(model.parent().unwrap()).unwrap();
    std::fs::write(&model, vec![5u8; 70_000]).unwrap();
    s.ok(&["adopt", "--offline"]);
    assert_eq!(stored_files(&s.home()), 1);

    // Deactivate: the list, then the removal.
    let said = s.ok(&["deactivate", REPO]);
    assert!(said.contains("Nothing was removed"), "{said}");
    assert!(model.exists());
    // Still in an app: not to be forgotten.
    let (ok, said) = s.run(&["forget", REPO, "--apply"]);
    assert!(
        !ok && said.contains("still has 1 files in app folders"),
        "{said}"
    );
    let said = s.ok(&["deactivate", REPO, "--apply"]);
    assert!(said.contains("removed 1 files"), "{said}");
    assert!(!model.exists());
    assert_eq!(stored_files(&s.home()), 1, "the stored copy stays");

    // Forget: what it would drop, then the drop.
    let said = s.ok(&["forget", REPO]);
    assert!(said.contains("Nothing was changed"), "{said}");
    let said = s.ok(&["gc"]);
    assert!(said.contains(": 0 files"), "still in the library: {said}");
    s.ok(&["forget", REPO, "--apply"]);
    let (ok, said) = s.run(&["forget", REPO]);
    assert!(!ok, "forgotten already: {said}");

    // Gc: the list, then the removal.
    let said = s.ok(&["gc"]);
    assert!(
        said.contains(": 1 files") && said.contains("Nothing was removed"),
        "{said}"
    );
    assert_eq!(stored_files(&s.home()), 1);
    let said = s.ok(&["gc", "--apply"]);
    assert!(said.contains("Removed."), "{said}");
    assert_eq!(stored_files(&s.home()), 0);
}

#[test]
fn asking_what_forget_would_drop_passes_over_an_app_file_that_is_gone() {
    let s = Scratch::new();
    let model = s
        .dir
        .path()
        .join("apps/bionic")
        .join(REPO)
        .join("model.gguf");
    std::fs::create_dir_all(model.parent().unwrap()).unwrap();
    std::fs::write(&model, vec![6u8; 70_000]).unwrap();
    s.ok(&["adopt", "--offline"]);
    // The owner removed the file by hand; no pass has noticed yet.
    std::fs::remove_file(&model).unwrap();
    let said = s.ok(&["forget", REPO]);
    assert!(said.contains("Nothing was changed"), "{said}");
    s.ok(&["forget", REPO, "--apply"]);
}

#[test]
fn an_address_off_this_mac_and_a_label_that_is_none_are_refused() {
    let s = Scratch::new();
    let (ok, said) = s.run(&["serve", "--listen", "0.0.0.0:1"]);
    assert!(!ok && said.contains("listens on this Mac only"), "{said}");
    // Refused before launchd is asked or a file is looked at.
    for label in ["../x", "a/b", ""] {
        for command in ["uninstall", "start", "stop", "agent"] {
            let (ok, said) = s.run(&[command, "--label", label]);
            assert!(
                !ok && said.contains("is not a launchd label"),
                "{command} {label}: {said}"
            );
        }
    }
}

/// The app reads `agent --json` with a decoder of its own. An example of
/// what the command prints is kept beside the control API's recorded
/// answers (`just fixtures` writes it), and the app's tests decode it, so
/// a field that changes here fails there.
#[test]
fn the_recorded_example_of_the_agents_state_is_what_the_type_writes() {
    let example = dedupd_api::daemon::AgentState {
        label: "io.descoped.dedupd.daemon".into(),
        plist: "/Users/example/Library/LaunchAgents/io.descoped.dedupd.daemon.plist".into(),
        installed: true,
        running: true,
        home: "/Users/example/.dedupd".into(),
        program: "/Users/example/.dedupd/bin/dedupd".into(),
        installed_version: Some("0.2.0".into()),
        same_build: false,
        version: "0.2.0".into(),
        log: "/Users/example/.dedupd/daemon.log".into(),
    };
    let written = serde_json::to_string_pretty(&example).unwrap() + "\n";
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../dedupd-server/tests/fixtures/control/agent.json");
    if std::env::var_os("DEDUPD_UPDATE_FIXTURES").is_some() {
        std::fs::write(&path, &written).unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(&path).unwrap_or_default(),
        written,
        "run `just fixtures`"
    );
}

#[test]
fn the_agents_state_is_printed_as_the_type_the_app_reads() {
    let s = Scratch::new();
    // A label no agent has: nothing is installed, and nothing is changed.
    let said = s.ok(&[
        "agent",
        "--json",
        "--label",
        "io.descoped.dedupd.test-not-installed",
    ]);
    let state: dedupd_api::daemon::AgentState = serde_json::from_str(&said).unwrap();
    assert!(!state.installed && !state.running && !state.same_build);
    assert_eq!(state.installed_version, None);
    assert_eq!(state.version, env!("CARGO_PKG_VERSION"));
    assert!(
        state.program.ends_with("home/bin/dedupd"),
        "{}",
        state.program
    );
}
