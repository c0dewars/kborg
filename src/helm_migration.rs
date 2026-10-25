//! Reviewed manual CRD storage migration, following Kubernetes' list/write
//! procedure. Reads alone never prove backing storage. A temporary annotation
//! forces acknowledged UID/RV-guarded writes through the approved target API.
use crate::{
    Result,
    helm_crds::{self, Binding, Instance, Inventory},
    helm_extensions::{self, CrdDefinition, CrdPlan},
    safety,
};
use http_body_util::BodyExt;
use kube::{
    Api, Client,
    api::{ApiResource, DynamicObject, PostParams},
};
use ring::{
    digest::{SHA256, digest},
    rand::{SecureRandom, SystemRandom},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{collections::BTreeSet, time::Duration};

const MARKER: &str = "kborg.dev/storage-migration";
const PREFIX: &str = "kborg-storage-";
const BUDGET: Duration = Duration::from_secs(10);
const MAX_REVIEW: usize = 512 * 1024;
#[derive(Clone)]
pub struct Entry {
    pub before: Binding,
    pub desired: CrdDefinition,
    pub inventory: Inventory,
    pub source_version: String,
    pub target_version: String,
    pub stored_versions: Vec<String>,
    pub controller: Option<Controller>,
}
#[derive(Clone)]
pub struct Controller {
    pub api: ApiResource,
    pub name: String,
    pub object: Value,
}
#[derive(Debug, Serialize)]
pub struct ControllerResult {
    pub api_version: String,
    pub name: String,
    pub uid: String,
}
pub struct Plan {
    entries: Vec<Entry>,
    namespace: String,
    release: String,
    marker: String,
    canonical: String,
}
#[derive(Default, Debug, Serialize)]
pub struct Report {
    pub switched: usize,
    pub rewritten: usize,
    pub finalized: usize,
    pub pending: usize,
    pub complete: bool,
    pub controller_migrations: Vec<ControllerResult>,
}
impl Plan {
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
    pub fn canonical(&self) -> &str {
        &self.canonical
    }
    pub fn instance_count(&self) -> usize {
        self.entries
            .iter()
            .map(|entry| entry.inventory.instances.len())
            .sum()
    }
    pub fn review(&self) -> Result<String> {
        let mut text = format!(
            "STORAGE MIGRATION / ALL NAMESPACES\nConfirmation label: {}/{}; this label does not restrict schema/instance scope.\nOnly identical structural schemas with None conversion. Existing served APIs are retained.\nFirst switch each reviewed schema to its target storage version; then rewrite EVERY captured instance through that target API with exact UID/resourceVersion.\nA temporary {} annotation is set and removed, forcing two acknowledged RV-advancing writes while preserving object content/status. Existing interrupted KBorg markers are cleared. A GET apiVersion is only conversion and is NEVER proof of persisted storage.\nNative writes alone do NOT prove complete backend migration. Only an available official StorageVersionMigration controller's fresh Succeeded result plus complete final inventory permits guarded status.storedVersions finalization. When its API is absent, history is retained and the operation is explicitly STAGED/PENDING. No automatic retry/rollback; partial results leave old storedVersions for a fresh complete review/rewrite.\nQuiesce schema writers/controllers and maintain a stable API-server storage configuration. Final checks cannot prevent a concurrent storage switch or in-flight old-handler write; this is a reviewed manual migration, not an HA coordination/etcd inspection service.\nBound: 1000 instances/8MiB total; incomplete, foreign, GitOps, controller-owned or terminating instances are refused. Known-sensitive fields redacted; private hashes bind exact inputs.\n",
            self.namespace, self.release, MARKER
        );
        for entry in &self.entries {
            text.push_str(&format!("\nSCHEMA {} UID {} RV {}\nStorage {} -> {}; recorded storedVersions {:?}\nBEFORE SCHEMA\n",entry.desired.name,entry.before.uid,entry.before.resource_version,entry.source_version,entry.target_version,entry.stored_versions));
            text.push_str(&review_value(entry.before.definition.object.clone())?);
            text.push_str("\nDESIRED SCHEMA\n");
            text.push_str(&review_value(entry.desired.object.clone())?);
            for instance in &entry.inventory.instances {
                text.push_str(&format!(
                    "\nREWRITE {}/{} UID {} RV {} via {}/{}\n",
                    instance.namespace,
                    instance.name,
                    instance.uid,
                    instance.resource_version,
                    entry.desired.group,
                    entry.target_version
                ));
                text.push_str(&review_value(instance.object.clone())?);
            }
            if let Some(controller) = &entry.controller {
                text.push_str(&format!(
                    "\nCREATE CLUSTER CONTROLLER MIGRATION {} {}\n",
                    controller.api.api_version, controller.name
                ));
                text.push_str(&review_value(controller.object.clone())?);
                text.push_str("\nController may continue after client cancellation and has its own retries; the migration object is retained for inspection and explicit cleanup. Concurrent additions can be touched by the official controller; quiesce writers before review.\n");
            } else {
                text.push_str("\nNo official StorageVersionMigration API discovered: native storage switch/rewrites only; historical storedVersions retained; completion PENDING.\n");
            }
            if text.len() > MAX_REVIEW {
                return Err(
                    "complete migration review exceeds512KiB; split schema operation".into(),
                );
            }
        }
        text.push_str(&format!(
            "\nExact migration plan SHA-256: {}\n",
            sha256(self.canonical.as_bytes())
        ));
        Ok(text)
    }
}
fn sha256(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn review_value(mut value: Value) -> Result<String> {
    helm_redact(&mut value)?;
    crate::inspect::yaml(&value)
}
fn helm_redact(value: &mut Value) -> Result<()> {
    crate::helm::redact_values(value, 0, &mut 0)?;
    safety::redact(value);
    Ok(())
}
fn schema_api() -> ApiResource {
    ApiResource {
        group: "apiextensions.k8s.io".into(),
        version: "v1".into(),
        api_version: "apiextensions.k8s.io/v1".into(),
        kind: "CustomResourceDefinition".into(),
        plural: "customresourcedefinitions".into(),
    }
}
fn failure(error: kube::Error, stage: &str) -> String {
    crate::errors::explain(
        &format!("CRD migration {stage}"),
        &error,
        &crate::errors::Scope::default(),
    )
}
fn dynamic(value: Value) -> Result<DynamicObject> {
    serde_json::from_value(value).map_err(|_| "invalid reviewed migration object".into())
}
fn schema_object(binding: &Binding, desired: &CrdDefinition) -> Result<DynamicObject> {
    let mut value = binding.definition.object.clone();
    value["spec"] = desired.object["spec"].clone();
    value["metadata"] = binding.live_metadata.clone();
    if let Some(metadata) = value["metadata"].as_object_mut() {
        metadata.remove("managedFields");
    }
    value["status"] = binding.live_status.clone();
    dynamic(value)
}
fn metadata_equal(before: &Value, after: &Value) -> bool {
    [
        "name",
        "namespace",
        "uid",
        "labels",
        "annotations",
        "finalizers",
        "ownerReferences",
    ]
    .iter()
    .all(|key| {
        let a = &before[key];
        let b = &after[key];
        a == b || (a.is_null() && b.as_array().is_some_and(Vec::is_empty))
    })
}
fn validate_instance(instance: &Instance, namespace: &str, release: &str) -> Result<()> {
    helm_extensions::validate_namespace(&instance.namespace)?;
    helm_extensions::check_gitops(&instance.object)?;
    let metadata = &instance.object["metadata"];
    if metadata["labels"]["app.kubernetes.io/managed-by"] != "Helm"
        || metadata["annotations"]["meta.helm.sh/release-name"] != release
        || metadata["annotations"]["meta.helm.sh/release-namespace"] != namespace
    {
        return Err(
            "storage migration refuses custom instances not owned by this exact Helm release"
                .into(),
        );
    }
    for key in ["ownerReferences", "finalizers"] {
        if metadata
            .get(key)
            .is_some_and(|value| value.as_array().is_none_or(|values| !values.is_empty()))
        {
            return Err(
                "migration requires quiescent instances without ownerReferences/finalizers".into(),
            );
        }
    }
    if !metadata["deletionTimestamp"].is_null() {
        return Err("migration refuses terminating instances".into());
    }
    if let Some(marker) = metadata["annotations"].get(MARKER) {
        let marker = marker
            .as_str()
            .filter(|marker| {
                marker.len() == PREFIX.len() + 32
                    && marker.starts_with(PREFIX)
                    && marker[PREFIX.len()..]
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())
            })
            .ok_or("foreign migration marker requires independent review")?;
        let _ = marker;
    }
    Ok(())
}
fn status_object(binding: &Binding, target: &str) -> Result<DynamicObject> {
    let mut value = binding.definition.object.clone();
    value["metadata"] = binding.live_metadata.clone();
    if let Some(metadata) = value["metadata"].as_object_mut() {
        metadata.remove("managedFields");
    }
    value["status"] = binding.live_status.clone();
    value["status"]["storedVersions"] = json!([target]);
    dynamic(value)
}
fn instance_object(
    instance: &Instance,
    version: &str,
    marker: Option<&str>,
) -> Result<DynamicObject> {
    let mut value = instance.object.clone();
    value["apiVersion"] = json!(version);
    let metadata = value["metadata"]
        .as_object_mut()
        .ok_or("instance metadata missing")?;
    metadata.remove("managedFields");
    let annotations = metadata
        .entry("annotations")
        .or_insert(json!({}))
        .as_object_mut()
        .ok_or("invalid instance annotations")?;
    if let Some(marker) = marker {
        annotations.insert(MARKER.into(), json!(marker));
    } else {
        annotations.remove(MARKER);
    }
    dynamic(value)
}
fn same_content(desired: &Value, actual: &Value) -> bool {
    fn normalized(mut value: Value) -> Value {
        if let Some(metadata) = value["metadata"].as_object_mut() {
            for key in ["resourceVersion", "managedFields"] {
                metadata.remove(key);
            }
            if metadata
                .get("annotations")
                .is_some_and(|value| value.as_object().is_some_and(serde_json::Map::is_empty))
            {
                metadata.remove("annotations");
            }
        }
        value
    }
    normalized(desired.clone()) == normalized(actual.clone())
}
async fn replace_schema(
    client: &Client,
    binding: &Binding,
    desired: &CrdDefinition,
    dry: bool,
) -> Result<Binding> {
    let api: Api<DynamicObject> = Api::all_with(client.clone(), &schema_api());
    let object = schema_object(binding, desired)?;
    let actual = tokio::time::timeout(
        BUDGET,
        api.replace(
            &desired.name,
            &PostParams {
                dry_run: dry,
                ..Default::default()
            },
            &object,
        ),
    )
    .await
    .map_err(|_| "schema switch request timed out".to_owned())?
    .map_err(|error| failure(error, "storage version switch"))?;
    let actual = serde_json::to_value(actual).map_err(|_| "invalid switched schema response")?;
    if !desired.matches_existing(&actual)
        || !metadata_equal(&binding.live_metadata, &actual["metadata"])
    {
        return Err(
            "admission changed reviewed migration schema/metadata; stop and reconcile".into(),
        );
    }
    helm_crds::validate_live(actual)
}
async fn status_update(
    client: &Client,
    binding: &Binding,
    target: &str,
    dry: bool,
) -> Result<Binding> {
    let api: Api<DynamicObject> = Api::all_with(client.clone(), &schema_api());
    let actual = tokio::time::timeout(
        BUDGET,
        api.replace_status(
            &binding.definition.name,
            &PostParams {
                dry_run: dry,
                ..Default::default()
            },
            &status_object(binding, target)?,
        ),
    )
    .await
    .map_err(|_| "guarded migration status update timed out".to_owned())?
    .map_err(|error| failure(error, "guarded storedVersions status update"))?;
    let actual = helm_crds::validate_live(
        serde_json::to_value(actual).map_err(|_| "invalid migration status response")?,
    )?;
    if actual.uid != binding.uid
        || actual.definition.object != binding.definition.object
        || !metadata_equal(&binding.live_metadata, &actual.live_metadata)
        || actual.stored_versions != [target]
    {
        return Err("status admission changed schema/identity or retained unexpected versions; stop and reconcile".into());
    }
    Ok(actual)
}
async fn instance_put(
    client: &Client,
    api: &ApiResource,
    instance: &Instance,
    marker: Option<&str>,
    dry: bool,
) -> Result<Instance> {
    let objects: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), &instance.namespace, api);
    let desired = instance_object(instance, &api.api_version, marker)?;
    let expected = serde_json::to_value(&desired).map_err(|_| "invalid desired instance")?;
    let actual = tokio::time::timeout(
        BUDGET,
        objects.replace(
            &instance.name,
            &PostParams {
                dry_run: dry,
                ..Default::default()
            },
            &desired,
        ),
    )
    .await
    .map_err(|_| "instance rewrite timed out; storage migration may be partial".to_owned())?
    .map_err(|error| failure(error, "target-version instance rewrite"))?;
    let value = serde_json::to_value(actual).map_err(|_| "invalid rewritten instance")?;
    let rv = value["metadata"]["resourceVersion"]
        .as_str()
        .filter(|rv| !rv.is_empty() && rv.len() <= 253)
        .ok_or("rewrite omitted bounded resourceVersion")?
        .to_owned();
    if !same_content(&expected, &value) || (!dry && rv == instance.resource_version) {
        return Err("instance rewrite did not prove an acknowledged content-preserving RV advance; old storedVersions retained".into());
    }
    Ok(Instance {
        namespace: instance.namespace.clone(),
        name: instance.name.clone(),
        uid: instance.uid.clone(),
        resource_version: rv,
        object: value,
    })
}

