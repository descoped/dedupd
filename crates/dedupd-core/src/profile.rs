//! App profiles: for each app, the ways a model can be put into it, what
//! each costs in disk space, and the versions of the app it was tested with.
//!
//! The profiles are data (`profiles/*.toml`, compiled in). `Profile::decide`
//! turns a profile, what is known about a model and the installed version of
//! the app into one answer: the method, its cost and a reason in a sentence.
//! The same input always gives the same answer.

use crate::place::{App, AppDirs};
use crate::select::Variant;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

const SOURCES: [&str; 3] = [
    include_str!("../profiles/bionic.toml"),
    include_str!("../profiles/ollama.toml"),
    include_str!("../profiles/hf.toml"),
];

/// The form a model's weights are in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Gguf,
    Mlx,
    /// Anything else: plain safetensors, ONNX and so on.
    Other,
}

impl Format {
    /// The format's name in a sentence.
    pub fn label(self) -> &'static str {
        match self {
            Format::Gguf => "GGUF",
            Format::Mlx => "MLX",
            Format::Other => "other",
        }
    }
}

impl Cost {
    /// The cost in words.
    pub fn label(self) -> &'static str {
        match self {
            Cost::None => "no new disk space",
            Cost::Megabytes => "some megabytes of new disk space",
            Cost::Full => "a second copy",
        }
    }
}

/// New disk space an activation takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Cost {
    /// A clone: nothing.
    None,
    /// A clone with a part rewritten.
    Megabytes,
    /// A second copy.
    Full,
}

/// Where an app's installed version is read from.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Detect {
    /// The app bundle whose `Info.plist` states the version.
    pub bundle: Option<String>,
    /// A file in the app's models folder that holds the version.
    pub file: Option<String>,
}

/// One way of putting a model into an app.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Method {
    pub id: String,
    /// Whether dedupd can do it. A method that is not built yet still
    /// tells the owner that the app could take the model.
    pub built: bool,
    pub cost: Cost,
    /// The formats it applies to.
    pub formats: Vec<Format>,
    /// The quantization widths the app runs; empty when that does not matter.
    #[serde(default)]
    pub bits: Vec<u32>,
    /// What rules the method out: a trait of the model and the reason.
    #[serde(default)]
    pub unless: BTreeMap<String, String>,
    /// What to tell the owner when this is the answer.
    pub says: String,
    /// The versions of the app it was run against.
    #[serde(default)]
    pub tested: Vec<String>,
    /// The versions it is known not to work with.
    #[serde(default)]
    pub broken: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub app: App,
    /// The app's name at the start of a sentence.
    pub name: String,
    /// What to say when no method applies to the model's format.
    pub otherwise: String,
    #[serde(default)]
    pub detect: Detect,
    /// Cheapest first.
    #[serde(rename = "method")]
    pub methods: Vec<Method>,
}

/// What a decision needs to know about a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Traits {
    pub format: Format,
    /// A GGUF in several parts.
    pub split: bool,
    /// The quantization widths in the model, when its config says. Empty
    /// when it is not known, for example before the model is downloaded.
    pub bits: Vec<u32>,
}

impl Traits {
    /// The traits of one variant of a repository whose provider lists
    /// `formats` for it.
    pub fn of(variant: &Variant, formats: &[String]) -> Self {
        let ggufs = match variant.kind.as_str() {
            "gguf" => variant.weights.len(),
            _ => 0,
        };
        Self::of_files(ggufs, formats)
    }

    /// The traits of a set of files with `ggufs` GGUF weight files among
    /// them.
    pub fn of_files(ggufs: usize, formats: &[String]) -> Self {
        let format = match ggufs {
            0 if formats.iter().any(|f| f == "mlx") => Format::Mlx,
            0 => Format::Other,
            _ => Format::Gguf,
        };
        Self {
            format,
            split: ggufs > 1,
            bits: Vec::new(),
        }
    }

    pub fn with_bits(mut self, bits: Vec<u32>) -> Self {
        self.bits = bits;
        self
    }

    fn has(&self, name: &str) -> bool {
        match name {
            "split" => self.split,
            _ => false,
        }
    }
}

