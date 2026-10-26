//! Fuzz entry points for the parsers that read untrusted input.
//!
//! Each target takes arbitrary bytes, feeds one parser and panics when an
//! invariant breaks, not only when the parser crashes. The cargo-fuzz
//! binaries in `fuzz/` call these under `--cfg fuzzing` (nightly); the
//! property tests at the bottom run every target on stable inside
//! `cargo test`, with deterministic mutations of the seed corpus in
//! `fuzz/seeds/TARGET/`, so the same checks always run.
use crate::{
    aliases, archive, extensions, gateway, helm, hotkeys, keymap, manifest_apply, navigation_views,
    safety, secrets, ui_config, views,
};
use serde_json::Value;
use std::{fs, path::Path};

/// A fuzz target: arbitrary bytes in, panic on a broken invariant.
pub type Target = fn(&[u8]);

/// Every target, by the name of its cargo-fuzz binary and seed directory.
pub const TARGETS: &[(&str, Target)] = &[
    ("sanitizer", sanitizer),
    ("kubeconfig", kubeconfig),
    ("manifest", manifest),
    ("archive", archive_extract),
    ("k9s_views", k9s_views),
    ("k9s_hotkeys", k9s_hotkeys),
    ("k9s_aliases", k9s_aliases),
    ("k9s_plugins", k9s_plugins),
    ("keymap", keymap_yaml),
    ("ui_config", ui_yaml),
    ("secret_masked", secret_masked),
    ("helm_storage", helm_storage),
];

/// Bidi, zero-width and invisible format characters that must never be painted.
fn format_control(c: char) -> bool {
    matches!(c,
        '\u{00ad}' | '\u{061c}' | '\u{180e}' | '\u{200b}'..='\u{200f}' |
        '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}' |
        '\u{fff9}'..='\u{fffb}' | '\u{e0000}'..='\u{e007f}')
}
fn paintable(c: char) -> bool {
    !c.is_control() && !format_control(c)
}
/// Single-line text a terminal can paint without interpreting anything.
fn plain(text: &str) -> bool {
    text.chars().all(paintable)
}
fn plain_lines(text: &str) -> bool {
    text.chars().all(|c| c == '\n' || paintable(c))
}
/// Errors are painted in the status line or printed; they may quote input
/// only after sanitizing it.
fn check_error(error: &str) {
    assert!(
        plain_lines(error),
        "error message carries terminal controls: {error:?}"
    );
}
fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut rest = haystack.chars();
    needle.chars().all(|c| rest.any(|h| h == c))
}

const REVIEW_NOTICE: &str =
    "Escaped review: \\\\ is a literal backslash; \\u{...} is a Unicode code point.\n\n";

/// Inverse of `safety::review_display`, or `None` if the output is ambiguous.
fn unescape_review(shown: &str) -> Option<String> {
    let Some(body) = shown.strip_prefix(REVIEW_NOTICE) else {
        return (!shown.contains('\\')).then(|| shown.to_owned());
    };
    let mut out = String::new();
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            '\\' => out.push('\\'),
            'u' => {
                if chars.next()? != '{' {
                    return None;
                }
                let hex: String = chars.by_ref().take_while(|c| *c != '}').collect();
                out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
            }
            _ => return None,
        }
    }
    Some(out)
}

