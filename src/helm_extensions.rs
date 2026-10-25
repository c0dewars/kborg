//! Bounded planning for explicitly reviewed Helm hooks and create-only CRDs.
//! Execution remains in helm_lifecycle; this module grants no capabilities.
//!
//! Semantics are pinned to Helm v4.3.1 pkg/action/hooks.go and install.go.
//! CRDs must be bootstrapped and Established before a separate CR chart review.
use crate::{Result, safety};
use kube::api::ApiResource;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const MAX_HOOKS: usize = 32;
pub const MAX_CRDS: usize = 16;
pub const MAX_EXTENSION_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Install,
    Upgrade,
    Rollback,
    Uninstall,
    Test,
}
impl Operation {
    fn events(self) -> &'static [&'static str] {
        match self {
            Self::Install => &["pre-install", "post-install"],
            Self::Upgrade => &["pre-upgrade", "post-upgrade"],
            Self::Rollback => &["pre-rollback", "post-rollback"],
            Self::Uninstall => &["pre-delete", "post-delete"],
            Self::Test => &["test"],
        }
    }
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ObjectIdentity {
    pub api_version: String,
    pub kind: String,
    pub namespace: String,
    pub name: String,
}
#[derive(Clone, Serialize)]
pub struct Hook {
    pub identity: ObjectIdentity,
    pub object: Value,
    pub events: Vec<String>,
    pub weight: i32,
    pub delete_policies: Vec<String>,
    pub keep_annotation: bool,
}
impl Hook {
    pub fn deletes_before_creation(&self) -> bool {
        self.delete_policies
            .iter()
            .any(|p| p == "before-hook-creation")
    }
    pub fn deletes_on_success(&self) -> bool {
        self.delete_policies.iter().any(|p| p == "hook-succeeded")
    }
    pub fn deletes_on_failure(&self) -> bool {
        self.delete_policies.iter().any(|p| p == "hook-failed")
    }
    pub fn readiness(&self) -> &'static str {
        if matches!(self.identity.kind.as_str(), "Job" | "Pod") {
            "successful completion (failure stops the release)"
        } else {
            "API resource loaded; this is not workload readiness"
        }
    }
    /// Extra server defaults are accepted only for absent map keys. Explicit
    /// desired fields and array membership must match, including ownership.
    /// The caller additionally requires the stored authorized hook template,
    /// and must seal/recheck live UID+resourceVersion before any deletion.
    pub fn matches_existing(&self, current: &Value) -> bool {
        current["apiVersion"] == self.identity.api_version
            && current["kind"] == self.identity.kind
            && current["metadata"]["namespace"] == self.identity.namespace
            && current["metadata"]["name"] == self.identity.name
            && desired_subset(&self.object, current)
    }
}
#[derive(Clone, Default)]
pub struct HookPlan {
    /// All hooks, including events not run by this operation, form the seal.
    pub all: Vec<Hook>,
    pub canonical: String,
    selected_events: Vec<String>,
}
impl HookPlan {
    /// Helm 4.3.1 execHook stably sorts weight then NAME. Equal pairs retain
    /// their stored array order; rendered hooks normally arrive kind-sorted.
    pub fn for_event(&self, event: &str) -> Vec<&Hook> {
        if !self.selected_events.iter().any(|value| value == event) {
            return vec![];
        }
        let mut hooks = self
            .all
            .iter()
            .filter(|hook| hook.events.iter().any(|value| value == event))
            .collect::<Vec<_>>();
        hooks.sort_by(|a, b| {
            a.weight
                .cmp(&b.weight)
                .then(a.identity.name.cmp(&b.identity.name))
        });
        hooks
    }
    pub fn active(&self) -> Vec<&Hook> {
        self.selected_events
            .iter()
            .flat_map(|event| self.for_event(event))
            .collect()
    }
    pub fn events(&self) -> &[String] {
        &self.selected_events
    }
    pub fn review(&self) -> String {
        let mut out = String::from(
            "HOOK PLAN: serial event/weight/name order; hook resources are not normal release resources.\nHook workloads can change external systems; cancellation cannot undo their effects.\nHook keep annotations do not override Helm 4.3.1 hook deletion policies. Retained hooks are not automatically removed by uninstall.\nDelete authorization is checked for absent hooks; future completion/delete admission cannot be fully simulated before creation.\n",
        );
        for event in &self.selected_events {
            for hook in self.for_event(event) {
                out.push_str(&format!("{event} weight {}: {} {}/{}; wait {}; delete-before {}; delete-success {}; delete-failure {}; keep annotation {}\n",hook.weight,hook.identity.kind,hook.identity.namespace,hook.identity.name,hook.readiness(),hook.deletes_before_creation(),hook.deletes_on_success(),hook.deletes_on_failure(),hook.keep_annotation));
            }
        }
        out
    }
}

