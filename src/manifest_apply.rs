//! Reviewed server-side apply of a local manifest file.
//!
//! Every document is dry-run before review, the review shows each create or
//! changed field, and execution rechecks every live object first: if any
//! object appeared, disappeared or changed since review, nothing is written.
//! Updates carry the reviewed resourceVersion, so a concurrent change fails
//! the write instead of being overwritten. Conflicts with other field
//! managers are reported, never forced.
use crate::{Result, gateway::Kind, safety};
use serde_json::{Map, Value};
use std::time::Instant;

pub const MAX_OBJECTS: usize = 16;
pub const FIELD_MANAGER: &str = "kborg";
const MAX_DIFF_LINES: usize = 120;
/// Server-owned or identity fields that never count as reviewed changes.
const IGNORED: &[&str] = &[
    "/metadata/resourceVersion",
    "/metadata/uid",
    "/metadata/generation",
    "/metadata/creationTimestamp",
    "/metadata/managedFields",
    "/metadata/selfLink",
    "/status",
];
/// Kinds that change who can do what; the review calls them out.
const PRIVILEGE_KINDS: &[&str] = &["Role", "RoleBinding", "ServiceAccount"];

pub struct Planned {
    pub(crate) kind: Kind,
    pub(crate) namespace: String,
    pub(crate) name: String,
    /// The apply body; updates include the reviewed resourceVersion as a lock.
    pub(crate) body: Value,
    /// `(uid, resourceVersion)` of the live object at review, or `None` to create.
    pub(crate) live: Option<(String, String)>,
}
impl Planned {
    pub fn label(&self) -> String {
        format!(
            "{} {}/{}",
            self.kind.kind.to_ascii_lowercase(),
            self.namespace,
            self.name
        )
    }
}

pub struct PreparedApply {
    pub(crate) session: String,
    pub(crate) created: Instant,
    pub(crate) objects: Vec<Planned>,
    file: String,
    summary: String,
}
impl PreparedApply {
    pub(crate) fn new(
        session: String,
        path: &std::path::Path,
        objects: Vec<Planned>,
        summary: String,
    ) -> Result<Self> {
        let file = path
            .file_name()
            .map(|name| safety::safe_text(&name.to_string_lossy(), 128))
            .filter(|name| !name.is_empty())
            .ok_or("manifest file needs a plain name")?;
        Ok(Self {
            session,
            created: Instant::now(),
            objects,
            summary: safety::review_text(&summary, 256 * 1024)?,
            file,
        })
    }
    pub fn summary(&self) -> &str {
        &self.summary
    }
    pub fn confirmation(&self) -> String {
        format!("apply {}", self.file)
    }
}

/// Expand `List` documents and reject anything apply does not support.
pub fn flatten(documents: Vec<Value>) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for document in documents {
        let kind = document["kind"].as_str().unwrap_or("");
        if kind == "OmittedSecret" {
            return Err("Secret documents are not applied: their values cannot be shown in a review. Use your secret manager or kubectl.".into());
        }
        if kind.ends_with("List") {
            let items = document["items"].as_array().cloned().unwrap_or_default();
            out.extend(flatten(items)?);
        } else {
            out.push(document);
        }
        if out.len() > MAX_OBJECTS {
            return Err(format!(
                "manifest has more than {MAX_OBJECTS} objects; split it"
            ));
        }
    }
    if out.is_empty() {
        return Err("manifest contains no objects".into());
    }
    Ok(out)
}

/// The catalog entry for a document's exact apiVersion and kind.
pub fn kind_for<'a>(kinds: &'a [Kind], document: &Value) -> Result<&'a Kind> {
    let api_version = document["apiVersion"].as_str().unwrap_or("");
    let kind = document["kind"].as_str().unwrap_or("");
    kinds
        .iter()
        .find(|candidate| {
            candidate.kind == kind
                && (if candidate.group.is_empty() {
                    candidate.version.clone()
                } else {
                    format!("{}/{}", candidate.group, candidate.version)
                }) == api_version
        })
        .ok_or_else(|| {
            format!(
                "{} {} is not served by this cluster; check apiVersion or run :discover",
                safety::safe_text(api_version, 120),
                safety::safe_text(kind, 64)
            )
        })
}

