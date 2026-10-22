//! Resolve one custom resource API through narrow discovery and its exact CRD.
//! This reader never lists schemas or grants a write capability.
use crate::{
    Result,
    helm_extensions::{self, CrdDefinition},
    safety,
};
use http_body_util::BodyExt;
use kube::Client;
use serde_json::Value;
use std::time::Duration;

const LIMIT: usize = 1024 * 1024;
const BUDGET: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct Binding {
    pub definition: CrdDefinition,
    pub uid: String,
    pub resource_version: String,
}

fn group_version(api: &str) -> Result<(&str, &str)> {
    let (group, version) = api
        .split_once('/')
        .ok_or("custom resource needs group/version")?;
    if group.len() > 253
        || !group.contains('.')
        || !group.split('.').all(safety::valid_id)
        || !safety::valid_id(version)
        || ["k8s.io", "kubernetes.io"]
            .iter()
            .any(|suffix| group == *suffix || group.ends_with(&format!(".{suffix}")))
    {
        return Err("custom resource requires a non-system DNS API group and valid version".into());
    }
    Ok((group, version))
}

async fn document(client: &Client, path: &str) -> Result<Value> {
    tokio::time::timeout(BUDGET, async {
        let request = http::Request::get(path)
            .header(http::header::ACCEPT, "application/json")
            .body(kube::client::Body::empty()).map_err(|_| "invalid CRD discovery request")?;
        let response = client.send(request).await.map_err(|_| "CRD discovery transport failed; details suppressed")?;
        if response.status() != http::StatusCode::OK {
            return Err(format!("CRD discovery HTTP {}; install chart CRDs first with helm crds, or check exact schema read permission", response.status().as_u16()));
        }
        let bytes = http_body_util::Limited::new(response.into_body(), LIMIT).collect().await
            .map_err(|_| "CRD discovery response exceeded 1 MiB or failed")?.to_bytes();
        serde_json::from_slice(&bytes).map_err(|_| "invalid CRD discovery JSON; details suppressed".into())
    }).await.map_err(|_| "CRD discovery exceeded 10 seconds".to_owned())?
}

/// Validate a live schema while retaining its exact UID/RV. The built-in CRD
/// cleanup finalizer is the only exception to the bootstrap metadata policy.
pub fn validate_live(mut object: Value) -> Result<Binding> {
    if !CrdDefinition::established(&object) {
        return Err("custom resource CRD is not Established or is terminating".into());
    }
    let uid = object["metadata"]["uid"]
        .as_str()
        .filter(|value| !value.is_empty() && value.len() <= 253)
        .ok_or("CRD lacks a bounded UID")?
        .to_owned();
    let resource_version = object["metadata"]["resourceVersion"]
        .as_str()
        .filter(|value| !value.is_empty() && value.len() <= 253)
        .ok_or("CRD lacks a bounded resourceVersion")?
        .to_owned();
    if let Some(finalizers) = object["metadata"].get("finalizers") {
        let finalizers = finalizers.as_array().ok_or("invalid live CRD finalizers")?;
        if finalizers.len() > 1
            || finalizers
                .iter()
                .any(|value| value != "customresourcecleanup.apiextensions.k8s.io")
        {
            return Err("custom CRD finalizers require an independent schema review".into());
        }
        object["metadata"]
            .as_object_mut()
            .ok_or("invalid CRD metadata")?
            .remove("finalizers");
    }
    // These server fields do not alter the schema; UID/RV bind all live data.
    object
        .as_object_mut()
        .ok_or("invalid CRD object")?
        .remove("status");
    if let Some(metadata) = object["metadata"].as_object_mut() {
        for field in [
            "managedFields",
            "uid",
            "resourceVersion",
            "creationTimestamp",
            "generation",
        ] {
            metadata.remove(field);
        }
    }
    let definition = helm_extensions::crd_definition(object)?;
    Ok(Binding {
        definition,
        uid,
        resource_version,
    })
}

/// Read exactly /apis/GROUP/VERSION, then /customresourcedefinitions/PLURAL.GROUP.
pub async fn resolve(client: &Client, api: &str, kind: &str) -> Result<Binding> {
    let (group, _) = group_version(api)?;
    if kind.is_empty()
        || kind.len() > 63
        || !kind.as_bytes()[0].is_ascii_uppercase()
        || !kind.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err("invalid custom resource kind".into());
    }
    let catalog = document(client, &format!("/apis/{api}")).await?;
    if catalog["groupVersion"] != api || catalog["kind"] != "APIResourceList" {
        return Err("custom resource discovery returned another group/version".into());
    }
    let resources = catalog["resources"]
        .as_array()
        .filter(|values| values.len() <= 512)
        .ok_or("custom resource discovery is malformed or too large")?;
    let matches = resources
        .iter()
        .filter(|resource| {
            resource["kind"] == kind
                && resource["name"]
                    .as_str()
                    .is_some_and(|name| !name.contains('/'))
        })
        .collect::<Vec<_>>();
    if matches.len() != 1 || matches[0]["namespaced"] != true {
        return Err("custom resource discovery requires one unambiguous namespaced API".into());
    }
    let plural = matches[0]["name"]
        .as_str()
        .filter(|name| safety::valid_id(name))
        .ok_or("invalid custom resource API plural")?;
    let name = format!("{plural}.{group}");
    if name.len() > 253 {
        return Err("custom resource CRD name exceeds its bound".into());
    }
    let binding = read(client, &name).await?;
    if binding.definition.resource_api(api, kind).is_none()
        || binding.definition.plural != plural
        || binding.definition.name != name
    {
        return Err("custom resource discovery and live CRD schema disagree".into());
    }
    Ok(binding)
}

