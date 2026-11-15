//! Reviewed YAML editing: every top-level field except identity and status,
//! plus metadata labels and annotations. Raw objects remain private; display
//! projections never replace them.
use crate::{Result, gateway::Kind, safety};
use serde_json::{Value, json};
use std::{collections::BTreeSet, time::Instant};

pub const MAX_DOCUMENT: usize = 64 * 1024;
const MAX_CHANGES: usize = 128;
/// Top-level fields that are never edited: identity and server-owned state.
const FIXED: [&str; 4] = ["apiVersion", "kind", "metadata", "status"];
/// The two metadata maps an operator edits.
const META: [&str; 2] = ["labels", "annotations"];

/// The editable part of an object: every top-level field except identity and
/// status (spec, data, rules, subjects...), and under `metadata` only labels
/// and annotations.
pub(crate) fn sections(object: &Value) -> serde_json::Map<String, Value> {
    let mut output = serde_json::Map::new();
    if let Some(map) = object.as_object() {
        for (key, value) in map {
            if !FIXED.contains(&key.as_str()) {
                output.insert(key.clone(), value.clone());
            }
        }
    }
    let mut metadata = serde_json::Map::new();
    for field in META {
        // An empty map is the same as no map, as the API server sees it.
        if let Some(value) = object["metadata"]
            .get(field)
            .filter(|value| !value.is_null() && value.as_object().is_none_or(|map| !map.is_empty()))
        {
            metadata.insert(field.into(), value.clone());
        }
    }
    output.insert("metadata".into(), Value::Object(metadata));
    output
}
/// Identity as shown in the editor; it must come back unchanged.
fn identity(object: &Value) -> Value {
    let mut metadata = object["metadata"].clone();
    if let Some(map) = metadata.as_object_mut() {
        for field in META {
            map.remove(field);
        }
    }
    json!({"apiVersion":object["apiVersion"],"kind":object["kind"],"metadata":metadata})
}
fn string_map(value: &Value) -> Result<()> {
    if value
        .as_object()
        .is_some_and(|map| map.values().all(Value::is_string))
    {
        Ok(())
    } else {
        Err(
            "labels and annotations must be mappings of text values (quote numbers and booleans)"
                .into(),
        )
    }
}
/// Write reviewed sections into an object (demo mode).
pub(crate) fn apply_expected(object: &mut Value, expected: &Value) {
    let Some(expected) = expected.as_object() else {
        return;
    };
    for (key, value) in expected {
        if key == "metadata" {
            for field in META {
                match value.get(field) {
                    Some(value) => object["metadata"][field] = value.clone(),
                    None => {
                        if let Some(metadata) = object["metadata"].as_object_mut() {
                            metadata.remove(field);
                        }
                    }
                }
            }
        } else {
            object[key.as_str()] = value.clone();
        }
    }
}

