use crate::{Result, safety};
use serde_json::{Value, json};

#[derive(Clone, PartialEq, Eq)]
pub struct ResourceEdit {
    pub cpu_request: String,
    pub memory_request: String,
    pub cpu_limit: String,
    pub memory_limit: String,
}

/// One atomic container edit. An omitted budget preserves all existing resources.
#[derive(Clone, PartialEq, Eq)]
pub struct ContainerEdit {
    pub image: String,
    pub resources: Option<ResourceEdit>,
}
impl ContainerEdit {
    pub fn validate(&self, container: &str) -> Result<()> {
        validate(container, &self.image)?;
        if let Some(resources) = &self.resources {
            resources.validate(container)?;
        }
        Ok(())
    }
}

/// Only the fields the controlled editor can change; no environment or credentials.
pub struct ContainerValues {
    pub name: String,
    pub image: String,
    /// Missing request/limit quantities are empty, so callers can show their absence.
    pub resources: ResourceEdit,
}

pub fn supports_resource(resource: &str) -> bool {
    matches!(
        resource,
        "deployments"
            | "deployments.apps"
            | "statefulsets"
            | "statefulsets.apps"
            | "daemonsets"
            | "daemonsets.apps"
            | "cronjobs"
            | "cronjobs.batch"
    )
}

fn container_path(object: &Value) -> Result<&'static str> {
    match (object["apiVersion"].as_str(), object["kind"].as_str()) {
        (Some("apps/v1"), Some("Deployment" | "StatefulSet" | "DaemonSet")) => {
            Ok("/template/spec/containers")
        }
        (Some("batch/v1"), Some("CronJob")) => Ok("/jobTemplate/spec/template/spec/containers"),
        _ => Err("controlled container editing supports apps/v1 Deployments, StatefulSets and DaemonSets, and batch/v1 CronJobs".into()),
    }
}

pub fn container_values(object: &Value) -> Result<Vec<ContainerValues>> {
    check_gitops(object)?;
    let containers = object["spec"]
        .pointer(container_path(object)?)
        .and_then(Value::as_array)
        .ok_or("workload has no ordinary containers")?;
    if containers.is_empty() || containers.len() > 128 {
        return Err("controlled editing requires 1..128 ordinary containers".into());
    }
    let mut values = Vec::with_capacity(containers.len());
    for container in containers {
        let name = container["name"]
            .as_str()
            .ok_or("container name is missing")?;
        let image = container["image"]
            .as_str()
            .ok_or("container image is missing")?;
        validate(name, image)?;
        if values
            .iter()
            .any(|value: &ContainerValues| value.name == name)
        {
            return Err("container names must be unique".into());
        }
        let quantity = |section: &str, key: &str| -> Result<String> {
            match &container["resources"][section][key] {
                Value::Null => Ok(String::new()),
                Value::String(value)
                    if value.len() <= 128 && safety::safe_text(value, 128) == *value =>
                {
                    Ok(value.clone())
                }
                _ => Err("existing resource quantity exceeds limits or is malformed".into()),
            }
        };
        values.push(ContainerValues {
            name: name.into(),
            image: image.into(),
            resources: ResourceEdit {
                cpu_request: quantity("requests", "cpu")?,
                memory_request: quantity("requests", "memory")?,
                cpu_limit: quantity("limits", "cpu")?,
                memory_limit: quantity("limits", "memory")?,
            },
        });
    }
    Ok(values)
}

fn update_effect(object: &Value) -> &'static str {
    match object["kind"].as_str() {
        Some("CronJob") => "changes future Jobs; existing Jobs and Pods are unchanged",
        Some("StatefulSet" | "DaemonSet") => {
            "updates pod template; rollout follows its updateStrategy (OnDelete requires pod replacement)"
        }
        _ => "triggers rollout",
    }
}

