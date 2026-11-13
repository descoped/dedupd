---
title: "Roadmap"
description: "What dedupd has today and what is planned: the catalogue, ModelScope, Linux with NVIDIA GPUs and more apps."
group: Project
order: 6
---

# Roadmap

dedupd keeps one copy of each model file and shares it with every app that can use it. The goal is the least disk
space for every pair of app and model, on every platform dedupd supports.

This page lists what is available today and what is planned. Planned work is listed in the order it will be built
and has no dates. A platform, an app or a model source is called supported only after it has been tested.

## Available today

| Area | What dedupd has |
|---|---|
| Platform | macOS 26 on Apple silicon. Requires APFS, with the store and the apps' model directories on one volume |
| Apps | Ollama; Bionic and LM Studio; the Hugging Face cache, which serves `hf`, `huggingface_hub`, `mlx_lm` and llama.cpp |
| Model source | Hugging Face |
| Store | One file per content, named by its sha256, shared with the apps as clones. Existing models are adopted; duplicate copies are replaced by clones |
| Downloads | A durable queue with pause, resume and retry, paced by what the source allows. Every file is verified against its sha256 |
| Front ends | The Hugging Face Hub API and the Ollama registry API on a local port, answered from the store, also when the source is unreachable |
| App profiles | The rules for each app as data: which formats it takes, by which method, at what cost in disk space, and with which versions of the app this was tested |
| Archive disk | A second copy of every stored file on another disk; archive and restore for one model |
| Interfaces | The `dedupd` command, a control API, and a macOS app |
| Distribution | A notarised disk image of the app, which carries the command and the daemon |

## Planned

| Order | Item | Summary |
|---|---|---|
| 1 | Catalogue | Families, editions and placements, so that the right edition is chosen for each app and machine |
| 2 | ModelScope | A second model source |
| 3 | Linux | The daemon and the command line on Linux, for machines with NVIDIA GPUs |
| 4 | More apps | Further app profiles, one at a time |

Each item is usable on its own.

## Catalogue

A model exists in several *editions*: GGUF in several quantizations, MLX in 4-bit or 8-bit, safetensors as AWQ,
GPTQ or FP8. Which edition a machine needs depends on its apps and its hardware. The catalogue has three levels:

| Level | What it is |
|---|---|
| Family | One base model. Hugging Face and ModelScope both state which base model a repository derives from |
| Edition | One form of the model that an app can load: a format, a quantization and its files |
| Placement | An edition activated in one app |

With the catalogue, dedupd shows a family once, states which edition each installed app takes, and downloads only
that edition. Everything a later activation needs is fetched at download, so a stored model can be activated in
another app without the source being reachable.

Editions do not share disk space, and dedupd does not convert between them. When an app needs another edition,
dedupd downloads it.

## ModelScope

A model source is a plug-in. It lists a repository's files with their sha256, resolves a branch to a fixed
revision, downloads with resume and provides the model's metadata. Hugging Face is the first source; ModelScope is
the second.

A first check against modelscope.cn showed that a model file carries the same sha256 there as on Hugging Face. A
model available from both sources is therefore stored once. ModelScope also serves an Ollama registry. To be
confirmed: its file listing, how revisions are resolved, and models with access terms.

Further sources, such as the Ollama library, follow the same plug-in interface.

## Linux

Target systems: Linux workstations and servers with NVIDIA GPUs, from RTX cards to H100 and H200.

### Deliverable

The dedupd daemon and command line for Linux. No graphical application is planned for Linux; a system is
administered through the command line and the control API. The command line covers the complete workflow: `pull`,
`activate`, `deactivate`, `adopt`, `status`, `apps`, `archive`, `restore`, `verify` and `gc`, with `--json` output
for automation.

### Prerequisites

dedupd depends on capabilities of the underlying system. Without them it does not operate; it does not substitute
symbolic links, hard links or copies.

| Prerequisite | Detail |
|---|---|
| Copy-on-write file cloning (reflink) | The file system must be able to create a second file that shares the data blocks of the first. On Linux this is the `FICLONE` operation, also used by `cp --reflink` |
| One file system for the store and the apps | The dedupd store and every app's model directory must reside on the same file system. A clone cannot span file systems |
| systemd | For running the daemon as a service |

File system support:

| File system | Status |
|---|---|
| Btrfs | Planned. Not yet tested with dedupd |
| XFS with reflink enabled (the default of current `mkfs.xfs`) | Planned. Not yet tested with dedupd |
| ZFS with block cloning | Under evaluation. Block cloning has had data-loss defects and has been disabled by default in some OpenZFS releases |
| ext4 | Not supported. ext4 does not provide file cloning |

