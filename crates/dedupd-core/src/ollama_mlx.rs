//! An MLX model for Ollama without a second copy of its weights.
//!
//! Ollama's MLX runner reads safetensors, as MLX tools do, but its own
//! import writes every tensor into a file of its own, which doubles the
//! model on disk. Its import changes no numbers, though: it renames two
//! tensors of each quantized layer and adds a note to the file saying how
//! the layer is quantized, and its loader accepts a file with many tensors.
//!
//! So each weights file is given to Ollama as a clone of the stored file
//! with only the index rewritten (`safetensors::rewritten_clone`). What the
//! index must say is not guessed: Ollama itself is asked, by having it
//! import a stand-in of a few hundred KB (the model's small files and one
//! small quantized layer). The answer gives the model's settings, the note,
//! and the names; when it shows that this Ollama cannot run the model, or
//! works differently from the one this was written against, nothing is
//! placed.
//!
//! The stand-in is imported by the installed Ollama's command line tool
//! into a scratch models folder, which is removed afterwards. The import
//! runs inside that tool; the owner's Ollama server and models folder are
//! not part of the asking.

use crate::adopt::Verify;
use crate::db::{NewPlacement, ORIGIN_PLACED, lock};
use crate::hash::hash_file;
use crate::ollama::{Manifest, digest_hex};
use crate::place::{App, Dest, Item, Placed, Target, ollama_manifest_path, place_clone};
use crate::safetensors::{self, Index, Notes, Tensor};
use crate::store::Fingerprint;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The method's name in the app's profile and in the database.
pub const METHOD: &str = "clone-with-rewritten-index";

/// Why a model was not placed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotPlaced {
    /// Ollama's answer when it was shown the stand-in. It holds for this
    /// model and this version of Ollama, so it is worth keeping.
    Ollama(String),
    /// Something about the model's files.
    Model(String),
}

impl std::fmt::Display for NotPlaced {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ollama(why) | Self::Model(why) => f.write_str(why),
        }
    }
}
/// The layer type Ollama gives a file of tensors.
const TENSOR: &str = "application/vnd.ollama.image.tensor";
const STAND_IN: &str = "dedupd-stand-in";

/// One way of quantizing a layer.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Way {
    pub bits: u64,
    pub group_size: u64,
    pub mode: String,
}

impl Way {
    fn of(entry: &Value) -> Option<Way> {
        Some(Way {
            bits: entry.get("bits")?.as_u64()?,
            group_size: entry.get("group_size")?.as_u64()?,
            mode: entry
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or("affine")
                .to_string(),
        })
    }

    fn as_config(&self) -> Value {
        json!({"group_size": self.group_size, "bits": self.bits, "mode": self.mode})
    }
}

impl std::fmt::Display for Way {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-bit", self.bits)
    }
}

/// How each layer of a model is quantized. MLX writes the model's default
/// into `config.json` and, for a model that mixes several ways, one entry
/// per layer that differs.
#[derive(Debug, Default)]
pub struct Ways {
    default: Option<Way>,
    per_layer: BTreeMap<String, Way>,
}

impl Ways {
    pub fn from_config(config: &Value) -> Self {
        let Some(q) = config.get("quantization").and_then(Value::as_object) else {
            return Self::default();
        };
        Self {
            default: Way::of(&Value::Object(q.clone())),
            per_layer: q
                .iter()
                .filter_map(|(layer, entry)| Some((layer.clone(), Way::of(entry)?)))
                .collect(),
        }
    }

    fn of(&self, layer: &str) -> Option<&Way> {
        self.per_layer
            .get(layer)
            .or_else(|| {
                self.per_layer
                    .iter()
                    .find(|(key, _)| layer.ends_with(&format!(".{key}")))
                    .map(|(_, way)| way)
            })
            .or(self.default.as_ref())
    }
}

/// The name Ollama gives a tensor MLX wrote.
pub fn ollama_name(name: &str) -> String {
    if let Some(layer) = name.strip_suffix(".scales") {
        format!("{layer}.weight.scale")
    } else if let Some(layer) = name.strip_suffix(".biases") {
        format!("{layer}.weight.bias")
    } else {
        name.to_string()
    }
}