pub fn hooks(
    raw: &Value,
    namespace: &str,
    release: &str,
    operation: Operation,
    allow: bool,
) -> Result<HookPlan> {
    if raw.is_null() {
        return hooks_from_manifests("", namespace, release, operation, allow);
    }
    let entries = raw
        .as_array()
        .filter(|entries| entries.len() <= MAX_HOOKS)
        .ok_or("Helm hooks must be an array of at most32 entries")?;
    let mut objects = Vec::new();
    let mut bytes = 0usize;
    for entry in entries {
        let manifest = entry["manifest"]
            .as_str()
            .ok_or("Helm hook has no manifest")?;
        bytes = bytes
            .checked_add(manifest.len())
            .filter(|n| *n <= MAX_EXTENSION_BYTES)
            .ok_or("Helm hooks exceed4 MiB")?;
        let mut docs = documents(manifest, 1)?;
        if docs.len() != 1 {
            return Err("each Helm hook must contain exactly one resource".into());
        }
        let object = docs.remove(0);
        let hook = hook_object(object, namespace, release)?;
        if entry["name"] != hook.identity.name || entry["kind"] != hook.identity.kind {
            return Err("Helm hook record disagrees with its manifest identity".into());
        }
        // Helm stores the effective fields separately from the manifest and
        // executes those fields. Omitted weight/policies have real defaults;
        // absence must never waive the annotation-versus-storage comparison.
        let stored_weight = match entry.get("weight") {
            None => 0,
            Some(value) => value
                .as_i64()
                .ok_or("Helm hook weight must be an integer")?,
        };
        if stored_weight != i64::from(hook.weight) {
            return Err("Helm hook record disagrees with its manifest weight".into());
        }
        if let Some(events) = entry.get("events") {
            let expected = string_array(events)?;
            if expected != hook.events {
                return Err("Helm hook events disagree with manifest annotations".into());
            }
        } else {
            return Err("Helm hook omitted its events".into());
        }
        let mut stored_policies = entry
            .get("delete_policies")
            .map(string_array)
            .transpose()?
            .unwrap_or_default();
        if stored_policies.is_empty() {
            stored_policies.push("before-hook-creation".into());
        }
        if stored_policies != hook.delete_policies {
            return Err("Helm hook deletion policies disagree with its manifest".into());
        }
        if entry
            .get("output_log_policies")
            .is_some_and(|value| value.as_array().is_none_or(|values| !values.is_empty()))
        {
            return Err("automatic hook log output is unsupported; review logs separately".into());
        }
        objects.push(hook);
    }
    hook_plan(objects, operation, allow)
}
pub fn hooks_from_manifests(
    manifests: &str,
    namespace: &str,
    release: &str,
    operation: Operation,
    allow: bool,
) -> Result<HookPlan> {
    let objects = documents(manifests, MAX_HOOKS)?
        .into_iter()
        .map(|object| hook_object(object, namespace, release))
        .collect::<Result<Vec<_>>>()?;
    hook_plan(objects, operation, allow)
}
fn hook_plan(all: Vec<Hook>, operation: Operation, allow: bool) -> Result<HookPlan> {
    if !all.is_empty() && !allow {
        return Err(
            "chart contains hooks; explicitly enable --allow-helm-hooks and review their effects"
                .into(),
        );
    }
    let mut identities = BTreeSet::new();
    if all.iter().any(|hook| !identities.insert(&hook.identity)) {
        return Err("duplicate Helm hook identity".into());
    }
    let canonical = serde_json::to_string(&all).map_err(|_| "cannot seal hook plan")?;
    Ok(HookPlan {
        all,
        canonical,
        selected_events: operation
            .events()
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
    })
}
fn hook_object(mut object: Value, namespace: &str, release: &str) -> Result<Hook> {
    validate_namespace(namespace)?;
    if !crate::helm::valid_release_name(release) {
        return Err("invalid hook release name".into());
    }
    let identity = namespaced_identity(&mut object, namespace)?;
    native_api(&identity.api_version, &identity.kind)?;
    check_gitops(&object)?;
    // Helm does not automatically attach these markers to hooks. Require the
    // chart author to declare them; existing-object deletion never adopts.
    if object["metadata"]["labels"]["app.kubernetes.io/managed-by"] != "Helm"
        || object["metadata"]["annotations"]["meta.helm.sh/release-name"] != release
        || object["metadata"]["annotations"]["meta.helm.sh/release-namespace"] != namespace
    {
        return Err("reviewed hooks must explicitly declare managed-by Helm and exact meta.helm.sh release-name/release-namespace ownership".into());
    }
    let annotations = &object["metadata"]["annotations"];
    let mut events = annotation_values(annotations, "helm.sh/hook")?
        .ok_or("hook manifest omitted helm.sh/hook")?;
    for event in &mut events {
        if event == "test-success" {
            *event = "test".into();
        }
    }
    if events.iter().any(|event| {
        ![
            "pre-install",
            "post-install",
            "pre-upgrade",
            "post-upgrade",
            "pre-rollback",
            "post-rollback",
            "pre-delete",
            "post-delete",
            "test",
        ]
        .contains(&event.as_str())
    }) {
        return Err("unsupported Helm hook event".into());
    }
    if events.iter().collect::<BTreeSet<_>>().len() != events.len() {
        return Err("duplicate Helm hook event".into());
    }
    let weight = match annotations.get("helm.sh/hook-weight") {
        Some(value) => value
            .as_str()
            .ok_or("hook weight must be a string")?
            .parse::<i32>()
            .map_err(|_| "hook weight must be a bounded signed32-bit integer")?,
        None => 0,
    };
    let delete_policies = annotation_values(annotations, "helm.sh/hook-delete-policy")?
        .unwrap_or_else(|| vec!["before-hook-creation".into()]);
    if delete_policies.iter().any(|value| {
        !["before-hook-creation", "hook-succeeded", "hook-failed"].contains(&value.as_str())
    }) || delete_policies.iter().collect::<BTreeSet<_>>().len() != delete_policies.len()
    {
        return Err("invalid or duplicate hook deletion policy".into());
    }
    if annotations.get("helm.sh/hook-output-log-policy").is_some() {
        return Err("automatic hook log output is unsupported; review logs separately".into());
    }
    let keep_annotation = match annotations.get("helm.sh/resource-policy") {
        None => false,
        Some(value) if value == "keep" => true,
        _ => return Err("unknown hook resource policy".into()),
    };
    Ok(Hook {
        identity,
        object,
        events,
        weight,
        delete_policies,
        keep_annotation,
    })
}
fn annotation_values(annotations: &Value, key: &str) -> Result<Option<Vec<String>>> {
    annotations
        .get(key)
        .map(|value| {
            let text = value
                .as_str()
                .filter(|text| text.len() <= 1024)
                .ok_or("hook annotation must be bounded string data")?;
            let values = text
                .split(',')
                .map(|value| value.trim().to_ascii_lowercase())
                .collect::<Vec<_>>();
        if text != values.join(",") {
            return Err("hook events and policies require canonical lowercase comma-separated values without whitespace".into());
        }
            if values.is_empty() || values.iter().any(String::is_empty) {
                return Err("empty hook annotation entry".into());
            }
            Ok(values)
        })
        .transpose()
}
fn string_array(value: &Value) -> Result<Vec<String>> {
    value
        .as_array()
        .filter(|values| values.len() <= 16)
        .ok_or("invalid hook record array")?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "invalid hook record entry".into())
        })
        .collect()
}
fn desired_subset(desired: &Value, actual: &Value) -> bool {
    match desired {
        Value::Object(desired) => actual.as_object().is_some_and(|actual| {
            desired.iter().all(|(key, value)| {
                actual
                    .get(key)
                    .is_some_and(|other| desired_subset(value, other))
            })
        }),
        Value::Array(desired) => actual.as_array().is_some_and(|actual| {
            actual.len() == desired.len()
                && desired
                    .iter()
                    .zip(actual)
                    .all(|(a, b)| desired_subset(a, b))
        }),
        _ => desired == actual,
    }
}