pub(crate) fn expected_restart(object: &Value, timestamp: &str) -> Result<(Value, String)> {
    if !matches!(
        object["kind"].as_str(),
        Some("Deployment" | "StatefulSet" | "DaemonSet")
    ) {
        return Err("restart supports Deployments, StatefulSets and DaemonSets".into());
    }
    container_path(object)?;
    let mut spec = object["spec"].clone();
    let template = spec
        .get_mut("template")
        .and_then(Value::as_object_mut)
        .ok_or("workload has no pod template")?;
    let metadata = template.entry("metadata").or_insert_with(|| json!({}));
    if metadata.is_null() {
        *metadata = json!({});
    }
    let metadata = metadata
        .as_object_mut()
        .ok_or("pod template metadata is malformed")?;
    let annotations = metadata.entry("annotations").or_insert_with(|| json!({}));
    if annotations.is_null() {
        *annotations = json!({});
    }
    let annotations = annotations
        .as_object_mut()
        .ok_or("pod template annotations are malformed")?;
    annotations.insert("kborg.dev/restartedAt".into(), json!(timestamp));
    Ok((
        spec,
        format!(
            "set pod-template annotation kborg.dev/restartedAt={timestamp}; {}",
            update_effect(object)
        ),
    ))
}
impl ResourceEdit {
    pub fn validate(&self, container: &str) -> Result<()> {
        validate(container, "placeholder:1")?;
        let (cpu_request, cpu_round) = crate::metrics::normalize_cpu_quantity(&self.cpu_request)?;
        let (cpu_limit, limit_round) = crate::metrics::normalize_cpu_quantity(&self.cpu_limit)?;
        let (memory_request, mem_round) =
            crate::metrics::normalize_memory_quantity(&self.memory_request)?;
        let (memory_limit, mem_limit_round) =
            crate::metrics::normalize_memory_quantity(&self.memory_limit)?;
        if cpu_round
            || limit_round
            || mem_round
            || mem_limit_round
            || cpu_request == 0
            || memory_request == 0
            || cpu_request > cpu_limit
            || memory_request > memory_limit
            || cpu_limit > 64_000_000_000
            || memory_limit > 128 * 1024 * 1024 * 1024
        {
            return Err("resource edit requires exact positive requests <= limits; maximum 64 CPU cores and 128 GiB memory per container".into());
        }
        Ok(())
    }
    pub(crate) fn value(&self) -> Value {
        json!({"requests":{"cpu":self.cpu_request,"memory":self.memory_request},"limits":{"cpu":self.cpu_limit,"memory":self.memory_limit}})
    }
}

/// Deliberately conservative container and image grammar. No URLs, credentials or whitespace.
pub fn validate(container: &str, image: &str) -> Result<()> {
    if container.is_empty()
        || container.len() > 63
        || !container
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || !container.as_bytes()[0].is_ascii_alphanumeric()
        || !container.as_bytes()[container.len() - 1].is_ascii_alphanumeric()
    {
        return Err("container must be a lowercase DNS label of at most 63 bytes".into());
    }
    if image.is_empty()
        || image.len() > 512
        || !image
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-/:@".contains(&b))
        || !image.as_bytes()[0].is_ascii_alphanumeric()
        || image.contains("://")
        || image.contains("..")
        || image.contains("//")
        || image.ends_with(['/', ':', '@'])
        || image.matches('@').count() > 1
    {
        return Err("invalid image reference: use a bounded registry/repository:tag or registry/repository@sha256:digest".into());
    }
    if let Some((_, digest)) = image.split_once('@') {
        let hex = digest
            .strip_prefix("sha256:")
            .ok_or("only sha256 image digests are supported")?;
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("image digest must contain exactly 64 hexadecimal characters".into());
        }
    }
    Ok(())
}

