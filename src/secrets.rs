//! Opt-in Secret inspection (k9s `x`): names, types and key sizes are listed;
//! one decoded value is fetched only on an explicit per-key reveal.
use crate::{Result, safety};
use std::collections::BTreeMap;

/// Values larger than this are reported by size and never painted.
pub const MAX_REVEAL_BYTES: usize = 16 * 1024;
/// At most this many Secrets are indexed for one namespace.
pub const MAX_SECRETS: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretEntry {
    pub name: String,
    pub uid: String,
    pub secret_type: String,
    /// Key name and decoded byte length. Values are never retained in the index.
    pub keys: Vec<(String, usize)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretIndex {
    pub namespace: String,
    pub items: Vec<SecretEntry>,
    pub truncated: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub enum Revealed {
    Text(String),
    Binary(usize),
    TooLarge(usize),
}
impl std::fmt::Debug for Revealed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(_) => f.write_str("Text(<hidden>)"),
            Self::Binary(n) => write!(f, "Binary({n})"),
            Self::TooLarge(n) => write!(f, "TooLarge({n})"),
        }
    }
}

pub fn entry(
    name: &str,
    uid: &str,
    secret_type: Option<&str>,
    data: Option<&BTreeMap<String, Vec<u8>>>,
) -> SecretEntry {
    SecretEntry {
        name: safety::safe_text(name, 253),
        uid: safety::safe_text(uid, 64),
        secret_type: safety::safe_text(secret_type.unwrap_or("Opaque"), 128),
        keys: data
            .into_iter()
            .flatten()
            .map(|(key, value)| (safety::safe_text(key, 253), value.len()))
            .collect(),
    }
}

/// Decodes one value for display. Text keeps line breaks and tabs but drops
/// terminal controls; anything else is reported as binary by size.
pub fn reveal(bytes: &[u8]) -> Revealed {
    if bytes.len() > MAX_REVEAL_BYTES {
        return Revealed::TooLarge(bytes.len());
    }
    match std::str::from_utf8(bytes) {
        Ok(text)
            if text
                .chars()
                .all(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t')) =>
        {
            Revealed::Text(safety::safe_lines(
                &text.replace("\r\n", "\n").replace('\t', "    "),
                MAX_REVEAL_BYTES,
            ))
        }
        _ => Revealed::Binary(bytes.len()),
    }
}

pub fn validate_request(namespace: &str, name: &str, key: &str) -> Result<()> {
    if namespace == "*" || !safety::valid_id(namespace) {
        return Err("Secret inspection requires one explicit namespace".into());
    }
    if name.is_empty() || name.len() > 253 || safety::safe_text(name, 253) != name {
        return Err("invalid Secret name".into());
    }
    if key.is_empty()
        || key.len() > 253
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err("invalid Secret key".into());
    }
    Ok(())
}

/// Offline sample Secret for demo mode; it is never part of generic demo lists.
pub fn demo_data() -> BTreeMap<String, Vec<u8>> {
    BTreeMap::from([
        ("username".to_owned(), b"demo-user".to_vec()),
        ("password".to_owned(), b"not-a-real-password".to_vec()),
        (
            "ca.crt".to_owned(),
            b"-----BEGIN CERTIFICATE-----\nMIIBdemo\n-----END CERTIFICATE-----\n".to_vec(),
        ),
        ("blob.bin".to_owned(), vec![0, 159, 146, 150]),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reveal_keeps_text_lines_and_never_paints_controls_or_binary() {
        assert_eq!(
            reveal(b"line1\r\nline2\tx"),
            Revealed::Text("line1\nline2    x".into())
        );
        assert_eq!(reveal(b"a\x1b[2Jb"), Revealed::Binary(6));
        assert_eq!(reveal(&[0, 159, 146]), Revealed::Binary(3));
        assert_eq!(
            reveal(&vec![b'a'; MAX_REVEAL_BYTES + 1]),
            Revealed::TooLarge(MAX_REVEAL_BYTES + 1)
        );
        assert_eq!(format!("{:?}", reveal(b"hunter2")), "Text(<hidden>)");
    }
    #[test]
    fn index_entries_hold_sizes_not_values() {
        let data = demo_data();
        let entry = entry("api", "uid-1", None, Some(&data));
        assert_eq!(entry.secret_type, "Opaque");
        assert!(entry.keys.contains(&("password".into(), 19)));
        assert!(!format!("{entry:?}").contains("not-a-real-password"));
    }
    #[test]
    fn requests_need_one_namespace_and_plain_keys() {
        assert!(validate_request("demo", "api", "tls.crt").is_ok());
        for (ns, name, key) in [
            ("*", "api", "k"),
            ("demo", "", "k"),
            ("demo", "api", ""),
            ("demo", "api", "../k"),
            ("demo", "api\u{1b}", "k"),
        ] {
            assert!(
                validate_request(ns, name, key).is_err(),
                "{ns} {name} {key}"
            );
        }
    }
}
