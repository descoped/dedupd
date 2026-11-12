# Apps and formats

dedupd determines from data which application can be given which model, by which method and at what cost in disk
space. Each application has a profile in `crates/dedupd-core/profiles/`. `dedupd apps` prints the profiles,
`dedupd apps org/repo` prints each application's result for a stored model, and the app shows the same result next
to each Activate button.

## Compatibility

| Application | GGUF | MLX | Other formats |
|---|---|---|---|
| Ollama | Clone, no additional space | 4-bit and 8-bit: clone with rewritten index, a few megabytes | Not supported |
| Bionic and LM Studio | Clone, no additional space | Clone, no additional space | Not supported |
| Hugging Face cache (`hf`, `huggingface_hub`, `mlx_lm`, llama.cpp) | Clone, no additional space | Clone, no additional space | Clone, no additional space |

Each method records the application versions it was tested with (Ollama 0.40.16, Bionic 1.2.0). Another version
is not refused; dedupd reports that the method was not tested with it.

## MLX models in Ollama

Ollama cannot pull an MLX repository from Hugging Face, and its own import writes a complete second copy of the
weights. dedupd places the model without that copy. Each weights file is provided as a clone in which only the
index at the start of the file is rewritten to the tensor names Ollama expects; the tensor data is shared with the
store. A 16 GB model required about 110 MB of additional disk space.

Constraints:

- Ollama runs 4-bit and 8-bit MLX models. Other bit widths are refused, because Ollama imports them and then fails
  to load them.
- Ollama must be installed. dedupd queries its command line tool for the settings it would write for the model.
- The method relies on behaviour Ollama does not document. It was tested with Ollama 0.40.16.

## Limitations

- **Split GGUF files in Ollama.** Ollama cannot use a GGUF that is split into parts.
- **Bionic and LM Studio have no endpoint setting.** Their downloads cannot be routed through the daemon. dedupd
  covers them by placing models into their directory and by adopting what they download.
- **Single files over 50 GB through `huggingface_hub`.** The client refuses such a download over plain HTTP. Use
  `dedupd pull … --for hf`; the client then finds the file in its cache.
- **Editions do not share space.** GGUF, MLX and other editions of a model are different files.
- **File systems without copy-on-write cloning are not supported.** See the requirements in
  [How it works](how-it-works.md#requirements).

## Model sources

Hugging Face is the only supported source. The app recognises a ModelScope address and reports it as not yet
supported; ModelScope is on the [roadmap](roadmap.md).