pub(crate) fn expected_spec(
    object: &Value,
    container: &str,
    image: &str,
) -> Result<(Value, String)> {
    validate(container, image)?;
    check_gitops(object)?;
    let path = container_path(object)?;
    let mut spec = object["spec"].clone();
    let selected = selected_container(&mut spec, path, container)?;
    let previous = selected["image"]
        .as_str()
        .ok_or("container image is missing")?;
    if previous.len() > 512 {
        return Err("existing image exceeds review limit".into());
    }
    let diff = format!(
        "container {container} image ({}):\n- {}\n+ {image}",
        update_effect(object),
        safety::safe_lines(previous, 512),
    );
    selected["image"] = json!(image);
    Ok((spec, diff))
}
fn check_gitops(object: &Value) -> Result<()> {
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
            return Err("GitOps-managed workload: change the reviewed fields in its source repository and reconcile through its controller".into());
        }
    }
    Ok(())
}
fn selected_container<'a>(
    spec: &'a mut Value,
    path: &str,
    container: &str,
) -> Result<&'a mut Value> {
    let containers = spec
        .pointer_mut(path)
        .and_then(Value::as_array_mut)
        .ok_or("workload has no ordinary containers")?;
    if containers
        .iter()
        .filter(|v| v["name"].as_str() == Some(container))
        .count()
        != 1
    {
        return Err("container must identify exactly one existing ordinary container".into());
    }
    containers
        .iter_mut()
        .find(|v| v["name"].as_str() == Some(container))
        .ok_or_else(|| "container is missing".into())
}
pub(crate) fn expected_resources(
    object: &Value,
    container: &str,
    resources: &ResourceEdit,
) -> Result<(Value, String)> {
    resources.validate(container)?;
    check_gitops(object)?;
    let path = container_path(object)?;
    let mut spec = object["spec"].clone();
    let selected = selected_container(&mut spec, path, container)?;
    if selected["resources"].is_null() {
        selected["resources"] = json!({});
    }
    if !selected["resources"].is_object() {
        return Err("existing container resources are malformed".into());
    }
    let mut diff = format!(
        "container {container} resources ({}; other resource keys preserved):",
        update_effect(object),
    );
    for (section, key, value) in [
        ("requests", "cpu", &resources.cpu_request),
        ("requests", "memory", &resources.memory_request),
        ("limits", "cpu", &resources.cpu_limit),
        ("limits", "memory", &resources.memory_limit),
    ] {
        if !selected["resources"][section][key].is_null()
            && !selected["resources"][section][key].is_string()
        {
            return Err("existing resource quantity is malformed".into());
        }
        let previous = selected["resources"][section][key]
            .as_str()
            .unwrap_or("unset");
        if previous.len() > 128 {
            return Err("existing resource quantity exceeds review limit".into());
        }
        diff.push_str(&format!(
            "\n- {section}.{key}: {}\n+ {section}.{key}: {value}",
            safety::safe_text(previous, 128)
        ));
        if selected["resources"][section].is_null() {
            selected["resources"][section] = json!({});
        }
        if !selected["resources"][section].is_object() {
            return Err("existing resource section is malformed".into());
        }
        selected["resources"][section][key] = json!(value);
    }
    Ok((spec, diff))
}

pub(crate) fn expected_container(
    object: &Value,
    container: &str,
    edit: &ContainerEdit,
) -> Result<(Value, String)> {
    edit.validate(container)?;
    let (mut spec, mut diff) = expected_spec(object, container, &edit.image)?;
    if let Some(resources) = &edit.resources {
        let mut with_image = object.clone();
        with_image["spec"] = spec;
        let (changed, resources_diff) = expected_resources(&with_image, container, resources)?;
        spec = changed;
        diff.push('\n');
        diff.push_str(&resources_diff);
    } else {
        diff.push_str("\nResource requests and limits preserved.");
    }
    Ok((spec, diff))
}

/// A strategic-merge patch that includes only the reviewed ordinary container.
pub(crate) fn container_patch_spec(
    object: &Value,
    container: &str,
    image: Option<&str>,
    resources: Option<&ResourceEdit>,
) -> Result<Value> {
    let path = container_path(object)?;
    let mut changed = json!({"name":container});
    if let Some(image) = image {
        validate(container, image)?;
        changed["image"] = json!(image);
    }
    if let Some(resources) = resources {
        resources.validate(container)?;
        changed["resources"] = resources.value();
    }
    if image.is_none() && resources.is_none() {
        return Err("container edit has no reviewed fields".into());
    }
    if path.starts_with("/jobTemplate/") {
        Ok(json!({"jobTemplate":{"spec":{"template":{"spec":{"containers":[changed]}}}}}))
    } else {
        Ok(json!({"template":{"spec":{"containers":[changed]}}}))
    }
}

pub(crate) fn check_admission(response: &Value, expected: &Value, original: &Value) -> Result<()> {
    check_metadata(response, original)?;
    if response["spec"] != *expected {
        return Err(
            "admission changed workload fields during dry-run; operation blocked for review".into(),
        );
    }
    Ok(())
}

