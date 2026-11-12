#!/usr/bin/env python3
"""Give Ollama an MLX model without a second copy of its weights.

Ollama's MLX runner reads safetensors, as MLX tools do, but its own import
(`ollama create`) writes every tensor into a file of its own, which doubles
the model on disk. This script shows that the doubling is not needed.

What Ollama's import changes, measured on 0.40.16:
  - the numbers: nothing. Every tensor's bytes, dtype and shape are the same;
  - the names of two tensors of each quantized layer:
        X.scales -> X.weight.scale        X.biases -> X.weight.bias
  - a note in the file's metadata saying how the layer is quantized, such as
        {"quant_type": "int4", "group_size": "64"}

And its loader accepts a file that holds many tensors. So the script:

  1. asks Ollama itself what it would write. It makes a stand-in for the
     model: the model's small files and one small quantized layer (a few
     hundred KB), and has Ollama import that. The import gives the model's
     settings (prompt renderer, parser, capabilities), the note for that way
     of quantizing, and the names Ollama uses. The script stops when the
     answer shows that this Ollama cannot take the model as it is:
       - no note for a quantized layer: Ollama does not know that bit width.
         On 0.40.16 that is every width but 4 and 8; its own import of a
         5-bit model succeeds and the model then fails to load;
       - other names than the ones above: this Ollama works differently
         from the one this was written against.
     A model that mixes several ways gets one stand-in per way, each saying
     in its config.json that the whole model is quantized that way. Ollama's
     own import reads only the model's default, labels every layer with it,
     and the model then fails to load; asked way by way it answers correctly.
  2. for each weights file, makes an APFS clone of it (no disk used) and
     rewrites only the index at the start of the clone, with Ollama's names
     and note. Where the new index is longer than the old one, the first few
     tensors are moved to the end of the file to make room, so that all the
     other numbers stay where they were and stay shared on disk.
     A model quantized in more than one way gets one clone per way, each
     listing only its own tensors, because the note is per file.
  3. names each clone by its sha256 in Ollama's blobs folder, writes a
     manifest that lists them with the settings from step 1, and removes the
     stand-in.

Standard library only. Needs macOS on APFS, the model folder on the same
volume as Ollama's models folder, and an Ollama server that uses that folder.

Usage:
  OLLAMA_HOST=127.0.0.1:<port> OLLAMA_MODELS=<folder> \\
  ollama_mlx_shared_clone.py <mlx-model-folder> <name> [--ollama <binary>] [--measure]

  <name> is written under manifests/, for example
  hf.co/mlx-community/Qwen3.5-0.8B-4bit:latest or plain mymodel.

OLLAMA_MODELS must be set, because `ollama create` writes into the default
folder otherwise, whatever folder the server uses. While trying this out,
use a scratch folder and a scratch server, never the folder with real models.
"""

import argparse
import ctypes
import fcntl
import glob
import hashlib
import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile

TENSOR = "application/vnd.ollama.image.tensor"
STAND_IN = "dedupd-stand-in"


def read_index(path):
    with open(path, "rb") as f:
        length = struct.unpack("<Q", f.read(8))[0]
        return length, json.loads(f.read(length))


def ollama_name(name):
    if name.endswith(".scales"):
        return name[: -len(".scales")] + ".weight.scale"
    if name.endswith(".biases"):
        return name[: -len(".biases")] + ".weight.bias"
    return name


def layer_of(name):
    for suffix in (".weight", ".scales", ".biases"):
        if name.endswith(suffix):
            return name[: -len(suffix)]
    return name


def ways_of_quantizing(folder):
    """A function from a layer's name to the way it is quantized.

    A way is (bits, group size, mode). MLX writes the model's default into
    config.json and, for a model that mixes several, one entry per layer.
    """
    with open(os.path.join(folder, "config.json")) as f:
        q = json.load(f).get("quantization") or {}

    def as_way(d):
        return (d.get("bits"), d.get("group_size"), d.get("mode", "affine"))

    default = as_way(q)
    per_layer = {k: as_way(v) for k, v in q.items() if isinstance(v, dict)}

    def way(layer):
        if layer in per_layer:
            return per_layer[layer]
        for key, value in per_layer.items():
            if layer.endswith("." + key):
                return value
        return default

    way.default = default
    return way


