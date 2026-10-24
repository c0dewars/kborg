//! Explicit, bounded, independent read-only context health snapshots and watches.
use crate::{
    Result,
    dashboard_health::{EventEvidence, EvidenceState, HealthSummary, PodHealth},
    gateway::{Connection, Gateway},
    safety,
};
use futures_util::{StreamExt, stream};
use serde::Serialize;
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Serialize)]
pub struct ContextSummary {
    pub demo: bool,
    pub context: String,
    pub namespace: String,
    pub pods: Option<usize>,
    pub unhealthy: Option<usize>,
    pub truncated: bool,
    pub error: Option<String>,
    pub state: EvidenceState,
    pub observed_unix_seconds: Option<u64>,
    pub health: Option<HealthSummary>,
    /// Node and workload readiness from the dashboard's bounded poll; not
    /// part of the versioned CLI overview projection.
    #[serde(skip)]
    pub infra: Option<crate::cluster_health::InfraSummary>,
}

impl ContextSummary {
    pub fn connecting(context: String, demo: bool) -> Self {
        Self {
            demo,
            context,
            namespace: String::new(),
            pods: None,
            unhealthy: None,
            truncated: false,
            error: Some("Connecting to Pods".into()),
            state: EvidenceState::Connecting,
            observed_unix_seconds: None,
            health: None,
            infra: None,
        }
    }
    fn fail(&mut self, message: &str) {
        self.error = Some(safety::safe_text(message, 200));
        self.state = EvidenceState::from_error(message);
        self.pods = None;
        self.unhealthy = None;
        self.health = None;
    }
    fn set_health(&mut self, pods: &PodHealth, events: &EventEvidence) {
        let health = pods.with_warnings(events, unix_seconds());
        self.unhealthy = Some(health.unhealthy());
        self.health = Some(health);
    }
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn read_only(options: &Connection, context: &str) -> Connection {
    let mut connection = options.clone();
    connection.context = Some(context.into());
    connection.writable = false;
    connection.logs = false;
    connection.allow_exec = false;
    connection.allow_attach = false;
    connection.allow_port_forward = false;
    connection.allow_file_copy = false;
    connection.allow_apply = false;
    connection.allow_node_maintenance = false;
    connection.allow_yaml_edit = false;
    connection.allow_force_kill = false;
    connection.allow_secret_reveal = false;
    connection.allow_helm_write = false;
    connection.allow_helm_hooks = false;
    connection.allow_helm_crds = false;
    connection
}

/// Contexts one live dashboard watches at once (k10s keys 1..9).
pub const MAX_LIVE_CONTEXTS: usize = 9;

pub fn validate(names: &[String]) -> Result<()> {
    if names.is_empty() || names.len() > 100 {
        return Err("overview requires 1..100 explicit contexts".into());
    }
    let mut seen = std::collections::HashSet::new();
    for name in names {
        if name.is_empty()
            || name.len() > 253
            || safety::safe_text(name, 253) != *name
            || !seen.insert(name)
        {
            return Err("overview context names must be unique bounded plain text".into());
        }
    }
    Ok(())
}

fn events_from_list(result: Result<Value>, pods: &[Value], demo: bool) -> EventEvidence {
    match result {
        Ok(mut list) => {
            let truncated = list["truncated"] == true;
            let Some(events) = list["items"].as_array_mut() else {
                return EventEvidence::unavailable(
                    EvidenceState::Unavailable,
                    "Invalid Event list",
                );
            };
            if demo {
                // These are explicitly synthetic fixtures; keep their event identity and
                // recency coherent without altering live cluster evidence.
                for event in events.iter_mut() {
                    if let Some(pod) = pods.iter().find(|pod| {
                        pod["metadata"]["namespace"] == event["involvedObject"]["namespace"]
                            && pod["metadata"]["name"] == event["involvedObject"]["name"]
                    }) {
                        event["involvedObject"]["uid"] = pod["metadata"]["uid"].clone();
                        event["lastTimestamp"] =
                            Value::String(k8s_openapi::jiff::Timestamp::now().to_string());
                    }
                }
            }
            EventEvidence::from_objects(events, truncated)
        }
        Err(error) => EventEvidence::unavailable(EvidenceState::from_error(&error), &error),
    }
}

async fn collect_context(options: Connection, context: String) -> ContextSummary {
    let mut row = ContextSummary::connecting(context, options.demo);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let gateway = match tokio::time::timeout_at(deadline, Gateway::connect(options)).await {
        Ok(Ok(gateway)) => gateway,
        result => {
            row.fail(&match result {
                Ok(Err(error)) => error,
                _ => "Context connection timed out".into(),
            });
            return row;
        }
    };
    row.namespace = gateway.namespace.clone();
    // A denied/slow Event source must never discard a successful Pod observation.
    let gateway_ref = &gateway;
    let read = |kind| async move {
        tokio::time::timeout_at(deadline, gateway_ref.list(kind, &gateway_ref.namespace))
            .await
            .map_err(|_| "Context snapshot budget exceeded".to_owned())?
    };
    let (pods, events) = tokio::join!(read("pods"), read("events"));
    match pods {
        Ok(mut list) => {
            if row.demo {
                for pod in list["items"].as_array_mut().into_iter().flatten() {
                    let ready = crate::gateway::summary(pod) == "Running"
                        && pod["status"]["containerStatuses"]
                            .as_array()
                            .is_some_and(|items| {
                                !items.is_empty() && items.iter().all(|item| item["ready"] == true)
                            });
                    pod["status"]["conditions"] = serde_json::json!([{
                        "type":"Ready", "status": if ready { "True" } else { "False" }
                    }]);
                }
            }
            if let Some(items) = list["items"].as_array() {
                row.pods = Some(items.len());
                row.truncated = list["truncated"] == true;
                row.state = if row.truncated {
                    EvidenceState::Truncated
                } else {
                    EvidenceState::Live
                };
                row.error = None;
                row.observed_unix_seconds = Some(unix_seconds());
                row.set_health(
                    &PodHealth::from_objects(items),
                    &events_from_list(events, items, row.demo),
                );
            } else {
                row.fail("Invalid Pod list");
            }
        }
        Err(error) => row.fail(&error),
    }
    row
}

pub async fn collect(options: &Connection, names: &[String]) -> Result<Vec<ContextSummary>> {
    validate(names)?;
    let mut pending = stream::iter(
        names
            .iter()
            .cloned()
            .map(|name| collect_context(read_only(options, &name), name)),
    )
    .buffer_unordered(4);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut rows = Vec::new();
    while let Ok(Some(row)) = tokio::time::timeout_at(deadline, pending.next()).await {
        rows.push(row);
    }
    drop(pending);
    for name in names {
        if !rows.iter().any(|row| &row.context == name) {
            let mut row = ContextSummary::connecting(name.clone(), options.demo);
            row.fail("Overview 30-second global budget exceeded; narrow the selected contexts");
            rows.push(row);
        }
    }
    rows.sort_by(|a, b| a.context.cmp(&b.context));
    Ok(rows)
}

#[cfg(test)]
fn pod_unhealthy(pod: &Value) -> bool {
    PodHealth::from_objects(std::slice::from_ref(pod))
        .summary
        .unhealthy()
        > 0
}

pub fn text(rows: &[ContextSummary]) -> String {
    let mut output = String::from(
        "READ ONLY | independent context snapshots (maximum 4 simultaneous contexts)\nCONTEXT\tNAMESPACE\tPODS\tUNHEALTHY\tSTATE\tREADY\tRESTARTS\tOOM PODS\tWARNINGS 15m\n",
    );
    if rows.iter().any(|row| row.demo) {
        output.insert_str(0, "DEMO: synthetic samples; no cluster contacted\n");
    }
    for row in rows {
        let number = |value: Option<usize>| value.map_or("-".into(), |v| v.to_string());
        let warnings = row.health.as_ref().map_or("-".into(), |health| {
            let count = number(health.warnings.count);
            if health.warnings.state == EvidenceState::Live {
                count
            } else {
                format!("{count} ({:?})", health.warnings.state)
            }
        });
        output.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            row.context,
            row.namespace,
            number(row.pods),
            number(row.unhealthy),
            row.error.as_deref().unwrap_or(if row.truncated {
                "TRUNCATED"
            } else {
                "snapshot"
            }),
            number(row.health.as_ref().map(|health| health.ready)),
            row.health
                .as_ref()
                .map_or("-".into(), |health| health.restarts.to_string()),
            number(row.health.as_ref().map(|health| health.oom_killed)),
            warnings,
        ));
    }
    output.push_str("Restarts are current container counters; OOM is current/last termination evidence. Warning records match current Pod UIDs within 15m.\n");
    output
}

