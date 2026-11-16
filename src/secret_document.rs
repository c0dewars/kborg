//! The whole-Secret editor's document: every key of one Secret with its value
//! as text, edited in KBorg's own editor and never written to disk.
//!
//! A value is written so that it reads back byte for byte: a multi-line text
//! value as a YAML block, anything else as a double-quoted ASCII string with
//! escapes. A value that is not UTF-8 text cannot be edited as text; it is
//! listed under `keep:` and stays as it is. Every rendered document is parsed
//! back and compared before it is shown.
use crate::{
    Result,
    secret_edit::{MAX_VALUE, SecretBytes, valid_key},
    secrets::wipe,
};
use k8s_openapi::api::core::v1::Secret;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Largest document the editor opens or accepts: the API server's limit for
/// a whole Secret.
pub const MAX_DOCUMENT: usize = 1024 * 1024;

/// What the editor keeps about the Secret it edits: never a value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretDoc {
    pub namespace: String,
    pub name: String,
    pub uid: String,
    /// The resourceVersion the document was read at.
    pub version: String,
    pub secret_type: String,
    pub keys: usize,
}

/// A rendered document. It holds Secret values: it never prints and its
/// bytes are overwritten when it is dropped.
pub struct SecretText(String);
impl SecretText {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl Drop for SecretText {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}
impl std::fmt::Debug for SecretText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretText({} bytes)", self.0.len())
    }
}

/// The edited document: new values by key and the keys kept unread.
#[derive(Debug)]
pub struct Edited {
    pub version: String,
    pub values: Vec<(String, SecretBytes)>,
    pub keep: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    data: Option<BTreeMap<String, String>>,
    #[serde(default)]
    keep: Option<Vec<String>>,
}
impl Drop for File {
    fn drop(&mut self) {
        for value in self.data.iter_mut().flat_map(|data| data.values_mut()) {
            wipe(value);
        }
    }
}

fn parse_file(text: &str) -> Result<File> {
    let options = serde_saphyr::options! {
        budget: serde_saphyr::budget! {
            max_reader_input_bytes: Some(MAX_DOCUMENT), max_events: 200_000, max_aliases: 0, max_anchors: 0,
            max_depth: 4, max_inclusion_depth: 0, max_documents: 1, max_nodes: 20_000,
            max_total_scalar_bytes: MAX_DOCUMENT, max_merge_keys: 0,
        },
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::Error,
        merge_keys: serde_saphyr::MergeKeyPolicy::Error,
        with_snippet: false,
    };
    // The parser's message can quote the text; only its position is kept.
    serde_saphyr::from_str_with_options(text, options).map_err(|error| {
        format!(
            "the Secret document is not valid{}: write each key as `name: \"value\"` under data:, with no duplicate keys",
            crate::safety::yaml_location(&error)
        )
    })
}

/// `"..."` with only printable ASCII inside: `\"`, `\\`, `\n`, `\t`, `\r`
/// and `\uXXXX` / `\UXXXXXXXX` for everything else.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            ' '..='~' => out.push(c),
            c if u32::from(c) <= 0xFFFF => out.push_str(&format!("\\u{:04X}", u32::from(c))),
            c => out.push_str(&format!("\\U{:08X}", u32::from(c))),
        }
    }
    out.push('"');
    out
}

/// A literal block for multi-line printable ASCII text, or `None` when the
/// value is better quoted.
fn block(text: &str) -> Option<String> {
    let body = text.trim_end_matches('\n');
    let trailing = text.len() - body.len();
    if !body.contains('\n')
        || body.starts_with(' ')
        || body.starts_with('\n')
        || !body
            .bytes()
            .all(|b| b == b'\n' || (b' '..=b'~').contains(&b))
        || body.lines().any(|line| line.ends_with(' '))
    {
        return None;
    }
    let chomp = match trailing {
        0 => "|-",
        1 => "|",
        _ => "|+",
    };
    let mut out = String::from(chomp);
    for line in body.split('\n') {
        out.push('\n');
        if !line.is_empty() {
            out.push_str("    ");
            out.push_str(line);
        }
    }
    // `|+` keeps the extra line breaks as empty lines of the block.
    for _ in 1..trailing {
        out.push('\n');
    }
    Some(out)
}

/// A key as YAML reads it back as the same string.
fn key_text(key: &str) -> String {
    let plain = key.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && !matches!(
            key.to_ascii_lowercase().as_str(),
            "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "y" | "n"
        );
    if plain { key.to_owned() } else { quoted(key) }
}