/// The layer a tensor belongs to.
fn layer_of(name: &str) -> &str {
    [".weight", ".scales", ".biases"]
        .iter()
        .find_map(|suffix| name.strip_suffix(suffix))
        .unwrap_or(name)
}

/// The tensors one clone lists, and the way they are quantized: `None` when
/// none of them is.
type Group = (Option<Way>, BTreeSet<String>);

/// The tensors of one weights file, grouped by the note their clone needs.
///
/// The note is per file, so a file that holds layers quantized in more than
/// one way becomes one clone per way, each listing only its own tensors.
/// Tensors that are not quantized need no note; they ride along with the
/// largest group, so a model quantized in one way stays one clone per file.
fn groups(index: &Index, ways: &Ways) -> Result<Vec<Group>, String> {
    let quantized: BTreeSet<&str> = index
        .tensors
        .iter()
        .filter(|(name, _)| name.ends_with(".scales"))
        .map(|(name, _)| layer_of(name))
        .collect();
    let mut by_way: BTreeMap<Way, BTreeSet<String>> = BTreeMap::new();
    let mut plain = BTreeSet::new();
    for (name, _) in &index.tensors {
        let layer = layer_of(name);
        if !quantized.contains(layer) {
            plain.insert(name.clone());
            continue;
        }
        let way = ways.of(layer).ok_or_else(|| {
            format!("the model's config.json does not say how the layer {layer} is quantized")
        })?;
        by_way.entry(way.clone()).or_default().insert(name.clone());
    }
    let Some(largest) = by_way
        .iter()
        .max_by(|a, b| a.1.len().cmp(&b.1.len()).then(b.0.cmp(a.0)))
        .map(|(way, _)| way.clone())
    else {
        return Ok(vec![(None, plain)]);
    };
    by_way.entry(largest).or_default().extend(plain);
    Ok(by_way
        .into_iter()
        .map(|(way, names)| (Some(way), names))
        .collect())
}

/// One quantized layer to show Ollama: the file it is in and its tensors.
struct Sample<'a> {
    file: &'a Path,
    index: &'a Index,
    parts: Vec<(String, Tensor)>,
}

impl Sample<'_> {
    fn bytes(&self) -> u64 {
        self.parts.iter().map(|(_, t)| t.bytes()).sum()
    }
}

/// For each way the model is quantized, its smallest quantized layer.
fn smallest_layers<'a>(files: &'a [(PathBuf, Index)], ways: &Ways) -> BTreeMap<Way, Sample<'a>> {
    let mut chosen: BTreeMap<Way, Sample<'a>> = BTreeMap::new();
    for (file, index) in files {
        for (name, _) in &index.tensors {
            let Some(layer) = name.strip_suffix(".scales") else {
                continue;
            };
            let parts: Vec<(String, Tensor)> = [".weight", ".scales", ".biases"]
                .iter()
                .filter_map(|suffix| {
                    let name = format!("{layer}{suffix}");
                    index.tensor(&name).cloned().map(|t| (name, t))
                })
                .collect();
            let Some(way) = ways.of(layer).filter(|_| parts.len() == 3) else {
                continue;
            };
            let sample = Sample { file, index, parts };
            if chosen.get(way).is_none_or(|c| sample.bytes() < c.bytes()) {
                chosen.insert(way.clone(), sample);
            }
        }
    }
    chosen
}

/// What Ollama said about a model.
#[derive(Debug)]
struct Answer {
    /// The manifest it wrote for the stand-in, without the weights: the
    /// config layer (prompt renderer, parser, capabilities) and the model's
    /// small files.
    manifest: Value,
    /// The note it writes into a weights file, per way of quantizing.
    notes: BTreeMap<Way, Notes>,
}

/// The installed Ollama's command line tool with a scratch models folder
/// and a scratch home. The folder is removed when this is dropped.
///
/// Ollama's import of a safetensors model runs inside the command line
/// tool; no server takes part. To be sure the owner's server is never the
/// one asked, the tool is pointed at an address nothing listens on.
struct Scratch {
    command: PathBuf,
    dir: PathBuf,
    host: String,
}