pub fn live_text(rows: &[ContextSummary]) -> String {
    text(rows)
        .replace(
            "independent context snapshots (maximum 4 simultaneous contexts)",
            "LIVE independent Pod/Event watches (maximum 4 contexts)",
        )
        .replace("\tsnapshot\t", "\tLIVE\t")
}

/// Four independent contexts, each with bounded Pod/Event watches. One coalesced
/// mailbox stores projections only. Dropping the feed aborts every connection.
pub struct OverviewFeed {
    rows: std::sync::Arc<std::sync::Mutex<Vec<ContextSummary>>>,
    changed: std::sync::Arc<tokio::sync::Notify>,
    jobs: Vec<tokio::task::JoinHandle<()>>,
    initial: bool,
    generation: std::sync::Arc<std::sync::atomic::AtomicU64>,
    consumed: std::cell::Cell<u64>,
}
impl OverviewFeed {
    pub fn new(options: &Connection, names: &[String]) -> Result<Self> {
        validate(names)?;
        if names.len() > MAX_LIVE_CONTEXTS {
            return Err(format!(
                "live overview supports 1..{MAX_LIVE_CONTEXTS} explicitly selected contexts"
            ));
        }
        let rows = std::sync::Arc::new(std::sync::Mutex::new(
            names
                .iter()
                .map(|context| ContextSummary::connecting(context.clone(), options.demo))
                .collect::<Vec<_>>(),
        ));
        let changed = std::sync::Arc::new(tokio::sync::Notify::new());
        let generation = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let mut jobs = Vec::new();
        for (index, name) in names.iter().enumerate() {
            let connection = read_only(options, name);
            let mut row = ContextSummary::connecting(name.clone(), connection.demo);
            let slot = rows.clone();
            let signal = changed.clone();
            let revision = generation.clone();
            jobs.push(tokio::spawn(async move {
                let publish = |row: &ContextSummary| {
                    if let Ok(mut rows) = slot.lock() {
                        rows[index] = row.clone();
                    }
                    revision.fetch_add(1, std::sync::atomic::Ordering::Release);
                    signal.notify_one();
                };
                let gateway = match tokio::time::timeout(Duration::from_secs(20), Gateway::connect(connection.clone())).await {
                    Ok(Ok(gateway)) => gateway,
                    result => {
                        row.fail(&match result {
                            Ok(Err(error)) => error,
                            _ => "Connection timed out; other contexts remain independent".into(),
                        });
                        publish(&row);
                        return;
                    }
                };
                row.namespace = gateway.namespace.clone();
                publish(&row);
                if gateway.is_demo() {
                    loop {
                        row = collect_context(connection.clone(), row.context.clone()).await;
                        row.infra = Some(crate::cluster_health::collect(&gateway, &gateway.namespace).await);
                        publish(&row);
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
                let pod_feed = match gateway.watch("pods", &gateway.namespace) {
                    Ok(feed) => feed,
                    Err(error) => {
                        row.fail(&error);
                        publish(&row);
                        return;
                    }
                };
                let mut events = EventEvidence::connecting();
                let event_feed = match gateway.watch("events", &gateway.namespace) {
                    Ok(feed) => Some(feed),
                    Err(error) => {
                        events = EventEvidence::unavailable(EvidenceState::from_error(&error), &error);
                        None
                    }
                };
                // Keep each in-flight LIST/WATCH future inside its own stream. A
                // faster peer or the five-second clock must not cancel/restart
                // a slower initial list and starve that source indefinitely.
                let snapshots = |feed| {
                    Box::pin(stream::unfold(Some(feed), |feed| async {
                        let mut feed: crate::watch::WatchFeed = feed?;
                        let snapshot = feed.next().await;
                        let next = (!snapshot.terminal).then_some(feed);
                        Some((snapshot, next))
                    }))
                };
                let mut pod_stream = snapshots(pod_feed);
                let mut event_stream = event_feed.map(snapshots);
                let mut pods: Option<PodHealth> = None;
                let mut pod_terminal = false;
                let mut tick = tokio::time::interval(Duration::from_secs(5));
                // Node/workload readiness is polled separately so a slow or
                // denied read never delays the Pod and Event watches.
                let infra_namespace = gateway.namespace.clone();
                let infra = |delay: u64| {
                    let gateway = &gateway;
                    let namespace = &infra_namespace;
                    Box::pin(async move {
                        tokio::time::sleep(Duration::from_secs(delay)).await;
                        crate::cluster_health::collect(gateway, namespace).await
                    })
                };
                let mut infra_poll = infra(0);
                loop {
                    tokio::select! {
                        summary = &mut infra_poll => {
                            row.infra = Some(summary);
                            infra_poll = infra(15);
                        },
                        Some(snapshot) = pod_stream.next(), if !pod_terminal => {
                            pod_terminal = snapshot.terminal;
                            row.truncated = snapshot.truncated;
                            if snapshot.stale {
                                pods = None;
                                row.fail(&snapshot.message);
                            } else {
                                row.pods = Some(snapshot.objects.len());
                                row.state = if snapshot.truncated { EvidenceState::Truncated } else { EvidenceState::Live };
                                row.error = None;
                                row.observed_unix_seconds = Some(unix_seconds());
                                pods = Some(PodHealth::from_objects(&snapshot.objects));
                            }
                        },
                        snapshot = async {
                            match event_stream.as_mut() {
                                Some(feed) => feed.next().await,
                                None => std::future::pending().await,
                            }
                        } => {
                            let Some(snapshot) = snapshot else {
                                event_stream = None;
                                continue;
                            };
                            events = if snapshot.stale {
                                EventEvidence::unavailable(EvidenceState::from_error(&snapshot.message), &snapshot.message)
                            } else {
                                EventEvidence::from_objects(&snapshot.objects, snapshot.truncated)
                            };
                            if snapshot.terminal {
                                event_stream = None;
                            }
                        },
                        _ = tick.tick() => {},
                    }
                    if let Some(pods) = &pods {
                        row.set_health(pods, &events);
                    }
                    publish(&row);
                    if pod_terminal && event_stream.is_none() {
                        break;
                    }
                }
            }));
        }
        Ok(Self {
            rows,
            changed,
            jobs,
            initial: true,
            generation,
            consumed: std::cell::Cell::new(u64::MAX),
        })
    }
    pub async fn next(&mut self) -> Result<Vec<ContextSummary>> {
        if self.initial {
            self.initial = false;
        } else {
            self.changed.notified().await;
        }
        let mut rows = self
            .rows
            .lock()
            .map_err(|_| "overview mailbox unavailable")?
            .clone();
        rows.sort_by(|a, b| a.context.cmp(&b.context));
        Ok(rows)
    }
    pub fn take_update(&self) -> Result<Option<Vec<ContextSummary>>> {
        let revision = self.generation.load(std::sync::atomic::Ordering::Acquire);
        if self.consumed.get() == revision {
            return Ok(None);
        }
        let rows = self.snapshot()?;
        self.consumed.set(revision);
        Ok(Some(rows))
    }
    pub fn snapshot(&self) -> Result<Vec<ContextSummary>> {
        self.rows
            .lock()
            .map(|rows| rows.clone())
            .map_err(|_| "overview mailbox unavailable".into())
    }
}
impl Drop for OverviewFeed {
    fn drop(&mut self) {
        for job in &self.jobs {
            job.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_or_denied_pods_clear_every_health_value_and_keep_last_observation_time() {
        let mut row = ContextSummary::connecting("example".into(), false);
        row.pods = Some(1);
        row.unhealthy = Some(0);
        row.observed_unix_seconds = Some(123);
        row.set_health(
            &PodHealth::from_objects(&[]),
            &EventEvidence::from_objects(&[], false),
        );
        row.fail("watch authorization denied; cache cleared");
        assert_eq!(row.state, EvidenceState::Denied);
        assert!(row.health.is_none() && row.pods.is_none() && row.unhealthy.is_none());
        assert_eq!(row.observed_unix_seconds, Some(123));
    }

    #[test]
    fn dashboard_connections_strip_all_mutation_and_stream_capabilities() {
        let options = Connection {
            writable: true,
            logs: true,
            allow_exec: true,
            allow_attach: true,
            allow_port_forward: true,
            allow_node_maintenance: true,
            allow_yaml_edit: true,
            allow_helm_write: true,
            allow_helm_hooks: true,
            allow_helm_crds: true,
            ..Default::default()
        };
        let scoped = read_only(&options, "isolated");
        assert_eq!(scoped.context.as_deref(), Some("isolated"));
        assert!(!scoped.writable && !scoped.logs && !scoped.allow_exec && !scoped.allow_attach);
        assert!(
            !scoped.allow_port_forward && !scoped.allow_node_maintenance && !scoped.allow_yaml_edit
        );
        assert!(!scoped.allow_helm_write && !scoped.allow_helm_hooks && !scoped.allow_helm_crds);
        assert!(options.writable && options.allow_yaml_edit);
    }

    #[test]
    fn live_overview_bounds_contexts_and_keeps_scope_isolated() -> Result<()> {
        crate::cli::runtime()?.block_on(async {
            let options = Connection {
                demo: true,
                writable: true,
                logs: true,
                allow_exec: true,
                ..Default::default()
            };
            assert!(
                OverviewFeed::new(
                    &options,
                    &(0..10).map(|index| format!("c{index}")).collect::<Vec<_>>()
                )
                .is_err()
            );
            let mut feed = OverviewFeed::new(&options, &["b".into(), "a".into()])?;
            let initial = feed.next().await?;
            assert_eq!(initial.len(), 2);
            assert!(initial.iter().all(|row| row.pods.is_none()));
            let rows = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let rows = feed.next().await?;
                    if rows.iter().all(|row| row.pods.is_some()) {
                        return Ok::<_, String>(rows);
                    }
                }
            })
            .await
            .map_err(|_| "overview fixtures timed out")??;
            assert!(
                rows.iter()
                    .all(|row| row.demo && row.pods == Some(2) && row.unhealthy == Some(1))
            );
            assert_eq!(rows[0].context, "a");
            assert_eq!(feed.jobs.len(), 2);
            let handles: Vec<_> = feed.jobs.iter().map(|job| job.abort_handle()).collect();
            drop(feed);
            tokio::time::timeout(Duration::from_secs(2), async {
                while !handles.iter().all(tokio::task::AbortHandle::is_finished) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .map_err(|_| "overview cancellation timed out")?;
            Ok(())
        })
    }
    #[test]
    fn unhealthy_includes_readiness_gates_terminating_and_unknown_status() {
        use serde_json::json;
        assert!(pod_unhealthy(
            &json!({"kind":"Pod","status":{"phase":"Running"}})
        ));
        assert!(pod_unhealthy(
            &json!({"kind":"Pod","status":{"phase":"Running","containerStatuses":[{"ready":true}],"conditions":[{"type":"Ready","status":"False"}]}})
        ));
        assert!(pod_unhealthy(
            &json!({"kind":"Pod","metadata":{"deletionTimestamp":"now"},"status":{"phase":"Succeeded"}})
        ));
        assert!(!pod_unhealthy(
            &json!({"kind":"Pod","status":{"phase":"Succeeded"}})
        ));
        assert!(!pod_unhealthy(
            &json!({"kind":"Pod","status":{"phase":"Running","containerStatuses":[{"ready":true}],"conditions":[{"type":"Ready","status":"True"}]}})
        ));
    }
    #[test]
    fn rejects_ambiguous_and_unbounded_scope() {
        assert!(validate(&[]).is_err());
        assert!(validate(&vec!["a".into(); 101]).is_err());
        assert!(validate(&["a".into(), "a".into()]).is_err());
        assert!(validate(&["bad\u{1b}".into()]).is_err());
    }
    #[test]
    fn snapshots_are_read_only_and_errors_are_not_zeroes() {
        crate::cli::runtime()
            .unwrap_or_else(|e| panic!("{e}"))
            .block_on(async {
                let rows = collect(
                    &Connection {
                        demo: true,
                        writable: true,
                        ..Default::default()
                    },
                    &["demo".into()],
                )
                .await
                .unwrap_or_else(|e| panic!("{e}"));
                assert!(rows[0].demo);
                assert!(text(&rows).starts_with("DEMO:"));
                assert_eq!(rows[0].pods, Some(2));
                assert_eq!(rows[0].unhealthy, Some(1));
            });
    }
}
