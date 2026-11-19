//! The Hugging Face token, shared by the Hub client and the fetcher.

use std::path::PathBuf;
use std::sync::Arc;

/// The Hugging Face token the clients send. One taken from the
/// environment (`HF_TOKEN`, or the file `hf auth login` writes) is read
/// again by `refresh`, so signing in or out takes effect without a restart.
pub struct Token {
    value: std::sync::RwLock<Option<String>>,
    /// Where the token is read again from; `None` for a fixed one.
    source: Option<Box<dyn Fn() -> Option<String> + Send + Sync>>,
}

impl Token {
    /// A token that never changes, or none.
    pub fn fixed(value: Option<String>) -> Arc<Self> {
        Arc::new(Self {
            value: std::sync::RwLock::new(value),
            source: None,
        })
    }

    /// The token other Hugging Face tools use, read again by `refresh`.
    pub fn from_environment() -> Arc<Self> {
        Self::read_by(from_environment)
    }

    /// A token read by `source` now and on every `refresh`.
    pub fn read_by(source: impl Fn() -> Option<String> + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            value: std::sync::RwLock::new(source()),
            source: Some(Box::new(source)),
        })
    }

    pub fn get(&self) -> Option<String> {
        self.value.read().expect("lock").clone()
    }

    /// Read the token again. Returns whether it changed.
    pub fn refresh(&self) -> bool {
        let Some(source) = &self.source else {
            return false;
        };
        let now = source();
        let mut value = self.value.write().expect("lock");
        if *value == now {
            return false;
        }
        *value = now;
        true
    }
}

/// Where `from_environment` would read the token from: the name
/// of the variable or the path of the file. `None` when there is none.
pub fn source() -> Option<String> {
    if std::env::var("HF_TOKEN").is_ok_and(|t| !t.trim().is_empty()) {
        return Some("HF_TOKEN".into());
    }
    let file = token_file()?;
    let text = std::fs::read_to_string(&file).ok()?;
    (!text.trim().is_empty()).then(|| file.to_string_lossy().into_owned())
}

/// The token other Hugging Face tools use: `HF_TOKEN`, else the token file.
pub fn from_environment() -> Option<String> {
    if let Ok(t) = std::env::var("HF_TOKEN")
        && !t.trim().is_empty()
    {
        return Some(t.trim().to_string());
    }
    let t = std::fs::read_to_string(token_file()?).ok()?;
    (!t.trim().is_empty()).then(|| t.trim().to_string())
}

/// The file Hugging Face's own tools keep the token in.
fn token_file() -> Option<PathBuf> {
    std::env::var_os("HF_TOKEN_PATH")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HF_HOME").map(|h| PathBuf::from(h).join("token")))
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache/huggingface/token"))
        })
}
