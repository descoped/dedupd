//! The index of a safetensors file, and a clone of such a file in which only
//! the index is rewritten.
//!
//! A safetensors file is eight bytes giving the length of its index, the
//! index (JSON: each tensor's name, type, shape and where its numbers are),
//! and then the numbers. An app that wants other names for the same numbers
//! can be given a clone whose index says what it expects: the numbers stay
//! where they are and stay shared on disk with the stored file.

use crate::clone::clone_into_place;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// The key of the index that holds the file's own notes.
const METADATA: &str = "__metadata__";
/// No real index is near this long; a longer one means the file is not
/// safetensors.
const INDEX_MAX: u64 = 256 << 20;

/// Where one tensor is: `data_offsets` count from the end of the index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tensor {
    pub dtype: String,
    pub shape: Vec<u64>,
    pub data_offsets: [u64; 2],
}

impl Tensor {
    pub fn bytes(&self) -> u64 {
        self.data_offsets[1] - self.data_offsets[0]
    }
}

/// The notes a file carries about itself: text to text.
pub type Notes = BTreeMap<String, String>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    /// The length of the index in the file, padding included.
    pub length: u64,
    pub notes: Option<Notes>,
    /// In the order of their numbers in the file.
    pub tensors: Vec<(String, Tensor)>,
}

impl Index {
    /// Where the numbers start in the file.
    pub fn data_start(&self) -> u64 {
        8 + self.length
    }

    pub fn tensor(&self, name: &str) -> Option<&Tensor> {
        self.tensors.iter().find(|(n, _)| n == name).map(|(_, t)| t)
    }

    /// The end of the last tensor, counted like `data_offsets`.
    fn data_length(&self) -> u64 {
        self.tensors
            .iter()
            .map(|(_, t)| t.data_offsets[1])
            .max()
            .unwrap_or(0)
    }
}

pub fn read_index(path: &Path) -> Result<Index> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let size = file.metadata()?.len();
    let mut length = [0u8; 8];
    file.read_exact(&mut length)
        .context("the file is too short to be safetensors")?;
    let length = u64::from_le_bytes(length);
    ensure!(
        length <= INDEX_MAX && 8 + length <= size,
        "{} is not a safetensors file",
        path.display()
    );
    let mut raw = vec![0u8; length as usize];
    file.read_exact(&mut raw)?;
    let mut entries: BTreeMap<String, serde_json::Value> = serde_json::from_slice(&raw)
        .with_context(|| format!("the index of {} is not valid", path.display()))?;
    let notes = match entries.remove(METADATA) {
        Some(notes) => {
            Some(serde_json::from_value(notes).context("the file's notes are not text")?)
        }
        None => None,
    };
    let mut tensors = Vec::with_capacity(entries.len());
    for (name, value) in entries {
        let tensor: Tensor = serde_json::from_value(value)
            .with_context(|| format!("the index entry for {name} is not valid"))?;
        ensure!(
            tensor.data_offsets[0] <= tensor.data_offsets[1]
                && 8 + length + tensor.data_offsets[1] <= size,
            "{name} lies outside {}",
            path.display()
        );
        tensors.push((name, tensor));
    }
    tensors.sort_by(|a, b| a.1.data_offsets.cmp(&b.1.data_offsets).then(a.0.cmp(&b.0)));
    Ok(Index {
        length,
        notes,
        tensors,
    })
}

/// An index as it is written: the notes first, then the tensors in the order
/// given. The same input always gives the same bytes.
fn index_json(notes: Option<&Notes>, tensors: &[(String, Tensor)]) -> Result<Vec<u8>> {
    let mut parts = Vec::with_capacity(tensors.len() + 1);
    if let Some(notes) = notes {
        parts.push(format!("\"{METADATA}\":{}", serde_json::to_string(notes)?));
    }
    for (name, tensor) in tensors {
        parts.push(format!(
            "{}:{}",
            serde_json::to_string(name)?,
            serde_json::to_string(tensor)?
        ));
    }
    Ok(format!("{{{}}}", parts.join(",")).into_bytes())
}

