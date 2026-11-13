//! An MLX model placed into a scratch Ollama folder as clones with a
//! rewritten index. What the index must say is asked of the Ollama that is
//! installed on this Mac, so the test is skipped where there is none. Its
//! models folder and its server are not used: the app folders are scratch
//! folders, and Ollama's import runs in its command line tool by itself.
#![cfg(target_os = "macos")]

mod common;

use common::{COMMIT, Ranged, engine, git_oid, sha256};
use dedupd_core::db::PROVIDER_HF;
use dedupd_core::place::App;
use dedupd_core::profile::Installed;
use dedupd_core::pull::{self, PullRequest};
use dedupd_core::refusals::Refusals;
use dedupd_core::safetensors::{self, Notes, Tensor};
use dedupd_core::select::Selection;
use dedupd_core::{Home, adopt, library, maintain, ollama_mlx};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const REPO: &str = "org/Tiny-MLX-4bit";
const OLLAMA: &str = "/Applications/Ollama.app/Contents/Resources/ollama";

/// The tensors of one quantized layer as MLX writes them: rows of packed
/// weights, and a scale and a bias per group.
fn layer(name: &str, bits: u64, seed: u8) -> Vec<(String, Tensor, Vec<u8>)> {
    // 16 rows of 64 weights in one group each; 32 bits to a word.
    let words = 64 * bits / 32;
    let part = |suffix: &str, dtype: &str, shape: &[u64], bytes: u64| {
        let data = (0..bytes).map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed));
        (
            format!("{name}.{suffix}"),
            Tensor {
                dtype: dtype.to_string(),
                shape: shape.to_vec(),
                data_offsets: [0, 0],
            },
            data.collect(),
        )
    };
    vec![
        part("weight", "U32", &[16, words], 16 * words * 4),
        part("scales", "F16", &[16, 1], 32),
        part("biases", "F16", &[16, 1], 32),
    ]
}

/// A weights file as bytes.
fn weights_file(dir: &Path, tensors: &[(String, Tensor, Vec<u8>)]) -> Vec<u8> {
    let path = dir.join("made.safetensors");
    let notes = Notes::from([("format".to_string(), "mlx".to_string())]);
    safetensors::write(&path, Some(&notes), tensors).unwrap();
    fs::read(path).unwrap()
}

struct Model {
    server: MockServer,
    /// Every tensor's bytes by the name MLX gave it.
    tensors: BTreeMap<String, Vec<u8>>,
    shards: Vec<Vec<u8>>,
    /// The sha256 of its `config.json`.
    config: String,
}

/// A fake Hub with a made-up MLX model of two weights files. The second
/// file mixes 4-bit and 8-bit layers.
async fn model(dir: &Path) -> Model {
    model_of(dir, "Qwen3_5ForConditionalGeneration", "qwen3_5").await
}

