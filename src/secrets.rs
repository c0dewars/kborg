//! Opt-in Secret inspection (k9s `x`): names, types and key sizes are listed;
//! one decoded value is fetched only on an explicit per-key reveal. The masked
//! YAML view ([`masked`]) shows a Secret's shape with every value replaced.
use crate::{Result, safety};
use k8s_openapi::api::core::v1::Secret;
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

/// Bytes of a binary value previewed as hex on reveal.
pub const HEX_PREVIEW_BYTES: usize = 32;

#[derive(Clone, PartialEq, Eq)]
pub enum Revealed {
    Text(String),
    /// Length and a hex preview of the first [`HEX_PREVIEW_BYTES`] bytes.
    Binary(usize, String),
    TooLarge(usize),
}
impl std::fmt::Debug for Revealed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(_) => f.write_str("Text(<hidden>)"),
            Self::Binary(n, _) => write!(f, "Binary({n})"),
            Self::TooLarge(n) => write!(f, "TooLarge({n})"),
        }
    }
}
/// A revealed value is overwritten before its memory is released.
impl Drop for Revealed {
    fn drop(&mut self) {
        match self {
            Self::Text(text) | Self::Binary(_, text) => wipe(text),
            Self::TooLarge(_) => {}
        }
    }
}

/// Overwrites a string's bytes before freeing them (best effort: copies made
/// by the HTTP client or allocator are outside KBorg's control).
pub fn wipe(text: &mut String) {
    wipe_bytes(std::mem::take(text).into_bytes());
}
fn wipe_bytes(mut bytes: Vec<u8>) {
    bytes.fill(0);
    std::hint::black_box(&bytes);
}
/// Overwrites every payload byte of a fetched Secret before it is dropped.
pub fn wipe_secret(secret: &mut Secret) {
    for value in secret.data.iter_mut().flat_map(|data| data.values_mut()) {
        wipe_bytes(std::mem::take(&mut value.0));
    }
    for value in secret
        .string_data
        .iter_mut()
        .flat_map(|data| data.values_mut())
    {
        wipe(value);
    }
    if let Some(applied) = secret
        .metadata
        .annotations
        .as_mut()
        .and_then(|annotations| annotations.get_mut(LAST_APPLIED))
    {
        wipe(applied);
    }
}

pub fn entry(
    name: &str,
    uid: &str,
    secret_type: Option<&str>,
    data: Option<&BTreeMap<String, Vec<u8>>>,
) -> SecretEntry {
    entry_sizes(
        name,
        uid,
        secret_type,
        data.into_iter()
            .flatten()
            .map(|(key, value)| (key.as_str(), value.len())),
    )
}
/// Index entry from key names and sizes, so values never need copying.
pub fn entry_sizes<'a>(
    name: &str,
    uid: &str,
    secret_type: Option<&str>,
    keys: impl Iterator<Item = (&'a str, usize)>,
) -> SecretEntry {
    SecretEntry {
        name: safety::safe_text(name, 253),
        uid: safety::safe_text(uid, 64),
        secret_type: safety::safe_text(secret_type.unwrap_or("Opaque"), 128),
        keys: keys
            .map(|(key, size)| (safety::safe_text(key, 253), size))
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
        _ => Revealed::Binary(bytes.len(), hex_preview(bytes)),
    }
}

fn hex_preview(bytes: &[u8]) -> String {
    let mut out: Vec<String> = bytes
        .iter()
        .take(HEX_PREVIEW_BYTES)
        .map(|b| format!("{b:02x}"))
        .collect();
    if bytes.len() > HEX_PREVIEW_BYTES {
        out.push("..".into());
    }
    out.join(" ")
}

pub fn validate_target(namespace: &str, name: &str) -> Result<()> {
    if namespace == "*" || !safety::valid_id(namespace) {
        return Err("Secret inspection requires one explicit namespace".into());
    }
    if name.is_empty() || name.len() > 253 || safety::safe_text(name, 253) != name {
        return Err("invalid Secret name".into());
    }
    Ok(())
}