/// Validate identity and build the apply body for one document.
pub fn body(
    document: &Value,
    kind: &Kind,
    default_namespace: &str,
) -> Result<(String, String, Value)> {
    if !kind.namespaced {
        return Err(format!(
            "{} is cluster-scoped; apply supports namespaced objects only",
            kind.kind
        ));
    }
    if kind.kind == "Secret" {
        return Err("Secret documents are not applied".into());
    }
    let metadata = document["metadata"]
        .as_object()
        .ok_or("each object needs metadata.name")?;
    if metadata.contains_key("generateName") && !metadata.contains_key("name") {
        return Err("metadata.generateName is not supported; set metadata.name".into());
    }
    let name = metadata
        .get("name")
        .and_then(Value::as_str)
        .ok_or("each object needs metadata.name")?;
    if name.is_empty()
        || name.len() > 253
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
    {
        return Err("metadata.name must be a lowercase DNS name".into());
    }
    let namespace = match metadata.get("namespace").filter(|value| !value.is_null()) {
        Some(Value::String(namespace)) => namespace.as_str(),
        Some(_) => return Err(format!("{name}: metadata.namespace must be a string")),
        None if default_namespace != "*" && !default_namespace.is_empty() => default_namespace,
        None => {
            return Err(format!(
                "{name} has no namespace; select a namespace or set metadata.namespace"
            ));
        }
    };
    if !safety::valid_id(namespace)
        || ["kube-system", "kube-public", "kube-node-lease"].contains(&namespace)
    {
        return Err(format!("namespace {namespace} is invalid or protected"));
    }
    crate::edit::check_gitops(document)?;
    let mut body = document.clone();
    let object = body.as_object_mut().ok_or("object must be a mapping")?;
    object.remove("status");
    if let Some(Value::Object(metadata)) = object.get_mut("metadata") {
        for field in [
            "uid",
            "resourceVersion",
            "generation",
            "creationTimestamp",
            "managedFields",
            "selfLink",
        ] {
            metadata.remove(field);
        }
        metadata.insert("namespace".into(), Value::String(namespace.into()));
    }
    Ok((namespace.into(), name.into(), body))
}

fn flatten_paths(value: &Value, path: String, out: &mut Map<String, Value>) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (key, child) in map {
                let escaped = key.replace('~', "~0").replace('/', "~1");
                flatten_paths(child, format!("{path}/{escaped}"), out);
            }
        }
        Value::Array(items) if !items.is_empty() => {
            for (index, child) in items.iter().enumerate() {
                flatten_paths(child, format!("{path}/{index}"), out);
            }
        }
        _ => {
            out.insert(path, value.clone());
        }
    }
}

fn hidden(path: &str) -> bool {
    path.split('/').any(|segment| {
        let lower = segment.to_ascii_lowercase();
        [
            "data",
            "stringdata",
            "binarydata",
            "value",
            "token",
            "password",
        ]
        .contains(&lower.as_str())
            || lower.contains("secret")
            || lower.contains("credential")
    })
}

fn shown(path: &str, value: &Value) -> String {
    if hidden(path) {
        return "(value hidden)".into();
    }
    let text = match value {
        Value::String(text) => format!("{text:?}"),
        other => other.to_string(),
    };
    let text = safety::safe_text(&text, 160);
    crate::manifest_edit::escape_non_ascii(&text, false)
}

/// Changed leaf paths between the live object and the dry-run result.
pub fn diff(live: Option<&Value>, result: &Value) -> Vec<String> {
    let mut before = Map::new();
    let mut after = Map::new();
    if let Some(live) = live {
        flatten_paths(live, String::new(), &mut before);
    }
    flatten_paths(result, String::new(), &mut after);
    let ignored = |path: &str| {
        IGNORED
            .iter()
            .any(|prefix| path == *prefix || path.starts_with(&format!("{prefix}/")))
    };
    let mut lines = Vec::new();
    for (path, value) in &after {
        if ignored(path) {
            continue;
        }
        match before.get(path) {
            None => lines.push(format!("  + {path}: {}", shown(path, value))),
            Some(old) if old != value => lines.push(format!(
                "  ~ {path}: {} -> {}",
                shown(path, old),
                shown(path, value)
            )),
            _ => {}
        }
    }
    for (path, value) in &before {
        if !ignored(path) && !after.contains_key(path) {
            lines.push(format!("  - {path}: {}", shown(path, value)));
        }
    }
    if lines.len() > MAX_DIFF_LINES {
        let more = lines.len() - MAX_DIFF_LINES;
        lines.truncate(MAX_DIFF_LINES);
        lines.push(format!("  … {more} more changed fields"));
    }
    lines
}