#[derive(Clone, Serialize)]
pub struct CrdDefinition {
    pub name: String,
    pub group: String,
    pub plural: String,
    pub kind: String,
    pub served_versions: Vec<String>,
    pub object: Value,
}
/// Compare the complete spec, including nested schemas/rules. Only documented
/// CRD defaulting and typed API omission of known empty fields are normalized.
/// No schema properties, validation rules or arbitrary map keys are ignored.
pub(crate) fn normalized_crd_spec(spec: &Value) -> Option<Value> {
    let mut spec = spec.clone();
    let map = spec.as_object_mut()?;
    let names = map.get_mut("names")?.as_object_mut()?;
    let kind = names.get("kind")?.as_str()?.to_owned();
    for (key, default) in [
        ("singular", kind.to_ascii_lowercase()),
        ("listKind", format!("{kind}List")),
    ] {
        if names
            .get(key)
            .is_none_or(|value| value.as_str() == Some(""))
        {
            names.insert(key.into(), json!(default));
        }
    }
    for key in ["shortNames", "categories"] {
        if names
            .get(key)
            .is_some_and(|value| value.as_array().is_some_and(Vec::is_empty))
        {
            names.remove(key);
        }
    }
    map.entry("conversion")
        .or_insert_with(|| json!({"strategy":"None"}));
    map.entry("preserveUnknownFields").or_insert(json!(false));
    for version in map.get_mut("versions")?.as_array_mut()? {
        let version = version.as_object_mut()?;
        version.entry("deprecated").or_insert(json!(false));
        for key in ["additionalPrinterColumns", "selectableFields"] {
            if version
                .get(key)
                .is_some_and(|value| value.as_array().is_some_and(Vec::is_empty))
            {
                version.remove(key);
            }
        }
    }
    Some(spec)
}
impl CrdDefinition {
    pub fn resource_api(&self, api: &str, kind: &str) -> Option<ApiResource> {
        let (group, version) = api.split_once('/')?;
        if group != self.group
            || kind != self.kind
            || !self.served_versions.iter().any(|value| value == version)
        {
            return None;
        }
        Some(ApiResource {
            group: group.into(),
            version: version.into(),
            api_version: api.into(),
            kind: kind.into(),
            plural: self.plural.clone(),
        })
    }
    pub fn matches_existing(&self, object: &Value) -> bool {
        object["apiVersion"] == "apiextensions.k8s.io/v1"
            && object["kind"] == "CustomResourceDefinition"
            && object["metadata"]["name"] == self.name
            && normalized_crd_spec(&self.object["spec"])
                .zip(normalized_crd_spec(&object["spec"]))
                .is_some_and(|(desired, actual)| desired == actual)
    }
    pub fn established(object: &Value) -> bool {
        let Some(conditions) = object["status"]["conditions"].as_array() else {
            return false;
        };
        conditions
            .iter()
            .any(|condition| condition["type"] == "Established" && condition["status"] == "True")
            && !conditions.iter().any(|condition| {
                condition["type"] == "NamesAccepted" && condition["status"] == "False"
            })
            && object["metadata"]["deletionTimestamp"].is_null()
    }
}
#[derive(Clone, Default)]
pub struct CrdPlan {
    pub definitions: Vec<CrdDefinition>,
    pub canonical: String,
}
pub fn crds(manifests: &str, allow: bool) -> Result<CrdPlan> {
    let mut definitions = documents(manifests, MAX_CRDS)?
        .into_iter()
        .map(crd_definition)
        .collect::<Result<Vec<_>>>()?;
    if !definitions.is_empty() && !allow {
        return Err(
            "CRDs require explicit --allow-helm-crds and a separate create-only review".into(),
        );
    }
    definitions.sort_by(|a, b| a.name.cmp(&b.name));
    if definitions
        .windows(2)
        .any(|pair| pair[0].name == pair[1].name)
    {
        return Err("duplicate CRD definition".into());
    }
    let canonical = serde_json::to_string(&definitions).map_err(|_| "cannot seal CRD plan")?;
    Ok(CrdPlan {
        definitions,
        canonical,
    })
}
pub fn crd_definition(object: Value) -> Result<CrdDefinition> {
    if serde_json::to_vec(&object)
        .map_err(|_| "invalid CRD")?
        .len()
        > 1024 * 1024
    {
        return Err("one CRD exceeds1 MiB".into());
    }
    if object["apiVersion"] != "apiextensions.k8s.io/v1"
        || object["kind"] != "CustomResourceDefinition"
        || !object["metadata"].is_object()
        || !object["spec"].is_object()
    {
        return Err(
            "CRD bootstrap accepts only apiextensions.k8s.io/v1 CustomResourceDefinition objects"
                .into(),
        );
    }
    let group = object["spec"]["group"]
        .as_str()
        .filter(|value| {
            dns_name(value)
                && value.contains('.')
                && !["k8s.io", "kubernetes.io"]
                    .iter()
                    .any(|reserved| *value == *reserved || value.ends_with(&format!(".{reserved}")))
        })
        .ok_or("CRD needs a non-system DNS group")?
        .to_owned();
    let plural = object["spec"]["names"]["plural"]
        .as_str()
        .filter(|value| safety::valid_id(value) && value.len() <= 63)
        .ok_or("invalid CRD plural")?
        .to_owned();
    let kind = object["spec"]["names"]["kind"]
        .as_str()
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 63
                && value.as_bytes()[0].is_ascii_uppercase()
                && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
        .ok_or("invalid CRD kind")?
        .to_owned();
    let name = format!("{plural}.{group}");
    if name.len() > 253
        || object["metadata"]["name"] != name
        || !object["metadata"]["namespace"].is_null()
        || object["spec"]["scope"] != "Namespaced"
    {
        return Err(
            "CRD identity must match plural.group with Namespaced scope and no metadata.namespace"
                .into(),
        );
    }
    if object["metadata"].get("generateName").is_some()
        || object["metadata"]
            .get("ownerReferences")
            .is_some_and(|value| value.as_array().is_none_or(|values| !values.is_empty()))
        || object["metadata"]
            .get("finalizers")
            .is_some_and(|value| value.as_array().is_none_or(|values| !values.is_empty()))
    {
        return Err(
            "CRD bootstrap forbids generated identities, ownerReferences and finalizers".into(),
        );
    }
    if object["spec"]["preserveUnknownFields"] == true {
        return Err("CRDs need structural schemas, not legacy preserveUnknownFields".into());
    }
    if let Some(conversion) = object["spec"].get("conversion")
        && (conversion["strategy"] != "None" || conversion.get("webhook").is_some())
    {
        return Err("CRD conversion webhooks are unsupported".into());
    }
    check_gitops(&object)?;
    let versions = object["spec"]["versions"]
        .as_array()
        .filter(|versions| !versions.is_empty() && versions.len() <= 8)
        .ok_or("CRD needs1..8 explicit versions")?;
    let mut names = BTreeSet::new();
    let mut served_versions = Vec::new();
    let mut storage = 0;
    for version in versions {
        let name = version["name"]
            .as_str()
            .filter(|value| safety::valid_id(value) && value.len() <= 63)
            .ok_or("invalid CRD version")?;
        if !names.insert(name)
            || !version["served"].is_boolean()
            || !version["storage"].is_boolean()
        {
            return Err("CRD versions must be unique with explicit served/storage booleans".into());
        }
        if version["schema"]["openAPIV3Schema"]["type"] != "object" {
            return Err("every CRD version requires a structural object schema; server dry-run validates it".into());
        }
        if version["served"] == true {
            served_versions.push(name.into());
        }
        if version["storage"] == true {
            storage += 1;
            if version["served"] != true {
                return Err("CRD storage version must be served".into());
            }
        }
    }
    if storage != 1 || served_versions.is_empty() {
        return Err("CRD needs exactly one served storage version".into());
    }
    Ok(CrdDefinition {
        name,
        group,
        plural,
        kind,
        served_versions,
        object,
    })
}