impl Scratch {
    fn new(command: &Path, dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir.join("models"))?;
        fs::create_dir_all(dir.join("home"))?;
        // A port that is free: ask for any, note it, and give it back.
        let port = TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port();
        Ok(Self {
            command: command.to_path_buf(),
            dir: dir.to_path_buf(),
            host: format!("127.0.0.1:{port}"),
        })
    }

    fn models(&self) -> PathBuf {
        self.dir.join("models")
    }

    /// Have Ollama import the model folder `work` under `name`. `Err` in
    /// the inner result is Ollama's own refusal.
    fn import(&self, name: &str, work: &Path) -> Result<Result<Value, String>> {
        let modelfile = work.join("Modelfile");
        fs::write(&modelfile, format!("FROM {}\n", work.display()))?;
        let done = Command::new(&self.command)
            .args(["create", name, "-f"])
            .arg(&modelfile)
            .env("OLLAMA_HOST", &self.host)
            .env("OLLAMA_MODELS", self.models())
            .env("HOME", self.dir.join("home"))
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("running {}", self.command.display()))?;
        if !done.status.success() {
            let said = String::from_utf8_lossy(&done.stderr);
            let last = said.lines().last().unwrap_or("it gave no reason").trim();
            let last = last.strip_prefix("Error:").map_or(last, str::trim);
            return Ok(Err(format!(
                "Ollama would not import a stand-in for the model: {last}"
            )));
        }
        // Which tree and which host name the manifest goes under differs
        // between versions of Ollama.
        for tree in ["manifests", "manifests-v2"] {
            let hosts = fs::read_dir(self.models().join(tree));
            for host in hosts.into_iter().flatten().flatten() {
                let path = host.path().join("library").join(name).join("latest");
                if let Ok(body) = fs::read_to_string(path) {
                    return Ok(Ok(serde_json::from_str(&body)
                        .context("Ollama wrote a manifest that is not valid")?));
                }
            }
        }
        bail!("Ollama imported the stand-in, but wrote no manifest for it")
    }

    fn blob(&self, digest: &str) -> Result<PathBuf> {
        let hex = digest_hex(digest).with_context(|| format!("{digest} is not a sha256"))?;
        Ok(self.models().join("blobs").join(format!("sha256-{hex}")))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn layers_of(manifest: &Value) -> Vec<Value> {
    manifest
        .get("layers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn is_tensor(layer: &Value) -> bool {
    layer.get("mediaType").and_then(Value::as_str) == Some(TENSOR)
}

fn digest_of(layer: &Value) -> &str {
    layer
        .get("digest")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// The stand-in for one way: the model's small files, with `config.json`
/// saying that the whole model is quantized this way, and one layer.
///
/// Ollama's own import reads only the model's default and labels every
/// layer with it; asked way by way it answers correctly.
fn write_stand_in(
    work: &Path,
    small: &[(String, PathBuf)],
    way: &Way,
    sample: &Sample<'_>,
) -> Result<()> {
    fs::create_dir_all(work)?;
    for (name, source) in small {
        let target = work.join(name);
        if name == "config.json" {
            let mut config: Value = serde_json::from_slice(&fs::read(source)?)
                .context("the model's config.json is not valid")?;
            for key in ["quantization", "quantization_config"] {
                if let Some(entry) = config.get_mut(key) {
                    *entry = way.as_config();
                }
            }
            fs::write(&target, serde_json::to_vec_pretty(&config)?)?;
        } else {
            crate::clone::clone_file(source, &target)
                .with_context(|| format!("cloning {name} for the stand-in"))?;
        }
    }
    let mut tensors = Vec::with_capacity(sample.parts.len());
    for (name, tensor) in &sample.parts {
        let data = safetensors::read_tensor(sample.file, sample.index, tensor)?;
        tensors.push((name.clone(), tensor.clone(), data));
    }
    let notes = Notes::from([("format".to_string(), "mlx".to_string())]);
    safetensors::write(&work.join("model.safetensors"), Some(&notes), &tensors)
}

/// Ask Ollama what it would write for this model. The settings it gives
/// are moved into the store. `Err` in the inner result is the reason this
/// Ollama cannot be given the model.
fn ask_ollama(
    dest: &Dest<'_>,
    command: &Path,
    small: &[(String, PathBuf)],
    files: &[(PathBuf, Index)],
    ways: &Ways,
) -> Result<Result<Answer, NotPlaced>> {
    let samples = smallest_layers(files, ways);
    // The model's default way first: its answer carries the settings.
    let mut order: Vec<&Way> = samples.keys().collect();
    order.sort_by_key(|way| (Some(*way) != ways.default.as_ref(), (*way).clone()));
    if order.is_empty() {
        return Ok(Err(NotPlaced::Model(
            "the model has no quantized layer to show Ollama".into(),
        )));
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = dest
        .store
        .tmp_path(&format!("ollama-asked-{}-{now}", std::process::id()));
    let scratch = Scratch::new(command, &dir)?;
    let mut manifest = None;
    let mut notes = BTreeMap::new();
    for (i, way) in order.into_iter().enumerate() {
        let sample = &samples[way];
        let work = dir.join(format!("stand-in-{i}"));
        write_stand_in(&work, small, way, sample)?;
        let answer = match scratch.import(&format!("{STAND_IN}-{i}"), &work)? {
            Ok(answer) => answer,
            Err(why) => return Ok(Err(NotPlaced::Ollama(why))),
        };
        // What Ollama wrote for the layer: its names, and the note in the
        // file that holds the weight.
        let mut written: BTreeMap<String, Option<Notes>> = BTreeMap::new();
        for layer in layers_of(&answer).iter().filter(|l| is_tensor(l)) {
            let index = safetensors::read_index(&scratch.blob(digest_of(layer))?)?;
            for (name, _) in &index.tensors {
                written.insert(name.clone(), index.notes.clone());
            }
        }
        for (name, _) in &sample.parts {
            if !written.contains_key(&ollama_name(name)) {
                return Ok(Err(NotPlaced::Ollama(format!(
                    "this Ollama names tensors differently from the one dedupd was written against: \
                     it wrote no {}",
                    ollama_name(name)
                ))));
            }
        }
        let weight = ollama_name(&sample.parts[0].0);
        match written.remove(&weight).flatten() {
            Some(note) => notes.insert(way.clone(), note),
            None => {
                return Ok(Err(NotPlaced::Ollama(format!(
                    "this Ollama has no name for {way} layers, so it cannot run this model"
                ))));
            }
        };
        manifest.get_or_insert(answer);
    }
    let mut manifest = manifest.expect("at least one way was asked about");

    // The settings go into the store like any other file of the model.
    let kept: Vec<Value> = layers_of(&manifest)
        .into_iter()
        .filter(|l| !is_tensor(l))
        .collect();
    let config = manifest.get("config").cloned().unwrap_or_default();
    for layer in std::iter::once(&config).chain(&kept) {
        let digest = digest_of(layer);
        let path = scratch.blob(digest)?;
        let hashes = hash_file(&path, false)
            .with_context(|| format!("reading the layer {digest} Ollama wrote"))?;
        if Some(hashes.sha256.as_str()) != digest_hex(digest) {
            bail!("the layer {digest} Ollama wrote holds other bytes than its name says");
        }
        dest.store.add_tmp(&path, &hashes.sha256)?;
        lock(dest.db).upsert_blob(&hashes.sha256, hashes.size, None)?;
    }
    manifest["layers"] = Value::Array(kept);
    Ok(Ok(Answer { manifest, notes }))
}

/// Whether the model is in Ollama already, as dedupd placed it.
enum Standing {
    /// Every layer the manifest names is there and as it was recorded.
    Placed(usize),
    /// A manifest is there that dedupd did not write.
    Foreign,
    /// Nothing is there, or what is there is ours and no longer whole.
    Missing,
}

fn standing(dest: &Dest<'_>, manifest_path: &Path, sources: &BTreeSet<&str>) -> Result<Standing> {
    let Ok(body) = fs::read_to_string(manifest_path) else {
        return Ok(Standing::Missing);
    };
    let Ok(manifest) = Manifest::parse(&body) else {
        return Ok(Standing::Foreign);
    };
    let db = lock(dest.db);
    let derived = db.derived_files()?;
    let ours = manifest
        .all()
        .any(|l| l.sha256().is_ok_and(|sha| derived.contains(sha)));
    if !ours {
        return Ok(Standing::Foreign);
    }
    let mut from: BTreeSet<String> = BTreeSet::new();
    let mut layers = 0;
    for layer in manifest.all() {
        let sha = layer.sha256()?;
        let path = dest.dirs.ollama.join(format!("blobs/sha256-{sha}"));
        let Some(known) = db.placement(&path.to_string_lossy())? else {
            return Ok(Standing::Missing);
        };
        if Fingerprint::of(&path).ok() != Some(known.fingerprint()) {
            return Ok(Standing::Missing);
        }
        if derived.contains(sha) {
            from.insert(known.sha256);
        }
        layers += 1;
    }
    // The clones must be made from the weights files being placed now.
    let same = from.iter().map(String::as_str).eq(sources.iter().copied());
    Ok(if same {
        Standing::Placed(layers)
    } else {
        Standing::Missing
    })
}

/// Put an MLX model into Ollama: the settings Ollama gives for it as
/// clones of stored files, each weights file as a clone with a rewritten
/// index, and last the manifest that names them.
///
/// `Err` in the inner result is the reason the model was not placed; the
/// outer error is a failure.
pub fn place(
    dest: &Dest<'_>,
    command: &Path,
    tag: &str,
    items: &[Item],
) -> Result<Result<Placed, NotPlaced>> {
    let mut out = Placed::default();
    // Ollama imports the files at the top of a model folder.
    let top: Vec<&Item> = items.iter().filter(|i| !i.rel_path.contains('/')).collect();
    for item in &top {
        if !dest.store.ensure_local(&item.sha256)? {
            bail!("{} is not in the store", item.rel_path);
        }
    }
    let path_of = |item: &Item| dest.store.blob_path(&item.sha256);
    let mut weights: Vec<&Item> = top
        .iter()
        .copied()
        .filter(|i| i.rel_path.ends_with(".safetensors"))
        .collect();
    weights.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    if weights.is_empty() {
        return Ok(Err(NotPlaced::Model(
            "the model has no safetensors weights files".into(),
        )));
    }
    let small: Vec<(String, PathBuf)> = top
        .iter()
        .filter(|i| !i.rel_path.ends_with(".safetensors"))
        // The list of which file holds which tensor would not match the
        // stand-in's one file.
        .filter(|i| i.rel_path != "model.safetensors.index.json")
        .map(|i| (i.rel_path.clone(), path_of(i)))
        .collect();
    let Some((_, config)) = small.iter().find(|(name, _)| name == "config.json") else {
        return Ok(Err(NotPlaced::Model("the model has no config.json".into())));
    };
    let config: Value = serde_json::from_slice(&fs::read(config)?)
        .context("the model's config.json is not valid")?;
    let ways = Ways::from_config(&config);

    let manifest_path = ollama_manifest_path(dest.dirs, dest.repo, tag);
    let sources: BTreeSet<&str> = weights.iter().map(|i| i.sha256.as_str()).collect();
    match standing(dest, &manifest_path, &sources)? {
        Standing::Placed(layers) => {
            out.present = layers;
            return Ok(Ok(out));
        }
        Standing::Foreign => {
            out.conflicts.push(format!(
                "{} exists and was not written by dedupd",
                manifest_path.display()
            ));
            return Ok(Ok(out));
        }
        Standing::Missing => {}
    }

    let mut files = Vec::with_capacity(weights.len());
    for item in &weights {
        let path = path_of(item);
        let index = safetensors::read_index(&path)
            .with_context(|| format!("reading the index of {}", item.rel_path))?;
        files.push((path, index));
    }
    // Settle how every file is split before anything is written.
    let mut split = Vec::with_capacity(files.len());
    for (_, index) in &files {
        match groups(index, &ways) {
            Ok(groups) => split.push(groups),
            Err(why) => return Ok(Err(NotPlaced::Model(why))),
        }
    }
    let answer = match ask_ollama(dest, command, &small, &files, &ways)? {
        Ok(answer) => answer,
        Err(why) => return Ok(Err(why)),
    };

    // The settings: stored files, placed as clones like any other layer.
    let kept = layers_of(&answer.manifest);
    let config_layer = answer.manifest.get("config").cloned().unwrap_or_default();
    for layer in std::iter::once(&config_layer).chain(&kept) {
        let sha = digest_hex(digest_of(layer)).context("a layer Ollama wrote has no sha256")?;
        let size = lock(dest.db)
            .blob(sha)?
            .with_context(|| format!("the layer {sha} is not in the store"))?
            .size;
        let rel = format!("blobs/sha256-{sha}");
        let target = Target {
            app: App::Ollama,
            rel_path: &rel,
            sha256: sha,
            size,
            commit: None,
        };
        place_clone(dest, &target, &dest.dirs.ollama.join(&rel), &mut out)?;
    }
    if !out.conflicts.is_empty() {
        return Ok(Ok(out));
    }

    // The weights: one clone per file and way of quantizing.
    let blobs = dest.dirs.ollama.join("blobs");
    let mut layers = Vec::new();
    for ((item, (source, _)), groups) in weights.iter().zip(&files).zip(split) {
        if !crate::clone::same_volume(&blobs, source)? {
            bail!(
                "{} is on a different volume from the store; clones need one APFS volume",
                blobs.display()
            );
        }
        for (n, (way, wanted)) in groups.into_iter().enumerate() {
            let note = match &way {
                Some(way) => Some(
                    answer
                        .notes
                        .get(way)
                        .with_context(|| format!("Ollama was not asked about {way} layers"))?,
                ),
                None => None,
            };
            let work = blobs.join(format!(".dedupd-{}-{n}", item.sha256));
            safetensors::rewritten_clone(source, &work, note, &wanted, &ollama_name)
                .with_context(|| format!("making Ollama's clone of {}", item.rel_path))?;
            let hashes = hash_file(&work, false)?;
            let rel = format!("blobs/sha256-{}", hashes.sha256);
            let placed = dest.dirs.ollama.join(&rel);
            if placed.is_file() {
                fs::remove_file(&work)?;
                out.present += 1;
            } else {
                fs::rename(&work, &placed)?;
                out.cloned += 1;
            }
            let fingerprint = Fingerprint::of(&placed)?;
            lock(dest.db).upsert_placement(&NewPlacement {
                app: App::Ollama.as_str(),
                path: &placed.to_string_lossy(),
                repo_id: dest.repo_id,
                rel_path: &rel,
                sha256: &item.sha256,
                size: fingerprint.size,
                mtime_ns: fingerprint.mtime_ns,
                origin: ORIGIN_PLACED,
                verify_state: Verify::Unchecked.as_str(),
                verified_commit: None,
                shared: true,
                derived: Some((METHOD, &hashes.sha256)),
            })?;
            layers.push(json!({
                "mediaType": TENSOR,
                "digest": format!("sha256:{}", hashes.sha256),
                "size": hashes.size,
                "name": item.rel_path,
            }));
        }
    }

    // The manifest last, so Ollama never sees a model whose layers are
    // missing.
    let mut manifest = answer.manifest;
    layers.extend(kept);
    manifest["layers"] = Value::Array(layers);
    let parent = manifest_path.parent().expect("manifest has a parent");
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".{tag}.dedupd-tmp"));
    fs::write(&tmp, serde_json::to_string(&manifest)?)?;
    fs::rename(&tmp, &manifest_path)?;
    Ok(Ok(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tensor(at: u64, bytes: u64) -> Tensor {
        Tensor {
            dtype: "U32".to_string(),
            shape: vec![bytes / 4],
            data_offsets: [at, at + bytes],
        }
    }

    fn index(names: &[(&str, u64)]) -> Index {
        let mut at = 0;
        let tensors = names
            .iter()
            .map(|(name, bytes)| {
                let t = tensor(at, *bytes);
                at += bytes;
                (name.to_string(), t)
            })
            .collect();
        Index {
            length: 0,
            notes: None,
            tensors,
        }
    }

    fn way(bits: u64) -> Way {
        Way {
            bits,
            group_size: 64,
            mode: "affine".to_string(),
        }
    }

    #[test]
    fn ollama_renames_the_scales_and_biases_of_a_layer() {
        assert_eq!(ollama_name("a.b.scales"), "a.b.weight.scale");
        assert_eq!(ollama_name("a.b.biases"), "a.b.weight.bias");
        assert_eq!(ollama_name("a.b.weight"), "a.b.weight");
        assert_eq!(ollama_name("a.b.bias"), "a.b.bias");
        assert_eq!(layer_of("a.b.scales"), "a.b");
        assert_eq!(layer_of("a.norm"), "a.norm");
    }

    #[test]
    fn the_ways_come_from_the_default_and_from_each_layer() {
        let ways = Ways::from_config(&json!({"quantization": {
            "group_size": 64, "bits": 4, "mode": "affine",
            "model.layers.0.mlp": {"group_size": 32, "bits": 8},
            "model.norm": false,
        }}));
        assert_eq!(ways.of("model.layers.1.attn"), Some(&way(4)));
        let eight = Way {
            group_size: 32,
            ..way(8)
        };
        assert_eq!(ways.of("model.layers.0.mlp"), Some(&eight));
        // A key can name the end of a layer's path.
        assert_eq!(ways.of("language_model.model.layers.0.mlp"), Some(&eight));
        assert_eq!(Ways::from_config(&json!({"model_type": "x"})).of("a"), None);
    }

    #[test]
    fn a_file_quantized_one_way_is_one_group_with_everything_in_it() {
        let ways = Ways::from_config(&json!({"quantization": {"group_size": 64, "bits": 4}}));
        let index = index(&[
            ("a.weight", 64),
            ("a.scales", 8),
            ("a.biases", 8),
            ("norm.weight", 16),
        ]);
        let groups = groups(&index, &ways).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, Some(way(4)));
        assert_eq!(groups[0].1.len(), 4);
    }

    #[test]
    fn a_file_quantized_two_ways_is_one_group_per_way() {
        let ways = Ways::from_config(&json!({"quantization": {
            "group_size": 64, "bits": 4,
            "b": {"group_size": 64, "bits": 8},
        }}));
        let index = index(&[
            ("a.weight", 64),
            ("a.scales", 8),
            ("a.biases", 8),
            ("c.weight", 64),
            ("c.scales", 8),
            ("c.biases", 8),
            ("b.weight", 64),
            ("b.scales", 8),
            ("b.biases", 8),
            ("norm.weight", 16),
        ]);
        let groups = groups(&index, &ways).unwrap();
        assert_eq!(groups.len(), 2);
        // The tensor that is not quantized rides with the larger group.
        assert_eq!(groups[0].0, Some(way(4)));
        assert_eq!(groups[0].1.len(), 7);
        assert!(groups[0].1.contains("norm.weight"));
        assert_eq!(groups[1].0, Some(way(8)));
        assert_eq!(groups[1].1.len(), 3);
    }

    #[test]
    fn a_quantized_layer_the_config_says_nothing_about_is_refused() {
        let index = index(&[("a.weight", 64), ("a.scales", 8), ("a.biases", 8)]);
        let why = groups(&index, &Ways::default()).unwrap_err();
        assert!(why.contains("does not say how the layer a is quantized"));
        // A file with no quantized layer needs no note.
        let plain = index_of_plain();
        assert_eq!(groups(&plain, &Ways::default()).unwrap()[0].0, None);
    }

    fn index_of_plain() -> Index {
        index(&[("norm.weight", 16), ("norm.bias", 16)])
    }

    #[test]
    fn the_smallest_layer_of_each_way_is_shown_to_ollama() {
        let ways = Ways::from_config(&json!({"quantization": {
            "group_size": 64, "bits": 4,
            "b": {"group_size": 64, "bits": 8},
        }}));
        let files = vec![
            (
                PathBuf::from("one"),
                index(&[
                    ("a.weight", 640),
                    ("a.scales", 8),
                    ("a.biases", 8),
                    // No biases: not a layer that can stand in.
                    ("d.weight", 4),
                    ("d.scales", 4),
                ]),
            ),
            (
                PathBuf::from("two"),
                index(&[
                    ("c.weight", 64),
                    ("c.scales", 8),
                    ("c.biases", 8),
                    ("b.weight", 64),
                    ("b.scales", 8),
                    ("b.biases", 8),
                ]),
            ),
        ];
        let samples = smallest_layers(&files, &ways);
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[&way(4)].file, Path::new("two"));
        assert_eq!(samples[&way(4)].parts[0].0, "c.weight");
        assert_eq!(samples[&way(8)].parts[1].0, "b.scales");
    }
}
