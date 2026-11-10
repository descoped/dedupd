//! The Ollama side of a Hugging Face repository: the manifest that Hugging
//! Face's registry generates for a GGUF, and the layers it names.

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// What dedupd presents itself as when asking for a manifest, since the
/// registry's answer depends on the client.
pub const USER_AGENT: &str = "ollama/0.40.16 (arm64 darwin) Go/go1.24.0";
pub const MANIFEST_TYPE: &str = "application/vnd.docker.distribution.manifest.v2+json";
/// The tag Ollama uses when a model name has none.
pub const DEFAULT_TAG: &str = "latest";
/// The registry host Ollama records for models pulled from Hugging Face.
pub const HOST: &str = "hf.co";

#[derive(Debug, Clone, Deserialize)]
pub struct Layer {
    #[serde(rename = "mediaType", default)]
    pub media_type: String,
    pub digest: String,
    #[serde(default)]
    pub size: u64,
}

impl Layer {
    /// The hex sha256 of the layer.
    pub fn sha256(&self) -> Result<&str> {
        let hex = self
            .digest
            .strip_prefix("sha256:")
            .with_context(|| format!("layer digest {} is not a sha256", self.digest))?;
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("layer digest {} is not a sha256", self.digest);
        }
        Ok(hex)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub config: Layer,
    #[serde(default)]
    pub layers: Vec<Layer>,
}

impl Manifest {
    pub fn parse(body: &str) -> Result<Self> {
        let m: Manifest = serde_json::from_str(body).context("the Ollama manifest is not valid")?;
        for l in m.all() {
            l.sha256()?;
        }
        Ok(m)
    }

    /// The config and every layer: all the files Ollama needs for the model.
    pub fn all(&self) -> impl Iterator<Item = &Layer> {
        std::iter::once(&self.config).chain(self.layers.iter())
    }
}