pub fn documents(manifest: &str, limit: usize) -> Result<Vec<Value>> {
    if manifest.len() > MAX_EXTENSION_BYTES {
        return Err("extension manifests exceed4 MiB".into());
    }
    let options = serde_saphyr::options! {budget:serde_saphyr::budget!{max_events:200_000,max_aliases:0,max_anchors:0,max_depth:64,max_inclusion_depth:0,max_documents:limit,max_nodes:100_000,max_total_scalar_bytes:MAX_EXTENSION_BYTES,max_merge_keys:0},merge_keys:serde_saphyr::MergeKeyPolicy::Error,duplicate_keys:serde_saphyr::DuplicateKeyPolicy::Error,with_snippet:false};
    let docs: Vec<Value> = serde_saphyr::from_multiple_with_options(manifest, options).map_err(
        |_| "extension manifests are malformed or exceed bounded YAML limits; contents suppressed",
    )?;
    let docs = docs
        .into_iter()
        .filter(|doc| !doc.is_null())
        .collect::<Vec<_>>();
    if docs.len() > limit {
        return Err("extension resource count exceeded".into());
    }
    Ok(docs)
}
pub fn validate_namespace(namespace: &str) -> Result<()> {
    if !safety::valid_id(namespace)
        || ["kube-system", "kube-public", "kube-node-lease"].contains(&namespace)
    {
        return Err("extension requires one explicit non-system namespace".into());
    }
    Ok(())
}
fn dns_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value
            .split('.')
            .all(|part| part.len() <= 63 && safety::valid_id(part))
}
pub fn namespaced_identity(object: &mut Value, namespace: &str) -> Result<ObjectIdentity> {
    validate_namespace(namespace)?;
    if !object.is_object()
        || !object["metadata"].is_object()
        || ["labels", "annotations"].iter().any(|key| {
            !object["metadata"][key].is_null()
                && object["metadata"][key]
                    .as_object()
                    .is_none_or(|values| values.values().any(|value| !value.is_string()))
        })
    {
        return Err("extension object has malformed metadata".into());
    }
    let api_version = object["apiVersion"]
        .as_str()
        .filter(|value| value.len() <= 253)
        .ok_or("extension apiVersion missing")?
        .to_owned();
    let kind = object["kind"]
        .as_str()
        .filter(|value| value.len() <= 63)
        .ok_or("extension kind missing")?
        .to_owned();
    let name = object["metadata"]["name"]
        .as_str()
        .filter(|name| dns_name(name))
        .ok_or("extension requires an explicit valid metadata.name")?
        .to_owned();
    if object["metadata"].get("generateName").is_some() {
        return Err("extension generateName is unsupported".into());
    }
    if let Some(ns) = object["metadata"].get("namespace")
        && ns != namespace
    {
        return Err("extension escaped the reviewed namespace".into());
    }
    object["metadata"]["namespace"] = json!(namespace);
    Ok(ObjectIdentity {
        api_version,
        kind,
        namespace: namespace.into(),
        name,
    })
}
pub fn check_gitops(object: &Value) -> Result<()> {
    for field in ["labels", "annotations"] {
        if let Some(values) = object["metadata"][field].as_object()
            && (values.keys().any(|key| {
                [
                    "argocd.argoproj.io/",
                    "kustomize.toolkit.fluxcd.io/",
                    "helm.toolkit.fluxcd.io/",
                    "fluxcd.io/",
                ]
                .iter()
                .any(|prefix| key.starts_with(prefix))
            }) || values
                .get("app.kubernetes.io/managed-by")
                .and_then(Value::as_str)
                .is_some_and(|value| {
                    ["argocd", "flux", "fluxcd"].contains(&value.to_ascii_lowercase().as_str())
                }))
        {
            return Err("GitOps-managed extension requires changes in its source".into());
        }
    }
    Ok(())
}
pub fn native_api(api: &str, kind: &str) -> Result<ApiResource> {
    let plural = match (api,kind) {
        ("v1","Secret")=>"secrets",("v1","ConfigMap")=>"configmaps",("v1","ServiceAccount")=>"serviceaccounts",("v1","Pod")=>"pods",("batch/v1","Job")=>"jobs",("rbac.authorization.k8s.io/v1","Role")=>"roles",("rbac.authorization.k8s.io/v1","RoleBinding")=>"rolebindings",
        _ => return Err("reviewed hooks allow only namespaced Pod/Job/Secret/ConfigMap/ServiceAccount/Role/RoleBinding resources".into()),
    };
    let (group, version) = api.split_once('/').unwrap_or(("", api));
    Ok(ApiResource {
        group: group.into(),
        version: version.into(),
        api_version: api.into(),
        kind: kind.into(),
        plural: plural.into(),
    })
}

