//! Bounded read-only health and ownership views used by :pulses and :xray.
use crate::{
    Result,
    gateway::{Gateway, summary},
    safety,
};
use futures_util::{StreamExt, stream};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    time::Duration,
};

const RESOURCES: &[&str] = &[
    "nodes",
    "pods",
    "deployments",
    "statefulsets",
    "daemonsets",
    "replicasets",
    "jobs",
    "cronjobs",
    "services",
];
const OUTPUT_LIMIT: usize = 256 * 1024;

/// Read-only drilldown preserves the complete Kubernetes label requirement set.
pub fn pod_selector(object: &Value) -> Result<String> {
    let kind = object["kind"].as_str().unwrap_or("");
    let api = object["apiVersion"].as_str().unwrap_or("");
    let value = match (kind, api) {
        ("Service" | "ReplicationController", "v1") => {
            serde_json::json!({"matchLabels":object["spec"]["selector"]})
        }
        ("Deployment" | "StatefulSet" | "DaemonSet" | "ReplicaSet", "apps/v1")
        | ("Job", "batch/v1") => object["spec"]["selector"].clone(),
        _ => return Err("this resource has no direct Pod label selector".into()),
    };
    let selector: k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector =
        serde_json::from_value(value).map_err(|_| "resource has an invalid Pod selector")?;
    let selector: kube::core::Selector = selector
        .try_into()
        .map_err(|_| "resource has an unsupported Pod selector")?;
    if selector.selects_all() {
        return Err("resource has no Pod selector; refusing an unscoped drilldown".into());
    }
    let text = selector.to_string();
    crate::selectors::validate(&text)?;
    Ok(text)
}

pub async fn workloads(gateway: &Gateway, namespace: &str) -> Result<String> {
    let reads = collect(
        gateway,
        namespace,
        &[
            "deployments",
            "statefulsets",
            "daemonsets",
            "jobs",
            "cronjobs",
        ],
    )
    .await;
    let mut out = format!(
        "WORKLOADS | {} / {} | {}\nKIND                 NAMESPACE / NAME                            STATUS\n",
        safety::safe_text(&gateway.context, 253),
        namespace,
        if gateway.is_demo() {
            "DEMO"
        } else {
            "read-only snapshot"
        }
    );
    for read in reads {
        if let Some(error) = read.error {
            out.push_str(&format!("UNAVAILABLE {}: {}\n", read.resource, error));
        }
        if read.incomplete {
            out.push_str(&format!(
                "INCOMPLETE {}: narrow scope or verify access\n",
                read.resource
            ));
        }
        for item in read.items {
            if out.len() > OUTPUT_LIMIT - 2048 {
                out.push_str("TRUNCATED output; narrow namespace\n");
                return Ok(out);
            }
            out.push_str(&format!(
                "{:<20} {:<43} {}\n",
                item["kind"].as_str().unwrap_or("?"),
                safety::safe_text(
                    &format!(
                        "{}/{}",
                        item["metadata"]["namespace"].as_str().unwrap_or("-"),
                        item["metadata"]["name"].as_str().unwrap_or("?")
                    ),
                    507
                ),
                summary(&item)
            ));
        }
    }
    Ok(out)
}

struct Read {
    resource: String,
    items: Vec<Value>,
    incomplete: bool,
    error: Option<String>,
}
async fn collect(gateway: &Gateway, namespace: &str, resources: &[&str]) -> Vec<Read> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    stream::iter(resources.iter().map(|resource| async move {
        let result = tokio::time::timeout_at(deadline, gateway.list(resource, namespace)).await;
        match result {
            Ok(Ok(mut list)) => Read {
                resource: (*resource).into(),
                incomplete: list["truncated"] == true,
                items: list["items"]
                    .as_array_mut()
                    .map(std::mem::take)
                    .unwrap_or_default(),
                error: None,
            },
            other => Read {
                resource: (*resource).into(),
                items: Vec::new(),
                incomplete: true,
                error: Some(match other {
                    Ok(Err(e)) => safety::safe_text(&e, 160),
                    _ => "snapshot deadline exceeded".into(),
                }),
            },
        }
    }))
    .buffered(4)
    .collect()
    .await
}