pub fn validate_request(namespace: &str, name: &str, key: &str) -> Result<()> {
    validate_target(namespace, name)?;
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

/// Generic resource reads, lists and YAML refuse Secrets with this pointer.
pub const GENERIC_REFUSAL: &str = "Secrets are excluded from generic views; use :secrets (--allow-secret-reveal) for a masked YAML view and one-key reveal";

/// Whether a resource argument names Secrets (`secrets`, `secret`, `sec`,
/// `Secret`, `v1/secrets`, `secrets.`...).
pub fn is_secret_alias(resource: &str) -> bool {
    let lower = resource.to_ascii_lowercase();
    let base = lower.rsplit('/').next().unwrap_or_default();
    let base = base.split('.').next().unwrap_or_default();
    matches!(base, "secrets" | "secret" | "sec")
}

/// kubectl's client-side apply record embeds the whole manifest, data included.
pub const LAST_APPLIED: &str = "kubectl.kubernetes.io/last-applied-configuration";
/// Certificates summarized per PEM value in the masked view.
const MAX_CERTS: usize = 10;
/// Registries listed per docker config in the masked view.
const MAX_REGISTRIES: usize = 50;

/// A Secret projected for display. Every data value is replaced by its size
/// before this exists; only structure (names, sizes, certificate subjects and
/// expiry, registry hostnames) survives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaskedSecret {
    pub namespace: String,
    pub name: String,
    pub uid: String,
    /// The masked YAML, one display line per entry.
    pub lines: Vec<String>,
    /// Revealable `data` keys and the line that shows each one.
    pub keys: Vec<(String, usize)>,
}

fn plain_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 253
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b))
        && !key.starts_with(['-', '.', '/'])
}
fn yaml_key(key: &str) -> String {
    if plain_key(key) {
        key.to_owned()
    } else {
        quote(key)
    }
}
fn quote(text: &str) -> String {
    serde_json::to_string(&safety::safe_text(text, 1024)).unwrap_or_else(|_| "\"\"".into())
}

/// Annotation or label values that may carry a credential are replaced by
/// their size. Deliberately broad: a false positive only hides metadata.
pub fn looks_sensitive(key: &str, value: &str) -> bool {
    let key = key.to_ascii_lowercase();
    let value_lower = value.to_ascii_lowercase();
    let longest_run = value
        .split(|c: char| !(c.is_ascii_alphanumeric() || "+/=_-".contains(c)))
        .map(str::len)
        .max()
        .unwrap_or(0);
    [
        "token",
        "secret",
        "password",
        "passwd",
        "pwd",
        "credential",
        "auth",
        "apikey",
        "api-key",
        "api_key",
        "private",
        "key",
        "cert",
        "dockerconfig",
        "connection",
        "dsn",
    ]
    .iter()
    .any(|word| key.contains(word))
        || value.len() > 256
        || longest_run >= 32
        || value.contains("-----BEGIN")
        || value.starts_with("eyJ")
        || (value_lower.contains("://") && value.contains('@'))
        || ["password=", "pwd=", "token=", "secret=", "bearer "]
            .iter()
            .any(|word| value_lower.contains(word))
}

fn metadata_map(
    lines: &mut Vec<String>,
    title: &str,
    map: Option<&BTreeMap<String, String>>,
    redact: bool,
) {
    let Some(map) = map.filter(|map| !map.is_empty()) else {
        return;
    };
    lines.push(format!("  {title}:"));
    for (key, value) in map {
        if key == LAST_APPLIED {
            lines.push(format!(
                "    # {LAST_APPLIED} omitted: it embeds the Secret data"
            ));
        } else if redact && looks_sensitive(key, value) {
            lines.push(format!(
                "    {}: \"<redacted: {} bytes>\"",
                yaml_key(key),
                value.len()
            ));
        } else {
            lines.push(format!("    {}: {}", yaml_key(key), quote(value)));
        }
    }
}

/// Structure notes shown under one masked value; never contain value bytes
/// beyond parsed certificate names/dates and sanitized registry hostnames.
fn structure(secret_type: &str, key: &str, bytes: &[u8]) -> Vec<String> {
    match (secret_type, key) {
        ("kubernetes.io/tls", "tls.crt" | "ca.crt") => certificate_notes(bytes),
        ("kubernetes.io/dockerconfigjson", ".dockerconfigjson") => registry_notes(bytes, true),
        ("kubernetes.io/dockercfg", ".dockercfg") => registry_notes(bytes, false),
        _ => Vec::new(),
    }
}

fn masked_value(secret_type: &str, key: &str, size: usize) -> String {
    if secret_type == "kubernetes.io/service-account-token" && key == "token" {
        "\"<hidden: service account token present>\"".into()
    } else {
        format!("\"<hidden: {size} bytes>\"")
    }
}