/// Prove a bounded, backwards-compatible update without performing storage
/// migration. Only validation relaxations and optional fields absent from all
/// captured instances are accepted; unsupported changes fail closed.
pub fn compatible_upgrade(
    previous: &CrdDefinition,
    next: &CrdDefinition,
    instances: &[Value],
) -> Result<()> {
    if previous.name != next.name || previous.kind != next.kind {
        return Err("CRD API identity changes require an explicit migration".into());
    }
    let mut old =
        normalized_crd_spec(&previous.object["spec"]).ok_or("invalid previous CRD spec")?;
    let mut new = normalized_crd_spec(&next.object["spec"]).ok_or("invalid next CRD spec")?;
    let old_versions = old["versions"]
        .as_array_mut()
        .ok_or("invalid previous versions")?;
    let new_versions = new["versions"]
        .as_array_mut()
        .ok_or("invalid next versions")?;
    if old_versions.len() != new_versions.len() {
        return Err(
            "adding/removing served or storage versions requires migration; schema update refused"
                .into(),
        );
    }
    for (old, new) in old_versions.iter_mut().zip(new_versions) {
        let old_schema = old
            .as_object_mut()
            .ok_or("invalid version")?
            .remove("schema")
            .ok_or("old schema missing")?;
        let new_schema = new
            .as_object_mut()
            .ok_or("invalid version")?
            .remove("schema")
            .ok_or("new schema missing")?;
        if old != new {
            return Err("CRD versions, served/storage flags, subresources and printer settings must remain unchanged; migrate separately".into());
        }
        let values = instances.iter().collect::<Vec<_>>();
        widening(
            &old_schema["openAPIV3Schema"],
            &new_schema["openAPIV3Schema"],
            &values,
            0,
        )?;
    }
    if old != new {
        return Err(
            "CRD group, names, scope, conversion or non-schema spec changed; migration required"
                .into(),
        );
    }
    Ok(())
}
fn widening(old: &Value, new: &Value, values: &[&Value], depth: usize) -> Result<()> {
    if depth > 64 {
        return Err("schema compatibility depth exceeded".into());
    }
    if old == new {
        return Ok(());
    }
    let old = old
        .as_object()
        .ok_or("unsupported schema compatibility form")?;
    let new = new
        .as_object()
        .ok_or("unsupported schema compatibility form")?;
    for (key, previous) in old {
        let next = new.get(key);
        match key.as_str() {
            "description" | "title" => {}
            "properties" => {
                let before = previous.as_object().ok_or("invalid schema properties")?;
                let after = next
                    .and_then(Value::as_object)
                    .ok_or("removing schema properties can prune stored data; refused")?;
                for (name, previous) in before {
                    let next = after
                        .get(name)
                        .ok_or("removing schema properties can prune stored data; refused")?;
                    let children = values
                        .iter()
                        .filter_map(|value| value.get(name))
                        .collect::<Vec<_>>();
                    widening(previous, next, &children, depth + 1)?;
                }
                for name in after.keys().filter(|name| !before.contains_key(*name)) {
                    if old.get("additionalProperties").is_some()
                        || old.get("x-kubernetes-preserve-unknown-fields") == Some(&json!(true))
                    {
                        return Err("new properties could constrain/prune previously preserved unknown data; refused".into());
                    }
                    no_new_defaults(&after[name], depth + 1)?;
                    if values.iter().any(|value| value.get(name).is_some()) {
                        return Err("new property already exists in captured data; independent data migration/validation required".into());
                    }
                }
            }
            "items" => {
                let next = next.ok_or("removing array item schema can alter pruning; refused")?;
                let children = values
                    .iter()
                    .filter_map(|value| value.as_array())
                    .flatten()
                    .collect::<Vec<_>>();
                widening(previous, next, &children, depth + 1)?;
            }
            "required" => {
                let before = previous.as_array().ok_or("invalid required schema")?;
                if let Some(next) = next {
                    let after = next.as_array().ok_or("invalid required schema")?;
                    if after.iter().any(|value| !before.contains(value)) {
                        return Err("new required fields can invalidate instances; refused".into());
                    }
                }
            }
            "enum" => {
                if let Some(next) = next {
                    let before = previous.as_array().ok_or("invalid enum")?;
                    let after = next.as_array().ok_or("invalid enum")?;
                    if before.iter().any(|value| !after.contains(value)) {
                        return Err("narrower enum can invalidate instances; refused".into());
                    }
                }
            }
            "minimum" | "maximum" | "exclusiveMinimum" | "exclusiveMaximum" | "minLength"
            | "maxLength" | "minItems" | "maxItems" | "minProperties" | "maxProperties"
            | "pattern" | "multipleOf" => {
                if next.is_some_and(|next| next != previous) {
                    return Err("changed validation constraint requires independent migration review; only removal is supported".into());
                }
            }
            "nullable" if previous == false && next == Some(&Value::Bool(true)) => {}
            _ => {
                if next != Some(previous) {
                    return Err(format!(
                        "schema field {} changed outside the proven compatible subset",
                        safety::safe_text(key, 80)
                    ));
                }
            }
        }
    }
    for (key, value) in new.iter().filter(|(key, _)| !old.contains_key(*key)) {
        match key.as_str() {
            "description" | "title" => {}
            "nullable" if value == true => {}
            "properties" => {
                if old.get("type") != Some(&json!("object"))
                    || old.get("additionalProperties").is_some()
                    || old.get("x-kubernetes-preserve-unknown-fields") == Some(&json!(true))
                {
                    return Err(
                        "new object properties may change unknown-field pruning; refused".into(),
                    );
                }
                let props = value.as_object().ok_or("invalid new properties")?;
                for property in props.values() {
                    no_new_defaults(property, depth + 1)?;
                }
                if props
                    .keys()
                    .any(|name| values.iter().any(|value| value.get(name).is_some()))
                {
                    return Err(
                        "new field already exists in captured data; migration required".into(),
                    );
                }
            }
            _ => {
                return Err(format!(
                    "new validation field {} can narrow schema; refused",
                    safety::safe_text(key, 80)
                ));
            }
        }
    }
    Ok(())
}