pub(crate) fn check_resource_admission(
    response: &Value,
    expected: &Value,
    original: &Value,
    container: &str,
) -> Result<()> {
    // Canonicalize only the four reviewed quantities; compare every other field
    // strictly so equivalent Kubernetes spellings do not mask admission effects.
    let mut response = response.clone();
    let mut expected = expected.clone();
    let path = container_path(original)?;
    for spec in [&mut response["spec"], &mut expected] {
        let selected = selected_container(spec, path, container)?;
        for section in ["requests", "limits"] {
            for key in ["cpu", "memory"] {
                let quantity = selected["resources"][section][key]
                    .as_str()
                    .ok_or("admitted resource quantity is missing")?;
                let (value, rounded) = if key == "cpu" {
                    crate::metrics::normalize_cpu_quantity(quantity)?
                } else {
                    crate::metrics::normalize_memory_quantity(quantity)?
                };
                if rounded {
                    return Err("admission changed a resource quantity to an inexact value".into());
                }
                selected["resources"][section][key] = json!(value);
            }
        }
    }
    check_admission(&response, &expected, original)
}

pub(crate) fn check_metadata(response: &Value, original: &Value) -> Result<()> {
    for field in [
        "labels",
        "annotations",
        "ownerReferences",
        "finalizers",
        "name",
        "namespace",
        "uid",
    ] {
        if field == "annotations" && daemonset_generation_only(response, original) {
            continue;
        }
        if response["metadata"][field] != original["metadata"][field] {
            return Err(
                "admission changed workload metadata during dry-run; operation blocked for review"
                    .into(),
            );
        }
    }
    Ok(())
}

fn daemonset_generation_only(response: &Value, original: &Value) -> bool {
    // apps/v1 converts internal DaemonSet Spec.TemplateGeneration to this
    // annotation. The update strategy increments it by exactly one when the
    // pod template changes (Kubernetes pkg/registry/apps/daemonset/strategy.go,
    // pkg/apis/apps/v1/conversion.go). Every other annotation stays exact.
    const COUNTER: &str = "deprecated.daemonset.template.generation";
    if original["apiVersion"] != "apps/v1"
        || response["apiVersion"] != "apps/v1"
        || original["kind"] != "DaemonSet"
        || response["kind"] != "DaemonSet"
        || original["spec"]["template"] == response["spec"]["template"]
    {
        return false;
    }
    let Some(before) = original["metadata"]["annotations"].as_object() else {
        return false;
    };
    let Some(after) = response["metadata"]["annotations"].as_object() else {
        return false;
    };
    let Some(old) = before.get(COUNTER).and_then(Value::as_str) else {
        return false;
    };
    let Ok(old_number) = old.parse::<i64>() else {
        return false;
    };
    let Some(next) = old_number.checked_add(1) else {
        return false;
    };
    if old_number < 1
        || old != old_number.to_string()
        || after.get(COUNTER).and_then(Value::as_str) != Some(next.to_string().as_str())
        || before.len() != after.len()
    {
        return false;
    }
    before
        .iter()
        .all(|(key, value)| key == COUNTER || after.get(key) == Some(value))
}

