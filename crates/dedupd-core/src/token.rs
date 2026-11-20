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

/// Whether `url` is on the provider at `endpoint`: the same scheme, host and
/// port. The token and a caller's `Authorization` go nowhere else. Text that
/// only begins like the endpoint (`https://huggingface.co.example.net`,
/// `https://huggingface.co@example.net`) is another host.
pub fn goes_to(endpoint: &str, url: &str) -> bool {
    let (Ok(ours), Ok(theirs)) = (reqwest::Url::parse(endpoint), reqwest::Url::parse(url)) else {
        return false;
    };
    ours.scheme() == theirs.scheme()
        && ours.host() == theirs.host()
        && ours.port_or_known_default() == theirs.port_or_known_default()
}

/// Where `from_environment` would read the token from: the name
/// of the variable or the path of the file. `None` when there is none.
pub fn source() -> Option<String> {
    found().map(|(source, _)| source)
}

/// The token other Hugging Face tools use: `HF_TOKEN`, else the token file.
pub fn from_environment() -> Option<String> {
    found().map(|(_, token)| token)
}

/// The one place that says where the token is looked for: its source and
/// its value. A value that cannot be a token counts as none (see
/// `usable`), so the next place is tried and `source` never names a place
/// the client does not read.
fn found() -> Option<(String, String)> {
    if let Some(token) = std::env::var("HF_TOKEN").ok().as_deref().and_then(usable) {
        return Some(("HF_TOKEN".into(), token));
    }
    let file = token_file()?;
    let token = usable(&std::fs::read_to_string(&file).ok()?)?;
    Some((file.to_string_lossy().into_owned(), token))
}

/// The token in `text`, trimmed, when it can be one: visible ASCII with
/// no space in it. Anything else (a file with a second line, a control
/// character) cannot be sent as a header; taken as a token it would make
/// every request fail as if the provider were out of reach.
fn usable(text: &str) -> Option<String> {
    let token = text.trim();
    (!token.is_empty() && token.len() <= 4096 && token.bytes().all(|b| b.is_ascii_graphic()))
        .then(|| token.to_string())
}

fn token_file() -> Option<PathBuf> {
    std::env::var_os("HF_TOKEN_PATH")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HF_HOME").map(|h| PathBuf::from(h).join("token")))
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache/huggingface/token"))
        })
}

#[cfg(test)]
mod tests {
    use super::{goes_to, usable};

    #[test]
    fn a_value_that_cannot_be_a_header_is_not_a_token() {
        assert_eq!(usable(" hf_abc123\n").as_deref(), Some("hf_abc123"));
        for bad in [
            "",
            "  \n",
            "hf_abc\nhf_def",
            "hf abc",
            "hf_\u{7}abc",
            "hf_æ",
        ] {
            assert_eq!(usable(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn only_the_endpoints_own_host_gets_the_token() {
        let hf = "https://huggingface.co";
        assert!(goes_to(hf, "https://huggingface.co/api/models/o/r"));
        assert!(goes_to(hf, "https://huggingface.co:443/o/r/resolve/main/f"));
        assert!(goes_to("http://127.0.0.1:9000", "http://127.0.0.1:9000/x"));
        for other in [
            "https://huggingface.co.example.net/api/models/o/r",
            "https://huggingface.co@example.net/api/models/o/r",
            "https://huggingface.co:8443/x",
            "http://huggingface.co/x",
            "https://cdn-lfs.huggingface.co/x",
            "not an address",
        ] {
            assert!(!goes_to(hf, other), "{other}");
        }
        assert!(!goes_to("http://127.0.0.1:9000", "http://127.0.0.1:9001/x"));
    }
}