fn healthy(value: &Value) -> Option<bool> {
    if value["metadata"]["deletionTimestamp"].is_string() {
        return Some(false);
    }
    match value["kind"].as_str().unwrap_or("") {
        "Pod" => Some(matches!(summary(value).as_str(), "Running" | "Completed")),
        "Node" => {
            let conditions = value["status"]["conditions"].as_array()?;
            let ready =
                conditions.iter().find(|c| c["type"] == "Ready")?["status"].as_str()? == "True";
            Some(
                ready
                    && !conditions.iter().any(|c| {
                        c["status"] == "True"
                            && matches!(
                                c["type"].as_str(),
                                Some(
                                    "MemoryPressure"
                                        | "DiskPressure"
                                        | "PIDPressure"
                                        | "NetworkUnavailable"
                                )
                            )
                    }),
            )
        }
        "Deployment" | "StatefulSet" | "ReplicaSet" => {
            let desired = value["spec"]["replicas"].as_u64().unwrap_or(1);
            if let Some(generation) = value["metadata"]["generation"].as_u64() {
                let observed = value["status"]["observedGeneration"].as_u64()?;
                if observed < generation {
                    return Some(false);
                }
            }
            let ready = value["status"]["readyReplicas"].as_u64().unwrap_or(0) >= desired;
            if value["kind"] == "Deployment" {
                return Some(
                    ready
                        && ["updatedReplicas", "availableReplicas"]
                            .iter()
                            .all(|field| value["status"][field].as_u64().unwrap_or(0) >= desired),
                );
            }
            if value["kind"] == "StatefulSet"
                && value["status"]["updateRevision"].is_string()
                && value["status"]["currentRevision"].is_string()
                && value["spec"]["updateStrategy"]["type"] != "OnDelete"
            {
                let partition = value["spec"]["updateStrategy"]["rollingUpdate"]["partition"]
                    .as_u64()
                    .unwrap_or(0);
                if partition > 0 {
                    return Some(
                        ready
                            && value["status"]["updatedReplicas"].as_u64().unwrap_or(0)
                                >= desired.saturating_sub(partition),
                    );
                }
                return Some(
                    ready
                        && value["status"]["updateRevision"] == value["status"]["currentRevision"],
                );
            }
            Some(ready)
        }
        "DaemonSet" => {
            if let Some(generation) = value["metadata"]["generation"].as_u64()
                && value["status"]["observedGeneration"].as_u64()? < generation
            {
                return Some(false);
            }
            value["status"]["desiredNumberScheduled"]
                .as_u64()
                .map(|desired| {
                    value["status"]["numberReady"].as_u64().unwrap_or(0) >= desired
                        && value["status"]["updatedNumberScheduled"]
                            .as_u64()
                            .unwrap_or(0)
                            >= desired
                })
        }
        "Job" => {
            if value["spec"]["suspend"] == true {
                return None;
            }
            let failed = value["status"]["conditions"].as_array().is_some_and(|c| {
                c.iter()
                    .any(|c| c["type"] == "Failed" && c["status"] == "True")
            });
            Some(
                !failed
                    && (value["status"]["active"].as_u64().unwrap_or(0) > 0
                        || value["status"]["succeeded"].as_u64().unwrap_or(0)
                            >= value["spec"]["completions"].as_u64().unwrap_or(1)),
            )
        }
        _ => None,
    }
}