/// The quantization widths an MLX model's `config.json` names: its default
/// and those of the layers that differ.
pub fn quantization_bits(config: &serde_json::Value) -> Vec<u32> {
    let Some(q) = config.get("quantization").and_then(|q| q.as_object()) else {
        return Vec::new();
    };
    let bits = |v: &serde_json::Value| v.get("bits").and_then(|b| b.as_u64()).map(|b| b as u32);
    let mut out: Vec<u32> = q
        .get("bits")
        .and_then(|b| b.as_u64())
        .map(|b| b as u32)
        .into_iter()
        .chain(q.values().filter_map(bits))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// The answer for one app and one model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Decision {
    pub app: String,
    /// `ready` when dedupd can put the model into the app, `planned` when
    /// the app could take it and dedupd cannot do it yet, `no` when the app
    /// cannot take it.
    pub state: String,
    /// The method, unless the state is `no`.
    pub method: Option<String>,
    /// New disk space the method takes: `none`, `megabytes` or `full`.
    pub cost: Option<Cost>,
    /// Why, in one sentence.
    pub reason: String,
    /// The installed version of the app, when it was found.
    pub version: Option<String>,
    /// `tested`, `untested` or `broken` for that version, `unknown` when
    /// the version was not found.
    pub version_state: String,
}

fn list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [most @ .., last] => format!("{} and {last}", most.join(", ")),
    }
}

fn widths(bits: &[u32]) -> String {
    list(
        &bits
            .iter()
            .map(|b| format!("{b}-bit"))
            .collect::<Vec<String>>(),
    )
}

impl Profile {
    /// Whether `version` of the app was tried: `tested`, `untested`,
    /// `broken`, or `unknown` without a version. For one method, or for any
    /// of them when none is given.
    pub fn version_state(&self, method: Option<&Method>, version: Option<&str>) -> &'static str {
        let Some(version) = version else {
            return "unknown";
        };
        let listed = |pick: fn(&Method) -> &Vec<String>| match method {
            Some(m) => pick(m).iter().any(|v| v == version),
            None => self
                .methods
                .iter()
                .any(|m| pick(m).iter().any(|v| v == version)),
        };
        if listed(|m| &m.broken) {
            "broken"
        } else if listed(|m| &m.tested) {
            "tested"
        } else {
            "untested"
        }
    }

    /// Why a model with other bit widths than the app runs is refused. The
    /// claim was only observed with the tested versions, and says so for
    /// any other.
    fn width_reason(&self, method: &Method, traits: &Traits, version: Option<&str>) -> String {
        let wrong: Vec<u32> = traits
            .bits
            .iter()
            .copied()
            .filter(|b| !method.bits.contains(b))
            .collect();
        let (wrong, runs, format) = (widths(&wrong), widths(&method.bits), traits.format.label());
        match version.filter(|v| method.tested.iter().any(|t| t == v)) {
            Some(v) => format!(
                "{} {v} cannot run {wrong} {format} models; it runs {runs} ones",
                self.name
            ),
            None => format!(
                "{} could not run {wrong} {format} models when dedupd tried it ({}); it ran {runs} ones",
                self.name,
                list(&method.tested)
            ),
        }
    }

    /// The method for a model, given the app's installed version.
    pub fn decide(&self, traits: &Traits, version: Option<&str>) -> Decision {
        let answer = |state: &str, method: Option<&Method>, reason: String| Decision {
            app: self.app.as_str().to_string(),
            state: state.to_string(),
            method: method.filter(|_| state != "no").map(|m| m.id.clone()),
            cost: method.filter(|_| state != "no").map(|m| m.cost),
            reason,
            version: version.map(str::to_string),
            version_state: self.version_state(method, version).to_string(),
        };
        // The first reason a method that fits the format gives for not
        // being used, and the first method that is only waiting to be built.
        let mut refused: Option<(&Method, String)> = None;
        let mut planned: Option<&Method> = None;
        for method in &self.methods {
            if !method.formats.contains(&traits.format) {
                continue;
            }
            let reason = if version.is_some_and(|v| method.broken.iter().any(|b| b == v)) {
                Some(format!(
                    "This does not work with {} {}",
                    self.name,
                    version.unwrap_or_default()
                ))
            } else if let Some((_, reason)) = method.unless.iter().find(|(t, _)| traits.has(t)) {
                Some(reason.clone())
            } else if !method.bits.is_empty()
                && traits.bits.iter().any(|b| !method.bits.contains(b))
            {
                Some(self.width_reason(method, traits, version))
            } else {
                None
            };
            match reason {
                Some(reason) => {
                    refused.get_or_insert((method, reason));
                }
                None if method.built => return answer("ready", Some(method), method.says.clone()),
                None => {
                    planned.get_or_insert(method);
                }
            }
        }
        match (planned, refused) {
            (Some(method), _) => answer("planned", Some(method), method.says.clone()),
            (None, Some((method, reason))) => answer("no", Some(method), reason),
            (None, None) => answer("no", None, self.otherwise.clone()),
        }
    }
}