/// Renders the masked YAML of a Secret. The caller still owns (and should
/// [`wipe_secret`]) the payload; nothing returned references it.
pub fn masked(secret: &Secret) -> MaskedSecret {
    let meta = &secret.metadata;
    let secret_type = secret.type_.as_deref().unwrap_or("Opaque");
    let mut lines = vec![
        "apiVersion: v1".to_owned(),
        "kind: Secret".to_owned(),
        "metadata:".to_owned(),
    ];
    for (field, value) in [
        ("name", meta.name.as_deref()),
        ("namespace", meta.namespace.as_deref()),
        ("uid", meta.uid.as_deref()),
        ("resourceVersion", meta.resource_version.as_deref()),
    ] {
        if let Some(value) = value {
            lines.push(format!("  {field}: {}", quote(value)));
        }
    }
    if let Some(created) = &meta.creation_timestamp {
        lines.push(format!(
            "  creationTimestamp: {}",
            quote(&created.0.to_string())
        ));
    }
    metadata_map(&mut lines, "labels", meta.labels.as_ref(), true);
    metadata_map(&mut lines, "annotations", meta.annotations.as_ref(), true);
    lines.push(format!("type: {}", quote(secret_type)));
    if let Some(immutable) = secret.immutable {
        lines.push(format!("immutable: {immutable}"));
    }
    let mut keys = Vec::new();
    if let Some(data) = secret.data.as_ref().filter(|data| !data.is_empty()) {
        lines.push("data:".into());
        for (key, value) in data {
            let shown = safety::safe_text(key, 253);
            keys.push((shown.clone(), lines.len()));
            lines.push(format!(
                "  {}: {}",
                yaml_key(&shown),
                masked_value(secret_type, key, value.0.len())
            ));
            lines.extend(
                structure(secret_type, key, &value.0)
                    .into_iter()
                    .map(|note| format!("    # {note}")),
            );
        }
    } else {
        lines.push("data: {}".into());
    }
    if let Some(data) = secret.string_data.as_ref().filter(|data| !data.is_empty()) {
        lines.push("stringData:  # write-only; not revealable here".into());
        for (key, value) in data {
            lines.push(format!(
                "  {}: {}",
                yaml_key(key),
                masked_value(secret_type, key, value.len())
            ));
        }
    }
    MaskedSecret {
        namespace: safety::safe_text(meta.namespace.as_deref().unwrap_or(""), 63),
        name: safety::safe_text(meta.name.as_deref().unwrap_or(""), 253),
        uid: safety::safe_text(meta.uid.as_deref().unwrap_or(""), 64),
        lines,
        keys,
    }
}

/// Hostnames of a docker config; usernames, passwords and auth are skipped.
fn registry_notes(bytes: &[u8], nested: bool) -> Vec<String> {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return vec!["not valid docker config JSON".into()];
    };
    let auths = if nested { &value["auths"] } else { &value };
    let Some(auths) = auths.as_object() else {
        return vec!["no registries listed".into()];
    };
    let mut notes: Vec<String> = auths
        .keys()
        .take(MAX_REGISTRIES)
        .map(|key| format!("registry: {}", registry_host(key)))
        .collect();
    if auths.len() > MAX_REGISTRIES {
        notes.push(format!(
            "... {} more registries",
            auths.len() - MAX_REGISTRIES
        ));
    }
    if notes.is_empty() {
        notes.push("no registries listed".into());
    }
    notes
}

/// The host[:port] of a registry key; scheme, userinfo and path are dropped.
pub fn registry_host(key: &str) -> String {
    let rest = key.split_once("://").map_or(key, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if host.is_empty()
        || host.len() > 253
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-.:[]_".contains(&b))
    {
        "<unrecognized registry key>".into()
    } else {
        host.to_owned()
    }
}

fn certificate_notes(bytes: &[u8]) -> Vec<String> {
    use base64::Engine;
    let Ok(text) = std::str::from_utf8(bytes) else {
        return vec!["not a PEM certificate".into()];
    };
    let mut notes = Vec::new();
    let mut blocks = text.split("-----BEGIN CERTIFICATE-----").skip(1);
    for (index, block) in blocks.by_ref().take(MAX_CERTS).enumerate() {
        let body: String = block
            .split("-----END CERTIFICATE-----")
            .next()
            .unwrap_or_default()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let summary = base64::engine::general_purpose::STANDARD
            .decode(body.as_bytes())
            .ok()
            .and_then(|der| certificate_summary(&der));
        notes.push(match summary {
            Some((subject, expiry)) => {
                format!(
                    "certificate {}: subject {subject}; expires {expiry}",
                    index + 1
                )
            }
            None => format!("certificate {}: unparseable", index + 1),
        });
    }
    let more = blocks.count();
    if more > 0 {
        notes.push(format!("... {more} more certificates"));
    }
    if notes.is_empty() {
        notes.push("no PEM certificate found".into());
    }
    notes
}

/// One DER element: tag, contents, remainder.
fn der(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let count = usize::from(first & 0x7f);
        if count == 0 || count > 4 {
            return None;
        }
        let digits = rest.get(..count)?;
        (
            digits
                .iter()
                .fold(0usize, |len, b| (len << 8) | usize::from(*b)),
            rest.get(count..)?,
        )
    };
    Some((tag, rest.get(..len)?, rest.get(len..)?))
}

