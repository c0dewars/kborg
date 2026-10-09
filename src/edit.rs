use crate::{Result, safety};
use serde_json::{Value, json};

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
            return Err("GitOps-managed workload: change the image in its source repository and reconcile through its controller".into());
        }
    }
    let mut spec = object["spec"].clone();
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
    let selected = containers
        .iter_mut()
        .find(|v| v["name"].as_str() == Some(container))
        .ok_or("container is missing")?;
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