fn write(doc: &SecretDoc, text_keys: &[(&str, &str)], keep: &[&str], blocks: bool) -> String {
    let mut out = format!(
        "# Secret {}/{} (type {}, {} key{}). Values are shown as stored.\n\
         # Change a value, add `key: \"value\"` under data:, or delete a key's line.\n\
         # Ctrl-S reviews the change (no value is shown there); Esc cancels.\n\
         # Nothing is written to disk or sent before you approve the review.\n\
         data:\n",
        doc.namespace,
        doc.name,
        doc.secret_type,
        doc.keys,
        if doc.keys == 1 { "" } else { "s" }
    );
    if text_keys.is_empty() {
        out.push_str("  # new-key: \"value\"\n");
    }
    for (key, value) in text_keys {
        let rendered = blocks
            .then(|| block(value))
            .flatten()
            .unwrap_or_else(|| quoted(value));
        out.push_str(&format!("  {}: {rendered}\n", key_text(key)));
    }
    if !keep.is_empty() {
        out.push_str(
            "# Values that are not text stay as they are. Delete a name to remove its key.\nkeep:\n",
        );
        for key in keep {
            out.push_str(&format!("  - {}\n", key_text(key)));
        }
    }
    out
}

/// The document for a Secret, and what the editor keeps about it. The
/// caller wipes the Secret; the returned text holds its values.
pub fn render(
    namespace: &str,
    name: &str,
    uid: &str,
    secret: &Secret,
) -> Result<(SecretDoc, SecretText)> {
    let doc = SecretDoc {
        namespace: namespace.into(),
        name: name.into(),
        uid: uid.into(),
        version: secret.metadata.resource_version.clone().unwrap_or_default(),
        secret_type: crate::safety::safe_text(secret.type_.as_deref().unwrap_or("Opaque"), 100),
        keys: secret.data.as_ref().map_or(0, BTreeMap::len),
    };
    if secret.immutable == Some(true) {
        return Err("this Secret is immutable: its keys cannot change. Next: delete it and create a new one".into());
    }
    let mut text_keys = Vec::new();
    let mut keep = Vec::new();
    for (key, value) in secret.data.iter().flatten() {
        if !valid_key(key) {
            return Err("the Secret has a key name KBorg cannot edit".into());
        }
        match std::str::from_utf8(&value.0) {
            Ok(text) => text_keys.push((key.as_str(), text)),
            Err(_) => keep.push(key.as_str()),
        }
    }
    for blocks in [true, false] {
        let mut text = write(&doc, &text_keys, &keep, blocks);
        if text.len() > MAX_DOCUMENT {
            wipe(&mut text);
            return Err("this Secret is too large to edit as a whole (over 1 MiB as text). Next: change one key with e and a @/path/to/file".into());
        }
        // Shown only if it reads back exactly as stored.
        let same = parse_file(&text).is_ok_and(|file| {
            let data_same = match &file.data {
                None => text_keys.is_empty(),
                Some(data) => {
                    data.len() == text_keys.len()
                        && text_keys
                            .iter()
                            .all(|(k, v)| data.get(*k).map(String::as_str) == Some(*v))
                }
            };
            file.keep.as_deref().unwrap_or_default() == keep && data_same
        });
        if same {
            return Ok((doc, SecretText(text)));
        }
        wipe(&mut text);
    }
    Err("this Secret cannot be shown as an editable document. Next: change one key at a time with e".into())
}

