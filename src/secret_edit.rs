//! Reviewed Secret changes: set or remove one key, edit the whole Secret in
//! KBorg's editor, or delete the Secret.
//!
//! A value is never shown: not in the review, not in the audit record and not
//! in an error. The review names the key, the sizes before and after, where
//! the new value came from and the Pods that use the Secret.
use crate::{Result, safety};
use k8s_openapi::api::core::v1::{Pod, Secret};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Instant;

/// Largest value accepted for one key. A whole Secret is limited to 1 MiB by
/// the API server; this leaves room for its other keys and metadata.
pub const MAX_VALUE: usize = 512 * 1024;
/// Pods read to find the users of a Secret.
pub const MAX_PODS: usize = 500;
const MAX_USER_LINES: usize = 20;

/// A new Secret value. Its bytes are overwritten when it is dropped and it
/// never prints its content.
pub struct SecretBytes(Vec<u8>);
impl SecretBytes {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.0
    }
}
impl Drop for SecretBytes {
    fn drop(&mut self) {
        self.0.fill(0);
        std::hint::black_box(&self.0);
    }
}
impl std::fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretBytes({} bytes)", self.0.len())
    }
}

/// Where a new value came from, for the review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Typed,
    File(PathBuf),
}

#[derive(Debug)]
pub enum Change {
    /// Delete the whole Secret.
    Delete,
    /// Add a key or replace its value.
    Set {
        key: String,
        value: SecretBytes,
        source: Source,
    },
    /// Remove one key.
    Remove { key: String },
    /// Create a new Opaque Secret with one key.
    Create {
        key: String,
        value: SecretBytes,
        source: Source,
    },
    /// The whole-Secret editor's document, before it is compared with the
    /// Secret; the gateway turns it into `Edit`.
    Document(crate::secret_document::Edited),
    /// The keys the whole-Secret editor changes, and how many it leaves.
    Edit {
        changes: Vec<KeyChange>,
        unchanged: usize,
    },
}

/// One key of a whole-Secret edit: a new value (`old` is the size before,
/// `None` for a new key) or, with no value, a removal.
#[derive(Debug)]
pub struct KeyChange {
    pub key: String,
    pub value: Option<SecretBytes>,
    pub old: Option<usize>,
}

/// Compares the edited document with the Secret as it is now. The Secret
/// must still be the version the editor opened.
pub fn resolve(edited: crate::secret_document::Edited, current: &Secret) -> Result<Change> {
    if current
        .metadata
        .resource_version
        .as_deref()
        .unwrap_or_default()
        != edited.version
    {
        return Err("the Secret changed since you opened it in the editor, so this edit could undo someone else's change. Next: note your changes, Esc, and press E to edit the current version".into());
    }
    let empty = std::collections::BTreeMap::new();
    let data = current.data.as_ref().unwrap_or(&empty);
    if let Some(key) = edited.keep.iter().find(|key| !data.contains_key(*key)) {
        return Err(format!(
            "key {key} under keep: is not in the Secret; remove it from keep:"
        ));
    }
    let edited_keys: std::collections::BTreeSet<String> =
        edited.values.iter().map(|(key, _)| key.clone()).collect();
    let mut changes = Vec::new();
    let mut unchanged = edited.keep.len();
    for (key, value) in edited.values {
        match data.get(&key) {
            Some(old) if old.0 == value.bytes() => unchanged += 1,
            old => changes.push(KeyChange {
                old: old.map(|old| old.0.len()),
                key,
                value: Some(value),
            }),
        }
    }
    for (key, old) in data {
        if !edited_keys.contains(key) && !edited.keep.contains(key) {
            changes.push(KeyChange {
                key: key.clone(),
                value: None,
                old: Some(old.0.len()),
            });
        }
    }
    changes.sort_by(|a, b| a.key.cmp(&b.key));
    if changes.is_empty() {
        return Err("Nothing changed: the Secret is as you opened it.".into());
    }
    Ok(Change::Edit { changes, unchanged })
}
impl Change {
    /// The audit action name.
    pub fn operation(&self) -> &'static str {
        match self {
            Self::Delete => "secret-delete",
            Self::Set { .. } => "secret-set-key",
            Self::Remove { .. } => "secret-remove-key",
            Self::Create { .. } => "secret-create",
            Self::Document(_) | Self::Edit { .. } => "secret-edit",
        }
    }
    fn key(&self) -> Option<&str> {
        match self {
            Self::Delete | Self::Document(_) | Self::Edit { .. } => None,
            Self::Set { key, .. } | Self::Remove { key } | Self::Create { key, .. } => Some(key),
        }
    }
}

