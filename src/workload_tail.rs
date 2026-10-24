//! Cross-cluster workload log tail for the dashboard (k10s-style). Each
//! selected workload is followed over its own read-only connection with
//! dynamic membership; lines from every cluster are merged in timestamp order
//! through a short reorder window and tagged with their context.
use crate::{
    Result,
    gateway::{Connection, Gateway},
    logs::{FollowOptions, PodLogSelection, ReadOptions},
};
use std::{
    cmp::Reverse,
    collections::{BTreeMap, BinaryHeap},
    time::{Duration, Instant},
};

/// Workloads one tail may follow.
pub const MAX_SOURCES: usize = 6;
/// Pods across every selected workload and cluster.
pub const MAX_TOTAL_PODS: usize = 24;
/// Workloads listed per context in the picker.
pub const MAX_LISTED: usize = 200;
/// Lines wait this long so a slightly later line from another cluster can
/// still be placed before them.
pub const REORDER_WINDOW: Duration = Duration::from_millis(1000);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkloadRef {
    pub context: String,
    /// "deployments" or "statefulsets"
    pub resource: String,
    pub namespace: String,
    pub name: String,
    pub uid: String,
}
impl WorkloadRef {
    pub fn label(&self) -> String {
        let kind = if self.resource == "statefulsets" {
            "sts"
        } else {
            "deploy"
        };
        format!("{}/{}/{kind}/{}", self.context, self.namespace, self.name)
    }
}

/// Deployments and StatefulSets visible to one context, in name order.
pub async fn list_workloads(connection: Connection, context: String) -> Result<Vec<WorkloadRef>> {
    let gateway = tokio::time::timeout(Duration::from_secs(20), Gateway::connect(connection))
        .await
        .map_err(|_| "connection timed out")??;
    let namespace = gateway.namespace.clone();
    let mut found = Vec::new();
    for resource in ["deployments", "statefulsets"] {
        let list =
            tokio::time::timeout(Duration::from_secs(10), gateway.list(resource, &namespace))
                .await
                .map_err(|_| format!("{resource} list timed out"))??;
        for item in list["items"].as_array().into_iter().flatten() {
            let field = |name: &str| item["metadata"][name].as_str().unwrap_or("").to_owned();
            let (name, uid, ns) = (field("name"), field("uid"), field("namespace"));
            if name.is_empty() || uid.is_empty() || !crate::safety::valid_id(&ns) {
                continue;
            }
            found.push(WorkloadRef {
                context: context.clone(),
                resource: resource.into(),
                namespace: ns,
                name: crate::safety::safe_text(&name, 253),
                uid: crate::safety::safe_text(&uid, 64),
            });
        }
    }
    found.sort_by(|a, b| (&a.namespace, &a.name).cmp(&(&b.namespace, &b.name)));
    found.truncate(MAX_LISTED);
    Ok(found)
}

/// Inserts the context tag after a leading timestamp so the log view's
/// timestamp toggle still works.
pub fn tag(context: &str, line: &str) -> String {
    match line.split_once(' ') {
        Some((stamp, rest)) if crate::metrics::timestamp_key(stamp).is_some() => {
            format!("{stamp} [{context}] {rest}")
        }
        _ => format!("[{context}] {line}"),
    }
}

type Key = (i64, String);

/// Timestamp-ordered merge with a bounded wait. Lines without a timestamp
/// (stream notices) inherit their source's last timestamp so they stay next
/// to the records they describe.
#[derive(Default)]
pub struct Reorder {
    heap: BinaryHeap<Reverse<(Key, u64, String)>>,
    arrived: BTreeMap<u64, Instant>,
    last: BTreeMap<String, Key>,
    sequence: u64,
}
impl Reorder {
    pub fn push(&mut self, source: &str, line: String, now: Instant) {
        let key = line
            .split_once(' ')
            .and_then(|(stamp, _)| crate::metrics::timestamp_key(stamp));
        let key = match key {
            Some(key) => {
                self.last.insert(source.to_owned(), key.clone());
                key
            }
            None => self
                .last
                .get(source)
                .cloned()
                .unwrap_or((i64::MIN, String::new())),
        };
        self.sequence += 1;
        self.arrived.insert(self.sequence, now);
        self.heap.push(Reverse((key, self.sequence, line)));
    }
    /// Lines that have waited the full window, oldest timestamp first.
    pub fn ready(&mut self, now: Instant) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(Reverse((_, sequence, _))) = self.heap.peek() {
            let waited = self
                .arrived
                .get(sequence)
                .is_none_or(|at| now.duration_since(*at) >= REORDER_WINDOW);
            if !waited {
                break;
            }
            if let Some(Reverse((_, sequence, line))) = self.heap.pop() {
                self.arrived.remove(&sequence);
                out.push(line);
            }
        }
        out
    }
    pub fn drain(&mut self) -> Vec<String> {
        self.arrived.clear();
        std::iter::from_fn(|| self.heap.pop().map(|Reverse((_, _, line))| line)).collect()
    }
}

/// One selected workload and the read-only connection for its context.
pub struct TailSource {
    pub connection: Connection,
    pub workload: WorkloadRef,
}

async fn open(
    source: TailSource,
    follow: FollowOptions,
    read: ReadOptions,
) -> Result<(String, usize, crate::logs::DynamicLogFeed)> {
    let workload = source.workload;
    let gateway =
        tokio::time::timeout(Duration::from_secs(20), Gateway::connect(source.connection))
            .await
            .map_err(|_| "connection timed out")??;
    let selection = PodLogSelection::Workload {
        resource: workload.resource.clone(),
        name: workload.name.clone(),
        uid: workload.uid.clone(),
    };
    let membership = tokio::time::timeout(
        Duration::from_secs(20),
        gateway.capture_log_membership(&workload.namespace, selection.clone()),
    )
    .await
    .map_err(|_| "membership capture timed out")??
    .into_dynamic(selection);
    let pods = membership.pods().len();
    let feed = tokio::time::timeout(
        Duration::from_secs(25),
        gateway.follow_dynamic_logs(&membership, follow, read),
    )
    .await
    .map_err(|_| "log open timed out")??;
    Ok((workload.context, pods, feed))
}