def describe(way):
    return "%s-bit, groups of %s" % (way[0], way[1])


def smallest_layers(shards, way):
    """For each way the model is quantized, its smallest quantized layer."""
    chosen = {}
    for path in shards:
        length, index = read_index(path)
        for name in index:
            if not name.endswith(".scales"):
                continue
            layer = layer_of(name)
            parts = [layer + s for s in (".weight", ".scales", ".biases")]
            if not all(p in index for p in parts):
                continue
            size = sum(index[p]["data_offsets"][1] - index[p]["data_offsets"][0] for p in parts)
            w = way(layer)
            if w not in chosen or size < chosen[w][0]:
                chosen[w] = (size, path, length, {p: index[p] for p in parts})
    return chosen


def import_stand_in(folder, way, layer, ollama, models):
    """Have Ollama import a stand-in quantized one way; return its manifest
    without the weights, and the note Ollama wrote for the layer."""
    _, path, length, parts = layer
    work = tempfile.mkdtemp(prefix="stand-in-", dir=models)
    try:
        for name in os.listdir(folder):
            source = os.path.join(folder, name)
            if not os.path.isfile(source) or name.endswith(".safetensors") or name == "model.safetensors.index.json":
                continue
            if name == "config.json":
                with open(source) as f:
                    config = json.load(f)
                for key in ("quantization", "quantization_config"):
                    if key in config:
                        config[key] = {"group_size": way[1], "bits": way[0], "mode": way[2]}
                with open(os.path.join(work, name), "w") as f:
                    json.dump(config, f, indent=4)
            else:
                subprocess.run(["cp", "-c", source, os.path.join(work, name)], check=True)
        index, data = {"__metadata__": {"format": "mlx"}}, b""
        with open(path, "rb") as f:
            for name, v in parts.items():
                a, b = v["data_offsets"]
                f.seek(8 + length + a)
                index[name] = {"dtype": v["dtype"], "shape": v["shape"], "data_offsets": [len(data), len(data) + b - a]}
                data += f.read(b - a)
        raw = json.dumps(index, separators=(",", ":")).encode()
        with open(os.path.join(work, "model.safetensors"), "wb") as f:
            f.write(struct.pack("<Q", len(raw)) + raw + data)
        modelfile = os.path.join(work, "Modelfile")
        with open(modelfile, "w") as f:
            f.write("FROM %s\n" % work)
        done = subprocess.run([ollama, "create", STAND_IN, "-f", modelfile], capture_output=True, text=True)
        if done.returncode != 0:
            sys.exit("Ollama would not import the stand-in: " + (done.stderr.strip().splitlines() or ["?"])[-1])
    finally:
        shutil.rmtree(work, ignore_errors=True)

    found = glob.glob(os.path.join(models, "manifests*/*/*/%s/latest" % STAND_IN))
    if not found:
        sys.exit("the stand-in was imported, but not into %s" % models)
    with open(os.path.realpath(found[0])) as f:
        manifest = json.load(f)
    written = {}
    for entry in manifest["layers"]:
        if entry["mediaType"] == TENSOR:
            _, index = read_index(os.path.join(models, "blobs", entry["digest"].replace(":", "-")))
            for name in index:
                if name != "__metadata__":
                    written[name] = index.get("__metadata__")
    for name in parts:
        if ollama_name(name) not in written:
            sys.exit("this Ollama names tensors differently: no %s among %s" % (ollama_name(name), sorted(written)))
    note = written[ollama_name(next(iter(parts)))]
    manifest["layers"] = [l for l in manifest["layers"] if l["mediaType"] != TENSOR]
    return manifest, note, len(data)