fn no_new_defaults(value: &Value, depth: usize) -> Result<()> {
    if depth > 64 {
        return Err("new schema subtree depth exceeded".into());
    }
    match value {
        Value::Object(map) => {
            if map.contains_key("default") {
                return Err("new optional fields with defaults can mutate existing reads; explicit migration required".into());
            }
            for child in map.values() {
                no_new_defaults(child, depth + 1)?;
            }
        }
        Value::Array(values) => {
            for child in values {
                no_new_defaults(child, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// A storage-only migration cannot reinterpret or prune any stored data. All
/// retained/new version schemas and subresource/default settings are identical.
pub fn migration_versions(
    previous: &CrdDefinition,
    next: &CrdDefinition,
) -> Result<(String, String)> {
    let mut before =
        normalized_crd_spec(&previous.object["spec"]).ok_or("invalid previous migration schema")?;
    let mut after =
        normalized_crd_spec(&next.object["spec"]).ok_or("invalid desired migration schema")?;
    let old = before
        .as_object_mut()
        .ok_or("invalid previous spec")?
        .remove("versions")
        .and_then(|value| value.as_array().cloned())
        .ok_or("previous versions missing")?;
    let new = after
        .as_object_mut()
        .ok_or("invalid desired spec")?
        .remove("versions")
        .and_then(|value| value.as_array().cloned())
        .ok_or("desired versions missing")?;
    if before != after || previous.name != next.name {
        return Err("storage migration preserves exact API identity, metadata policy, scope and None conversion".into());
    }
    let storage = |versions: &[Value]| -> Result<String> {
        versions
            .iter()
            .find(|version| version["storage"] == true && version["served"] == true)
            .and_then(|version| version["name"].as_str())
            .map(str::to_owned)
            .ok_or_else(|| "migration requires one served storage version".into())
    };
    let source = storage(&old)?;
    let target = storage(&new)?;
    let template = |version: &Value| -> Result<Value> {
        let mut version = version.clone();
        let map = version
            .as_object_mut()
            .ok_or("invalid version definition")?;
        for key in ["name", "storage", "served"] {
            map.remove(key);
        }
        Ok(version)
    };
    let baseline = template(old.first().ok_or("old schema versions missing")?)?;
    for version in old.iter().chain(&new) {
        if template(version)? != baseline {
            return Err("storage migration requires identical structural schemas, defaults and version settings; conversion/schema changes are unsupported".into());
        }
    }
    for previous in &old {
        let name = previous["name"]
            .as_str()
            .ok_or("old version name missing")?;
        let next = new
            .iter()
            .find(|version| version["name"] == name)
            .ok_or("storage migration never removes an existing version; retain old served APIs")?;
        if previous["served"] != next["served"]
            && !(name == target && previous["served"] == false && next["served"] == true)
        {
            return Err(
                "storage migration cannot change existing served-version membership".into(),
            );
        }
    }
    if new.iter().any(|version| {
        !old.iter()
            .any(|previous| previous["name"] == version["name"])
            && version["name"] != target
    }) {
        return Err("storage migration can add only the new storage version".into());
    }
    Ok((source, target))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hook(name: &str, kind: &str, event: &str, weight: i32) -> Value {
        json!({"apiVersion":if kind=="Job"{"batch/v1"}else{"v1"},"kind":kind,"metadata":{"name":name,"namespace":"staging","labels":{"app.kubernetes.io/managed-by":"Helm"},"annotations":{"meta.helm.sh/release-name":"api","meta.helm.sh/release-namespace":"staging","helm.sh/hook":event,"helm.sh/hook-weight":weight.to_string()}},"spec":{"template":{"spec":{"containers":[{"name":"job","image":"fixture"}]}}}})
    }
    fn manifests(objects: &[Value]) -> String {
        objects
            .iter()
            .map(|object| serde_json::to_string(object).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n---\n")
    }
    fn definition() -> Value {
        json!({"apiVersion":"apiextensions.k8s.io/v1","kind":"CustomResourceDefinition","metadata":{"name":"widgets.example.com"},"spec":{"group":"example.com","scope":"Namespaced","names":{"plural":"widgets","kind":"Widget"},"versions":[{"name":"v1","served":true,"storage":true,"schema":{"openAPIV3Schema":{"type":"object","properties":{"spec":{"type":"object","properties":{"value":{"type":"string"}}}}}}}]}})
    }
    #[test]
    fn hook_order_events_default_deletion_and_full_seal() -> Result<()> {
        let docs = manifests(&[
            hook("z-config", "ConfigMap", "pre-install", 0),
            hook("a-job", "Job", "pre-install,post-upgrade", 0),
            hook("first", "Pod", "pre-install", -1),
        ]);
        assert!(hooks_from_manifests(&docs, "staging", "api", Operation::Install, false).is_err());
        let plan = hooks_from_manifests(&docs, "staging", "api", Operation::Install, true)?;
        assert_eq!(
            plan.for_event("pre-install")
                .iter()
                .map(|hook| hook.identity.name.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "a-job", "z-config"]
        );
        assert!(plan.for_event("post-upgrade").is_empty());
        assert!(plan.all.iter().all(Hook::deletes_before_creation));
        assert!(plan.canonical.contains("post-upgrade"));
        assert!(
            plan.review()
                .contains("not automatically removed by uninstall")
        );
        Ok(())
    }
    #[test]
    fn hook_identity_collision_defaults_and_exact_desired_arrays() -> Result<()> {
        let desired = hook("work", "Job", "pre-delete", 1);
        let plan = hooks_from_manifests(
            &manifests(std::slice::from_ref(&desired)),
            "staging",
            "api",
            Operation::Uninstall,
            true,
        )?;
        let hook = &plan.all[0];
        let mut current = desired.clone();
        current["metadata"]["uid"] = json!("live");
        current["spec"]["backoffLimit"] = json!(6);
        assert!(hook.matches_existing(&current));
        current["spec"]["template"]["spec"]["containers"][0]["image"] = json!("other");
        assert!(!hook.matches_existing(&current));
        current = desired.clone();
        current["metadata"]["annotations"]["meta.helm.sh/release-name"] = json!("foreign");
        assert!(!hook.matches_existing(&current));
        assert!(
            hooks_from_manifests(
                &manifests(&[desired.clone(), desired]),
                "staging",
                "api",
                Operation::Uninstall,
                true
            )
            .is_err()
        );
        Ok(())
    }
    #[test]
    fn hooks_reject_scope_unknown_metadata_and_hidden_output() -> Result<()> {
        for variant in 0..7 {
            let mut object = hook("work", "Job", "pre-install", 0);
            match variant {
                0 => object["metadata"]["namespace"] = json!("foreign"),
                1 => object["kind"] = json!("ClusterRole"),
                2 => object["metadata"]["annotations"]["helm.sh/hook"] = json!("crd-install"),
                3 => {
                    object["metadata"]["annotations"]["helm.sh/hook-weight"] =
                        json!("999999999999999999999999999")
                }
                4 => {
                    object["metadata"]["annotations"]["helm.sh/hook-output-log-policy"] =
                        json!("hook-failed")
                }
                5 => {
                    object["metadata"]["annotations"]["meta.helm.sh/release-name"] =
                        json!("foreign")
                }
                _ => object["metadata"]["labels"]["argocd.argoproj.io/instance"] = json!("source"),
            };
            assert!(
                hooks_from_manifests(
                    &manifests(&[object]),
                    "staging",
                    "api",
                    Operation::Install,
                    true
                )
                .is_err()
            );
        }
        assert!(documents("apiVersion: v1\nkind: Job\nkind: Pod\n", 1).is_err());
        assert!(documents("a: &x private\nb: *x\n", 1).is_err());
        Ok(())
    }
    #[test]
    fn hook_json_must_match_manifest_and_policy_retention_is_explicit() -> Result<()> {
        let mut object = hook("work", "Job", "pre-install,post-install", -2);
        object["metadata"]["annotations"]["helm.sh/hook-delete-policy"] =
            json!("hook-succeeded,hook-failed");
        object["metadata"]["annotations"]["helm.sh/resource-policy"] = json!("keep");
        let mut raw = json!([{"name":"work","kind":"Job","manifest":object.to_string(),"events":["pre-install","post-install"],"weight":-2,"delete_policies":["hook-succeeded","hook-failed"]}]);
        let plan = hooks(&raw, "staging", "api", Operation::Install, true)?;
        assert!(!plan.all[0].deletes_before_creation());
        assert!(
            plan.all[0].deletes_on_success()
                && plan.all[0].deletes_on_failure()
                && plan.all[0].keep_annotation
        );
        raw[0]["weight"] = json!(2);
        assert!(hooks(&raw, "staging", "api", Operation::Install, true).is_err());
        Ok(())
    }
    #[test]
    fn stored_hook_omitted_defaults_cannot_change_the_reviewed_execution_policy() -> Result<()> {
        let object = hook("work", "Job", "pre-install", 0);
        let base = json!([{"name":"work","kind":"Job","manifest":object.to_string(),"events":["pre-install"]}]);
        hooks(&base, "staging", "api", Operation::Install, true)?;
        let mut empty = base.clone();
        empty[0]["delete_policies"] = json!([]);
        hooks(&empty, "staging", "api", Operation::Install, true)?;
        for raw_weight in [None, Some(json!(0)), Some(json!("1")), Some(Value::Null)] {
            let object = hook("work", "Job", "pre-install", 1);
            let mut changed = base.clone();
            changed[0]["manifest"] = json!(object.to_string());
            if let Some(value) = raw_weight {
                changed[0]["weight"] = value;
            }
            assert!(hooks(&changed, "staging", "api", Operation::Install, true).is_err());
        }
        for raw_policy in [
            None,
            Some(json!([])),
            Some(Value::Null),
            Some(json!("hook-succeeded")),
            Some(json!(["unknown"])),
        ] {
            let mut object = object.clone();
            object["metadata"]["annotations"]["helm.sh/hook-delete-policy"] =
                json!("hook-succeeded");
            let mut changed = base.clone();
            changed[0]["manifest"] = json!(object.to_string());
            if let Some(value) = raw_policy {
                changed[0]["delete_policies"] = value;
            }
            assert!(hooks(&changed, "staging", "api", Operation::Install, true).is_err());
        }
        for invalid in [Value::Null, json!("pre-install"), json!(["unknown"])] {
            let mut changed = base.clone();
            changed[0]["events"] = invalid;
            assert!(hooks(&changed, "staging", "api", Operation::Install, true).is_err());
        }
        let mut changed = base;
        changed[0]["output_log_policies"] = Value::Null;
        assert!(hooks(&changed, "staging", "api", Operation::Install, true).is_err());
        Ok(())
    }
    #[test]
    fn stored_hook_equal_weight_name_ties_preserve_authoritative_array_order() -> Result<()> {
        let record = |object: Value| json!({"name":"same","kind":object["kind"],"manifest":object.to_string(),"events":["pre-install"]});
        let job = record(hook("same", "Job", "pre-install", 0));
        let config = record(hook("same", "ConfigMap", "pre-install", 0));
        let first = hooks(
            &json!([job.clone(), config.clone()]),
            "staging",
            "api",
            Operation::Install,
            true,
        )?;
        assert_eq!(
            first
                .for_event("pre-install")
                .iter()
                .map(|hook| hook.identity.kind.as_str())
                .collect::<Vec<_>>(),
            vec!["Job", "ConfigMap"]
        );
        assert_eq!(first.all[0].identity.kind, "Job");
        let reversed = hooks(
            &json!([config.clone(), job.clone()]),
            "staging",
            "api",
            Operation::Install,
            true,
        )?;
        assert_eq!(
            reversed.for_event("pre-install")[0].identity.kind,
            "ConfigMap"
        );
        assert_ne!(first.canonical, reversed.canonical);
        assert!(
            hooks(
                &json!([job.clone(), config, job]),
                "staging",
                "api",
                Operation::Install,
                true
            )
            .is_err()
        );
        Ok(())
    }
    #[test]
    fn crds_are_create_only_namespaced_schema_bound_and_gate_custom_api() -> Result<()> {
        let object = definition();
        assert!(crds(&object.to_string(), false).is_err());
        let plan = crds(&object.to_string(), true)?;
        let def = &plan.definitions[0];
        assert_eq!(
            def.resource_api("example.com/v1", "Widget")
                .map(|api| api.plural),
            Some("widgets".into())
        );
        assert!(def.resource_api("foreign.com/v1", "Widget").is_none());
        assert!(def.resource_api("example.com/v2", "Widget").is_none());
        let mut current = object;
        current["metadata"]["uid"] = json!("live");
        current["spec"]["conversion"] = json!({"strategy":"None"});
        assert!(def.matches_existing(&current));
        current["status"]["conditions"] = json!([{"type":"Established","status":"True"}]);
        assert!(CrdDefinition::established(&current));
        current["metadata"]["deletionTimestamp"] = json!("2026-10-22T00:00:00Z");
        assert!(!CrdDefinition::established(&current));
        Ok(())
    }
    #[test]
    fn crd_schema_comparison_accepts_only_known_defaults_and_rejects_nested_additions() -> Result<()>
    {
        let desired = definition();
        let definition = crd_definition(desired.clone())?;
        let mut actual = desired.clone();
        actual["spec"]["names"]["singular"] = json!("widget");
        actual["spec"]["names"]["listKind"] = json!("WidgetList");
        actual["spec"]["names"]["shortNames"] = json!([]);
        actual["spec"]["names"]["categories"] = json!([]);
        actual["spec"]["conversion"] = json!({"strategy":"None"});
        actual["spec"]["preserveUnknownFields"] = json!(false);
        actual["spec"]["versions"][0]["deprecated"] = json!(false);
        actual["spec"]["versions"][0]["additionalPrinterColumns"] = json!([]);
        actual["spec"]["versions"][0]["selectableFields"] = json!([]);
        assert!(definition.matches_existing(&actual));
        for change in 0..5 {
            let mut changed = actual.clone();
            match change {
                0 => {
                    changed["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]
                        ["properties"]["injected"] = json!({"type":"string"})
                }
                1 => {
                    changed["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["x-kubernetes-validations"] =
                        json!([{"rule":"self.spec.value != 'forbidden'"}])
                }
                2 => {
                    changed["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]
                        ["required"] = json!(["value"])
                }
                3 => changed["spec"]["names"]["shortNames"] = json!(["injected"]),
                _ => changed["spec"]["versions"][0]["deprecated"] = json!(true),
            }
            assert!(!definition.matches_existing(&changed));
        }
        let mut malformed = actual;
        malformed["spec"]["names"] = json!("malformed");
        assert!(!definition.matches_existing(&malformed));
        let mut explicit = desired.clone();
        explicit["spec"]["names"]["singular"] = json!("");
        explicit["spec"]["names"]["listKind"] = json!("");
        assert!(crd_definition(explicit)?.matches_existing(&desired));
        Ok(())
    }
    #[test]
    fn crds_reject_cluster_webhook_identity_schema_and_ownership_hazards() {
        for variant in 0..7 {
            let mut object = definition();
            match variant {
                0 => object["spec"]["scope"] = json!("Cluster"),
                1 => {
                    object["spec"]["conversion"] = json!({"strategy":"Webhook","webhook":{"clientConfig":{"url":"https://foreign"}}})
                }
                2 => object["metadata"]["name"] = json!("wrong.example.com"),
                3 => object["spec"]["versions"][0]["schema"] = Value::Null,
                4 => object["metadata"]["ownerReferences"] = json!([{"uid":"foreign"}]),
                5 => object["spec"]["versions"][0]["storage"] = json!(false),
                _ => object["spec"]["group"] = json!("evil.k8s.io"),
            };
            assert!(crds(&object.to_string(), true).is_err());
        }
        assert!(crds(&manifests(&vec![definition(); MAX_CRDS + 1]), true).is_err());
    }
    #[test]
    fn schema_upgrade_proves_optional_expansion_and_relaxation_but_refuses_migration() -> Result<()>
    {
        let old = crd_definition(definition())?;
        let instance = json!({"spec":{"value":"known"}});
        let mut optional = definition();
        optional["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]["properties"]
            ["extra"] = json!({"type":"string"});
        let next = crd_definition(optional.clone())?;
        compatible_upgrade(&old, &next, std::slice::from_ref(&instance))?;
        let mut with_extra = instance;
        with_extra["spec"]["extra"] = json!(17);
        assert!(compatible_upgrade(&old, &next, &[with_extra]).is_err());
        for variant in 0..7 {
            let mut changed = optional.clone();
            match variant {
                0 => {
                    changed["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]
                        ["properties"]["extra"]["default"] = json!("mutates reads")
                }
                1 => {
                    changed["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]
                        ["required"] = json!(["extra"])
                }
                2 => changed["spec"]["versions"][0]["name"] = json!("v2"),
                3 => {
                    changed["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]
                        ["properties"]["value"]["type"] = json!("integer")
                }
                4 => {
                    changed["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]
                        ["properties"]["value"]["maxLength"] = json!(5)
                }
                5 => {
                    changed["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]["properties"].as_object_mut().ok_or("properties")?.remove("value");
                }
                _ => {
                    changed["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]
                        ["properties"]["extra"] = json!({"type":"object","properties":{"nested":{"type":"string","default":"hidden"}}})
                }
            };
            assert!(
                compatible_upgrade(&old, &crd_definition(changed)?, &[]).is_err(),
                "unsafe variant {variant}"
            );
        }
        let mut limited = definition();
        limited["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]["properties"]
            ["value"]["maxLength"] = json!(5);
        limited["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]["required"] =
            json!(["value"]);
        compatible_upgrade(&crd_definition(limited)?, &old, &[])?;
        for field in [
            "additionalProperties",
            "x-kubernetes-preserve-unknown-fields",
        ] {
            let mut previous = definition();
            previous["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]
                [field] = json!(true);
            let mut after = previous.clone();
            after["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]["properties"]
                ["extra"] = json!({"type":"string"});
            assert!(
                compatible_upgrade(&crd_definition(previous)?, &crd_definition(after)?, &[])
                    .is_err()
            );
        }
        Ok(())
    }
    #[test]
    fn storage_migration_only_changes_storage_and_adds_identical_target_version() -> Result<()> {
        let previous = crd_definition(definition())?;
        let mut next = definition();
        let mut target = next["spec"]["versions"][0].clone();
        target["name"] = json!("v2");
        next["spec"]["versions"][0]["storage"] = json!(false);
        next["spec"]["versions"]
            .as_array_mut()
            .ok_or("versions")?
            .push(target);
        let desired = crd_definition(next.clone())?;
        assert_eq!(
            migration_versions(&previous, &desired)?,
            ("v1".into(), "v2".into())
        );
        assert_eq!(
            migration_versions(&desired, &desired)?,
            ("v2".into(), "v2".into())
        );
        for variant in 0..5 {
            let mut changed = next.clone();
            match variant {
                0 => {
                    changed["spec"]["versions"][1]["schema"]["openAPIV3Schema"]["properties"]["spec"]
                        ["properties"]["value"]["type"] = json!("integer")
                }
                1 => changed["spec"]["versions"][0]["served"] = json!(false),
                2 => {
                    changed["spec"]["versions"]
                        .as_array_mut()
                        .ok_or("versions")?
                        .remove(0);
                }
                3 => {
                    changed["spec"]["versions"][1]["schema"]["openAPIV3Schema"]["properties"]["spec"]
                        ["properties"]["value"]["default"] = json!("new")
                }
                _ => changed["spec"]["names"]["singular"] = json!("other"),
            };
            assert!(migration_versions(&previous, &crd_definition(changed)?).is_err());
        }
        Ok(())
    }
}