/// `safe_text`, `safe_lines`, `review_text` and `review_display`: output has
/// no controls, is bounded, idempotent, and review output is exact or refused.
pub fn sanitizer(data: &[u8]) {
    let (limit, rest) = data.split_first().map_or((0, data), |(b, r)| (*b, r));
    let input = String::from_utf8_lossy(rest);
    let max = usize::from(limit) * 8;

    let text = safety::safe_text(&input, max);
    assert!(plain(&text), "safe_text leaked a control: {text:?}");
    assert!(text.chars().count() <= max);
    assert!(is_subsequence(&text, &input), "safe_text invented text");
    assert_eq!(
        safety::safe_text(&text, max),
        text,
        "safe_text not idempotent"
    );

    let lines = safety::safe_lines(&input, max);
    assert!(
        plain_lines(&lines),
        "safe_lines leaked a control: {lines:?}"
    );
    assert!(lines.chars().count() <= max);
    assert!(lines.split('\n').all(|line| line.chars().count() <= 4096));
    assert_eq!(
        safety::safe_lines(&lines, max),
        lines,
        "safe_lines not idempotent"
    );

    match safety::review_text(&input, max) {
        Ok(shown) => assert_eq!(shown, input, "review_text changed approved text"),
        Err(error) => {
            assert_ne!(lines, input, "review_text refused clean text");
            check_error(&error);
        }
    }
    match safety::review_display(&input) {
        Ok(shown) => {
            assert!(shown.is_ascii(), "review display is not ASCII");
            assert!(plain_lines(&shown), "review display leaked a control");
            assert_eq!(
                unescape_review(&shown).as_deref(),
                Some(input.as_ref()),
                "review display is not reversible"
            );
        }
        Err(error) => {
            assert!(input.len() > 1024 * 1024 / 12, "review display refused");
            check_error(&error);
        }
    }
}

/// Kubeconfig text: names bounded, plain and unique across documents;
/// every file reference resolved against the kubeconfig's directory.
pub fn kubeconfig(data: &[u8]) {
    let text = String::from_utf8_lossy(data);
    let parent = std::env::temp_dir().join("kborg-fuzz-kube");
    let config = match gateway::parse_config_text(&text, &parent) {
        Ok(config) => config,
        Err(error) => return check_error(&error),
    };
    for names in [
        config
            .contexts
            .iter()
            .map(|v| v.name.as_str())
            .collect::<Vec<_>>(),
        config.clusters.iter().map(|v| v.name.as_str()).collect(),
        config.auth_infos.iter().map(|v| v.name.as_str()).collect(),
    ] {
        let mut unique = std::collections::HashSet::new();
        for name in names {
            assert!(!name.is_empty() && name.len() <= 253 && plain(name));
            assert!(unique.insert(name), "duplicate kubeconfig name accepted");
        }
    }
    let mut files = Vec::new();
    for cluster in config.clusters.iter().filter_map(|c| c.cluster.as_ref()) {
        files.extend(cluster.certificate_authority.as_deref());
    }
    for user in config
        .auth_infos
        .iter()
        .filter_map(|u| u.auth_info.as_ref())
    {
        files.extend(user.client_certificate.as_deref());
        files.extend(user.client_key.as_deref());
        files.extend(user.token_file.as_deref());
    }
    for file in files {
        assert!(Path::new(file).is_absolute(), "unresolved file {file:?}");
    }
}

fn assert_no_secret(document: &Value) {
    let kind = document["kind"].as_str().unwrap_or("");
    assert!(
        !kind.eq_ignore_ascii_case("secret"),
        "a Secret document survived parsing"
    );
    if kind == "OmittedSecret" {
        assert_eq!(
            document,
            &serde_json::json!({"apiVersion":"v1","kind":"OmittedSecret","notice":"Secret document omitted"}),
            "an omitted Secret kept data"
        );
    } else if kind.ends_with("List") {
        for item in document["items"].as_array().into_iter().flatten() {
            assert_no_secret(item);
        }
    }
}

