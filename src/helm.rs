//! Native Helm v3/v4 release storage reads with explicit, bounded projections.
//! Public views exclude raw storage, chart files, notes and rendered manifests.
//! Reviewed lifecycle receives only a crate-private, identity-bound hook projection.
use crate::{Result, gateway::Gateway, safety};
use base64::{Engine, engine::general_purpose::STANDARD};
use flate2::read::MultiGzDecoder;
use k8s_openapi::{
    api::core::v1::{ConfigMap, Secret},
    apimachinery::pkg::apis::meta::v1::{ListMeta, ObjectMeta},
};
use kube::{Api, Client, api::ListParams};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    time::Duration,
};

const MAX_ENCODED: usize = 1024 * 1024;
const MAX_DECODED: usize = 8 * 1024 * 1024;
const MAX_RECORDS: usize = 1000;
const MAX_PAGES: usize = 250;
const MAX_RESOURCES: usize = 2000;
const READ_BUDGET: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Storage {
    Secret,
    ConfigMap,
}
impl Storage {
    pub fn name(self) -> &'static str {
        match self {
            Self::Secret => "Secret",
            Self::ConfigMap => "ConfigMap",
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Release {
    pub namespace: String,
    pub name: String,
    pub revision: u32,
    pub status: String,
    pub chart_name: String,
    pub chart_version: String,
    pub app_version: String,
    pub updated: String,
    pub storage: Storage,
    pub uid: String,
    pub resource_version: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub releases: Vec<Release>,
    pub truncated: bool,
    pub issues: Vec<String>,
    pub source: String,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ResourceRef {
    pub api_version: String,
    pub kind: String,
    pub namespace: String,
    pub name: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Detail {
    pub release: Release,
    pub resources: Vec<ResourceRef>,
    pub resources_truncated: bool,
    pub issues: Vec<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Values {
    pub release: Release,
    pub values: Value,
    pub notice: String,
}
impl Snapshot {
    pub fn text(&self) -> String {
        let mut out = format!(
            "HELM | {} | release revisions by storage driver\nNAMESPACE / NAME                     REV STATUS             CHART                         APP VERSION  STORAGE\n",
            self.source
        );
        for release in &self.releases {
            out.push_str(&format!(
                "{:<36} {:>3} {:<18} {:<29} {:<12} {}\n",
                format!("{}/{}", release.namespace, release.name),
                release.revision,
                release.status,
                format!("{}-{}", release.chart_name, release.chart_version),
                release.app_version,
                release.storage.name()
            ));
        }
        if self.truncated {
            out.push_str("INCOMPLETE: history or latest revisions may be missing.\n");
        }
        for issue in &self.issues {
            out.push_str(&format!("{issue}\n"));
        }
        out
    }
}
impl Detail {
    pub fn text(&self) -> String {
        let r = &self.release;
        let mut out = format!(
            "HELM {}/{} | revision {} | {} storage\nStatus: {}\nChart: {} {} | app {}\nUpdated: {}\nUID: {}\nResource version: {}\n\nRendered resource references (identities only):\n",
            r.namespace,
            r.name,
            r.revision,
            r.storage.name(),
            r.status,
            r.chart_name,
            r.chart_version,
            r.app_version,
            r.updated,
            r.uid,
            r.resource_version
        );
        for resource in &self.resources {
            out.push_str(&format!(
                "{} {} {}/{}\n",
                resource.api_version,
                resource.kind,
                if resource.namespace.is_empty() {
                    "[cluster scope or namespace omitted]"
                } else {
                    &resource.namespace
                },
                resource.name
            ));
        }
        if self.resources_truncated {
            out.push_str("INCOMPLETE resource references.\n");
        }
        for issue in &self.issues {
            out.push_str(&format!("{issue}\n"));
        }
        out.push_str("\nValues require an explicit selected-revision values request. Notes, chart files and raw manifests are excluded.\n");
        out
    }
}
impl Values {
    pub fn text(&self) -> Result<String> {
        Ok(format!(
            "HELM VALUES {}/{} | revision {}\n{}\n\n{}",
            self.release.namespace,
            self.release.name,
            self.release.revision,
            self.notice,
            crate::inspect::yaml(&self.values)?
        ))
    }
}

/// The gateway constructs this narrow reader after its Helm capability check.
pub(crate) struct Reader {
    secrets: Api<Secret>,
    configmaps: Api<ConfigMap>,
    namespace: String,
}
impl Reader {
    pub(crate) fn new(client: Client, namespace: &str) -> Result<Self> {
        validate_namespace(namespace, true)?;
        Ok(Self {
            secrets: if namespace == "*" {
                Api::all(client.clone())
            } else {
                Api::namespaced(client.clone(), namespace)
            },
            configmaps: if namespace == "*" {
                Api::all(client)
            } else {
                Api::namespaced(client, namespace)
            },
            namespace: namespace.into(),
        })
    }
    async fn page(&self, storage: Storage, params: &ListParams) -> Result<(ListMeta, Vec<Stored>)> {
        match storage {
            Storage::Secret => self.secrets.list(params).await.map(|list| {
                (
                    list.metadata,
                    list.items.into_iter().map(Stored::Secret).collect(),
                )
            }),
            Storage::ConfigMap => self.configmaps.list(params).await.map(|list| {
                (
                    list.metadata,
                    list.items.into_iter().map(Stored::ConfigMap).collect(),
                )
            }),
        }
        .map_err(storage_error)
    }
    async fn get(&self, storage: Storage, name: &str) -> Result<Stored> {
        match storage {
            Storage::Secret => self.secrets.get(name).await.map(Stored::Secret),
            Storage::ConfigMap => self.configmaps.get(name).await.map(Stored::ConfigMap),
        }
        .map_err(storage_error)
    }
    async fn collect(
        &self,
        storage: Storage,
        name: Option<&str>,
        deadline: tokio::time::Instant,
    ) -> Snapshot {
        let mut snapshot = Snapshot {
            releases: Vec::new(),
            truncated: false,
            issues: Vec::new(),
            source: "cluster".into(),
        };
        let mut budget = DecodeBudget::default();
        let mut continuation = None;
        let mut seen_tokens = BTreeSet::new();
        let mut seen_objects = BTreeSet::new();
        let mut version = None;
        let mut records = 0usize;
        let selector = if let Some(name) = name {
            format!("owner=helm,name={name}")
        } else {
            "owner=helm".into()
        };
        for _ in 0..MAX_PAGES {
            let params = ListParams {
                limit: Some(4),
                label_selector: Some(selector.clone()),
                continue_token: continuation,
                ..Default::default()
            };
            let result = tokio::time::timeout_at(deadline, self.page(storage, &params)).await;
            let (metadata, items) = match result {
                Ok(Ok(page)) => page,
                result => {
                    snapshot.truncated = true;
                    snapshot.issues.push(format!(
                        "{} storage unavailable: {}",
                        storage.name(),
                        match result {
                            Ok(Err(error)) => error,
                            _ => "read deadline exceeded".into(),
                        }
                    ));
                    return snapshot;
                }
            };
            let current_version = metadata
                .resource_version
                .filter(|value| bounded(value, 253, false));
            if current_version.is_none()
                || version
                    .as_ref()
                    .is_some_and(|version| Some(version) != current_version.as_ref())
            {
                snapshot.truncated = true;
                snapshot.issues.push(format!(
                    "{} storage pagination changed or omitted resourceVersion",
                    storage.name()
                ));
                return snapshot;
            }
            version = current_version;
            for object in items {
                records += 1;
                if records > MAX_RECORDS || tokio::time::Instant::now() >= deadline {
                    snapshot.truncated = true;
                    snapshot.issues.push(format!(
                        "{} storage record/read limit reached",
                        storage.name()
                    ));
                    return snapshot;
                }
                let result = (|| {
                    let identity = object.identity(storage, &self.namespace)?;
                    if name.is_some_and(|name| identity.name != name) {
                        return Err("release label escaped requested history".into());
                    }
                    if !seen_objects.insert((
                        identity.namespace.clone(),
                        identity.name.clone(),
                        identity.revision,
                    )) {
                        return Err("duplicate storage revision".into());
                    }
                    let bytes = decode(object.payload()?, &mut budget)?;
                    project(&bytes, &identity)
                })();
                match result {
                    Ok(release) => snapshot.releases.push(release),
                    Err(error) => {
                        snapshot.truncated = true;
                        if snapshot.issues.len() < 32 {
                            snapshot.issues.push(format!(
                                "{} storage record excluded: {error}",
                                storage.name()
                            ));
                        }
                    }
                }
                if budget.exceeded {
                    return snapshot;
                }
            }
            continuation = metadata.continue_.filter(|value| !value.is_empty());
            if continuation.is_none() {
                return snapshot;
            }
            if let Some(token) = &continuation
                && (!bounded(token, 4096, false) || !seen_tokens.insert(token.clone()))
            {
                snapshot.truncated = true;
                snapshot.issues.push(format!(
                    "{} storage repeated/invalid continuation token",
                    storage.name()
                ));
                return snapshot;
            }
        }
        snapshot.truncated = true;
        snapshot.issues.push(format!(
            "{} storage pagination limit reached",
            storage.name()
        ));
        snapshot
    }
}
enum Stored {
    Secret(Secret),
    ConfigMap(ConfigMap),
}
impl Stored {
    fn metadata(&self) -> &ObjectMeta {
        match self {
            Self::Secret(object) => &object.metadata,
            Self::ConfigMap(object) => &object.metadata,
        }
    }
    fn payload(&self) -> Result<&[u8]> {
        let bytes = match self {
            Self::Secret(object) => object
                .data
                .as_ref()
                .and_then(|data| data.get("release"))
                .map(|bytes| bytes.0.as_slice()),
            Self::ConfigMap(object) => object
                .data
                .as_ref()
                .and_then(|data| data.get("release"))
                .map(|text| text.as_bytes()),
        }
        .ok_or("Helm release payload is missing")?;
        if bytes.is_empty() || bytes.len() > MAX_ENCODED {
            return Err("Helm encoded release exceeds 1 MiB".into());
        }
        Ok(bytes)
    }
    fn identity(&self, storage: Storage, namespace: &str) -> Result<Identity> {
        if matches!(self,Self::Secret(object) if object.type_.as_deref()!=Some("helm.sh/release.v1"))
        {
            return Err("storage Secret has a non-Helm type".into());
        }
        let metadata = self.metadata();
        let labels = metadata
            .labels
            .as_ref()
            .ok_or("Helm ownership labels are missing")?;
        if labels.get("owner").map(String::as_str) != Some("helm") {
            return Err("storage owner is not Helm".into());
        }
        let name = labels
            .get("name")
            .filter(|name| valid_name(name))
            .ok_or("invalid Helm release name")?;
        let revision_text = labels
            .get("version")
            .ok_or("Helm revision label is missing")?;
        let revision = revision_text
            .parse::<u32>()
            .ok()
            .filter(|number| *number > 0 && number.to_string() == *revision_text)
            .ok_or("invalid Helm revision label")?;
        let key = storage_key(name, revision);
        if metadata.name.as_deref() != Some(&key) {
            return Err("Helm storage name does not match release/revision labels".into());
        }
        let actual_namespace = metadata
            .namespace
            .as_deref()
            .ok_or("Helm storage namespace is missing")?;
        validate_namespace(actual_namespace, false)?;
        if namespace != "*" && actual_namespace != namespace {
            return Err("Helm storage escaped namespace scope".into());
        }
        let uid = metadata
            .uid
            .as_deref()
            .filter(|value| bounded(value, 253, false))
            .ok_or("Helm storage UID is missing or invalid")?;
        let resource_version = metadata
            .resource_version
            .as_deref()
            .filter(|value| bounded(value, 253, false))
            .ok_or("Helm storage resourceVersion is missing or invalid")?;
        let status = labels
            .get("status")
            .filter(|value| valid_status(value))
            .ok_or("invalid Helm status label")?;
        Ok(Identity {
            name: name.clone(),
            namespace: actual_namespace.into(),
            revision,
            status: status.clone(),
            storage,
            uid: uid.into(),
            resource_version: resource_version.into(),
        })
    }
}
struct Identity {
    name: String,
    namespace: String,
    revision: u32,
    status: String,
    storage: Storage,
    uid: String,
    resource_version: String,
}
#[derive(Default)]
struct DecodeBudget {
    encoded: usize,
    decoded: usize,
    exceeded: bool,
}
fn decode(encoded: &[u8], budget: &mut DecodeBudget) -> Result<Vec<u8>> {
    budget.encoded = budget.encoded.saturating_add(encoded.len());
    if encoded.len() > MAX_ENCODED || budget.encoded > 16 * 1024 * 1024 {
        budget.exceeded = true;
        return Err("encoded release read budget exceeded".into());
    }
    let compressed = STANDARD
        .decode(encoded)
        .map_err(|_| "invalid Helm release encoding")?;
    let bytes = if compressed.starts_with(&[0x1f, 0x8b, 0x08]) {
        let mut bytes = Vec::new();
        MultiGzDecoder::new(compressed.as_slice())
            .take((MAX_DECODED + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| "invalid Helm gzip payload")?;
        bytes
    } else {
        compressed
    };
    budget.decoded = budget.decoded.saturating_add(bytes.len());
    if budget.decoded > 64 * 1024 * 1024 {
        budget.exceeded = true;
        return Err("decoded release read budget exceeded".into());
    }
    if bytes.len() > MAX_DECODED {
        return Err("decoded Helm release exceeds 8 MiB".into());
    }
    Ok(bytes)
}
#[derive(Deserialize)]
struct WireRelease {
    name: String,
    namespace: String,
    version: u32,
    info: WireInfo,
    chart: WireChart,
}
#[derive(Deserialize)]
struct WireInfo {
    status: String,
    #[serde(default)]
    last_deployed: String,
}
#[derive(Deserialize)]
struct WireChart {
    metadata: WireChartMetadata,
}
#[derive(Deserialize)]
struct WireChartMetadata {
    name: String,
    version: String,
    #[serde(default, rename = "appVersion")]
    app_version: String,
}
fn project(bytes: &[u8], identity: &Identity) -> Result<Release> {
    let release: WireRelease =
        serde_json::from_slice(bytes).map_err(|_| "invalid Helm release JSON")?;
    if release.name != identity.name
        || release.namespace != identity.namespace
        || release.version != identity.revision
        || release.info.status != identity.status
    {
        return Err("Helm release payload does not match storage identity/status".into());
    }
    for (value, max, empty) in [
        (&release.chart.metadata.name, 128, false),
        (&release.chart.metadata.version, 128, false),
        (&release.chart.metadata.app_version, 128, true),
        (&release.info.last_deployed, 64, true),
    ] {
        if !bounded(value, max, empty) {
            return Err("Helm chart/status metadata exceeds safe display limits".into());
        }
    }
    Ok(Release {
        namespace: identity.namespace.clone(),
        name: identity.name.clone(),
        revision: identity.revision,
        status: identity.status.clone(),
        chart_name: release.chart.metadata.name,
        chart_version: release.chart.metadata.version,
        app_version: release.chart.metadata.app_version,
        updated: release.info.last_deployed,
        storage: identity.storage,
        uid: identity.uid.clone(),
        resource_version: identity.resource_version.clone(),
    })
}
fn bounded(value: &str, max: usize, empty: bool) -> bool {
    (empty || !value.is_empty()) && value.len() <= max && safety::safe_text(value, max) == value
}
fn validate_namespace(namespace: &str, all: bool) -> Result<()> {
    if (all && namespace == "*") || safety::valid_id(namespace) {
        Ok(())
    } else {
        Err("Helm needs a concrete namespace or * for list".into())
    }
}
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 53 && name.split('.').all(safety::valid_id)
}
pub fn valid_release_name(name: &str) -> bool {
    valid_name(name)
}
fn valid_status(status: &str) -> bool {
    matches!(
        status,
        "unknown"
            | "deployed"
            | "uninstalled"
            | "superseded"
            | "failed"
            | "uninstalling"
            | "pending-install"
            | "pending-upgrade"
            | "pending-rollback"
    )
}
fn storage_key(name: &str, revision: u32) -> String {
    format!("sh.helm.release.v1.{name}.v{revision}")
}
fn storage_error(error: kube::Error) -> String {
    match error {
        kube::Error::Api(status) => {
            format!("Kubernetes API HTTP {}; server body withheld", status.code)
        }
        _ => "Kubernetes Helm storage transport/response failure".into(),
    }
}

pub async fn list(gateway: &Gateway, namespace: &str) -> Result<Snapshot> {
    list_with_storage(gateway, namespace, None).await
}
pub async fn list_with_storage(
    gateway: &Gateway,
    namespace: &str,
    storage: Option<Storage>,
) -> Result<Snapshot> {
    validate_namespace(namespace, true)?;
    if gateway.is_demo() {
        let mut snapshot = demo(namespace);
        if let Some(storage) = storage {
            snapshot
                .releases
                .retain(|release| release.storage == storage);
        }
        return Ok(snapshot);
    }
    let reader = gateway.helm_reader(namespace)?;
    let deadline = tokio::time::Instant::now() + READ_BUDGET;
    if let Some(storage) = storage {
        let mut snapshot = reader.collect(storage, None, deadline).await;
        latest_revisions(&mut snapshot);
        return Ok(snapshot);
    }
    let (mut secrets, configmaps) = futures_util::future::join(
        reader.collect(Storage::Secret, None, deadline),
        reader.collect(Storage::ConfigMap, None, deadline),
    )
    .await;
    secrets.truncated |= configmaps.truncated;
    secrets.issues.extend(configmaps.issues);
    secrets.releases.extend(configmaps.releases);
    latest_revisions(&mut secrets);
    Ok(secrets)
}
fn latest_revisions(snapshot: &mut Snapshot) {
    let mut latest = BTreeMap::<(String, String, Storage), Release>::new();
    for release in std::mem::take(&mut snapshot.releases) {
        let key = (
            release.namespace.clone(),
            release.name.clone(),
            release.storage,
        );
        if latest
            .get(&key)
            .is_none_or(|previous| previous.revision < release.revision)
        {
            latest.insert(key, release);
        }
    }
    snapshot.releases = latest.into_values().collect();
}
pub fn select_release(
    snapshot: &Snapshot,
    name: &str,
    storage: Option<Storage>,
) -> Result<Release> {
    if !valid_name(name) {
        return Err("invalid Helm release name; use a DNS name of at most 53 bytes".into());
    }
    if snapshot.truncated || !snapshot.issues.is_empty() {
        return Err("Helm inventory is incomplete; narrow namespace and --helm-storage, then refresh before selecting a release by name".into());
    }
    let mut matches = BTreeMap::<(&str, Storage), &Release>::new();
    for release in snapshot.releases.iter().filter(|release| {
        release.name == name && storage.is_none_or(|storage| release.storage == storage)
    }) {
        let key = (release.namespace.as_str(), release.storage);
        if matches
            .get(&key)
            .is_none_or(|old| old.revision < release.revision)
        {
            matches.insert(key, release);
        }
    }
    if matches.len() > 1 {
        return Err("Helm release name is ambiguous; select a concrete namespace and --helm-storage secret|configmap".into());
    }
    matches
        .into_values()
        .next()
        .cloned()
        .ok_or("Helm release not found in the complete selected inventory".into())
}
pub async fn history(gateway: &Gateway, release: &Release) -> Result<Snapshot> {
    validate_selection(release)?;
    if gateway.is_demo() {
        let mut snapshot = demo_history(&release.namespace);
        snapshot
            .releases
            .retain(|item| item.name == release.name && item.storage == release.storage);
        return Ok(snapshot);
    }
    let reader = gateway.helm_reader(&release.namespace)?;
    let mut snapshot = reader
        .collect(
            release.storage,
            Some(&release.name),
            tokio::time::Instant::now() + READ_BUDGET,
        )
        .await;
    snapshot
        .releases
        .sort_by_key(|item| std::cmp::Reverse(item.revision));
    Ok(snapshot)
}
fn validate_selection(release: &Release) -> Result<()> {
    validate_namespace(&release.namespace, false)?;
    if !valid_name(&release.name) || release.revision == 0 || !bounded(&release.uid, 253, false) {
        return Err("invalid selected Helm release identity".into());
    }
    Ok(())
}
async fn selected(gateway: &Gateway, release: &Release) -> Result<(Release, Vec<u8>)> {
    validate_selection(release)?;
    let reader = gateway.helm_reader(&release.namespace)?;
    let object = tokio::time::timeout(
        READ_BUDGET,
        reader.get(
            release.storage,
            &storage_key(&release.name, release.revision),
        ),
    )
    .await
    .map_err(|_| "Helm revision read deadline exceeded")??;
    selected_object(object, release)
}
fn selected_object(object: Stored, release: &Release) -> Result<(Release, Vec<u8>)> {
    let identity = object.identity(release.storage, &release.namespace)?;
    if identity.uid != release.uid
        || identity.name != release.name
        || identity.revision != release.revision
    {
        return Err("selected Helm storage revision was replaced; refresh and select again".into());
    }
    let bytes = decode(object.payload()?, &mut DecodeBudget::default())?;
    Ok((project(&bytes, &identity)?, bytes))
}
/// A lifecycle-only read of the exact reviewed storage revision. Never expose
/// the returned manifests through the public release browser or diagnostics.
pub(crate) async fn lifecycle_hooks(gateway: &Gateway, release: &Release) -> Result<Value> {
    let (current, bytes) = selected(gateway, release).await?;
    if current.resource_version != release.resource_version || current.status != release.status {
        return Err("stored Helm hook revision changed; refresh and prepare again".into());
    }
    hook_projection(&bytes)
}
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct TestRun {
    pub name: String,
    pub kind: String,
    pub started: String,
    pub completed: String,
    pub phase: String,
}
pub(crate) async fn lifecycle_test_results(
    gateway: &Gateway,
    release: &Release,
) -> Result<Vec<TestRun>> {
    let (current, bytes) = selected(gateway, release).await?;
    if current.resource_version != release.resource_version {
        return Err("stored test results changed during read".into());
    }
    let _ = hook_projection(&bytes)?;
    let raw: Value = serde_json::from_slice(&bytes).map_err(|_| "invalid stored test results")?;
    let mut results = Vec::new();
    for hook in raw["hooks"].as_array().into_iter().flatten() {
        if !hook["events"]
            .as_array()
            .is_some_and(|events| events.iter().any(|event| event == "test"))
        {
            continue;
        }
        let field = |key: &str| -> Result<String> {
            hook[key]
                .as_str()
                .filter(|value| !value.is_empty() && value.len() <= 253)
                .map(str::to_owned)
                .ok_or_else(|| "invalid stored test identity".into())
        };
        let run = &hook["last_run"];
        if !run.is_null() && !run.is_object() {
            return Err("invalid stored hook execution result".into());
        }
        if run.as_object().is_some_and(|map| {
            map.keys()
                .any(|key| !["started_at", "completed_at", "phase"].contains(&key.as_str()))
        }) {
            return Err("noncanonical stored test execution result".into());
        }
        let started = run["started_at"]
            .as_str()
            .filter(|value| value.len() <= 80)
            .unwrap_or("")
            .to_owned();
        let completed = run["completed_at"]
            .as_str()
            .filter(|value| value.len() <= 80)
            .unwrap_or("")
            .to_owned();
        let phase = run["phase"].as_str().unwrap_or("");
        if !["", "Unknown", "Running", "Succeeded", "Failed"].contains(&phase) {
            return Err("unknown stored test phase".into());
        }
        results.push(TestRun {
            name: field("name")?,
            kind: field("kind")?,
            started,
            completed,
            phase: phase.into(),
        });
    }
    Ok(results)
}
fn hook_projection(bytes: &[u8]) -> Result<Value> {
    let release: Value = serde_json::from_slice(bytes)
        .map_err(|_| "invalid stored Helm hook envelope; contents suppressed")?;
    let fields = release
        .as_object()
        .ok_or("invalid stored Helm hook envelope")?;
    if fields
        .keys()
        .any(|key| key.eq_ignore_ascii_case("hooks") && key != "hooks")
    {
        return Err(
            "noncanonical stored Helm hook field; execution semantics are ambiguous".into(),
        );
    }
    let Some(hooks) = fields.get("hooks").filter(|value| !value.is_null()) else {
        return Ok(Value::Array(vec![]));
    };
    let hooks = hooks
        .as_array()
        .filter(|hooks| hooks.len() <= crate::helm_extensions::MAX_HOOKS)
        .ok_or("stored Helm hooks must be a bounded array")?;
    let mut projected = Vec::with_capacity(hooks.len());
    for hook in hooks {
        let hook = hook.as_object().ok_or("invalid stored Helm hook record")?;
        // Go's JSON decoder also accepts case-insensitive field aliases. Reject
        // aliases/unknown fields rather than projecting different semantics.
        if hook.keys().any(|key| {
            ![
                "name",
                "kind",
                "path",
                "manifest",
                "events",
                "last_run",
                "weight",
                "delete_policies",
                "output_log_policies",
            ]
            .contains(&key.as_str())
        }) {
            return Err("stored Helm hook has unsupported/noncanonical execution fields".into());
        }
        let mut result = serde_json::Map::new();
        for key in ["name", "kind", "manifest"] {
            result.insert(key.into(), hook.get(key).cloned().unwrap_or(Value::Null));
        }
        result.insert(
            "weight".into(),
            hook.get("weight").cloned().unwrap_or(json!(0)),
        );
        for key in ["events", "delete_policies", "output_log_policies"] {
            result.insert(key.into(), hook.get(key).cloned().unwrap_or(json!([])));
        }
        projected.push(Value::Object(result));
    }
    Ok(Value::Array(projected))
}
pub async fn detail(gateway: &Gateway, release: &Release) -> Result<Detail> {
    if gateway.is_demo() {
        let release = demo_selected(release)?;
        return Ok(Detail {
            resources: vec![ResourceRef {
                api_version: "apps/v1".into(),
                kind: "Deployment".into(),
                namespace: release.namespace.clone(),
                name: "api".into(),
            }],
            release,
            resources_truncated: false,
            issues: Vec::new(),
        });
    }
    let (release, bytes) = selected(gateway, release).await?;
    #[derive(Deserialize)]
    struct Manifest {
        #[serde(default)]
        manifest: String,
    }
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|_| "invalid Helm manifest envelope")?;
    let (mut resources, resources_truncated, issues) = resource_references(&manifest.manifest);
    for resource in &mut resources {
        let namespaced = gateway.kinds.iter().any(|kind| {
            kind.kind == resource.kind
                && (if kind.group.is_empty() {
                    kind.version.clone()
                } else {
                    format!("{}/{}", kind.group, kind.version)
                }) == resource.api_version
                && kind.namespaced
        }) || (resource.api_version == "v1" && resource.kind == "Secret");
        if resource.namespace.is_empty() && namespaced {
            resource.namespace = release.namespace.clone();
        }
    }
    Ok(Detail {
        release,
        resources,
        resources_truncated,
        issues,
    })
}
pub async fn values(gateway: &Gateway, release: &Release) -> Result<Values> {
    const NOTICE: &str = "Stored release overrides only; known sensitive keys are redacted. Custom values may still contain confidential data. Chart defaults/files/notes are excluded.";
    if gateway.is_demo() {
        let release = demo_selected(release)?;
        return Ok(Values {
            values: serde_json::json!({"replicaCount":release.revision,"image":{"tag":"v1"},"password":"[REDACTED]"}),
            release,
            notice: NOTICE.into(),
        });
    }
    let (release, bytes) = selected(gateway, release).await?;
    #[derive(Deserialize)]
    struct Config {
        #[serde(default)]
        config: Value,
    }
    let mut config: Config =
        serde_json::from_slice(&bytes).map_err(|_| "invalid Helm values envelope")?;
    redact_values(&mut config.config, 0, &mut 0)?;
    safety::redact(&mut config.config);
    // Renderer supplies a second independent depth/node/serialized-size preflight.
    let rendered = crate::inspect::yaml(&config.config)?;
    if rendered.len() > 256 * 1024 {
        return Err(
            "redacted Helm values exceed 256 KiB; narrow the chart values outside KBorg".into(),
        );
    }
    Ok(Values {
        release,
        values: config.config,
        notice: NOTICE.into(),
    })
}
pub(crate) fn redact_values(value: &mut Value, depth: usize, nodes: &mut usize) -> Result<()> {
    redact_values_at(value, depth, nodes, false)
}
fn redact_values_at(value: &mut Value, depth: usize, nodes: &mut usize, tls: bool) -> Result<()> {
    *nodes += 1;
    if depth > 64 || *nodes > 30_000 {
        return Err("Helm values exceed 64 levels or 30000 values".into());
    }
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                let lower = key.to_ascii_lowercase();
                if [
                    "secret",
                    "password",
                    "passwd",
                    "passphrase",
                    "apikey",
                    "api_key",
                    "api-key",
                    "token",
                    "credential",
                    "privatekey",
                    "private_key",
                    "accesskey",
                    "access_key",
                    "authorization",
                    "connectionstring",
                    "connection_string",
                ]
                .iter()
                .any(|needle| lower.contains(needle))
                    || lower == "tls.key"
                    || (tls && lower == "key")
                {
                    *value = Value::String("[REDACTED]".into());
                } else {
                    redact_values_at(value, depth + 1, nodes, lower == "tls")?;
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_values_at(item, depth + 1, nodes, tls)?;
            }
        }
        Value::String(text) => {
            *text = if text.contains("-----BEGIN ") && text.contains("PRIVATE KEY-----") {
                "[REDACTED]".into()
            } else {
                safety::safe_lines(text, 16384)
            }
        }
        _ => {}
    }
    Ok(())
}
#[derive(Deserialize)]
struct ManifestObject {
    #[serde(rename = "apiVersion")]
    api_version: String,
    kind: String,
    #[serde(default)]
    metadata: ManifestMetadata,
    #[serde(default)]
    items: Option<Vec<ManifestObject>>,
}
#[derive(Default, Deserialize)]
struct ManifestMetadata {
    #[serde(default)]
    name: String,
    #[serde(default)]
    namespace: String,
}
fn resource_references(manifest: &str) -> (Vec<ResourceRef>, bool, Vec<String>) {
    if manifest.is_empty() {
        return (Vec::new(), false, Vec::new());
    }
    let options = serde_saphyr::options! {budget:serde_saphyr::budget!{max_events:250_000,max_aliases:32,max_anchors:32,max_depth:64,max_inclusion_depth:0,max_documents:MAX_RESOURCES,max_nodes:100_000,max_total_scalar_bytes:MAX_DECODED,max_merge_keys:0},alias_limits:serde_saphyr::alias_limits!{max_replay_stack_depth:16,max_alias_expansions_per_anchor:8},merge_keys:serde_saphyr::MergeKeyPolicy::Error,duplicate_keys:serde_saphyr::DuplicateKeyPolicy::Error,with_snippet:false};
    let documents:Vec<ManifestObject>=match serde_saphyr::from_multiple_with_options(manifest,options){Ok(documents)=>documents,Err(_)=>return(Vec::new(),true,vec!["Rendered resource references unavailable: malformed or oversized manifest; contents withheld".into()])};
    fn collect(
        object: ManifestObject,
        refs: &mut BTreeSet<ResourceRef>,
        depth: usize,
    ) -> Result<()> {
        if depth > 32 || refs.len() >= MAX_RESOURCES {
            return Err("resource reference limit reached".into());
        }
        // A custom kind such as BlockList is an ordinary resource. Only the
        // generic Kubernetes List or a recognized built-in typed-list envelope
        // may replace its own identity with the contained resource identities.
        let typed_list = object.items.is_some()
            && object.kind.strip_suffix("List").is_some_and(|base| {
                crate::gateway::kinds().iter().any(|kind| {
                    kind.kind == base
                        && object.api_version
                            == if kind.group.is_empty() {
                                kind.version.clone()
                            } else {
                                format!("{}/{}", kind.group, kind.version)
                            }
                })
            });
        if object.kind == "List" || typed_list {
            let items = object.items.ok_or("List envelope has no items")?;
            for item in items {
                collect(item, refs, depth + 1)?;
            }
            return Ok(());
        }
        if !bounded(&object.api_version, 320, false)
            || !bounded(&object.kind, 128, false)
            || !bounded(&object.metadata.name, 253, false)
            || !bounded(&object.metadata.namespace, 63, true)
        {
            return Err("invalid rendered resource identity".into());
        }
        refs.insert(ResourceRef {
            api_version: object.api_version,
            kind: object.kind,
            namespace: object.metadata.namespace,
            name: object.metadata.name,
        });
        Ok(())
    }
    let mut refs = BTreeSet::new();
    for object in documents {
        if let Err(error) = collect(object, &mut refs, 0) {
            return (refs.into_iter().collect(), true, vec![error]);
        }
    }
    (refs.into_iter().collect(), false, Vec::new())
}
fn demo(namespace: &str) -> Snapshot {
    let namespace = if namespace == "*" { "demo" } else { namespace };
    Snapshot {
        releases: vec![Release {
            namespace: namespace.into(),
            name: "demo-api".into(),
            revision: 2,
            status: "deployed".into(),
            chart_name: "api".into(),
            chart_version: "1.2.0".into(),
            app_version: "v1".into(),
            updated: "2026-10-10T08:00:00Z".into(),
            storage: Storage::Secret,
            uid: "demo-helm-v2".into(),
            resource_version: "2".into(),
        }],
        truncated: false,
        issues: Vec::new(),
        source: "DEMO sample Helm data".into(),
    }
}
fn demo_history(namespace: &str) -> Snapshot {
    let mut snapshot = demo(namespace);
    let mut previous = snapshot.releases[0].clone();
    previous.revision = 1;
    previous.status = "superseded".into();
    previous.uid = "demo-helm-v1".into();
    previous.resource_version = "1".into();
    previous.chart_version = "1.1.0".into();
    snapshot.releases.push(previous);
    snapshot
}
fn demo_selected(selected: &Release) -> Result<Release> {
    validate_selection(selected)?;
    demo_history(&selected.namespace)
        .releases
        .into_iter()
        .find(|release| {
            release.name == selected.name
                && release.revision == selected.revision
                && release.storage == selected.storage
                && release.uid == selected.uid
        })
        .ok_or("selected demo Helm release identity is unavailable".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use serde_json::json;
    use std::{
        io::Write,
        sync::{Arc, Mutex},
    };
    use tower::Layer;
    fn payload(name: &str, revision: u32, status: &str) -> Value {
        json!({"name":name,"namespace":"test","version":revision,"info":{"status":status,"last_deployed":"2026-10-22T12:00:00Z","notes":"NOTES_CANARY","description":"DESCRIPTION_CANARY"},"chart":{"metadata":{"name":"example","version":"1.2.3","appVersion":"v1"},"files":[{"name":"private.key","data":"FILES_CANARY"}],"templates":[{"name":"secret.yaml","data":"TEMPLATE_CANARY"}],"values":{"password":"DEFAULT_CANARY"}},"config":{"nested":{"refresh_token":"TOKEN_CANARY"},"apiPassword":"PASSWORD_CANARY","replicaCount":2},"manifest":"apiVersion: v1\nkind: Secret\nmetadata:\n  name: db-auth\ndata:\n  private: MANIFEST_CANARY\n---\napiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: api\n  namespace: test\nspec:\n  template:\n    spec:\n      containers:\n        - name: api\n          env:\n            - name: TOKEN\n              value: ENV_CANARY\n"})
    }
    fn encoded(payload: &Value, gzip: bool) -> Result<String> {
        let bytes = serde_json::to_vec(payload).map_err(|_| "fixture JSON failed")?;
        let bytes = if gzip {
            let mut writer = GzEncoder::new(Vec::new(), Compression::fast());
            writer
                .write_all(&bytes)
                .map_err(|_| "fixture gzip write failed")?;
            writer.finish().map_err(|_| "fixture gzip finish failed")?
        } else {
            bytes
        };
        Ok(STANDARD.encode(bytes))
    }
    fn object_value(
        storage: Storage,
        name: &str,
        revision: u32,
        status: &str,
        gzip: bool,
    ) -> Result<Value> {
        let encoded = encoded(&payload(name, revision, status), gzip)?;
        Ok(
            json!({"apiVersion":"v1","kind":storage.name(),"metadata":{"name":storage_key(name,revision),"namespace":"test","uid":format!("{name}-{revision}"),"resourceVersion":revision.to_string(),"labels":{"owner":"helm","name":name,"version":revision.to_string(),"status":status}},"type":"helm.sh/release.v1","data":{"release":if storage==Storage::Secret{STANDARD.encode(encoded)}else{encoded}}}),
        )
    }
    fn stored(storage: Storage, value: Value) -> Result<Stored> {
        match storage {
            Storage::Secret => serde_json::from_value(value).map(Stored::Secret),
            Storage::ConfigMap => serde_json::from_value(value).map(Stored::ConfigMap),
        }
        .map_err(|_| "invalid storage fixture".into())
    }
    #[test]
    fn lifecycle_hook_projection_binds_native_storage_uid_rv_and_actual_execution_metadata()
    -> Result<()> {
        crate::cli::runtime()?.block_on(async {
            for storage in [Storage::Secret, Storage::ConfigMap] {
                let hook = json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"cleanup","namespace":"test","labels":{"app.kubernetes.io/managed-by":"Helm"},"annotations":{"meta.helm.sh/release-name":"sample","meta.helm.sh/release-namespace":"test","helm.sh/hook":"pre-delete"}},"data":{"message":"cleanup"}});
                let mut wire = payload("sample", 2, "deployed");
                wire["hooks"] = json!([{"name":"cleanup","kind":"ConfigMap","path":"PATH_CANARY","manifest":hook.to_string(),"events":["pre-delete"],"weight":0,"delete_policies":[],"output_log_policies":[],"last_run":{"phase":"RUN_CANARY"}}]);
                let encoded = encoded(&wire, true)?;
                let mut body = object_value(storage, "sample", 2, "deployed", true)?;
                body["data"]["release"] = json!(if storage == Storage::Secret { STANDARD.encode(encoded) } else { encoded });
                let object = stored(storage, body.clone())?;
                let identity = object.identity(storage, "test")?;
                let expected = project(&decode(object.payload()?, &mut DecodeBudget::default())?, &identity)?;
                let returned = Arc::new(Mutex::new(body));
                let state = returned.clone();
                let service = tower::service_fn(move |request: http::Request<kube::client::Body>| {
                    let body = state.lock().unwrap_or_else(|error| panic!("{error}")).clone();
                    let suffix = if storage == Storage::Secret { "secrets" } else { "configmaps" };
                    assert_eq!(request.method(), http::Method::GET);
                    assert_eq!(request.uri().path(), format!("/api/v1/namespaces/test/{suffix}/sh.helm.release.v1.sample.v2"));
                    async move { Ok::<_, std::convert::Infallible>(http::Response::new(http_body_util::Full::new(bytes::Bytes::from(body.to_string())))) }
                });
                let gateway = Gateway::for_helm_test_client(Client::new(service, "test"), "test");
                let raw = lifecycle_hooks(&gateway, &expected).await?;
                assert!(!raw.to_string().contains("CANARY"));
                let plan = crate::helm_extensions::hooks(&raw,"test","sample",crate::helm_extensions::Operation::Uninstall,true)?;
                assert_eq!(plan.active().len(), 1);
                for variant in 0..4 {
                    let mut altered = wire.clone();
                    match variant {
                        0 => altered["hooks"][0]["events"] = json!(["post-install"]),
                        1 => altered["hooks"][0]["delete_policies"] = json!(["hook-failed"]),
                        2 => altered["hooks"][0]["weight"] = json!(1),
                        _ => altered["hooks"][0]["output_log_policies"] = json!(["hook-succeeded"]),
                    }
                    let projected = hook_projection(&serde_json::to_vec(&altered).map_err(|_| "fixture JSON")?)?;
                    assert!(crate::helm_extensions::hooks(&projected,"test","sample",crate::helm_extensions::Operation::Uninstall,true).is_err());
                }
                for field in ["resourceVersion", "uid"] {
                    returned.lock().unwrap_or_else(|error| panic!("{error}"))["metadata"][field] = json!("changed");
                    assert!(lifecycle_hooks(&gateway, &expected).await.is_err());
                }
            }
            let mut aliases = payload("sample", 2, "deployed");
            aliases["Hooks"] = json!([]);
            assert!(hook_projection(&serde_json::to_vec(&aliases).map_err(|_| "fixture JSON")?).is_err());
            aliases.as_object_mut().ok_or("fixture object")?.remove("Hooks");
            aliases["hooks"] = json!([{"Events":["pre-delete"]}]);
            assert!(hook_projection(&serde_json::to_vec(&aliases).map_err(|_| "fixture JSON")?).is_err());
            Ok(())
        })
    }
    #[test]
    fn secret_and_configmap_storage_decode_only_safe_metadata_for_gzip_and_legacy() -> Result<()> {
        for storage in [Storage::Secret, Storage::ConfigMap] {
            for gzip in [false, true] {
                let object = stored(
                    storage,
                    object_value(storage, "sample", 2, "deployed", gzip)?,
                )?;
                let identity = object.identity(storage, "test")?;
                let bytes = decode(object.payload()?, &mut DecodeBudget::default())?;
                let release = project(&bytes, &identity)?;
                assert_eq!(release.revision, 2);
                assert_eq!(release.chart_name, "example");
                assert_eq!(release.storage, storage);
                let rendered =
                    serde_json::to_string(&release).map_err(|_| "cannot render safe projection")?;
                for forbidden in [
                    "CANARY",
                    "\"manifest\":",
                    "\"config\":",
                    "\"templates\":",
                    "\"notes\":",
                    "\"description\":",
                    "\"files\":",
                ] {
                    assert!(!rendered.contains(forbidden), "{rendered}");
                }
                let (refs, incomplete, issues) = resource_references(
                    payload("sample", 2, "deployed")["manifest"]
                        .as_str()
                        .ok_or("fixture manifest missing")?,
                );
                assert!(!incomplete && issues.is_empty());
                assert_eq!(refs.len(), 2);
                let refs = serde_json::to_string(&refs).map_err(|_| "cannot render refs")?;
                assert!(refs.contains("db-auth") && refs.contains("Deployment"));
                assert!(!refs.contains("CANARY"));
            }
        }
        Ok(())
    }
    #[test]
    fn custom_list_suffixed_resources_are_kept_and_only_known_envelopes_expand() {
        let manifest = "apiVersion: security.example/v1\nkind: BlockList\nmetadata:\n  name: blocked-hosts\n---\napiVersion: v1\nkind: PodList\nitems:\n- apiVersion: v1\n  kind: Pod\n  metadata:\n    name: api\n---\napiVersion: v1\nkind: List\nitems: []\n";
        let (refs, incomplete, issues) = resource_references(manifest);
        assert!(!incomplete && issues.is_empty());
        assert_eq!(refs.len(), 2);
        assert!(
            refs.iter()
                .any(|resource| resource.kind == "BlockList" && resource.name == "blocked-hosts")
        );
        assert!(
            refs.iter()
                .any(|resource| resource.kind == "Pod" && resource.name == "api")
        );
        let (_, incomplete, _) = resource_references("apiVersion: v1\nkind: List\n");
        assert!(incomplete);
    }
    #[test]
    fn shared_lookup_requires_complete_unambiguous_inventory_and_uses_latest_revision() -> Result<()>
    {
        assert!(valid_release_name("release.with-dots"));
        for invalid in [
            "",
            ".release",
            "release.",
            "release..name",
            "Upper",
            "bad/name",
        ] {
            assert!(!valid_release_name(invalid), "{invalid}");
        }
        let mut snapshot = demo_history("test");
        assert_eq!(select_release(&snapshot, "demo-api", None)?.revision, 2);
        assert!(select_release(&snapshot, "missing", None).is_err());
        snapshot.truncated = true;
        assert!(select_release(&snapshot, "demo-api", None).is_err());
        snapshot.truncated = false;
        snapshot.issues.push("ConfigMap read denied".into());
        assert!(select_release(&snapshot, "demo-api", Some(Storage::Secret)).is_err());
        snapshot.issues.clear();
        let mut other = snapshot.releases[0].clone();
        other.storage = Storage::ConfigMap;
        snapshot.releases.push(other);
        assert!(select_release(&snapshot, "demo-api", None).is_err());
        assert_eq!(
            select_release(&snapshot, "demo-api", Some(Storage::ConfigMap))?.storage,
            Storage::ConfigMap
        );
        let mut other = snapshot.releases[0].clone();
        other.namespace = "another".into();
        snapshot.releases.push(other);
        assert!(select_release(&snapshot, "demo-api", Some(Storage::Secret)).is_err());
        Ok(())
    }
    #[test]
    fn ownership_type_revision_scope_uid_and_payload_consistency_are_mandatory() -> Result<()> {
        let valid = object_value(Storage::Secret, "sample", 2, "deployed", true)?;
        for (pointer, value) in [
            ("/type", json!("Opaque")),
            ("/metadata/labels/owner", json!("other")),
            ("/metadata/labels/version", json!("02")),
            ("/metadata/labels/name", json!("other")),
            ("/metadata/name", json!("unrelated-secret")),
            ("/metadata/namespace", json!("outside")),
            ("/metadata/uid", json!("")),
            ("/metadata/labels/status", json!("made-up")),
        ] {
            let mut changed = valid.clone();
            *changed
                .pointer_mut(pointer)
                .ok_or("fixture pointer missing")? = value;
            assert!(
                stored(Storage::Secret, changed)?
                    .identity(Storage::Secret, "test")
                    .is_err(),
                "{pointer}"
            );
        }
        let object = stored(Storage::Secret, valid.clone())?;
        let identity = object.identity(Storage::Secret, "test")?;
        let mut release = project(
            &decode(object.payload()?, &mut DecodeBudget::default())?,
            &identity,
        )?;
        release.uid = "replaced".into();
        assert!(selected_object(stored(Storage::Secret, valid.clone())?, &release).is_err());
        let mut corrupt = payload("other", 2, "deployed");
        for (field, value) in [
            ("name", json!("other")),
            ("namespace", json!("outside")),
            ("version", json!(3)),
        ] {
            corrupt[field] = value;
            assert!(
                project(
                    &serde_json::to_vec(&corrupt).map_err(|_| "fixture encoding")?,
                    &identity
                )
                .is_err()
            );
            corrupt = payload("sample", 2, "deployed");
        }
        let mut corrupt = payload("sample", 2, "deployed");
        corrupt["chart"]["metadata"]["name"] = json!("bad\u{001b}[2J");
        assert!(
            project(
                &serde_json::to_vec(&corrupt).map_err(|_| "fixture encoding")?,
                &identity
            )
            .is_err()
        );
        Ok(())
    }
    #[test]
    fn decompression_crc_and_aggregate_budgets_are_enforced() -> Result<()> {
        let mut writer = GzEncoder::new(Vec::new(), Compression::fast());
        writer
            .write_all(&vec![b'x'; MAX_DECODED + 1])
            .map_err(|_| "fixture gzip failed")?;
        let compressed = writer.finish().map_err(|_| "fixture gzip finish failed")?;
        assert!(
            decode(
                STANDARD.encode(&compressed).as_bytes(),
                &mut DecodeBudget::default()
            )
            .is_err()
        );
        let mut encoded = STANDARD
            .decode(encoded(&payload("sample", 1, "deployed"), true)?)
            .map_err(|_| "fixture base64")?;
        encoded.truncate(encoded.len().saturating_sub(5));
        assert!(
            decode(
                STANDARD.encode(encoded).as_bytes(),
                &mut DecodeBudget::default()
            )
            .is_err()
        );
        assert!(decode(b"not base64!", &mut DecodeBudget::default()).is_err());
        let mut budget = DecodeBudget {
            encoded: 16 * 1024 * 1024,
            decoded: 0,
            exceeded: false,
        };
        assert!(decode(b"e30=", &mut budget).is_err());
        assert!(budget.exceeded);
        let mut values = json!({"refresh_token":"TOKEN_CANARY","image":{"tag":"v1"},"apiKey":"API_CANARY","api_key":"API_CANARY","passphrase":"PHRASE_CANARY","tls":{"key":"TLS_CANARY","certificate":"public certificate"},"unusual":"-----BEGIN EC PRIVATE KEY-----\nPEM_CANARY\n-----END EC PRIVATE KEY-----", "nested":[{"privateKey":"KEY_CANARY"}]});
        redact_values(&mut values, 0, &mut 0)?;
        safety::redact(&mut values);
        assert!(!values.to_string().contains("CANARY"));
        assert_eq!(values["image"]["tag"], "v1");
        assert_eq!(values["tls"]["certificate"], "public certificate");
        Ok(())
    }
    #[test]
    fn paginated_storage_uses_helm_selectors_and_rejects_repeated_tokens_without_secret_errors()
    -> Result<()> {
        crate::cli::runtime()?.block_on(async {
            let requests=Arc::new(Mutex::new(Vec::new()));let recorded=requests.clone();
            let item=object_value(Storage::ConfigMap,"sample",1,"deployed",true)?;
            let service=tower::service_fn(move|request:http::Request<kube::client::Body>|{
                recorded.lock().unwrap_or_else(|error|panic!("{error}")).push(request.uri().to_string());
                let item=item.clone();async move {
                    let body=json!({"apiVersion":"v1","kind":"ConfigMapList","metadata":{"resourceVersion":"10","continue":"repeated"},"items":[item]});
                    Ok::<_,std::convert::Infallible>(http::Response::new(http_body_util::Full::new(bytes::Bytes::from(body.to_string()))))
                }
            });
            let client=Client::new(crate::transport::BodyBudgetLayer::default().layer(service),"test");
            let reader=Reader::new(client,"test")?;
            let snapshot=reader.collect(Storage::ConfigMap,Some("sample"),tokio::time::Instant::now()+READ_BUDGET).await;
            assert!(snapshot.truncated);assert_eq!(snapshot.releases.len(),1);
            let requests=requests.lock().unwrap_or_else(|error|panic!("{error}"));
            assert_eq!(requests.len(),2);assert!(requests.iter().all(|uri|uri.contains("labelSelector=owner%3Dhelm%2Cname%3Dsample")));
            assert!(!serde_json::to_string(&snapshot).map_err(|_|"snapshot encoding")?.contains("CANARY"));
            Ok(())
        })
    }
    #[test]
    fn public_reads_keep_drivers_separate_and_values_explicit_and_reject_replacements() -> Result<()>
    {
        crate::cli::runtime()?.block_on(async {
            let previous=object_value(Storage::Secret,"sample",1,"superseded",true)?;
            let current=object_value(Storage::Secret,"sample",2,"deployed",true)?;
            let alternate=object_value(Storage::ConfigMap,"sample",1,"deployed",true)?;
            let replaced=Arc::new(std::sync::atomic::AtomicBool::new(false));let replacement=replaced.clone();
            let requests=Arc::new(Mutex::new(Vec::new()));let recorded=requests.clone();
            let service=tower::service_fn(move|request:http::Request<kube::client::Body>|{
                assert_eq!(request.method(),http::Method::GET);
                recorded.lock().unwrap_or_else(|error|panic!("{error}")).push(request.uri().to_string());
                let previous=previous.clone();let mut current=current.clone();let alternate=alternate.clone();let replacement=replacement.clone();
                async move {
                    let body=if request.uri().path().ends_with("/secrets") {
                        assert!(request.uri().query().is_some_and(|query|query.contains("owner%3Dhelm")));
                        json!({"apiVersion":"v1","kind":"SecretList","metadata":{"resourceVersion":"10"},"items":[previous,current]})
                    }else if request.uri().path().ends_with("/configmaps") {
                        json!({"apiVersion":"v1","kind":"ConfigMapList","metadata":{"resourceVersion":"10"},"items":[alternate]})
                    }else {
                        assert!(request.uri().path().ends_with("/secrets/sh.helm.release.v1.sample.v2"));
                        if replacement.load(std::sync::atomic::Ordering::SeqCst){current["metadata"]["uid"]=json!("replacement-uid");}
                        current
                    };
                    Ok::<_,std::convert::Infallible>(http::Response::new(http_body_util::Full::new(bytes::Bytes::from(body.to_string()))))
                }
            });
            let client=Client::new(crate::transport::BodyBudgetLayer::default().layer(service),"test");
            let denied=Gateway::for_test_client(client.clone(),"test");
            assert!(list(&denied,"test").await.is_err());
            assert!(denied.list("secrets","test").await.is_err());
            assert!(requests.lock().unwrap_or_else(|error|panic!("{error}")).is_empty());
            let gateway=Gateway::for_helm_test_client(client,"test");
            let snapshot=list(&gateway,"test").await?;
            assert_eq!(snapshot.releases.len(),2);assert!(!snapshot.truncated);
            let selected=snapshot.releases.iter().find(|release|release.storage==Storage::Secret).ok_or("Secret release projection missing")?;
            assert_eq!(selected.revision,2);
            let revisions=history(&gateway,selected).await?;
            assert_eq!(revisions.releases.iter().map(|release|release.revision).collect::<Vec<_>>(),vec![2,1]);
            let details=detail(&gateway,selected).await?;
            assert!(!details.resources_truncated);
            assert!(details.resources.iter().all(|reference|reference.namespace=="test"));
            let rendered=serde_json::to_string(&details).map_err(|_|"safe detail encoding")?;
            assert!(!rendered.contains("CANARY"));assert!(!rendered.contains("replicaCount"));
            let projected_values=values(&gateway,selected).await?;
            assert_eq!(projected_values.values["replicaCount"],2);
            assert_eq!(projected_values.values["nested"]["refresh_token"],"[REDACTED]");
            assert!(!projected_values.text()?.contains("CANARY"));
            replaced.store(true,std::sync::atomic::Ordering::SeqCst);
            assert!(detail(&gateway,selected).await.is_err());
            assert!(values(&gateway,selected).await.is_err());
            Ok(())
        })
    }
    #[test]
    fn denied_driver_is_incomplete_evidence_and_never_reflects_secret_server_message() -> Result<()>
    {
        crate::cli::runtime()?.block_on(async {
            let current=object_value(Storage::Secret,"sample",2,"deployed",true)?;
            let service=tower::service_fn(move|request:http::Request<kube::client::Body>|{
                let current=current.clone();async move {
                    let (status,body)=if request.uri().path().ends_with("/secrets") {
                        (200,json!({"apiVersion":"v1","kind":"SecretList","metadata":{"resourceVersion":"10"},"items":[current]}))
                    }else {(403,json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Forbidden","message":"SERVER_CREDENTIAL_CANARY","code":403}))};
                    Ok::<_,std::convert::Infallible>(http::Response::builder().status(status).body(http_body_util::Full::new(bytes::Bytes::from(body.to_string()))).unwrap_or_else(|error|panic!("{error}")))
                }
            });
            let gateway=Gateway::for_helm_test_client(Client::new(service,"test"),"test");
            let snapshot=list(&gateway,"test").await?;
            assert_eq!(snapshot.releases.len(),1);assert!(snapshot.truncated);
            assert!(snapshot.issues.iter().any(|issue|issue.contains("ConfigMap")&&issue.contains("403")));
            assert!(!snapshot.text().contains("CANARY"));
            Ok(())
        })
    }
}