/// What the review needs to know about the Secret: never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shape {
    pub secret_type: String,
    /// Key names and value sizes in bytes.
    pub keys: Vec<(String, usize)>,
    pub immutable: bool,
    pub version: String,
    /// Controller that owns the Secret, as `Kind name`.
    pub owner: Option<String>,
}
impl Shape {
    /// A Secret that does not exist yet.
    pub fn absent() -> Self {
        Self {
            secret_type: "Opaque".into(),
            keys: Vec::new(),
            immutable: false,
            version: String::new(),
            owner: None,
        }
    }
    pub fn of(secret: &Secret) -> Self {
        Self {
            secret_type: secret.type_.clone().unwrap_or_else(|| "Opaque".into()),
            keys: secret
                .data
                .iter()
                .flatten()
                .map(|(key, value)| (key.clone(), value.0.len()))
                .collect(),
            immutable: secret.immutable == Some(true),
            version: secret.metadata.resource_version.clone().unwrap_or_default(),
            owner: secret
                .metadata
                .owner_references
                .iter()
                .flatten()
                .next()
                .map(|owner| format!("{} {}", owner.kind, owner.name)),
        }
    }
    fn size(&self, key: &str) -> Option<usize> {
        self.keys
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, size)| *size)
    }
}

/// A reviewed Secret change, bound to the UID and resourceVersion it was
/// reviewed against.
pub struct Prepared {
    pub(crate) namespace: String,
    pub(crate) name: String,
    pub(crate) uid: String,
    pub(crate) version: String,
    pub(crate) change: Change,
    /// Whether the Secret has a `data` map to patch into.
    pub(crate) has_data: bool,
    pub(crate) session: String,
    pub(crate) created: Instant,
    summary: String,
}
impl Prepared {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        namespace: &str,
        name: &str,
        uid: &str,
        shape: &Shape,
        change: Change,
        session: &str,
        summary: String,
    ) -> Result<Self> {
        Ok(Self {
            namespace: namespace.into(),
            name: name.into(),
            uid: uid.into(),
            version: shape.version.clone(),
            has_data: !shape.keys.is_empty(),
            change,
            session: session.into(),
            created: Instant::now(),
            summary: safety::review_text(&summary, 64 * 1024)?,
        })
    }
    pub fn summary(&self) -> &str {
        &self.summary
    }
    pub fn uid(&self) -> &str {
        &self.uid
    }
    pub fn confirmation(&self) -> String {
        format!("{}/{}", self.namespace, self.name)
    }
    /// One line for the result view and the status bar.
    pub fn outcome(&self) -> String {
        let target = self.confirmation();
        match &self.change {
            Change::Delete => format!("Secret {target} deleted."),
            Change::Set { key, value, .. } => format!(
                "Secret {target}: key {key} set ({} bytes). Pods read environment values when they start; restart them to pick it up. Mounted files follow within about a minute.",
                value.len()
            ),
            Change::Remove { key } => format!(
                "Secret {target}: key {key} removed. A Pod that needs it fails at its next start."
            ),
            Change::Create { key, value, .. } => format!(
                "Secret {target} created with key {key} ({} bytes). Pods waiting for it start on their next retry.",
                value.len()
            ),
            Change::Edit { changes, .. } => {
                let (set, added, removed) = counts(changes);
                format!(
                    "Secret {target}: {} key{} changed ({set} set, {added} added, {removed} removed). Pods read environment values when they start; restart them to pick it up. Mounted files follow within about a minute.",
                    changes.len(),
                    if changes.len() == 1 { "" } else { "s" }
                )
            }
            Change::Document(_) => format!("Secret {target}: not compared yet."),
        }
    }
}

/// (set, added, removed) keys of a whole-Secret edit.
fn counts(changes: &[KeyChange]) -> (usize, usize, usize) {
    changes
        .iter()
        .fold((0, 0, 0), |(set, added, removed), change| {
            match (&change.value, change.old) {
                (None, _) => (set, added, removed + 1),
                (Some(_), Some(_)) => (set + 1, added, removed),
                (Some(_), None) => (set, added + 1, removed),
            }
        })
}

/// A name the API accepts for a new Secret (DNS-1123 subdomain).
pub fn valid_name(name: &str) -> bool {
    let edge = |b: Option<&u8>| b.is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    name.len() <= 253
        && edge(name.as_bytes().first())
        && edge(name.as_bytes().last())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
}

/// A key name the API accepts in a Secret.
pub fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 253
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

/// The new value from the prompt: the text as typed, or the bytes of a file
/// when the text is `@PATH` (absolute, or starting with `~/`).
pub fn value_from_input(text: &str) -> Result<(SecretBytes, Source)> {
    let Some(path) = text.strip_prefix('@') else {
        if text.len() > MAX_VALUE {
            return Err("the value is larger than 512 KiB".into());
        }
        return Ok((SecretBytes::new(text.as_bytes().to_vec()), Source::Typed));
    };
    let path = match path.strip_prefix("~/") {
        Some(rest) => {
            PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set; use an absolute path")?)
                .join(rest)
        }
        None => PathBuf::from(path),
    };
    if !path.is_absolute() {
        return Err("write the file as @/absolute/path or @~/path".into());
    }
    let bytes = safety::read_small(&path, MAX_VALUE as u64).map_err(|error| {
        format!(
            "cannot read {}: {error}. Next: check the path; the file must be a regular file of at most 512 KiB",
            safety::safe_text(&path.display().to_string(), 300)
        )
    })?;
    Ok((SecretBytes::new(bytes), Source::File(path)))
}