/// Local manifest for apply: Secrets never survive, flattening is bounded and
/// each body keeps the identity the file names (never a silent substitute).
pub fn manifest(data: &[u8]) {
    let text = String::from_utf8_lossy(data);
    let documents = match navigation_views::parse_manifest_text(&text) {
        Ok(documents) => documents,
        Err(error) => return check_error(&error),
    };
    documents.iter().for_each(assert_no_secret);
    let flat = match manifest_apply::flatten(documents) {
        Ok(flat) => flat,
        Err(error) => return check_error(&error),
    };
    assert!(!flat.is_empty() && flat.len() <= manifest_apply::MAX_OBJECTS);
    let kinds = gateway::kinds();
    for document in &flat {
        let kind_name = document["kind"].as_str().unwrap_or("");
        assert!(kind_name != "OmittedSecret" && !kind_name.ends_with("List"));
        let kind = match manifest_apply::kind_for(&kinds, document) {
            Ok(kind) => kind,
            Err(error) => {
                check_error(&error);
                continue;
            }
        };
        let (namespace, name, body) = match manifest_apply::body(document, kind, "fuzz") {
            Ok(planned) => planned,
            Err(error) => {
                check_error(&error);
                continue;
            }
        };
        assert_ne!(kind.kind, "Secret");
        assert!(safety::valid_id(&namespace));
        assert!(!["kube-system", "kube-public", "kube-node-lease"].contains(&namespace.as_str()));
        assert!(!name.is_empty() && name.len() <= 253);
        match document["metadata"].get("namespace") {
            Some(Value::String(declared)) => assert_eq!(declared, &namespace),
            None | Some(Value::Null) => assert_eq!(namespace, "fuzz"),
            Some(other) => panic!("non-string namespace {other} accepted"),
        }
        assert_eq!(body["metadata"]["namespace"], Value::String(namespace));
        assert_eq!(body["metadata"]["name"], Value::String(name));
        assert!(body.get("status").is_none());
        for field in ["uid", "resourceVersion", "generation", "managedFields"] {
            assert!(body["metadata"].get(field).is_none());
        }
    }
}

/// Strict tar extraction: nothing lands outside the new directory, no links
/// are created, files are private, and a failure leaves nothing behind.
pub fn archive_extract(data: &[u8]) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let sandbox = std::env::temp_dir().join(format!(
        "kborg-fuzz-archive-{}-{sequence}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&sandbox);
    if fs::create_dir_all(&sandbox).is_err() {
        return;
    }
    let target = sandbox.join("out");
    let result = archive::extract(data, "top", &target);
    let mut siblings: Vec<String> = fs::read_dir(&sandbox)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    siblings.sort();
    match result {
        Ok(done) => {
            assert_eq!(siblings, ["out"], "extraction wrote outside its directory");
            let (mut files, mut bytes) = (0, 0);
            let mut stack = vec![target.clone()];
            while let Some(directory) = stack.pop() {
                for entry in fs::read_dir(&directory).into_iter().flatten().flatten() {
                    let path = entry.path();
                    let Ok(meta) = fs::symlink_metadata(&path) else {
                        continue;
                    };
                    assert!(!meta.file_type().is_symlink(), "extraction made a link");
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        assert_eq!(meta.permissions().mode() & 0o7077, 0, "not private");
                    }
                    if meta.is_dir() {
                        stack.push(path);
                    } else {
                        assert!(meta.is_file(), "extraction made a special file");
                        files += 1;
                        bytes += meta.len();
                    }
                }
            }
            assert_eq!((files, bytes), (done.files, done.bytes));
        }
        Err(error) => {
            check_error(&error);
            assert!(siblings.is_empty(), "failed extraction left {siblings:?}");
        }
    }
    let _ = fs::remove_dir_all(&sandbox);
}

/// k9s views.yaml: bounded views and columns, safe titles, and column paths
/// that evaluate against any object without panicking.
pub fn k9s_views(data: &[u8]) {
    let parsed = match views::parse(&String::from_utf8_lossy(data)) {
        Ok(parsed) => parsed,
        Err(error) => return check_error(&error),
    };
    assert!(parsed.len() <= 128);
    let sample = serde_json::json!({"metadata":{"name":"web","labels":{"app":"web"}},
        "spec":{"containers":[{"name":"a","image":"x"},{"name":"b"}]},"status":{"phase":"Running"}});
    for view in parsed.values() {
        assert!(view.columns.len() <= 24);
        for column in &view.columns {
            assert!(!column.title.is_empty() && column.title.len() <= 32);
            assert!(column.title.bytes().all(|b| b.is_ascii_graphic()));
            if let Some(path) = &column.path {
                assert!(!path.is_empty() && path.len() <= 16);
                let _ = views::eval(path, &sample);
            }
        }
    }
}

