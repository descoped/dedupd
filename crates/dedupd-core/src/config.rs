//! Settings read from `<home>/config.toml`. Every field is optional.

use crate::paths::{Archive, Home, PRIMARY_STORE, StoreRoot, default_archive_name};
use anyhow::{Context, Result};
use dedupd_api::daemon::Setting;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const DEFAULT_LISTEN: &str = dedupd_api::daemon::DEFAULT_ADDRESS;

#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Folder on another disk that holds the second copy of the store, for
    /// example `/Volumes/Archive/dedupd`: the backup archive, which gets a
    /// copy of every stored file.
    pub archive: Option<PathBuf>,
    /// More archives (cold locations), each with a name. They hold only the
    /// models the owner archives to them. The backup archive may be listed
    /// here too, which gives it its name.
    pub archives: Option<Vec<NamedArchive>>,
    /// More stores (warm locations), each on an APFS volume of its own,
    /// besides the one in dedupd's home. A store serves the app folders on
    /// its volume.
    pub stores: Option<Vec<NamedArchive>>,
    /// Address the daemon listens on.
    pub listen: Option<String>,
    /// Hugging Face endpoint to fetch from.
    pub hf_endpoint: Option<String>,
    /// App folders, when they are not in the default place.
    pub bionic_models: Option<PathBuf>,
    pub ollama_models: Option<PathBuf>,
    pub hf_cache: Option<PathBuf>,
    /// Whether the daemon replaces duplicate app files with clones.
    pub reclaim: Option<bool>,
    /// Whether the daemon points apps started from the Dock or Finder at
    /// itself by setting `HF_ENDPOINT` for the login session.
    pub set_env: Option<bool>,
    /// Days between the daemon's full checks of the store; 0 turns them off.
    pub verify_days: Option<u64>,
    /// Seconds between the daemon's scans of the app folders when nothing
    /// was seen to change.
    pub scan_interval: Option<u64>,
    /// The most downloads to run at once. How many actually run follows
    /// what the provider can take.
    pub max_connections: Option<usize>,
    /// Bytes per second all downloads together may use; 0 is no limit.
    pub speed_limit: Option<u64>,
}

/// One archive in `config.toml` (`[[archives]]`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct NamedArchive {
    pub name: String,
    pub path: PathBuf,
}

/// The folder dedupd uses on a folder the owner picked: the folder itself
/// when it is named `dedupd` or already holds an archive, otherwise a
/// `dedupd` folder inside it, so a disk's top folder is not filled.
pub fn archive_folder_for(picked: &std::path::Path) -> PathBuf {
    let is_archive = picked.file_name().is_some_and(|n| n == "dedupd")
        || picked.join("store").is_dir()
        || picked.join("dedupd.sqlite").is_file();
    if is_archive {
        picked.to_path_buf()
    } else {
        picked.join("dedupd")
    }
}

/// Days between full checks of the store unless `verify_days` says otherwise.
const DEFAULT_VERIFY_DAYS: u64 = 7;
/// Seconds between scans of the app folders unless `scan_interval` says
/// otherwise.
const DEFAULT_SCAN_INTERVAL: u64 = 300;

/// Whether the address is one of this Mac itself; the rule the client
/// checks an address with too.
pub use dedupd_api::url::is_loopback;

/// Why an address that is not this Mac's own is refused, in a sentence.
pub fn not_loopback(address: &str) -> String {
    format!(
        "dedupd listens on this Mac only, and {address} is not an address of this Mac itself; use {DEFAULT_LISTEN}"
    )
}

impl Config {
    /// Where the config file of `home` is.
    pub fn path(home: &Home) -> PathBuf {
        home.config_path()
    }