def ask_ollama(folder, shards, way, ollama, models):
    """The model's settings and the note for each way it is quantized."""
    layers = smallest_layers(shards, way)
    order = sorted(layers, key=lambda w: (w != way.default, str(w)))
    manifest, notes = None, {}
    for w in order:
        answer, notes[w], size = import_stand_in(folder, w, layers[w], ollama, models)
        print("asked Ollama with a stand-in of %d bytes: %s -> %s" % (size, describe(w), notes[w]))
        if manifest is None:
            # The settings are kept by the manifest written later; removing
            # the stand-in itself would remove them before that.
            manifest = answer
            keep = os.path.join(models, "manifests", "registry.ollama.ai", "library", STAND_IN + "-settings", "latest")
            os.makedirs(os.path.dirname(keep), exist_ok=True)
            with open(keep, "w") as f:
                json.dump(answer, f, separators=(",", ":"))
    unknown = [describe(w) for w in order if notes[w] is None]
    if unknown:
        subprocess.run([ollama, "rm", STAND_IN, STAND_IN + "-settings"], capture_output=True)
        sys.exit("this Ollama has no name for %s layers, so it cannot run this model" % " and ".join(unknown))
    return manifest, notes


def plan(tensors, old_length, note, wanted):
    """The new index for one clone, and how much data moves to make room."""
    ordered = sorted(tensors.items(), key=lambda kv: kv[1]["data_offsets"][0])
    data_length = max(v["data_offsets"][1] for _, v in ordered)
    moved = 0
    while True:
        room = ordered[moved - 1][1]["data_offsets"][1] if moved else 0
        index = {"__metadata__": note} if note else {}
        for i, (name, v) in enumerate(ordered):
            if name not in wanted:
                continue
            a, b = v["data_offsets"]
            if i < moved:
                a, b = data_length - room + a, data_length - room + b
            else:
                a, b = a - room, b - room
            index[ollama_name(name)] = {"dtype": v["dtype"], "shape": v["shape"], "data_offsets": [a, b]}
        raw = json.dumps(index, separators=(",", ":")).encode()
        if len(raw) <= old_length + room:
            return raw + b" " * (old_length + room - len(raw)), room, moved, ordered
        moved += 1


def sha256_of(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(16 << 20), b""):
            h.update(block)
    return h.hexdigest()


class Log2Phys(ctypes.Structure):
    _pack_ = 4
    _fields_ = [("flags", ctypes.c_uint32), ("contig", ctypes.c_int64), ("dev", ctypes.c_int64)]


def shared_blocks(a_path, b_path, step=1 << 20):
    """How many of the sampled positions are the same blocks on disk."""
    a, b = os.open(a_path, os.O_RDONLY), os.open(b_path, os.O_RDONLY)

    def where(fd, offset):
        answer = fcntl.fcntl(fd, 65, bytes(Log2Phys(0, 4096, offset)))  # F_LOG2PHYS_EXT
        return Log2Phys.from_buffer_copy(answer).dev

    same = total = 0
    try:
        for offset in range(0, os.path.getsize(a_path) - 4096, step):
            total += 1
            same += where(a, offset) == where(b, offset)
    finally:
        os.close(a)
        os.close(b)
    return same, total