ext4 is the default of the Ubuntu installer. On such a system the models require a dedicated Btrfs or XFS volume
that holds the dedupd store and the apps' model directories. The archive disk is exempt: it holds plain copies and
can use any file system.

### Implementation

The store, the database, the download queue, the Hugging Face and Ollama front ends, the control API and the app
profiles are platform-independent Rust and carry over unchanged. A Linux implementation consists of the
following work:

| Component | Work |
|---|---|
| Cloning | Cloning is isolated in one module. The Linux variant issues `FICLONE` and keeps the existing contract: clone into a temporary name, then rename into place atomically |
| Prerequisite verification | At installation and at start, dedupd attempts a clone within the store's file system and between the store and each app directory. A directory that fails is reported by name and excluded from placement; nothing is copied instead |
| Service management | `dedupd install`, `uninstall`, `start`, `stop` and `agent` manage a systemd unit. Whether this is a user unit or a system unit is open; see the open points |
| Directory monitoring | The background pass watches the app directories through inotify, with the existing periodic scan as fallback. Recursive watches on large model directories are subject to the kernel's inotify watch limit |
| App profiles | Profiles gain per-platform entries: the directories each app uses on Linux, how its installed version is detected, and which placement methods apply |
| Testing | The test suite creates real clones. On Linux it runs against Btrfs and XFS volumes, in continuous integration as well |
| Distribution | The form of delivery (single binary, distribution package or both) is to be decided |

### Operation

The intended operation on a Linux system:

1. The models volume is Btrfs or XFS and holds the dedupd home directory and the apps' model directories.
2. `dedupd install` verifies the prerequisites and registers the daemon as a systemd service.
3. `dedupd adopt` takes the models already present in the app directories into the store. Duplicate copies are
   then replaced by clones.
4. New models are obtained with `dedupd pull <model> --for <apps>`, which downloads once and activates the model
   in the named apps.
5. Tools that use the Hugging Face Hub API are pointed at the daemon with `HF_ENDPOINT`; Ollama can pull through
   the daemon's registry endpoint. Both are answered from the store.
6. With an archive disk configured, every stored file is also copied there, and single models can be archived and
   restored.

### Apps and model editions on NVIDIA hardware

dedupd does not use the GPU. The hardware determines which inference stacks run on a system, and those stacks load
specific editions of a model:

| Stack | Edition | Status |
|---|---|---|
| Ollama, llama.cpp | GGUF | Planned; the placement method exists |
| LM Studio | GGUF | Planned; its directories on Linux are to be confirmed |
| vLLM, SGLang | Safetensors: original weights, AWQ, GPTQ, FP8 | These stacks load from the Hugging Face cache, a layout dedupd already writes. Not yet tested with them |
| Containerised stacks | The edition the contained stack loads | Containers commonly mount the host's Hugging Face cache. To be tested |

Which edition a given GPU can run is defined by each stack; vLLM, for example, supports FP8 on some GPU families
only. App profiles hold such constraints as data, per app and version, so that dedupd can state before a download
whether an edition is usable on the system.

### Open points

- **Ollama's service account.** A standard Linux installation runs Ollama as the user `ollama`, with its models
  under `/usr/share/ollama/.ollama/models`. How dedupd obtains write access to that directory is to be decided,
  and determines whether dedupd runs as a user unit or a system unit.
- **Multi-user systems.** A store shared between users requires a defined model for file ownership and
  permissions.
- **Remote administration.** The daemon binds to the loopback interface and uses plain HTTP. A system is
  administered from a shell on that system. Access from other hosts requires authentication and transport
  encryption, which are not designed yet.
- **RTX 50 series.** The editions each stack supports on these GPUs are to be confirmed from each stack's
  documentation.
- **ZFS**, as stated under prerequisites.

## More apps

Hugging Face lists about 25 local apps on its model pages; most load GGUF. An app that keeps its own models folder
can be given a profile: where it keeps its files, which formats it takes, and which versions dedupd was tested
with. This includes image tools, which keep large safetensors files in folders of their own. Each profile is added
with tests against the real app.

## macOS

- Updates for the app.
- Downloads from Hugging Face through its Xet storage, which is faster than the plain download used today.

## Not planned

- **Conversion between formats.** dedupd downloads the edition an app needs.
- **Operation without file cloning.** dedupd does not fall back to symbolic links, hard links or copies on file
  systems without copy-on-write cloning.
- **Interception of apps' encrypted traffic** with a local certificate. Apps that cannot be pointed at the daemon
  are covered by placing models into their folders and adopting what they download.