/// The edited text as new values and kept keys. Errors never quote a value.
pub fn parse(text: &str, version: &str) -> Result<Edited> {
    if text.len() > MAX_DOCUMENT {
        return Err("the Secret document is larger than 1 MiB".into());
    }
    let mut file = parse_file(text)?;
    let keep = file.keep.take().unwrap_or_default();
    let data = file.data.take().unwrap_or_default();
    let mut values = Vec::with_capacity(data.len());
    let mut total = 0usize;
    let mut bad = None;
    for (mut key, mut value) in data {
        if bad.is_some() {
            wipe(&mut value);
            continue;
        }
        if !valid_key(&key) {
            bad = Some(format!(
                "{:?} is not a Secret key name: 1 to 253 letters, digits, '-', '_' and '.'",
                crate::safety::safe_text(&key, 64)
            ));
        } else if keep.contains(&key) {
            bad = Some(format!(
                "key {key} is both under data: and keep:; keep it in one place"
            ));
        } else if value.len() > MAX_VALUE {
            bad = Some(format!("the value of {key} is larger than 512 KiB"));
        }
        total = total.saturating_add(value.len() + key.len());
        if bad.is_some() {
            wipe(&mut value);
            wipe(&mut key);
            continue;
        }
        let bytes = std::mem::take(&mut value).into_bytes();
        values.push((key, SecretBytes::new(bytes)));
    }
    if let Some(error) = bad {
        return Err(error);
    }
    if total > MAX_DOCUMENT {
        return Err("the Secret would be larger than 1 MiB".into());
    }
    if let Some(key) = keep.iter().find(|key| !valid_key(key)) {
        return Err(format!(
            "{:?} under keep: is not a Secret key name",
            crate::safety::safe_text(key, 64)
        ));
    }
    Ok(Edited {
        version: version.into(),
        values,
        keep,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(entries: &[(&str, &[u8])]) -> Secret {
        let mut secret = Secret {
            type_: Some("Opaque".into()),
            data: Some(
                entries
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), k8s_openapi::ByteString(v.to_vec())))
                    .collect(),
            ),
            ..Default::default()
        };
        secret.metadata.resource_version = Some("41".into());
        secret
    }
    fn values(edited: &Edited) -> Vec<(&str, &[u8])> {
        edited
            .values
            .iter()
            .map(|(k, v)| (k.as_str(), v.bytes()))
            .collect()
    }

    #[test]
    fn every_value_reads_back_byte_for_byte() -> Result<()> {
        let entries: &[(&str, &[u8])] = &[
            ("username", b"admin"),
            ("password", b"p@ss \"w0rd\" \\ #not-a-comment: x"),
            (
                "ca.crt",
                b"-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n",
            ),
            ("script", b"line one\n  indented\n\nlast"),
            ("blank-lines", b"a\nb\n\n\n"),
            ("leading-space", b" x\ny"),
            ("trailing-space", b"x \ny"),
            ("unicode", "café ☕ 𝄞".as_bytes()),
            ("tabs", b"a\tb\r\n"),
            ("empty", b""),
            ("number", b"007"),
            ("yes", b"no"),
            ("1st", b"true"),
            ("blob.bin", &[0, 159, 146, 150]),
        ];
        let (doc, rendered) = render("demo", "api", "uid-1", &secret(entries))?;
        let text = rendered.as_str();
        assert_eq!((doc.version.as_str(), doc.keys), ("41", 14));
        assert!(text.is_ascii(), "the editor takes ASCII only");
        assert!(
            text.contains("  ca.crt: |\n    -----BEGIN CERTIFICATE-----"),
            "{text}"
        );
        assert!(text.contains("keep:\n  - blob.bin"), "{text}");
        let edited = parse(text, &doc.version)?;
        let mut expected: Vec<(&str, &[u8])> = entries
            .iter()
            .filter(|(k, _)| *k != "blob.bin")
            .copied()
            .collect();
        expected.sort();
        assert_eq!(values(&edited), expected);
        assert_eq!(edited.keep, ["blob.bin"]);
        Ok(())
    }

    #[test]
    fn edits_add_and_remove_keys_and_errors_never_quote_a_value() -> Result<()> {
        let (_, text) = render("demo", "api", "u", &secret(&[("a", b"one"), ("b", b"two")]))?;
        let edited = text
            .as_str()
            .replace("  a: \"one\"\n", "  a: \"ONE\"\n  c: three\n")
            .replace("  b: \"two\"\n", "");
        let parsed = parse(&edited, "41")?;
        assert_eq!(values(&parsed), [("a", &b"ONE"[..]), ("c", &b"three"[..])]);
        // A plain scalar keeps its text exactly: 007 stays 007.
        let parsed = parse("data:\n  pin: 007\n  ratio: 1.10\n", "1")?;
        assert_eq!(
            values(&parsed),
            [("pin", &b"007"[..]), ("ratio", &b"1.10"[..])]
        );
        for (bad, message) in [
            ("data:\n  a: \"hunter2\"\n  a: \"hunter3\"\n", "not valid"),
            ("data:\n  \"bad key\": hunter2\n", "not a Secret key name"),
            (
                "data:\n  a: hunter2\nkeep:\n  - a\n",
                "both under data: and keep:",
            ),
            ("data:\n  a: [hunter2]\n", "not valid"),
            ("other: hunter2\n", "not valid"),
        ] {
            let error = parse(bad, "1").err().ok_or(bad)?;
            assert!(error.contains(message), "{bad}: {error}");
            assert!(!error.contains("hunter"), "{error}");
        }
        assert!(parse("data:\n", "1")?.values.is_empty());
        Ok(())
    }

    #[test]
    fn immutable_and_oversized_secrets_do_not_open() -> Result<()> {
        let mut fixed = secret(&[("a", b"1")]);
        fixed.immutable = Some(true);
        assert!(render("d", "n", "u", &fixed).is_err_and(|e| e.contains("immutable")));
        let big = vec![b'x'; MAX_DOCUMENT];
        assert!(
            render("d", "n", "u", &secret(&[("big", &big)]))
                .is_err_and(|e| e.contains("too large"))
        );
        let (_, text) = render("d", "n", "u", &secret(&[]))?;
        assert!(text.as_str().contains("# new-key"));
        assert!(parse(text.as_str(), "1")?.values.is_empty());
        Ok(())
    }
}