pub async fn prepare(
    client: &Client,
    desired: &CrdPlan,
    namespace: &str,
    release: &str,
) -> Result<Plan> {
    helm_extensions::validate_namespace(namespace)?;
    if !crate::helm::valid_release_name(release) {
        return Err("invalid migration release confirmation label".into());
    }
    if desired.definitions.is_empty() || desired.definitions.len() > helm_extensions::MAX_CRDS {
        return Err("migration chart contains no schemas".into());
    }
    let mut nonce = [0u8; 16];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| "cannot generate migration write marker")?;
    let marker = format!(
        "{PREFIX}{}",
        nonce
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let controller_api = discover_controller(client).await?;
    let mut entries = Vec::new();
    let mut count = 0;
    let mut bytes = 0;
    for desired in &desired.definitions {
        let checked = helm_extensions::crd_definition(desired.object.clone())?;
        if checked.name != desired.name
            || checked.group != desired.group
            || checked.kind != desired.kind
            || checked.plural != desired.plural
            || checked.served_versions != desired.served_versions
        {
            return Err("migration schema projection differs from its validated object".into());
        }
        let before = helm_crds::read(client, &desired.name).await?;
        let metadata = &before.live_metadata;
        let has_owner = metadata["labels"]
            .get("app.kubernetes.io/managed-by")
            .is_some()
            || metadata["annotations"]
                .get("meta.helm.sh/release-name")
                .is_some()
            || metadata["annotations"]
                .get("meta.helm.sh/release-namespace")
                .is_some();
        if has_owner
            && (metadata["labels"]["app.kubernetes.io/managed-by"] != "Helm"
                || metadata["annotations"]["meta.helm.sh/release-name"] != release
                || metadata["annotations"]["meta.helm.sh/release-namespace"] != namespace)
        {
            return Err(
                "migration refuses a schema explicitly owned by another release/manager".into(),
            );
        }
        let (source_version, target_version) =
            helm_extensions::migration_versions(&before.definition, desired)?;
        let names = before.definition.object["spec"]["versions"]
            .as_array()
            .ok_or("migration versions missing")?
            .iter()
            .filter_map(|version| version["name"].as_str())
            .collect::<BTreeSet<_>>();
        if before.stored_versions.is_empty()
            || before
                .stored_versions
                .iter()
                .any(|version| !names.contains(version.as_str()))
        {
            return Err(
                "migration needs complete valid storedVersions from the live schema".into(),
            );
        }
        let inventory = helm_crds::inventory(client, &before, &mut count, &mut bytes).await?;
        for instance in &inventory.instances {
            validate_instance(instance, namespace, release)?;
            let _ = instance_put(client, &inventory.api, instance, Some(&marker), true).await?;
        }
        // Check create/update admission and status UPDATE authorization without
        // prematurely removing a version before the target exists in spec.
        let _ = replace_schema(client, &before, desired, true).await?;
        let controller = controller_api.as_ref().map(|api| {
            controller_plan(
                api,
                &marker,
                entries.len(),
                &before,
                desired,
                &target_version,
                namespace,
                release,
            )
        });
        if let Some(controller) = &controller {
            controller_preflight(client, controller).await?;
            let _ = status_update(client, &before, &source_version, true).await?;
        }
        let stored_versions = before.stored_versions.clone();
        entries.push(Entry {
            before,
            desired: desired.clone(),
            inventory,
            source_version,
            target_version,
            stored_versions,
            controller,
        });
    }
    let canonical=serde_json::to_string(&json!({"marker":marker,"namespace":namespace,"release":release,"entries":entries.iter().map(|entry|json!({"uid":entry.before.uid,"rv":entry.before.resource_version,"before":entry.before.definition.object,"stored_versions":entry.stored_versions,"desired":entry.desired.object,"controller":entry.controller.as_ref().map(|controller|&controller.object),"instances":entry.inventory.instances.iter().map(|instance|&instance.object).collect::<Vec<_>>()})).collect::<Vec<_>>()})).map_err(|_|"cannot seal migration plan")?;
    let plan = Plan {
        entries,
        namespace: namespace.into(),
        release: release.into(),
        marker,
        canonical,
    };
    let _ = plan.review()?;
    Ok(plan)
}
pub async fn revalidate(client: &Client, plan: &Plan) -> Result<()> {
    let mut count = 0;
    let mut bytes = 0;
    for entry in &plan.entries {
        helm_crds::recheck(client, std::slice::from_ref(&entry.before)).await?;
        let current = helm_crds::read(client, &entry.before.definition.name).await?;
        if current.stored_versions != entry.stored_versions {
            return Err("recorded storage versions changed after migration review".into());
        }
        let inventory = helm_crds::inventory(client, &current, &mut count, &mut bytes).await?;
        if !entry.inventory.same_membership(&inventory) {
            return Err(
                "complete migration instance UID/RV membership changed; prepare again".into(),
            );
        }
        for instance in &inventory.instances {
            validate_instance(instance, &plan.namespace, &plan.release)?;
        }
        let _ = replace_schema(client, &current, &entry.desired, true).await?;
        if let Some(controller) = &entry.controller {
            controller_preflight(client, controller).await?;
            let _ = status_update(client, &current, &entry.source_version, true).await?;
        }
    }
    Ok(())
}