pub async fn pulses(gateway: &Gateway, namespace: &str) -> Result<String> {
    let reads = collect(gateway, namespace, RESOURCES).await;
    let mut out = format!(
        "PULSES  {} / {}\nRead-only health snapshot | {}\n\nRESOURCE                  TOTAL HEALTHY  ATTENTION UNKNOWN  STATE\n",
        safety::safe_text(&gateway.context, 253),
        namespace,
        if gateway.is_demo() {
            "DEMO sample data"
        } else {
            "up to 1000 objects per kind; refresh to resample"
        }
    );
    for read in reads {
        if let Some(error) = read.error {
            out.push_str(&format!(
                "{:<25}     -       -                 -   UNAVAILABLE: {}\n",
                read.resource, error
            ));
            continue;
        }
        let evaluated: Vec<bool> = read.items.iter().filter_map(healthy).collect();
        let ready = evaluated.iter().filter(|ready| **ready).count();
        let counts = if evaluated.is_empty() {
            ("-".into(), "-".into())
        } else {
            (ready.to_string(), (evaluated.len() - ready).to_string())
        };
        out.push_str(&format!(
            "{:<25} {:>5} {:>7} {:>10} {:>7}  {}\n",
            read.resource,
            read.items.len(),
            counts.0,
            counts.1,
            read.items.len() - evaluated.len(),
            if read.incomplete {
                "TRUNCATED"
            } else {
                "snapshot"
            }
        ));
    }
    out.push_str("\nHEALTHY uses available pod/node/workload status; UNKNOWN has no health assessment.\nUnavailable and truncated reads do not imply an empty or healthy cluster.\n:pods /!Running|Completed shows pods needing attention; :nodes opens nodes.");
    Ok(out)
}

