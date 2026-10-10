//! Explicit, bounded, independent read-only context snapshots.
use crate::{
    Result,
    gateway::{Connection, Gateway},
    safety,
};
use futures_util::{StreamExt, stream};
use serde::Serialize;
use std::time::Duration;

#[derive(Clone, Serialize)]
pub struct ContextSummary {
    pub demo: bool,
    pub context: String,
    pub namespace: String,
    pub pods: Option<usize>,
    pub unhealthy: Option<usize>,
    pub truncated: bool,
    pub error: Option<String>,
}

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

pub async fn collect(options: &Connection, names: &[String]) -> Result<Vec<ContextSummary>> {
    validate(names)?;
    // Each connection owns its identity and transport. No credentials, object caches,
    // or writes are shared; release each object list before retaining the summary.
    let mut pending = stream::iter(names.iter().cloned().map(|name| {
        let mut connection = options.clone();
        connection.context = Some(name.clone());
        connection.writable = false;
        connection.logs = false;
        connection.allow_exec = false;
        connection.allow_attach = false;
        connection.allow_port_forward = false;
        async move {
            let result = tokio::time::timeout(Duration::from_secs(20), async {
                let gateway = Gateway::connect(connection).await?;
                let list = gateway.list("pods", &gateway.namespace).await?;
                let items = list["items"].as_array().ok_or("invalid pod list")?;
                let unhealthy = items.iter().filter(|pod| pod_unhealthy(pod)).count();
                Ok::<_, String>((
                    gateway.namespace,
                    items.len(),
                    unhealthy,
                    list["truncated"] == true,
                ))
            })
            .await;
            match result {
                Ok(Ok((namespace, pods, unhealthy, truncated))) => ContextSummary {
                    demo: options.demo,
                    context: name,
                    namespace,
                    pods: Some(pods),
                    unhealthy: Some(unhealthy),
                    truncated,
                    error: None,
                },
                other => ContextSummary {
                    demo: options.demo,
                    context: name,
                    namespace: String::new(),
                    pods: None,
                    unhealthy: None,
                    truncated: false,
                    error: Some(match other {
                        Ok(Err(e)) => safety::safe_text(&e, 200),
                        _ => "context timed out".into(),
                    }),
                },
            }
        }
    }))
    .buffer_unordered(4);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut rows = Vec::new();
    while let Ok(Some(row)) = tokio::time::timeout_at(deadline, pending.next()).await {
        rows.push(row);
    }
    drop(pending);
    for name in names {
        if !rows.iter().any(|row| &row.context == name) {
            rows.push(ContextSummary {
                demo: options.demo,
                context: name.clone(),
                namespace: String::new(),
                pods: None,
                unhealthy: None,
                truncated: false,
                error: Some(
                    "overview 30-second global budget exceeded; narrow the selected contexts"
                        .into(),
                ),
            });
        }
    }
    rows.sort_by(|a, b| a.context.cmp(&b.context));
    Ok(rows)
}

fn pod_unhealthy(pod: &serde_json::Value) -> bool {
    if pod["metadata"]["deletionTimestamp"].is_string() {
        return true;
    }
    if pod["status"]["phase"] == "Succeeded" {
        return false;
    }
    crate::gateway::summary(pod) != "Running"
        || pod["status"]["containerStatuses"]
            .as_array()
            .is_none_or(|statuses| {
                statuses.is_empty() || statuses.iter().any(|status| status["ready"] != true)
            })
}

pub fn text(rows: &[ContextSummary]) -> String {
    let mut output = String::from(
        "READ ONLY | independent context snapshots (maximum 4 simultaneous requests)\nCONTEXT\tNAMESPACE\tPODS\tUNHEALTHY\tSTATE\n",
    );
    if rows.iter().any(|row| row.demo) {
        output.insert_str(0, "DEMO: synthetic samples; no cluster contacted\n");
    }
    for row in rows {
        output.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\n",
            row.context,
            row.namespace,
            row.pods.map_or("-".into(), |v| v.to_string()),
            row.unhealthy.map_or("-".into(), |v| v.to_string()),
            row.error.as_deref().unwrap_or(if row.truncated {
                "TRUNCATED"
            } else {
                "snapshot"
            })
        ));
    }
    output
}