/// Every app's profile, in the order of `App::ALL`.
pub fn profiles() -> &'static [Profile] {
    static PROFILES: OnceLock<Vec<Profile>> = OnceLock::new();
    PROFILES.get_or_init(|| {
        let parsed: Vec<Profile> = SOURCES
            .iter()
            .map(|text| toml::from_str(text).expect("a profile that is compiled in is valid"))
            .collect();
        App::ALL
            .iter()
            .map(|app| {
                parsed
                    .iter()
                    .find(|p| p.app == *app)
                    .cloned()
                    .expect("every app has a profile")
            })
            .collect()
    })
}

pub fn profile(app: App) -> &'static Profile {
    profiles()
        .iter()
        .find(|p| p.app == app)
        .expect("every app has a profile")
}

/// The versions of the apps that are installed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Installed(BTreeMap<&'static str, String>);

/// The value of `CFBundleShortVersionString` in an XML property list.
fn bundle_version(plist: &Path) -> Option<String> {
    let text = std::fs::read_to_string(plist).ok()?;
    let rest = text.split("<key>CFBundleShortVersionString</key>").nth(1)?;
    let value = rest.split("<string>").nth(1)?.split("</string>").next()?;
    Some(value.trim().to_string()).filter(|v| !v.is_empty())
}

impl Installed {
    /// Nothing is known about any app.
    pub fn unknown() -> Self {
        Self::default()
    }

    /// Read each app's version from where its profile says it is.
    pub fn detect(dirs: &AppDirs) -> Self {
        let mut found = BTreeMap::new();
        for p in profiles() {
            let version = match (&p.detect.bundle, &p.detect.file) {
                (Some(bundle), _) => {
                    bundle_version(&dirs.apps.join(bundle).join("Contents/Info.plist"))
                }
                (None, Some(file)) => std::fs::read_to_string(dirs.models(p.app).join(file))
                    .ok()
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty()),
                (None, None) => None,
            };
            if let Some(version) = version {
                found.insert(p.app.as_str(), version);
            }
        }
        Self(found)
    }

    pub fn with(mut self, app: App, version: &str) -> Self {
        self.0.insert(app.as_str(), version.to_string());
        self
    }

    pub fn version(&self, app: App) -> Option<&str> {
        self.0.get(app.as_str()).map(String::as_str)
    }
}

