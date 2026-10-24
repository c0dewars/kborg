//! Complete, connection-bound Pod log membership captures.
use super::{
    MAX_LOG_PODS, MAX_MERGED_CONTAINERS, MAX_WORKLOAD_LOG_SOURCES, MEMBERSHIP_BYTES,
    merged::open_failure, ordinary_container_names, validate_merge_target,
};
use crate::{Result, safety};
use k8s_openapi::api::core::v1::Pod;
use kube::{Api, api::ListParams};
use serde::Serialize;
use serde_json::Value;
use tokio::time::Instant;

#[derive(Clone, Debug)]
pub enum PodLogSelection {
    Selector(String),
    Workload {
        resource: String,
        name: String,
        uid: String,
    },
}
#[derive(Clone, Debug, Serialize)]
pub struct LogPod {
    pub name: String,
    pub uid: String,
    pub containers: Vec<String>,
}
#[derive(Clone, Debug, Serialize)]
pub(crate) struct LogParent {
    pub resource: String,
    pub name: String,
    pub uid: String,
}
/// A complete, immutable source snapshot bound to the connection that captured
/// it. Refresh is an explicit new capture, never an implicit Pod replacement.
#[derive(Clone, Debug, Serialize)]
pub struct LogMembership {
    pub(super) namespace: String,
    pub(super) description: String,
    pub(super) pods: Vec<LogPod>,
    #[serde(skip)]
    pub(crate) parents: Vec<LogParent>,
    #[serde(skip)]
    connection: String,
    /// Present only when the user explicitly opted into dynamic membership.
    #[serde(skip)]
    pub(crate) dynamic: Option<PodLogSelection>,
}
impl LogMembership {
    pub fn namespace(&self) -> &str {
        &self.namespace
    }
    pub fn description(&self) -> &str {
        &self.description
    }
    pub fn pods(&self) -> &[LogPod] {
        &self.pods
    }
    pub fn source_count(&self) -> usize {
        self.pods.iter().map(|pod| pod.containers.len()).sum()
    }
    pub(crate) fn new(
        namespace: &str,
        description: String,
        mut pods: Vec<LogPod>,
        parents: Vec<LogParent>,
        connection: &str,
    ) -> Result<Self> {
        if !safety::valid_id(namespace)
            || pods.is_empty()
            || pods.len() > MAX_LOG_PODS
            || parents.len() > MAX_LOG_PODS + 1
        {
            return Err("log membership requires 1..8 Pods in one concrete namespace; narrow the selector or workload".into());
        }
        pods.sort_by(|a, b| a.name.cmp(&b.name));
        let mut names = std::collections::BTreeSet::new();
        let mut identities = std::collections::BTreeSet::new();
        for pod in &pods {
            validate_merge_target(namespace, &pod.name, &pod.uid)?;
            if !identities.insert(&pod.uid)
                || !names.insert(&pod.name)
                || pod.containers.is_empty()
                || pod.containers.len() > MAX_MERGED_CONTAINERS
            {
                return Err("log membership contains duplicate or invalid Pod sources".into());
            }
            let mut names = std::collections::BTreeSet::new();
            if pod
                .containers
                .iter()
                .any(|name| !safety::valid_id(name) || !names.insert(name))
            {
                return Err(
                    "log membership contains invalid or duplicate ordinary containers".into(),
                );
            }
        }
        if pods.iter().map(|pod| pod.containers.len()).sum::<usize>() > MAX_WORKLOAD_LOG_SOURCES {
            return Err("log membership exceeds 32 ordinary-container streams; narrow the selector or workload".into());
        }
        Ok(Self {
            namespace: namespace.into(),
            description: safety::safe_text(&description, 4096),
            pods,
            parents,
            connection: connection.into(),
            dynamic: None,
        })
    }
    /// Opt this capture into announced joins and departures. The selection is
    /// recaptured with the same complete-inventory rules on every refresh.
    pub(crate) fn into_dynamic(mut self, selection: PodLogSelection) -> Self {
        self.description = self
            .description
            .replace("fixed membership", "dynamic membership");
        self.dynamic = Some(selection);
        self
    }
    pub fn is_dynamic(&self) -> bool {
        self.dynamic.is_some()
    }
    pub(crate) fn verify_connection(&self, connection: &str) -> Result<()> {
        if self.connection != connection {
            return Err("log membership belongs to another connection; capture sources again after reconnecting".into());
        }
        Ok(())
    }
}