#[cfg(test)]
mod tests {
    #[test]
    fn resource_admission_permits_only_equivalent_quantity_spellings() -> Result<()> {
        let resources = ResourceEdit {
            cpu_request: "0.1".into(),
            memory_request: "64Mi".into(),
            cpu_limit: "1".into(),
            memory_limit: "128Mi".into(),
        };
        let original = object();
        let (expected, _) = expected_resources(&original, "api", &resources)?;
        let mut admitted = original.clone();
        admitted["spec"] = expected.clone();
        admitted["spec"]["template"]["spec"]["containers"][0]["resources"]["requests"]["cpu"] =
            json!("100m");
        admitted["spec"]["template"]["spec"]["containers"][0]["resources"]["limits"]["memory"] =
            json!("134217728");
        check_resource_admission(&admitted, &expected, &original, "api")?;
        admitted["spec"]["replicas"] = json!(99);
        assert!(check_resource_admission(&admitted, &expected, &original, "api").is_err());
        admitted["spec"] = expected.clone();
        admitted["spec"]["template"]["spec"]["containers"][0]["resources"]["limits"]["cpu"] =
            json!("2");
        assert!(check_resource_admission(&admitted, &expected, &original, "api").is_err());
        Ok(())
    }
    #[test]
    fn resource_edits_preserve_unreviewed_data_and_require_exact_bounded_quantities() -> Result<()>
    {
        let resources = ResourceEdit {
            cpu_request: "100m".into(),
            memory_request: "64Mi".into(),
            cpu_limit: "500m".into(),
            memory_limit: "128Mi".into(),
        };
        let mut original = object();
        original["spec"]["template"]["spec"]["containers"][0]["resources"] = json!({"requests":{"ephemeral-storage":"1Gi"},"limits":{"example.com/gpu":"1"},"claims":[{"name":"pool"}]});
        let (spec, diff) = expected_resources(&original, "api", &resources)?;
        assert_eq!(
            spec["template"]["spec"]["containers"][0]["resources"]["limits"]["example.com/gpu"],
            "1"
        );
        assert_eq!(
            spec["template"]["spec"]["containers"][0]["resources"]["requests"]["ephemeral-storage"],
            "1Gi"
        );
        assert_eq!(
            spec["template"]["spec"]["containers"][0]["env"][0]["value"],
            "private-canary"
        );
        assert!(!diff.contains("private-canary"));
        assert!(diff.contains("+ limits.cpu: 500m"));
        for invalid in ["0", "65", "NaN", "100n\u{001b}"] {
            let mut changed = resources.clone();
            changed.cpu_limit = invalid.into();
            assert!(changed.validate("api").is_err());
        }
        let mut reversed = resources.clone();
        reversed.memory_limit = "1Mi".into();
        assert!(reversed.validate("api").is_err());
        original["metadata"]["labels"] = json!({"argocd.argoproj.io/instance":"app"});
        assert!(expected_resources(&original, "api", &resources).is_err());
        Ok(())
    }
    #[test]
    fn independent_admission_metadata_changes_are_not_silently_approved() {
        let original = object();
        let (expected, _) =
            expected_spec(&original, "api", "nginx:2").unwrap_or_else(|e| panic!("{e}"));
        for (field, value) in [
            ("labels", json!({"security.example/bypass":"true"})),
            ("annotations", json!({"controller.example/action":"delete"})),
            ("ownerReferences", json!([{"uid":"different-parent"}])),
            ("finalizers", json!(["unexpected-controller.example/hold"])),
        ] {
            let mut admitted = original.clone();
            admitted["spec"] = expected.clone();
            admitted["metadata"][field] = value;
            assert!(
                check_admission(&admitted, &expected, &original).is_err(),
                "unreviewed {field} was accepted"
            );
        }
        let mut ordinary = original.clone();
        ordinary["spec"] = expected.clone();
        ordinary["metadata"]["managedFields"] = json!([{"manager":"kube-apiserver"}]);
        assert!(check_admission(&ordinary, &expected, &original).is_ok());
    }
    use super::*;
    fn object() -> Value {
        json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{},"spec":{"template":{"spec":{"containers":[{"name":"api","image":"nginx:1","env":[{"name":"TOKEN","value":"private-canary"}]}]}}}})
    }

    fn workload(kind: &str) -> Value {
        let mut object = object();
        object["kind"] = json!(kind);
        object["spec"]["template"]["spec"]["containers"]
            .as_array_mut()
            .unwrap_or_else(|| panic!("missing fixture containers"))
            .push(json!({"name":"sidecar","image":"example/sidecar:1","resources":{"requests":{"cpu":"25m"}}}));
        if kind == "CronJob" {
            object["apiVersion"] = json!("batch/v1");
            let template = object["spec"]["template"].take();
            object["spec"] = json!({"schedule":"*/5 * * * *","jobTemplate":{"spec":{"backoffLimit":2,"template":template}}});
        }
        object
    }

    #[test]
    fn unified_workload_edits_preserve_other_fields_and_emit_only_reviewed_patch() -> Result<()> {
        let budget = ResourceEdit {
            cpu_request: "100m".into(),
            memory_request: "64Mi".into(),
            cpu_limit: "500m".into(),
            memory_limit: "128Mi".into(),
        };
        for kind in ["Deployment", "StatefulSet", "DaemonSet", "CronJob"] {
            let original = workload(kind);
            let path = container_path(&original)?;
            let edit = ContainerEdit {
                image: "example/api:2".into(),
                resources: Some(budget.clone()),
            };
            let (spec, diff) = expected_container(&original, "api", &edit)?;
            let containers = spec.pointer(path).ok_or("missing edited containers")?;
            assert_eq!(containers[0]["image"], "example/api:2");
            assert_eq!(containers[0]["resources"]["limits"]["memory"], "128Mi");
            assert_eq!(containers[0]["env"][0]["value"], "private-canary");
            assert_eq!(
                containers[1],
                original["spec"]
                    .pointer(path)
                    .ok_or("missing original containers")?[1]
            );
            assert!(!diff.contains("private-canary"));
            assert!(diff.contains("+ example/api:2"));
            let patch =
                container_patch_spec(&original, "api", Some(&edit.image), edit.resources.as_ref())?;
            let patched = patch
                .pointer(path)
                .and_then(Value::as_array)
                .ok_or("missing patch containers")?;
            assert_eq!(patched.len(), 1);
            assert_eq!(patched[0].as_object().map(serde_json::Map::len), Some(3));
            assert!(patched[0]["env"].is_null());
            let mut admitted = original.clone();
            admitted["spec"] = spec.clone();
            admitted["spec"]
                .pointer_mut(path)
                .ok_or("missing admitted containers")?[0]["resources"]["requests"]["cpu"] =
                json!("0.1");
            check_resource_admission(&admitted, &spec, &original, "api")?;
            admitted["spec"]
                .pointer_mut(path)
                .ok_or("missing admitted containers")?[1]["image"] = json!("unreviewed:2");
            assert!(check_resource_admission(&admitted, &spec, &original, "api").is_err());
            if kind == "CronJob" {
                assert!(diff.contains("future Jobs"));
                assert_eq!(spec["schedule"], original["spec"]["schedule"]);
                assert_eq!(spec["jobTemplate"]["spec"]["backoffLimit"], 2);
            }
        }
        Ok(())
    }

    #[test]
    fn editor_prefills_only_known_fields_and_absent_budget_stays_unchanged() -> Result<()> {
        let original = workload("CronJob");
        let values = container_values(&original)?;
        assert_eq!(values.len(), 2);
        assert_eq!(values[0].name, "api");
        assert_eq!(values[0].image, "nginx:1");
        assert!(values[0].resources.cpu_request.is_empty());
        assert_eq!(values[1].resources.cpu_request, "25m");
        let edit = ContainerEdit {
            image: "nginx:2".into(),
            resources: None,
        };
        let (spec, diff) = expected_container(&original, "api", &edit)?;
        let path = container_path(&original)?;
        let mut expected = original["spec"].clone();
        expected
            .pointer_mut(path)
            .ok_or("missing fixture containers")?[0]["image"] = json!("nginx:2");
        assert_eq!(spec, expected);
        assert!(diff.contains("Resource requests and limits preserved"));
        let patch = container_patch_spec(&original, "api", Some(&edit.image), None)?;
        assert_eq!(
            patch.pointer(path).ok_or("missing patch")?[0],
            json!({"name":"api","image":"nginx:2"})
        );
        for kind in ["Pod", "Job", "ReplicaSet", "CustomWorkload"] {
            let mut unsupported = workload("Deployment");
            unsupported["kind"] = json!(kind);
            assert!(container_values(&unsupported).is_err());
            assert!(expected_container(&unsupported, "api", &edit).is_err());
        }
        let mut custom = original;
        custom["apiVersion"] = json!("untrusted.example/v1");
        assert!(container_values(&custom).is_err());
        Ok(())
    }

    #[test]
    fn restart_preserves_template_metadata_and_explains_on_delete_strategy() -> Result<()> {
        for kind in ["Deployment", "StatefulSet", "DaemonSet"] {
            let mut original = workload(kind);
            original["spec"]["template"]["metadata"] =
                json!({"labels":{"app":"api"},"annotations":{"existing":"preserved"}});
            let (spec, diff) = expected_restart(&original, "12345")?;
            assert_eq!(
                spec["template"]["metadata"]["labels"],
                original["spec"]["template"]["metadata"]["labels"]
            );
            assert_eq!(
                spec["template"]["metadata"]["annotations"]["existing"],
                "preserved"
            );
            assert_eq!(
                spec["template"]["metadata"]["annotations"]["kborg.dev/restartedAt"],
                "12345"
            );
            if kind != "Deployment" {
                assert!(diff.contains("OnDelete requires pod replacement"));
            }
            original["spec"]["template"]["metadata"]["annotations"] = json!("malformed");
            assert!(expected_restart(&original, "12345").is_err());
        }
        assert!(expected_restart(&workload("CronJob"), "12345").is_err());
        Ok(())
    }

    #[test]
    fn daemonset_admission_accepts_only_the_native_template_counter_transition() -> Result<()> {
        const COUNTER: &str = "deprecated.daemonset.template.generation";
        let mut original = workload("DaemonSet");
        original["metadata"]["annotations"] = json!({COUNTER:"1","unchanged.example/note":"keep"});
        let (expected, _) = expected_spec(&original, "api", "nginx:2")?;
        let mut admitted = original.clone();
        admitted["spec"] = expected.clone();
        admitted["metadata"]["annotations"][COUNTER] = json!("2");
        check_admission(&admitted, &expected, &original)?;
        for invalid in [
            "0",
            "1",
            "3",
            "02",
            "-2",
            "9223372036854775808",
            "unreviewed",
        ] {
            admitted["metadata"]["annotations"][COUNTER] = json!(invalid);
            if invalid == "1" {
                // An unchanged counter is unchanged metadata, so it remains valid.
                check_admission(&admitted, &expected, &original)?;
            } else {
                assert!(
                    check_admission(&admitted, &expected, &original).is_err(),
                    "accepted counter {invalid}"
                );
            }
        }
        admitted["metadata"]["annotations"][COUNTER] = json!("2");
        admitted["metadata"]["annotations"]["unchanged.example/note"] = json!("changed");
        assert!(check_admission(&admitted, &expected, &original).is_err());
        admitted["metadata"]["annotations"]["unchanged.example/note"] = json!("keep");
        admitted["metadata"]["annotations"]["unexpected.example/key"] = json!("added");
        assert!(check_admission(&admitted, &expected, &original).is_err());
        admitted["metadata"]["annotations"]
            .as_object_mut()
            .ok_or("missing annotations")?
            .remove("unexpected.example/key");
        admitted["spec"] = original["spec"].clone();
        assert!(
            check_admission(&admitted, &original["spec"], &original).is_err(),
            "counter changed without pod template update"
        );
        admitted["spec"] = expected.clone();
        admitted["spec"]["template"]["spec"]["containers"][1]["image"] = json!("unreviewed:2");
        assert!(check_admission(&admitted, &expected, &original).is_err());
        for kind in ["Deployment", "StatefulSet", "CronJob"] {
            original["kind"] = json!(kind);
            admitted["kind"] = json!(kind);
            assert!(
                check_metadata(&admitted, &original).is_err(),
                "accepted DaemonSet counter exception for {kind}"
            );
        }
        Ok(())
    }
    #[test]
    fn conservative_references_and_diff() {
        for bad in [
            "",
            "x\nsecret",
            "https://registry/image",
            "x@token",
            "x@sha256:abc",
            "../x",
            "x;sh",
            "x\u{202e}",
        ] {
            assert!(validate("api", bad).is_err());
        }
        assert!(validate("api", "registry.example:5000/team/api:v2").is_ok());
        assert!(validate("api", &format!("x@sha256:{}", "a".repeat(64))).is_ok());
        let (spec, diff) =
            expected_spec(&object(), "api", "nginx:2").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            spec["template"]["spec"]["containers"][0]["env"][0]["value"],
            "private-canary"
        );
        assert!(!diff.contains("private-canary"));
        assert!(diff.contains("- nginx:1\n+ nginx:2"));
        assert!(expected_spec(&object(), "missing", "nginx:2").is_err());
    }
    #[test]
    fn admission_metadata_surprises_are_rejected() {
        let original = object();
        for field in [
            "labels",
            "annotations",
            "ownerReferences",
            "finalizers",
            "name",
            "namespace",
            "uid",
        ] {
            let mut response = original.clone();
            response["metadata"][field] = json!("unexpected");
            assert!(check_metadata(&response, &original).is_err());
        }
        let mut response = original.clone();
        response["metadata"]["resourceVersion"] = json!("9");
        response["metadata"]["managedFields"] = json!([]);
        assert!(check_metadata(&response, &original).is_ok());
    }
    #[test]
    fn gitops_and_admission_fail_closed() {
        let mut managed = object();
        managed["metadata"]["labels"] = json!({"kustomize.toolkit.fluxcd.io/name":"app"});
        assert!(expected_spec(&managed, "api", "nginx:2").is_err());
        let (expected, _) =
            expected_spec(&object(), "api", "nginx:2").unwrap_or_else(|e| panic!("{e}"));
        let mut response = json!({"spec":expected});
        assert!(check_admission(&response, &expected, &json!({})).is_ok());
        response["spec"]["replicas"] = json!(99);
        assert!(check_admission(&response, &expected, &json!({})).is_err());
    }
}