pub async fn read(client: &Client, name: &str) -> Result<Binding> {
    if name.len() > 253 || !name.contains('.') || !name.split('.').all(safety::valid_id) {
        return Err("invalid exact CRD read identity".into());
    }
    let object = document(
        client,
        &format!("/apis/apiextensions.k8s.io/v1/customresourcedefinitions/{name}"),
    )
    .await?;
    let binding = validate_live(object)?;
    if binding.definition.name != name {
        return Err("CRD read returned another identity".into());
    }
    Ok(binding)
}

pub async fn recheck(client: &Client, bindings: &[Binding]) -> Result<()> {
    for expected in bindings {
        let actual = read(client, &expected.definition.name).await?;
        if actual.uid != expected.uid
            || actual.resource_version != expected.resource_version
            || actual.definition.object != expected.definition.object
        {
            return Err(
                "CRD schema UID/resourceVersion changed after review; prepare again".into(),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn definition() -> Value {
        json!({"apiVersion":"apiextensions.k8s.io/v1","kind":"CustomResourceDefinition",
            "metadata":{"name":"widgets.example.test","uid":"schema-uid","resourceVersion":"3","finalizers":["customresourcecleanup.apiextensions.k8s.io"]},
            "spec":{"group":"example.test","scope":"Namespaced","names":{"kind":"Widget","plural":"widgets"},
                "versions":[{"name":"v1","served":true,"storage":true,"schema":{"openAPIV3Schema":{"type":"object"}}}]},
            "status":{"conditions":[{"type":"Established","status":"True"}]}})
    }
    fn mock(replies: Vec<(String, Value)>) -> Client {
        let replies = Arc::new(Mutex::new(replies.into_iter()));
        Client::new(
            tower::service_fn(move |request: http::Request<kube::client::Body>| {
                let replies = replies.clone();
                async move {
                    let (path, value) = replies
                        .lock()
                        .map_err(|_| std::io::Error::other("fixture lock"))?
                        .next()
                        .ok_or_else(|| std::io::Error::other("unexpected CRD request"))?;
                    assert_eq!(request.method(), http::Method::GET);
                    assert_eq!(request.uri().to_string(), path);
                    Ok::<_, std::io::Error>(http::Response::new(http_body_util::Full::new(
                        bytes::Bytes::from(value.to_string()),
                    )))
                }
            }),
            "test",
        )
    }
    #[test]
    fn resolve_is_narrow_and_recheck_rejects_changed_schema() -> Result<()> {
        tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|_| "runtime")?.block_on(async {
            let path = "/apis/apiextensions.k8s.io/v1/customresourcedefinitions/widgets.example.test";
            let mut changed = definition();
            changed["metadata"]["resourceVersion"] = json!("4");
            let client = mock(vec![
                ("/apis/example.test/v1".into(), json!({"kind":"APIResourceList","groupVersion":"example.test/v1","resources":[{"name":"widgets","kind":"Widget","namespaced":true},{"name":"widgets/status","kind":"Widget","namespaced":true}]})),
                (path.into(), definition()), (path.into(), changed),
            ]);
            let binding = resolve(&client, "example.test/v1", "Widget").await?;
            assert_eq!(binding.definition.plural, "widgets");
            assert!(recheck(&client, &[binding]).await.is_err());
            Ok(())
        })
    }
    #[test]
    fn live_schema_rejects_webhooks_cluster_scope_and_foreign_finalizers() -> Result<()> {
        assert!(validate_live(definition()).is_ok());
        for variant in 0..5 {
            let mut object = definition();
            match variant {
                0 => object["spec"]["conversion"] = json!({"strategy":"Webhook"}),
                1 => object["spec"]["scope"] = json!("Cluster"),
                2 => object["metadata"]["finalizers"] = json!(["foreign.example/finalizer"]),
                3 => object["metadata"]["deletionTimestamp"] = json!("now"),
                _ => object["status"]["conditions"] = json!([]),
            }
            assert!(validate_live(object).is_err());
        }
        for api in [
            "v1",
            "../../v1",
            "evil.k8s.io/v1",
            "example.test/v1?query",
            "example.test/v1/extra",
        ] {
            assert!(group_version(api).is_err());
        }
        Ok(())
    }
}