pub async fn execute(client: &Client, plan: &Plan) -> Result<Report> {
    revalidate(client, plan).await?;
    let mut report = Report::default();
    for entry in &plan.entries {
        let result = execute_entry(client, plan, entry, &mut report).await;
        if let Err(error) = result {
            return Err(format!(
                "Migration PARTIAL/UNKNOWN after {} schema switches, {} instance rewrites, {} status finalizations: {}. Old versions are never intentionally cleared before all rewrites; inspect exact schemas and prepare a fresh complete review to resume; no automatic retry.",
                report.switched, report.rewritten, report.finalized, error
            ));
        }
    }
    report.complete = report.finalized == plan.entries.len();
    Ok(report)
}
async fn current_target(client: &Client, entry: &Entry) -> Result<Binding> {
    let binding = helm_crds::read(client, &entry.desired.name).await?;
    if binding.uid != entry.before.uid
        || !entry.desired.matches_existing(&binding.definition.object)
    {
        return Err("schema UID or approved target storage/spec changed during migration; old storedVersions retained".into());
    }
    Ok(binding)
}
async fn execute_entry(
    client: &Client,
    plan: &Plan,
    entry: &Entry,
    report: &mut Report,
) -> Result<()> {
    let mut current = if entry.source_version != entry.target_version {
        let current = replace_schema(client, &entry.before, &entry.desired, false).await?;
        report.switched += 1;
        current
    } else {
        current_target(client, entry).await?
    };
    // Establishment and status recognition can lag the successful spec PUT.
    while !current.stored_versions.contains(&entry.target_version) {
        tokio::time::sleep(Duration::from_millis(200)).await;
        current = current_target(client, entry).await?;
    }
    let api = current
        .definition
        .resource_api(
            &format!("{}/{}", entry.desired.group, entry.target_version),
            &entry.desired.kind,
        )
        .ok_or("target migration API is not served")?;
    let mut rewritten = Vec::new();
    for instance in &entry.inventory.instances {
        let _ = current_target(client, entry).await?;
        let _ = instance_put(client, &api, instance, Some(&plan.marker), true).await?;
        let marked = instance_put(client, &api, instance, Some(&plan.marker), false).await?;
        let restored = instance_put(client, &api, &marked, None, false).await?;
        report.rewritten += 1;
        rewritten.push(restored);
    }
    let current = current_target(client, entry).await?;
    let mut count = 0;
    let mut bytes = 0;
    let actual = helm_crds::inventory(client, &current, &mut count, &mut bytes).await?;
    let expected = Inventory {
        schema: entry.inventory.schema.clone(),
        api: api.clone(),
        instances: rewritten,
    };
    if !expected.same_membership(&actual) {
        return Err(
            "instance membership/RV changed after rewrites; no storedVersions finalization".into(),
        );
    }
    let Some(controller) = &entry.controller else {
        report.pending += 1;
        return Ok(());
    };
    let result = controller_execute(client, controller, entry).await?;
    report.controller_migrations.push(result);
    let current = current_target(client, entry).await?;
    let mut count = 0;
    let mut bytes = 0;
    let final_inventory = helm_crds::inventory(client, &current, &mut count, &mut bytes).await?;
    if final_inventory.instances.len() != expected.instances.len()
        || !expected
            .instances
            .iter()
            .zip(&final_inventory.instances)
            .all(|(before, after)| {
                before.namespace == after.namespace
                    && before.name == after.name
                    && before.uid == after.uid
                    && same_content(&before.object, &after.object)
            })
    {
        return Err("controller completed but reviewed instance membership/content changed; historical versions retained".into());
    }
    let _ = status_update(client, &current, &entry.target_version, true).await?;
    let fresh = current_target(client, entry).await?;
    if fresh.resource_version != current.resource_version
        || fresh.stored_versions != current.stored_versions
    {
        return Err("schema/status changed before guarded migration finalization".into());
    }
    let mut count = 0;
    let mut bytes = 0;
    let last = helm_crds::inventory(client, &fresh, &mut count, &mut bytes).await?;
    if !final_inventory.same_membership(&last) {
        return Err("instance inventory changed immediately before migration finalization; historical versions retained".into());
    }
    let _ = status_update(client, &fresh, &entry.target_version, false).await?;
    report.finalized += 1;
    Ok(())
}

