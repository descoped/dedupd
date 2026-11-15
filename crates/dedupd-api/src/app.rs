//! The apps dedupd puts models into.

use serde::{Deserialize, Serialize};

/// An app, or a family of tools that read one folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum), uniffi(name = "ModelApp"))]
pub enum App {
    /// Bionic and LM Studio, which share `~/.lmstudio`.
    Bionic,
    Ollama,
    /// The Hugging Face cache, which `huggingface_hub`, MLX tools and
    /// llama.cpp all read.
    #[cfg_attr(feature = "ffi", uniffi(name = "huggingFace"))]
    Hf,
}

impl App {
    pub const ALL: [App; 3] = [App::Bionic, App::Ollama, App::Hf];

    /// The app's short name, as the control API and the database write it.
    pub const fn as_str(self) -> &'static str {
        match self {
            App::Bionic => "bionic",
            App::Ollama => "ollama",
            App::Hf => "hf",
        }
    }

    /// The app's name in a sentence.
    pub const fn title(self) -> &'static str {
        match self {
            App::Bionic => "Bionic",
            App::Ollama => "Ollama",
            App::Hf => "the Hugging Face cache",
        }
    }
}

impl std::fmt::Display for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(self.as_str())
    }
}

/// A name that is not one of the apps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownApp(pub String);

impl std::fmt::Display for UnknownApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown app {}; use bionic, ollama or hf (also: mlx, llama.cpp)",
            self.0
        )
    }
}

impl std::error::Error for UnknownApp {}

impl std::str::FromStr for App {
    type Err = UnknownApp;

    /// The short name, or another name people know the app by.
    fn from_str(s: &str) -> Result<Self, UnknownApp> {
        Ok(match s.trim().to_lowercase().as_str() {
            "bionic" | "lmstudio" | "lm-studio" => App::Bionic,
            "ollama" => App::Ollama,
            "hf" | "huggingface" | "mlx" | "llama.cpp" | "llamacpp" => App::Hf,
            other => return Err(UnknownApp(other.to_string())),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_read_back() {
        for app in App::ALL {
            assert_eq!(app.as_str().parse::<App>(), Ok(app));
            let json = serde_json::to_string(&app).unwrap();
            assert_eq!(json, format!("\"{}\"", app.as_str()));
        }
        assert_eq!("LM-Studio".parse::<App>(), Ok(App::Bionic));
        assert!("gimp".parse::<App>().is_err());
    }
}