#[derive(Clone)]
struct Entity {
    id: String,
    kind: String,
    namespace: String,
    name: String,
    state: String,
    owners: Vec<String>,
    labels: BTreeMap<String, String>,
    selector: BTreeMap<String, String>,
    node: String,
}
impl Entity {
    fn from(value: &Value) -> Option<Self> {
        let field = |key: &str| {
            value["metadata"][key]
                .as_str()
                .map(|s| safety::safe_text(s, 253))
        };
        let map = |value: &Value| {
            value
                .as_object()
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
                        .collect()
                })
                .unwrap_or_default()
        };
        Some(Self {
            id: field("uid")?,
            kind: value["kind"].as_str()?.into(),
            namespace: field("namespace").unwrap_or_default(),
            name: field("name")?,
            state: summary(value),
            owners: value["metadata"]["ownerReferences"]
                .as_array()
                .map(|v| {
                    v.iter()
                        .filter_map(|o| o["uid"].as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
            labels: map(&value["metadata"]["labels"]),
            selector: map(&value["spec"]["selector"]),
            node: value["spec"]["nodeName"].as_str().unwrap_or("").into(),
        })
    }
    fn child_of(&self, parent: &Self) -> bool {
        if parent.kind == "Node" {
            return self.kind == "Pod" && self.node == parent.name;
        }
        if self.namespace != parent.namespace {
            return false;
        }
        self.owners.iter().any(|id| id == &parent.id)
            || (parent.kind == "Service"
                && self.kind == "Pod"
                && !parent.selector.is_empty()
                && parent
                    .selector
                    .iter()
                    .all(|(k, v)| self.labels.get(k) == Some(v)))
    }
}

fn tree(
    entity: &Entity,
    all: &[Entity],
    depth: usize,
    seen: &mut HashSet<String>,
    out: &mut String,
    lines: &mut usize,
) {
    if *lines >= 2000 || out.len() >= OUTPUT_LIMIT.saturating_sub(2048) {
        return;
    }
    *lines += 1;
    out.push_str(&format!(
        "{}{} / {}{}  [{}]\n",
        "  ".repeat(depth),
        entity.kind,
        if entity.namespace.is_empty() {
            String::new()
        } else {
            format!("{}/", entity.namespace)
        },
        entity.name,
        entity.state
    ));
    if !seen.insert(entity.id.clone()) {
        out.push_str("  [already shown; relationship cycle or shared child]\n");
        return;
    }
    if depth == 8 {
        out.push_str("  [depth limit]\n");
        return;
    }
    for child in all.iter().filter(|child| child.child_of(entity)) {
        tree(child, all, depth + 1, seen, out, lines);
    }
}

fn xray_kind(kind: &crate::gateway::Kind) -> bool {
    kind.version == "v1"
        && matches!(
            (
                kind.group.as_str(),
                kind.plural.as_str(),
                kind.kind.as_str()
            ),
            ("", "nodes", "Node")
                | ("", "pods", "Pod")
                | ("", "services", "Service")
                | ("apps", "deployments", "Deployment")
                | ("apps", "statefulsets", "StatefulSet")
                | ("apps", "daemonsets", "DaemonSet")
                | ("apps", "replicasets", "ReplicaSet")
                | ("batch", "jobs", "Job")
                | ("batch", "cronjobs", "CronJob")
        )
}
pub async fn xray(gateway: &Gateway, resource: &str, namespace: &str) -> Result<String> {
    let kind = gateway.find(resource)?;
    if !xray_kind(kind) {
        return Err(
            "xray supports po, svc, dp/deploy, rs, sts, ds, jobs, cronjobs and nodes".into(),
        );
    }
    let reads = collect(gateway, namespace, RESOURCES).await;
    let mut out = format!(
        "XRAY  {} / {} / {}\n{}\nOwnership uses UID; Service edges use label selectors; Node edges use nodeName.\n\n",
        safety::safe_text(&gateway.context, 253),
        namespace,
        kind.plural,
        if gateway.is_demo() {
            "DEMO sample data"
        } else {
            "Read-only relationship snapshot"
        }
    );
    let mut all = Vec::new();
    for read in reads {
        if let Some(error) = read.error {
            out.push_str(&format!("INCOMPLETE {}: {}\n", read.resource, error));
        } else if read.incomplete {
            out.push_str(&format!(
                "TRUNCATED {}: graph may omit relationships\n",
                read.resource
            ));
        }
        all.extend(read.items.iter().filter_map(Entity::from));
    }
    all.sort_by(|a, b| (&a.namespace, &a.kind, &a.name).cmp(&(&b.namespace, &b.kind, &b.name)));
    let mut lines = 0;
    for root in all.iter().filter(|e| e.kind == kind.kind) {
        tree(root, &all, 0, &mut HashSet::new(), &mut out, &mut lines);
    }
    if lines == 0 {
        out.push_str("No roots in the available snapshot. Check any INCOMPLETE messages above.\n");
    }
    if lines >= 2000 || out.len() >= OUTPUT_LIMIT.saturating_sub(2048) {
        out.push_str("TRUNCATED output: narrow namespace scope.\n");
    }
    Ok(safety::safe_lines(&out, OUTPUT_LIMIT))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn entity(kind: &str, name: &str, uid: &str, ns: &str) -> Entity {
        Entity::from(&json!({"kind":kind,"metadata":{"name":name,"uid":uid,"namespace":ns}}))
            .unwrap_or_else(|| panic!("fixture"))
    }
    #[test]
    fn health_marks_unobserved_rollouts_pressure_and_pending_jobs() {
        assert_eq!(
            healthy(
                &json!({"kind":"Deployment","metadata":{"generation":2},"spec":{"replicas":1},"status":{"readyReplicas":1,"observedGeneration":1}})
            ),
            Some(false)
        );
        assert_eq!(
            healthy(
                &json!({"kind":"Deployment","metadata":{"generation":2},"spec":{"replicas":1},"status":{"readyReplicas":1,"observedGeneration":2,"updatedReplicas":0}})
            ),
            Some(false)
        );
        assert_eq!(
            healthy(
                &json!({"kind":"Node","status":{"conditions":[{"type":"Ready","status":"True"},{"type":"DiskPressure","status":"True"}]}})
            ),
            Some(false)
        );
        assert_eq!(healthy(&json!({"kind":"Job"})), Some(false));
        assert_eq!(
            healthy(&json!({"kind":"Job","status":{"active":1}})),
            Some(true)
        );
        assert_eq!(
            healthy(
                &json!({"kind":"DaemonSet","metadata":{"generation":2},"status":{"numberReady":1,"desiredNumberScheduled":1}})
            ),
            None
        );
    }
    #[test]
    fn omitted_zero_rollout_counters_do_not_report_health() {
        let mut deployment = json!({"kind":"Deployment","metadata":{"generation":2},"spec":{"replicas":1},"status":{"readyReplicas":1,"availableReplicas":1,"observedGeneration":2}});
        assert_eq!(healthy(&deployment), Some(false));
        deployment["status"]["updatedReplicas"] = json!(1);
        assert_eq!(healthy(&deployment), Some(true));
        deployment["status"]["availableReplicas"] = Value::Null;
        assert_eq!(healthy(&deployment), Some(false));
        let mut daemon = json!({"kind":"DaemonSet","metadata":{"generation":2},"status":{"numberReady":1,"desiredNumberScheduled":1,"observedGeneration":2}});
        assert_eq!(healthy(&daemon), Some(false));
        daemon["status"]["updatedNumberScheduled"] = json!(1);
        assert_eq!(healthy(&daemon), Some(true));
        deployment["spec"]["replicas"] = json!(0);
        assert_eq!(healthy(&deployment), Some(true));
        let mut partitioned = json!({"kind":"StatefulSet","metadata":{"generation":2},"spec":{"replicas":3,"updateStrategy":{"type":"RollingUpdate","rollingUpdate":{"partition":2}}},"status":{"readyReplicas":3,"updatedReplicas":1,"observedGeneration":2,"currentRevision":"old","updateRevision":"new"}});
        assert_eq!(healthy(&partitioned), Some(true));
        partitioned["status"]["updatedReplicas"] = json!(0);
        assert_eq!(healthy(&partitioned), Some(false));
        partitioned["spec"]["updateStrategy"]["rollingUpdate"]["partition"] = json!(0);
        assert_eq!(healthy(&partitioned), Some(false));
    }
    #[test]
    fn pod_drilldown_preserves_expressions_and_refuses_empty_selectors() -> Result<()> {
        let selector = pod_selector(
            &json!({"apiVersion":"apps/v1","kind":"Deployment","spec":{"selector":{"matchLabels":{"app":"api"},"matchExpressions":[{"key":"tier","operator":"NotIn","values":["batch"]}]}}}),
        )?;
        assert!(crate::selectors::matches(
            &selector,
            &json!({"app":"api","tier":"web"})
        ));
        assert!(!crate::selectors::matches(
            &selector,
            &json!({"app":"api","tier":"batch"})
        ));
        assert!(
            pod_selector(&json!({"apiVersion":"v1","kind":"Service","spec":{"selector":{}}}))
                .is_err()
        );
        assert!(pod_selector(&json!({"apiVersion":"example.io/v1","kind":"Deployment","spec":{"selector":{"matchLabels":{"app":"api"}}}})).is_err());
        let mut node = crate::gateway::Kind {
            plural: "nodes".into(),
            kind: "Node".into(),
            group: String::new(),
            version: "v1".into(),
            namespaced: false,
            short_names: vec![],
        };
        assert!(xray_kind(&node));
        node.group = "example.io".into();
        assert!(!xray_kind(&node));
        Ok(())
    }
    #[test]
    fn relationships_respect_uid_namespace_and_nonempty_service_selectors() {
        let deployment = entity("Deployment", "api", "old", "one");
        let mut pod = entity("Pod", "api-1", "pod", "one");
        pod.owners.push("new".into());
        assert!(!pod.child_of(&deployment));
        pod.owners.push("old".into());
        assert!(pod.child_of(&deployment));
        pod.namespace = "two".into();
        assert!(!pod.child_of(&deployment));
        let mut svc = entity("Service", "api", "service", "two");
        assert!(!pod.child_of(&svc));
        svc.selector.insert("app".into(), "api".into());
        pod.labels.insert("app".into(), "api".into());
        assert!(pod.child_of(&svc));
        let node = entity("Node", "worker", "node", "");
        pod.node = "worker".into();
        assert!(pod.child_of(&node));
    }
    #[test]
    fn cycles_are_bounded_and_health_does_not_invent_service_readiness() {
        let mut a = entity("ReplicaSet", "a", "a", "one");
        let mut b = entity("ReplicaSet", "b", "b", "one");
        a.owners.push("b".into());
        b.owners.push("a".into());
        let mut text = String::new();
        let mut lines = 0;
        tree(
            &a,
            &[a.clone(), b],
            0,
            &mut HashSet::new(),
            &mut text,
            &mut lines,
        );
        assert_eq!(lines, 3);
        assert!(text.contains("cycle"));
        assert_eq!(healthy(&json!({"kind":"Service"})), None);
        assert_eq!(
            healthy(
                &json!({"kind":"Node","status":{"conditions":[{"type":"Ready","status":"False"}]}})
            ),
            Some(false)
        );
    }
}