/// k9s hotkeys.yaml: unique shortcuts that round-trip, plain bounded commands.
pub fn k9s_hotkeys(data: &[u8]) {
    let parsed = match hotkeys::parse(&String::from_utf8_lossy(data)) {
        Ok(parsed) => parsed,
        Err(error) => return check_error(&error),
    };
    assert!(parsed.len() <= 64);
    for (index, hotkey) in parsed.iter().enumerate() {
        assert!(
            parsed[..index]
                .iter()
                .all(|o| o.shortcut != hotkey.shortcut)
        );
        assert_eq!(
            hotkeys::Shortcut::parse(&hotkey.shortcut.to_string()),
            Some(hotkey.shortcut)
        );
        assert!(!hotkey.command.is_empty() && hotkey.command.len() <= 2048);
        assert!(plain(&hotkey.command) && plain(&hotkey.description));
        assert!(hotkey.description.len() <= 120);
    }
}

/// k9s aliases.yaml: every alias expands, within the command limit, to plain text.
pub fn k9s_aliases(data: &[u8]) {
    let text = String::from_utf8_lossy(data);
    let parsed = match aliases::parse(&text) {
        Ok(parsed) => parsed,
        Err(error) => return check_error(&error),
    };
    assert!(parsed.len() <= 256);
    let probe = text.lines().next().unwrap_or("");
    for command in parsed.keys().map(String::as_str).chain([probe]) {
        match aliases::expand(command, &parsed) {
            Ok(expanded) => {
                assert!(expanded.len() <= 2048);
                if parsed.contains_key(command) {
                    assert!(plain(&expanded), "alias expanded to controls");
                }
            }
            Err(error) => check_error(&error),
        }
    }
}

/// Splits a line printed for a POSIX shell into words, or `None` when any
/// character could be interpreted by the shell (unquoted metacharacters).
fn shell_words(line: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' => words.extend(word.take()),
            '\'' => {
                let current = word.get_or_insert_with(String::new);
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => current.push(c),
                    }
                }
            }
            '\\' => {
                let escaped = chars.next()?;
                word.get_or_insert_with(String::new).push(escaped);
            }
            c if c.is_ascii_alphanumeric() || "_./=:@%+,-".contains(c) => {
                word.get_or_insert_with(String::new).push(c);
            }
            _ => return None,
        }
    }
    words.extend(word);
    Some(words)
}

/// k9s plugins.yaml import report: plain text, and every suggested register
/// line is exactly one quoted argv for a POSIX shell.
pub fn k9s_plugins(data: &[u8]) {
    let resolve = |name: &str| Some(std::path::PathBuf::from(format!("/opt/my tools/{name}")));
    let report =
        match extensions::import_k9s(&String::from_utf8_lossy(data), "my plugins.yaml", &resolve) {
            Ok(report) => report,
            Err(error) => return check_error(&error),
        };
    assert!(plain_lines(&report), "import report carries controls");
    for line in report.lines() {
        let Some(command) = line.strip_prefix("    kborg extensions register-k9s ") else {
            continue;
        };
        let words = shell_words(command)
            .unwrap_or_else(|| panic!("suggestion is not shell-safe: {line:?}"));
        assert_eq!(words.len(), 4, "{line:?}");
        assert_eq!(words[0], "my plugins.yaml");
        assert!(safety::valid_id(&words[1]));
        assert_eq!(words[2], "--tool");
        assert!(words[3].starts_with('/'), "{line:?}");
    }
}

/// keymap.yaml: every effective key round-trips through its display name.
pub fn keymap_yaml(data: &[u8]) {
    let parsed = match keymap::parse(&String::from_utf8_lossy(data)) {
        Ok(parsed) => parsed,
        Err(error) => return check_error(&error),
    };
    for scope in keymap::Scope::ALL {
        for (_, keys, _) in parsed.entries(scope) {
            assert!(keys.len() <= 9);
            for key in keys {
                assert_eq!(keymap::Key::parse(&key.to_string()), Some(*key));
            }
        }
    }
}

/// ui.yaml: accepted or refused with a plain message, never a panic.
pub fn ui_yaml(data: &[u8]) {
    if let Err(error) = ui_config::parse(&String::from_utf8_lossy(data)) {
        check_error(&error);
    }
}

const SECRET_TYPES: [&str; 5] = [
    "Opaque",
    "kubernetes.io/tls",
    "kubernetes.io/dockerconfigjson",
    "kubernetes.io/dockercfg",
    "kubernetes.io/service-account-token",
];

