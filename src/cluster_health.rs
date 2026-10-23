//! Dashboard Node and workload readiness (k10s-style health), collected by a
//! bounded read-only poll. Each source keeps its own evidence state, so a
//! denied or missing source is shown as such and never as zero.
use crate::{dashboard_health::EvidenceState, gateway::Gateway, metrics};
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

/// Each read has its own deadline; a slow source cannot hold the others.
const READ_BUDGET: Duration = Duration::from_secs(8);
/// Degraded workloads named on a card; the count is always complete.
pub const NAMED_DEGRADED: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NodeEvidence {
    pub state: EvidenceState,
    pub ready: usize,
    pub total: usize,
    pub cordoned: usize,
    /// Cluster usage as a percentage of summed allocatable, when both the
    /// Metrics API and allocatable values are available.
    pub cpu_percent: Option<u64>,
    pub memory_percent: Option<u64>,
    pub metrics: EvidenceState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WorkloadEvidence {
    pub state: EvidenceState,
    pub deployments: usize,
    pub statefulsets: usize,
    pub degraded: usize,
    /// "deploy/NAME" or "sts/NAME", at most NAMED_DEGRADED entries.
    pub named: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InfraSummary {
    pub nodes: NodeEvidence,
    pub workloads: WorkloadEvidence,
}

impl InfraSummary {
    pub fn connecting() -> Self {
        Self {
            nodes: NodeEvidence {
                state: EvidenceState::Connecting,
                ready: 0,
                total: 0,
                cordoned: 0,
                cpu_percent: None,
                memory_percent: None,
                metrics: EvidenceState::Connecting,
            },
            workloads: WorkloadEvidence {
                state: EvidenceState::Connecting,
                deployments: 0,
                statefulsets: 0,
                degraded: 0,
                named: Vec::new(),
            },
        }
    }
}

fn items(list: &Value) -> &[Value] {
    list["items"].as_array().map_or(&[], Vec::as_slice)
}

fn truncated(list: &Value) -> bool {
    list["truncated"].as_bool().unwrap_or(false)
}

fn condition(object: &Value, kind: &str) -> Option<bool> {
    object["status"]["conditions"]
        .as_array()?
        .iter()
        .find(|c| c["type"] == kind)
        .map(|c| c["status"] == "True")
}

/// Ready/total/cordoned Nodes plus summed allocatable CPU nanocores and bytes.
fn node_counts(nodes: &[Value]) -> (usize, usize, Option<u64>, Option<u64>) {
    let ready = nodes
        .iter()
        .filter(|node| condition(node, "Ready") == Some(true))
        .count();
    let cordoned = nodes
        .iter()
        .filter(|node| node["spec"]["unschedulable"] == true)
        .count();
    let sum = |field: &str, parse: fn(&str) -> crate::Result<(u64, bool)>| {
        nodes.iter().try_fold(0u64, |total, node| {
            let raw = node["status"]["allocatable"][field].as_str()?;
            total.checked_add(parse(raw).ok()?.0)
        })
    };
    (
        ready,
        cordoned,
        sum("cpu", metrics::normalize_cpu_quantity),
        sum("memory", metrics::normalize_memory_quantity),
    )
}

fn percent(used: u64, available: u64) -> Option<u64> {
    (available > 0).then(|| ((u128::from(used) * 100) / u128::from(available)) as u64)
}

/// A workload is degraded when it wants replicas that are not available or
/// ready, or when Kubernetes reports the rollout stuck. Scaled-to-zero is not.
pub fn degraded(kind: &str, object: &Value) -> bool {
    let wanted = object["spec"]["replicas"].as_u64().unwrap_or(1);
    if wanted == 0 {
        return false;
    }
    let status = &object["status"];
    match kind {
        "deploy" => {
            status["availableReplicas"].as_u64().unwrap_or(0) < wanted
                || condition(object, "Available") == Some(false)
                || status["conditions"].as_array().is_some_and(|conditions| {
                    conditions.iter().any(|c| {
                        c["type"] == "Progressing" && c["reason"] == "ProgressDeadlineExceeded"
                    })
                })
        }
        _ => status["readyReplicas"].as_u64().unwrap_or(0) < wanted,
    }
}

pub fn workloads(deployments: &[Value], statefulsets: &[Value]) -> WorkloadEvidence {
    let mut names = deployments
        .iter()
        .map(|o| ("deploy", o))
        .chain(statefulsets.iter().map(|o| ("sts", o)))
        .filter(|(kind, object)| degraded(kind, object))
        .map(|(kind, object)| {
            format!(
                "{kind}/{}",
                crate::safety::safe_text(object["metadata"]["name"].as_str().unwrap_or("?"), 63)
            )
        })
        .collect::<Vec<_>>();
    names.sort();
    let degraded = names.len();
    names.truncate(NAMED_DEGRADED);
    WorkloadEvidence {
        state: EvidenceState::Live,
        deployments: deployments.len(),
        statefulsets: statefulsets.len(),
        degraded,
        named: names,
    }
}

async fn list(gateway: &Gateway, key: &str, ns: &str) -> std::result::Result<Value, EvidenceState> {
    match tokio::time::timeout(READ_BUDGET, gateway.list(key, ns)).await {
        Ok(Ok(list)) if truncated(&list) => Err(EvidenceState::Truncated),
        Ok(Ok(list)) => Ok(list),
        Ok(Err(error)) => Err(EvidenceState::from_error(&error)),
        Err(_) => Err(EvidenceState::Stale),
    }
}

/// One bounded poll. Nodes are cluster-scoped and commonly denied to
/// namespace-scoped identities; that is reported, not hidden.
pub async fn collect(gateway: &Gateway, namespace: &str) -> InfraSummary {
    let (nodes, deployments, statefulsets, usage) = tokio::join!(
        list(gateway, "nodes", "*"),
        list(gateway, "deployments", namespace),
        list(gateway, "statefulsets", namespace),
        tokio::time::timeout(
            READ_BUDGET,
            metrics::collect(gateway, metrics::MetricsScope::Nodes, "*")
        ),
    );
    let nodes = match nodes {
        Ok(list) => {
            let nodes = items(&list);
            let (ready, cordoned, cpu, memory) = node_counts(nodes);
            let (metrics_state, cpu_percent, memory_percent) = match usage {
                Ok(Ok(snapshot)) if !snapshot.truncated => {
                    let used_cpu = snapshot.items.iter().map(|row| row.cpu_nanocores).sum();
                    let used_memory = snapshot.items.iter().map(|row| row.memory_bytes).sum();
                    (
                        EvidenceState::Live,
                        cpu.and_then(|cpu| percent(used_cpu, cpu)),
                        memory.and_then(|memory| percent(used_memory, memory)),
                    )
                }
                Ok(Ok(_)) => (EvidenceState::Truncated, None, None),
                Ok(Err(error)) => (EvidenceState::from_error(&error), None, None),
                Err(_) => (EvidenceState::Stale, None, None),
            };
            NodeEvidence {
                state: EvidenceState::Live,
                ready,
                total: nodes.len(),
                cordoned,
                cpu_percent,
                memory_percent,
                metrics: metrics_state,
            }
        }
        Err(state) => NodeEvidence {
            state,
            ..InfraSummary::connecting().nodes
        },
    };
    let workloads = match (deployments, statefulsets) {
        (Ok(deployments), Ok(statefulsets)) => workloads(items(&deployments), items(&statefulsets)),
        (Err(state), _) | (_, Err(state)) => WorkloadEvidence {
            state,
            ..InfraSummary::connecting().workloads
        },
    };
    InfraSummary { nodes, workloads }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn degraded_workloads_are_named_counted_and_scaled_to_zero_is_healthy() {
        let deploys = vec![
            json!({"metadata":{"name":"api"},"spec":{"replicas":3},"status":{"availableReplicas":2}}),
            json!({"metadata":{"name":"web"},"spec":{"replicas":2},"status":{"availableReplicas":2,"conditions":[{"type":"Available","status":"True"}]}}),
            json!({"metadata":{"name":"idle"},"spec":{"replicas":0},"status":{}}),
            json!({"metadata":{"name":"stuck"},"spec":{"replicas":1},"status":{"availableReplicas":1,"conditions":[{"type":"Progressing","status":"False","reason":"ProgressDeadlineExceeded"}]}}),
        ];
        let sts = vec![
            json!({"metadata":{"name":"db"},"spec":{"replicas":3},"status":{"readyReplicas":3}}),
            json!({"metadata":{"name":"cache\u{1b}[2J"},"spec":{"replicas":1},"status":{}}),
        ];
        let evidence = workloads(&deploys, &sts);
        assert_eq!(
            (
                evidence.deployments,
                evidence.statefulsets,
                evidence.degraded
            ),
            (4, 2, 3)
        );
        assert_eq!(
            evidence.named,
            vec!["deploy/api", "deploy/stuck", "sts/cache[2J"]
        );
    }
    #[test]
    fn nodes_count_ready_cordoned_and_sum_allocatable_exactly() {
        let nodes = vec![
            json!({"spec":{},"status":{"conditions":[{"type":"Ready","status":"True"}],"allocatable":{"cpu":"3500m","memory":"7Gi"}}}),
            json!({"spec":{"unschedulable":true},"status":{"conditions":[{"type":"Ready","status":"False"}],"allocatable":{"cpu":"2","memory":"1Gi"}}}),
        ];
        let (ready, cordoned, cpu, memory) = node_counts(&nodes);
        assert_eq!((ready, cordoned), (1, 1));
        assert_eq!(cpu, Some(5_500_000_000));
        assert_eq!(memory, Some(8 * 1024 * 1024 * 1024));
        assert_eq!(percent(2_750_000_000, 5_500_000_000), Some(50));
        assert_eq!(percent(1, 0), None);
        let missing = vec![json!({"status":{"allocatable":{"cpu":"1"}}})];
        assert_eq!(node_counts(&missing).3, None);
    }
}