    /// Read the config file. A missing file is an empty config.
    pub fn load(home: &Home) -> Result<Self> {
        let path = Self::path(home);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("reading {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Change settings in the config file and read it again. `changes` maps
    /// a setting to its new value; `null` removes the setting. The rest of
    /// the file, comments included, stays as the owner wrote it. Nothing is
    /// written when the result would not be a valid config.
    pub fn update(home: &Home, changes: &BTreeMap<Setting, serde_json::Value>) -> Result<Self> {
        Self::edit(home, |doc| {
            for (setting, value) in changes {
                let key = setting.as_str();
                let item = match value {
                    serde_json::Value::Null => None,
                    serde_json::Value::Bool(b) => Some(toml_edit::value(*b)),
                    serde_json::Value::String(s) => Some(toml_edit::value(s.as_str())),
                    serde_json::Value::Number(n) => match n.as_i64() {
                        Some(i) => Some(toml_edit::value(i)),
                        None => anyhow::bail!("{key} must be a whole number"),
                    },
                    _ => anyhow::bail!("{key} cannot be a list or a table"),
                };
                match item {
                    Some(item) => doc[key] = item,
                    None => {
                        doc.remove(key);
                    }
                }
            }
            Ok(())
        })
    }

    /// Change the config file with `change`, under the config lock, and
    /// read it again. The rest of the file, comments included, stays as the
    /// owner wrote it; nothing is written when the result would not be a
    /// valid config.
    fn edit(
        home: &Home,
        change: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<()>,
    ) -> Result<Self> {
        let path = Self::path(home);
        std::fs::create_dir_all(home.root())?;
        // The daemon (a setting changed in the app) and the command line
        // (`install --env`) both come here: one at a time, so neither
        // drops the other's change.
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(home.root().join("config.lock"))
            .context("opening the config lock")?;
        lock.lock().context("waiting for the config lock")?;
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let mut doc: toml_edit::DocumentMut = text
            .parse()
            .with_context(|| format!("reading {}", path.display()))?;
        change(&mut doc)?;
        let text = doc.to_string();
        let config: Self = toml::from_str(&text).context("the settings are not valid")?;
        if !is_loopback(config.listen()) {
            anyhow::bail!("{}", not_loopback(config.listen()));
        }
        crate::paths::write_in_one_step(&path, text.as_bytes())
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(config)
    }

    /// The address the daemon listens on: the setting, or the default.
    pub fn listen(&self) -> &str {
        self.listen.as_deref().unwrap_or(DEFAULT_LISTEN)
    }

    pub fn hf_endpoint(&self) -> &str {
        self.hf_endpoint
            .as_deref()
            .unwrap_or(crate::hub::DEFAULT_ENDPOINT)
    }

    /// Duplicate app files are replaced with clones unless turned off.
    pub fn reclaim(&self) -> bool {
        self.reclaim != Some(false)
    }

    /// `HF_ENDPOINT` is set for the login session only when asked for.
    pub fn set_env(&self) -> bool {
        self.set_env == Some(true)
    }

    /// Days between full checks of the store; 0 turns them off.
    pub fn verify_days(&self) -> u64 {
        self.verify_days.unwrap_or(DEFAULT_VERIFY_DAYS)
    }

    /// Seconds between scans of the app folders when nothing was seen to
    /// change.
    pub fn scan_interval(&self) -> u64 {
        self.scan_interval.unwrap_or(DEFAULT_SCAN_INTERVAL)
    }

    pub fn max_connections(&self) -> usize {
        self.max_connections
            .unwrap_or(crate::governor::DEFAULT_CONNECTIONS)
            .max(1)
    }

    pub fn speed_limit(&self) -> u64 {
        self.speed_limit.unwrap_or(0)
    }

    pub fn archive_home(&self) -> Option<Home> {
        self.archive.clone().map(Home::new)
    }

    /// The stores besides the one in dedupd's home, in the order the file
    /// lists them.
    pub fn store_list(&self) -> Vec<StoreRoot> {
        self.stores
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|s| StoreRoot {
                name: s.name,
                home: Home::new(&s.path),
                primary: false,
            })
            .collect()
    }

    /// Write the list of stores besides the one in dedupd's home. A picked
    /// folder becomes the `dedupd` folder in it and a missing name the
    /// disk's; a folder that is a store already stays as it is. A new store
    /// must be on an attached APFS volume that has no store yet (the home's
    /// included). Refused, with nothing written, when a name or a folder is
    /// there twice, or a rule does not hold.
    pub fn set_stores(home: &Home, stores: &[NamedArchive]) -> Result<Self> {
        let known: Vec<PathBuf> = Self::load(home)?
            .store_list()
            .into_iter()
            .map(|s| s.home.root().to_path_buf())
            .collect();
        let mut list: Vec<NamedArchive> = Vec::new();
        let mut volumes: Vec<u64> = vec![crate::paths::volume_of(&home.store_dir())?];
        for s in stores {
            if !s.path.is_absolute() {
                anyhow::bail!(
                    "a store's folder must be a full path, not {}",
                    s.path.display()
                );
            }
            let is_known = known.iter().any(|k| k == &s.path);
            let path = if is_known {
                s.path.clone()
            } else {
                archive_folder_for(&s.path)
            };
            let name = match s.name.trim() {
                "" => default_archive_name(&path),
                name => name.to_string(),
            };
            if name.eq_ignore_ascii_case(PRIMARY_STORE)
                || list.iter().any(|b| b.name.eq_ignore_ascii_case(&name))
            {
                anyhow::bail!("two stores are named {name}; give each its own name");
            }
            if list.iter().any(|b| b.path == path) {
                anyhow::bail!("{} is listed twice", path.display());
            }
            if path.starts_with(home.root()) || home.root().starts_with(&path) {
                anyhow::bail!(
                    "{} is dedupd's own folder, which has its store already",
                    path.display()
                );
            }
            // A store already in use keeps its place while its disk is away.
            if !is_known || Home::new(&path).attached() {
                let volume = crate::paths::volume_of(&path)
                    .with_context(|| format!("the disk of {} is not attached", path.display()))?;
                if !crate::paths::is_apfs(&path)? {
                    anyhow::bail!(
                        "{} is not on an APFS volume; a store needs one, because apps get clones of its files",
                        path.display()
                    );
                }
                if volumes.contains(&volume) {
                    anyhow::bail!(
                        "the disk of {} has a store already; one store serves every app folder on a disk",
                        path.display()
                    );
                }
                volumes.push(volume);
            }
            list.push(NamedArchive { name, path });
        }
        Self::edit(home, |doc| {
            if list.is_empty() {
                doc.remove("stores");
            } else {
                let mut tables = toml_edit::ArrayOfTables::new();
                for s in &list {
                    let mut t = toml_edit::Table::new();
                    t["name"] = toml_edit::value(s.name.as_str());
                    t["path"] = toml_edit::value(s.path.to_string_lossy().as_ref());
                    tables.push(t);
                }
                doc["stores"] = toml_edit::Item::ArrayOfTables(tables);
            }
            Ok(())
        })
    }

    /// Every archive: the backup archive first, then the others in the
    /// order the file lists them. The backup archive is named by its entry
    /// in `archives` when it has one, otherwise after its disk.
    pub fn archive_list(&self) -> Vec<Archive> {
        let listed = self.archives.clone().unwrap_or_default();
        let mut out = Vec::new();
        if let Some(path) = &self.archive {
            let name = listed
                .iter()
                .find(|a| &a.path == path)
                .map(|a| a.name.clone())
                .unwrap_or_else(|| default_archive_name(path));
            out.push(Archive {
                name,
                home: Home::new(path),
                backup: true,
            });
        }
        for a in listed {
            if self.archive.as_ref() != Some(&a.path) {
                out.push(Archive {
                    name: a.name,
                    home: Home::new(&a.path),
                    backup: false,
                });
            }
        }
        out
    }

    /// Write the list of archives: every one as `[[archives]]`, and the
    /// backup archive (by its folder) as `archive`, or none. A picked
    /// folder becomes the `dedupd` folder in it (`archive_folder_for`), and
    /// a missing name becomes the disk's. Refused, with nothing written,
    /// when a name or a folder is there twice, a folder is not a full path,
    /// or the backup is not one of the list.
    pub fn set_archives(
        home: &Home,
        archives: &[NamedArchive],
        backup: Option<&std::path::Path>,
    ) -> Result<Self> {
        // A folder that is an archive already stays as it is, whatever its
        // name and whether its disk is attached: the app sends the whole
        // list with every change. Only a newly picked folder gets the
        // `dedupd` folder in it.
        let known: Vec<PathBuf> = Self::load(home)?
            .archive_list()
            .into_iter()
            .map(|a| a.home.root().to_path_buf())
            .collect();
        let folder = |picked: &std::path::Path| {
            if known.iter().any(|k| k == picked) {
                picked.to_path_buf()
            } else {
                archive_folder_for(picked)
            }
        };
        let mut list: Vec<NamedArchive> = Vec::new();
        for a in archives {
            if !a.path.is_absolute() {
                anyhow::bail!(
                    "an archive's folder must be a full path, not {}",
                    a.path.display()
                );
            }
            let path = folder(&a.path);
            let name = match a.name.trim() {
                "" => default_archive_name(&path),
                name => name.to_string(),
            };
            if list.iter().any(|b| b.name.eq_ignore_ascii_case(&name)) {
                anyhow::bail!("two archives are named {name}; give each its own name");
            }
            if list.iter().any(|b| b.path == path) {
                anyhow::bail!("{} is listed twice", path.display());
            }
            if path.starts_with(home.root()) || home.root().starts_with(&path) {
                anyhow::bail!(
                    "{} is dedupd's own folder; an archive must be a folder of its own",
                    path.display()
                );
            }
            list.push(NamedArchive { name, path });
        }
        let backup = match backup {
            None => None,
            Some(b) => {
                let b = folder(b);
                if !list.iter().any(|a| a.path == b) {
                    anyhow::bail!(
                        "the backup archive {} is not one of the archives",
                        b.display()
                    );
                }
                Some(b)
            }
        };
        Self::edit(home, |doc| {
            match &backup {
                Some(b) => doc["archive"] = toml_edit::value(b.to_string_lossy().as_ref()),
                None => {
                    doc.remove("archive");
                }
            }
            if list.is_empty() {
                doc.remove("archives");
            } else {
                let mut tables = toml_edit::ArrayOfTables::new();
                for a in &list {
                    let mut t = toml_edit::Table::new();
                    t["name"] = toml_edit::value(a.name.as_str());
                    t["path"] = toml_edit::value(a.path.to_string_lossy().as_ref());
                    tables.push(t);
                }
                doc["archives"] = toml_edit::Item::ArrayOfTables(tables);
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn a_store_is_refused_where_it_could_not_serve_its_own_disk() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path().join("home"));
        std::fs::create_dir_all(home.store_dir()).unwrap();
        let entry = |name: &str, path: &Path| NamedArchive {
            name: name.into(),
            path: path.to_path_buf(),
        };
        // On the disk of dedupd's home, which has its store already.
        let other = dir.path().join("elsewhere");
        let refused = Config::set_stores(&home, &[entry("Other", &other)]).unwrap_err();
        assert!(
            refused.to_string().contains("has a store already"),
            "{refused:#}"
        );
        // Inside the home, a relative folder, and the primary's name.
        let inside = Config::set_stores(&home, &[entry("In", home.root())]).unwrap_err();
        assert!(
            inside.to_string().contains("dedupd's own folder"),
            "{inside:#}"
        );
        let relative = Config::set_stores(&home, &[entry("R", Path::new("x"))]).unwrap_err();
        assert!(relative.to_string().contains("full path"), "{relative:#}");
        let named = Config::set_stores(&home, &[entry("internal", Path::new("/Volumes/Gone"))])
            .unwrap_err();
        assert!(named.to_string().contains("named"), "{named:#}");
        // A disk that is not attached cannot be added.
        let away = Config::set_stores(&home, &[entry("Gone", Path::new("/Volumes/dedupd-none"))])
            .unwrap_err();
        assert!(away.to_string().contains("not attached"), "{away:#}");
        // Nothing was written; an empty list is accepted.
        assert!(Config::load(&home).unwrap().stores.is_none());
        assert!(
            Config::set_stores(&home, &[])
                .unwrap()
                .store_list()
                .is_empty()
        );
    }

    #[test]
    fn only_an_address_of_this_mac_itself_is_listened_on() {
        for ours in [
            "127.0.0.1:8642",
            "[::1]:8642",
            "localhost:8642",
            "127.0.0.1:0",
        ] {
            assert!(is_loopback(ours), "{ours}");
        }
        for other in [
            "0.0.0.0:8642",
            "192.168.1.5:8642",
            "[::]:8642",
            "localhost",
            "example.com:80",
            "",
        ] {
            assert!(!is_loopback(other), "{other}");
        }
        // The setting is refused, and the file stays as it was.
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path());
        let changes = BTreeMap::from([(Setting::Listen, serde_json::json!("0.0.0.0:8642"))]);
        let refused = Config::update(&home, &changes).unwrap_err();
        assert!(refused.to_string().contains("this Mac only"), "{refused:#}");
        assert!(!Config::path(&home).exists());
    }

    #[test]
    fn update_keeps_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path());
        std::fs::write(
            Config::path(&home),
            "# where the second copy goes\narchive = \"/Volumes/Archive/dedupd\"\nverify_days = 30\n",
        )
        .unwrap();
        let changes = BTreeMap::from([
            (Setting::MaxConnections, serde_json::json!(4)),
            (Setting::VerifyDays, serde_json::Value::Null),
        ]);
        let config = Config::update(&home, &changes).unwrap();
        assert_eq!(
            (config.max_connections, config.verify_days),
            (Some(4), None)
        );
        assert_eq!(
            std::fs::read_to_string(Config::path(&home)).unwrap(),
            "# where the second copy goes\narchive = \"/Volumes/Archive/dedupd\"\nmax_connections = 4\n"
        );

        // A value of the wrong kind changes nothing.
        let bad = BTreeMap::from([(Setting::MaxConnections, serde_json::json!("many"))]);
        assert!(Config::update(&home, &bad).is_err());
        assert_eq!(Config::load(&home).unwrap().max_connections, Some(4));
    }

