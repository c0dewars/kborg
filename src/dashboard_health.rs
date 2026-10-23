//! Bounded health evidence for dashboard cards. No credentials, Pod specifications,
//! Event messages, or arbitrary object data leave these projections.
use crate::safety;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;

pub const WARNING_WINDOW_SECONDS: u64 = 15 * 60;
const MAX_RECENT_WARNINGS: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceState {
    Connecting,
    Live,
    Stale,
    Denied,
    Unavailable,
    Truncated,
}

impl EvidenceState {
    pub fn from_error(message: &str) -> Self {
        let message = message.to_ascii_lowercase();
        if message.contains("limit exceeded") || message.contains("page limit") {
            Self::Truncated
        } else if message.contains("authorization denied")
            || message.contains("api 401")
            || message.contains("api 403")
        {
            Self::Denied
        } else if message.contains("unavailable")
            || message.contains("unsupported")
            || message.contains("invalid")
            || message.contains("rejected")
        {
            Self::Unavailable
        } else {
            Self::Stale
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct WarningEvidence {
    pub namespace: String,
    pub pod: String,
    /// Exact Pod UID for event drilldown; not part of the CLI projection.
    #[serde(skip)]
    pub uid: String,
    pub reason: String,
    pub occurrences: u64,
    pub observed_unix_seconds: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct WarningSummary {
    pub state: EvidenceState,
    /// Number of UID-linked Warning Event records in the last 15 minutes.
    /// This is neither all namespace Events nor the sum of occurrence counters.
    pub count: Option<usize>,
    pub recent: Vec<WarningEvidence>,
    pub message: Option<String>,
}

impl WarningSummary {
    pub fn connecting() -> Self {
        Self {
            state: EvidenceState::Connecting,
            count: None,
            recent: Vec::new(),
            message: Some("Connecting to Warning Events".into()),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct HealthSummary {
    pub ready: usize,
    /// Running Pods whose readiness or container state is unhealthy.
    pub unready: usize,
    pub pending: usize,
    pub failed: usize,
    pub succeeded: usize,
    pub terminating: usize,
    pub unknown: usize,
    /// Current reported container restart counters, not a historical rate.
    pub restarts: u64,
    /// Pods with OOMKilled in current or last container termination evidence.
    pub oom_killed: usize,
    pub warnings: WarningSummary,
}

impl HealthSummary {
    pub fn unhealthy(&self) -> usize {
        self.unready + self.pending + self.failed + self.terminating + self.unknown
    }
}

pub(crate) struct PodHealth {
    pub summary: HealthSummary,
    identities: BTreeSet<(String, String, String)>,
}

impl PodHealth {
    pub fn from_objects(pods: &[Value]) -> Self {
        let mut summary = HealthSummary {
            ready: 0,
            unready: 0,
            pending: 0,
            failed: 0,
            succeeded: 0,
            terminating: 0,
            unknown: 0,
            restarts: 0,
            oom_killed: 0,
            warnings: WarningSummary::connecting(),
        };
        let mut identities = BTreeSet::new();
        for pod in pods {
            if let Some(identity) = identity(&pod["metadata"]) {
                identities.insert(identity);
            }
            let mut oom = false;
            for field in [
                "containerStatuses",
                "initContainerStatuses",
                "ephemeralContainerStatuses",
            ] {
                for status in pod["status"][field].as_array().into_iter().flatten() {
                    summary.restarts = summary
                        .restarts
                        .saturating_add(status["restartCount"].as_u64().unwrap_or(0));
                    oom |= status["state"]["terminated"]["reason"] == "OOMKilled"
                        || status["lastState"]["terminated"]["reason"] == "OOMKilled";
                }
            }
            summary.oom_killed += usize::from(oom);
            if pod["metadata"]["deletionTimestamp"].is_string() {
                summary.terminating += 1;
                continue;
            }
            match pod["status"]["phase"].as_str() {
                Some("Succeeded") => summary.succeeded += 1,
                Some("Pending") => summary.pending += 1,
                Some("Failed") => summary.failed += 1,
                Some("Running") => {
                    let ready = crate::gateway::summary(pod) == "Running"
                        && pod["status"]["conditions"]
                            .as_array()
                            .is_some_and(|conditions| {
                                conditions.iter().any(|condition| {
                                    condition["type"] == "Ready" && condition["status"] == "True"
                                })
                            })
                        && pod["status"]["containerStatuses"]
                            .as_array()
                            .is_some_and(|statuses| {
                                !statuses.is_empty()
                                    && statuses.iter().all(|status| status["ready"] == true)
                                    && pod["spec"]["containers"].as_array().is_none_or(
                                        |containers| {
                                            containers.iter().all(|container| {
                                                statuses.iter().any(|status| {
                                                    status["name"] == container["name"]
                                                })
                                            })
                                        },
                                    )
                            });
                    if ready {
                        summary.ready += 1;
                    } else {
                        summary.unready += 1;
                    }
                }
                _ => summary.unknown += 1,
            }
        }
        Self {
            summary,
            identities,
        }
    }

    pub fn with_warnings(&self, events: &EventEvidence, now: u64) -> HealthSummary {
        let mut summary = self.summary.clone();
        summary.warnings = events.for_pods(&self.identities, now);
        summary
    }
}

fn identity(metadata: &Value) -> Option<(String, String, String)> {
    let namespace = metadata["namespace"].as_str()?;
    let name = metadata["name"].as_str()?;
    let uid = metadata["uid"].as_str()?;
    if !safety::valid_id(namespace)
        || name.len() > 253
        || !name.split('.').all(safety::valid_id)
        || uid.is_empty()
        || uid.len() > 253
        || safety::safe_text(uid, 253) != uid
    {
        return None;
    }
    Some((namespace.into(), name.into(), uid.into()))
}

struct WarningCandidate {
    identity: (String, String, String),
    reason: String,
    occurrences: u64,
    timestamp: Option<u64>,
}

pub(crate) struct EventEvidence {
    state: EvidenceState,
    message: Option<String>,
    candidates: Vec<WarningCandidate>,
}

impl EventEvidence {
    pub fn connecting() -> Self {
        Self {
            state: EvidenceState::Connecting,
            message: Some("Connecting to Warning Events".into()),
            candidates: Vec::new(),
        }
    }

    pub fn unavailable(state: EvidenceState, message: &str) -> Self {
        Self {
            state,
            message: Some(safety::safe_text(message, 200)),
            candidates: Vec::new(),
        }
    }

    pub fn from_objects(events: &[Value], truncated: bool) -> Self {
        let candidates = events
            .iter()
            .filter(|event| event["type"] == "Warning" && event["involvedObject"]["kind"] == "Pod")
            .filter_map(|event| {
                let identity = identity(&event["involvedObject"])?;
                // Namespace mismatches cannot be attributed to this Pod.
                if event["metadata"]["namespace"].as_str() != Some(identity.0.as_str()) {
                    return None;
                }
                let timestamp = [
                    &event["series"]["lastObservedTime"],
                    &event["lastTimestamp"],
                    &event["eventTime"],
                    &event["firstTimestamp"],
                ]
                .into_iter()
                .filter_map(parse_timestamp)
                .max();
                Some(WarningCandidate {
                    identity,
                    reason: safety::safe_text(event["reason"].as_str().unwrap_or("Warning"), 80),
                    occurrences: event["series"]["count"]
                        .as_u64()
                        .or_else(|| event["count"].as_u64())
                        .unwrap_or(1)
                        .max(1),
                    timestamp,
                })
            })
            .collect();
        Self {
            state: if truncated {
                EvidenceState::Truncated
            } else {
                EvidenceState::Live
            },
            message: truncated.then(|| "Event evidence truncated; narrow namespace scope".into()),
            candidates,
        }
    }

    fn for_pods(
        &self,
        identities: &BTreeSet<(String, String, String)>,
        now: u64,
    ) -> WarningSummary {
        if !matches!(self.state, EvidenceState::Live | EvidenceState::Truncated) {
            return WarningSummary {
                state: self.state,
                count: None,
                recent: Vec::new(),
                message: self.message.clone(),
            };
        }
        let mut unknown_time = false;
        let mut recent = Vec::new();
        for candidate in &self.candidates {
            if !identities.contains(&candidate.identity) {
                continue;
            }
            let Some(timestamp) = candidate.timestamp else {
                unknown_time = true;
                continue;
            };
            if timestamp > now.saturating_add(60) {
                unknown_time = true;
                continue;
            }
            if now.saturating_sub(timestamp) > WARNING_WINDOW_SECONDS {
                continue;
            }
            recent.push(WarningEvidence {
                namespace: candidate.identity.0.clone(),
                pod: candidate.identity.1.clone(),
                uid: candidate.identity.2.clone(),
                reason: candidate.reason.clone(),
                occurrences: candidate.occurrences,
                observed_unix_seconds: timestamp,
            });
        }
        recent.sort_by(|a, b| {
            b.observed_unix_seconds
                .cmp(&a.observed_unix_seconds)
                .then_with(|| a.namespace.cmp(&b.namespace))
                .then_with(|| a.pod.cmp(&b.pod))
                .then_with(|| a.reason.cmp(&b.reason))
        });
        let count = recent.len();
        recent.truncate(MAX_RECENT_WARNINGS);
        WarningSummary {
            state: if unknown_time {
                EvidenceState::Truncated
            } else {
                self.state
            },
            count: Some(count),
            recent,
            message: if unknown_time {
                Some("Some linked Warning Events have unverified timestamps".into())
            } else {
                self.message.clone()
            },
        }
    }
}

fn parse_timestamp(value: &Value) -> Option<u64> {
    let timestamp = value
        .as_str()?
        .parse::<k8s_openapi::jiff::Timestamp>()
        .ok()?;
    u64::try_from(timestamp.as_second()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pod(uid: &str) -> Value {
        json!({"kind":"Pod","metadata":{"namespace":"team","name":"api","uid":uid},"spec":{"containers":[{"name":"app"}]},"status":{"phase":"Running","conditions":[{"type":"Ready","status":"True"}],"containerStatuses":[{"name":"app","ready":true,"restartCount":2}]}})
    }
    fn event(uid: &str, timestamp: &str) -> Value {
        json!({"metadata":{"namespace":"team","name":"warning"},"type":"Warning","reason":"BackOff","message":"sensitive message must never be exported","count":7,"lastTimestamp":timestamp,"involvedObject":{"kind":"Pod","namespace":"team","name":"api","uid":uid}})
    }

    #[test]
    fn health_counts_readiness_phases_and_historical_oom_without_double_counting() {
        let mut ready = pod("ready");
        ready["status"]["containerStatuses"][0]["lastState"] =
            json!({"terminated":{"reason":"OOMKilled"}});
        ready["status"]["initContainerStatuses"] = json!([{"name":"init","restartCount":3,"state":{"terminated":{"exitCode":0,"reason":"OOMKilled"}}}]);
        let mut unready = pod("unready");
        unready["status"]["conditions"][0]["status"] = json!("False");
        let mut missing = pod("missing");
        missing["spec"]["containers"] = json!([{"name":"app"},{"name":"sidecar"}]);
        let mut items = vec![ready, unready, missing];
        for phase in ["Pending", "Failed", "Succeeded", "Unknown"] {
            let mut item = pod(phase);
            item["status"]["phase"] = json!(phase);
            items.push(item);
        }
        let mut terminating = pod("terminating");
        terminating["metadata"]["deletionTimestamp"] = json!("2026-10-23T12:00:00Z");
        items.push(terminating);
        let summary = PodHealth::from_objects(&items).summary;
        assert_eq!((summary.ready, summary.unready, summary.pending), (1, 2, 1));
        assert_eq!(
            (summary.failed, summary.succeeded, summary.unknown),
            (1, 1, 1)
        );
        assert_eq!(
            (summary.terminating, summary.oom_killed, summary.restarts),
            (1, 1, 19)
        );
        assert_eq!(summary.unhealthy(), 6);
    }

    #[test]
    fn warning_evidence_requires_exact_current_uid_namespace_and_recent_timestamp() {
        let now = parse_timestamp(&json!("2026-10-23T12:00:00Z")).unwrap_or(0);
        let pods = PodHealth::from_objects(&[pod("new-uid")]);
        let mut wrong_namespace = event("new-uid", "2026-10-23T11:59:00Z");
        wrong_namespace["metadata"]["namespace"] = json!("other");
        let mut normal = event("new-uid", "2026-10-23T11:59:00Z");
        normal["type"] = json!("Normal");
        let events = EventEvidence::from_objects(
            &[
                event("old-uid", "2026-10-23T11:59:00Z"),
                event("new-uid", "2026-10-23T11:40:00Z"),
                event("new-uid", "2026-10-23T11:59:00Z"),
                wrong_namespace,
                normal,
            ],
            false,
        );
        let health = pods.with_warnings(&events, now);
        assert_eq!(health.warnings.state, EvidenceState::Live);
        assert_eq!(health.warnings.count, Some(1));
        assert_eq!(health.warnings.recent[0].occurrences, 7);
        let encoded = serde_json::to_string(&health).unwrap_or_default();
        assert!(!encoded.contains("sensitive"));
        assert!(!encoded.contains("old-uid"));
        assert_eq!(
            pods.with_warnings(&events, now + 901).warnings.count,
            Some(0)
        );
    }

    #[test]
    fn unknown_denied_and_incomplete_events_never_look_like_complete_healthy_zero() {
        let pods = PodHealth::from_objects(&[pod("uid")]);
        let denied =
            EventEvidence::unavailable(EvidenceState::Denied, "watch authorization denied");
        let health = pods.with_warnings(&denied, 100);
        assert_eq!(health.ready, 1);
        assert_eq!(health.warnings.count, None);
        assert_eq!(health.warnings.state, EvidenceState::Denied);
        let unknown = EventEvidence::from_objects(&[event("uid", "not-a-time")], false);
        assert_eq!(
            pods.with_warnings(&unknown, 100).warnings.state,
            EvidenceState::Truncated
        );
        let truncated = EventEvidence::from_objects(&[], true);
        assert_eq!(
            pods.with_warnings(&truncated, 100).warnings.state,
            EvidenceState::Truncated
        );
        assert_eq!(
            EvidenceState::from_error("Kubernetes API 403 (Forbidden)"),
            EvidenceState::Denied
        );
        assert_eq!(
            EvidenceState::from_error("watch cache limit exceeded; narrow namespace scope"),
            EvidenceState::Truncated
        );
    }

    #[test]
    fn running_containers_without_a_ready_condition_are_not_ready_evidence() {
        let mut item = pod("uid");
        item["status"]["conditions"] = Value::Null;
        let health = PodHealth::from_objects(&[item]).summary;
        assert_eq!((health.ready, health.unready), (0, 1));
    }

    #[test]
    fn long_dns_subdomain_pod_names_keep_uid_linked_warning_evidence() {
        let name = format!("{}.{}", "a".repeat(63), "b".repeat(63));
        let mut item = pod("uid");
        item["metadata"]["name"] = json!(name);
        let mut warning = event("uid", "2026-10-23T11:59:00Z");
        warning["involvedObject"]["name"] = json!(name);
        let now = parse_timestamp(&json!("2026-10-23T12:00:00Z")).unwrap_or(0);
        let health = PodHealth::from_objects(&[item])
            .with_warnings(&EventEvidence::from_objects(&[warning], false), now);
        assert_eq!(health.warnings.count, Some(1));
    }

    #[test]
    fn warnings_are_bounded_ordered_and_sum_occurrences_separately() {
        let pods = PodHealth::from_objects(&[pod("uid")]);
        let mut events = Vec::new();
        for second in 0..10 {
            let mut item = event("uid", &format!("2026-10-23T11:59:{second:02}Z"));
            item["reason"] = json!(format!("Reason{second}"));
            events.push(item);
        }
        let now = parse_timestamp(&json!("2026-10-23T12:00:00Z")).unwrap_or(0);
        let health = pods.with_warnings(&EventEvidence::from_objects(&events, false), now);
        assert_eq!(health.warnings.count, Some(10));
        assert_eq!(health.warnings.recent.len(), 5);
        assert_eq!(health.warnings.recent[0].reason, "Reason9");
    }
}
