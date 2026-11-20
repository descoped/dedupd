//! Percent-encoding, as the control API, the Hub API and the provider's
//! URLs use it: one set of rules for a path and one for a query.

/// The bytes that are never encoded (RFC 3986's unreserved characters).
fn unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~')
}

fn encode_where(text: &str, keep: impl Fn(u8) -> bool) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if keep(b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Encode one value: a query value, or one segment of a path.
pub fn encode_component(text: &str) -> String {
    encode_where(text, unreserved)
}

/// Encode a path such as `org/repo` or `sub/file.gguf`, keeping its `/`.
pub fn encode_path(path: &str) -> String {
    encode_where(path, |b| unreserved(b) || b == b'/')
}

fn decode(text: &str, plus_is_space: bool) -> String {
    let bytes = text.as_bytes();
    let hex = |i: usize| bytes.get(i).and_then(|b| (*b as char).to_digit(16));
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match (bytes[i], hex(i + 1), hex(i + 2)) {
            (b'%', Some(high), Some(low)) => {
                out.push((high * 16 + low) as u8);
                i += 2;
            }
            (b'+', _, _) if plus_is_space => out.push(b' '),
            (b, _, _) => out.push(b),
        }
        i += 1;
    }
    // An escape that is not UTF-8 becomes U+FFFD, on purpose: what is
    // decoded here is a name or a search word, which its reader checks
    // (`names::is_repo_name`, `is_file_path`) and refuses in that form.
    String::from_utf8_lossy(&out).into_owned()
}

/// Decode a path: `%XX` only; a `+` is a plus sign.
pub fn decode_path(text: &str) -> String {
    decode(text, false)
}

/// Decode a query value: `%XX`, and `+` is a space.
pub fn decode_query(text: &str) -> String {
    decode(text, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_keeps_its_slashes_and_its_plus_signs() {
        assert_eq!(encode_path("org/a b+c.gguf"), "org/a%20b%2Bc.gguf");
        assert_eq!(decode_path("a%20b+c.gguf"), "a b+c.gguf");
        assert_eq!(decode_path("org%2Frepo"), "org/repo");
    }

    #[test]
    fn a_query_value_is_encoded_whole_and_plus_is_a_space() {
        assert_eq!(encode_component("base_model:a/b"), "base_model%3Aa%2Fb");
        assert_eq!(decode_query("a+b%2Bc"), "a b+c");
    }

    #[test]
    fn a_broken_escape_is_kept_as_it_is() {
        assert_eq!(decode_path("100%"), "100%");
        assert_eq!(decode_path("%zz"), "%zz");
        assert_eq!(decode_query("%e2%82%ac"), "€");
    }
}
