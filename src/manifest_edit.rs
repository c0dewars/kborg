//! Reviewed spec editing. Raw objects remain private; display projections never replace them.
use crate::{Result, gateway::Kind, safety};
use serde_json::{Value, json};
use std::{collections::BTreeSet, time::Instant};

pub const MAX_DOCUMENT: usize = 64 * 1024;
const MAX_CHANGES: usize = 128;

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
            "name":original["metadata"]["name"],"namespace":original["metadata"]["namespace"],
            "uid":original["metadata"]["uid"],"resourceVersion":original["metadata"]["resourceVersion"]},"spec":original["spec"]});
        safety::redact(&mut projection);
        let text = escape_non_ascii(&crate::inspect::yaml(&projection)?, true);
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
        if current["spec"] != self.original["spec"] {
            return Err("resource spec changed; reload and review again".into());
        }
        Ok(())
    }
    pub(crate) fn changes(&self, text: &str) -> Result<(Value, Value, String)> {
        let edited = parse(text)?;
        let mut protected = edited.clone();
        let spec = protected
            .as_object_mut()
            .ok_or("YAML must be one resource mapping")?
            .remove("spec")
            .ok_or("YAML spec is missing")?;
        let mut expected_protected = self.projection.clone();
        expected_protected
            .as_object_mut()
            .ok_or("invalid baseline")?
            .remove("spec");
        if protected != expected_protected {
            return Err(
                "only spec fields can be edited; identity, metadata and status are protected"
                    .into(),
            );
        }
        if !spec.is_object() {
            return Err("spec must remain a mapping".into());
        }
        let mut operations = Vec::new();
        let mut diff = Vec::new();
        let result = merge_reviewed(
            &self.original["spec"],
            &self.projection["spec"],
            &spec,
            "/spec",
            &mut operations,
            &mut diff,
        )?;
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
    pub(crate) expected_spec: Value,
    pub(crate) created: Instant,
    summary: String,
}
impl PreparedYaml {
    pub fn summary(&self) -> &str {
        &self.summary
    }
    pub fn confirmation(&self) -> String {
        format!("{}/{}", self.draft.namespace(), self.draft.name())
    }
    pub(crate) fn new(
        draft: Draft,
        patch: Value,
        expected_spec: Value,
        summary: String,
    ) -> Result<Self> {
        let summary = safety::review_text(&summary, MAX_DOCUMENT + 4096)?;
        Ok(Self {
            draft,
            patch,
            expected_spec,
            created: Instant::now(),
            summary,
        })
    }
}

fn check_scope(object: &Value, kind: &Kind) -> Result<()> {
    let namespace = object["metadata"]["namespace"].as_str().unwrap_or("");
    if !kind.namespaced
        || !safety::valid_id(namespace)
        || ["kube-system", "kube-public", "kube-node-lease"].contains(&namespace)
    {
        return Err("YAML editing requires a non-system namespaced resource".into());
    }
    if kind.kind.eq_ignore_ascii_case("Secret")
        || kind.plural.eq_ignore_ascii_case("secrets")
        || !object["spec"].is_object()
    {
        return Err("YAML editing supports non-Secret resources with a spec mapping; data, RBAC and cluster objects are excluded".into());
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
    safety::review_text(&encoded, 4096)
}
fn unchanged_projection(raw: &Value) -> bool {
    let mut safe = raw.clone();
    safety::redact(&mut safe);
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
    if let Some(before) = before {
        diff.push(format!("- {shown_path}: {}", display(before)?));
    }
    if let Some(after) = after {
        diff.push(format!("+ {shown_path}: {}", display(after)?));
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
    crate::edit::check_admission(response, expected, original)?;
    for field in ["apiVersion", "kind"] {
        if response[field] != original[field] {
            return Err("admission changed resource identity; YAML operation blocked".into());
        }
    }
    // Status is server-owned and may legitimately be refreshed during validation.
    let object = response.as_object().ok_or("invalid admission resource")?;
    let before = original.as_object().ok_or("invalid original resource")?;
    for key in object.keys().chain(before.keys()) {
        if !["metadata", "spec", "status"].contains(&key.as_str())
            && object.get(key) != before.get(key)
        {
            return Err("admission changed fields outside the reviewed spec".into());
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
            expected["template"]["spec"]["containers"][0]["env"][0]["value"],
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
        let mut response = draft.original.clone();
        response["spec"]["replicas"] = json!(4);
        assert!(check_admission(&response, &draft.original["spec"], &draft.original).is_err());
        assert!(
            check_applied(&response, &draft.original["spec"], &draft.original)
                .err()
                .is_some_and(|error| error.contains("already be applied"))
        );
        response = draft.original.clone();
        response["metadata"]["labels"]["injected"] = json!("yes");
        assert!(check_admission(&response, &draft.original["spec"], &draft.original).is_err());
        Ok(())
    }
}