#[derive(Clone)]
pub struct Draft {
    pub(crate) original: Value,
    pub(crate) projection: Value,
    pub(crate) kind: Kind,
    pub(crate) session: String,
    text: String,
}
impl Draft {
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn resource(&self) -> String {
        self.kind.key()
    }
    pub fn namespace(&self) -> &str {
        self.original["metadata"]["namespace"]
            .as_str()
            .unwrap_or("")
    }
    pub fn name(&self) -> &str {
        self.original["metadata"]["name"].as_str().unwrap_or("")
    }
    pub(crate) fn new(original: Value, kind: Kind, session: String) -> Result<Self> {
        check_scope(&original, &kind)?;
        let mut projection = json!({"apiVersion":original["apiVersion"],"kind":original["kind"],"metadata":{
            "name":original["metadata"]["name"],
            "uid":original["metadata"]["uid"],"resourceVersion":original["metadata"]["resourceVersion"]}});
        // Cluster-scoped objects have no namespace line at all.
        if original["metadata"]["namespace"].is_string() {
            projection["metadata"]["namespace"] = original["metadata"]["namespace"].clone();
        }
        for (key, value) in sections(&original) {
            if key == "metadata" {
                for field in META {
                    if let Some(value) = value.get(field) {
                        projection["metadata"][field] = value.clone();
                    }
                }
            } else {
                projection[key.as_str()] = value;
            }
        }
        // What the screen shows is what can be edited: ConfigMap data,
        // annotations and literal values are in; credential-named fields
        // stay placeholders and are preserved.
        safety::redact_shown(&mut projection);
        let text = escape_non_ascii(&crate::inspect::yaml_editable(&projection)?, true);
        if text.len() > MAX_DOCUMENT {
            return Err("editable YAML exceeds 64 KiB; use a narrower operation".into());
        }
        Ok(Self {
            original,
            projection,
            kind,
            session,
            text,
        })
    }
    pub(crate) fn unchanged(&self, current: &Value) -> Result<()> {
        check_scope(current, &self.kind)?;
        for field in ["uid", "resourceVersion", "name", "namespace"] {
            if current["metadata"][field] != self.original["metadata"][field] {
                return Err(
                    "resource changed since the YAML template was opened; reload and review again"
                        .into(),
                );
            }
        }
        if sections(current) != sections(&self.original) {
            return Err("resource changed; reload and review again".into());
        }
        Ok(())
    }
    pub(crate) fn changes(&self, text: &str) -> Result<(Value, Value, String)> {
        let edited = parse(text)?;
        let fields = edited
            .as_object()
            .ok_or("YAML must be one resource mapping")?;
        if fields.contains_key("status") {
            return Err("status is server-owned and cannot be edited".into());
        }
        if identity(&edited) != identity(&self.projection) {
            return Err(
                "apiVersion, kind, name, namespace, uid and resourceVersion are protected; only labels and annotations change under metadata"
                    .into(),
            );
        }
        let (raw, base, next) = (
            sections(&self.original),
            sections(&self.projection),
            sections(&edited),
        );
        let mut operations = Vec::new();
        let mut diff = Vec::new();
        let mut result = serde_json::Map::new();
        let keys: BTreeSet<&String> = base.keys().chain(next.keys()).collect();
        for key in keys {
            if key == "metadata" {
                continue;
            }
            let at = pointer("", key);
            match (base.get(key), next.get(key)) {
                (Some(before), Some(after)) => {
                    result.insert(
                        key.clone(),
                        merge_reviewed(
                            raw.get(key).ok_or("invalid projection baseline")?,
                            before,
                            after,
                            &at,
                            &mut operations,
                            &mut diff,
                        )?,
                    );
                }
                (Some(_), None) => {
                    return Err(format!(
                        "top-level field {} cannot be removed; edit its contents instead",
                        safety::safe_text(key, 64)
                    ));
                }
                (None, Some(after)) => {
                    if sensitive_key(key)
                        || !unchanged_projection(after)
                        || protected_descendant(after)
                    {
                        return Err(
                            "cannot add hidden/sensitive fields through the YAML projection".into(),
                        );
                    }
                    record("add", &at, None, Some(after), &mut operations, &mut diff)?;
                    result.insert(key.clone(), after.clone());
                }
                (None, None) => {}
            }
        }
        let mut metadata = serde_json::Map::new();
        for field in META {
            let at = format!("/metadata/{field}");
            match (base["metadata"].get(field), next["metadata"].get(field)) {
                (Some(before), Some(after)) => {
                    // A hidden map comes back as its placeholder, untouched.
                    if before != after {
                        string_map(after)?;
                    }
                    metadata.insert(
                        field.into(),
                        merge_reviewed(
                            &raw["metadata"][field],
                            before,
                            after,
                            &at,
                            &mut operations,
                            &mut diff,
                        )?,
                    );
                }
                (Some(before), None) => {
                    if raw["metadata"][field] != *before {
                        return Err(
                            "cannot remove fields containing hidden values or protected metadata"
                                .into(),
                        );
                    }
                    record(
                        "remove",
                        &at,
                        Some(before),
                        None,
                        &mut operations,
                        &mut diff,
                    )?;
                }
                (None, Some(after)) => {
                    string_map(after)?;
                    if !unchanged_projection(after) {
                        return Err(
                            "cannot add hidden/sensitive fields through the YAML projection".into(),
                        );
                    }
                    record("add", &at, None, Some(after), &mut operations, &mut diff)?;
                    metadata.insert(field.into(), after.clone());
                }
                (None, None) => {}
            }
        }
        result.insert("metadata".into(), Value::Object(metadata));
        let result = Value::Object(result);
        if operations.is_empty() {
            return Err("YAML has no editable changes".into());
        }
        if operations.len() > MAX_CHANGES {
            return Err("YAML edit exceeds 128 field changes; split the edit".into());
        }
        operations.insert(0, json!({"op":"test","path":"/metadata/resourceVersion","value":self.original["metadata"]["resourceVersion"]}));
        operations.insert(
            0,
            json!({"op":"test","path":"/metadata/uid","value":self.original["metadata"]["uid"]}),
        );
        let diff = diff.join("\n");
        safety::review_text(&diff, MAX_DOCUMENT)?;
        Ok((Value::Array(operations), result, diff))
    }
}