/// Write a safetensors file from tensors held in memory.
pub fn write(
    path: &Path,
    notes: Option<&Notes>,
    tensors: &[(String, Tensor, Vec<u8>)],
) -> Result<()> {
    let mut at = 0;
    let mut listed = Vec::with_capacity(tensors.len());
    for (name, tensor, data) in tensors {
        let end = at + data.len() as u64;
        listed.push((
            name.clone(),
            Tensor {
                data_offsets: [at, end],
                ..tensor.clone()
            },
        ));
        at = end;
    }
    let index = index_json(notes, &listed)?;
    let mut file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    file.write_all(&(index.len() as u64).to_le_bytes())?;
    file.write_all(&index)?;
    for (_, _, data) in tensors {
        file.write_all(data)?;
    }
    Ok(())
}

/// Read one tensor's numbers.
pub fn read_tensor(path: &Path, index: &Index, tensor: &Tensor) -> Result<Vec<u8>> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(index.data_start() + tensor.data_offsets[0]))?;
    let mut data = vec![0u8; tensor.bytes() as usize];
    file.read_exact(&mut data)?;
    Ok(data)
}

/// What a rewritten clone will hold.
#[derive(Debug)]
struct Plan {
    /// The new index, padded with spaces to the room it has.
    index: Vec<u8>,
    /// How many bytes of numbers at the start give way to the longer index.
    room: u64,
    /// How many tensors lie in that room; they go to the end of the file.
    moved: usize,
}

/// The new index for a clone that lists `wanted` under new names.
///
/// The index must fit where the old one was, or every tensor after it would
/// have to move and nothing would stay shared. Where it is longer, the first
/// tensors make room: they are written again at the end of the file, and all
/// the others stay where they were.
fn plan(
    old: &Index,
    notes: Option<&Notes>,
    wanted: &BTreeSet<String>,
    rename: &dyn Fn(&str) -> String,
) -> Result<Plan> {
    let data_length = old.data_length();
    for moved in 0..=old.tensors.len() {
        let room = match moved {
            0 => 0,
            n => old.tensors[n - 1].1.data_offsets[1],
        };
        let listed: Vec<(String, Tensor)> = old
            .tensors
            .iter()
            .enumerate()
            .filter(|(_, (name, _))| wanted.contains(name))
            .map(|(i, (name, tensor))| {
                let [a, b] = tensor.data_offsets;
                let data_offsets = if i < moved {
                    [data_length - room + a, data_length - room + b]
                } else {
                    [a - room, b - room]
                };
                (
                    rename(name),
                    Tensor {
                        data_offsets,
                        ..tensor.clone()
                    },
                )
            })
            .collect();
        let mut index = index_json(notes, &listed)?;
        let fits = old.length + room;
        if index.len() as u64 <= fits {
            index.resize(fits as usize, b' ');
            return Ok(Plan { index, room, moved });
        }
    }
    bail!("the new index is longer than the whole file")
}

/// Put a clone of the safetensors file `src` at `dst` whose index lists only
/// the tensors in `wanted`, under the names `rename` gives them, with
/// `notes` as the file's notes. Every listed tensor holds the bytes it held
/// in `src`; this is checked before `dst` appears.
pub fn rewritten_clone(
    src: &Path,
    dst: &Path,
    notes: Option<&Notes>,
    wanted: &BTreeSet<String>,
    rename: &dyn Fn(&str) -> String,
) -> Result<()> {
    let old = read_index(src)?;
    let size = std::fs::metadata(src)?.len();
    // The plan counts on the numbers lying back to back from the end of the
    // index to the end of the file, as every writer of the format leaves them.
    let mut at = 0;
    for (name, tensor) in &old.tensors {
        ensure!(
            tensor.data_offsets[0] == at,
            "{name} does not follow the tensor before it in {}",
            src.display()
        );
        at = tensor.data_offsets[1];
    }
    ensure!(
        old.data_start() + at == size,
        "{} holds more than its index lists",
        src.display()
    );
    let plan = plan(&old, notes, wanted, rename)?;

    clone_into_place(src, dst, |tmp| {
        // Store files are read-only; this one is the app's own.
        std::fs::set_permissions(tmp, std::fs::Permissions::from_mode(0o644))?;
        let mut file = File::options().read(true).write(true).open(tmp)?;
        let mut head = vec![0u8; plan.room as usize];
        file.seek(SeekFrom::Start(old.data_start()))?;
        file.read_exact(&mut head)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&(plan.index.len() as u64).to_le_bytes())?;
        file.write_all(&plan.index)?;
        let moved_wanted = old.tensors[..plan.moved]
            .iter()
            .any(|(name, _)| wanted.contains(name));
        if moved_wanted {
            file.seek(SeekFrom::End(0))?;
            file.write_all(&head)?;
        }
        file.sync_all()?;
        drop(file);
        check(src, &old, tmp, &plan, wanted, rename)
    })
}