pub fn live_text(rows: &[ContextSummary]) -> String {
    text(rows)
        .replace(
            "independent context snapshots (maximum 4 simultaneous requests)",
            "LIVE independent pod watches (maximum 4 contexts)",
        )
        .replace("\tsnapshot\n", "\tLIVE\n")
}

/// Four independent watches, one coalesced mailbox, no credentials/objects exported.
/// Dropping the feed aborts every connection and releases each bounded watch cache.
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
        if names.len() > 4 {
            return Err("live overview supports 1..4 explicitly selected contexts".into());
        }
        let rows = std::sync::Arc::new(std::sync::Mutex::new(
            names
                .iter()
                .map(|context| ContextSummary {
                    demo: options.demo,
                    context: context.clone(),
                    namespace: String::new(),
                    pods: None,
                    unhealthy: None,
                    truncated: false,
                    error: Some("CONNECTING".into()),
                })
                .collect::<Vec<_>>(),
        ));
        let changed = std::sync::Arc::new(tokio::sync::Notify::new());
        let generation = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let mut jobs = Vec::new();
        for (index, name) in names.iter().enumerate() {
            let mut connection = options.clone();
            connection.context = Some(name.clone());
            connection.writable = false;
            connection.logs = false;
            connection.allow_exec = false;
            connection.allow_attach = false;
            connection.allow_port_forward = false;
            let context = name.clone();
            let slot = rows.clone();
            let signal = changed.clone();
            let revision = generation.clone();
            jobs.push(tokio::spawn(async move {
                let publish = |namespace: String,
                               pods: Option<usize>,
                               unhealthy: Option<usize>,
                               truncated: bool,
                               error: Option<String>| {
                    if let Ok(mut rows) = slot.lock() {
                        rows[index] = ContextSummary {
                            demo: connection.demo,
                            context: context.clone(),
                            namespace,
                            pods,
                            unhealthy,
                            truncated,
                            error,
                        };
                    }
                    revision.fetch_add(1, std::sync::atomic::Ordering::Release);
                    signal.notify_one();
                };
                let gateway = match tokio::time::timeout(
                    Duration::from_secs(20),
                    Gateway::connect(connection.clone()),
                )
                .await
                {
                    Ok(Ok(gateway)) => gateway,
                    Ok(Err(error)) => {
                        publish(
                            String::new(),
                            None,
                            None,
                            false,
                            Some(safety::safe_text(&error, 200)),
                        );
                        return;
                    }
                    Err(_) => {
                        publish(
                            String::new(),
                            None,
                            None,
                            false,
                            Some("connection timed out; other contexts remain independent".into()),
                        );
                        return;
                    }
                };
                if gateway.is_demo() {
                    loop {
                        match gateway.list("pods", &gateway.namespace).await {
                            Ok(list) => {
                                let items = list["items"].as_array();
                                publish(
                                    gateway.namespace.clone(),
                                    items.map(Vec::len),
                                    items.map(|items| {
                                        items.iter().filter(|pod| pod_unhealthy(pod)).count()
                                    }),
                                    list["truncated"] == true,
                                    None,
                                );
                            }
                            Err(error) => publish(
                                gateway.namespace.clone(),
                                None,
                                None,
                                false,
                                Some(safety::safe_text(&error, 200)),
                            ),
                        }
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
                let mut feed = match gateway.watch("pods", &gateway.namespace) {
                    Ok(feed) => feed,
                    Err(error) => {
                        publish(
                            gateway.namespace.clone(),
                            None,
                            None,
                            false,
                            Some(safety::safe_text(&error, 200)),
                        );
                        return;
                    }
                };
                loop {
                    let snapshot = feed.next().await;
                    let unhealthy = snapshot
                        .objects
                        .iter()
                        .filter(|pod| pod_unhealthy(pod))
                        .count();
                    // Failed or revoked watches report unknown counts, never healthy zeroes.
                    publish(
                        gateway.namespace.clone(),
                        (!snapshot.stale).then_some(snapshot.objects.len()),
                        (!snapshot.stale).then_some(unhealthy),
                        snapshot.truncated,
                        snapshot
                            .stale
                            .then(|| safety::safe_text(&snapshot.message, 200)),
                    );
                    if snapshot.terminal {
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
                    &["a".into(), "b".into(), "c".into(), "d".into(), "e".into()]
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
            &json!({"kind":"Pod","status":{"phase":"Running","containerStatuses":[{"ready":true}]}})
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