/// Subject (CN/O/OU) and notAfter of an X.509 certificate, from DER.
fn certificate_summary(bytes: &[u8]) -> Option<(String, String)> {
    let (0x30, certificate, _) = der(bytes)? else {
        return None;
    };
    let (0x30, mut tbs, _) = der(certificate)? else {
        return None;
    };
    let mut next = || -> Option<(u8, &[u8])> {
        let (tag, content, rest) = der(tbs)?;
        tbs = rest;
        Some((tag, content))
    };
    // [0] version (optional), serial, signature algorithm, issuer, validity, subject.
    if next()?.0 == 0xa0 {
        let _serial = next()?;
    }
    let _signature = next()?;
    let _issuer = next()?;
    let (0x30, validity) = next()? else {
        return None;
    };
    let (0x30, subject) = next()? else {
        return None;
    };
    let (_, _, rest) = der(validity)?;
    let (time_tag, not_after, _) = der(rest)?;
    Some((name(subject), time(time_tag, not_after)?))
}

fn name(mut subject: &[u8]) -> String {
    let mut parts = Vec::new();
    while let Some((0x31, set, rest)) = der(subject) {
        subject = rest;
        let Some((0x30, pair, _)) = der(set) else {
            continue;
        };
        let Some((0x06, oid, rest)) = der(pair) else {
            continue;
        };
        let Some((_, value, _)) = der(rest) else {
            continue;
        };
        let label = match oid {
            [0x55, 0x04, 0x03] => "CN",
            [0x55, 0x04, 0x0a] => "O",
            [0x55, 0x04, 0x0b] => "OU",
            _ => continue,
        };
        parts.push(format!(
            "{label}={}",
            safety::safe_text(&String::from_utf8_lossy(value), 128)
        ));
    }
    if parts.is_empty() {
        "(no CN/O)".into()
    } else {
        parts.join(", ")
    }
}

fn time(tag: u8, value: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(value).ok()?;
    if !text.ends_with('Z') || !text[..text.len() - 1].bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let digits = &text[..text.len() - 1];
    let (year, rest) = match (tag, digits.len()) {
        (0x17, 12) => {
            let yy: u32 = digits.get(..2)?.parse().ok()?;
            (
                if yy < 50 { 2000 + yy } else { 1900 + yy },
                digits.get(2..)?,
            )
        }
        (0x18, 14) => (digits.get(..4)?.parse().ok()?, digits.get(4..)?),
        _ => return None,
    };
    let part = |range: std::ops::Range<usize>| rest.get(range);
    Some(format!(
        "{year:04}-{}-{} {}:{}:{} UTC",
        part(0..2)?,
        part(2..4)?,
        part(4..6)?,
        part(6..8)?,
        part(8..10)?
    ))
}