/// Opened streams plus the notices describing each source's outcome.
pub struct Opened {
    pub notices: Vec<String>,
    pub feeds: Vec<(String, crate::logs::DynamicLogFeed)>,
}

/// Opens every source concurrently. Failures become notices; the combined
/// Pod count is capped so a broad selection cannot fan out without bound.
pub async fn open_all(
    sources: Vec<TailSource>,
    follow: FollowOptions,
    read: ReadOptions,
) -> Opened {
    let mut notices = Vec::new();
    if sources.is_empty() || sources.len() > MAX_SOURCES {
        notices.push(format!("[Tail refused: select 1..{MAX_SOURCES} workloads]"));
        return Opened {
            notices,
            feeds: Vec::new(),
        };
    }
    let labels = sources
        .iter()
        .map(|s| s.workload.label())
        .collect::<Vec<_>>();
    let opened = futures_util::future::join_all(
        sources.into_iter().map(|source| open(source, follow, read)),
    )
    .await;
    let mut feeds = Vec::new();
    let mut pods = 0usize;
    for (label, result) in labels.iter().zip(opened) {
        match result {
            Ok((context, count, feed)) => {
                pods += count;
                notices.push(format!("[Tail {label}: following {count} Pods]"));
                feeds.push((context, feed));
            }
            Err(error) => notices.push(format!("[Tail {label} unavailable: {error}]")),
        }
    }
    if pods > MAX_TOTAL_PODS {
        notices.push(format!(
            "[Tail stopped: {pods} Pods exceed the {MAX_TOTAL_PODS}-Pod cross-cluster limit; select fewer workloads]"
        ));
        feeds.clear();
    } else if feeds.is_empty() {
        notices.push("[Tail ended: no workload stream could be opened]".into());
    }
    Opened { notices, feeds }
}

/// Delivers merged, tagged lines to `sink` until every stream ends or `sink`
/// returns false (a newer session took over). Dropping the future aborts
/// every stream.
pub async fn stream(opened: Opened, sink: impl Fn(&str) -> bool) {
    for notice in &opened.notices {
        if !sink(notice) {
            return;
        }
    }
    if opened.feeds.is_empty() {
        return;
    }
    let feeds = opened.feeds;
    let (sender, mut receiver) = tokio::sync::mpsc::channel::<(String, String)>(1024);
    let mut tasks = tokio::task::JoinSet::new();
    for (index, (context, mut feed)) in feeds.into_iter().enumerate() {
        let sender = sender.clone();
        let source = format!("{index}:{context}");
        tasks.spawn(async move {
            loop {
                let line = match feed.next().await {
                    Ok(Some(line)) => line,
                    Ok(None) => format!("[Log stream ended for {context}]"),
                    Err(error) => format!("[Stopped {context}: {error}]"),
                };
                let done =
                    line.starts_with("[Log stream ended for ") || line.starts_with("[Stopped ");
                if sender
                    .send((source.clone(), tag(&context, &line)))
                    .await
                    .is_err()
                    || done
                {
                    break;
                }
            }
        });
    }
    drop(sender);
    let mut reorder = Reorder::default();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            received = receiver.recv() => match received {
                Some((source, line)) => reorder.push(&source, line, Instant::now()),
                None => break,
            },
            _ = tick.tick() => {
                for line in reorder.ready(Instant::now()) {
                    if !sink(&line) {
                        return;
                    }
                }
            }
        }
    }
    for line in reorder.drain() {
        if !sink(&line) {
            return;
        }
    }
    sink("[Cross-cluster tail ended]");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tags_follow_the_timestamp_so_the_toggle_still_hides_it() {
        assert_eq!(
            tag("prod", "2026-10-24T10:00:00.5Z [ns/api:app] boom"),
            "2026-10-24T10:00:00.5Z [prod] [ns/api:app] boom"
        );
        assert_eq!(
            tag("prod", "[Log stream merge: x]"),
            "[prod] [Log stream merge: x]"
        );
        assert_eq!(
            crate::logs::without_timestamp(&tag("prod", "2026-10-24T10:00:00Z hi")),
            "[prod] hi"
        );
    }
    #[test]
    fn reorder_waits_one_window_and_emits_by_timestamp_across_clusters() {
        let start = Instant::now();
        let mut reorder = Reorder::default();
        reorder.push("a", "2026-10-24T10:00:02Z a-late".into(), start);
        reorder.push("b", "2026-10-24T10:00:01.9Z b-early".into(), start);
        reorder.push("b", "[notice from b]".into(), start);
        reorder.push("a", "2026-10-24T10:00:01.95Z a-mid".into(), start);
        assert!(reorder.ready(start + Duration::from_millis(500)).is_empty());
        assert_eq!(
            reorder.ready(start + REORDER_WINDOW),
            vec![
                "2026-10-24T10:00:01.9Z b-early",
                "[notice from b]",
                "2026-10-24T10:00:01.95Z a-mid",
                "2026-10-24T10:00:02Z a-late"
            ]
        );
        reorder.push("a", "2026-10-24T10:00:03Z next".into(), start);
        assert_eq!(reorder.drain(), vec!["2026-10-24T10:00:03Z next"]);
    }
}