pub fn privilege_note(kind: &Kind) -> Option<&'static str> {
    PRIVILEGE_KINDS
        .contains(&kind.kind.as_str())
        .then_some("  ! changes who may act in this namespace")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn kinds() -> Vec<Kind> {
        crate::gateway::kinds()
    }
    #[test]
    fn documents_are_flattened_bounded_and_secrets_refused() {
        let list = json!({"apiVersion":"v1","kind":"List","items":[{"kind":"A"},{"kind":"B"}]});
        assert_eq!(
            flatten(vec![list, json!({"kind":"C"})]).map(|v| v.len()),
            Ok(3)
        );
        assert!(flatten(vec![json!({"kind":"OmittedSecret"})]).is_err());
        assert!(flatten(vec![json!({"kind":"X"}); MAX_OBJECTS + 1]).is_err());
        assert!(flatten(vec![]).is_err());
    }
    #[test]
    fn bodies_need_namespaced_safe_identity_and_drop_server_fields() -> Result<()> {
        let kinds = kinds();
        let doc = json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"web","resourceVersion":"9","uid":"x"},"spec":{"replicas":2},"status":{"ready":1}});
        let kind = kind_for(&kinds, &doc)?;
        let (ns, name, body) = body(&doc, kind, "shop")?;
        assert_eq!((ns.as_str(), name.as_str()), ("shop", "web"));
        assert_eq!(
            body,
            json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"web","namespace":"shop"},"spec":{"replicas":2}})
        );
        assert!(body_err(
            &kinds,
            json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":"x"}}),
            "shop"
        ));
        assert!(body_err(
            &kinds,
            json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"x","namespace":"kube-system"}}),
            "shop"
        ));
        assert!(body_err(
            &kinds,
            json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"X"}}),
            "shop"
        ));
        assert!(body_err(
            &kinds,
            json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"x"}}),
            "*"
        ));
        assert!(body_err(
            &kinds,
            json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"x","labels":{"app.kubernetes.io/managed-by":"argocd"}}}),
            "shop"
        ));
        assert!(kind_for(&kinds, &json!({"apiVersion":"apps/v2","kind":"Deployment"})).is_err());
        Ok(())
    }
    #[test]
    fn non_string_namespace_is_rejected_not_replaced_by_the_default() -> Result<()> {
        // `namespace: 2024` in YAML is a number; it must not silently become
        // the selected namespace.
        let kinds = kinds();
        for namespace in [json!(2024), json!(true), json!(["a"]), json!({"a": 1})] {
            assert!(body_err(
                &kinds,
                json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"x","namespace":namespace}}),
                "shop"
            ));
        }
        let doc =
            json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"x","namespace":null}});
        let (ns, _, _) = body(&doc, kind_for(&kinds, &doc)?, "shop")?;
        assert_eq!(ns, "shop");
        Ok(())
    }
    fn body_err(kinds: &[Kind], doc: Value, ns: &str) -> bool {
        kind_for(kinds, &doc)
            .and_then(|kind| body(&doc, kind, ns))
            .is_err()
    }
    #[test]
    fn diff_lists_changes_hides_values_and_ignores_server_fields() {
        let live = json!({"metadata":{"name":"c","resourceVersion":"1","labels":{"a":"1"}},"data":{"k":"old"},"spec":{"replicas":1,"gone":true}});
        let result = json!({"metadata":{"name":"c","resourceVersion":"2","labels":{"a":"2"}},"data":{"k":"new"},"spec":{"replicas":3},"status":{"x":1}});
        assert_eq!(
            diff(Some(&live), &result),
            vec![
                "  ~ /data/k: (value hidden) -> (value hidden)",
                "  ~ /metadata/labels/a: \"1\" -> \"2\"",
                "  ~ /spec/replicas: 1 -> 3",
                "  - /spec/gone: true",
            ]
        );
        assert!(diff(None, &json!({"spec":{"a":"\u{1b}[2J"}}))[0].starts_with("  + /spec/a:"));
        assert!(!diff(None, &json!({"spec":{"a":"\u{1b}[2J"}}))[0].contains('\u{1b}'));
    }
}