/// Offline sample Secret for the demo masked view, matching [`demo_data`].
pub fn demo_secret(namespace: &str) -> Secret {
    let mut secret = Secret {
        type_: Some("Opaque".into()),
        data: Some(
            demo_data()
                .into_iter()
                .map(|(key, value)| (key, k8s_openapi::ByteString(value)))
                .collect(),
        ),
        ..Default::default()
    };
    secret.metadata.name = Some("api-credentials".into());
    secret.metadata.namespace = Some(namespace.into());
    secret.metadata.uid = Some("demo-secret-uid".into());
    secret.metadata.resource_version = Some("1".into());
    secret.metadata.labels = Some(BTreeMap::from([("app".into(), "api".into())]));
    secret
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
        assert_eq!(
            reveal(b"a\x1b[2Jb"),
            Revealed::Binary(6, "61 1b 5b 32 4a 62".into())
        );
        assert_eq!(
            reveal(&[0, 159, 146]),
            Revealed::Binary(3, "00 9f 92".into())
        );
        match &reveal(&[7; 40]) {
            Revealed::Binary(40, hex) => assert!(hex.ends_with("07 ..") && !hex.contains('\x07')),
            other => panic!("{other:?}"),
        }
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
    const CERT: &str = "-----BEGIN CERTIFICATE-----\nMIIBlDCCATqgAwIBAgIUPE+c36bXJ4Thf9NXKJN7NA73MzowCgYIKoZIzj0EAwIw\nMDETMBEGA1UECgwKS0JvcmcgVGVzdDEZMBcGA1UEAwwQYXBpLmV4YW1wbGUudGVz\ndDAeFw0yNjEwMjUxNzI4MDZaFw0zNjEwMjIxNzI4MDZaMDAxEzARBgNVBAoMCktC\nb3JnIFRlc3QxGTAXBgNVBAMMEGFwaS5leGFtcGxlLnRlc3QwWTATBgcqhkjOPQIB\nBggqhkjOPQMBBwNCAAQa0i1SelD711B2rf2BHB/MuWg1vNqpcDYyFF0EwuXRCndt\n+qHSDsak0Lc9NQ4jxRxOIkAiq8GC1t1Box8vZMulozIwMDAdBgNVHQ4EFgQUM3xe\nEi55wuPme+LTFicJ1dvJyBwwDwYDVR0TAQH/BAUwAwEB/zAKBggqhkjOPQQDAgNI\nADBFAiEAr7FEe/0C1l4enVIuXtUNMGGneLVascCsqjuvjq9HxIMCIDnhNZ+qbgTu\ngJHKjInEU+X0KlDdm9YY8YBZnzVv73ZQ\n-----END CERTIFICATE-----\n";

    fn fixture(secret_type: &str, data: &[(&str, &str)], annotations: &[(&str, &str)]) -> Secret {
        let mut secret = Secret {
            type_: Some(secret_type.into()),
            immutable: Some(true),
            data: Some(
                data.iter()
                    .map(|(k, v)| {
                        (
                            (*k).to_owned(),
                            k8s_openapi::ByteString(v.as_bytes().to_vec()),
                        )
                    })
                    .collect(),
            ),
            ..Default::default()
        };
        secret.metadata.name = Some("fixture".into());
        secret.metadata.namespace = Some("demo".into());
        secret.metadata.uid = Some("uid-fixture".into());
        secret.metadata.resource_version = Some("42".into());
        secret.metadata.labels = Some(BTreeMap::from([("app".into(), "api".into())]));
        secret.metadata.annotations = Some(
            annotations
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        );
        secret
    }

    /// Every non-trivial line of every value must be absent from the view.
    fn assert_masked(secret: &Secret, extra: &[&str]) -> String {
        let doc = masked(secret);
        let text = doc.lines.join("\n");
        let values = secret
            .data
            .iter()
            .flatten()
            .map(|(_, v)| String::from_utf8_lossy(&v.0).into_owned());
        for value in values.chain(extra.iter().map(|v| (*v).to_owned())) {
            for line in value.lines().filter(|line| line.trim().len() >= 6) {
                assert!(!text.contains(line.trim()), "leaked {line:?} in\n{text}");
            }
        }
        text
    }

    #[test]
    fn opaque_values_and_last_applied_never_reach_the_masked_yaml() {
        let applied = r#"{"apiVersion":"v1","kind":"Secret","data":{"password":"aHVudGVyMi1vcGFxdWU="},"stringData":{"note":"hunter2-opaque-value"}}"#;
        let secret = fixture(
            "Opaque",
            &[
                ("password", "hunter2-opaque-value"),
                ("api.key", "ak_live_1234567890abcdef"),
            ],
            &[
                (LAST_APPLIED, applied),
                ("example.com/db-password", "anno-pass-998877"),
                ("example.com/url", "postgres://user:pw-in-url@db:5432/app"),
                (
                    "example.com/blob",
                    "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVo0NTY3ODk=",
                ),
                ("description", "plain note"),
            ],
        );
        let text = assert_masked(
            &secret,
            &[
                applied,
                "aHVudGVyMi1vcGFxdWU=",
                "anno-pass-998877",
                "pw-in-url",
                "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVo0NTY3ODk=",
            ],
        );
        assert!(text.contains("password: \"<hidden: 20 bytes>\""), "{text}");
        assert!(text.contains("api.key: \"<hidden: 24 bytes>\""));
        assert!(text.contains("description: \"plain note\""));
        assert!(text.contains("example.com/db-password: \"<redacted: 16 bytes>\""));
        assert!(text.contains("omitted: it embeds the Secret data"));
        assert!(!text.contains("\"data\""));
        for field in [
            "apiVersion: v1",
            "kind: Secret",
            "uid: \"uid-fixture\"",
            "resourceVersion: \"42\"",
            "app: \"api\"",
            "type: \"Opaque\"",
            "immutable: true",
        ] {
            assert!(text.contains(field), "{field} missing in\n{text}");
        }
        let doc = masked(&secret);
        assert_eq!(doc.keys.len(), 2);
        assert!(doc.lines[doc.keys[0].1].contains("api.key"));
    }

    #[test]
    fn tls_shows_certificate_subject_and_expiry_only() {
        let key = "-----BEGIN PRIVATE KEY-----\nMIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgPRIVATEKEYBYTES\n-----END PRIVATE KEY-----\n";
        let secret = fixture(
            "kubernetes.io/tls",
            &[("tls.crt", CERT), ("tls.key", key)],
            &[],
        );
        let text = assert_masked(&secret, &["-----BEGIN"]);
        assert!(
            text.contains("# certificate 1: subject O=KBorg Test, CN=api.example.test; expires 2036-10-22 17:28:06 UTC"),
            "{text}"
        );
        assert!(text.contains(&format!("tls.key: \"<hidden: {} bytes>\"", key.len())));
        let broken = fixture(
            "kubernetes.io/tls",
            &[(
                "tls.crt",
                "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----",
            )],
            &[],
        );
        assert!(
            masked(&broken)
                .lines
                .join("\n")
                .contains("certificate 1: unparseable")
        );
    }

    #[test]
    fn docker_config_lists_hostnames_without_usernames_or_auth() {
        let config = r#"{"auths":{"https://robot-user:robot-pass@registry.example.com/v2/":{"username":"robot-user","password":"robot-pass-123","auth":"cm9ib3QtdXNlcjpyb2JvdC1wYXNzLTEyMw=="},"ghcr.io":{"auth":"Z2hjci11c2VyOmdoY3ItdG9rZW4="}}}"#;
        let secret = fixture(
            "kubernetes.io/dockerconfigjson",
            &[(".dockerconfigjson", config)],
            &[],
        );
        let text = assert_masked(
            &secret,
            &["robot-user", "robot-pass", "cm9ib3Qt", "Z2hjci11"],
        );
        assert!(text.contains("# registry: registry.example.com"), "{text}");
        assert!(text.contains("# registry: ghcr.io"));
        assert_eq!(
            registry_host("evil\u{1b}[2J.io"),
            "<unrecognized registry key>"
        );
    }

    #[test]
    fn basic_auth_and_service_account_tokens_stay_masked() {
        let basic = fixture(
            "kubernetes.io/basic-auth",
            &[
                ("username", "admin-user-name"),
                ("password", "basic-pass-xyz"),
            ],
            &[],
        );
        assert_masked(&basic, &[]);
        let token = "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJzeXN0ZW0ifQ.signature-bytes";
        let sa = fixture(
            "kubernetes.io/service-account-token",
            &[
                ("token", token),
                ("ca.crt", CERT),
                ("namespace", "sa-namespace-value"),
            ],
            &[("kubernetes.io/service-account.name", "builder")],
        );
        let text = assert_masked(&sa, &["eyJhbGci"]);
        assert!(
            text.contains("token: \"<hidden: service account token present>\""),
            "{text}"
        );
        assert!(!text.contains(&format!("{} bytes", token.len())));
        assert!(text.contains("kubernetes.io/service-account.name: \"builder\""));
        assert!(!text.contains("certificate 1"));
    }

    #[test]
    fn wiping_clears_payload_bytes() {
        let mut secret = fixture(
            "Opaque",
            &[("password", "hunter2")],
            &[(LAST_APPLIED, "{\"data\":1}")],
        );
        wipe_secret(&mut secret);
        assert!(secret.data.iter().flatten().all(|(_, v)| v.0.is_empty()));
        assert_eq!(
            secret
                .metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get(LAST_APPLIED))
                .map(String::len),
            Some(0)
        );
        let mut text = String::from("hunter2");
        wipe(&mut text);
        assert!(text.is_empty());
    }
}