async fn discovery_document(client: &Client, path: &str) -> Result<Option<Value>> {
    tokio::time::timeout(BUDGET, async {
        let request = http::Request::get(path)
            .header(http::header::ACCEPT, "application/json")
            .body(kube::client::Body::empty())
            .map_err(|_| "invalid migration discovery request")?;
        let response = client
            .send(request)
            .await
            .map_err(|_| "CRD migration discovery transport failed; details suppressed. Next: check VPN/proxy and network access to the API server, then prepare the migration again")?;
        if response.status() == http::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if response.status() != http::StatusCode::OK {
            return Err(crate::errors::http_status(
                "CRD migration discovery (cannot safely choose controller mode)",
                response.status().as_u16(),
                &crate::errors::Scope::default(),
            ));
        }
        let bytes = http_body_util::Limited::new(response.into_body(), 1024 * 1024)
            .collect()
            .await
            .map_err(|_| "migration discovery exceeded1MiB or failed")?
            .to_bytes();
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| "invalid migration discovery JSON".into())
    })
    .await
    .map_err(|_| "migration discovery timed out".to_owned())?
}
async fn discover_controller(client: &Client) -> Result<Option<ApiResource>> {
    for version in ["v1", "v1beta1", "v1alpha1"] {
        let api = format!("storagemigration.k8s.io/{version}");
        let Some(catalog) = discovery_document(client, &format!("/apis/{api}")).await? else {
            continue;
        };
        if catalog["groupVersion"] != api || catalog["kind"] != "APIResourceList" {
            return Err("migration discovery returned another API/version".into());
        }
        let resources = catalog["resources"]
            .as_array()
            .filter(|resources| resources.len() <= 512)
            .ok_or("invalid migration API catalog")?;
        let matches = resources
            .iter()
            .filter(|resource| resource["name"] == "storageversionmigrations")
            .collect::<Vec<_>>();
        if matches.is_empty() {
            continue;
        }
        if matches.len() != 1
            || matches[0]["kind"] != "StorageVersionMigration"
            || matches[0]["namespaced"] != false
        {
            return Err("ambiguous/noncluster StorageVersionMigration API".into());
        }
        let verbs = matches[0]["verbs"]
            .as_array()
            .ok_or("migration API verbs unavailable")?;
        if !["get", "create"]
            .iter()
            .all(|verb| verbs.iter().any(|value| value == verb))
        {
            return Err("migration API lacks create/get support".into());
        }
        return Ok(Some(ApiResource {
            group: "storagemigration.k8s.io".into(),
            version: version.into(),
            api_version: api,
            kind: "StorageVersionMigration".into(),
            plural: "storageversionmigrations".into(),
        }));
    }
    Ok(None)
}
#[allow(clippy::too_many_arguments)]
fn controller_plan(
    api: &ApiResource,
    marker: &str,
    index: usize,
    before: &Binding,
    desired: &CrdDefinition,
    target: &str,
    namespace: &str,
    release: &str,
) -> Controller {
    let name = format!("kborg-migrate-{}-{index}", &marker[PREFIX.len()..]);
    let mut resource = json!({"group":desired.group,"resource":desired.plural});
    if api.version == "v1alpha1" {
        resource["version"] = json!(target);
    }
    let object = json!({"apiVersion":api.api_version,"kind":"StorageVersionMigration","metadata":{"name":name,"labels":{"app.kubernetes.io/managed-by":"kborg"},"annotations":{"kborg.dev/crd-uid":before.uid,"kborg.dev/confirmation-namespace":namespace,"kborg.dev/confirmation-release":release,"kborg.dev/target-storage-version":target}},"spec":{"resource":resource}});
    Controller {
        api: api.clone(),
        name,
        object,
    }
}
fn controller_matches(controller: &Controller, value: &Value) -> bool {
    value["apiVersion"] == controller.api.api_version
        && value["kind"] == "StorageVersionMigration"
        && value["metadata"]["name"] == controller.name
        && value["metadata"]["namespace"].is_null()
        && value["metadata"]["deletionTimestamp"].is_null()
        && value["spec"] == controller.object["spec"]
        && value["metadata"]["labels"] == controller.object["metadata"]["labels"]
        && value["metadata"]["annotations"] == controller.object["metadata"]["annotations"]
}
async fn controller_preflight(client: &Client, controller: &Controller) -> Result<()> {
    let api: Api<DynamicObject> = Api::all_with(client.clone(), &controller.api);
    if tokio::time::timeout(BUDGET, api.get_opt(&controller.name))
        .await
        .map_err(|_| "migration identity check timed out".to_owned())?
        .map_err(|error| failure(error, "controller migration identity check"))?
        .is_some()
    {
        return Err(
            "reviewed controller migration name already exists; fresh review required".into(),
        );
    }
    let actual = tokio::time::timeout(
        BUDGET,
        api.create(
            &PostParams {
                dry_run: true,
                ..Default::default()
            },
            &dynamic(controller.object.clone())?,
        ),
    )
    .await
    .map_err(|_| "controller migration dry-run timed out".to_owned())?
    .map_err(|error| failure(error, "controller migration create dry-run"))?;
    if !controller_matches(
        controller,
        &serde_json::to_value(actual).map_err(|_| "invalid migration admission response")?,
    ) {
        return Err("controller migration admission changed exact reviewed target/metadata".into());
    }
    Ok(())
}
fn controller_state(value: &Value) -> Result<Option<bool>> {
    let Some(conditions) = value["status"]["conditions"].as_array() else {
        return if value["status"]["conditions"].is_null() {
            Ok(None)
        } else {
            Err("invalid migration controller conditions".into())
        };
    };
    if conditions.len() > 16 {
        return Err("migration conditions exceeded bound".into());
    }
    let mut succeeded = None;
    let mut failed = None;
    for condition in conditions {
        match condition["type"].as_str() {
            Some("Succeeded") => {
                if succeeded.is_some() {
                    return Err("duplicate migration success condition".into());
                }
                succeeded = Some(condition["status"] == "True");
            }
            Some("Failed") => {
                if failed.is_some() {
                    return Err("duplicate migration failure condition".into());
                }
                failed = Some(condition["status"] == "True");
            }
            _ => {}
        }
    }
    if failed == Some(true) {
        return Ok(Some(false));
    }
    if succeeded == Some(true) {
        if value["status"]["resourceVersion"]
            .as_str()
            .is_none_or(|version| version.is_empty() || version.len() > 253)
        {
            return Err("controller success omitted its migration resource checkpoint".into());
        }
        return Ok(Some(true));
    }
    Ok(None)
}
async fn controller_execute(
    client: &Client,
    controller: &Controller,
    entry: &Entry,
) -> Result<ControllerResult> {
    let api: Api<DynamicObject> = Api::all_with(client.clone(), &controller.api);
    let created = tokio::time::timeout(
        BUDGET,
        api.create(&PostParams::default(), &dynamic(controller.object.clone())?),
    )
    .await
    .map_err(|_| "controller migration creation timed out; reconcile named migration".to_owned())?
    .map_err(|error| failure(error, "controller migration create"))?;
    let created = serde_json::to_value(created).map_err(|_| "invalid created migration")?;
    let uid = created["metadata"]["uid"]
        .as_str()
        .filter(|uid| !uid.is_empty() && uid.len() <= 253)
        .ok_or("created migration omitted bounded UID")?
        .to_owned();
    if !controller_matches(controller, &created) {
        return Err(format!(
            "created controller migration {} UID{} changed reviewed target; inspect independently",
            controller.name, uid
        ));
    }
    loop {
        let _ = current_target(client, entry).await?;
        let value = tokio::time::timeout(BUDGET, api.get(&controller.name))
            .await
            .map_err(|_| {
                format!(
                    "controller migration {} wait timed out; object may continue",
                    controller.name
                )
            })?
            .map_err(|error| failure(error, "controller migration status read"))?;
        let value =
            serde_json::to_value(value).map_err(|_| "invalid migration controller response")?;
        if value["metadata"]["uid"] != uid || !controller_matches(controller, &value) {
            return Err(format!(
                "controller migration {} UID/target changed; historical versions retained",
                controller.name
            ));
        }
        match controller_state(&value)? {
            Some(true) => {
                return Ok(ControllerResult {
                    api_version: controller.api.api_version.clone(),
                    name: controller.name.clone(),
                    uid,
                });
            }
            Some(false) => {
                return Err(format!(
                    "official controller migration {} UID{} FAILED; historical versions retained; remote diagnostics suppressed",
                    controller.name, uid
                ));
            }
            None => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::{BodyExt, Full};
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
    };
    #[derive(Clone)]
    struct State {
        schema: Value,
        objects: BTreeMap<String, Value>,
        backing: BTreeMap<String, String>,
        controller: Option<Value>,
        mode: u8,
        writes: usize,
        status_writes: usize,
        controller_succeeded: bool,
    }
    fn schema() -> Value {
        json!({"apiVersion":"apiextensions.k8s.io/v1","kind":"CustomResourceDefinition","metadata":{"name":"widgets.migrate.example.test","uid":"schema-uid","resourceVersion":"10","labels":{"kborg.dev/run-id":"fixture"},"finalizers":["customresourcecleanup.apiextensions.k8s.io"]},"spec":{"group":"migrate.example.test","scope":"Namespaced","names":{"kind":"Widget","plural":"widgets"},"versions":[{"name":"v1","served":true,"storage":true,"schema":{"openAPIV3Schema":{"type":"object","properties":{"spec":{"type":"object","properties":{"value":{"type":"string"}}},"status":{"type":"object","properties":{"ready":{"type":"boolean"}}}}}}}]},"status":{"storedVersions":["v1"],"conditions":[{"type":"Established","status":"True"}]}})
    }
    fn desired() -> Result<CrdPlan> {
        let mut object = schema();
        object.as_object_mut().ok_or("fixture")?.remove("status");
        object["metadata"] = json!({"name":"widgets.migrate.example.test"});
        let mut version = object["spec"]["versions"][0].clone();
        version["name"] = json!("v2");
        object["spec"]["versions"][0]["storage"] = json!(false);
        object["spec"]["versions"]
            .as_array_mut()
            .ok_or("versions")?
            .push(version);
        helm_extensions::crds(&object.to_string(), true)
    }
    fn instance(name: &str) -> Value {
        json!({"apiVersion":"migrate.example.test/v1","kind":"Widget","metadata":{"namespace":"fixture","name":name,"uid":format!("uid-{name}"),"resourceVersion":"20","labels":{"app.kubernetes.io/managed-by":"Helm"},"annotations":{"meta.helm.sh/release-name":"migration","meta.helm.sh/release-namespace":"fixture"}},"spec":{"value":"preserve"},"status":{"ready":true}})
    }
    fn mock(controller: bool, mode: u8) -> (Client, Arc<Mutex<State>>) {
        let state = Arc::new(Mutex::new(State {
            schema: schema(),
            objects: BTreeMap::from([("a".into(), instance("a"))]),
            backing: BTreeMap::from([("a".into(), "v1".into())]),
            controller: None,
            mode,
            writes: 0,
            status_writes: 0,
            controller_succeeded: false,
        }));
        let shared = state.clone();
        let client = Client::new(
            tower::service_fn(move |request: http::Request<kube::client::Body>| {
                let state = shared.clone();
                async move {
                    let method = request.method().clone();
                    let uri = request.uri().to_string();
                    let body = request
                        .into_body()
                        .collect()
                        .await
                        .map_err(std::io::Error::other)?
                        .to_bytes();
                    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                    let mut state = state
                        .lock()
                        .map_err(|_| std::io::Error::other("fixture lock"))?;
                    let dry = uri.contains("dryRun=All");
                    let missing = || json!({"apiVersion":"v1","kind":"Status","code":404,"reason":"NotFound"});
                    let mut code = 200;
                    let mut value;
                    if uri.starts_with("/apis/storagemigration.k8s.io/")
                        && !uri.contains("storageversionmigrations")
                    {
                        if controller && uri.ends_with("v1beta1") {
                            value = json!({"apiVersion":"v1","kind":"APIResourceList","groupVersion":"storagemigration.k8s.io/v1beta1","resources":[{"name":"storageversionmigrations","kind":"StorageVersionMigration","namespaced":false,"verbs":["get","create"]}]});
                        } else {
                            code = 404;
                            value = missing();
                        }
                    } else if uri.contains("storageversionmigrations") {
                        if method == http::Method::POST {
                            value = body;
                            if !dry {
                                value["metadata"]["uid"] = json!("migration-uid");
                                value["metadata"]["resourceVersion"] = json!("30");
                                state.controller = Some(value.clone());
                            }
                        } else if let Some(mut current) = state.controller.clone() {
                            match state.mode {
                                2 => {
                                    current["status"] =
                                        json!({"conditions":[{"type":"Failed","status":"True"}]})
                                }
                                3 => current["metadata"]["uid"] = json!("replacement"),
                                _ => {
                                    current["status"] = json!({"resourceVersion":"30","conditions":[{"type":"Succeeded","status":"True"}]});
                                    state.controller_succeeded = true;
                                    if state.mode == 4 {
                                        let added = instance("new");
                                        state.objects.insert("new".into(), added);
                                        state.backing.insert("new".into(), "v2".into());
                                    }
                                }
                            }
                            value = current;
                        } else {
                            code = 404;
                            value = missing();
                        }
                    } else if uri.contains("/customresourcedefinitions/") {
                        if method == http::Method::GET {
                            value = state.schema.clone();
                        } else if uri.contains("/status") {
                            value = state.schema.clone();
                            value["status"] = body["status"].clone();
                            if !dry {
                                assert!(
                                    state.controller_succeeded,
                                    "native PUTs/GET apiVersion cannot authorize status cleanup"
                                );
                                assert!(state.backing.values().all(|version| version == "v2"));
                                assert_eq!(body["metadata"]["uid"], "schema-uid");
                                assert_eq!(
                                    body["metadata"]["resourceVersion"],
                                    state.schema["metadata"]["resourceVersion"]
                                );
                                state.status_writes += 1;
                                value["metadata"]["resourceVersion"] = json!("99");
                                state.schema = value.clone();
                            }
                        } else {
                            value = state.schema.clone();
                            value["spec"] = body["spec"].clone();
                            value["status"]["storedVersions"] = json!(["v1", "v2"]);
                            if !dry {
                                state.writes += 1;
                                value["metadata"]["resourceVersion"] = json!("11");
                                state.schema = value.clone();
                            }
                        }
                    } else if method == http::Method::GET && uri.contains("?limit=") {
                        let version = if uri.contains("/v2/") { "v2" } else { "v1" };
                        let items = state
                            .objects
                            .values()
                            .cloned()
                            .map(|mut object| {
                                object["apiVersion"] =
                                    json!(format!("migrate.example.test/{version}"));
                                object
                            })
                            .collect::<Vec<_>>();
                        value = json!({"apiVersion":format!("migrate.example.test/{version}"),"kind":"WidgetList","metadata":{},"items":items});
                    } else if method == http::Method::PUT {
                        let name = body["metadata"]["name"].as_str().unwrap_or("").to_owned();
                        let old = state
                            .objects
                            .get(&name)
                            .cloned()
                            .ok_or_else(|| std::io::Error::other("missing fixture instance"))?;
                        if !dry
                            && state.mode == 1
                            && body["metadata"]["annotations"].get(MARKER).is_none()
                        {
                            code = 409;
                            value = json!({"apiVersion":"v1","kind":"Status","code":409,"reason":"Conflict","message":"PRIVATE_CANARY"});
                        } else {
                            assert_eq!(body["metadata"]["uid"], old["metadata"]["uid"]);
                            assert_eq!(
                                body["metadata"]["resourceVersion"],
                                old["metadata"]["resourceVersion"]
                            );
                            value = body;
                            if !dry {
                                state.writes += 1;
                                value["metadata"]["resourceVersion"] =
                                    json!((20 + state.writes).to_string());
                                state.objects.insert(name.clone(), value.clone());
                                let version = state.schema["spec"]["versions"]
                                    .as_array()
                                    .and_then(|versions| {
                                        versions.iter().find(|version| version["storage"] == true)
                                    })
                                    .and_then(|version| version["name"].as_str())
                                    .unwrap_or("v1")
                                    .to_owned();
                                state.backing.insert(name, version);
                            }
                        }
                    } else {
                        panic!("unexpected migration request {method} {uri}");
                    }
                    let response = http::Response::builder()
                        .status(code)
                        .header(http::header::CONTENT_TYPE, "application/json")
                        .body(Full::new(bytes::Bytes::from(value.to_string())))
                        .map_err(std::io::Error::other)?;
                    Ok::<_, std::io::Error>(response)
                }
            }),
            "fixture",
        );
        (client, state)
    }
    #[test]
    fn native_fallback_stages_but_never_clears_historical_versions() -> Result<()> {
        crate::cli::runtime()?.block_on(async {
            let (client, state) = mock(false, 0);
            let plan = prepare(&client, &desired()?, "fixture", "migration").await?;
            assert!(plan.review()?.contains("STAGED/PENDING"));
            let report = execute(&client, &plan).await?;
            assert!(!report.complete);
            assert_eq!(report.pending, 1);
            assert_eq!(report.rewritten, 1);
            assert_eq!(report.finalized, 0);
            let state = state.lock().map_err(|_| "lock")?;
            assert_eq!(
                state.schema["status"]["storedVersions"],
                json!(["v1", "v2"])
            );
            assert_eq!(state.status_writes, 0);
            assert_eq!(state.backing["a"], "v2");
            assert!(
                state.objects["a"]["metadata"]["annotations"]
                    .get(MARKER)
                    .is_none()
            );
            Ok(())
        })
    }
    #[test]
    fn authoritative_controller_success_is_required_before_guarded_status_cleanup() -> Result<()> {
        crate::cli::runtime()?.block_on(async {
            for mode in [0, 2, 3, 4] {
                let (client, state) = mock(true, mode);
                let plan = prepare(&client, &desired()?, "fixture", "migration").await?;
                assert_eq!(
                    plan.entries()[0]
                        .controller
                        .as_ref()
                        .map(|controller| controller.api.version.as_str()),
                    Some("v1beta1")
                );
                let report = execute(&client, &plan).await;
                let state = state.lock().map_err(|_| "lock")?;
                if mode == 0 {
                    let report = report?;
                    assert!(report.complete);
                    assert_eq!(report.finalized, 1);
                    assert_eq!(report.controller_migrations[0].uid, "migration-uid");
                    assert_eq!(state.schema["status"]["storedVersions"], json!(["v2"]));
                    assert_eq!(state.status_writes, 1);
                } else {
                    assert!(report.is_err());
                    assert_eq!(state.status_writes, 0);
                    assert_eq!(
                        state.schema["status"]["storedVersions"],
                        json!(["v1", "v2"])
                    );
                }
            }
            Ok(())
        })
    }
    #[test]
    fn partial_rewrite_can_be_freshly_reviewed_and_stale_inventory_stops_before_switch()
    -> Result<()> {
        crate::cli::runtime()?.block_on(async {
            let (client, state) = mock(false, 1);
            let plan = prepare(&client, &desired()?, "fixture", "migration").await?;
            assert!(
                execute(&client, &plan).await.is_err_and(
                    |error| error.contains("PARTIAL/UNKNOWN") && !error.contains("CANARY")
                )
            );
            {
                let mut state = state.lock().map_err(|_| "lock")?;
                assert_eq!(state.status_writes, 0);
                assert!(
                    state.objects["a"]["metadata"]["annotations"]
                        .get(MARKER)
                        .is_some()
                );
                state.mode = 0;
            }
            let resumed = prepare(&client, &desired()?, "fixture", "migration").await?;
            let report = execute(&client, &resumed).await?;
            assert_eq!(report.switched, 0);
            assert_eq!(report.pending, 1);
            assert!(
                state.lock().map_err(|_| "lock")?.objects["a"]["metadata"]["annotations"]
                    .get(MARKER)
                    .is_none()
            );
            let (client, state) = mock(false, 0);
            let plan = prepare(&client, &desired()?, "fixture", "migration").await?;
            state
                .lock()
                .map_err(|_| "lock")?
                .objects
                .get_mut("a")
                .ok_or("instance")?["metadata"]["resourceVersion"] = json!("999");
            assert!(execute(&client, &plan).await.is_err());
            assert_eq!(state.lock().map_err(|_| "lock")?.writes, 0);
            Ok(())
        })
    }
    #[test]
    fn controller_condition_requires_checkpoint_and_rejects_replacement_or_failure() {
        assert!(
            controller_state(
                &json!({"status":{"conditions":[{"type":"Succeeded","status":"True"}]}})
            )
            .is_err()
        );
        assert_eq!(
            controller_state(&json!({"status":{"conditions":[{"type":"Failed","status":"True"}]}})),
            Ok(Some(false))
        );
        assert!(controller_state(&json!({"status":{"resourceVersion":"5","conditions":[{"type":"Succeeded","status":"True"},{"type":"Succeeded","status":"True"}]}})).is_err());
    }
}