    #[test]
    fn archives_are_named_and_kept_in_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path().join("home"));
        std::fs::create_dir_all(home.root()).unwrap();
        std::fs::write(Config::path(&home), "# mine\nverify_days = 30\n").unwrap();
        let drawer = std::path::PathBuf::from("/Volumes/Drawer");
        let nas = dir.path().join("nas");
        std::fs::create_dir_all(nas.join("store")).unwrap();
        let list = [
            NamedArchive {
                name: String::new(),
                path: drawer.clone(),
            },
            NamedArchive {
                name: "NAS".into(),
                path: nas.clone(),
            },
        ];
        let config = Config::set_archives(&home, &list, Some(&drawer)).unwrap();
        let archives = config.archive_list();
        // A disk's top folder gets a dedupd folder, named after the disk; a
        // folder that holds an archive already is used as it is.
        assert_eq!(
            archives
                .iter()
                .map(|a| (a.name.as_str(), a.home.root().to_path_buf(), a.backup))
                .collect::<Vec<_>>(),
            [
                ("Drawer", drawer.join("dedupd"), true),
                ("NAS", nas.clone(), false)
            ]
        );
        assert_eq!(config.archive, Some(drawer.join("dedupd")));
        let text = std::fs::read_to_string(Config::path(&home)).unwrap();
        assert!(text.starts_with("# mine\nverify_days = 30\n"), "{text}");

        // Refused, and the file stays: one name twice, a backup not listed,
        // a folder that is not a full path, dedupd's own folder.
        let twice = [
            NamedArchive {
                name: "A".into(),
                path: drawer.clone(),
            },
            NamedArchive {
                name: "a".into(),
                path: nas.clone(),
            },
        ];
        assert!(Config::set_archives(&home, &twice, None).is_err());
        assert!(Config::set_archives(&home, &list[1..], Some(&drawer)).is_err());
        let relative = [NamedArchive {
            name: "X".into(),
            path: "here".into(),
        }];
        assert!(Config::set_archives(&home, &relative, None).is_err());
        let own = [NamedArchive {
            name: "X".into(),
            path: home.root().join("old"),
        }];
        assert!(Config::set_archives(&home, &own, None).is_err());
        assert_eq!(std::fs::read_to_string(Config::path(&home)).unwrap(), text);

        // No archives: both keys go.
        let none = Config::set_archives(&home, &[], None).unwrap();
        assert!(none.archive_list().is_empty());
        assert_eq!(
            std::fs::read_to_string(Config::path(&home)).unwrap(),
            "# mine\nverify_days = 30\n"
        );

        // An archive written before, whose folder is not named dedupd and
        // whose disk is away, keeps its folder when the list is sent again.
        let ext = std::path::PathBuf::from("/Volumes/Ext/models-backup");
        std::fs::write(
            Config::path(&home),
            "archive = \"/Volumes/Ext/models-backup\"\n",
        )
        .unwrap();
        let mut again: Vec<NamedArchive> = Config::load(&home)
            .unwrap()
            .archive_list()
            .into_iter()
            .map(|a| NamedArchive {
                name: a.name,
                path: a.home.root().to_path_buf(),
            })
            .collect();
        again.push(NamedArchive {
            name: "NAS".into(),
            path: nas.clone(),
        });
        let kept = Config::set_archives(&home, &again, Some(&ext)).unwrap();
        assert_eq!(kept.archive.as_ref(), Some(&ext));
        assert_eq!(
            kept.archive_list()
                .iter()
                .map(|a| (a.name.as_str(), a.home.root().to_path_buf()))
                .collect::<Vec<_>>(),
            [("Ext", ext.clone()), ("NAS", nas.clone())]
        );
    }

    /// What `dedupd install --env` does: a `set_env = false` line, or a
    /// comment that names the setting, does not keep it off.
    #[test]
    fn update_turns_set_env_on_over_a_false_line_and_a_comment() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path());
        let changes = BTreeMap::from([(Setting::SetEnv, serde_json::json!(true))]);
        for text in [
            "# set_env = true\nset_env = false\n",
            "# set_env = true\n",
            "",
        ] {
            std::fs::write(Config::path(&home), text).unwrap();
            let config = Config::update(&home, &changes).unwrap();
            assert_eq!(config.set_env, Some(true), "from {text:?}");
            let written = std::fs::read_to_string(Config::path(&home)).unwrap();
            assert_eq!(
                written.matches("set_env = true").count(),
                if text.is_empty() { 1 } else { 2 }
            );
        }
    }
}