/// The same model, said to be of another architecture.
async fn model_of(dir: &Path, architecture: &str, model_type: &str) -> Model {
    let mut first = layer("model.layers.0.proj", 4, 1);
    first.push((
        "model.norm.weight".to_string(),
        Tensor {
            dtype: "F16".to_string(),
            shape: vec![16],
            data_offsets: [0, 0],
        },
        vec![3u8; 32],
    ));
    let mut second = layer("model.layers.1.proj", 4, 2);
    second.extend(layer("model.layers.1.mlp", 8, 5));
    let tensors = first
        .iter()
        .chain(&second)
        .map(|(name, _, data)| (name.clone(), data.clone()))
        .collect();
    let shards = vec![weights_file(dir, &first), weights_file(dir, &second)];

    let config = serde_json::to_vec_pretty(&json!({
        "architectures": [architecture],
        "model_type": model_type,
        "quantization": {
            "group_size": 64, "bits": 4, "mode": "affine",
            "model.layers.1.mlp": {"group_size": 64, "bits": 8},
        },
    }))
    .unwrap();
    let readme = b"# Tiny\n".to_vec();
    let small: [(&str, &Vec<u8>); 2] = [("config.json", &config), ("README.md", &readme)];
    let names = [
        "model-00001-of-00002.safetensors",
        "model-00002-of-00002.safetensors",
    ];

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/models/{REPO}/revision/main")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sha": COMMIT, "id": REPO, "library_name": "mlx", "tags": ["mlx", "safetensors"],
        })))
        .mount(&server)
        .await;
    let mut tree: Vec<Value> = small
        .iter()
        .map(|(name, body)| json!({"type": "file", "path": name, "size": body.len(), "oid": git_oid(body)}))
        .collect();
    for (name, body) in names.iter().zip(&shards) {
        tree.push(json!({
            "type": "file", "path": name, "size": body.len(), "oid": "pointer",
            "lfs": {"oid": sha256(body), "size": body.len(), "pointerSize": 134},
        }));
    }
    Mock::given(method("GET"))
        .and(path(format!("/api/models/{REPO}/tree/{COMMIT}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(tree))
        .mount(&server)
        .await;
    let files = small
        .iter()
        .map(|(name, body)| (*name, (*body).clone()))
        .chain(names.iter().copied().zip(shards.iter().cloned()));
    for (name, body) in files {
        Mock::given(method("GET"))
            .and(path(format!("/{REPO}/resolve/{COMMIT}/{name}")))
            .respond_with(Ranged(body))
            .mount(&server)
            .await;
    }
    Model {
        server,
        tensors,
        shards,
        config: sha256(&config),
    }
}

fn request(offline: bool) -> PullRequest {
    PullRequest {
        repo: REPO.into(),
        revision: "main".into(),
        selection: Selection {
            quant: None,
            include: vec![],
        },
        apps: vec![App::Ollama],
        offline,
        jobs: 2,
    }
}

/// The tensor layers a manifest names, as `(file name, path of the blob)`.
fn tensor_layers(ollama: &Path, manifest: &Value) -> Vec<(String, PathBuf)> {
    manifest["layers"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|l| l["mediaType"] == "application/vnd.ollama.image.tensor")
        .map(|l| {
            let digest = l["digest"].as_str().unwrap().replace(':', "-");
            (
                l["name"].as_str().unwrap().to_string(),
                ollama.join("blobs").join(digest),
            )
        })
        .collect()
}

#[tokio::test]
async fn an_mlx_model_is_placed_into_ollama_as_clones_with_a_rewritten_index() {
    if !Path::new(OLLAMA).is_file() {
        eprintln!("skipped: Ollama is not installed in /Applications");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let up = model(dir.path()).await;
    let home = Home::new(dir.path().join("home"));
    let mut e = engine(&home, &up.server.uri(), &dir.path().join("apps"), None);
    // The installed Ollama is asked; its own folders are not used.
    Arc::get_mut(&mut e).unwrap().dirs.apps = PathBuf::from("/Applications");
    let installed = Installed::detect(&e.dirs);
    assert!(installed.version(App::Ollama).is_some());

    let done = pull::pull(&e, &request(false), &Arc::default())
        .await
        .unwrap();
    assert_eq!(done.notes, Vec::<String>::new());
    let (app, placed) = &done.placed[0];
    assert_eq!(*app, App::Ollama);
    assert!(placed.conflicts.is_empty());

    // The manifest names three clones: one for the first weights file and,
    // for the second, one per way its layers are quantized.
    let manifest_path = e
        .dirs
        .ollama
        .join("manifests/hf.co")
        .join(REPO)
        .join("latest");
    let body = fs::read_to_string(&manifest_path).unwrap();
    let manifest: Value = serde_json::from_str(&body).unwrap();
    let layers = tensor_layers(&e.dirs.ollama, &manifest);
    let names: Vec<&str> = layers.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "model-00001-of-00002.safetensors",
            "model-00002-of-00002.safetensors",
            "model-00002-of-00002.safetensors"
        ]
    );
    assert_eq!(
        placed.cloned,
        manifest["layers"].as_array().unwrap().len() + 1
    );
    // Ollama's settings for the model came with its answer.
    let settings = e.dirs.ollama.join("blobs").join(
        manifest["config"]["digest"]
            .as_str()
            .unwrap()
            .replace(':', "-"),
    );
    let settings: Value = serde_json::from_slice(&fs::read(settings).unwrap()).unwrap();
    assert_eq!(settings["model_format"], "safetensors");

    // Each clone lists its tensors under Ollama's names, says how they are
    // quantized, and holds the bytes MLX wrote.
    let mut seen = Vec::new();
    let mut widths = Vec::new();
    for (i, (_, blob)) in layers.iter().enumerate() {
        let index = safetensors::read_index(blob).unwrap();
        let notes = index.notes.clone().unwrap();
        widths.push(notes["quant_type"].clone());
        assert_eq!(notes["group_size"], "64");
        for (name, tensor) in &index.tensors {
            assert!(!name.ends_with(".scales") && !name.ends_with(".biases"));
            let mlx_name = name
                .replace(".weight.scale", ".scales")
                .replace(".weight.bias", ".biases");
            assert_eq!(ollama_mlx::ollama_name(&mlx_name), *name);
            assert_eq!(
                safetensors::read_tensor(blob, &index, tensor).unwrap(),
                up.tensors[&mlx_name],
                "{name}"
            );
            seen.push(mlx_name);
        }
        // A clone is as long as its weights file, or a little longer where
        // the first tensors made room for the index.
        let source = up.shards[i.min(1)].len() as u64;
        let size = fs::metadata(blob).unwrap().len();
        assert!(
            size >= source && size < source + 4096,
            "{size} for {source}"
        );
    }
    assert_eq!(widths, ["int4", "int4", "int8"]);
    seen.sort();
    assert_eq!(seen, up.tensors.keys().cloned().collect::<Vec<_>>());

    // The library counts the model as in Ollama, and the records say the
    // clones were made from the stored weights files.
    let detail = library::model(&e.db(), &e.store, REPO, &installed)
        .unwrap()
        .unwrap();
    let variant = &detail.variants[0];
    assert_eq!(variant.placed_in, ["ollama"]);
    assert_eq!(variant.usable_by, ["bionic", "ollama", "hf"]);
    let ollama = variant
        .decisions
        .iter()
        .find(|d| d.app == "ollama")
        .unwrap();
    assert_eq!(ollama.method.as_deref(), Some(ollama_mlx::METHOD));
    let derived = e.db().derived_files().unwrap();
    assert_eq!(derived.len(), 3);
    let recorded: Vec<_> = e
        .db()
        .placements()
        .unwrap()
        .into_iter()
        .filter(|p| p.derived_sha256.is_some())
        .collect();
    for p in &recorded {
        assert!(p.shared);
        assert!(e.store.contains(&p.sha256));
        assert!(p.path.ends_with(p.derived_sha256.as_deref().unwrap()));
    }

    // Placing again changes nothing.
    let again = pull::pull(&e, &request(false), &Arc::default())
        .await
        .unwrap();
    assert_eq!(again.placed[0].1.cloned, 0);
    assert_eq!(again.placed[0].1.present, placed.cloned);

    // The background pass leaves the clones alone: they are not adopted as
    // new content, their manifest is not archived, and reclaim skips them.
    let stored = e.db().all_blobs().unwrap().len();
    let found = adopt::scan_apps(&e.db, &e.dirs, &[App::Ollama]).unwrap();
    for repo in &found {
        for file in &repo.files {
            let sha = file.rel_path.trim_start_matches("blobs/sha256-");
            assert!(!derived.contains(sha), "{} would be adopted", file.rel_path);
        }
    }
    let repo_id = e.db().repo_id(PROVIDER_HF, REPO).unwrap().unwrap();
    assert!(e.db().manifest(repo_id, "latest", None).unwrap().is_none());
    let swapped = maintain::reclaim(&e.db, &e.store, Duration::ZERO, false).unwrap();
    assert_eq!(swapped.swapped, 0);
    assert_eq!(e.db().all_blobs().unwrap().len(), stored);

    // Removing the model from Ollama takes the clones and the manifest;
    // the stored files stay.
    let plan = maintain::unplace_plan(&e.db, &e.store, &e.dirs, REPO, App::Ollama).unwrap();
    assert_eq!(plan.keep, Vec::<(String, String)>::new());
    assert_eq!(plan.remove.len(), placed.cloned);
    maintain::unplace_apply(&e.db, &e.dirs, &plan).unwrap();
    assert!(!manifest_path.exists());
    assert!(layers.iter().all(|(_, blob)| !blob.exists()));
    assert!(e.db().derived_files().unwrap().is_empty());
    for p in &recorded {
        assert!(e.store.contains(&p.sha256));
    }

    // Placed again, with Hugging Face out of reach, it is the same model
    // byte for byte.
    let offline = pull::pull(&e, &request(true), &Arc::default())
        .await
        .unwrap();
    assert_eq!(offline.notes, Vec::<String>::new());
    assert_eq!(fs::read_to_string(&manifest_path).unwrap(), body);
    assert!(layers.iter().all(|(_, blob)| blob.is_file()));
}

#[tokio::test]
async fn a_width_ollama_cannot_run_is_not_placed() {
    if !Path::new(OLLAMA).is_file() {
        eprintln!("skipped: Ollama is not installed in /Applications");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    let mut e = engine(&home, "http://127.0.0.1:9", &dir.path().join("apps"), None);
    Arc::get_mut(&mut e).unwrap().dirs.apps = PathBuf::from("/Applications");

    // A 5-bit model, put into the store by hand: the profile would refuse
    // it, so the placer is called directly to see what Ollama itself says.
    let weights = weights_file(dir.path(), &layer("model.layers.0.proj", 5, 1));
    let config = serde_json::to_vec(&json!({
        "architectures": ["Qwen3_5ForConditionalGeneration"],
        "model_type": "qwen3_5",
        "quantization": {"group_size": 64, "bits": 5, "mode": "affine"},
    }))
    .unwrap();
    let mut items = Vec::new();
    for (name, body) in [("config.json", &config), ("model.safetensors", &weights)] {
        let sha = sha256(body);
        let tmp = e.store.tmp_path(name);
        fs::write(&tmp, body).unwrap();
        e.store.add_tmp(&tmp, &sha).unwrap();
        e.db().upsert_blob(&sha, body.len() as u64, None).unwrap();
        items.push(dedupd_core::place::Item {
            rel_path: name.to_string(),
            etag: sha.clone(),
            sha256: sha,
            size: body.len() as u64,
        });
    }
    let repo_id = e.db().upsert_repo(PROVIDER_HF, REPO).unwrap();
    let dest = dedupd_core::place::Dest {
        db: &e.db,
        store: &e.store,
        dirs: &e.dirs,
        repo_id,
        repo: REPO,
    };
    let refused = ollama_mlx::place(&dest, Path::new(OLLAMA), "latest", &items)
        .unwrap()
        .unwrap_err();
    // Ollama's own answer, which dedupd keeps.
    assert_eq!(
        refused,
        ollama_mlx::NotPlaced::Ollama(
            "this Ollama has no name for 5-bit layers, so it cannot run this model".into()
        )
    );
    // Nothing was written into Ollama's folder, and nothing is left of the
    // asking.
    assert!(!e.dirs.ollama.join("manifests").exists());
    assert!(e.db().placements().unwrap().is_empty());
    let left: Vec<_> = fs::read_dir(home.tmp_dir()).unwrap().flatten().collect();
    assert!(left.is_empty(), "{left:?}");
}

#[tokio::test]
async fn an_architecture_ollama_refuses_is_remembered_and_not_offered_again() {
    if !Path::new(OLLAMA).is_file() {
        eprintln!("skipped: Ollama is not installed in /Applications");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let up = model_of(dir.path(), "NoSuchModelForCausalLM", "no_such_model").await;
    let home = Home::new(dir.path().join("home"));
    let mut e = engine(&home, &up.server.uri(), &dir.path().join("apps"), None);
    Arc::get_mut(&mut e).unwrap().dirs.apps = PathBuf::from("/Applications");
    let installed = Installed::detect(&e.dirs);
    let version = installed.version(App::Ollama).unwrap().to_string();

    // Ollama is asked, says no, and nothing is placed.
    let first = pull::pull(&e, &request(false), &Arc::default())
        .await
        .unwrap();
    assert!(first.placed.is_empty());
    assert_eq!(first.notes.len(), 1);
    assert!(
        first.notes[0].starts_with("not placed into Ollama: Ollama would not import a stand-in"),
        "{:?}",
        first.notes
    );
    assert!(!first.notes[0].contains("Error:"), "{:?}", first.notes);
    assert!(!e.dirs.ollama.join("manifests").exists());

    // Its answer is kept for this model's config.json and this version.
    let known = Refusals::load(&home);
    let kept = known
        .find("ollama", &version, ollama_mlx::METHOD, &up.config)
        .expect("the refusal is kept");
    assert!(kept.reason.starts_with("Ollama would not import"));

    // The library shows Ollama as unable to take it, with the reason.
    let library = library::model(&e.db(), &e.store, REPO, &installed)
        .unwrap()
        .unwrap();
    let ollama = library.variants[0]
        .decisions
        .iter()
        .find(|d| d.app == "ollama")
        .unwrap();
    assert_eq!(ollama.state, "no");
    assert!(
        ollama
            .reason
            .ends_with(&format!("(asked with Ollama {version})")),
        "{}",
        ollama.reason
    );
    assert!(
        !library.variants[0]
            .usable_by
            .contains(&"ollama".to_string())
    );

    // A second try is answered from what is kept, without asking Ollama.
    let second = pull::pull(&e, &request(false), &Arc::default())
        .await
        .unwrap();
    assert!(second.placed.is_empty());
    assert!(
        second.notes[0].ends_with(&format!("(asked with Ollama {version})")),
        "{:?}",
        second.notes
    );
}