/// How one Pod uses the named Secret; empty if it does not.
pub fn pod_uses(name: &str, pod: &Pod) -> Vec<String> {
    let Some(spec) = &pod.spec else {
        return Vec::new();
    };
    let mut uses = Vec::new();
    for volume in spec.volumes.iter().flatten() {
        let direct = volume
            .secret
            .as_ref()
            .is_some_and(|source| source.secret_name.as_deref() == Some(name));
        let projected = volume
            .projected
            .iter()
            .flat_map(|projected| projected.sources.iter().flatten())
            .any(|source| source.secret.as_ref().is_some_and(|s| s.name == name));
        if direct || projected {
            uses.push(format!("volume {}", safety::safe_text(&volume.name, 63)));
        }
    }
    let containers = spec
        .init_containers
        .iter()
        .flatten()
        .chain(&spec.containers);
    for container in containers {
        let all = container
            .env_from
            .iter()
            .flatten()
            .any(|source| source.secret_ref.as_ref().is_some_and(|r| r.name == name));
        let keys = container
            .env
            .iter()
            .flatten()
            .filter(|entry| {
                entry
                    .value_from
                    .as_ref()
                    .and_then(|from| from.secret_key_ref.as_ref())
                    .is_some_and(|r| r.name == name)
            })
            .count();
        let container = safety::safe_text(&container.name, 63);
        if all {
            uses.push(format!("all keys as env in {container}"));
        }
        if keys > 0 {
            uses.push(format!("{keys} env from keys in {container}"));
        }
    }
    if spec
        .image_pull_secrets
        .iter()
        .flatten()
        .any(|pull| pull.name == name)
    {
        uses.push("image pull".into());
    }
    uses
}

/// `(pod, how)` for every Pod that uses the Secret.
pub fn users(name: &str, pods: &[Pod]) -> Vec<(String, String)> {
    pods.iter()
        .filter_map(|pod| {
            let uses = pod_uses(name, pod);
            (!uses.is_empty()).then(|| {
                (
                    safety::safe_text(pod.metadata.name.as_deref().unwrap_or("-"), 253),
                    uses.join("; "),
                )
            })
        })
        .collect()
}

/// What is known about the Pods of the namespace when the review is written.
pub struct Usage {
    pub users: Vec<(String, String)>,
    pub pods: usize,
    pub truncated: bool,
    /// Why the Pods could not be read, when they could not.
    pub unknown: Option<String>,
}

fn describe(value: &SecretBytes) -> String {
    match std::str::from_utf8(value.bytes()) {
        Ok(text) => {
            let lines = text.lines().count().max(1);
            format!("text, {lines} line{}", if lines == 1 { "" } else { "s" })
        }
        Err(_) => "binary".into(),
    }
}

/// Something about a new value worth a second look, never the value itself.
fn value_warning(value: &SecretBytes) -> Option<&'static str> {
    let bytes = value.bytes();
    if value.is_empty() {
        Some("The new value is empty.")
    } else if bytes.ends_with(b"\n") {
        Some(
            "The value ends with a line break, and that is stored too. A password read from a file often should not have one.",
        )
    } else if bytes.first().is_some_and(u8::is_ascii_whitespace)
        || bytes.last().is_some_and(u8::is_ascii_whitespace)
    {
        Some("The value starts or ends with a space; it is stored as it is.")
    } else {
        None
    }
}

