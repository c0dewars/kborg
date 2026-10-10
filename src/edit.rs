use crate::{Result, safety};
use serde_json::{Value, json};

#[derive(Clone, PartialEq, Eq)]
pub struct ResourceEdit {
    pub cpu_request: String,
    pub memory_request: String,
    pub cpu_limit: String,
    pub memory_limit: String,
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
    let mut spec = object["spec"].clone();
    let selected = selected_container(&mut spec, container)?;
    let previous = selected["image"]
        .as_str()
        .ok_or("container image is missing")?;
    if previous.len() > 512 {
        return Err("existing image exceeds review limit".into());
    }
    let diff = format!(
        "container {container} image (triggers rollout):\n- {}\n+ {image}",
        safety::safe_lines(previous, 512)
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
fn selected_container<'a>(spec: &'a mut Value, container: &str) -> Result<&'a mut Value> {
    let containers = spec["template"]["spec"]["containers"]
        .as_array_mut()
        .ok_or("deployment has no ordinary containers")?;
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
    let mut spec = object["spec"].clone();
    let selected = selected_container(&mut spec, container)?;
    if selected["resources"].is_null() {
        selected["resources"] = json!({});
    }
    if !selected["resources"].is_object() {
        return Err("existing container resources are malformed".into());
    }
    let mut diff = format!(
        "container {container} resources (triggers rollout; other resource keys preserved):"
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

pub(crate) fn check_admission(response: &Value, expected: &Value, original: &Value) -> Result<()> {
    check_metadata(response, original)?;
    if response["spec"] != *expected {
        return Err(
            "admission changed deployment fields during dry-run; operation blocked for review"
                .into(),
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
    for spec in [&mut response["spec"], &mut expected] {
        let selected = selected_container(spec, container)?;
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
        if response["metadata"][field] != original["metadata"][field] {
            return Err("admission changed deployment metadata during dry-run; operation blocked for review".into());
        }
    }
    Ok(())
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
        json!({"metadata":{},"spec":{"template":{"spec":{"containers":[{"name":"api","image":"nginx:1","env":[{"name":"TOKEN","value":"private-canary"}]}]}}}})
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