/// Keys whose value the masked view parses for structure notes.
fn structured(secret_type: &str, key: &str) -> bool {
    matches!(
        (secret_type, key),
        ("kubernetes.io/tls", "tls.crt" | "ca.crt")
            | ("kubernetes.io/dockerconfigjson", ".dockerconfigjson")
            | ("kubernetes.io/dockercfg", ".dockercfg")
    )
}

/// A Secret from fuzz bytes: fields split on 0xff, each led by a tag byte;
/// key and value split on 0xfe.
fn secret_from(data: &[u8]) -> k8s_openapi::api::core::v1::Secret {
    use k8s_openapi::{ByteString, api::core::v1::Secret};
    use std::collections::BTreeMap;
    let mut fields = data.split(|b| *b == 0xff);
    let selector = fields.next().and_then(|f| f.first()).copied().unwrap_or(0);
    let mut secret = Secret {
        type_: Some(SECRET_TYPES[usize::from(selector) % SECRET_TYPES.len()].into()),
        ..Default::default()
    };
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    for field in fields {
        let Some((tag, rest)) = field.split_first() else {
            continue;
        };
        let (key, value) = match rest.iter().position(|b| *b == 0xfe) {
            Some(at) => (&rest[..at], &rest[at + 1..]),
            None => (rest, &[][..]),
        };
        let map = |slot: &mut Option<BTreeMap<String, String>>| {
            slot.get_or_insert_with(BTreeMap::new)
                .insert(text(key), text(value));
        };
        match tag % 8 {
            0..=3 => {
                secret
                    .data
                    .get_or_insert_with(BTreeMap::new)
                    .insert(text(key), ByteString(value.to_vec()));
            }
            4 => {
                secret
                    .string_data
                    .get_or_insert_with(BTreeMap::new)
                    .insert(text(key), text(value));
            }
            5 => map(&mut secret.metadata.labels),
            6 => map(&mut secret.metadata.annotations),
            _ => {
                secret.metadata.name = Some(text(key));
                secret.metadata.namespace = Some(text(value));
            }
        }
    }
    secret
}

/// Same byte length, always classified as sensitive when long enough to be.
fn sensitive_filler(len: usize) -> String {
    if len >= 3 {
        format!("eyJ{}", "x".repeat(len - 3))
    } else {
        "x".repeat(len)
    }
}

/// The masked Secret YAML never depends on a value beyond its length (except
/// the documented certificate/registry notes), and every line is plain.
pub fn secret_masked(data: &[u8]) {
    use k8s_openapi::ByteString;
    let secret = secret_from(data);
    let secret_type = secret.type_.clone().unwrap_or_default();
    let masked = secrets::masked(&secret);
    assert!(
        masked.lines.iter().all(|line| plain(line)),
        "masked line has controls"
    );
    for (key, line) in &masked.keys {
        assert!(plain(key) && *line < masked.lines.len());
    }
    // Replace everything the view must not reveal with same-length filler.
    let mut blank = secret.clone();
    for (key, value) in blank.data.iter_mut().flatten() {
        if !structured(&secret_type, key) {
            let filler = if value.0.iter().all(|b| *b == b'x') {
                b'y'
            } else {
                b'x'
            };
            *value = ByteString(vec![filler; value.0.len()]);
        }
    }
    for value in blank.string_data.iter_mut().flat_map(|m| m.values_mut()) {
        *value = "x".repeat(value.len());
    }
    for map in [&mut blank.metadata.labels, &mut blank.metadata.annotations] {
        for (key, value) in map.iter_mut().flatten() {
            if key == secrets::LAST_APPLIED {
                value.clear();
            } else if secrets::looks_sensitive(key, value)
                && secrets::looks_sensitive(key, &sensitive_filler(value.len()))
            {
                *value = sensitive_filler(value.len());
            }
        }
    }
    assert_eq!(
        secrets::masked(&blank).lines,
        masked.lines,
        "masked Secret output depends on a hidden value"
    );
    // Structure notes use fixed shapes only.
    for line in masked.lines.iter().filter_map(|l| l.strip_prefix("    # ")) {
        let allowed = [
            "not a PEM certificate",
            "no PEM certificate found",
            "not valid docker config JSON",
            "no registries listed",
        ]
        .contains(&line)
            || line.starts_with("certificate ")
            || line.starts_with("... ")
            || line.starts_with(secrets::LAST_APPLIED)
            || line.strip_prefix("registry: ").is_some_and(|host| {
                host == "<unrecognized registry key>"
                    || host
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-.:[]_".contains(&b))
            });
        assert!(allowed, "unexpected structure note {line:?}");
    }
    for (key, value) in secret.data.iter().flatten() {
        let host = secrets::registry_host(key);
        assert!(
            !host.contains('@') && plain(&host),
            "registry userinfo shown"
        );
        match &secrets::reveal(&value.0) {
            secrets::Revealed::Text(text) => assert!(plain_lines(text)),
            secrets::Revealed::Binary(size, hex) => {
                assert_eq!(*size, value.0.len());
                assert!(
                    hex.bytes()
                        .all(|b| b.is_ascii_hexdigit() || b" .".contains(&b))
                );
            }
            secrets::Revealed::TooLarge(size) => assert!(*size > secrets::MAX_REVEAL_BYTES),
        }
    }
}