/// Refuses a change that cannot work before anything is sent.
pub fn check(shape: &Shape, change: &Change) -> Result<()> {
    if let Some(key) = change.key()
        && !valid_key(key)
    {
        return Err(
            "a Secret key is 1 to 253 characters: letters, digits, '-', '_' and '.'".into(),
        );
    }
    if shape.immutable && !matches!(change, Change::Delete) {
        return Err(
            "this Secret is immutable: its keys cannot change. Next: delete it and create a new one".into(),
        );
    }
    match change {
        Change::Remove { key } if shape.size(key).is_none() => {
            Err("the Secret has no such key; reopen :secrets".into())
        }
        Change::Set { value, .. } | Change::Create { value, .. } if value.len() > MAX_VALUE => {
            Err("the value is larger than 512 KiB".into())
        }
        Change::Document(_) => Err("the edited Secret was not compared with the cluster".into()),
        Change::Edit { changes, .. } => {
            for change in changes {
                if !valid_key(&change.key) {
                    return Err(
                        "a Secret key is 1 to 253 characters: letters, digits, '-', '_' and '.'"
                            .into(),
                    );
                }
                if change.value.as_ref().is_some_and(|v| v.len() > MAX_VALUE) {
                    return Err(format!(
                        "the value of {} is larger than 512 KiB",
                        change.key
                    ));
                }
                if change.value.is_none() && shape.size(&change.key).is_none() {
                    return Err("the Secret has no such key; reopen :secrets".into());
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The review text. It never contains a value.
#[allow(clippy::too_many_arguments)]
pub fn review(
    context: &str,
    server: &str,
    namespace: &str,
    name: &str,
    uid: &str,
    shape: &Shape,
    change: &Change,
    usage: &Usage,
    demo: bool,
) -> String {
    let target = format!("{namespace}/{name}");
    let count = shape.keys.len();
    let creating = matches!(change, Change::Create { .. });
    let mut lines = vec![
        if creating {
            "REVIEW NEW SECRET"
        } else {
            "REVIEW SECRET CHANGE"
        }
        .to_owned(),
        format!("Context: {context}"),
        format!("Server: {server}"),
    ];
    if creating {
        lines.push(format!(
            "Secret: {target} (type Opaque, does not exist yet)"
        ));
    } else {
        lines.push(format!(
            "Secret: {target} (type {}, {count} key{})",
            safety::safe_text(&shape.secret_type, 100),
            if count == 1 { "" } else { "s" }
        ));
        lines.push(format!("UID: {uid}"));
        lines.push(format!("ResourceVersion: {}", shape.version));
    }
    lines.push(String::new());
    match change {
        Change::Delete => {
            lines.push(format!(
                "DELETE the Secret {target} and all {count} of its keys"
            ));
            lines.push("! A Pod that mounts or reads it fails at its next start.".into());
        }
        Change::Set { key, value, source } | Change::Create { key, value, source } => {
            lines.push(match shape.size(key) {
                _ if creating => format!(
                    "CREATE the Secret {target} with key {key}: {} bytes ({})",
                    value.len(),
                    describe(value)
                ),
                Some(old) => format!(
                    "SET key {key}: {old} bytes -> {} bytes ({})",
                    value.len(),
                    describe(value)
                ),
                None => format!("ADD key {key}: {} bytes ({})", value.len(), describe(value)),
            });
            lines.push(match source {
                Source::Typed => "The value is not shown. Source: typed in KBorg.".into(),
                Source::File(path) => format!(
                    "The value is not shown. Source: file {}",
                    safety::safe_text(&path.display().to_string(), 300)
                ),
            });
            if let Some(warning) = value_warning(value) {
                lines.push(format!("! {warning}"));
            }
        }
        Change::Edit { changes, unchanged } => {
            let (set, added, removed) = counts(changes);
            lines.push(format!(
                "EDIT the Secret {target}: {set} set, {added} added, {removed} removed, {unchanged} unchanged"
            ));
            for change in changes {
                lines.push(match (&change.value, change.old) {
                    (Some(value), Some(old)) => format!(
                        "  SET key {}: {old} bytes -> {} bytes ({})",
                        change.key,
                        value.len(),
                        describe(value)
                    ),
                    (Some(value), None) => format!(
                        "  ADD key {}: {} bytes ({})",
                        change.key,
                        value.len(),
                        describe(value)
                    ),
                    (None, old) => {
                        format!("  REMOVE key {} ({} bytes)", change.key, old.unwrap_or(0))
                    }
                });
            }
            lines.push("The values are not shown. Source: KBorg's Secret editor.".into());
            for change in changes {
                if let Some(warning) = change.value.as_ref().and_then(value_warning) {
                    lines.push(format!("! Key {}: {warning}", change.key));
                }
            }
            if removed > 0 {
                lines.push("! A Pod that needs a removed key fails at its next start.".into());
            }
        }
        Change::Document(_) => lines.push("The edit was not compared with the Secret.".into()),
        Change::Remove { key } => {
            lines.push(format!(
                "REMOVE key {key} ({} bytes)",
                shape.size(key).unwrap_or(0)
            ));
            lines.push("! A Pod that needs this key fails at its next start.".into());
        }
    }
    match shape.secret_type.as_str() {
        "kubernetes.io/service-account-token" => lines.push(
            "! This is a ServiceAccount token: whatever signs in with it loses or changes its access."
                .into(),
        ),
        "helm.sh/release.v1" => lines.push(
            "! This is Helm's record of a release revision: changing or deleting it breaks helm history, rollback and upgrade for that release."
                .into(),
        ),
        "kubernetes.io/tls" => lines.push(
            "! This is a TLS certificate: an Ingress or server that uses it serves the new one, or fails without it."
                .into(),
        ),
        "kubernetes.io/dockerconfigjson" | "kubernetes.io/dockercfg" => lines.push(
            "! This holds registry credentials: image pulls that rely on it can start failing."
                .into(),
        ),
        _ => {}
    }
    if let Some(owner) = &shape.owner {
        lines.push(format!(
            "! Owned by {}: its controller may write the Secret back.",
            safety::safe_text(owner, 320)
        ));
    }
    lines.push(String::new());
    match &usage.unknown {
        Some(reason) => lines.push(format!(
            "Which Pods use it is unknown: {}",
            safety::safe_text(reason, 300)
        )),
        None => {
            lines.push(format!(
                "{} {} of {} Pods in the namespace{}{}",
                if creating {
                    "Already named by"
                } else {
                    "Used by"
                },
                usage.users.len(),
                usage.pods,
                if usage.truncated {
                    " (the Pod list was cut at its limit)"
                } else {
                    ""
                },
                if usage.users.is_empty() { "." } else { ":" }
            ));
            for (pod, how) in usage.users.iter().take(MAX_USER_LINES) {
                lines.push(format!("  {pod}  {how}"));
            }
            if usage.users.len() > MAX_USER_LINES {
                lines.push(format!(
                    "  ... and {} more",
                    usage.users.len() - MAX_USER_LINES
                ));
            }
        }
    }
    if creating {
        lines.push(
            "A Pod that is waiting for this Secret starts on its next retry; running Pods are not changed."
                .into(),
        );
    } else if !matches!(change, Change::Delete) {
        lines.push(
            "Pods read environment values when they start: restart them to pick up a change. Mounted files follow within about a minute."
                .into(),
        );
    }
    lines.push(String::new());
    lines.push(
        if demo {
            "Demo validation only; no cluster contacted and the sample Secret stays as it is."
        } else {
            "Server dry-run passed. Admission may behave differently on the real write."
        }
        .into(),
    );
    lines.push(format!(
        "Type {target} to {}. Review expires after 60 seconds.",
        if creating { "create it" } else { "apply" }
    ));
    lines.join("\n")
}

/// The Secret a create sends. Wipe it with `secrets::wipe_secret` after use.
pub fn new_secret(prepared: &Prepared) -> Option<Secret> {
    let Change::Create { key, value, .. } = &prepared.change else {
        return None;
    };
    Some(Secret {
        metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
            name: Some(prepared.name.clone()),
            namespace: Some(prepared.namespace.clone()),
            ..Default::default()
        },
        type_: Some("Opaque".into()),
        data: Some([(key.clone(), k8s_openapi::ByteString(value.bytes().to_vec()))].into()),
        ..Default::default()
    })
}

fn pointer(key: &str) -> String {
    format!("/data/{}", key.replace('~', "~0").replace('/', "~1"))
}

/// The JSON patch for a key change, locked to the reviewed UID and
/// resourceVersion. `None` for a delete or a create, which are not patches.
pub fn patch(prepared: &Prepared) -> Option<Value> {
    let mut operations = vec![
        json!({"op":"test","path":"/metadata/uid","value":prepared.uid}),
        json!({"op":"test","path":"/metadata/resourceVersion","value":prepared.version}),
    ];
    match &prepared.change {
        Change::Delete | Change::Create { .. } | Change::Document(_) => return None,
        Change::Edit { changes, .. } => {
            use base64::Engine;
            let encode = |value: &SecretBytes| {
                base64::engine::general_purpose::STANDARD.encode(value.bytes())
            };
            if prepared.has_data {
                for change in changes {
                    operations.push(match &change.value {
                        Some(value) => {
                            json!({"op":"add","path":pointer(&change.key),"value":encode(value)})
                        }
                        None => json!({"op":"remove","path":pointer(&change.key)}),
                    });
                }
            } else {
                // A Secret without keys gets them in one data map.
                let data: serde_json::Map<String, Value> = changes
                    .iter()
                    .filter_map(|c| Some((c.key.clone(), json!(encode(c.value.as_ref()?)))))
                    .collect();
                operations.push(json!({"op":"add","path":"/data","value":data}));
            }
        }
        Change::Set { key, value, .. } => {
            use base64::Engine;
            let encoded = base64::engine::general_purpose::STANDARD.encode(value.bytes());
            if prepared.has_data {
                operations.push(json!({"op":"add","path":pointer(key),"value":encoded}));
            } else {
                operations.push(json!({"op":"add","path":"/data","value":{key.as_str():encoded}}));
            }
        }
        Change::Remove { key } => operations.push(json!({"op":"remove","path":pointer(key)})),
    }
    Some(Value::Array(operations))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_whole_secret_edit_reviews_key_changes_and_patches_only_them() -> Result<()> {
        use crate::secret_document::{parse, render};
        let mut secret = Secret {
            data: Some(
                [("a", "one"), ("b", "two"), ("c", "three")]
                    .into_iter()
                    .map(|(k, v)| (k.to_owned(), k8s_openapi::ByteString(v.as_bytes().to_vec())))
                    .collect(),
            ),
            ..Default::default()
        };
        secret.metadata.resource_version = Some("41".into());
        let (_, text) = render("demo", "api", "uid-1", &secret)?;
        let edited = format!(
            "{}  d: \"canary-2\"\n",
            text.as_str()
                .replace("  a: \"one\"\n", "  a: \"canary-1\"\n")
                .replace("  b: \"two\"\n", "")
        );
        let change = resolve(parse(&edited, "41")?, &secret)?;
        let shape = Shape::of(&secret);
        check(&shape, &change)?;
        let usage = Usage {
            users: Vec::new(),
            pods: 0,
            truncated: false,
            unknown: None,
        };
        let summary = review(
            "ctx",
            "https://s",
            "demo",
            "api",
            "uid-1",
            &shape,
            &change,
            &usage,
            false,
        );
        for line in [
            "EDIT the Secret demo/api: 1 set, 1 added, 1 removed, 1 unchanged",
            "  SET key a: 3 bytes -> 8 bytes (text, 1 line)",
            "  ADD key d: 8 bytes (text, 1 line)",
            "  REMOVE key b (3 bytes)",
            "! A Pod that needs a removed key fails at its next start.",
        ] {
            assert!(summary.contains(line), "review line missing: {line}");
        }
        assert!(!summary.contains("canary"), "a value reached the review");
        let prepared = Prepared::new("demo", "api", "uid-1", &shape, change, "s", summary)?;
        assert!(
            prepared
                .outcome()
                .contains("3 keys changed (1 set, 1 added, 1 removed)")
        );
        let patch = patch(&prepared).ok_or("no patch")?;
        use base64::Engine;
        let encode = |v: &str| base64::engine::general_purpose::STANDARD.encode(v);
        assert_eq!(
            patch,
            json!([
                {"op":"test","path":"/metadata/uid","value":"uid-1"},
                {"op":"test","path":"/metadata/resourceVersion","value":"41"},
                {"op":"add","path":"/data/a","value":encode("canary-1")},
                {"op":"remove","path":"/data/b"},
                {"op":"add","path":"/data/d","value":encode("canary-2")},
            ])
        );
        // Opened at another version, unchanged, or keeping a key that is gone.
        let stale = resolve(parse(text.as_str(), "40")?, &secret)
            .err()
            .unwrap_or_default();
        assert!(stale.contains("changed since you opened it"));
        let same = resolve(parse(text.as_str(), "41")?, &secret)
            .err()
            .unwrap_or_default();
        assert!(same.contains("Nothing changed"));
        let gone = resolve(parse("data:\n  a: one\nkeep:\n  - zzz\n", "41")?, &secret)
            .err()
            .unwrap_or_default();
        assert!(gone.contains("under keep:"));
        // A Secret without keys gets its new keys in one data map.
        let mut empty = Secret::default();
        empty.metadata.resource_version = Some("7".into());
        let change = resolve(parse("data:\n  x: \"1\"\n", "7")?, &empty)?;
        let shape = Shape::of(&empty);
        let prepared = Prepared::new("demo", "e", "u", &shape, change, "s", "r".into())?;
        assert_eq!(
            patch_value(&prepared)?,
            json!({"op":"add","path":"/data","value":{"x":encode("1")}})
        );
        Ok(())
    }
    fn patch_value(prepared: &Prepared) -> Result<Value> {
        patch(prepared)
            .and_then(|p| p.get(2).cloned())
            .ok_or_else(|| "no patch".into())
    }

    fn shape(keys: &[(&str, usize)]) -> Shape {
        Shape {
            secret_type: "Opaque".into(),
            keys: keys.iter().map(|(k, n)| ((*k).to_owned(), *n)).collect(),
            immutable: false,
            version: "41".into(),
            owner: None,
        }
    }
    fn usage() -> Usage {
        Usage {
            users: vec![("web-1".into(), "volume creds".into())],
            pods: 3,
            truncated: false,
            unknown: None,
        }
    }
    fn set(key: &str, value: &[u8]) -> Change {
        Change::Set {
            key: key.into(),
            value: SecretBytes::new(value.to_vec()),
            source: Source::Typed,
        }
    }
    fn text(shape: &Shape, change: &Change) -> String {
        review(
            "ctx",
            "https://server/",
            "demo",
            "db",
            "uid-1",
            shape,
            change,
            &usage(),
            false,
        )
    }

    #[test]
    fn the_review_names_key_sizes_and_users_and_never_the_value() {
        let shape = shape(&[("password", 12), ("user", 5)]);
        let replaced = text(&shape, &set("password", b"new-value-canary"));
        assert!(
            replaced.contains("Secret: demo/db (type Opaque, 2 keys)"),
            "{replaced}"
        );
        assert!(
            replaced.contains("SET key password: 12 bytes -> 16 bytes (text, 1 line)"),
            "{replaced}"
        );
        assert!(replaced.contains("Source: typed in KBorg"), "{replaced}");
        assert!(
            replaced.contains("Used by 1 of 3 Pods in the namespace:\n  web-1  volume creds"),
            "{replaced}"
        );
        assert!(replaced.contains("Type demo/db to apply"), "{replaced}");
        assert!(!replaced.contains("canary"), "{replaced}");
        let added = text(&shape, &set("token", &[0xff, 0x00, 0x01]));
        assert!(added.contains("ADD key token: 3 bytes (binary)"), "{added}");
        let removed = text(&shape, &Change::Remove { key: "user".into() });
        assert!(
            removed.contains("REMOVE key user (5 bytes)")
                && removed.contains("fails at its next start"),
            "{removed}"
        );
        let deleted = text(&shape, &Change::Delete);
        assert!(
            deleted.contains("DELETE the Secret demo/db and all 2 of its keys"),
            "{deleted}"
        );
        assert!(!deleted.contains("restart them"), "{deleted}");
        // The text passes the approval check: plain, complete, no controls.
        for review in [&replaced, &added, &removed, &deleted] {
            assert_eq!(
                safety::review_text(review, 64 * 1024).as_deref(),
                Ok(review.as_str())
            );
        }
    }

    #[test]
    fn the_review_warns_about_line_breaks_spaces_empty_values_types_and_owners() {
        let plain = shape(&[("password", 12)]);
        assert!(text(&plain, &set("password", b"value\n")).contains("ends with a line break"));
        assert!(text(&plain, &set("password", b" value")).contains("starts or ends with a space"));
        assert!(text(&plain, &set("password", b"")).contains("The new value is empty"));
        assert!(!text(&plain, &set("password", b"value")).contains("! The value"));
        let file = Change::Set {
            key: "password".into(),
            value: SecretBytes::new(b"x".to_vec()),
            source: Source::File("/tmp/secret.txt".into()),
        };
        assert!(text(&plain, &file).contains("Source: file /tmp/secret.txt"));
        for (secret_type, note) in [
            (
                "kubernetes.io/service-account-token",
                "ServiceAccount token",
            ),
            ("helm.sh/release.v1", "Helm's record"),
            ("kubernetes.io/tls", "TLS certificate"),
            ("kubernetes.io/dockerconfigjson", "registry credentials"),
        ] {
            let mut typed = plain.clone();
            typed.secret_type = secret_type.into();
            assert!(
                text(&typed, &Change::Delete).contains(note),
                "type-specific warning missing"
            );
        }
        let mut owned = plain.clone();
        owned.owner = Some("Certificate web-tls".into());
        assert!(text(&owned, &Change::Delete).contains("! Owned by Certificate web-tls"));
        let unknown = Usage {
            users: Vec::new(),
            pods: 0,
            truncated: false,
            unknown: Some("pods is forbidden".into()),
        };
        let review = review(
            "c",
            "s",
            "demo",
            "db",
            "u",
            &plain,
            &Change::Delete,
            &unknown,
            true,
        );
        assert!(
            review.contains("Which Pods use it is unknown: pods is forbidden"),
            "{review}"
        );
        assert!(review.contains("Demo validation only"), "{review}");
    }

    #[test]
    fn impossible_changes_are_refused_before_anything_is_sent() {
        let plain = shape(&[("password", 12)]);
        assert!(check(&plain, &set("password", b"x")).is_ok());
        assert!(check(&plain, &set("new-key_1.txt", b"x")).is_ok());
        for bad in ["", "has space", "slash/key", "caf\u{e9}"] {
            assert!(check(&plain, &set(bad, b"x")).is_err(), "{bad}");
        }
        assert!(
            check(
                &plain,
                &Change::Remove {
                    key: "absent".into()
                }
            )
            .is_err()
        );
        assert!(
            check(
                &plain,
                &Change::Remove {
                    key: "password".into()
                }
            )
            .is_ok()
        );
        assert!(check(&plain, &set("password", &vec![0; MAX_VALUE + 1])).is_err());
        let mut immutable = plain.clone();
        immutable.immutable = true;
        let error = check(&immutable, &set("password", b"x"))
            .err()
            .unwrap_or_default();
        assert!(
            error.contains("immutable") && error.contains("Next:"),
            "{error}"
        );
        assert!(
            check(
                &immutable,
                &Change::Remove {
                    key: "password".into()
                }
            )
            .is_err()
        );
        assert!(check(&immutable, &Change::Delete).is_ok());
    }

    #[test]
    fn the_patch_is_locked_to_the_review_and_encodes_the_value() -> Result<()> {
        let plain = shape(&[("password", 12)]);
        let prepare = |shape: &Shape, change: Change| {
            Prepared::new(
                "demo",
                "db",
                "uid-1",
                shape,
                change,
                "session",
                "review".into(),
            )
        };
        let prepared = prepare(&plain, set("password", b"hi\n"))?;
        assert_eq!(
            patch(&prepared),
            Some(json!([
                {"op":"test","path":"/metadata/uid","value":"uid-1"},
                {"op":"test","path":"/metadata/resourceVersion","value":"41"},
                {"op":"add","path":"/data/password","value":"aGkK"},
            ]))
        );
        assert_eq!(prepared.confirmation(), "demo/db");
        assert!(prepared.outcome().contains("key password set (3 bytes)"));
        // A Secret without data gets the map created with the key.
        let empty = prepare(&shape(&[]), set("first", b"a"))?;
        assert_eq!(
            patch(&empty).map(|p| p[2].clone()),
            Some(json!({"op":"add","path":"/data","value":{"first":"YQ=="}}))
        );
        let removed = prepare(
            &plain,
            Change::Remove {
                key: "password".into(),
            },
        )?;
        assert_eq!(
            patch(&removed).map(|p| p[2].clone()),
            Some(json!({"op":"remove","path":"/data/password"}))
        );
        assert!(patch(&prepare(&plain, Change::Delete)?).is_none());
        // Debug output never carries the bytes.
        let debug = format!("{:?}", set("password", b"debug-canary"));
        assert!(
            debug.contains("12 bytes") && !debug.contains("canary"),
            "{debug}"
        );
        Ok(())
    }

    #[test]
    fn a_new_secret_is_reviewed_without_its_value_and_sent_as_one_opaque_key() -> Result<()> {
        let create = Change::Create {
            key: "password".into(),
            value: SecretBytes::new(b"create-canary".to_vec()),
            source: Source::Typed,
        };
        let absent = Shape::absent();
        assert!(check(&absent, &create).is_ok());
        let review = text(&absent, &create);
        assert!(review.starts_with("REVIEW NEW SECRET"), "{review}");
        for line in [
            "Secret: demo/db (type Opaque, does not exist yet)",
            "CREATE the Secret demo/db with key password: 13 bytes (text, 1 line)",
            "Already named by 1 of 3 Pods in the namespace:",
            "Type demo/db to create it.",
        ] {
            assert!(review.contains(line), "{line}\n{review}");
        }
        assert!(
            !review.contains("canary") && !review.contains("UID:"),
            "{review}"
        );
        let prepared = Prepared::new("demo", "db", "", &absent, create, "s", review)?;
        assert!(patch(&prepared).is_none());
        let mut secret = new_secret(&prepared).ok_or("no Secret body")?;
        assert_eq!(secret.metadata.name.as_deref(), Some("db"));
        assert_eq!(secret.type_.as_deref(), Some("Opaque"));
        assert_eq!(
            secret
                .data
                .as_ref()
                .and_then(|d| d.get("password"))
                .map(|b| b.0.as_slice()),
            Some(&b"create-canary"[..])
        );
        crate::secrets::wipe_secret(&mut secret);
        assert!(
            prepared
                .outcome()
                .contains("created with key password (13 bytes)")
        );
        assert_eq!(prepared.change.operation(), "secret-create");
        for name in ["db", "db-creds.v2", "a"] {
            assert!(valid_name(name), "{name}");
        }
        for name in ["", "Db", "db_creds", "-db", "db.", &"a".repeat(254)] {
            assert!(!valid_name(name), "{name}");
        }
        Ok(())
    }

    #[test]
    fn a_value_is_typed_text_or_the_bytes_of_a_file() -> Result<()> {
        let (value, source) = value_from_input("p@ss word")?;
        assert_eq!(
            (value.bytes(), &source),
            (&b"p@ss word"[..], &Source::Typed)
        );
        let dir = std::env::temp_dir().join(format!("kborg-secret-edit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|_| "fixture")?;
        let dir = dir.canonicalize().map_err(|_| "fixture")?;
        let path = dir.join("value.bin");
        std::fs::write(&path, b"line\n\xff").map_err(|_| "fixture")?;
        let (value, source) = value_from_input(&format!("@{}", path.display()))?;
        assert_eq!(value.bytes(), b"line\n\xff");
        assert_eq!(source, Source::File(path.clone()));
        assert!(value_from_input("@relative/path").is_err());
        let missing = value_from_input(&format!("@{}", dir.join("absent").display()));
        assert!(missing.is_err_and(|error| error.contains("Next:")));
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn pods_that_mount_read_or_pull_with_the_secret_are_found() -> Result<()> {
        let pod = |name: &str, spec: Value| -> Result<Pod> {
            serde_json::from_value(json!({"metadata":{"name":name},"spec":spec}))
                .map_err(|_| "fixture".into())
        };
        let pods = [
            pod(
                "mounts",
                json!({"containers":[{"name":"app"}],"volumes":[{"name":"creds","secret":{"secretName":"db"}}]}),
            )?,
            pod(
                "projects",
                json!({"containers":[{"name":"app"}],"volumes":[{"name":"mixed","projected":{"sources":[{"secret":{"name":"db"}}]}}]}),
            )?,
            pod(
                "reads",
                json!({"initContainers":[{"name":"init","envFrom":[{"secretRef":{"name":"db"}}]}],"containers":[{"name":"app","env":[{"name":"A","valueFrom":{"secretKeyRef":{"name":"db","key":"a"}}},{"name":"B","valueFrom":{"secretKeyRef":{"name":"other","key":"b"}}}]}]}),
            )?,
            pod(
                "pulls",
                json!({"containers":[{"name":"app"}],"imagePullSecrets":[{"name":"db"}]}),
            )?,
            pod(
                "unrelated",
                json!({"containers":[{"name":"app"}],"volumes":[{"name":"creds","secret":{"secretName":"other"}}]}),
            )?,
        ];
        assert_eq!(
            users("db", &pods),
            [
                ("mounts".to_owned(), "volume creds".to_owned()),
                ("projects".to_owned(), "volume mixed".to_owned()),
                (
                    "reads".to_owned(),
                    "all keys as env in init; 1 env from keys in app".to_owned()
                ),
                ("pulls".to_owned(), "image pull".to_owned()),
            ]
        );
        assert!(users("absent", &pods).is_empty());
        Ok(())
    }
}