def make_clone(source, blobs, note, wanted, measure):
    old_length, old_index = read_index(source)
    tensors = {k: v for k, v in old_index.items() if k != "__metadata__"}
    raw, room, moved, ordered = plan(tensors, old_length, note, wanted)
    work = os.path.join(blobs, ".clone-in-progress")
    if os.path.lexists(work):
        os.remove(work)
    subprocess.run(["cp", "-c", source, work], check=True)
    with open(work, "r+b") as f:
        f.seek(8 + old_length)
        head = f.read(room)
        f.seek(0)
        f.write(struct.pack("<Q", len(raw)))
        f.write(raw)
        if any(name in wanted for name, _ in ordered[:moved]):
            f.seek(0, 2)
            f.write(head)
    # Every listed tensor must hold the bytes it held in the original.
    new_length, new_index = read_index(work)
    with open(source, "rb") as old, open(work, "rb") as new:
        for i, (name, v) in enumerate(ordered):
            if name not in wanted:
                continue
            a, b = v["data_offsets"]
            c, d = new_index[ollama_name(name)]["data_offsets"]
            assert d - c == b - a
            if i >= moved:
                assert new_length + c == old_length + a, name
            if i < moved or i % 50 == 0:
                old.seek(8 + old_length + a)
                new.seek(8 + new_length + c)
                assert old.read(min(b - a, 1 << 20)) == new.read(min(b - a, 1 << 20)), name
    size = os.path.getsize(work)
    shared = shared_blocks(source, work) if measure else None
    digest = sha256_of(work)
    os.replace(work, os.path.join(blobs, "sha256-" + digest))
    return {"digest": digest, "size": size, "tensors": len(wanted), "shared": shared,
            "longer_by": size - os.path.getsize(source)}


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("folder")
    p.add_argument("name")
    p.add_argument("--ollama", default="ollama")
    p.add_argument("--measure", action="store_true", help="report how much of each clone is shared on disk")
    args = p.parse_args()
    models = os.environ.get("OLLAMA_MODELS") or sys.exit("set OLLAMA_MODELS (and OLLAMA_HOST) to the scratch folder and server")
    blobs = os.path.join(models, "blobs")
    os.makedirs(blobs, exist_ok=True)

    shards = sorted(glob.glob(os.path.join(args.folder, "*.safetensors")))
    way = ways_of_quantizing(args.folder)
    manifest, notes = ask_ollama(args.folder, shards, way, args.ollama, models)
    with open(os.path.join(blobs, manifest["config"]["digest"].replace(":", "-"))) as f:
        settings = json.load(f)
    print("settings:", {k: settings.get(k) for k in ("renderer", "parser", "file_type", "capabilities")})

    layers = []
    for source in shards:
        _, index = read_index(source)
        names = [k for k in index if k != "__metadata__"]
        quantized = {layer_of(n) for n in names if n.endswith(".scales")}
        groups = {}
        for n in names:
            groups.setdefault(way(layer_of(n)) if layer_of(n) in quantized else None, set()).add(n)
        # Tensors that are not quantized need no note; they ride along with
        # the largest group, so that a model quantized in one way stays one
        # clone per weights file.
        plain = groups.pop(None, set())
        if groups:
            groups[max(groups, key=lambda g: len(groups[g]))] |= plain
        else:
            groups[None] = plain
        for w, wanted in sorted(groups.items(), key=lambda kv: str(kv[0])):
            note = notes[w] if w else None
            made = make_clone(source, blobs, note, wanted, args.measure)
            line = "%s  %s: %d tensors, %d bytes longer than the original" % (
                os.path.basename(source), describe(w) if w else "not quantized", made["tensors"], made["longer_by"])
            if made["shared"]:
                line += ", %d of %d sampled positions shared on disk" % made["shared"]
            print(line)
            layers.append({"mediaType": TENSOR, "digest": "sha256:" + made["digest"], "size": made["size"],
                           "name": os.path.basename(source)})

    manifest["layers"] = layers + manifest["layers"]
    name, _, tag = args.name.partition(":")
    if "/" not in name:
        name = "registry.ollama.ai/library/" + name
    target = os.path.join(models, "manifests", name, tag or "latest")
    os.makedirs(os.path.dirname(target), exist_ok=True)
    with open(target, "w") as f:
        json.dump(manifest, f, separators=(",", ":"))
    # Removing the stand-ins leaves the layers that the new manifest also names.
    subprocess.run([args.ollama, "rm", STAND_IN, STAND_IN + "-settings"], capture_output=True)
    print("manifest: %s (%d layers)" % (target, len(manifest["layers"])))


if __name__ == "__main__":
    main()