fn check_release_projection(bytes: &[u8]) {
    match helm::fuzz::project(bytes) {
        Ok(release) => {
            for (value, max, empty) in [
                (&release.chart_name, 128, false),
                (&release.chart_version, 128, false),
                (&release.app_version, 128, true),
                (&release.updated, 64, true),
            ] {
                assert!(value.len() <= max && (empty || !value.is_empty()) && plain(value));
            }
        }
        Err(error) => check_error(&error),
    }
    if let Err(error) = helm::fuzz::hooks(bytes) {
        check_error(&error);
    }
    let release: Value = serde_json::from_slice(bytes).unwrap_or_default();
    if let Some(manifest) = release["manifest"].as_str() {
        check_references(manifest);
    }
}

fn check_references(manifest: &str) {
    let (refs, _, notes) = helm::fuzz::references(manifest);
    assert!(refs.len() <= helm::fuzz::MAX_RESOURCES);
    for reference in &refs {
        for value in [
            &reference.api_version,
            &reference.kind,
            &reference.name,
            &reference.namespace,
        ] {
            assert!(value.len() <= 320 && plain(value));
        }
    }
    notes.iter().for_each(|note| check_error(note));
}

/// Helm release storage: base64 (and gzip) decoding stays within its budget,
/// and every projection is bounded plain text. The first byte picks how the
/// rest is wrapped, so both the encoding and the JSON/YAML get mutated.
pub fn helm_storage(data: &[u8]) {
    use base64::Engine;
    use std::io::Write;
    let Some((mode, rest)) = data.split_first() else {
        return;
    };
    let payload = if mode & 2 == 0 {
        rest.to_vec()
    } else {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        if encoder.write_all(rest).is_err() {
            return;
        }
        let Ok(gzip) = encoder.finish() else {
            return;
        };
        gzip
    };
    let encoded = if mode & 1 == 0 {
        base64::engine::general_purpose::STANDARD
            .encode(payload)
            .into_bytes()
    } else {
        payload
    };
    match helm::fuzz::decode(&encoded) {
        Ok(bytes) => {
            assert!(bytes.len() <= helm::fuzz::MAX_DECODED);
            check_release_projection(&bytes);
        }
        Err(error) => check_error(&error),
    }
    if mode & 4 != 0 {
        check_references(&String::from_utf8_lossy(rest));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// SplitMix64: deterministic, so a failure reproduces with the same seed.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }
        fn below(&mut self, n: usize) -> usize {
            if n == 0 {
                0
            } else {
                (self.next() % n as u64) as usize
            }
        }
    }

    /// Tokens that make parsers and sanitizers take unusual paths.
    const DICTIONARY: &[&str] = &[
        "\n",
        "\r\n",
        "\r",
        "\t",
        " ",
        ": ",
        "- ",
        "---\n",
        "...\n",
        "&a ",
        "*a",
        "<<: ",
        "{",
        "}",
        "[",
        "]",
        ",",
        "'",
        "\"",
        "\\",
        "#",
        "!!binary ",
        "!!str ",
        "? ",
        "|\n",
        ">-\n",
        "~",
        "null",
        "true",
        "no",
        "0x1f",
        "1e999",
        "-1",
        "99999999999999999999",
        "\u{0}",
        "\u{7}",
        "\u{1b}[2J",
        "\u{1b}]52;c;aGk=\u{7}",
        "\u{1b}P",
        "\u{9b}",
        "\u{85}",
        "\u{7f}",
        "\u{202e}",
        "\u{2066}",
        "\u{200b}",
        "\u{feff}",
        "\u{e0041}",
        "\u{2028}",
        "é",
        "😀",
        "../",
        "/",
        "./",
        "$",
        "$(id)",
        "`id`",
        ";",
        "|",
        "&&",
        "%",
        "@",
        "://",
        "\\u{41}",
        "\\\\",
        "-----BEGIN CERTIFICATE-----\n",
        "-----END CERTIFICATE-----\n",
        "eyJ",
        "password=",
        "kubectl.kubernetes.io/last-applied-configuration",
        "Secret",
        "List",
        "items: ",
        "apiVersion: v1\n",
        "kind: ",
        "metadata: ",
        "name: ",
        "namespace: ",
        "Shift-1",
        "Ctrl-G",
        "Alt-x",
        "F5",
        "PgDown",
        "command: ",
        "args: ",
        "shortCut: ",
        "\"\\e[2J\"",
        "\"\\x9b\"",
        "\"\\u202e\"",
        "\"\\0\"",
        "\"\\N\"",
        "\"\\L\"",
        "\"\\t\"",
    ];

    fn seeds(target: &str) -> Vec<Vec<u8>> {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fuzz/seeds")
            .join(target);
        let mut paths: Vec<_> = fs::read_dir(&directory)
            .unwrap_or_else(|e| panic!("seed corpus {}: {e}", directory.display()))
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .collect();
        paths.sort();
        let seeds: Vec<Vec<u8>> = paths
            .iter()
            .map(|path| fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
            .collect();
        assert!(!seeds.is_empty(), "no seeds for {target}");
        seeds
    }

    fn mutate(rng: &mut Rng, input: &[u8], seeds: &[Vec<u8>]) -> Vec<u8> {
        let mut out = input.to_vec();
        for _ in 0..=rng.below(6) {
            let at = rng.below(out.len() + 1);
            match rng.below(8) {
                0 if !out.is_empty() => {
                    let at = rng.below(out.len());
                    out[at] ^= 1 << rng.below(8);
                }
                1 => out.insert(at, rng.next() as u8),
                2 | 3 => {
                    let token = DICTIONARY[rng.below(DICTIONARY.len())].as_bytes();
                    out.splice(at..at, token.iter().copied());
                }
                4 if at < out.len() => {
                    let end = (at + 1 + rng.below(16)).min(out.len());
                    out.drain(at..end);
                }
                5 if at < out.len() => {
                    let end = (at + 1 + rng.below(64)).min(out.len());
                    let chunk = out[at..end].to_vec();
                    out.splice(at..at, chunk);
                }
                6 => {
                    let other = &seeds[rng.below(seeds.len())];
                    let from = rng.below(other.len() + 1);
                    let end = (from + rng.below(128)).min(other.len());
                    out.splice(at..at, other[from..end].iter().copied());
                }
                _ => out.truncate(at),
            }
        }
        out
    }

    /// Runs the seeds, then `iterations` mutated inputs, through one target.
    fn run(target: &str, iterations: usize) {
        let check = TARGETS
            .iter()
            .find(|(name, _)| *name == target)
            .map(|(_, check)| *check)
            .unwrap_or_else(|| panic!("unknown target {target}"));
        let seeds = seeds(target);
        let started = Instant::now();
        let mut rng = Rng(0x6b62_6f72_6700 ^ target.len() as u64);
        for seed in &seeds {
            check(seed);
        }
        for _ in 0..iterations {
            let base = &seeds[rng.below(seeds.len())];
            check(&mutate(&mut rng, base, &seeds));
        }
        assert!(
            started.elapsed().as_secs() < 30,
            "{target} property run is too slow for cargo test"
        );
    }

    #[test]
    fn fuzz_sanitizer() {
        run("sanitizer", 3000);
    }
    #[test]
    fn fuzz_kubeconfig() {
        run("kubeconfig", 600);
    }
    #[test]
    fn fuzz_manifest() {
        run("manifest", 600);
    }
    #[test]
    fn fuzz_archive() {
        run("archive", 300);
    }
    #[test]
    fn fuzz_k9s_views() {
        run("k9s_views", 600);
    }
    #[test]
    fn fuzz_k9s_hotkeys() {
        run("k9s_hotkeys", 600);
    }
    #[test]
    fn fuzz_k9s_aliases() {
        run("k9s_aliases", 600);
    }
    #[test]
    fn fuzz_k9s_plugins() {
        run("k9s_plugins", 600);
    }
    #[test]
    fn fuzz_keymap() {
        run("keymap", 600);
    }
    #[test]
    fn fuzz_ui_config() {
        run("ui_config", 600);
    }
    #[test]
    fn fuzz_secret_masked() {
        run("secret_masked", 1500);
    }
    #[test]
    fn fuzz_helm_storage() {
        run("helm_storage", 600);
    }

    /// Minimal ustar header for hostile archive fixtures.
    fn tar_entry(out: &mut Vec<u8>, name: &str, kind: u8, body: &[u8], link: &str) {
        let mut header = [0u8; 512];
        let name = name.as_bytes();
        header[..name.len().min(100)].copy_from_slice(&name[..name.len().min(100)]);
        header[100..107].copy_from_slice(b"0000644");
        header[124..135].copy_from_slice(format!("{:011o}", body.len()).as_bytes());
        header[156] = kind;
        let link = link.as_bytes();
        header[157..157 + link.len().min(100)].copy_from_slice(&link[..link.len().min(100)]);
        header[257..263].copy_from_slice(b"ustar\0");
        out.extend_from_slice(&header);
        out.extend_from_slice(body);
        out.resize(out.len().div_ceil(512) * 512, 0);
    }

    #[test]
    fn archive_never_escapes_with_generated_hostile_entries() {
        const NAMES: &[&str] = &[
            "top/",
            "top/a",
            "top/d/",
            "top/d/b",
            "top/../x",
            "../x",
            "/etc/x",
            "top/./a",
            "./top/c",
            "top//e",
            "other/f",
            "top/\u{1b}[2J",
            "top/d/../../y",
            "top",
            "top/a/b",
            "top/link",
            "",
        ];
        let mut rng = Rng(7);
        for _ in 0..200 {
            let mut tar = Vec::new();
            for _ in 0..=rng.below(6) {
                let name = NAMES[rng.below(NAMES.len())];
                let kind = [b'0', b'5', b'2', b'1', b'L', b'x', b'3', 0][rng.below(8)];
                let body: Vec<u8> = match kind {
                    b'L' => NAMES[rng.below(NAMES.len())].as_bytes().to_vec(),
                    b'x' => format!("30 path={}\n", NAMES[rng.below(NAMES.len())]).into_bytes(),
                    b'5' | b'2' | b'1' => Vec::new(),
                    _ => vec![b'z'; rng.below(700)],
                };
                tar_entry(&mut tar, name, kind, &body, "../../etc/passwd");
            }
            if rng.below(4) != 0 {
                tar.extend_from_slice(&[0; 1024]);
            }
            archive_extract(&tar);
        }
    }

    #[test]
    fn shell_word_quoting_round_trips_through_the_checker() {
        let mut rng = Rng(11);
        for _ in 0..500 {
            let word: String = (0..rng.below(12))
                .map(|_| {
                    let token = DICTIONARY[rng.below(DICTIONARY.len())];
                    safety::safe_text(token, 64)
                })
                .collect();
            let quoted = extensions::shell_word(&word);
            let parsed = shell_words(&format!("x {quoted}"));
            if word.is_empty() {
                assert_eq!(parsed, Some(vec!["x".to_owned(), String::new()]));
            } else {
                assert_eq!(parsed, Some(vec!["x".to_owned(), word]));
            }
        }
    }
}