pub struct PreparedYaml {
    pub(crate) draft: Draft,
    pub(crate) patch: Value,
    /// The reviewed [`sections`] after the edit.
    pub(crate) expected: Value,
    pub(crate) created: Instant,
    summary: String,
}
impl PreparedYaml {
    pub fn summary(&self) -> &str {
        &self.summary
    }
    /// `namespace/name`, or the name alone for a cluster-scoped object.
    pub fn confirmation(&self) -> String {
        if self.draft.namespace().is_empty() {
            self.draft.name().to_owned()
        } else {
            format!("{}/{}", self.draft.namespace(), self.draft.name())
        }
    }
    pub(crate) fn new(
        draft: Draft,
        patch: Value,
        expected: Value,
        summary: String,
    ) -> Result<Self> {
        let summary = safety::review_text(&summary, MAX_DOCUMENT + 4096)?;
        Ok(Self {
            draft,
            patch,
            expected,
            created: Instant::now(),
            summary,
        })
    }
}

fn check_scope(object: &Value, kind: &Kind) -> Result<()> {
    // System namespaces and cluster-scoped objects are the gateway policy's
    // call (operator config); here only the shape is checked.
    let namespace = object["metadata"]["namespace"].as_str().unwrap_or("");
    if kind.namespaced != !namespace.is_empty() || (kind.namespaced && !safety::valid_id(namespace))
    {
        return Err("resource namespace does not match its kind".into());
    }
    if kind.kind.eq_ignore_ascii_case("Secret") || kind.plural.eq_ignore_ascii_case("secrets") {
        return Err("Secrets are not edited here; their values are never shown".into());
    }
    for field in ["name", "uid", "resourceVersion"] {
        let value = object["metadata"][field].as_str().unwrap_or("");
        if value.is_empty() || value.len() > 253 || safety::safe_text(value, 253) != value {
            return Err("resource identity is missing or unsafe".into());
        }
    }
    if object["metadata"]["deletionTimestamp"].is_string() {
        return Err("terminating resources cannot be YAML-edited".into());
    }
    for field in ["labels", "annotations"] {
        if let Some(values) = object["metadata"][field].as_object()
            && (values.keys().any(|key| {
                key.starts_with("argocd.argoproj.io/")
                    || key.starts_with("kustomize.toolkit.fluxcd.io/")
                    || key.starts_with("helm.toolkit.fluxcd.io/")
                    || key.starts_with("fluxcd.io/")
            }) || values
                .get("app.kubernetes.io/managed-by")
                .and_then(Value::as_str)
                .is_some_and(|v| {
                    ["argocd", "flux", "fluxcd"].contains(&v.to_ascii_lowercase().as_str())
                }))
        {
            return Err("GitOps-managed resource: edit its source repository and reconcile through its controller".into());
        }
    }
    Ok(())
}

