//! URL helpers.

/// Percent-encode everything but unreserved characters (RFC 3986).
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn encodes_queries() {
        assert_eq!(super::encode("rust slint"), "rust%20slint");
        assert_eq!(super::encode("a&b=c/ü"), "a%26b%3Dc%2F%C3%BC");
    }
}