pub(crate) fn log_pod(pod: &Pod, namespace: &str) -> Result<LogPod> {
    let name = pod.metadata.name.as_deref().ok_or("log Pod name missing")?;
    let uid = pod.metadata.uid.as_deref().ok_or("log Pod UID missing")?;
    Ok(LogPod {
        name: name.into(),
        uid: uid.into(),
        containers: ordinary_container_names(pod, namespace, name, uid)?,
    })
}
pub(crate) fn owned_by(value: &Value, api: &str, kind: &str, uid: &str) -> bool {
    value["metadata"]["ownerReferences"]
        .as_array()
        .is_some_and(|owners| {
            owners.iter().any(|owner| {
                owner["apiVersion"] == api
                    && owner["kind"] == kind
                    && owner["uid"] == uid
                    && owner["controller"] == true
            })
        })
}
pub(crate) fn membership_bytes(bytes: &mut usize, value: &impl Serialize) -> Result<()> {
    // Bound the complete capture cumulatively, independent of per-response limits.
    struct Counter<'a>(&'a mut usize);
    impl std::io::Write for Counter<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MEMBERSHIP_BYTES.saturating_sub(*self.0) {
                return Err(std::io::ErrorKind::FileTooLarge.into());
            }
            *self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter(bytes), value)
        .map_err(|_| "log membership capture exceeded its shared 8 MiB resource budget".into())
}
pub(crate) async fn selected_pods(
    api: &Api<Pod>,
    selector: &str,
    owner: Option<&str>,
    deadline: Instant,
    bytes: &mut usize,
) -> Result<Vec<LogPod>> {
    crate::selectors::validate(selector)?;
    if selector.trim().is_empty() {
        return Err("an explicit nonempty Pod selector is required for workload logs".into());
    }
    let list = tokio::time::timeout_at(
        deadline,
        api.list(&ListParams {
            limit: Some((MAX_LOG_PODS + 1) as u32),
            label_selector: Some(selector.into()),
            ..Default::default()
        }),
    )
    .await
    .map_err(|_| "log membership Pod listing timed out")?
    .map_err(|error| open_failure(error, false))?;
    membership_bytes(bytes, &list)?;
    if list
        .metadata
        .resource_version
        .as_deref()
        .is_none_or(|version| {
            version.is_empty() || version.len() > 253 || safety::safe_text(version, 253) != version
        })
    {
        return Err("Pod log membership omitted a valid list resourceVersion; complete membership cannot be established".into());
    }
    if list
        .metadata
        .continue_
        .as_ref()
        .is_some_and(|token| !token.is_empty())
        || list.items.len() > MAX_LOG_PODS
    {
        return Err(
            "Pod selector exceeds the complete 8-Pod log membership limit; narrow the selector"
                .into(),
        );
    }
    let ns = api.namespace().ok_or("logs require a concrete namespace")?;
    let mut pods = Vec::new();
    for pod in list.items {
        let member = log_pod(&pod, ns)?;
        let labels =
            serde_json::to_value(pod.metadata.labels.as_ref()).map_err(|_| "invalid Pod labels")?;
        if !crate::selectors::matches(selector, &labels) {
            return Err(
                "Pod listing escaped the selected label scope; no log streams opened".into(),
            );
        }
        if owner.is_some_and(|uid| {
            !pod.metadata
                .owner_references
                .as_ref()
                .is_some_and(|owners| {
                    owners.iter().any(|owner| {
                        owner.api_version == "batch/v1"
                            && owner.kind == "Job"
                            && owner.uid == uid
                            && owner.controller == Some(true)
                    })
                })
        }) {
            continue;
        }
        pods.push(member);
    }
    Ok(pods)
}