/// Every listed tensor must hold the bytes it held in the original: one
/// that stayed lies at the same place in the file, one that moved is read
/// and compared.
fn check(
    src: &Path,
    old: &Index,
    made: &Path,
    plan: &Plan,
    wanted: &BTreeSet<String>,
    rename: &dyn Fn(&str) -> String,
) -> Result<()> {
    let new = read_index(made)?;
    ensure!(
        new.tensors.len() == wanted.len(),
        "the clone lists {} tensors, not {}",
        new.tensors.len(),
        wanted.len()
    );
    for (i, (name, was)) in old.tensors.iter().enumerate() {
        if !wanted.contains(name) {
            continue;
        }
        let is = new
            .tensor(&rename(name))
            .with_context(|| format!("{name} is missing from the clone"))?;
        ensure!(
            is.dtype == was.dtype && is.shape == was.shape && is.bytes() == was.bytes(),
            "{name} changed in the clone"
        );
        if i >= plan.moved {
            ensure!(
                new.data_start() + is.data_offsets[0] == old.data_start() + was.data_offsets[0],
                "{name} moved in the clone"
            );
        } else {
            ensure!(
                read_tensor(src, old, was)? == read_tensor(made, &new, is)?,
                "{name} holds other bytes in the clone"
            );
        }
    }
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    fn tensor(dtype: &str, shape: &[u64]) -> Tensor {
        Tensor {
            dtype: dtype.to_string(),
            shape: shape.to_vec(),
            data_offsets: [0, 0],
        }
    }

    /// A file with three tensors of 64 KiB, 16 bytes and 32 bytes.
    fn sample(path: &Path) -> Vec<(String, Tensor, Vec<u8>)> {
        let tensors = vec![
            (
                "layer.weight".to_string(),
                tensor("U32", &[128, 128]),
                (0..65536u32).map(|i| (i % 251) as u8).collect::<Vec<u8>>(),
            ),
            (
                "layer.scales".to_string(),
                tensor("F16", &[8]),
                vec![7u8; 16],
            ),
            (
                "layer.biases".to_string(),
                tensor("F16", &[16]),
                vec![9u8; 32],
            ),
        ];
        let notes = Notes::from([("format".to_string(), "mlx".to_string())]);
        write(path, Some(&notes), &tensors).unwrap();
        tensors
    }

    fn all(tensors: &[(String, Tensor, Vec<u8>)]) -> BTreeSet<String> {
        tensors.iter().map(|(n, _, _)| n.clone()).collect()
    }

    fn renamed(name: &str) -> String {
        name.replace(".scales", ".weight.scale")
    }

    #[test]
    fn an_index_is_read_back_as_it_was_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.safetensors");
        let tensors = sample(&path);
        let index = read_index(&path).unwrap();
        assert_eq!(index.notes.as_ref().unwrap()["format"], "mlx");
        let names: Vec<&str> = index.tensors.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["layer.weight", "layer.scales", "layer.biases"]);
        assert_eq!(index.tensors[1].1.data_offsets, [65536, 65552]);
        for (name, _, data) in &tensors {
            let t = index.tensor(name).unwrap();
            assert_eq!(&read_tensor(&path, &index, t).unwrap(), data);
        }
        assert!(read_index(&dir.path().join("missing")).is_err());
        std::fs::write(dir.path().join("short"), b"abc").unwrap();
        assert!(read_index(&dir.path().join("short")).is_err());
    }

    #[test]
    fn a_shorter_index_leaves_every_tensor_where_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let (src, dst) = (dir.path().join("src"), dir.path().join("dst"));
        let tensors = sample(&src);
        // Without the notes the new index is shorter than the old one.
        rewritten_clone(&src, &dst, None, &all(&tensors), &|n| n.to_string()).unwrap();

        let (old, new) = (read_index(&src).unwrap(), read_index(&dst).unwrap());
        assert_eq!(new.length, old.length);
        assert_eq!(new.notes, None);
        assert_eq!(new.tensors, old.tensors);
        assert_eq!(
            std::fs::metadata(&dst).unwrap().len(),
            std::fs::metadata(&src).unwrap().len()
        );
        // The original is untouched and still read-only where it was.
        assert_eq!(read_index(&src).unwrap(), old);
    }

    #[test]
    fn a_longer_index_moves_the_first_tensors_to_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let (src, dst) = (dir.path().join("src"), dir.path().join("dst"));
        let tensors = sample(&src);
        let notes = Notes::from([
            ("quant_type".to_string(), "int4".to_string()),
            ("group_size".to_string(), "64".to_string()),
        ]);
        rewritten_clone(&src, &dst, Some(&notes), &all(&tensors), &renamed).unwrap();

        let (old, new) = (read_index(&src).unwrap(), read_index(&dst).unwrap());
        assert_eq!(new.notes.as_ref(), Some(&notes));
        // The first tensor gave way: the index grew by its length, and the
        // file by the same.
        assert_eq!(new.length, old.length + 65536);
        assert_eq!(
            std::fs::metadata(&dst).unwrap().len(),
            std::fs::metadata(&src).unwrap().len() + 65536
        );
        for (name, _, data) in &tensors {
            let t = new.tensor(&renamed(name)).unwrap();
            assert_eq!(&read_tensor(&dst, &new, t).unwrap(), data, "{name}");
        }
        assert!(new.tensor("layer.scales").is_none());
        // The two that stayed are at the same place in the file.
        let (was, is) = (
            old.tensor("layer.biases").unwrap(),
            new.tensor("layer.biases").unwrap(),
        );
        assert_eq!(
            old.data_start() + was.data_offsets[0],
            new.data_start() + is.data_offsets[0]
        );
    }

    #[test]
    fn a_clone_can_list_some_of_the_tensors_only() {
        let dir = tempfile::tempdir().unwrap();
        let (src, dst) = (dir.path().join("src"), dir.path().join("dst"));
        let tensors = sample(&src);
        let wanted = BTreeSet::from(["layer.biases".to_string()]);
        let notes = Notes::from([("quant_type".to_string(), "int8".to_string())]);
        rewritten_clone(&src, &dst, Some(&notes), &wanted, &renamed).unwrap();

        let new = read_index(&dst).unwrap();
        assert_eq!(new.tensors.len(), 1);
        let t = new.tensor("layer.biases").unwrap();
        assert_eq!(read_tensor(&dst, &new, t).unwrap(), tensors[2].2);
        // Nothing was listed that had to move, so the file is no longer.
        assert_eq!(
            std::fs::metadata(&dst).unwrap().len(),
            std::fs::metadata(&src).unwrap().len()
        );
    }

    #[test]
    fn the_same_input_gives_the_same_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let tensors = sample(&src);
        let notes = Notes::from([("quant_type".to_string(), "int4".to_string())]);
        let make = |name: &str| {
            let dst = dir.path().join(name);
            rewritten_clone(&src, &dst, Some(&notes), &all(&tensors), &renamed).unwrap();
            std::fs::read(dst).unwrap()
        };
        assert_eq!(make("one"), make("two"));
    }

    #[test]
    fn a_file_with_a_gap_or_a_tail_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (src, dst) = (dir.path().join("src"), dir.path().join("dst"));
        let tensors = sample(&src);
        let mut bytes = std::fs::read(&src).unwrap();
        bytes.extend_from_slice(b"tail");
        std::fs::write(&src, bytes).unwrap();
        let refused = rewritten_clone(&src, &dst, None, &all(&tensors), &|n| n.to_string());
        assert!(
            refused
                .unwrap_err()
                .to_string()
                .contains("more than its index")
        );
        assert!(!dst.exists());
    }
}
