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
    pub(crate) live_metadata: Value,
    pub stored_versions: Vec<String>,
    pub(crate) live_status: Value,
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
            return Err(crate::errors::http_status("CRD discovery (install chart CRDs first with helm crds, or check exact schema read permission)", response.status().as_u16(), &crate::errors::Scope::default()));
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
    let live_metadata = object["metadata"].clone();
    let live_status = object["status"].clone();
    let mut stored_versions = Vec::new();
    if let Some(versions) = live_status.get("storedVersions") {
        let versions = versions
            .as_array()
            .filter(|versions| versions.len() <= 8)
            .ok_or("CRD storedVersions is not a bounded version array")?;
        for version in versions {
            let version = version
                .as_str()
                .filter(|version| version.len() <= 63 && safety::valid_id(version))
                .ok_or("CRD storedVersions has an invalid version")?;
            if stored_versions.iter().any(|old| old == version) {
                return Err("CRD storedVersions contains a duplicate version".into());
            }
            stored_versions.push(version.to_owned());
        }
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
        live_metadata,
        stored_versions,
        live_status,
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

/// A complete cluster-wide snapshot. No pagination, omitted identities or
/// partial response is accepted for a schema-changing operation.
#[derive(Clone)]
pub struct Inventory {
    pub schema: String,
    pub api: kube::api::ApiResource,
    pub instances: Vec<Instance>,
}
#[derive(Clone)]
pub struct Instance {
    pub namespace: String,
    pub name: String,
    pub uid: String,
    pub resource_version: String,
    pub object: Value,
}
impl Inventory {
    pub fn same_membership(&self, other: &Self) -> bool {
        self.schema == other.schema
            && self.instances.len() == other.instances.len()
            && self.instances.iter().zip(&other.instances).all(|(a, b)| {
                a.namespace == b.namespace
                    && a.name == b.name
                    && a.uid == b.uid
                    && a.resource_version == b.resource_version
            })
    }
}
pub const MAX_INSTANCES: usize = 1000;
pub const MAX_INSTANCE_BYTES: usize = 8 * 1024 * 1024;

pub async fn inventory(
    client: &Client,
    binding: &Binding,
    count: &mut usize,
    bytes: &mut usize,
) -> Result<Inventory> {
    let version = binding.definition.object["spec"]["versions"]
        .as_array()
        .ok_or("invalid stored CRD versions")?
        .iter()
        .find(|version| version["storage"] == true && version["served"] == true)
        .and_then(|version| version["name"].as_str())
        .ok_or("CRD storage API unavailable")?;
    let api = binding
        .definition
        .resource_api(
            &format!("{}/{version}", binding.definition.group),
            &binding.definition.kind,
        )
        .ok_or("CRD storage API is not served")?;
    let path = format!(
        "/apis/{}/{}/{}?limit={}",
        api.group,
        api.version,
        api.plural,
        MAX_INSTANCES + 1
    );
    let object=tokio::time::timeout(BUDGET,async {
        let request=http::Request::get(&path).header(http::header::ACCEPT,"application/json").body(kube::client::Body::empty()).map_err(|_|"invalid custom instance request")?;
        let response=client.send(request).await.map_err(|_|"cluster-wide custom instance list transport failed; details suppressed")?;
        if response.status()!=http::StatusCode::OK { return Err(crate::errors::http_status("Cluster-wide custom instance list (all-namespace list permission is required; no schema action sent)", response.status().as_u16(), &crate::errors::Scope::default())); }
        let data=http_body_util::Limited::new(response.into_body(),MAX_INSTANCE_BYTES).collect().await.map_err(|_|"custom instance inventory exceeds8 MiB or failed")?.to_bytes();
        *bytes=bytes.checked_add(data.len()).ok_or("instance inventory size overflow")?;
        if *bytes>MAX_INSTANCE_BYTES {return Err("combined custom instance inventory exceeds8 MiB".into());}
        serde_json::from_slice::<Value>(&data).map_err(|_|"invalid custom instance inventory JSON; contents suppressed".into())
    }).await.map_err(|_|"cluster-wide custom instance list exceeded10 seconds".to_owned())??;
    parse_inventory(object, binding, api, count)
}
fn parse_inventory(
    object: Value,
    binding: &Binding,
    api: kube::api::ApiResource,
    count: &mut usize,
) -> Result<Inventory> {
    let expected_kind = binding.definition.object["spec"]["names"]["listKind"]
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{}List", api.kind));
    if object["apiVersion"] != api.api_version
        || object["kind"] != expected_kind
        || object["metadata"]["continue"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
        || (!object["metadata"]["continue"].is_null()
            && !object["metadata"]["continue"].is_string())
        || (!object["metadata"]["remainingItemCount"].is_null()
            && !object["metadata"]["remainingItemCount"].is_u64())
        || object["metadata"]["remainingItemCount"]
            .as_u64()
            .is_some_and(|value| value > 0)
    {
        return Err("custom instance inventory is incomplete or returned another API; no schema action sent".into());
    }
    let items = object["items"]
        .as_array()
        .ok_or("custom instance inventory omitted items")?;
    *count = count
        .checked_add(items.len())
        .ok_or("instance inventory count overflow")?;
    if *count > MAX_INSTANCES {
        return Err("combined schema impact exceeds1000 instances; no schema action sent".into());
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut instances = Vec::new();
    for object in items {
        let id = |key: &str| -> Result<String> {
            object["metadata"][key]
                .as_str()
                .filter(|value| !value.is_empty() && value.len() <= 253)
                .map(str::to_owned)
                .ok_or_else(|| format!("custom instance omitted bounded {key}"))
        };
        let namespace = id("namespace")?;
        let name = id("name")?;
        if !safety::valid_id(&namespace)
            || !name.split('.').all(safety::valid_id)
            || object["apiVersion"] != api.api_version
            || object["kind"] != api.kind
            || !seen.insert((namespace.clone(), name.clone()))
        {
            return Err(
                "custom instance inventory has invalid/duplicate/cross-API identity".into(),
            );
        }
        let uid = id("uid")?;
        let resource_version = id("resourceVersion")?;
        instances.push(Instance {
            namespace,
            name,
            uid,
            resource_version,
            object: object.clone(),
        });
    }
    instances.sort_by(|a, b| (&a.namespace, &a.name).cmp(&(&b.namespace, &b.name)));
    Ok(Inventory {
        schema: binding.definition.name.clone(),
        api,
        instances,
    })
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
    #[test]
    fn complete_inventory_rejects_pagination_malformed_identity_duplicates_and_global_bound()
    -> Result<()> {
        let binding = validate_live(definition())?;
        let api = binding
            .definition
            .resource_api("example.test/v1", "Widget")
            .ok_or("fixture API")?;
        let instance = json!({"apiVersion":"example.test/v1","kind":"Widget","metadata":{"namespace":"other","name":"a","uid":"uid-a","resourceVersion":"7"}});
        let valid = json!({"apiVersion":"example.test/v1","kind":"WidgetList","metadata":{"resourceVersion":"10"},"items":[instance]});
        let inventory = parse_inventory(valid.clone(), &binding, api.clone(), &mut 0)?;
        assert_eq!(inventory.instances[0].namespace, "other");
        for variant in 0..8 {
            let mut invalid = valid.clone();
            match variant {
                0 => invalid["metadata"]["continue"] = json!("next"),
                1 => invalid["metadata"]["remainingItemCount"] = json!(1),
                2 => invalid["items"][0]["metadata"]["uid"] = Value::Null,
                3 => invalid["items"][0]["metadata"]["namespace"] = json!("../other"),
                4 => invalid["items"] = json!([invalid["items"][0], invalid["items"][0]]),
                5 => invalid["apiVersion"] = json!("foreign.test/v1"),
                6 => invalid["metadata"]["continue"] = json!(false),
                _ => invalid["items"][0]["kind"] = json!("Foreign"),
            };
            assert!(parse_inventory(invalid, &binding, api.clone(), &mut 0).is_err());
        }
        let mut full = MAX_INSTANCES;
        assert!(parse_inventory(valid.clone(), &binding, api.clone(), &mut full).is_err());
        let mut changed = valid;
        changed["items"][0]["metadata"]["uid"] = json!("replacement");
        let changed = parse_inventory(changed, &binding, api, &mut 0)?;
        assert!(!inventory.same_membership(&changed));
        Ok(())
    }
}