fn parse(text: &str) -> Result<Value> {
    if text.len() > MAX_DOCUMENT {
        return Err("editable YAML exceeds 64 KiB".into());
    }
    if text
        .chars()
        .any(|c| c != '\n' && safety::safe_text(&c.to_string(), 1).is_empty())
    {
        return Err("YAML contains terminal controls or unsupported text".into());
    }
    let options = serde_saphyr::options! {budget:serde_saphyr::budget!{max_events:20_000,max_aliases:0,max_anchors:0,max_depth:32,max_inclusion_depth:0,max_documents:1,max_nodes:10_000,max_total_scalar_bytes:MAX_DOCUMENT,max_merge_keys:0},merge_keys:serde_saphyr::MergeKeyPolicy::Error,duplicate_keys:serde_saphyr::DuplicateKeyPolicy::Error,with_snippet:false};
    let documents: Vec<Value> = serde_saphyr::from_multiple_with_options(text, options).map_err(|_| "invalid or excessive YAML; aliases, duplicate keys and multiple documents are forbidden (contents suppressed)")?;
    if documents.len() != 1 {
        return Err("provide exactly one YAML resource".into());
    }
    documents
        .into_iter()
        .next()
        .ok_or_else(|| "YAML is empty".into())
}
fn pointer(parent: &str, key: &str) -> String {
    format!("{parent}/{}", key.replace('~', "~0").replace('/', "~1"))
}
pub(crate) fn escape_non_ascii(text: &str, yaml: bool) -> String {
    let mut output = String::new();
    for c in text.chars() {
        if c.is_ascii() {
            output.push(c);
        } else if yaml && c as u32 > 0xffff {
            output.push_str(&format!("\\U{:08x}", c as u32));
        } else {
            for unit in c.encode_utf16(&mut [0; 2]) {
                output.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    output
}
fn display(value: &Value) -> Result<String> {
    let encoded = escape_non_ascii(
        &serde_json::to_string(value).map_err(|_| "cannot encode YAML change")?,
        false,
    );
    safety::review_text(&encoded, 16384)
}
/// A changed multi-line text (a ConfigMap file) as the lines that differ, so
/// a one-line change in a long file is reviewed as one line. Everything that
/// changes is listed; only the equal lines above and below are counted.
fn text_change(shown_path: &str, before: &str, after: &str, diff: &mut Vec<String>) -> Result<()> {
    let (old, new): (Vec<&str>, Vec<&str>) =
        (before.split('\n').collect(), after.split('\n').collect());
    let above = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let below = old[above..]
        .iter()
        .rev()
        .zip(new[above..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    diff.push(format!(
        "~ {shown_path}: text of {} lines becomes {} lines; {above} equal above, {below} equal below",
        old.len(),
        new.len()
    ));
    for (sign, lines) in [('-', &old), ('+', &new)] {
        for (index, line) in lines
            .iter()
            .enumerate()
            .take(lines.len() - below)
            .skip(above)
        {
            diff.push(format!(
                "{sign} {:>5}: {}",
                index + 1,
                display(&Value::String((*line).into()))?
            ));
        }
    }
    Ok(())
}
fn unchanged_projection(raw: &Value) -> bool {
    let mut safe = raw.clone();
    safety::redact_shown(&mut safe);
    safe == *raw
}
fn sensitive_key(key: &str) -> bool {
    let value = json!({key: null});
    !unchanged_projection(&value)
}
fn protected_descendant(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(key, value)| {
            key == "metadata" || sensitive_key(key) || protected_descendant(value)
        }),
        Value::Array(items) => items.iter().any(protected_descendant),
        _ => !unchanged_projection(value),
    }
}
fn record(
    op: &str,
    path: &str,
    before: Option<&Value>,
    after: Option<&Value>,
    operations: &mut Vec<Value>,
    diff: &mut Vec<String>,
) -> Result<()> {
    if operations.len() >= MAX_CHANGES {
        return Err("YAML edit exceeds 128 field changes".into());
    }
    safety::review_text(path, 1024)?;
    let shown_path = display(&Value::String(path.into()))?;
    match (before, after) {
        (Some(Value::String(old)), Some(Value::String(new)))
            if old.contains('\n') || new.contains('\n') =>
        {
            text_change(&shown_path, old, new, diff)?;
        }
        _ => {
            if let Some(before) = before {
                diff.push(format!("- {shown_path}: {}", display(before)?));
            }
            if let Some(after) = after {
                diff.push(format!("+ {shown_path}: {}", display(after)?));
            }
        }
    }
    let mut patch = json!({"op":op,"path":path});
    if let Some(after) = after {
        patch["value"] = after.clone();
    }
    operations.push(patch);
    Ok(())
}
fn merge_reviewed(
    raw: &Value,
    baseline: &Value,
    edited: &Value,
    path: &str,
    operations: &mut Vec<Value>,
    diff: &mut Vec<String>,
) -> Result<Value> {
    if baseline == edited {
        return Ok(raw.clone());
    }
    // Never let placeholders, sanitized strings or protected nested metadata become write input.
    if (!raw.is_object() && !raw.is_array() && raw != baseline)
        || (raw.is_object() != baseline.is_object())
        || (raw.is_array() != baseline.is_array())
        || path.ends_with("/metadata")
    {
        return Err(
            "hidden values and nested metadata are read-only; their source fields are preserved"
                .into(),
        );
    }
    match (raw, baseline, edited) {
        (Value::Object(raw), Value::Object(base), Value::Object(next)) => {
            let mut output = raw.clone();
            let keys: BTreeSet<_> = base.keys().chain(next.keys()).collect();
            for key in keys {
                let at = pointer(path, key);
                if (key == "metadata" || sensitive_key(key)) && base.get(key) != next.get(key) {
                    return Err("hidden values and nested metadata are read-only".into());
                }
                match (base.get(key), next.get(key)) {
                    (Some(before), Some(after)) => {
                        output.insert(
                            key.clone(),
                            merge_reviewed(
                                raw.get(key).ok_or("invalid projection baseline")?,
                                before,
                                after,
                                &at,
                                operations,
                                diff,
                            )?,
                        );
                    }
                    (Some(before), None) => {
                        if raw.get(key) != Some(before)
                            || protected_descendant(before)
                            || key == "metadata"
                        {
                            return Err("cannot remove fields containing hidden values or protected metadata".into());
                        }
                        record("remove", &at, Some(before), None, operations, diff)?;
                        output.remove(key);
                    }
                    (None, Some(after)) => {
                        let singleton = json!({key:after});
                        if key == "metadata"
                            || !unchanged_projection(&singleton)
                            || protected_descendant(after)
                        {
                            return Err(
                                "cannot add hidden/sensitive fields through the YAML projection"
                                    .into(),
                            );
                        }
                        record("add", &at, None, Some(after), operations, diff)?;
                        output.insert(key.clone(), after.clone());
                    }
                    _ => {}
                }
            }
            Ok(Value::Object(output))
        }
        (Value::Array(raw), Value::Array(base), Value::Array(next))
            if raw.len() == base.len() && base.len() == next.len() =>
        {
            let mut output = Vec::with_capacity(raw.len());
            for (index, ((raw, before), after)) in raw.iter().zip(base).zip(next).enumerate() {
                if (raw != before || protected_descendant(raw))
                    && ["name", "key", "mountPath", "containerPort"]
                        .iter()
                        .any(|key| before[*key] != after[*key])
                {
                    return Err("array item identity containing hidden values cannot be changed or reordered".into());
                }
                output.push(merge_reviewed(
                    raw,
                    before,
                    after,
                    &format!("{path}/{index}"),
                    operations,
                    diff,
                )?);
            }
            Ok(Value::Array(output))
        }
        _ => {
            if raw != baseline
                || protected_descendant(raw)
                || !unchanged_projection(edited)
                || protected_descendant(edited)
            {
                return Err("cannot replace a field or array containing hidden values".into());
            }
            record(
                "replace",
                path,
                Some(baseline),
                Some(edited),
                operations,
                diff,
            )?;
            Ok(edited.clone())
        }
    }
}

pub(crate) fn check_applied(response: &Value, expected: &Value, original: &Value) -> Result<()> {
    check_admission(response, expected, original).map_err(|_| "YAML write was accepted but its response differs from the reviewed fields; changes may already be applied. Inspect server state and audit before another write; no automatic rollback or retry.".into())
}

pub(crate) fn check_admission(response: &Value, expected: &Value, original: &Value) -> Result<()> {
    for field in ["apiVersion", "kind"] {
        if response[field] != original[field] {
            return Err("admission changed resource identity; YAML operation blocked".into());
        }
    }
    // Metadata must equal the original with the reviewed labels/annotations.
    let mut reference = original.clone();
    apply_expected(&mut reference, &json!({"metadata": expected["metadata"]}));
    crate::edit::check_metadata(response, &reference)?;
    // Status is server-owned and may legitimately be refreshed during
    // validation; every other field must be exactly what was reviewed.
    let after = sections(response);
    let reviewed = expected.as_object().ok_or("invalid reviewed YAML")?;
    for key in after.keys().chain(reviewed.keys()) {
        if key != "metadata" && after.get(key) != reviewed.get(key) {
            return Err(
                "admission changed fields outside the reviewed edit; operation blocked for review"
                    .into(),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn draft() -> Result<Draft> {
        Draft::new(
            json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"web","namespace":"demo","uid":"u1","resourceVersion":"8","labels":{"app":"web"}},"spec":{"replicas":2,"template":{"metadata":{"labels":{"app":"web"}},"spec":{"containers":[{"name":"web","image":"nginx:1","env":[{"name":"PASSWORD","value":"private-secret-canary"}]}]}}},"status":{"readyReplicas":2}}),
            crate::gateway::kinds()
                .into_iter()
                .find(|k| k.kind == "Deployment")
                .ok_or("fixture")?,
            "session".into(),
        )
    }
    fn encode(value: &Value) -> Result<String> {
        serde_json::to_string_pretty(value).map_err(|_| "fixture".into())
    }
    #[test]
    fn projection_edits_only_changed_paths_and_preserves_hidden_fields() -> Result<()> {
        let draft = draft()?;
        assert!(!draft.text().contains("private-secret-canary"));
        let mut edited = draft.projection.clone();
        edited["spec"]["replicas"] = json!(3);
        edited["spec"]["template"]["spec"]["containers"][0]["image"] = json!("nginx:2");
        let (patch, expected, diff) = draft.changes(&encode(&edited)?)?;
        assert_eq!(patch.as_array().map(Vec::len), Some(4));
        assert_eq!(patch[0]["path"], "/metadata/uid");
        assert_eq!(patch[1]["path"], "/metadata/resourceVersion");
        assert!(!patch.to_string().contains("private-secret-canary"));
        assert!(!diff.contains("private-secret-canary"));
        assert_eq!(
            expected["spec"]["template"]["spec"]["containers"][0]["env"][0]["value"],
            "private-secret-canary"
        );
        assert!(diff.contains("+ \"/spec/replicas\": 3"));
        Ok(())
    }
    #[test]
    fn hidden_identity_metadata_status_gitops_and_staleness_fail_closed() -> Result<()> {
        let draft = draft()?;
        for path in [
            "/metadata/uid",
            "/metadata/name",
            "/spec/template/spec/containers/0/env/0/value",
            "/spec/template/spec/containers/0/name",
            "/spec/template/metadata/labels/app",
        ] {
            let mut edited = draft.projection.clone();
            *edited.pointer_mut(path).ok_or("fixture path")? = json!("changed");
            assert!(draft.changes(&encode(&edited)?).is_err(), "accepted {path}");
        }
        let mut edited = draft.projection.clone();
        edited["status"] = json!({"readyReplicas":7});
        assert!(draft.changes(&encode(&edited)?).is_err());
        edited = draft.projection.clone();
        edited["spec"]["password"] = json!("new secret");
        assert!(draft.changes(&encode(&edited)?).is_err());
        edited["spec"]["template"]["spec"]["containers"] = json!([]);
        assert!(draft.changes(&encode(&edited)?).is_err());
        let mut live = draft.original.clone();
        live["metadata"]["resourceVersion"] = json!("9");
        assert!(draft.unchanged(&live).is_err());
        live = draft.original.clone();
        live["metadata"]["uid"] = json!("replacement");
        assert!(draft.unchanged(&live).is_err());
        live = draft.original.clone();
        live["metadata"]["annotations"] = json!({"argocd.argoproj.io/tracking-id":"app"});
        assert!(Draft::new(live, draft.kind.clone(), "x".into()).is_err());
        Ok(())
    }
    fn kind(name: &str) -> Result<Kind> {
        crate::gateway::kinds()
            .into_iter()
            .find(|k| k.kind == name)
            .ok_or_else(|| "fixture".into())
    }
    fn paths(patch: &Value) -> Vec<(String, String)> {
        patch
            .as_array()
            .into_iter()
            .flatten()
            .filter(|operation| operation["op"] != "test")
            .map(|operation| {
                (
                    operation["op"].as_str().unwrap_or("").to_owned(),
                    operation["path"].as_str().unwrap_or("").to_owned(),
                )
            })
            .collect()
    }
    #[test]
    fn top_level_fields_labels_and_annotations_are_editable() -> Result<()> {
        let original = json!({"apiVersion":"storage.k8s.io/v1","kind":"StorageClass","metadata":{"name":"fast","uid":"u2","resourceVersion":"3","labels":{"app":"web"},"managedFields":[{"manager":"x"}]},"provisioner":"example.com/disk","parameters":{"mode":"fast","level":"info"}});
        let draft = Draft::new(original.clone(), kind("StorageClass")?, "session".into())?;
        assert!(draft.text().contains("mode") && draft.text().contains("labels"));
        assert!(!draft.text().contains("managedFields"));
        let mut edited = draft.projection.clone();
        edited["parameters"]["mode"] = json!("safe");
        edited["parameters"]["extra"] = json!("1");
        edited["parameters"]
            .as_object_mut()
            .ok_or("fixture")?
            .remove("level");
        edited["metadata"]["labels"]["tier"] = json!("backend");
        edited["metadata"]["annotations"] = json!({"example.com/owner":"ops"});
        let (patch, expected, diff) = draft.changes(&encode(&edited)?)?;
        let mut changed = paths(&patch);
        changed.sort();
        assert_eq!(
            changed,
            [
                ("add", "/metadata/annotations"),
                ("add", "/metadata/labels/tier"),
                ("add", "/parameters/extra"),
                ("remove", "/parameters/level"),
                ("replace", "/parameters/mode"),
            ]
            .map(|(op, path)| (op.to_owned(), path.to_owned()))
        );
        assert!(diff.contains("+ \"/parameters/mode\": \"safe\""), "{diff}");
        assert_eq!(expected["parameters"], json!({"mode":"safe","extra":"1"}));
        assert_eq!(expected["provisioner"], "example.com/disk");
        assert_eq!(expected["metadata"]["labels"]["tier"], "backend");
        // What the server returns after the patch passes the admission check;
        // anything else it changed does not.
        let mut response = original.clone();
        apply_expected(&mut response, &expected);
        assert_eq!(
            response["metadata"]["annotations"]["example.com/owner"],
            "ops"
        );
        assert!(check_admission(&response, &expected, &original).is_ok());
        response["parameters"]["injected"] = json!("x");
        assert!(check_admission(&response, &expected, &original).is_err());
        response = original.clone();
        apply_expected(&mut response, &expected);
        response["metadata"]["labels"]["injected"] = json!("x");
        assert!(check_admission(&response, &expected, &original).is_err());
        // Removing every label is one reviewed removal; an empty map is no map.
        let mut edited = draft.projection.clone();
        edited["metadata"]["labels"] = json!({});
        let (patch, expected, _) = draft.changes(&encode(&edited)?)?;
        assert_eq!(
            paths(&patch),
            [("remove".to_owned(), "/metadata/labels".to_owned())]
        );
        assert!(expected["metadata"].get("labels").is_none());
        // Label values are text; identity, status and whole sections stay put.
        for (path, value) in [
            ("/metadata/labels/app", json!(3)),
            ("/metadata/name", json!("other")),
            ("/kind", json!("Secret")),
            ("/status", json!({"x":1})),
            ("/metadata/finalizers", json!(["x"])),
            ("/metadata/namespace", json!("demo")),
        ] {
            let mut edited = draft.projection.clone();
            match edited.pointer_mut(path) {
                Some(slot) => *slot = value,
                None => {
                    let (parent, key) = path.rsplit_once('/').ok_or("fixture")?;
                    let parent = if parent.is_empty() {
                        &mut edited
                    } else {
                        edited.pointer_mut(parent).ok_or("fixture")?
                    };
                    parent[key] = value;
                }
            }
            assert!(draft.changes(&encode(&edited)?).is_err(), "accepted {path}");
        }
        let mut edited = draft.projection.clone();
        edited
            .as_object_mut()
            .ok_or("fixture")?
            .remove("parameters");
        assert!(draft.changes(&encode(&edited)?).is_err());
        Ok(())
    }
    #[test]
    fn configmap_data_annotations_and_env_values_are_shown_and_editable() -> Result<()> {
        let file = "listen 80;\nserver_name old;\ngzip on;\n";
        let original = json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"settings","namespace":"demo","uid":"u4","resourceVersion":"7","annotations":{"note":"first"}},"data":{"mode":"blue","nginx.conf":file,"caf\u{e9}":"cr\u{e8}me\nbr\u{fb}l\u{e9}e\n"}});
        let draft = Draft::new(original.clone(), kind("ConfigMap")?, "session".into())?;
        // A file is a literal block; text the editor escapes stays quoted.
        assert!(
            draft.text().contains(
                "\"nginx.conf\": |\n    listen 80;\n    server_name old;\n    gzip on;\n"
            ),
            "{}",
            draft.text()
        );
        assert!(
            draft.text().contains("\"note\": \"first\"") && !draft.text().contains("br\u{fb}l")
        );
        // Unchanged text parses back to the same values, blocks included.
        assert_eq!(
            draft.changes(draft.text()).err().as_deref(),
            Some("YAML has no editable changes")
        );
        // One line of the file, one value and one annotation change.
        let text = draft
            .text()
            .replace("server_name old;", "server_name new;")
            .replace("\"mode\": \"blue\"", "\"mode\": \"green\"")
            .replace("\"note\": \"first\"", "\"note\": \"second\"");
        let (patch, expected, diff) = draft.changes(&text)?;
        assert_eq!(
            paths(&patch),
            [
                ("replace".to_owned(), "/data/mode".to_owned()),
                ("replace".to_owned(), "/data/nginx.conf".to_owned()),
                (
                    "replace".to_owned(),
                    "/metadata/annotations/note".to_owned()
                ),
            ]
        );
        assert_eq!(
            expected["data"]["nginx.conf"],
            json!("listen 80;\nserver_name new;\ngzip on;\n")
        );
        assert_eq!(expected["data"]["caf\u{e9}"], original["data"]["caf\u{e9}"]);
        // The review lists the changed line, not the whole file.
        assert!(
            diff.contains("text of 4 lines becomes 4 lines; 1 equal above, 2 equal below"),
            "{diff}"
        );
        assert!(
            diff.contains("-     2: \"server_name old;\"")
                && diff.contains("+     2: \"server_name new;\""),
            "{diff}"
        );
        assert!(!diff.contains("gzip"), "{diff}");
        let mut response = original.clone();
        apply_expected(&mut response, &expected);
        assert!(check_admission(&response, &expected, &original).is_ok());

        // A literal environment value is editable; a credential-named field is not.
        let original = json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"api","namespace":"demo","uid":"u5","resourceVersion":"9"},"spec":{"template":{"spec":{"containers":[{"name":"api","image":"api:1","env":[{"name":"LOG_LEVEL","value":"info"}],"password":"private-canary"}]}}}});
        let draft = Draft::new(original, kind("Deployment")?, "session".into())?;
        assert!(draft.text().contains("\"value\": \"info\"") && !draft.text().contains("canary"));
        let (patch, expected, _) = draft.changes(
            &draft
                .text()
                .replace("\"value\": \"info\"", "\"value\": \"debug\""),
        )?;
        assert_eq!(
            paths(&patch),
            [(
                "replace".to_owned(),
                "/spec/template/spec/containers/0/env/0/value".to_owned()
            )]
        );
        assert_eq!(
            expected["spec"]["template"]["spec"]["containers"][0]["password"],
            json!("private-canary")
        );
        assert!(
            draft
                .changes(&draft.text().replace("[REDACTED]", "guess"))
                .is_err()
        );
        Ok(())
    }
    #[test]
    fn cluster_scoped_objects_and_rbac_rules_are_editable_and_secrets_are_not() -> Result<()> {
        let original = json!({"apiVersion":"rbac.authorization.k8s.io/v1","kind":"ClusterRole","metadata":{"name":"reader","uid":"u3","resourceVersion":"5"},"rules":[{"apiGroups":[""],"resources":["pods"],"verbs":["get"]}]});
        let draft = Draft::new(original, kind("ClusterRole")?, "session".into())?;
        assert_eq!(draft.namespace(), "");
        assert!(!draft.text().contains("namespace"), "{}", draft.text());
        let mut edited = draft.projection.clone();
        edited["rules"][0]["verbs"] = json!(["get", "list"]);
        edited["metadata"]["labels"] = json!({"team":"platform"});
        let (patch, expected, _) = draft.changes(&encode(&edited)?)?;
        assert_eq!(
            paths(&patch),
            [
                ("replace".to_owned(), "/rules/0/verbs".to_owned()),
                ("add".to_owned(), "/metadata/labels".to_owned()),
            ]
        );
        assert_eq!(expected["rules"][0]["verbs"], json!(["get", "list"]));
        let prepared = PreparedYaml::new(draft.clone(), patch, expected, "review".into())?;
        assert_eq!(prepared.confirmation(), "reader");
        // A namespace on a cluster-scoped kind, or none on a namespaced one, is refused.
        let mut wrong = draft.original.clone();
        wrong["metadata"]["namespace"] = json!("demo");
        assert!(Draft::new(wrong, kind("ClusterRole")?, "x".into()).is_err());
        let mut wrong = draft.original.clone();
        wrong["kind"] = json!("ConfigMap");
        assert!(Draft::new(wrong, kind("ConfigMap")?, "x".into()).is_err());
        let secret = json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":"s","namespace":"demo","uid":"u","resourceVersion":"1"},"data":{"k":"dg=="}});
        let mut secrets = kind("ConfigMap")?;
        secrets.kind = "Secret".into();
        secrets.plural = "secrets".into();
        assert!(Draft::new(secret, secrets, "x".into()).is_err());
        Ok(())
    }
    #[test]
    fn unicode_template_roundtrips_and_hidden_placeholder_values_stay_locked() -> Result<()> {
        let mut baseline = draft()?;
        baseline.original["spec"]["description"] = json!("café \u{1f31f}");
        baseline.original["spec"]["café"] = json!(1);
        baseline.original["spec"]["password"] = json!("[REDACTED]");
        let draft = Draft::new(baseline.original, baseline.kind, baseline.session)?;
        assert!(draft.text().is_ascii());
        let mut edited = parse(draft.text())?;
        assert_eq!(edited["spec"]["description"], "café \u{1f31f}");
        edited["spec"]["replicas"] = json!(3);
        edited["spec"]["café"] = json!(2);
        let (patch, _, diff) = draft.changes(&encode(&edited)?)?;
        assert!(diff.is_ascii());
        assert!(diff.contains("/spec/caf\\u00e9"));
        assert!(
            patch
                .as_array()
                .ok_or("patch")?
                .iter()
                .any(|operation| operation["path"] == "/spec/café")
        );
        edited["spec"]["password"] = json!("new");
        assert!(draft.changes(&encode(&edited)?).is_err());
        let mut edited = draft.projection.clone();
        edited["spec"]
            .as_object_mut()
            .ok_or("fixture spec")?
            .remove("template");
        assert!(draft.changes(&encode(&edited)?).is_err());
        Ok(())
    }
    #[test]
    fn aliases_duplicates_documents_control_and_admission_mutations_rejected() -> Result<()> {
        for input in [
            "spec: 1\nspec: 2",
            "spec: &a {x: 1}\nx: *a",
            "---\nspec: {}\n---\nspec: {}",
            "spec: \u{1b}[2J",
            "spec: [",
        ] {
            assert!(parse(input).is_err());
        }
        let draft = draft()?;
        assert!(draft.changes(draft.text()).is_err());
        let unchanged = Value::Object(sections(&draft.original));
        assert!(check_admission(&draft.original, &unchanged, &draft.original).is_ok());
        let mut response = draft.original.clone();
        response["spec"]["replicas"] = json!(4);
        assert!(check_admission(&response, &unchanged, &draft.original).is_err());
        assert!(
            check_applied(&response, &unchanged, &draft.original)
                .err()
                .is_some_and(|error| error.contains("already be applied"))
        );
        response = draft.original.clone();
        response["metadata"]["labels"]["injected"] = json!("yes");
        assert!(check_admission(&response, &unchanged, &draft.original).is_err());
        Ok(())
    }
}