/// The answer of every app for one model.
pub fn decisions(traits: &Traits, installed: &Installed) -> Vec<Decision> {
    profiles()
        .iter()
        .map(|p| p.decide(traits, installed.version(p.app)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn traits(format: Format) -> Traits {
        Traits {
            format,
            split: false,
            bits: Vec::new(),
        }
    }

    fn states(traits: &Traits) -> Vec<(String, String)> {
        decisions(traits, &Installed::unknown())
            .into_iter()
            .map(|d| (d.app, d.state))
            .collect()
    }

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn every_app_has_a_profile_with_a_method() {
        let all = profiles();
        assert_eq!(all.len(), App::ALL.len());
        for (p, app) in all.iter().zip(App::ALL) {
            assert_eq!(p.app, app);
            assert!(!p.methods.is_empty());
            assert!(p.detect.bundle.is_some() || p.detect.file.is_some());
        }
    }

    #[test]
    fn a_single_gguf_is_ready_everywhere_and_costs_nothing() {
        let all = decisions(&traits(Format::Gguf), &Installed::unknown());
        for d in &all {
            assert_eq!(d.state, "ready");
            assert_eq!(d.method.as_deref(), Some("clone"));
            assert_eq!(d.cost, Some(Cost::None));
            assert_eq!(d.version_state, "unknown");
        }
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn a_split_gguf_is_refused_by_ollama_with_the_reason() {
        let mut t = traits(Format::Gguf);
        t.split = true;
        assert_eq!(
            states(&t),
            pairs(&[("bionic", "ready"), ("ollama", "no"), ("hf", "ready")])
        );
        let ollama = profile(App::Ollama).decide(&t, None);
        assert_eq!(
            ollama.reason,
            "Ollama cannot use a GGUF that is split into parts"
        );
        assert_eq!(ollama.method, None);
        assert_eq!(ollama.cost, None);
    }

    #[test]
    fn an_mlx_model_is_planned_for_ollama_when_its_widths_are_run() {
        for bits in [vec![], vec![4], vec![4, 8]] {
            let t = traits(Format::Mlx).with_bits(bits);
            assert_eq!(
                states(&t),
                pairs(&[("bionic", "ready"), ("ollama", "planned"), ("hf", "ready")])
            );
            let d = profile(App::Ollama).decide(&t, Some("0.40.16"));
            assert_eq!(d.method.as_deref(), Some("clone-with-rewritten-index"));
            assert_eq!(d.cost, Some(Cost::Megabytes));
            assert_eq!(d.version_state, "tested");
        }
    }

    #[test]
    fn other_widths_are_refused_and_the_claim_names_what_was_tested() {
        let t = traits(Format::Mlx).with_bits(vec![5]);
        let tested = profile(App::Ollama).decide(&t, Some("0.40.16"));
        assert_eq!(tested.state, "no");
        assert_eq!(
            tested.reason,
            "Ollama 0.40.16 cannot run 5-bit MLX models; it runs 4-bit and 8-bit ones"
        );
        let other = profile(App::Ollama).decide(&t, Some("0.41.0"));
        assert_eq!(other.version_state, "untested");
        assert_eq!(
            other.reason,
            "Ollama could not run 5-bit MLX models when dedupd tried it (0.40.16); \
             it ran 4-bit and 8-bit ones"
        );
        let mixed = traits(Format::Mlx).with_bits(vec![4, 6]);
        assert!(
            profile(App::Ollama)
                .decide(&mixed, None)
                .reason
                .contains("6-bit MLX models")
        );
    }

    #[test]
    fn another_format_only_goes_into_the_hugging_face_cache() {
        let t = traits(Format::Other);
        assert_eq!(
            states(&t),
            pairs(&[("bionic", "no"), ("ollama", "no"), ("hf", "ready")])
        );
        assert_eq!(
            profile(App::Ollama).decide(&t, None).reason,
            "Ollama cannot run this format"
        );
    }

    #[test]
    fn a_version_is_tested_untested_or_unknown() {
        let t = traits(Format::Gguf);
        let p = profile(App::Bionic);
        assert_eq!(p.decide(&t, Some("1.2.0+2")).version_state, "tested");
        assert_eq!(p.decide(&t, Some("9.9")).version_state, "untested");
        assert_eq!(p.decide(&t, Some("9.9")).state, "ready");
        assert_eq!(p.decide(&t, None).version_state, "unknown");
    }

    #[test]
    fn a_broken_version_skips_the_method() {
        let mut p = profile(App::Bionic).clone();
        p.methods[0].broken = vec!["2.0".into()];
        let d = p.decide(&traits(Format::Gguf), Some("2.0"));
        assert_eq!(d.state, "no");
        assert_eq!(d.version_state, "broken");
        assert_eq!(d.reason, "This does not work with Bionic 2.0");
    }

    #[test]
    fn the_widths_come_from_the_default_and_from_each_layer() {
        let plain =
            serde_json::json!({"quantization": {"group_size": 64, "bits": 4, "mode": "affine"}});
        assert_eq!(quantization_bits(&plain), [4]);
        let mixed = serde_json::json!({"quantization": {
            "group_size": 64, "bits": 4,
            "model.layers.0.mlp": {"group_size": 64, "bits": 8},
            "model.layers.1.mlp": {"group_size": 64, "bits": 4},
            "model.norm": false,
        }});
        assert_eq!(quantization_bits(&mixed), [4, 8]);
        assert!(quantization_bits(&serde_json::json!({"model_type": "qwen"})).is_empty());
    }

    #[test]
    fn the_installed_versions_are_read_from_the_bundle_and_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = AppDirs::under(dir.path());
        assert_eq!(Installed::detect(&dirs), Installed::unknown());
        let bundle = dirs.apps.join("Ollama.app/Contents");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(
            bundle.join("Info.plist"),
            "<plist><dict>\n<key>CFBundleName</key>\n<string>Ollama</string>\n\
             <key>CFBundleShortVersionString</key>\n\t<string>0.40.16</string>\n</dict></plist>",
        )
        .unwrap();
        std::fs::create_dir_all(&dirs.hf_cache).unwrap();
        std::fs::write(dirs.hf_cache.join("version.txt"), "1\n").unwrap();
        let found = Installed::detect(&dirs);
        assert_eq!(found.version(App::Ollama), Some("0.40.16"));
        assert_eq!(found.version(App::Hf), Some("1"));
        assert_eq!(found.version(App::Bionic), None);
    }
}
