//! Reviewed, bounded node scheduling and PDB-respecting workload evacuation.
//! A drain never force-deletes Pods or automatically restores scheduling.
use crate::{Result, gateway::Gateway, safety};
use kube::{
    Api, Client,
    api::{ApiResource, DynamicObject, ListParams},
};
use ring::digest::{SHA256, digest};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    future::Future,
    io::Write,
    path::Path,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_PODS: usize = 1000;
const MAX_EVICTIONS: usize = 64;
const MAX_INVENTORY_BYTES: usize = 8 * 1024 * 1024;
const API_BUDGET: Duration = Duration::from_secs(15);
const REVIEW_BUDGET: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    Cordon,
    Uncordon,
    Drain { seconds: u16 },
}
impl Request {
    fn name(self) -> &'static str {
        match self {
            Self::Cordon => "node-cordon",
            Self::Uncordon => "node-uncordon",
            Self::Drain { .. } => "node-drain",
        }
    }
    fn desired(self) -> bool {
        self != Self::Uncordon
    }
    fn budget(self) -> Duration {
        Duration::from_secs(match self {
            Self::Drain { seconds } => u64::from(seconds),
            _ => 30,
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct Identity {
    namespace: String,
    name: String,
    uid: String,
    version: String,
}
impl Identity {
    fn from_object(value: &Value, namespaced: bool) -> Result<Self> {
        let field = |key: &str| -> Result<String> {
            let text = value["metadata"][key]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or("target has incomplete identity")?;
            if text.len() > 253 || safety::safe_text(text, 253) != text {
                return Err("target identity exceeds its plain-text limit".into());
            }
            Ok(text.into())
        };
        let identity = Self {
            namespace: if namespaced {
                field("namespace")?
            } else {
                String::new()
            },
            name: field("name")?,
            uid: field("uid")?,
            version: field("resourceVersion")?,
        };
        if !dns_name(&identity.name) || (namespaced && !safety::valid_id(&identity.namespace)) {
            return Err("invalid target name or namespace".into());
        }
        Ok(identity)
    }
    fn label(&self) -> String {
        if self.namespace.is_empty() {
            format!("nodes/{}", self.name)
        } else {
            format!("{}/{}", self.namespace, self.name)
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct PodTarget {
    identity: Identity,
    retained: Option<&'static str>,
    controller: Option<(String, String, String, String)>,
}
#[derive(Clone)]
struct Inventory {
    pods: Vec<PodTarget>,
}
impl Inventory {
    fn evictable(&self) -> impl Iterator<Item = &PodTarget> {
        self.pods.iter().filter(|pod| pod.retained.is_none())
    }
    fn fingerprint(&self) -> String {
        let value: Vec<_> = self
            .pods
            .iter()
            .map(|pod| (&pod.identity, pod.retained))
            .collect();
        hash(serde_json::to_string(&value).unwrap_or_default().as_bytes())
    }
}
pub struct ReviewedNode {
    request: Request,
    node: Identity,
    node_content: Value,
    session: String,
    created: Instant,
    inventory: Inventory,
    summary: String,
}
impl ReviewedNode {
    pub fn summary(&self) -> &str {
        &self.summary
    }
    pub fn uid(&self) -> &str {
        &self.node.uid
    }
    pub fn confirmation(&self) -> String {
        self.node.label()
    }
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub complete: bool,
    pub node_cordoned: Option<bool>,
    pub evicted: Vec<String>,
    pub retained: Vec<String>,
    pub pending: Vec<String>,
    pub message: String,
}
impl Report {
    pub fn summary(&self) -> String {
        let state = match self.node_cordoned {
            Some(true) => "cordoned",
            Some(false) => "schedulable",
            None => "scheduling state unknown",
        };
        let mut text = format!(
            "{}\nNode: {state}\nEvacuated: {}  Retained: {}  Pending: {}",
            self.message,
            self.evicted.len(),
            self.retained.len(),
            self.pending.len()
        );
        for item in &self.retained {
            text.push_str(&format!("\nRETAINED {item}"));
        }
        for item in &self.pending {
            text.push_str(&format!("\nPENDING {item}"));
        }
        text
    }
}
fn dns_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 253 && name.split('.').all(safety::valid_id)
}
fn hash(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn resource(plural: &str, kind: &str, group: &str) -> ApiResource {
    ApiResource {
        group: group.into(),
        version: "v1".into(),
        api_version: if group.is_empty() {
            "v1".into()
        } else {
            format!("{group}/v1")
        },
        kind: kind.into(),
        plural: plural.into(),
    }
}
fn nodes(client: &Client) -> Api<DynamicObject> {
    Api::all_with(client.clone(), &resource("nodes", "Node", ""))
}
fn pods(client: &Client) -> Api<DynamicObject> {
    Api::all_with(client.clone(), &resource("pods", "Pod", ""))
}
fn value(object: DynamicObject) -> Result<Value> {
    serde_json::to_value(object).map_err(|_| "cannot decode node operation target".into())
}
fn api_failure(error: kube::Error) -> String {
    match &error {
        kube::Error::Api(status) if status.code == 429 => crate::errors::local(
            "Eviction blocked",
            "a PodDisruptionBudget or API throttling refused it; no forced deletion",
            "wait for replacement Pods to become ready, or review the PodDisruptionBudget, then prepare the drain again",
        ),
        _ => crate::errors::explain(
            "Node maintenance request",
            &error,
            &crate::errors::Scope::default(),
        ),
    }
}
async fn bounded<T>(
    future: impl Future<Output = std::result::Result<T, kube::Error>>,
) -> Result<T> {
    tokio::time::timeout(API_BUDGET, future)
        .await
        .map_err(|_| "Kubernetes request timed out; write outcome may be unknown".to_string())?
        .map_err(api_failure)
}
fn classify(value: &Value, node: &str) -> Result<PodTarget> {
    let identity = Identity::from_object(value, true)?;
    if value["spec"]["nodeName"].as_str() != Some(node) {
        return Err("Pod inventory returned another node; review blocked".into());
    }
    let references = value["metadata"]["ownerReferences"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let controllers: Vec<_> = references
        .iter()
        .filter(|owner| owner["controller"] == true)
        .collect();
    let retained = if value["metadata"]["annotations"]["kubernetes.io/config.mirror"].is_string() {
        Some("mirror Pod")
    } else if controllers
        .iter()
        .any(|owner| owner["kind"] == "DaemonSet" && owner["apiVersion"] == "apps/v1")
    {
        Some("DaemonSet Pod")
    } else if matches!(
        identity.namespace.as_str(),
        "kube-system" | "kube-public" | "kube-node-lease"
    ) {
        Some("protected system Pod")
    } else if matches!(
        value["status"]["phase"].as_str(),
        Some("Succeeded" | "Failed")
    ) {
        Some("completed Pod")
    } else {
        None
    };
    if retained.is_some() {
        return Ok(PodTarget {
            identity,
            retained,
            controller: None,
        });
    }
    if value["metadata"]["deletionTimestamp"].is_string() {
        return Err(format!(
            "{} is already terminating; wait and review again",
            identity.label()
        ));
    }
    if value["spec"]["priority"].as_i64().unwrap_or(0) >= 2_000_000_000
        || matches!(
            value["spec"]["priorityClassName"].as_str(),
            Some("system-node-critical" | "system-cluster-critical")
        )
    {
        return Err(format!(
            "{} has protected critical priority; drain blocked",
            identity.label()
        ));
    }
    if value["spec"]["volumes"].as_array().is_some_and(|volumes| {
        volumes.iter().any(|volume| {
            volume.get("emptyDir").is_some()
                || volume.get("hostPath").is_some()
                || volume.get("ephemeral").is_some()
        })
    }) {
        return Err(format!(
            "{} uses emptyDir, hostPath or ephemeral storage; drain blocked to avoid local data loss",
            identity.label()
        ));
    }
    if controllers.len() != 1 {
        return Err(format!(
            "{} has no single supported controller; unmanaged Pods are never force-deleted",
            identity.label()
        ));
    }
    let owner = controllers[0];
    let api = owner["apiVersion"].as_str().unwrap_or("");
    let kind = owner["kind"].as_str().unwrap_or("");
    if !matches!(
        (api, kind),
        ("apps/v1", "ReplicaSet" | "StatefulSet")
            | ("batch/v1", "Job")
            | ("v1", "ReplicationController")
    ) {
        return Err(format!(
            "{} has an unsupported controller; drain blocked",
            identity.label()
        ));
    }
    let name = owner["name"].as_str().unwrap_or("");
    let uid = owner["uid"].as_str().unwrap_or("");
    if !dns_name(name) || uid.is_empty() || safety::safe_text(uid, 253) != uid {
        return Err("Pod controller has an invalid identity".into());
    }
    Ok(PodTarget {
        identity,
        retained: None,
        controller: Some((api.into(), kind.into(), name.into(), uid.into())),
    })
}
async fn inventory(client: &Client, node: &str) -> Result<Inventory> {
    let api = pods(client);
    let mut params = ListParams::default()
        .fields(&format!("spec.nodeName={node}"))
        .limit(200);
    let mut result = Vec::new();
    let mut ids = BTreeSet::new();
    let mut bytes = 0usize;
    let mut list_version: Option<String> = None;
    for _ in 0..10 {
        let page = bounded(api.list(&params)).await?;
        if page
            .metadata
            .resource_version
            .as_ref()
            .is_none_or(String::is_empty)
        {
            return Err("Pod inventory has no snapshot version".into());
        }
        if let Some(version) = &list_version {
            if page.metadata.resource_version.as_ref() != Some(version) {
                return Err("Pod inventory snapshot changed between pages".into());
            }
        } else {
            list_version = page.metadata.resource_version.clone();
        }
        for object in page.items {
            let object = value(object)?;
            bytes = bytes.saturating_add(
                serde_json::to_vec(&object)
                    .map_err(|_| "cannot measure Pod inventory")?
                    .len(),
            );
            if result.len() == MAX_PODS || bytes > MAX_INVENTORY_BYTES {
                return Err(
                    "complete node inventory exceeds 1000 Pods or 8 MiB; drain blocked".into(),
                );
            }
            let target = classify(&object, node)?;
            if !ids.insert((
                target.identity.namespace.clone(),
                target.identity.name.clone(),
            )) {
                return Err("duplicate Pod identity in node inventory".into());
            }
            result.push(target);
        }
        match page.metadata.continue_.filter(|token| !token.is_empty()) {
            Some(token) => {
                params.continue_token = Some(token);
            }
            None => {
                result.sort_by(|a, b| {
                    (&a.identity.namespace, &a.identity.name)
                        .cmp(&(&b.identity.namespace, &b.identity.name))
                });
                let inventory = Inventory { pods: result };
                if inventory.evictable().count() > MAX_EVICTIONS {
                    return Err("drain exceeds 64 reviewed evictions; split maintenance into smaller workloads".into());
                }
                return Ok(inventory);
            }
        }
    }
    Err("complete node inventory exceeds 10 pages; drain blocked".into())
}
async fn check_owner(client: &Client, pod: &PodTarget) -> Result<()> {
    let Some((version, kind, name, uid)) = &pod.controller else {
        return Ok(());
    };
    let plural = match kind.as_str() {
        "ReplicaSet" => "replicasets",
        "StatefulSet" => "statefulsets",
        "Job" => "jobs",
        "ReplicationController" => "replicationcontrollers",
        _ => return Err("unsupported Pod controller".into()),
    };
    let group = version
        .split_once('/')
        .map(|(group, _)| group)
        .unwrap_or("");
    let api: Api<DynamicObject> = Api::namespaced_with(
        client.clone(),
        &pod.identity.namespace,
        &resource(plural, kind, group),
    );
    let owner = bounded(api.get(name)).await?;
    if owner.metadata.uid.as_ref() != Some(uid) || owner.metadata.deletion_timestamp.is_some() {
        return Err(format!(
            "{} controller disappeared, changed or is terminating; drain blocked",
            pod.identity.label()
        ));
    }
    Ok(())
}
fn scheduling_patch(node: &Identity, desired: bool) -> Value {
    json!([
        {"op":"test","path":"/metadata/uid","value":node.uid},
        {"op":"test","path":"/metadata/resourceVersion","value":node.version},
        {"op":"add","path":"/spec/unschedulable","value":desired}
    ])
}
async fn patch_scheduling(
    client: &Client,
    node: &Identity,
    desired: bool,
    dry_run: bool,
) -> Result<Value> {
    let uri = format!(
        "/api/v1/nodes/{}{}",
        node.name,
        if dry_run { "?dryRun=All" } else { "" }
    );
    let request = http::Request::patch(uri)
        .header(http::header::CONTENT_TYPE, "application/json-patch+json")
        .body(
            serde_json::to_vec(&scheduling_patch(node, desired))
                .map_err(|_| "cannot encode scheduling patch")?,
        )
        .map_err(|_| "cannot build scheduling request")?;
    bounded(client.request(request)).await
}
fn node_content(value: &Value) -> Value {
    let mut content = value.clone();
    if let Some(object) = content.as_object_mut() {
        object.remove("status");
    }
    if let Some(metadata) = content["metadata"].as_object_mut() {
        metadata.remove("resourceVersion");
        metadata.remove("managedFields");
    }
    content
}
fn check_scheduling_response(before: &Value, after: &Value, desired: bool) -> Result<()> {
    let mut expected = node_content(before);
    expected["spec"]["unschedulable"] = json!(desired);
    let mut actual = node_content(after);
    // Kubernetes omits the false bool from NodeSpec JSON after uncordon.
    if actual["spec"].get("unschedulable").is_none() {
        actual["spec"]["unschedulable"] = json!(false);
    }
    if actual != expected {
        return Err(
            "admission changed additional node scheduling fields; inspect the node before retrying"
                .into(),
        );
    }
    Ok(())
}
pub async fn prepare(gateway: &Gateway, node: &str, request: Request) -> Result<ReviewedNode> {
    if !dns_name(node) {
        return Err("invalid node name".into());
    }
    if let Request::Drain { seconds } = request
        && !(1..=300).contains(&seconds)
    {
        return Err("drain deadline must be 1..300 seconds".into());
    }
    let client = gateway.node_maintenance_access()?;
    tokio::time::timeout(Duration::from_secs(45), async {
        let current = value(bounded(nodes(&client).get(node)).await?)?;
        let identity = Identity::from_object(&current,false)?;
        if current["metadata"]["deletionTimestamp"].is_string() { return Err("node is terminating".into()); }
        let inventory = if matches!(request,Request::Drain { .. }) { inventory(&client,node).await? } else { Inventory { pods: vec![] } };
        for pod in inventory.evictable() { check_owner(&client,pod).await?; }
        let dry = patch_scheduling(&client,&identity,request.desired(),true).await?;
        check_scheduling_response(&current,&dry,request.desired())?;
        let mut summary = format!("{}\nserver: {}\ncontext: {}\ntarget: nodes/{node}\nUID: {}\nresourceVersion: {}\nScheduling: {} -> {}\n",request.name(),gateway.server,gateway.context,identity.uid,identity.version,if current["spec"]["unschedulable"] == true { "cordoned" } else { "schedulable" }, if request.desired() { "cordoned" } else { "schedulable" });
        if let Request::Drain { seconds } = request {
            summary.push_str(&format!("\nCordon, then policy/v1 eviction of {} reviewed Pods; deadline {seconds}s.\nEvery eviction checks its reviewed UID/resourceVersion and respects PodDisruptionBudgets and the Pod's termination grace period. No forced deletion, retry or automatic uncordon.\nAll namespaces inventoried. Mirror, DaemonSet, completed and protected system Pods remain. This is workload evacuation; retained Pods prevent a blanket safe-to-power-off guarantee.\nSingle-node clusters may lose workload availability. PVC relocation/capacity and external workload effects are not proven.\nConcurrent direct bindings or unschedulable-tolerating Pods can arrive; final inventory detects them but cannot prevent later arrivals.\n",inventory.evictable().count()));
            for pod in &inventory.pods {
                summary.push_str(&format!("{} {} UID {} RV {}{}\n",if pod.retained.is_some() { "RETAIN" } else { "EVICT" },pod.identity.label(),pod.identity.uid,pod.identity.version,pod.retained.map(|why|format!(" ({why})")).unwrap_or_default()));
            }
            summary.push_str(&format!("Inventory SHA-256: {}\n",inventory.fingerprint()));
        } else if request == Request::Cordon { summary.push_str("Existing Pods keep running. New ordinary scheduling stops; direct bindings and tolerating Pods can still arrive.\n"); }
        else { summary.push_str("New workloads may schedule on this node immediately.\n"); }
        summary.push_str(&format!("\nType nodes/{node} to approve within 60 seconds. Node status-only resourceVersion refresh is allowed; all node identity, metadata and spec stay bound. Evicted Pod resourceVersions stay exact. Other drift requires a fresh review."));
        Ok(ReviewedNode { request, node: identity, node_content: node_content(&current), session: gateway.operation_session().into(), created: Instant::now(), inventory, summary: safety::review_text(&summary,512*1024)? })
    }).await.map_err(|_| "node review exceeded 45-second budget".to_string())?
}

struct Audit {
    file: std::fs::File,
}
impl Audit {
    fn start(
        directory: &Path,
        context: &str,
        target: &Identity,
        action: &str,
        inventory: Option<&Inventory>,
    ) -> Result<Self> {
        safety::ensure_private_dir(directory)?;
        if std::fs::read_dir(directory)
            .map_err(|_| "cannot inspect audit spool")?
            .take(1001)
            .count()
            >= 1000
        {
            return Err("audit spool full; archive it before node maintenance".into());
        }
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "clock error")?
            .as_nanos();
        let path = directory.join(format!("{stamp}-{}-node.jsonl", std::process::id()));
        let record = json!({"schema_version":1,"stage":"intent","context":context,"namespace":target.namespace,"resource":if target.namespace.is_empty(){"nodes"}else{"pods"},"name":target.name,"uid":target.uid,"resourceVersion":target.version,"action":action,"inventory_sha256":inventory.map(Inventory::fingerprint)});
        safety::checked_path(&path)?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&path)
            .map_err(|_| "cannot create durable node maintenance intent")?;
        let mut audit = Self { file };
        audit.record(&record)?;
        // Sync the parent after creating the intent, before any Kubernetes write.
        #[cfg(unix)]
        std::fs::File::open(directory)
            .and_then(|dir| dir.sync_all())
            .map_err(|_| "cannot sync node maintenance audit directory")?;
        Ok(audit)
    }
    fn record(&mut self, record: &Value) -> Result<()> {
        writeln!(self.file, "{record}")
            .and_then(|_| self.file.sync_all())
            .map_err(|_| "node operation may have executed; durable audit write failed".into())
    }
    fn outcome(&mut self, status: &str) -> Result<()> {
        self.record(&json!({"stage":"outcome","status":status}))
    }
}
fn eviction_body(pod: &Identity) -> Value {
    // kube 4.2's convenience eviction encoder spells this delete_options;
    // construct the standard policy/v1 body to preserve server preconditions.
    json!({"apiVersion":"policy/v1","kind":"Eviction","metadata":{"namespace":pod.namespace,"name":pod.name},"deleteOptions":{"preconditions":{"uid":pod.uid,"resourceVersion":pod.version}}})
}
async fn evict(client: &Client, pod: &Identity) -> Result<()> {
    let request = http::Request::post(format!(
        "/api/v1/namespaces/{}/pods/{}/eviction",
        pod.namespace, pod.name
    ))
    .header(http::header::CONTENT_TYPE, "application/json")
    .body(serde_json::to_vec(&eviction_body(pod)).map_err(|_| "cannot encode eviction")?)
    .map_err(|_| "cannot build eviction")?;
    let _: Value = bounded(client.request(request)).await?;
    Ok(())
}
fn recheck_inventory(reviewed: &Inventory, current: &Inventory) -> Result<()> {
    if reviewed.pods.len() != current.pods.len() {
        return Err("node Pod membership changed; review again".into());
    }
    for (before, after) in reviewed.pods.iter().zip(&current.pods) {
        if before.identity.namespace != after.identity.namespace
            || before.identity.name != after.identity.name
            || before.identity.uid != after.identity.uid
            || before.retained != after.retained
            || before.controller != after.controller
            || (before.retained.is_none() && before.identity.version != after.identity.version)
        {
            return Err(
                "node Pod identity, disposition or resourceVersion changed; review again".into(),
            );
        }
    }
    Ok(())
}
pub async fn execute(gateway: &Gateway, review: ReviewedNode, typed: &str) -> Result<Report> {
    let directory = safety::config_dir()?.join("audit");
    execute_at(gateway, review, typed, &directory).await
}
async fn execute_at(
    gateway: &Gateway,
    review: ReviewedNode,
    typed: &str,
    audit_dir: &Path,
) -> Result<Report> {
    let client = gateway.node_maintenance_access()?;
    if typed != review.confirmation() {
        return Err("confirmation must exactly match nodes/name".into());
    }
    if review.created.elapsed() > REVIEW_BUDGET || review.session != gateway.operation_session() {
        return Err("node approval expired or connection changed; review again".into());
    }
    let mut report = Report {
        complete: false,
        node_cordoned: None,
        evicted: vec![],
        retained: review
            .inventory
            .pods
            .iter()
            .filter_map(|p| {
                p.retained
                    .map(|why| format!("{} ({why})", p.identity.label()))
            })
            .collect(),
        pending: review
            .inventory
            .evictable()
            .map(|p| p.identity.label())
            .collect(),
        message: String::new(),
    };
    // Keep the durable intent and all post-intent failure handling outside timeout.
    let mut audit = Audit::start(
        audit_dir,
        &gateway.context,
        &review.node,
        review.request.name(),
        Some(&review.inventory),
    )?;
    let result = tokio::time::timeout(review.request.budget(), async {
        let current = value(bounded(nodes(&client).get(&review.node.name)).await?)?;
        let refreshed = Identity::from_object(&current,false)?;
        if refreshed.uid != review.node.uid || node_content(&current) != review.node_content {
            return Err("node identity, metadata or spec changed; review again".into());
        }
        if refreshed.version != review.node.version {
            audit.record(&json!({"stage":"progress","status_only_resourceVersion":refreshed.version}))?;
        }
        report.node_cordoned = Some(current["spec"]["unschedulable"] == true);
        if matches!(review.request,Request::Drain { .. }) {
            let fresh = inventory(&client,&review.node.name).await?;
            recheck_inventory(&review.inventory,&fresh)?;
            for pod in fresh.evictable() { check_owner(&client,pod).await?; }
        }
        // A failed/timeout patch has unknown outcome, even when a previous GET succeeded.
        report.node_cordoned = None;
        let changed = patch_scheduling(&client,&refreshed,review.request.desired(),false).await?;
        check_scheduling_response(&current,&changed,review.request.desired())?;
        report.node_cordoned = Some(review.request.desired());
        audit.record(&json!({"stage":"progress","node_cordoned":review.request.desired()}))?;
        if !matches!(review.request,Request::Drain { .. }) { return Ok(()); }
        let mut accepted = Vec::new();
        for pod in review.inventory.evictable() {
            let live_node = value(bounded(nodes(&client).get(&review.node.name)).await?)?;
            if live_node["metadata"]["uid"].as_str() != Some(&review.node.uid) || live_node["spec"]["unschedulable"] != true {
                report.node_cordoned = None;
                return Err("node identity or scheduling changed before eviction; inspect current state".into());
            }
            let api: Api<DynamicObject> = Api::namespaced_with(client.clone(),&pod.identity.namespace,&resource("pods","Pod",""));
            let current = value(bounded(api.get(&pod.identity.name)).await?)?;
            if classify(&current,&review.node.name)? != *pod { return Err(format!("{} changed before eviction; node remains cordoned",pod.identity.label())); }
            check_owner(&client,pod).await?;
            let mut child = Audit::start(audit_dir,&gateway.context,&pod.identity,"node-drain-evict",None)?;
            match evict(&client,&pod.identity).await {
                Ok(()) => { child.record(&json!({"stage":"progress","eviction_accepted":true}))?; accepted.push((pod.identity.clone(),child)); },
                Err(error) => { child.outcome("failed-or-unknown")?; return Err(format!("{}: {error}",pod.identity.label())); }
            }
        }
        // Waiting only confirms disappearance of the old UID; accepted eviction
        // is not reported as completed termination. No automatic retry occurs.
        while !accepted.is_empty() {
            let mut index = 0;
            while index < accepted.len() {
                let identity = &accepted[index].0;
                let api: Api<DynamicObject> = Api::namespaced_with(client.clone(),&identity.namespace,&resource("pods","Pod",""));
                let current = bounded(api.get_opt(&identity.name)).await?;
                if current.as_ref().is_none_or(|pod|pod.metadata.uid.as_ref() != Some(&identity.uid)) {
                    let (identity,mut child) = accepted.remove(index);
                    child.outcome("succeeded")?;
                    report.pending.retain(|label|*label != identity.label());
                    report.evicted.push(identity.label());
                } else { index += 1; }
            }
            if !accepted.is_empty() { tokio::time::sleep(Duration::from_millis(250)).await; }
        }
        let final_node = value(bounded(nodes(&client).get(&review.node.name)).await?)?;
        if final_node["metadata"]["uid"].as_str() != Some(&review.node.uid) || final_node["spec"]["unschedulable"] != true { report.node_cordoned = None; return Err("node identity or scheduling changed during drain; inspect current state".into()); }
        let fresh = inventory(&client,&review.node.name).await?;
        if fresh.evictable().next().is_some() { report.pending = fresh.evictable().map(|p|p.identity.label()).collect(); return Err("new or remaining eligible Pods were found after eviction; node remains cordoned; review again".into()); }
        report.retained = fresh.pods.iter().filter_map(|p|p.retained.map(|why|format!("{} ({why})",p.identity.label()))).collect();
        Ok(())
    }).await;
    match result {
        Ok(Ok(())) => {
            report.complete = true;
            report.message = match review.request { Request::Cordon => "Node cordoned; existing Pods retained.".into(), Request::Uncordon => "Node schedulable; new workloads may arrive.".into(), Request::Drain { .. } => "Reviewed workload evacuation completed. Retained Pods still require separate maintenance planning.".into() };
            audit.outcome("succeeded")?;
        }
        Ok(Err(error)) => {
            report.message = format!(
                "INCOMPLETE: {error}. Scheduling is never automatically restored; inspect the node and audit before retrying."
            );
            audit.outcome("failed-or-unknown")?;
        }
        Err(_) => {
            report.message = "INCOMPLETE: operation deadline expired. Accepted evictions can continue after disconnect; scheduling is never automatically restored. Inspect the node and audit before retrying.".into();
            audit.outcome("failed-or-unknown")?;
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use std::sync::{Arc, Mutex};

    fn node() -> Value {
        json!({"apiVersion":"v1","kind":"Node","metadata":{"name":"worker.test","uid":"node-uid","resourceVersion":"10"},"spec":{}})
    }
    fn cordoned() -> Value {
        let mut node = node();
        node["spec"]["unschedulable"] = json!(true);
        node["metadata"]["resourceVersion"] = json!("11");
        node
    }
    fn pod() -> Value {
        json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"app-abc","namespace":"test","uid":"pod-uid","resourceVersion":"20","ownerReferences":[{"apiVersion":"apps/v1","kind":"ReplicaSet","name":"app-rs","uid":"owner-uid","controller":true}]},"spec":{"nodeName":"worker.test","containers":[{"name":"app","image":"example.test/app:1"}]},"status":{"phase":"Running"}})
    }
    fn owner() -> Value {
        json!({"apiVersion":"apps/v1","kind":"ReplicaSet","metadata":{"name":"app-rs","namespace":"test","uid":"owner-uid","resourceVersion":"1"}})
    }
    fn list(items: Vec<Value>) -> Value {
        json!({"apiVersion":"v1","kind":"PodList","metadata":{"resourceVersion":"30"},"items":items})
    }
    type Calls = Arc<Mutex<Vec<(String, String, Value)>>>;
    type Replies = Vec<(&'static str, &'static str, u16, Value)>;
    fn client(replies: Replies) -> (Client, Calls) {
        let replies = Arc::new(Mutex::new(replies.into_iter()));
        let calls: Calls = Arc::new(Mutex::new(vec![]));
        let captured = calls.clone();
        let client = Client::new(
            tower::service_fn(move |request: http::Request<kube::client::Body>| {
                let replies = replies.clone();
                let calls = captured.clone();
                async move {
                    let method = request.method().to_string();
                    let path = request.uri().path().to_string();
                    let uri = request.uri().to_string();
                    let body = request
                        .into_body()
                        .collect()
                        .await
                        .map_err(std::io::Error::other)?
                        .to_bytes();
                    let body = if body.is_empty() {
                        Value::Null
                    } else {
                        serde_json::from_slice(&body).map_err(std::io::Error::other)?
                    };
                    calls
                        .lock()
                        .map_err(|_| std::io::Error::other("lock"))?
                        .push((method.clone(), uri, body));
                    let (expected_method, expected_path, status, value) = replies
                        .lock()
                        .map_err(|_| std::io::Error::other("lock"))?
                        .next()
                        .ok_or_else(|| {
                            std::io::Error::other(format!("unexpected {method} {path}"))
                        })?;
                    assert_eq!(method, expected_method);
                    assert_eq!(path, expected_path);
                    http::Response::builder()
                        .status(status)
                        .header("Content-Type", "application/json")
                        .body(http_body_util::Full::new(bytes::Bytes::from(
                            value.to_string(),
                        )))
                        .map_err(std::io::Error::other)
                }
            }),
            "test",
        );
        (client, calls)
    }
    fn runtime<T>(future: impl Future<Output = Result<T>>) -> Result<T> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "runtime")?
            .block_on(future)
    }
    fn review(request: Request, targets: Vec<PodTarget>) -> Result<ReviewedNode> {
        Ok(ReviewedNode {
            request,
            node: Identity::from_object(&node(), false)?,
            node_content: node_content(&node()),
            session: "test".into(),
            created: Instant::now(),
            inventory: Inventory { pods: targets },
            summary: "fixture review".into(),
        })
    }
    fn audit_dir() -> Result<std::path::PathBuf> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "clock")?
            .as_nanos();
        Ok(std::env::temp_dir()
            .canonicalize()
            .map_err(|_| "temp")?
            .join(format!("kborg-node-tests-{}-{stamp}", std::process::id())))
    }

    #[test]
    fn inventory_dispositions_protect_data_and_unmanaged_workloads() -> Result<()> {
        assert!(classify(&pod(), "worker.test")?.retained.is_none());
        for (pointer, replacement) in [
            ("/metadata/ownerReferences", json!([])),
            ("/spec/priorityClassName", json!("system-node-critical")),
            ("/spec/volumes", json!([{"name":"local","emptyDir":{}}])),
            (
                "/spec/volumes",
                json!([{"name":"local","hostPath":{"path":"/data"}}]),
            ),
            ("/spec/volumes", json!([{"name":"local","ephemeral":{}}])),
            ("/metadata/deletionTimestamp", json!("2026-01-01T00:00:00Z")),
        ] {
            let mut value = pod();
            let (parent, key) = pointer.rsplit_once('/').ok_or("pointer")?;
            value.pointer_mut(parent).ok_or("parent")?[key] = replacement;
            assert!(classify(&value, "worker.test").is_err(), "{pointer}");
        }
        for mode in 0..4 {
            let mut value = pod();
            match mode {
                0 => value["metadata"]["namespace"] = json!("kube-system"),
                1 => {
                    value["metadata"]["annotations"] = json!({"kubernetes.io/config.mirror":"hash"})
                }
                2 => value["metadata"]["ownerReferences"][0]["kind"] = json!("DaemonSet"),
                _ => value["status"]["phase"] = json!("Succeeded"),
            };
            assert!(classify(&value, "worker.test")?.retained.is_some());
        }
        assert!(classify(&pod(), "other").is_err());
        Ok(())
    }
    #[test]
    fn wire_payloads_use_server_identity_guards_and_never_force() -> Result<()> {
        let identity = Identity::from_object(&pod(), true)?;
        let eviction = eviction_body(&identity);
        assert_eq!(eviction["apiVersion"], "policy/v1");
        assert_eq!(eviction["deleteOptions"]["preconditions"]["uid"], "pod-uid");
        assert_eq!(
            eviction["deleteOptions"]["preconditions"]["resourceVersion"],
            "20"
        );
        assert!(eviction.get("delete_options").is_none());
        assert!(
            eviction["deleteOptions"]
                .get("gracePeriodSeconds")
                .is_none()
        );
        let patch = scheduling_patch(&Identity::from_object(&node(), false)?, true);
        assert_eq!(patch[0]["op"], "test");
        assert_eq!(patch[1]["path"], "/metadata/resourceVersion");
        assert_eq!(patch[2]["value"], true);
        Ok(())
    }
    #[test]
    fn refreshed_inventory_binds_all_members_and_evictable_versions() -> Result<()> {
        let target = classify(&pod(), "worker.test")?;
        let before = Inventory {
            pods: vec![target.clone()],
        };
        let mut changed = target.clone();
        changed.identity.version = "21".into();
        assert!(
            recheck_inventory(
                &before,
                &Inventory {
                    pods: vec![changed]
                }
            )
            .is_err()
        );
        assert!(recheck_inventory(&before, &Inventory { pods: vec![] }).is_err());
        let mut retained = target;
        retained.retained = Some("protected system Pod");
        retained.controller = None;
        let before = Inventory {
            pods: vec![retained.clone()],
        };
        retained.identity.version = "21".into();
        assert!(
            recheck_inventory(
                &before,
                &Inventory {
                    pods: vec![retained]
                }
            )
            .is_ok()
        );
        Ok(())
    }
    #[test]
    fn capability_and_expired_approval_never_issue_requests() -> Result<()> {
        runtime(async {
            let (client, calls) = client(vec![]);
            let denied = Gateway::for_test_client(client.clone(), "test");
            assert!(
                prepare(&denied, "worker.test", Request::Cordon)
                    .await
                    .is_err()
            );
            let gateway = Gateway::for_node_maintenance_test_client(client);
            let mut draft = review(Request::Cordon, vec![])?;
            draft.created = Instant::now() - Duration::from_secs(61);
            assert!(
                execute_at(&gateway, draft, "nodes/worker.test", &audit_dir()?)
                    .await
                    .is_err()
            );
            assert!(calls.lock().map_err(|_| "lock")?.is_empty());
            Ok(())
        })
    }
    #[test]
    fn complete_review_has_real_dry_run_and_no_pod_contents() -> Result<()> {
        runtime(async {
            let (client, calls) = client(vec![
                ("GET", "/api/v1/nodes/worker.test", 200, node()),
                ("GET", "/api/v1/pods", 200, list(vec![pod()])),
                (
                    "GET",
                    "/apis/apps/v1/namespaces/test/replicasets/app-rs",
                    200,
                    owner(),
                ),
                ("PATCH", "/api/v1/nodes/worker.test", 200, cordoned()),
            ]);
            let gateway = Gateway::for_node_maintenance_test_client(client);
            let draft = prepare(&gateway, "worker.test", Request::Drain { seconds: 120 }).await?;
            assert!(
                draft
                    .summary()
                    .contains("EVICT test/app-abc UID pod-uid RV 20")
            );
            assert!(draft.summary().contains("No forced deletion"));
            assert!(!draft.summary().contains("example.test/app"));
            assert_eq!(draft.confirmation(), "nodes/worker.test");
            assert!(
                calls.lock().map_err(|_| "lock")?[3]
                    .1
                    .contains("dryRun=All")
            );
            Ok(())
        })
    }
    #[test]
    fn cordon_has_durable_intent_and_guarded_patch() -> Result<()> {
        runtime(async {
            let (client, calls) = client(vec![
                ("GET", "/api/v1/nodes/worker.test", 200, node()),
                ("PATCH", "/api/v1/nodes/worker.test", 200, cordoned()),
            ]);
            let gateway = Gateway::for_node_maintenance_test_client(client);
            let dir = audit_dir()?;
            let report = execute_at(
                &gateway,
                review(Request::Cordon, vec![])?,
                "nodes/worker.test",
                &dir,
            )
            .await?;
            assert!(report.complete);
            assert_eq!(report.node_cordoned, Some(true));
            assert_eq!(
                calls.lock().map_err(|_| "lock")?[1].2[0]["value"],
                "node-uid"
            );
            let file = std::fs::read_dir(&dir)
                .map_err(|_| "dir")?
                .next()
                .ok_or("intent")?
                .map_err(|_| "entry")?
                .path();
            let audit = std::fs::read_to_string(file).map_err(|_| "read")?;
            assert!(audit.contains("\"stage\":\"intent\""));
            assert!(audit.contains("\"status\":\"succeeded\""));
            std::fs::remove_dir_all(dir).map_err(|_| "cleanup")?;
            Ok(())
        })
    }
    #[test]
    fn changed_node_blocks_every_mutation() -> Result<()> {
        runtime(async {
            let mut changed = node();
            changed["metadata"]["resourceVersion"] = json!("11");
            changed["metadata"]["labels"] = json!({"changed":"yes"});
            let (client, calls) = client(vec![("GET", "/api/v1/nodes/worker.test", 200, changed)]);
            let gateway = Gateway::for_node_maintenance_test_client(client);
            let dir = audit_dir()?;
            let report = execute_at(
                &gateway,
                review(Request::Cordon, vec![])?,
                "nodes/worker.test",
                &dir,
            )
            .await?;
            assert!(!report.complete);
            assert!(report.message.contains("metadata or spec changed"));
            assert_eq!(calls.lock().map_err(|_| "lock")?.len(), 1);
            std::fs::remove_dir_all(dir).map_err(|_| "cleanup")?;
            Ok(())
        })
    }
    fn drain_replies(eviction_status: u16) -> Replies {
        vec![
            ("GET", "/api/v1/nodes/worker.test", 200, node()),
            ("GET", "/api/v1/pods", 200, list(vec![pod()])),
            (
                "GET",
                "/apis/apps/v1/namespaces/test/replicasets/app-rs",
                200,
                owner(),
            ),
            ("PATCH", "/api/v1/nodes/worker.test", 200, cordoned()),
            ("GET", "/api/v1/nodes/worker.test", 200, cordoned()),
            ("GET", "/api/v1/namespaces/test/pods/app-abc", 200, pod()),
            (
                "GET",
                "/apis/apps/v1/namespaces/test/replicasets/app-rs",
                200,
                owner(),
            ),
            (
                "POST",
                "/api/v1/namespaces/test/pods/app-abc/eviction",
                eviction_status,
                json!({"apiVersion":"v1","kind":"Status","status":if eviction_status==200{"Success"}else{"Failure"},"code":eviction_status,"reason":"TooManyRequests","message":"suppressed-server-detail"}),
            ),
        ]
    }
    #[test]
    fn pdb_refusal_keeps_node_cordoned_and_never_deletes_or_retries() -> Result<()> {
        runtime(async {
            let (client, calls) = client(drain_replies(429));
            let gateway = Gateway::for_node_maintenance_test_client(client);
            let dir = audit_dir()?;
            let report = execute_at(
                &gateway,
                review(
                    Request::Drain { seconds: 10 },
                    vec![classify(&pod(), "worker.test")?],
                )?,
                "nodes/worker.test",
                &dir,
            )
            .await?;
            assert!(!report.complete);
            assert_eq!(report.node_cordoned, Some(true));
            assert!(report.message.contains("PodDisruptionBudget"));
            assert!(!report.message.contains("suppressed-server-detail"));
            assert_eq!(report.pending, vec!["test/app-abc"]);
            let calls = calls.lock().map_err(|_| "lock")?;
            assert_eq!(
                calls
                    .iter()
                    .filter(|(method, _, _)| method == "POST")
                    .count(),
                1
            );
            assert!(!calls.iter().any(|(method, _, _)| method == "DELETE"));
            assert_eq!(
                calls[7].2["deleteOptions"]["preconditions"]["resourceVersion"],
                "20"
            );
            std::fs::remove_dir_all(dir).map_err(|_| "cleanup")?;
            Ok(())
        })
    }
    #[test]
    fn successful_drain_waits_for_old_uid_and_checks_final_inventory() -> Result<()> {
        runtime(async {
            let mut replies = drain_replies(200);
            replies.extend([("GET","/api/v1/namespaces/test/pods/app-abc",404,json!({"apiVersion":"v1","kind":"Status","code":404,"reason":"NotFound","message":"gone","status":"Failure"})),("GET","/api/v1/nodes/worker.test",200,cordoned()),("GET","/api/v1/pods",200,list(vec![]))]);
            let (client, _) = client(replies);
            let gateway = Gateway::for_node_maintenance_test_client(client);
            let dir = audit_dir()?;
            let report = execute_at(
                &gateway,
                review(
                    Request::Drain { seconds: 10 },
                    vec![classify(&pod(), "worker.test")?],
                )?,
                "nodes/worker.test",
                &dir,
            )
            .await?;
            assert!(report.complete);
            assert_eq!(report.evicted, vec!["test/app-abc"]);
            assert!(report.pending.is_empty());
            assert_eq!(std::fs::read_dir(&dir).map_err(|_| "dir")?.count(), 2);
            std::fs::remove_dir_all(dir).map_err(|_| "cleanup")?;
            Ok(())
        })
    }
    #[test]
    fn dry_run_rejects_unreviewed_admission_scheduling_changes() -> Result<()> {
        let mut after = cordoned();
        after["spec"]["taints"] = json!([{"key":"injected","effect":"NoSchedule"}]);
        assert!(check_scheduling_response(&node(), &after, true).is_err());
        assert!(check_scheduling_response(&node(), &cordoned(), true).is_ok());
        Ok(())
    }
    #[test]
    fn node_status_only_refresh_uses_fresh_rv_but_other_content_stays_bound() -> Result<()> {
        runtime(async {
            let mut heartbeat = node();
            heartbeat["metadata"]["resourceVersion"] = json!("12");
            heartbeat["status"] = json!({"conditions":[{"type":"Ready","status":"True","lastHeartbeatTime":"2026-10-23T00:00:00Z"}]});
            heartbeat["metadata"]["managedFields"] = json!([{"manager":"kubelet"}]);
            assert_eq!(node_content(&node()), node_content(&heartbeat));
            let mut changed = heartbeat.clone();
            changed["spec"]["taints"] = json!([]);
            assert_ne!(node_content(&node()), node_content(&changed));
            changed = heartbeat.clone();
            changed["metadata"]["annotations"] = json!({"change":"yes"});
            assert_ne!(node_content(&node()), node_content(&changed));
            let (client, calls) = client(vec![
                ("GET", "/api/v1/nodes/worker.test", 200, heartbeat),
                ("PATCH", "/api/v1/nodes/worker.test", 200, cordoned()),
            ]);
            let gateway = Gateway::for_node_maintenance_test_client(client);
            let dir = audit_dir()?;
            let report = execute_at(
                &gateway,
                review(Request::Cordon, vec![])?,
                "nodes/worker.test",
                &dir,
            )
            .await?;
            assert!(report.complete);
            assert_eq!(calls.lock().map_err(|_| "lock")?[1].2[1]["value"], "12");
            std::fs::remove_dir_all(dir).map_err(|_| "cleanup")?;
            Ok(())
        })
    }
    #[test]
    fn uncordon_accepts_api_omission_of_false_unschedulable() -> Result<()> {
        check_scheduling_response(&cordoned(), &node(), false)?;
        assert!(check_scheduling_response(&cordoned(), &node(), true).is_err());
        Ok(())
    }
    #[test]
    fn accepted_eviction_timeout_keeps_pending_audit_and_cordon() -> Result<()> {
        runtime(async {
            let mut replies = drain_replies(200);
            for _ in 0..8 {
                replies.push(("GET", "/api/v1/namespaces/test/pods/app-abc", 200, pod()));
            }
            let (client, _) = client(replies);
            let gateway = Gateway::for_node_maintenance_test_client(client);
            let dir = audit_dir()?;
            let report = execute_at(
                &gateway,
                review(
                    Request::Drain { seconds: 1 },
                    vec![classify(&pod(), "worker.test")?],
                )?,
                "nodes/worker.test",
                &dir,
            )
            .await?;
            assert!(!report.complete);
            assert_eq!(report.node_cordoned, Some(true));
            assert_eq!(report.pending, vec!["test/app-abc"]);
            assert!(report.message.contains("deadline expired"));
            let mut accepted = 0;
            for entry in std::fs::read_dir(&dir).map_err(|_| "dir")? {
                let text = std::fs::read_to_string(entry.map_err(|_| "entry")?.path())
                    .map_err(|_| "read")?;
                if text.contains("node-drain-evict") {
                    assert!(text.contains("eviction_accepted"));
                    assert!(!text.contains("succeeded"));
                    accepted += 1;
                }
            }
            assert_eq!(accepted, 1);
            std::fs::remove_dir_all(dir).map_err(|_| "cleanup")?;
            Ok(())
        })
    }
    #[test]
    fn newly_bound_pod_after_eviction_prevents_complete_drain() -> Result<()> {
        runtime(async {
            let mut arrived = pod();
            arrived["metadata"]["uid"] = json!("new-uid");
            arrived["metadata"]["resourceVersion"] = json!("22");
            let mut replies = drain_replies(200);
            replies.extend([
                (
                    "GET",
                    "/api/v1/namespaces/test/pods/app-abc",
                    200,
                    arrived.clone(),
                ),
                ("GET", "/api/v1/nodes/worker.test", 200, cordoned()),
                ("GET", "/api/v1/pods", 200, list(vec![arrived])),
            ]);
            let (client, calls) = client(replies);
            let gateway = Gateway::for_node_maintenance_test_client(client);
            let dir = audit_dir()?;
            let report = execute_at(
                &gateway,
                review(
                    Request::Drain { seconds: 10 },
                    vec![classify(&pod(), "worker.test")?],
                )?,
                "nodes/worker.test",
                &dir,
            )
            .await?;
            assert!(!report.complete);
            assert!(report.message.contains("new or remaining eligible Pods"));
            assert_eq!(report.evicted, vec!["test/app-abc"]);
            assert_eq!(report.pending, vec!["test/app-abc"]);
            assert_eq!(
                calls
                    .lock()
                    .map_err(|_| "lock")?
                    .iter()
                    .filter(|(method, _, _)| method == "POST")
                    .count(),
                1
            );
            std::fs::remove_dir_all(dir).map_err(|_| "cleanup")?;
            Ok(())
        })
    }
}
