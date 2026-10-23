//! Pull-driven, time-bounded log follow with a rolling display and flood guards.
use crate::{Result, safety};
use futures_util::{
    StreamExt,
    io::{AsyncBufRead, AsyncReadExt},
};
use k8s_openapi::api::core::v1::Pod;
use kube::{
    Api,
    api::{DynamicObject, ListParams, LogParams},
};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::Poll,
    time::Duration,
};
use tokio::time::Instant;

pub const MAX_LOG_BYTES: usize = 256 * 1024;
pub const MAX_LINE_BYTES: usize = 4096;
pub const MAX_LOG_LINES: usize = 2000;
pub const DEFAULT_FOLLOW_SECONDS: u32 = 3600;
pub const MAX_FOLLOW_SECONDS: u32 = 86_400;
const MAX_RATE_BYTES: usize = 1024 * 1024;
const MAX_RATE_LINES: usize = 5000;
const MAX_RECONNECTS: u32 = 5;
const CONNECT_BUDGET: Duration = Duration::from_secs(15);
/// Pod-wide logging deliberately excludes init and ephemeral containers.
pub const MAX_MERGED_CONTAINERS: usize = 16;
pub const MAX_LOG_PODS: usize = 8;
pub const MAX_WORKLOAD_LOG_SOURCES: usize = 32;
pub(crate) const MAX_LOG_JOBS: usize = 64;
const MERGED_OPEN_CONCURRENCY: usize = 4;
const MEMBERSHIP_CHECK_INTERVAL: Duration = Duration::from_secs(5);
const MEMBERSHIP_CHECK_BUDGET: Duration = Duration::from_secs(3);
const MEMBERSHIP_BYTES: usize = 8 * 1024 * 1024;

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
    namespace: String,
    description: String,
    pods: Vec<LogPod>,
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

#[derive(Clone)]
pub(crate) enum IdentityGuard {
    Pod {
        api: Api<Pod>,
        name: String,
        uid: String,
    },
    Parent {
        api: Api<DynamicObject>,
        name: String,
        uid: String,
    },
}
impl IdentityGuard {
    async fn verify(&self, deadline: Instant) -> std::result::Result<(), ReopenFailure> {
        match self {
            Self::Pod { api, name, uid } => {
                check_uid(api, name, uid, deadline)
                    .await
                    .map_err(|error| match error {
                        ReopenFailure::Stop(error) => ReopenFailure::Stop(format!(
                            "captured Pod {}/{} UID {}: {error}",
                            api.namespace().unwrap_or("?"),
                            name,
                            uid
                        )),
                        ReopenFailure::Retry => ReopenFailure::Retry,
                    })
            }
            Self::Parent { api, name, uid } => {
                let value = tokio::time::timeout_at(deadline, api.get_metadata(name))
                    .await
                    .map_err(|_| ReopenFailure::Retry)?
                    .map_err(reopen_failure)?;
                if value.metadata.name.as_deref() != Some(name)
                    || value.metadata.namespace.as_deref() != api.namespace()
                    || value.metadata.uid.as_deref() != Some(uid)
                {
                    return Err(ReopenFailure::Stop(format!(
                        "captured workload or Job {}/{} UID {} identity changed; capture log membership again",
                        api.namespace().unwrap_or("?"),
                        name,
                        uid
                    )));
                }
                Ok(())
            }
        }
    }
}
async fn verify_guards(
    guards: &[IdentityGuard],
    deadline: Instant,
) -> std::result::Result<(), ReopenFailure> {
    let mut checks = futures_util::stream::iter(guards.to_vec())
        .map(|guard| async move { guard.verify(deadline).await })
        .buffer_unordered(MERGED_OPEN_CONCURRENCY);
    while let Some(result) = checks.next().await {
        result?;
    }
    Ok(())
}

#[derive(Default)]
struct MergeBudget {
    total_bytes: usize,
    rate_epoch: Option<Instant>,
    rate_bytes: usize,
    rate_lines: usize,
    stopped: Option<String>,
}
impl MergeBudget {
    fn charge(&mut self, bytes: usize, lines: usize) -> Result<()> {
        if let Some(error) = &self.stopped {
            return Err(error.clone());
        }
        let now = Instant::now();
        if self
            .rate_epoch
            .is_none_or(|epoch| now.saturating_duration_since(epoch) >= Duration::from_secs(1))
        {
            self.rate_epoch = Some(now);
            self.rate_bytes = 0;
            self.rate_lines = 0;
        }
        self.total_bytes = self.total_bytes.saturating_add(bytes);
        self.rate_bytes = self.rate_bytes.saturating_add(bytes);
        self.rate_lines = self.rate_lines.saturating_add(lines);
        let error = if self.total_bytes > crate::transport::LOG_STREAM_BYTES {
            Some(
                "merged logs reached the shared 64 MiB session byte limit; start a new session to continue",
            )
        } else if self.rate_bytes > MAX_RATE_BYTES {
            Some("merged log flood exceeded the shared 1 MiB/second limit; all streams stopped")
        } else if self.rate_lines > MAX_RATE_LINES {
            Some(
                "merged log flood exceeded the shared 5000 lines/second limit; all streams stopped",
            )
        } else {
            None
        };
        if let Some(error) = error {
            self.stopped = Some(error.into());
            return Err(error.into());
        }
        Ok(())
    }
}
type SharedMergeBudget = Arc<Mutex<MergeBudget>>;
fn charge_merge(budget: &SharedMergeBudget, bytes: usize, lines: usize) -> Result<()> {
    budget
        .lock()
        .map_err(|_| "merged log budget unavailable")?
        .charge(bytes, lines)
}

/// Server-side retrieval range. Timestamps are always retained in the raw
/// bounded cache so toggling their display never loses or refetches records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadOptions {
    pub tail_lines: u32,
    pub since_seconds: Option<u32>,
    /// Read the beginning of the currently retained container log, as a snapshot.
    /// Kubernetes does not expose rotated-away files through the logs endpoint.
    pub head: bool,
}
impl Default for ReadOptions {
    fn default() -> Self {
        Self {
            tail_lines: 200,
            since_seconds: None,
            head: false,
        }
    }
}
impl ReadOptions {
    pub fn validate(self) -> Result<Self> {
        if !(1..=MAX_LOG_LINES as u32).contains(&self.tail_lines) {
            return Err("log tail must be 1..2000 lines".into());
        }
        if self
            .since_seconds
            .is_some_and(|seconds| !(1..=MAX_FOLLOW_SECONDS).contains(&seconds))
        {
            return Err("log lookback must be 1..86400 seconds".into());
        }
        if self.head && self.since_seconds.is_some() {
            return Err("head logs cannot also specify a lookback".into());
        }
        Ok(self)
    }
    /// The log-view range shortcuts used by k9s. Head is intentionally a finite
    /// snapshot; all other ranges may be used with an independently bounded follow.
    pub fn for_key(key: char) -> Option<Self> {
        let mut options = Self::default();
        match key {
            '0' => {}
            '1' => options.head = true,
            '2' => options.since_seconds = Some(60),
            '3' => options.since_seconds = Some(300),
            '4' => options.since_seconds = Some(900),
            '5' => options.since_seconds = Some(1800),
            '6' => options.since_seconds = Some(3600),
            _ => return None,
        }
        Some(options)
    }
    pub fn label(self) -> String {
        if self.head {
            format!("head: first {} retained lines (snapshot)", self.tail_lines)
        } else if let Some(seconds) = self.since_seconds {
            format!("last {seconds}s; newest {} lines", self.tail_lines)
        } else {
            format!("tail: newest {} lines", self.tail_lines)
        }
    }
    pub(crate) fn params(
        self,
        container: Option<String>,
        previous: bool,
        follow: bool,
    ) -> Result<kube::api::LogParams> {
        let options = self.validate()?;
        if previous && follow {
            return Err("previous container logs support snapshots only; omit follow".into());
        }
        Ok(kube::api::LogParams {
            container,
            previous,
            follow: follow && !options.head,
            tail_lines: (!options.head).then_some(i64::from(options.tail_lines)),
            since_seconds: options.since_seconds.map(i64::from),
            limit_bytes: (!follow || options.head).then_some(MAX_LOG_BYTES as i64),
            timestamps: true,
            ..Default::default()
        })
    }
}

/// Strip only the Kubernetes RFC3339 timestamp prefix, leaving application text
/// and malformed timestamp-looking prefixes intact. No shell or regex parsing.
pub fn without_timestamp(line: &str) -> &str {
    match line.split_once(' ') {
        Some((timestamp, message)) if crate::metrics::timestamp_key(timestamp).is_some() => message,
        _ => line,
    }
}

const LINE_NOTICE: &str = " [line truncated at 4096 bytes]";

fn bounded_line(line: &str) -> String {
    let mut line = safety::safe_text(line, MAX_LINE_BYTES + 1);
    if line.len() > MAX_LINE_BYTES {
        let mut end = MAX_LINE_BYTES - LINE_NOTICE.len();
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        line.truncate(end);
        line.push_str(LINE_NOTICE);
    }
    line
}

/// Apply the selected head/tail bound even when a server ignores tailLines.
/// Both modes retain a finite sanitized result; head never rolls away its start.
pub(crate) fn snapshot_text(text: &str, options: ReadOptions) -> String {
    const RESERVE: usize = 128;
    let mut output = if options.head {
        let mut output = String::new();
        for line in text.lines().take(options.tail_lines as usize) {
            let line = bounded_line(line);
            if output.len() + line.len() + 1 > MAX_LOG_BYTES - RESERVE {
                output.push_str("[Head snapshot reached the display byte limit]\n");
                break;
            }
            output.push_str(&line);
            output.push('\n');
        }
        output
    } else {
        let mut buffer = RollingLog::default();
        for line in text
            .lines()
            .rev()
            .take(options.tail_lines as usize)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            buffer.push(line);
        }
        buffer.text()
    };
    if text.len() >= MAX_LOG_BYTES {
        // Kubernetes may cut a line at limitBytes; no inference that the last
        // retained line is a complete application record is safe here.
        const NOTICE: &str =
            "[Snapshot reached the server byte limit; the final record may be incomplete]\n";
        if output.len() + NOTICE.len() > MAX_LOG_BYTES {
            let mut end = MAX_LOG_BYTES - NOTICE.len();
            while !output.is_char_boundary(end) {
                end -= 1;
            }
            output.truncate(end);
        }
        output.push_str(NOTICE);
    }
    output
}

#[derive(Clone, Copy, Debug)]
pub struct FollowOptions {
    pub seconds: u32,
}
impl Default for FollowOptions {
    fn default() -> Self {
        Self {
            seconds: DEFAULT_FOLLOW_SECONDS,
        }
    }
}
impl FollowOptions {
    pub fn validate(self) -> Result<Self> {
        if !(1..=MAX_FOLLOW_SECONDS).contains(&self.seconds) {
            return Err("log duration must be 1..86400 seconds".into());
        }
        Ok(self)
    }
}

/// Retain the newest lines, never the entire session. No raw log persistence.
#[derive(Default)]
pub struct RollingLog {
    lines: VecDeque<String>,
    bytes: usize,
    dropped: u64,
}
impl RollingLog {
    pub fn push(&mut self, line: &str) {
        let line = bounded_line(line);
        while self.lines.len() >= MAX_LOG_LINES || self.bytes + line.len() + 1 > MAX_LOG_BYTES - 96
        {
            if let Some(old) = self.lines.pop_front() {
                self.bytes -= old.len() + 1;
                self.dropped = self.dropped.saturating_add(1);
            } else {
                break;
            }
        }
        self.bytes += line.len() + 1;
        self.lines.push_back(line);
    }
    pub fn text(&self) -> String {
        self.filtered_text(true, |_| true)
    }
    /// Reproject the retained window without a new API read. The caller may use
    /// its already compiled resource/log filter. The predicate sees the visible
    /// message, so a hidden timestamp cannot unexpectedly satisfy the filter.
    pub fn filtered_text(&self, show_timestamps: bool, matches: impl Fn(&str) -> bool) -> String {
        let mut text = String::with_capacity(self.bytes + 96);
        if self.dropped > 0 {
            text.push_str(&format!(
                "[Rolling buffer: {} older lines discarded]\n",
                self.dropped
            ));
        }
        for line in &self.lines {
            let shown = if show_timestamps {
                line.as_str()
            } else {
                without_timestamp(line)
            };
            if matches(shown) {
                text.push_str(shown);
                text.push('\n');
            }
        }
        text
    }
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
}

pub struct LogFeed {
    reader: Pin<Box<dyn AsyncBufRead + Send>>,
    pending: Vec<u8>,
    chunk: [u8; 4096],
    offset: usize,
    available: usize,
    deadline: Instant,
    seconds: u32,
    rate_epoch: Instant,
    rate_bytes: usize,
    rate_lines: usize,
    discarding: bool,
    ended: bool,
    remaining_lines: Option<u32>,
    total_bytes: usize,
    resume: Option<ResumeSource>,
    reconnects: u32,
    last_timestamp: Option<String>,
    reconnect_pending: bool,
    merge_budget: Option<SharedMergeBudget>,
    last_control: bool,
    /// Dynamic membership: a replaced or deleted Pod ends only its own source.
    isolate_identity: bool,
}

#[derive(Clone)]
struct ResumeSource {
    api: Api<Pod>,
    name: String,
    uid: String,
    params: LogParams,
    guards: Arc<Vec<IdentityGuard>>,
}

enum ReopenFailure {
    Retry,
    Stop(String),
}
fn reopen_failure(error: kube::Error) -> ReopenFailure {
    match error {
        kube::Error::Api(response) if matches!(response.code, 408 | 429 | 500..=599) => {
            ReopenFailure::Retry
        }
        kube::Error::Api(response) => ReopenFailure::Stop(format!(
            "log reconnect stopped (Kubernetes API {}); reopen after checking target and permissions",
            response.code
        )),
        kube::Error::HyperError(_) | kube::Error::Service(_) | kube::Error::ReadEvents(_) => {
            ReopenFailure::Retry
        }
        _ => ReopenFailure::Stop(
            "log reconnect received an invalid response; stopped without displaying server details"
                .into(),
        ),
    }
}
async fn check_uid(
    api: &Api<Pod>,
    name: &str,
    uid: &str,
    deadline: Instant,
) -> std::result::Result<(), ReopenFailure> {
    let pod = tokio::time::timeout_at(deadline, api.get_metadata(name))
        .await
        .map_err(|_| ReopenFailure::Retry)?
        .map_err(reopen_failure)?;
    if pod.metadata.name.as_deref() != Some(name)
        || pod.metadata.namespace.as_deref() != api.namespace()
        || pod.metadata.uid.as_deref() != Some(uid)
    {
        return Err(ReopenFailure::Stop("log target was replaced or returned outside the selected scope; reopen the selected pod".into()));
    }
    Ok(())
}
/// Called only by the explicitly identity-bound resumable workflow. A plain log
/// read never adds a new pod-spec permission requirement.
pub(crate) async fn verify_uid(api: &Api<Pod>, name: &str, uid: &str) -> Result<()> {
    if uid.is_empty() || uid.len() > 253 || safety::safe_text(uid, 253) != uid {
        return Err("invalid selected pod identity for log reconnect".into());
    }
    match check_uid(api, name, uid, Instant::now() + CONNECT_BUDGET).await {
        Ok(()) => Ok(()),
        Err(ReopenFailure::Stop(message)) => Err(message),
        Err(ReopenFailure::Retry) => {
            Err("log target identity verification failed; retry after checking connectivity".into())
        }
    }
}
impl LogFeed {
    fn stop_merge_for_identity(&self, message: String) -> String {
        if let Some(budget) = &self.merge_budget
            && let Ok(mut budget) = budget.lock()
        {
            budget.stopped = Some(format!("{message}; all merged streams stopped"));
        }
        message
    }
    /// Pod identity loss stops every merged stream for fixed membership, but
    /// only this source when the session explicitly opted into dynamic membership.
    fn stop_source_for_identity(&self, message: String) -> String {
        if self.isolate_identity {
            format!("{message}; this Pod's stream closed")
        } else {
            self.stop_merge_for_identity(message)
        }
    }
    #[cfg(test)]
    pub(crate) fn new(reader: impl AsyncBufRead + Send + 'static) -> Self {
        Self::create(reader, FollowOptions::default())
    }
    pub(crate) fn with_options(
        reader: impl AsyncBufRead + Send + 'static,
        options: FollowOptions,
    ) -> Result<Self> {
        Ok(Self::create(reader, options.validate()?))
    }
    pub(crate) fn with_read_options(
        reader: impl AsyncBufRead + Send + 'static,
        options: FollowOptions,
        read_options: ReadOptions,
    ) -> Result<Self> {
        let read_options = read_options.validate()?;
        let mut feed = Self::create(reader, options.validate()?);
        feed.remaining_lines = read_options.head.then_some(read_options.tail_lines);
        Ok(feed)
    }
    pub(crate) fn with_resume(
        mut self,
        api: Api<Pod>,
        name: &str,
        uid: &str,
        container: Option<String>,
        read_options: ReadOptions,
    ) -> Result<Self> {
        if !read_options.head {
            self.resume = Some(ResumeSource {
                api,
                name: name.into(),
                uid: uid.into(),
                params: read_options.params(container, false, true)?,
                guards: Arc::new(Vec::new()),
            });
        }
        Ok(self)
    }
    fn create(reader: impl AsyncBufRead + Send + 'static, options: FollowOptions) -> Self {
        let now = Instant::now();
        Self {
            reader: Box::pin(reader),
            pending: Vec::with_capacity(MAX_LINE_BYTES),
            chunk: [0; 4096],
            offset: 0,
            available: 0,
            deadline: now + Duration::from_secs(u64::from(options.seconds)),
            seconds: options.seconds,
            rate_epoch: now,
            rate_bytes: 0,
            rate_lines: 0,
            discarding: false,
            ended: false,
            remaining_lines: None,
            total_bytes: 0,
            resume: None,
            reconnects: 0,
            last_timestamp: None,
            reconnect_pending: false,
            merge_budget: None,
            isolate_identity: false,
            last_control: false,
        }
    }
    fn rate_window(&mut self) {
        let now = Instant::now();
        if now.saturating_duration_since(self.rate_epoch) >= Duration::from_secs(1) {
            self.rate_epoch = now;
            self.rate_bytes = 0;
            self.rate_lines = 0;
        }
    }
    fn line(&mut self) -> Result<String> {
        if let Some(budget) = &self.merge_budget {
            charge_merge(budget, 0, 1)?;
        }
        self.rate_window();
        self.rate_lines += 1;
        if self.rate_lines > MAX_RATE_LINES {
            self.ended = true;
            self.pending.clear();
            return Err("log flood exceeded 5000 lines/second; follow stopped".into());
        }
        let prefix_limit = if self.discarding {
            MAX_LINE_BYTES - LINE_NOTICE.len()
        } else {
            MAX_LINE_BYTES
        };
        let mut line = safety::safe_text(&String::from_utf8_lossy(&self.pending), prefix_limit);
        self.pending.clear();
        if self.discarding {
            line.push_str(LINE_NOTICE);
        }
        self.discarding = false;
        if let Some((timestamp, _)) = line.split_once(' ')
            && crate::metrics::timestamp_key(timestamp).is_some()
        {
            self.last_timestamp = Some(timestamp.into());
        }
        if let Some(remaining) = self.remaining_lines.as_mut() {
            *remaining = remaining.saturating_sub(1);
            if *remaining == 0 {
                self.ended = true;
            }
        }
        Ok(bounded_line(&line))
    }
    async fn reconnect(&mut self) -> Result<String> {
        let source = self.resume.clone().ok_or("log reconnect is unavailable")?;
        let timestamp = self.last_timestamp.as_deref().ok_or(
            "log transport failed before a valid Kubernetes timestamp; reopen the viewer to retry",
        )?;
        // kube rounds sinceTime to whole seconds. Remove fractional digits
        // before parsing so rounding cannot advance past the last seen record.
        let whole_second = timestamp.split_once('.').map_or_else(
            || timestamp.to_owned(),
            |(_, fraction)| {
                let zone = fraction.find(['Z', '+', '-']).unwrap_or(fraction.len());
                format!("{}{}", &timestamp[..19], &fraction[zone..])
            },
        );
        let mut params = source.params.clone();
        params.since_seconds = None;
        params.since_time = Some(
            whole_second
                .parse()
                .map_err(|_| "log timestamp cannot be resumed safely; reopen the viewer")?,
        );
        // tailLines would silently discard records produced during disconnect.
        params.tail_lines = None;
        loop {
            if self.reconnects >= MAX_RECONNECTS {
                return Err(
                    "log transport failed; five reconnect attempts exhausted for this session"
                        .into(),
                );
            }
            self.reconnects += 1;
            let backoff = Duration::from_secs(1u64 << (self.reconnects - 1).min(3));
            if Instant::now() + backoff >= self.deadline {
                return Err(
                    "log session deadline reached before reconnect; start a new session".into(),
                );
            }
            tokio::time::sleep(backoff).await;
            let deadline = self.deadline.min(Instant::now() + CONNECT_BUDGET);
            match verify_guards(&source.guards, deadline).await {
                Ok(()) => {}
                Err(ReopenFailure::Retry) => continue,
                Err(ReopenFailure::Stop(message)) => {
                    return Err(self.stop_merge_for_identity(message));
                }
            }
            match check_uid(&source.api, &source.name, &source.uid, deadline).await {
                Ok(()) => {}
                Err(ReopenFailure::Retry) => continue,
                Err(ReopenFailure::Stop(message)) => {
                    return Err(self.stop_source_for_identity(message));
                }
            }
            match tokio::time::timeout_at(deadline, source.api.log_stream(&source.name, &params))
                .await
            {
                Ok(Ok(reader)) => {
                    // Recheck after opening to catch replacement during the
                    // request. The API lacks a UID precondition on pod/log.
                    match check_uid(&source.api, &source.name, &source.uid, deadline).await {
                        Ok(()) => {}
                        Err(ReopenFailure::Retry) => continue,
                        Err(ReopenFailure::Stop(message)) => {
                            return Err(self.stop_source_for_identity(message));
                        }
                    }
                    self.reader = Box::pin(reader);
                    self.pending.clear();
                    self.discarding = false;
                    self.offset = 0;
                    self.available = 0;
                    return Ok(format!(
                        "[Log stream resumed after reconnect attempt {}/5 from the last observed second; overlap may repeat records, and rotation/restarts may leave gaps]",
                        self.reconnects
                    ));
                }
                Ok(Err(error)) => match reopen_failure(error) {
                    ReopenFailure::Retry => continue,
                    ReopenFailure::Stop(message) => return Err(message),
                },
                Err(_) => continue,
            }
        }
    }
    /// One sanitized line at a time. Oversized lines are truncated explicitly.
    /// Quiet streams wait until the configured deadline; dropping cancels the body.
    pub async fn next(&mut self) -> Result<Option<String>> {
        self.last_control = false;
        if self.ended {
            return Ok(None);
        }
        if self.reconnect_pending {
            return match self.reconnect().await {
                Ok(message) => {
                    self.reconnect_pending = false;
                    self.last_control = true;
                    Ok(Some(message))
                }
                Err(error) => {
                    self.ended = true;
                    Err(error)
                }
            };
        }
        loop {
            if Instant::now() >= self.deadline {
                self.ended = true;
                self.pending.clear();
                return Err(format!(
                    "log follow reached its {}-second session limit; start a new session to continue",
                    self.seconds
                ));
            }
            while self.offset < self.available {
                let byte = self.chunk[self.offset];
                self.offset += 1;
                if byte == b'\n' {
                    return self.line().map(Some);
                }
                if self.pending.len() == MAX_LINE_BYTES {
                    self.discarding = true;
                }
                if !self.discarding {
                    self.pending.push(byte);
                }
            }
            match tokio::time::timeout_at(self.deadline, self.reader.read(&mut self.chunk)).await {
                Ok(Ok(0)) => {
                    self.ended = true;
                    if self.pending.is_empty() && !self.discarding {
                        return Ok(None);
                    }
                    return self.line().map(Some);
                }
                Ok(Ok(count)) => {
                    if let Some(budget) = &self.merge_budget {
                        charge_merge(budget, count, 0)?;
                    }
                    self.total_bytes += count;
                    if self.total_bytes > crate::transport::LOG_STREAM_BYTES {
                        self.ended = true;
                        self.pending.clear();
                        return Err("log follow reached its 64 MiB session byte limit; start a new session to continue".into());
                    }
                    self.rate_window();
                    self.rate_bytes += count;
                    if self.rate_bytes > MAX_RATE_BYTES {
                        self.ended = true;
                        self.pending.clear();
                        return Err("log flood exceeded 1 MiB/second; follow stopped".into());
                    }
                    self.offset = 0;
                    self.available = count;
                }
                Ok(Err(_)) => {
                    self.pending.clear();
                    if self.resume.is_some()
                        && self.last_timestamp.is_some()
                        && self.reconnects < MAX_RECONNECTS
                        && self.total_bytes < crate::transport::LOG_STREAM_BYTES
                    {
                        self.reconnect_pending = true;
                        self.last_control = true;
                        return Ok(Some("[Log stream interrupted; reconnecting. Overlap may repeat records; rotation/restarts may leave gaps]".into()));
                    }
                    self.ended = true;
                    return Err("log stream transport failure; follow stopped".into());
                }
                Err(_) => {
                    self.ended = true;
                    self.pending.clear();
                    return Err(format!(
                        "log follow reached its {}-second session limit; start a new session to continue",
                        self.seconds
                    ));
                }
            }
        }
    }
}

/// Validate the complete Pod identity before deriving ordinary container names.
/// A same-name replacement must never silently join an existing log session.
pub(crate) fn ordinary_container_names(
    pod: &Pod,
    ns: &str,
    name: &str,
    uid: &str,
) -> Result<Vec<String>> {
    validate_merge_target(ns, name, uid)?;
    if pod.metadata.name.as_deref() != Some(name)
        || pod.metadata.namespace.as_deref() != Some(ns)
        || pod.metadata.uid.as_deref() != Some(uid)
    {
        return Err("merged log target was replaced or returned outside the selected scope; reopen the selected pod".into());
    }
    let containers = &pod
        .spec
        .as_ref()
        .ok_or("merged log target has no pod spec")?
        .containers;
    if containers.is_empty() || containers.len() > MAX_MERGED_CONTAINERS {
        return Err(format!(
            "merged logs require 1..{MAX_MERGED_CONTAINERS} ordinary containers; select one container instead"
        ));
    }
    let mut names = Vec::with_capacity(containers.len());
    for container in containers {
        if !safety::valid_id(&container.name) || names.contains(&container.name) {
            return Err(
                "merged log pod contains invalid or duplicate ordinary container names".into(),
            );
        }
        names.push(container.name.clone());
    }
    Ok(names)
}
pub(crate) fn validate_merge_target(ns: &str, name: &str, uid: &str) -> Result<()> {
    if !safety::valid_id(ns)
        || name.is_empty()
        || name.len() > 253
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.'))
        || uid.is_empty()
        || uid.len() > 253
        || safety::safe_text(uid, 253) != uid
    {
        return Err("invalid identity-bound merged log target".into());
    }
    Ok(())
}

type PendingSource = Pin<Box<dyn Future<Output = (LogFeed, Result<Option<String>>)> + Send>>;
struct MergedSource {
    identity: String,
    pending: Option<PendingSource>,
    notice: Option<String>,
}
fn pending_source(mut feed: LogFeed) -> PendingSource {
    Box::pin(async move {
        let result = feed.next().await;
        (feed, result)
    })
}
fn source_notice(identity: &str, message: &str) -> String {
    bounded_line(&format!("[Log stream {identity}: {message}]"))
}
fn source_line(identity: &str, line: &str, control: bool) -> String {
    // Leave the timestamp first: local timestamp toggling and reconnect semantics
    // remain meaningful. Application text can never impersonate a merge notice.
    if control {
        return source_notice(identity, line.trim_start_matches('[').trim_end_matches(']'));
    }
    if let Some((timestamp, message)) = line.split_once(' ')
        && crate::metrics::timestamp_key(timestamp).is_some()
    {
        return bounded_line(&format!("{timestamp} [{identity}] {message}"));
    }
    bounded_line(&format!("[{identity}] {line}"))
}
fn open_failure(error: kube::Error, previous: bool) -> String {
    match error {
        kube::Error::Api(response) => format!(
            "{} unavailable (Kubernetes API {}); other container streams continue",
            if previous { "previous logs" } else { "logs" },
            response.code
        ),
        _ => "log connection failed; other container streams continue (server details suppressed)"
            .into(),
    }
}

/// One bounded read future per source; no spawned tasks, queues or detached work.
/// Ready sources rotate fairly. Dropping the feed cancels every open body/read.
pub struct MergedLogFeed {
    sources: Vec<MergedSource>,
    cursor: usize,
    deadline: Instant,
    budget: SharedMergeBudget,
    header: VecDeque<String>,
    ended: bool,
    snapshot_mode: bool,
    guards: Arc<Vec<IdentityGuard>>,
    next_identity_check: Option<Instant>,
    identity_failures: u8,
    notice_reserve: usize,
    /// Dynamic sessions wait for joins instead of ending when no source is active.
    keep_open: bool,
}
impl MergedLogFeed {
    /// Close every source of one departed Pod. Dropping a pending read cancels it.
    pub(crate) fn remove_pod(&mut self, namespace: &str, pod: &str) -> usize {
        let prefix = format!("{namespace}/{pod}:");
        let before = self.sources.len();
        self.sources
            .retain(|source| !source.identity.starts_with(&prefix));
        self.cursor = 0;
        before - self.sources.len()
    }
    /// Admit sources for one newly joined Pod under the same session deadline,
    /// shared budget and parent guards. The 32-source bound stays global.
    pub(crate) fn add_sources(&mut self, sources: Vec<(String, Result<LogFeed>)>) -> Result<()> {
        if self.ended {
            return Err("log session already ended".into());
        }
        if self.sources.len() + sources.len() > MAX_WORKLOAD_LOG_SOURCES {
            return Err("joined Pod would exceed the shared 32-source limit".into());
        }
        for (identity, result) in sources {
            if self
                .sources
                .iter()
                .any(|source| source.identity == identity)
            {
                return Err("joined Pod duplicates an active log source".into());
            }
            let (pending, notice) = match result {
                Ok(mut feed) => {
                    feed.deadline = self.deadline;
                    feed.merge_budget = Some(self.budget.clone());
                    feed.isolate_identity = true;
                    if let Some(resume) = &mut feed.resume {
                        resume.guards = self.guards.clone();
                    }
                    (Some(pending_source(feed)), None)
                }
                Err(error) => (
                    None,
                    Some(source_notice(&identity, &safety::safe_text(&error, 512))),
                ),
            };
            self.sources.push(MergedSource {
                identity,
                pending,
                notice,
            });
        }
        Ok(())
    }
    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }
    pub(crate) async fn open_membership(
        api: Api<Pod>,
        membership: &LogMembership,
        mut guards: Vec<IdentityGuard>,
        previous: bool,
        follow: Option<FollowOptions>,
        read: ReadOptions,
    ) -> Result<Self> {
        if api.namespace() != Some(membership.namespace()) {
            return Err("log membership API escaped its captured namespace".into());
        }
        let read = read.validate()?;
        if previous && follow.is_some() {
            return Err("previous container logs support snapshots only; omit follow".into());
        }
        let options = follow
            .unwrap_or(FollowOptions {
                seconds: CONNECT_BUDGET.as_secs() as u32,
            })
            .validate()?;
        let deadline = Instant::now() + Duration::from_secs(u64::from(options.seconds));
        let connect_deadline = deadline.min(Instant::now() + CONNECT_BUDGET);
        let mut bytes = 0;
        let mut captures=futures_util::stream::iter(membership.pods.iter().map(|member| {
            let api=api.clone(); async move {
                let pod=tokio::time::timeout_at(connect_deadline,api.get(&member.name)).await.map_err(|_|"captured log Pod verification timed out")?.map_err(|error|open_failure(error,false))?;
                let current=log_pod(&pod,membership.namespace())?;
                let mut actual=current.containers.clone(); actual.sort();
                let mut expected=member.containers.clone(); expected.sort();
                if current.name!=member.name||current.uid!=member.uid||actual!=expected {return Err("captured log Pod or ordinary-container membership changed; capture sources again".into());}
                Ok::<Pod,String>(pod)
            }
        })).buffer_unordered(MERGED_OPEN_CONCURRENCY);
        while let Some(pod) = captures.next().await {
            membership_bytes(&mut bytes, &pod?)?;
        }
        // Dynamic sessions verify every captured Pod while opening, but keep only
        // the parent workload as a session-wide guard: a departing Pod is a
        // reported membership change, not a reason to stop its peers.
        let dynamic = membership.is_dynamic();
        let session_guards = dynamic.then(|| guards.clone());
        for member in &membership.pods {
            guards.push(IdentityGuard::Pod {
                api: api.clone(),
                name: member.name.clone(),
                uid: member.uid.clone(),
            });
        }
        match verify_guards(&guards, connect_deadline).await {
            Ok(()) => {}
            Err(ReopenFailure::Retry) => {
                return Err(
                    "captured workload log identities could not be verified; retry explicitly"
                        .into(),
                );
            }
            Err(ReopenFailure::Stop(error)) => return Err(error),
        }
        let requests = membership
            .pods
            .iter()
            .flat_map(|pod| pod.containers.iter().map(move |container| (pod, container)));
        let mut opens =
            futures_util::stream::iter(requests.enumerate().map(|(index, (pod, container))| {
                let api = api.clone();
                async move {
                    let result = async {
                        let params =
                            read.params(Some(container.clone()), previous, follow.is_some())?;
                        let reader = tokio::time::timeout_at(
                            connect_deadline,
                            api.log_stream(&pod.name, &params),
                        )
                        .await
                        .map_err(|_| "log source connection timed out".to_owned())?
                        .map_err(|error| open_failure(error, previous))?;
                        let mut feed = LogFeed::with_read_options(reader, options, read)?;
                        feed.deadline = deadline;
                        feed.isolate_identity = dynamic;
                        if follow.is_some() {
                            feed = feed.with_resume(
                                api,
                                &pod.name,
                                &pod.uid,
                                Some(container.clone()),
                                read,
                            )?;
                        }
                        Ok(feed)
                    }
                    .await;
                    (index, pod.name.clone(), container.clone(), result)
                }
            }))
            .buffer_unordered(MERGED_OPEN_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        // Prevent replacement while any of the source requests were opening.
        match verify_guards(&guards,connect_deadline).await {
            Ok(())=>{}, Err(ReopenFailure::Retry)=>return Err("captured workload log identities could not be verified after opening; retry explicitly".into()), Err(ReopenFailure::Stop(error))=>return Err(error),
        }
        opens.sort_by_key(|(index, _, _, _)| *index);
        if let Some(parents) = session_guards {
            guards = parents;
        }
        if follow.is_none() || read.head {
            guards.clear();
        }
        Self::from_membership_sources(
            membership,
            opens
                .into_iter()
                .map(|(_, pod, container, result)| (pod, container, result))
                .collect(),
            deadline,
            guards,
        )
    }
    pub(crate) fn from_membership_sources(
        membership: &LogMembership,
        sources: Vec<(String, String, Result<LogFeed>)>,
        deadline: Instant,
        guards: Vec<IdentityGuard>,
    ) -> Result<Self> {
        if sources.len() != membership.source_count() {
            return Err("log streams do not cover the complete captured membership".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for (pod, container, _) in &sources {
            if !membership
                .pods
                .iter()
                .any(|member| member.name == *pod && member.containers.contains(container))
                || !seen.insert((pod, container))
            {
                return Err(
                    "log stream escaped or duplicated the captured Pod/container membership".into(),
                );
            }
        }
        let mut headers = VecDeque::new();
        let admission = if membership.is_dynamic() {
            "dynamic membership opted in; every join and departure is announced, replacements are a departure plus a join"
        } else {
            "fixed membership, no new Pods or replacements admitted"
        };
        headers.push_back(bounded_line(&format!("[Log stream merge: {}; {} Pods / {} ordinary containers; {admission}; delivery arrival order, timestamps preserved; init/ephemeral excluded]",membership.description,membership.pods.len(),sources.len())));
        for pod in &membership.pods {
            headers.push_back(source_notice(
                &format!("{}/{}", membership.namespace, pod.name),
                &format!(
                    "captured UID {}; {} ordinary containers",
                    pod.uid,
                    pod.containers.len()
                ),
            ));
        }
        let sources = sources
            .into_iter()
            .map(|(pod, container, result)| {
                (
                    format!("{}/{pod}:{container}", membership.namespace),
                    result,
                )
            })
            .collect();
        let mut feed = Self::build_sources(sources, headers, deadline, guards)?;
        feed.notice_reserve = 64 * 1024;
        Ok(feed)
    }
    pub(crate) async fn open(
        api: Api<Pod>,
        name: &str,
        uid: &str,
        previous: bool,
        follow: Option<FollowOptions>,
        read_options: ReadOptions,
    ) -> Result<Self> {
        let ns = api
            .namespace()
            .ok_or("merged logs require a concrete namespace")?
            .to_owned();
        validate_merge_target(&ns, name, uid)?;
        let read_options = read_options.validate()?;
        if previous && follow.is_some() {
            return Err("previous container logs support snapshots only; omit follow".into());
        }
        let options = follow
            .unwrap_or(FollowOptions {
                seconds: CONNECT_BUDGET.as_secs() as u32,
            })
            .validate()?;
        let started = Instant::now();
        let deadline = started + Duration::from_secs(u64::from(options.seconds));
        let connect_deadline = deadline.min(started + CONNECT_BUDGET);
        let pod = tokio::time::timeout_at(connect_deadline, api.get(name))
            .await
            .map_err(|_| "merged log pod lookup timed out")?
            .map_err(|error| match error {
                kube::Error::Api(response) => format!("cannot read merged-log Pod specification (Kubernetes API {}); all-container logs require pod get permission", response.code),
                _ => "merged-log Pod lookup failed; check connectivity and pod get permission (server details suppressed)".into(),
            })?;
        let names = ordinary_container_names(&pod, &ns, name, uid)?;
        let mut opened =
            futures_util::stream::iter(names.into_iter().enumerate().map(|(index, container)| {
                let api = api.clone();
                async move {
                    let result = async {
                        let params = read_options.params(
                            Some(container.clone()),
                            previous,
                            follow.is_some(),
                        )?;
                        let reader = tokio::time::timeout_at(
                            connect_deadline,
                            api.log_stream(name, &params),
                        )
                        .await
                        .map_err(|_| {
                            "log connection timed out; other container streams continue".to_owned()
                        })?
                        .map_err(|error| open_failure(error, previous))?;
                        let mut feed = LogFeed::with_read_options(reader, options, read_options)?;
                        feed.deadline = deadline;
                        if follow.is_some() {
                            feed = feed.with_resume(
                                api,
                                name,
                                uid,
                                Some(container.clone()),
                                read_options,
                            )?;
                        }
                        Ok(feed)
                    }
                    .await;
                    (index, container, result)
                }
            }))
            .buffer_unordered(MERGED_OPEN_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        // No source body is polled before this post-open identity check.
        match check_uid(&api, name, uid, connect_deadline).await {
            Ok(()) => {},
            Err(ReopenFailure::Stop(error)) => return Err(error),
            Err(ReopenFailure::Retry) => return Err("merged log target identity verification failed; reopen after checking connectivity".into()),
        }
        opened.sort_by_key(|(index, _, _)| *index);
        Self::from_sources_at(
            &ns,
            name,
            uid,
            opened
                .into_iter()
                .map(|(_, name, result)| (name, result))
                .collect(),
            deadline,
        )
    }
    pub(crate) fn from_sources(
        ns: &str,
        name: &str,
        uid: &str,
        sources: Vec<(String, Result<LogFeed>)>,
        options: FollowOptions,
    ) -> Result<Self> {
        let options = options.validate()?;
        Self::from_sources_at(
            ns,
            name,
            uid,
            sources,
            Instant::now() + Duration::from_secs(u64::from(options.seconds)),
        )
    }
    fn from_sources_at(
        ns: &str,
        name: &str,
        uid: &str,
        sources: Vec<(String, Result<LogFeed>)>,
        deadline: Instant,
    ) -> Result<Self> {
        validate_merge_target(ns, name, uid)?;
        if sources.is_empty() || sources.len() > MAX_MERGED_CONTAINERS {
            return Err(
                "merged log source count exceeded its bounded ordinary-container scope".into(),
            );
        }
        let mut names = Vec::with_capacity(sources.len());
        let mut identified = Vec::with_capacity(sources.len());
        for (container, result) in sources {
            if !safety::valid_id(&container) || names.contains(&container) {
                return Err("invalid or duplicate merged log source".into());
            }
            names.push(container.clone());
            let identity = format!("{ns}/{name}:{container}");
            identified.push((identity, result));
        }
        Self::build_sources(
            identified,
            VecDeque::from([format!(
                "[Log stream merge: {} ordinary containers; pod UID {uid}; delivery in arrival order, source timestamps preserved; init/ephemeral excluded]",
                names.len()
            )]),
            deadline,
            Vec::new(),
        )
    }
    fn build_sources(
        sources: Vec<(String, Result<LogFeed>)>,
        header: VecDeque<String>,
        deadline: Instant,
        guards: Vec<IdentityGuard>,
    ) -> Result<Self> {
        if sources.is_empty() || sources.len() > MAX_WORKLOAD_LOG_SOURCES {
            return Err("merged logs exceed the shared 32-source limit".into());
        }
        let budget: SharedMergeBudget = Arc::new(Mutex::new(MergeBudget::default()));
        let guards = Arc::new(guards);
        let mut merged = Vec::with_capacity(sources.len());
        for (identity, result) in sources {
            let (pending, notice) = match result {
                Ok(mut feed) => {
                    feed.deadline = deadline;
                    feed.merge_budget = Some(budget.clone());
                    if let Some(resume) = &mut feed.resume {
                        resume.guards = guards.clone();
                    }
                    (Some(pending_source(feed)), None)
                }
                Err(error) => (
                    None,
                    Some(source_notice(&identity, &safety::safe_text(&error, 512))),
                ),
            };
            merged.push(MergedSource {
                identity,
                pending,
                notice,
            });
        }
        Ok(Self {
            sources: merged,
            cursor: 0,
            deadline,
            budget,
            header,
            ended: false,
            snapshot_mode: false,
            next_identity_check: (!guards.is_empty())
                .then(|| Instant::now() + MEMBERSHIP_CHECK_INTERVAL),
            guards,
            identity_failures: 0,
            notice_reserve: 16 * 1024,
            keep_open: false,
        })
    }
    pub async fn next(&mut self) -> Result<Option<String>> {
        loop {
            if self.ended {
                return Ok(None);
            }
            let shared_error = self
                .budget
                .lock()
                .map_err(|_| "merged log budget unavailable")?
                .stopped
                .clone();
            if let Some(error) = shared_error {
                self.stop();
                return Err(error);
            }
            if Instant::now() >= self.deadline {
                self.stop();
                return Err("merged log session deadline reached; all streams stopped; start a new session to continue".into());
            }
            if self
                .next_identity_check
                .is_some_and(|next| Instant::now() >= next)
            {
                let deadline = self.deadline.min(Instant::now() + MEMBERSHIP_CHECK_BUDGET);
                let result = verify_guards(&self.guards, deadline).await;
                if Instant::now() >= self.deadline {
                    self.stop();
                    return Err("merged log session deadline reached during identity verification; all streams stopped".into());
                }
                self.next_identity_check = Some(Instant::now() + MEMBERSHIP_CHECK_INTERVAL);
                match result {
                    Ok(()) => self.identity_failures = 0,
                    Err(ReopenFailure::Stop(error)) => {
                        self.stop();
                        return Err(format!(
                            "{error}; all {} log streams stopped",
                            if self.keep_open {
                                "dynamic-membership"
                            } else {
                                "fixed-membership"
                            }
                        ));
                    }
                    Err(ReopenFailure::Retry) => {
                        self.identity_failures += 1;
                        if self.identity_failures >= 3 {
                            self.stop();
                            return Err("fixed log membership could not be verified three times; all streams stopped; capture sources again".into());
                        }
                        return Ok(Some("[Log stream membership verification temporarily unavailable; captured sources retained; no new Pods admitted]".into()));
                    }
                }
            }
            if let Some(header) = self.header.pop_front() {
                return Ok(Some(header));
            }
            let count = self.sources.len();
            let keep_open = self.keep_open;
            for distance in 0..count {
                let index = (self.cursor + distance) % count;
                if let Some(notice) = self.sources[index].notice.take() {
                    self.cursor = (index + 1) % count;
                    return Ok(Some(notice));
                }
            }
            let ready = std::future::poll_fn(|cx| {
                let mut active = false;
                for distance in 0..count {
                    let index = (self.cursor + distance) % count;
                    if let Some(pending) = self.sources[index].pending.as_mut() {
                        active = true;
                        if let Poll::Ready(result) = pending.as_mut().poll(cx) {
                            return Poll::Ready(Some((index, result)));
                        }
                    }
                }
                if active || keep_open {
                    Poll::Pending
                } else {
                    Poll::Ready(None)
                }
            });
            let wait_deadline = self
                .next_identity_check
                .map_or(self.deadline, |check| check.min(self.deadline));
            let ready = match tokio::time::timeout_at(wait_deadline, ready).await {
                Ok(ready) => ready,
                Err(_) => {
                    if Instant::now() < self.deadline {
                        continue;
                    }
                    self.stop();
                    return Err("merged log session deadline reached; all streams stopped; start a new session to continue".into());
                }
            };
            let Some((index, (feed, result))) = ready else {
                self.stop();
                return Ok(None);
            };
            self.sources[index].pending = None;
            self.cursor = (index + 1) % count;
            // A shared budget violation is fatal to every source, not a recoverable
            // per-container failure. No remaining buffered data is delivered.
            let shared_error = self
                .budget
                .lock()
                .map_err(|_| "merged log budget unavailable")?
                .stopped
                .clone();
            if let Some(error) = shared_error {
                self.stop();
                return Err(error);
            }
            return match result {
                Ok(Some(line)) => {
                    let output =
                        source_line(&self.sources[index].identity, &line, feed.last_control);
                    if let Err(error) =
                        charge_merge(&self.budget, output.len().saturating_sub(line.len()), 0)
                    {
                        self.stop();
                        return Err(error);
                    }
                    self.sources[index].pending = Some(pending_source(feed));
                    Ok(Some(output))
                }
                Ok(None) => Ok(Some(source_notice(
                    &self.sources[index].identity,
                    if self.snapshot_mode && feed.total_bytes >= MAX_LOG_BYTES {
                        "ended at the requested server byte limit; the final record may be incomplete; other container streams continue"
                    } else {
                        "ended; other container streams continue"
                    },
                ))),
                Err(error) => Ok(Some(source_notice(
                    &self.sources[index].identity,
                    &safety::safe_text(&error, 512),
                ))),
            };
        }
    }
    fn stop(&mut self) {
        self.ended = true;
        self.sources.clear();
    }
    /// A bounded timestamp-ordered projection. Selection is global across the
    /// sources, rather than multiplying the requested head/tail by their count.
    pub(crate) async fn snapshot(mut self, options: ReadOptions) -> Result<String> {
        let options = options.validate()?;
        self.snapshot_mode = true;
        let notice_bytes = self.notice_reserve;
        let mut notices = String::new();
        let mut lines = Vec::new();
        let mut bytes = 0usize;
        let mut ordinal = 0usize;
        let mut discarded = false;
        loop {
            let line = match self.next().await {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(error) => {
                    notices.push_str(&format!("[Stopped: {}]\n", safety::safe_text(&error, 512)));
                    break;
                }
            };
            if line.starts_with("[Log stream ") {
                if notices.len() + line.len() < notice_bytes - 1024 {
                    notices.push_str(&line);
                    notices.push('\n');
                }
                continue;
            }
            let key = line
                .split_once(' ')
                .and_then(|(time, _)| crate::metrics::timestamp_key(time));
            let entry = (key, ordinal, line);
            ordinal += 1;
            let index = lines.partition_point(|old: &(Option<(i64, String)>, usize, String)| {
                (old.0.as_ref(), old.1) <= (entry.0.as_ref(), entry.1)
            });
            bytes += entry.2.len() + 1;
            lines.insert(index, entry);
            while lines.len() > options.tail_lines as usize || bytes > MAX_LOG_BYTES - notice_bytes
            {
                let removed = if options.head {
                    lines.pop().ok_or("empty merged snapshot")?
                } else {
                    lines.remove(0)
                };
                bytes -= removed.2.len() + 1;
                discarded = true;
            }
        }
        notices.push_str("[Log stream merge snapshot: timestamped records ordered by Kubernetes timestamp; untimestamped records precede them in arrival order; requested line limit is shared across containers]\n");
        if discarded {
            notices.push_str("[Log stream merge snapshot: records omitted by the shared head/tail or display byte limit]\n");
        }
        for (_, _, line) in lines {
            notices.push_str(&line);
            notices.push('\n');
        }
        Ok(notices)
    }
}

/// A followed workload log session with fixed or explicitly dynamic membership.
pub enum WorkloadLogFeed {
    Merged(MergedLogFeed),
    Dynamic(Box<DynamicLogFeed>),
}
impl WorkloadLogFeed {
    pub async fn next(&mut self) -> Result<Option<String>> {
        match self {
            Self::Merged(feed) => feed.next().await,
            Self::Dynamic(feed) => feed.next().await,
        }
    }
}

/// Membership refresh cadence for explicitly opted-in dynamic sessions.
pub const MEMBERSHIP_REFRESH_INTERVAL: Duration = Duration::from_secs(10);
const MEMBERSHIP_REFRESH_FAILURES: u8 = 3;

/// Pods that left (by exact UID) and Pods that joined between two captures.
/// A same-name replacement appears in both lists, never as a continuation.
pub(crate) fn membership_changes<'a>(
    current: &'a LogMembership,
    next: &'a LogMembership,
) -> (Vec<&'a LogPod>, Vec<&'a LogPod>) {
    let left = current
        .pods
        .iter()
        .filter(|pod| !next.pods.iter().any(|other| other.uid == pod.uid))
        .collect();
    let joined = next
        .pods
        .iter()
        .filter(|pod| !current.pods.iter().any(|other| other.uid == pod.uid))
        .collect();
    (left, joined)
}

fn membership_notice(namespace: &str, pod: &LogPod, change: &str) -> String {
    bounded_line(&format!(
        "[Log membership: {namespace}/{} UID {} {change}]",
        pod.name, pod.uid
    ))
}

/// Opt-in workload tail. It periodically recaptures the same selection with the
/// complete-inventory rules of a fixed capture, announces every departure and
/// join, and opens joined Pods only after verifying their exact UID. Refresh
/// failures retain current sources and are reported; repeated transport or
/// permission failures stop the session rather than hiding stale membership.
pub struct DynamicLogFeed {
    gateway: crate::gateway::Gateway,
    api: Option<Api<Pod>>,
    membership: LogMembership,
    feed: MergedLogFeed,
    read: ReadOptions,
    options: FollowOptions,
    next_refresh: Instant,
    notices: VecDeque<String>,
    failures: u8,
    last_limit: Option<String>,
    /// Joined Pods whose streams could not all open yet (for example, still
    /// starting). They are retried each refresh and announced only once.
    waiting: std::collections::BTreeSet<String>,
}
impl DynamicLogFeed {
    pub(crate) fn new(
        gateway: crate::gateway::Gateway,
        api: Option<Api<Pod>>,
        membership: LogMembership,
        mut feed: MergedLogFeed,
        read: ReadOptions,
        options: FollowOptions,
    ) -> Result<Self> {
        if !membership.is_dynamic() {
            return Err("dynamic log feed requires an explicitly dynamic membership".into());
        }
        if read.head {
            return Err(
                "dynamic membership follows new records; choose a tail or time range, not head"
                    .into(),
            );
        }
        feed.keep_open = true;
        Ok(Self {
            gateway,
            api,
            membership,
            feed,
            read,
            options,
            next_refresh: Instant::now() + MEMBERSHIP_REFRESH_INTERVAL,
            notices: VecDeque::new(),
            failures: 0,
            last_limit: None,
            waiting: std::collections::BTreeSet::new(),
        })
    }
    pub async fn next(&mut self) -> Result<Option<String>> {
        loop {
            if let Some(notice) = self.notices.pop_front() {
                return Ok(Some(notice));
            }
            if Instant::now() >= self.feed.deadline() {
                return Err("dynamic log session deadline reached; all streams stopped; start a new session to continue".into());
            }
            if Instant::now() >= self.next_refresh {
                self.refresh().await?;
                continue;
            }
            // The merged read is cancel-safe: pending source futures stay owned
            // by the feed, so a refresh tick never drops buffered records.
            if let Ok(result) = tokio::time::timeout_at(self.next_refresh, self.feed.next()).await {
                return result;
            }
        }
    }
    async fn refresh(&mut self) -> Result<()> {
        self.next_refresh = Instant::now() + MEMBERSHIP_REFRESH_INTERVAL;
        let Some(selection) = self.membership.dynamic.clone() else {
            return Err("dynamic log membership selection unavailable".into());
        };
        let deadline = self
            .feed
            .deadline()
            .min(Instant::now() + MEMBERSHIP_CHECK_INTERVAL + CONNECT_BUDGET);
        let captured = tokio::time::timeout_at(
            deadline,
            self.gateway
                .capture_log_membership(self.membership.namespace(), selection.clone()),
        )
        .await
        .unwrap_or_else(|_| Err("membership refresh timed out".into()));
        let next = match captured {
            Ok(next) => next.into_dynamic(selection),
            Err(error) => {
                let error = safety::safe_text(&error, 400);
                // A scale-up past the bounded inventory is a visible, stable
                // condition; transport and permission failures count as strikes.
                if error.contains("exceeds") || error.contains("limit") {
                    if self.last_limit.as_deref() != Some(error.as_str()) {
                        self.notices.push_back(bounded_line(&format!(
                            "[Log membership refresh refused: {error}; current sources retained; no Pods admitted]"
                        )));
                        self.last_limit = Some(error);
                    }
                    return Ok(());
                }
                self.failures += 1;
                if self.failures >= MEMBERSHIP_REFRESH_FAILURES {
                    return Err(format!(
                        "dynamic log membership could not be refreshed three times ({error}); all streams stopped; capture again"
                    ));
                }
                self.notices.push_back(bounded_line(&format!(
                    "[Log membership refresh failed: {error}; current sources retained; no Pods admitted]"
                )));
                return Ok(());
            }
        };
        self.failures = 0;
        self.last_limit = None;
        let namespace = self.membership.namespace().to_owned();
        let (left, joined) = membership_changes(&self.membership, &next);
        for pod in left {
            let closed = self.feed.remove_pod(&namespace, &pod.name);
            self.notices.push_back(membership_notice(
                &namespace,
                pod,
                &format!("left; {closed} streams closed"),
            ));
        }
        let mut admitted = Vec::new();
        for pod in joined {
            let opened = match self.api.clone() {
                Some(api) => {
                    open_joined_pod(
                        api,
                        &namespace,
                        pod,
                        self.read,
                        self.options,
                        self.feed.deadline().min(Instant::now() + CONNECT_BUDGET),
                    )
                    .await
                }
                None => Err("joined Pods cannot be opened without a live connection".into()),
            };
            match opened {
                Ok(sources) => match self.feed.add_sources(sources) {
                    Ok(()) => {
                        self.notices.push_back(membership_notice(
                            &namespace,
                            pod,
                            &format!("joined; {} ordinary containers", pod.containers.len()),
                        ));
                        self.waiting.remove(&pod.uid);
                        admitted.push(pod.uid.clone());
                    }
                    Err(error) => self.notices.push_back(membership_notice(
                        &namespace,
                        pod,
                        &format!("not admitted: {error}"),
                    )),
                },
                Err(error) => {
                    if self.waiting.insert(pod.uid.clone()) {
                        self.notices.push_back(membership_notice(
                            &namespace,
                            pod,
                            &format!(
                                "waiting to join: {}; retried every refresh",
                                safety::safe_text(&error, 300)
                            ),
                        ));
                    }
                }
            }
        }
        // Only admitted Pods become membership; a refused join is retried next refresh.
        self.waiting
            .retain(|uid| next.pods.iter().any(|pod| &pod.uid == uid));
        let current = std::mem::replace(&mut self.membership, next);
        self.membership.pods.retain(|pod| {
            current.pods.iter().any(|old| old.uid == pod.uid) || admitted.contains(&pod.uid)
        });
        Ok(())
    }
}

async fn open_joined_pod(
    api: Api<Pod>,
    namespace: &str,
    member: &LogPod,
    read: ReadOptions,
    options: FollowOptions,
    deadline: Instant,
) -> Result<Vec<(String, Result<LogFeed>)>> {
    let pod = tokio::time::timeout_at(deadline, api.get(&member.name))
        .await
        .map_err(|_| "joined Pod verification timed out")?
        .map_err(|error| open_failure(error, false))?;
    let current = log_pod(&pod, namespace)?;
    if current.uid != member.uid || current.containers != member.containers {
        return Err("joined Pod changed before its streams opened".into());
    }
    let mut sources = Vec::new();
    for container in &member.containers {
        let identity = format!("{namespace}/{}:{container}", member.name);
        let result = async {
            let params = read.params(Some(container.clone()), false, true)?;
            let reader = tokio::time::timeout_at(deadline, api.log_stream(&member.name, &params))
                .await
                .map_err(|_| "log source connection timed out".to_owned())?
                .map_err(|error| open_failure(error, false))?;
            LogFeed::with_read_options(reader, options, read)?.with_resume(
                api.clone(),
                &member.name,
                &member.uid,
                Some(container.clone()),
                read,
            )
        }
        .await;
        // All-or-nothing: a Pod whose containers are still starting joins on
        // a later refresh instead of streaming with a silent permanent gap.
        sources.push((identity, Ok(result?)));
    }
    // The pod/log endpoint has no UID precondition; recheck after opening.
    match check_uid(&api, &member.name, &member.uid, deadline).await {
        Ok(()) => Ok(sources),
        Err(ReopenFailure::Stop(error)) => Err(error),
        Err(ReopenFailure::Retry) => {
            Err("joined Pod identity could not be verified after opening".into())
        }
    }
}

#[cfg(test)]
mod tests {
    fn workload_membership(containers: usize) -> Result<LogMembership> {
        LogMembership::new(
            "default",
            "selector app=web; fixed membership".into(),
            ["one", "two"]
                .into_iter()
                .map(|name| LogPod {
                    name: name.into(),
                    uid: format!("uid-{name}"),
                    containers: (0..containers).map(|index| format!("c{index}")).collect(),
                })
                .collect(),
            Vec::new(),
            "connection",
        )
    }
    #[test]
    fn workload_membership_is_complete_connection_bound_and_caps_pods_and_sources() -> Result<()> {
        let membership = workload_membership(2)?;
        assert_eq!(membership.pods().len(), 2);
        assert_eq!(membership.source_count(), 4);
        assert!(membership.verify_connection("connection").is_ok());
        assert!(membership.verify_connection("other").is_err());
        let json = serde_json::to_string(&membership).map_err(|_| "membership JSON")?;
        assert!(!json.contains("connection"));
        assert!(json.contains("uid-one"));
        let pod = LogPod {
            name: "one".into(),
            uid: "uid".into(),
            containers: vec!["main".into()],
        };
        assert!(
            LogMembership::new(
                "*",
                "scope".into(),
                vec![pod.clone()],
                Vec::new(),
                "connection"
            )
            .is_err()
        );
        assert!(
            LogMembership::new(
                "default",
                "scope".into(),
                vec![pod.clone(), pod],
                Vec::new(),
                "connection"
            )
            .is_err()
        );
        for (count, containers, valid) in
            [(8, 4, true), (9, 1, false), (8, 5, false), (1, 17, false)]
        {
            let pods = (0..count)
                .map(|n| LogPod {
                    name: format!("pod-{n}"),
                    uid: format!("uid-{n}"),
                    containers: (0..containers).map(|n| format!("c{n}")).collect(),
                })
                .collect();
            assert_eq!(
                LogMembership::new("default", "scope".into(), pods, Vec::new(), "connection")
                    .is_ok(),
                valid
            );
        }
        let mut bytes = MEMBERSHIP_BYTES - 2;
        assert!(membership_bytes(&mut bytes, &"data").is_err());
        assert!(bytes <= MEMBERSHIP_BYTES);
        Ok(())
    }
    #[test]
    fn dynamic_changes_are_exact_uid_departures_and_joins() -> Result<()> {
        let current = workload_membership(1)?;
        let next = LogMembership::new(
            "default",
            "selector app=web; fixed membership".into(),
            vec![
                LogPod {
                    name: "two".into(),
                    uid: "uid-two".into(),
                    containers: vec!["c0".into()],
                },
                LogPod {
                    name: "one".into(),
                    uid: "uid-replacement".into(),
                    containers: vec!["c0".into()],
                },
            ],
            Vec::new(),
            "connection",
        )?;
        let (left, joined) = membership_changes(&current, &next);
        assert_eq!(
            left.iter().map(|pod| pod.uid.as_str()).collect::<Vec<_>>(),
            ["uid-one"]
        );
        assert_eq!(
            joined
                .iter()
                .map(|pod| pod.uid.as_str())
                .collect::<Vec<_>>(),
            ["uid-replacement"],
            "a same-name replacement is a departure plus a join"
        );
        let dynamic = current.into_dynamic(PodLogSelection::Selector("app=web".into()));
        assert!(dynamic.is_dynamic());
        assert!(dynamic.description().contains("dynamic membership"));
        let json = serde_json::to_string(&dynamic).map_err(|_| "membership JSON")?;
        assert!(!json.contains("app=web\"") && !json.contains("Selector"));
        Ok(())
    }
    #[test]
    fn dynamic_merge_closes_departed_sources_admits_joins_and_stays_open() -> Result<()> {
        runtime()?.block_on(async {
            let membership =
                workload_membership(1)?.into_dynamic(PodLogSelection::Selector("app=web".into()));
            let source = |text: &str| Ok(LogFeed::new(Cursor::new(text.as_bytes().to_vec())));
            let mut feed = MergedLogFeed::from_membership_sources(
                &membership,
                vec![
                    ("one".into(), "c0".into(), source("one-record\n")),
                    ("two".into(), "c0".into(), source("two-record\n")),
                ],
                Instant::now() + Duration::from_secs(10),
                Vec::new(),
            )?;
            feed.keep_open = true;
            assert!(
                feed.next()
                    .await?
                    .is_some_and(|line| line.contains("dynamic membership opted in"))
            );
            for _ in 0..2 {
                feed.next().await?;
            }
            assert_eq!(feed.remove_pod("default", "one"), 1);
            assert_eq!(feed.remove_pod("default", "one"), 0);
            feed.add_sources(vec![("default/three:c0".into(), source("three-record\n"))])?;
            assert!(
                feed.add_sources(vec![("default/three:c0".into(), source("again\n"))])
                    .is_err(),
                "duplicate sources are refused"
            );
            let mut seen = Vec::new();
            while let Ok(Ok(Some(line))) =
                tokio::time::timeout(Duration::from_millis(200), feed.next()).await
            {
                seen.push(line);
            }
            assert!(
                seen.iter()
                    .any(|line| line.contains("[default/two:c0] two-record"))
            );
            assert!(
                seen.iter()
                    .any(|line| line.contains("[default/three:c0] three-record"))
            );
            assert!(!seen.iter().any(|line| line.contains("one-record")));
            assert!(
                !feed.ended,
                "dynamic sessions wait for joins after sources end"
            );
            let many = (0..MAX_WORKLOAD_LOG_SOURCES)
                .map(|index| (format!("default/many:c{index}"), source("x\n")))
                .collect();
            assert!(
                feed.add_sources(many).is_err(),
                "the 32-source bound is global"
            );
            Ok(())
        })
    }
    #[test]
    fn multi_pod_streams_are_attributed_fair_and_have_one_global_tail() -> Result<()> {
        runtime()?.block_on(async {
            let membership = workload_membership(1)?;
            let sources = vec![
                (
                    "one".into(),
                    "c0".into(),
                    Ok(LogFeed::new(Cursor::new(
                        b"2026-10-22T12:00:01Z one-first\n2026-10-22T12:00:03Z one-last\n".to_vec(),
                    ))),
                ),
                (
                    "two".into(),
                    "c0".into(),
                    Ok(LogFeed::new(Cursor::new(
                        b"2026-10-22T12:00:02Z two-first\n2026-10-22T12:00:04Z two-last\n".to_vec(),
                    ))),
                ),
            ];
            let mut feed = MergedLogFeed::from_membership_sources(
                &membership,
                sources,
                Instant::now() + Duration::from_secs(10),
                Vec::new(),
            )?;
            for expected in ["fixed membership", "uid-one", "uid-two"] {
                assert!(
                    feed.next()
                        .await?
                        .is_some_and(|line| line.contains(expected))
                );
            }
            assert!(
                feed.next()
                    .await?
                    .is_some_and(|line| line.contains("[default/one:c0] one-first"))
            );
            assert!(
                feed.next()
                    .await?
                    .is_some_and(|line| line.contains("[default/two:c0] two-first"))
            );
            let text = feed
                .snapshot(ReadOptions {
                    tail_lines: 2,
                    ..Default::default()
                })
                .await?;
            let data = text
                .lines()
                .filter(|line| line.starts_with("2026-"))
                .collect::<Vec<_>>();
            assert_eq!(data.len(), 2);
            assert!(data[0].ends_with("one-last"));
            assert!(data[1].ends_with("two-last"));
            assert!(text.len() <= MAX_LOG_BYTES);
            Ok(())
        })
    }
    #[test]
    fn multi_pod_partial_source_errors_survive_without_hiding_other_pod_data() -> Result<()> {
        runtime()?.block_on(async {
            let membership = workload_membership(1)?;
            let sources = vec![
                (
                    "one".into(),
                    "c0".into(),
                    Err("previous logs unavailable (Kubernetes API 400)".into()),
                ),
                (
                    "two".into(),
                    "c0".into(),
                    Ok(LogFeed::new(Cursor::new(
                        b"2026-10-22T12:00:00Z survivor\n".to_vec(),
                    ))),
                ),
            ];
            let text = MergedLogFeed::from_membership_sources(
                &membership,
                sources,
                Instant::now() + Duration::from_secs(10),
                Vec::new(),
            )?
            .snapshot(ReadOptions::default())
            .await?;
            assert!(text.contains("[Log stream default/one:c0: previous logs unavailable"));
            assert!(text.contains("[default/two:c0] survivor"));
            assert!(
                MergedLogFeed::from_membership_sources(
                    &membership,
                    Vec::new(),
                    Instant::now(),
                    Vec::new()
                )
                .is_err()
            );
            Ok(())
        })
    }
    #[test]
    fn multi_pod_global_flood_guard_and_drop_cancel_all_sources() -> Result<()> {
        runtime()?.block_on(async {
            let membership = workload_membership(1)?;
            let data = "line\n".repeat(3000);
            let sources = membership
                .pods()
                .iter()
                .map(|pod| {
                    (
                        pod.name.clone(),
                        "c0".into(),
                        Ok(LogFeed::new(Cursor::new(data.as_bytes().to_vec()))),
                    )
                })
                .collect();
            let mut feed = MergedLogFeed::from_membership_sources(
                &membership,
                sources,
                Instant::now() + Duration::from_secs(10),
                Vec::new(),
            )?;
            loop {
                match feed.next().await {
                    Ok(Some(_)) => {}
                    Ok(None) => return Err("multi-Pod flood guard missing".into()),
                    Err(error) => {
                        assert!(error.contains("shared 5000 lines"));
                        break;
                    }
                }
            }
            assert_eq!(feed.next().await?, None);
            let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let sources = membership
                .pods()
                .iter()
                .map(|pod| {
                    (
                        pod.name.clone(),
                        "c0".into(),
                        LogFeed::with_options(
                            futures_util::io::BufReader::new(QuietDrop(dropped.clone())),
                            FollowOptions::default(),
                        ),
                    )
                })
                .collect();
            let mut feed = MergedLogFeed::from_membership_sources(
                &membership,
                sources,
                Instant::now() + Duration::from_secs(10),
                Vec::new(),
            )?;
            for _ in 0..3 {
                feed.next().await?;
            }
            assert!(
                tokio::time::timeout(Duration::from_millis(5), feed.next())
                    .await
                    .is_err()
            );
            drop(feed);
            assert_eq!(dropped.load(std::sync::atomic::Ordering::SeqCst), 2);
            Ok(())
        })
    }
    #[test]
    fn quiet_multi_pod_follow_verifies_pod_and_workload_uids_without_stream_activity() -> Result<()>
    {
        runtime()?.block_on(async {
            for parent in [false,true] {
                let membership=workload_membership(1)?;
                let (api,requests)=resume_api(vec![(200,serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"namespace":"default","name":"one","uid":"replacement"}}).to_string())]);
                let guard=if parent {IdentityGuard::Parent{api:Api::namespaced_with(api.into_client(),"default",&kube::api::ApiResource::erase::<k8s_openapi::api::apps::v1::Deployment>(&())),name:"one".into(),uid:"uid-one".into()}}else{IdentityGuard::Pod{api,name:"one".into(),uid:"uid-one".into()}};
                let dropped=Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let sources=membership.pods().iter().map(|pod|(pod.name.clone(),"c0".into(),LogFeed::with_options(futures_util::io::BufReader::new(QuietDrop(dropped.clone())),FollowOptions::default()))).collect();
                let mut feed=MergedLogFeed::from_membership_sources(&membership,sources,Instant::now()+Duration::from_secs(10),vec![guard])?;
                if !parent {for _ in 0..3 {feed.next().await?;}}
                feed.next_identity_check=Some(Instant::now()+if parent{Duration::ZERO}else{Duration::from_millis(5)});
                let error=feed.next().await.err().ok_or("quiet membership replacement was ignored")?;
                assert!(error.contains("changed")||error.contains("replaced"));assert!(error.contains("all fixed-membership"));
                assert_eq!(dropped.load(std::sync::atomic::Ordering::SeqCst),2);
                assert_eq!(requests.lock().map_err(|_|"requests")?.len(),1);
                assert_eq!(feed.next().await?,None);
            }
            Ok(())
        })
    }
    #[test]
    fn workload_guard_deletion_is_fatal_and_transient_verification_is_bounded() -> Result<()> {
        runtime()?.block_on(async {
            for code in [404,503] {
                let membership=workload_membership(1)?;
                let status=serde_json::json!({"apiVersion":"v1","kind":"Status","status":"Failure","code":code,"reason":"Unavailable","message":"CANARY private server error"}).to_string();
                let (api,requests)=resume_api(vec![(code,status.clone()),(code,status.clone()),(code,status)]);
                let guard=IdentityGuard::Pod{api,name:"one".into(),uid:"uid-one".into()};
                let dropped=Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let sources=membership.pods().iter().map(|pod|(pod.name.clone(),"c0".into(),LogFeed::with_options(futures_util::io::BufReader::new(QuietDrop(dropped.clone())),FollowOptions::default()))).collect();
                let mut feed=MergedLogFeed::from_membership_sources(&membership,sources,Instant::now()+Duration::from_secs(30),vec![guard])?;
                if code==503 {
                    for _ in 0..2 {feed.next_identity_check=Some(Instant::now());let notice=feed.next().await?.ok_or("transient guard ended early")?;assert!(notice.contains("temporarily unavailable")&&!notice.contains("CANARY"));assert_eq!(dropped.load(std::sync::atomic::Ordering::SeqCst),0);}
                }
                feed.next_identity_check=Some(Instant::now());
                let error=feed.next().await.err().ok_or("guard failure did not stop")?;
                assert!(!error.contains("CANARY"));assert!(error.contains("404")||error.contains("three times"));
                assert_eq!(dropped.load(std::sync::atomic::Ordering::SeqCst),2);
                assert_eq!(requests.lock().map_err(|_|"requests")?.len(),if code==503{3}else{1});
            }
            Ok(())
        })
    }
    #[test]
    fn multi_pod_open_checks_every_uid_after_open_before_delivering_any_source() -> Result<()> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        runtime()?.block_on(async {
            for replace in [false,true] {
                let membership=workload_membership(1)?;
                let opens=Arc::new(AtomicUsize::new(0));let calls=opens.clone();
                let requests:Requests=Default::default();let recorded=requests.clone();
                let service=tower::service_fn(move|request:http::Request<kube::client::Body>|{
                    let path=request.uri().path();recorded.lock().unwrap_or_else(|e|panic!("{e}")).push(request.uri().to_string());
                    let text=if path.ends_with("/log") {
                        calls.fetch_add(1,Ordering::SeqCst);
                        format!("2026-10-22T12:00:00Z {}\n",if path.contains("/one/"){ "source-one" }else{"source-two"})
                    }else{
                        let name=path.rsplit('/').next().unwrap_or("invalid");
                        serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"namespace":"default","name":name,"uid":if replace&&name=="one"&&calls.load(Ordering::SeqCst)==2{"replaced".into()}else{format!("uid-{name}")}},"spec":{"containers":[{"name":"c0","image":"fixture"}]}}).to_string()
                    };
                    async move{Ok::<_,std::convert::Infallible>(http::Response::new(http_body_util::Full::new(bytes::Bytes::from(text))))}
                });
                let api=Api::namespaced(kube::Client::new(service,"default"),"default");
                let result=MergedLogFeed::open_membership(api,&membership,Vec::new(),false,Some(FollowOptions{seconds:10}),ReadOptions::default()).await;
                assert_eq!(opens.load(Ordering::SeqCst),2);
                if replace {assert!(result.is_err_and(|error|error.contains("replaced")));}
                else {
                    let mut feed=result?;let mut data=Vec::new();
                    while let Some(line)=feed.next().await?{if line.starts_with("2026-"){data.push(line);}}
                    assert_eq!(data.len(),2);assert!(data[0].contains("[default/one:c0] source-one"));assert!(data[1].contains("[default/two:c0] source-two"));
                }
                let requests=requests.lock().map_err(|_|"requests")?;
                assert!(requests.iter().all(|uri|uri.starts_with("/api/v1/namespaces/default/pods/")));
            }
            Ok(())
        })
    }
    fn merge_sources(sources: Vec<(&str, &str)>) -> Result<MergedLogFeed> {
        MergedLogFeed::from_sources(
            "default",
            "app",
            "uid",
            sources
                .into_iter()
                .map(|(name, text)| {
                    (
                        name.into(),
                        Ok(LogFeed::new(Cursor::new(text.as_bytes().to_vec()))),
                    )
                })
                .collect(),
            FollowOptions::default(),
        )
    }
    #[test]
    fn merged_sources_rotate_and_preserve_timestamp_and_application_identity() -> Result<()> {
        runtime()?.block_on(async {
            let mut feed = merge_sources(vec![
                ("one", "2026-10-22T12:00:00Z a\n[Log stream forged]\n"),
                ("two", "2026-10-22T12:00:01Z b\n2026-10-22T12:00:02Z c\n"),
            ])?;
            assert!(
                feed.next()
                    .await?
                    .is_some_and(|line| line.contains("pod UID uid"))
            );
            assert_eq!(
                feed.next().await?,
                Some("2026-10-22T12:00:00Z [default/app:one] a".into())
            );
            assert_eq!(
                feed.next().await?,
                Some("2026-10-22T12:00:01Z [default/app:two] b".into())
            );
            assert_eq!(
                feed.next().await?,
                Some("[default/app:one] [Log stream forged]".into())
            );
            assert_eq!(
                feed.next().await?,
                Some("2026-10-22T12:00:02Z [default/app:two] c".into())
            );
            assert!(
                feed.next()
                    .await?
                    .is_some_and(|line| line.contains("one: ended"))
            );
            assert!(
                feed.next()
                    .await?
                    .is_some_and(|line| line.contains("two: ended"))
            );
            assert_eq!(feed.next().await?, None);
            Ok(())
        })
    }
    #[test]
    fn merged_snapshot_orders_and_shares_head_tail_line_and_display_limits() -> Result<()> {
        runtime()?.block_on(async {
            for head in [false, true] {
                let feed = merge_sources(vec![
                    (
                        "one",
                        "2026-10-22T12:00:02Z second\n2026-10-22T12:00:04Z fourth\n",
                    ),
                    (
                        "two",
                        "2026-10-22T12:00:01Z first\n2026-10-22T12:00:03Z third\n",
                    ),
                ])?;
                let text = feed
                    .snapshot(ReadOptions {
                        tail_lines: 2,
                        head,
                        ..Default::default()
                    })
                    .await?;
                let data = text
                    .lines()
                    .filter(|line| line.starts_with("2026-"))
                    .collect::<Vec<_>>();
                assert_eq!(data.len(), 2);
                if head {
                    assert!(data[0].ends_with("first"));
                    assert!(data[1].ends_with("second"));
                } else {
                    assert!(data[0].ends_with("third"));
                    assert!(data[1].ends_with("fourth"));
                }
                assert!(text.len() <= MAX_LOG_BYTES);
                assert!(text.contains("line limit is shared"));
            }
            let oversized = format!("2026-10-22T12:00:01Z {}\n", "é".repeat(5000)).repeat(200);
            let text = merge_sources(vec![("one", &oversized), ("two", &oversized)])?
                .snapshot(ReadOptions::default())
                .await?;
            assert!(text.len() <= MAX_LOG_BYTES);
            assert!(text.contains("shared"));
            Ok(())
        })
    }
    #[test]
    fn merged_line_byte_and_total_budgets_are_global_and_fatal() -> Result<()> {
        runtime()?.block_on(async {
            let many = "x\n".repeat(3000);
            let mut feed = merge_sources(vec![("one", &many), ("two", &many)])?;
            let mut delivered = 0;
            let error = loop {
                match feed.next().await {
                    Ok(Some(_)) => delivered += 1,
                    Ok(None) => return Err("shared line guard failed".into()),
                    Err(error) => break error,
                }
            };
            assert!(error.contains("shared 5000 lines"));
            assert!(delivered <= MAX_RATE_LINES + 1);
            assert_eq!(feed.next().await?, None);
            let bytes = format!("{}\n", "x".repeat(3999)).repeat(160);
            let mut feed = merge_sources(vec![("one", &bytes), ("two", &bytes)])?;
            let error = loop {
                match feed.next().await {
                    Ok(Some(_)) => {}
                    Ok(None) => return Err("shared byte rate guard failed".into()),
                    Err(error) => break error,
                }
            };
            assert!(error.contains("shared 1 MiB/second"));
            assert_eq!(feed.next().await?, None);
            let mut feed = merge_sources(vec![("one", "abc\n"), ("two", "def\n")])?;
            feed.budget.lock().map_err(|_| "test budget")?.total_bytes =
                crate::transport::LOG_STREAM_BYTES - 2;
            assert!(feed.next().await?.is_some());
            assert!(
                feed.next()
                    .await
                    .is_err_and(|error| error.contains("shared 64 MiB"))
            );
            assert_eq!(feed.next().await?, None);
            Ok(())
        })
    }
    struct QuietDrop(Arc<std::sync::atomic::AtomicUsize>);
    impl Drop for QuietDrop {
        fn drop(&mut self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    impl futures_util::io::AsyncRead for QuietDrop {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            _: &mut [u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Pending
        }
    }
    fn quiet_merge(dropped: Arc<std::sync::atomic::AtomicUsize>) -> Result<MergedLogFeed> {
        MergedLogFeed::from_sources(
            "default",
            "app",
            "uid",
            ["one", "two"]
                .into_iter()
                .map(|name| {
                    (
                        name.into(),
                        LogFeed::with_options(
                            futures_util::io::BufReader::new(QuietDrop(dropped.clone())),
                            FollowOptions::default(),
                        ),
                    )
                })
                .collect(),
            FollowOptions::default(),
        )
    }
    #[test]
    fn merged_cancel_and_shared_quiet_deadline_drop_every_pending_reader() -> Result<()> {
        runtime()?.block_on(async {
            let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let mut feed = quiet_merge(dropped.clone())?;
            assert!(feed.next().await?.is_some());
            assert!(
                tokio::time::timeout(Duration::from_millis(5), feed.next())
                    .await
                    .is_err()
            );
            drop(feed);
            assert_eq!(dropped.load(std::sync::atomic::Ordering::SeqCst), 2);
            let mut feed = quiet_merge(dropped.clone())?;
            feed.deadline = Instant::now() + Duration::from_millis(5);
            assert!(feed.next().await?.is_some());
            assert!(
                feed.next()
                    .await
                    .is_err_and(|error| error.contains("deadline"))
            );
            assert_eq!(dropped.load(std::sync::atomic::Ordering::SeqCst), 4);
            assert_eq!(feed.next().await?, None);
            Ok(())
        })
    }
    #[test]
    fn merged_reconnect_replacement_stops_and_drops_every_source() -> Result<()> {
        runtime()?.block_on(async {
            let (api, _) = resume_api(vec![(200, metadata("replacement"))]);
            let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let first = broken_feed(api, "2026-10-22T12:00:00Z first\n")?;
            let quiet = LogFeed::with_options(
                futures_util::io::BufReader::new(QuietDrop(dropped.clone())),
                FollowOptions::default(),
            )?;
            let mut feed = MergedLogFeed::from_sources(
                "default",
                "app",
                "uid",
                vec![("one".into(), Ok(first)), ("two".into(), Ok(quiet))],
                FollowOptions::default(),
            )?;
            assert!(feed.next().await?.is_some());
            assert!(
                feed.next()
                    .await?
                    .is_some_and(|line| line.ends_with(" first"))
            );
            assert!(
                feed.next()
                    .await?
                    .is_some_and(|line| line.contains("interrupted"))
            );
            assert!(
                feed.next()
                    .await
                    .is_err_and(|error| error.contains("replaced")
                        && error.contains("all merged streams stopped"))
            );
            assert_eq!(dropped.load(std::sync::atomic::Ordering::SeqCst), 1);
            assert_eq!(feed.next().await?, None);
            Ok(())
        })
    }
    fn merge_pod(uid: &str, names: &[&str]) -> String {
        serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"namespace":"default","name":"app","uid":uid},"spec":{"containers":names.iter().map(|name| serde_json::json!({"name":name,"image":"fixture"})).collect::<Vec<_>>(),"initContainers":[{"name":"init","image":"fixture"}],"ephemeralContainers":[{"name":"debug","image":"fixture"}]} }).to_string()
    }
    #[test]
    fn merged_scope_source_count_and_uid_are_validated_before_reading_logs() -> Result<()> {
        let pod: Pod =
            serde_json::from_str(&merge_pod("uid", &["one", "two"])).map_err(|_| "test pod")?;
        assert_eq!(
            ordinary_container_names(&pod, "default", "app", "uid")?,
            ["one", "two"]
        );
        assert!(ordinary_container_names(&pod, "other", "app", "uid").is_err());
        assert!(ordinary_container_names(&pod, "default", "app", "recreated").is_err());
        let too_many = (0..=MAX_MERGED_CONTAINERS)
            .map(|n| format!("c{n}"))
            .collect::<Vec<_>>();
        let pod: Pod = serde_json::from_str(&merge_pod(
            "uid",
            &too_many.iter().map(String::as_str).collect::<Vec<_>>(),
        ))
        .map_err(|_| "test pod")?;
        assert!(ordinary_container_names(&pod, "default", "app", "uid").is_err());
        runtime()?.block_on(async {
            let (api, requests) = resume_api(vec![(200, merge_pod("recreated", &["one", "two"]))]);
            assert!(
                MergedLogFeed::open(
                    api,
                    "app",
                    "uid",
                    false,
                    Some(FollowOptions::default()),
                    ReadOptions::default()
                )
                .await
                .is_err()
            );
            assert_eq!(requests.lock().map_err(|_| "requests")?.len(), 1);
            Ok(())
        })
    }
    #[test]
    fn merged_previous_failure_is_source_specific_and_post_open_replacement_is_rejected()
    -> Result<()> {
        runtime()?.block_on(async {
            let (api, requests) = resume_api(vec![(200, merge_pod("uid", &["one", "two"])), (400, serde_json::json!({"apiVersion":"v1","kind":"Status","status":"Failure","message":"CANARY should not leak","reason":"BadRequest","code":400}).to_string()), (200, "2026-10-22T12:00:00Z surviving\n".into()), (200, metadata("uid"))]);
            let feed = MergedLogFeed::open(api, "app", "uid", true, None, ReadOptions::default()).await?;
            let text = feed.snapshot(ReadOptions::default()).await?;
            assert!(text.contains("one: previous logs unavailable (Kubernetes API 400)"));
            assert!(text.contains("[default/app:two] surviving"));
            assert!(!text.contains("CANARY"));
            {
                let requests = requests.lock().map_err(|_| "requests")?;
                let log_requests = requests.iter().filter(|request| request.contains("/log?")).collect::<Vec<_>>();
                assert_eq!(log_requests.len(), 2);
                assert!(log_requests.iter().all(|uri| uri.contains("previous=true") && uri.contains("timestamps=true")));
            }
            let (api, _) = resume_api(vec![(200, merge_pod("uid", &["one", "two"])), (200, "secret first\n".into()), (200, "secret second\n".into()), (200, metadata("replacement"))]);
            assert!(MergedLogFeed::open(api, "app", "uid", false, Some(FollowOptions::default()), ReadOptions::default()).await.is_err_and(|error| error.contains("replaced")));
            Ok(())
        })
    }
    type Requests = std::sync::Arc<std::sync::Mutex<Vec<String>>>;
    #[test]
    fn merged_open_concurrency_is_four_and_never_fetches_init_or_ephemeral_logs() -> Result<()> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        runtime()?.block_on(async {
            let active = Arc::new(AtomicUsize::new(0));
            let maximum = Arc::new(AtomicUsize::new(0));
            let requests: Requests = Default::default();
            let (active_service, maximum_service, requests_service) =
                (active.clone(), maximum.clone(), requests.clone());
            let service = tower::service_fn(move |request: http::Request<kube::client::Body>| {
                let (active, maximum, requests) = (
                    active_service.clone(),
                    maximum_service.clone(),
                    requests_service.clone(),
                );
                async move {
                    let uri = request.uri().to_string();
                    requests
                        .lock()
                        .unwrap_or_else(|e| panic!("{e}"))
                        .push(uri.clone());
                    let text = if request.uri().path().ends_with("/log") {
                        let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                        maximum.fetch_max(current, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(5)).await;
                        active.fetch_sub(1, Ordering::SeqCst);
                        "2026-10-22T12:00:00Z fixture\n".into()
                    } else {
                        merge_pod("uid", &["one", "two", "three", "four", "five", "six"])
                    };
                    Ok::<_, std::convert::Infallible>(http::Response::new(
                        http_body_util::Full::new(bytes::Bytes::from(text)),
                    ))
                }
            });
            let api = Api::namespaced(kube::Client::new(service, "default"), "default");
            let feed =
                MergedLogFeed::open(api, "app", "uid", false, None, ReadOptions::default()).await?;
            let text = feed.snapshot(ReadOptions::default()).await?;
            assert_eq!(maximum.load(Ordering::SeqCst), MERGED_OPEN_CONCURRENCY);
            assert_eq!(active.load(Ordering::SeqCst), 0);
            let requests = requests.lock().map_err(|_| "requests")?;
            let log_requests = requests
                .iter()
                .filter(|uri| uri.contains("/log?"))
                .collect::<Vec<_>>();
            assert_eq!(log_requests.len(), 6);
            assert!(
                !log_requests
                    .iter()
                    .any(|uri| uri.contains("container=init") || uri.contains("container=debug"))
            );
            for name in ["one", "two", "three", "four", "five", "six"] {
                assert!(text.contains(&format!("[default/app:{name}] fixture")));
            }
            Ok(())
        })
    }
    fn resume_api(replies: Vec<(u16, String)>) -> (Api<Pod>, Requests) {
        let replies = std::sync::Arc::new(std::sync::Mutex::new(VecDeque::from(replies)));
        let requests: Requests = Default::default();
        let recorded = requests.clone();
        let service = tower::service_fn(move |request: http::Request<kube::client::Body>| {
            recorded
                .lock()
                .unwrap_or_else(|e| panic!("{e}"))
                .push(request.uri().to_string());
            let (status, text) = replies
                .lock()
                .unwrap_or_else(|e| panic!("{e}"))
                .pop_front()
                .unwrap_or_else(|| panic!("unexpected log request"));
            async move {
                Ok::<_, std::convert::Infallible>(
                    http::Response::builder()
                        .status(status)
                        .body(http_body_util::Full::new(bytes::Bytes::from(text)))
                        .unwrap_or_else(|e| panic!("{e}")),
                )
            }
        });
        (
            Api::namespaced(kube::Client::new(service, "default"), "default"),
            requests,
        )
    }
    fn metadata(uid: &str) -> String {
        serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"namespace":"default","name":"app","uid":uid}}).to_string()
    }
    struct Broken;
    impl futures_util::io::AsyncRead for Broken {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            _: &mut [u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Ready(Err(std::io::Error::other(
                "CANARY transport detail must not be displayed",
            )))
        }
    }
    fn broken_feed(api: Api<Pod>, prefix: &str) -> Result<LogFeed> {
        let reader =
            futures_util::io::BufReader::new(Cursor::new(prefix.as_bytes().to_vec()).chain(Broken));
        LogFeed::with_options(reader, FollowOptions::default())?.with_resume(
            api,
            "app",
            "uid",
            Some("main".into()),
            ReadOptions {
                since_seconds: Some(300),
                ..Default::default()
            },
        )
    }
    #[test]
    fn transport_resume_floors_since_time_checks_uid_and_labels_overlap_and_gaps() -> Result<()> {
        runtime()?.block_on(async {
            let (api, requests) = resume_api(vec![
                (200, metadata("uid")),
                (
                    200,
                    "2026-10-22T12:00:56.9Z repeated\n2026-10-22T12:00:57Z next\n".into(),
                ),
                (200, metadata("uid")),
            ]);
            let mut feed = broken_feed(api, "2026-10-22T12:00:56.9Z first\n")?;
            let deadline = feed.deadline;
            assert!(
                feed.next()
                    .await?
                    .is_some_and(|line| line.ends_with(" first"))
            );
            assert!(
                feed.next()
                    .await?
                    .is_some_and(|line| line.contains("interrupted"))
            );
            let notice = feed.next().await?.ok_or("missing reconnect notice")?;
            assert!(notice.contains("overlap may repeat"));
            assert!(notice.contains("may leave gaps"));
            assert_eq!(feed.deadline, deadline);
            assert!(
                feed.next()
                    .await?
                    .is_some_and(|line| line.ends_with(" repeated"))
            );
            assert!(
                feed.next()
                    .await?
                    .is_some_and(|line| line.ends_with(" next"))
            );
            assert_eq!(feed.next().await?, None); // A clean container EOF is final.
            let requests = requests.lock().unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(requests.len(), 3);
            assert!(requests[1].contains("sinceTime=2026-10-22T12%3A00%3A56Z"));
            assert!(!requests[1].contains("tailLines"));
            assert!(!requests[1].contains("sinceSeconds"));
            assert!(requests[1].contains("container=main"));
            Ok(())
        })
    }
    #[test]
    fn reconnect_stops_on_replacement_denial_missing_timestamp_and_original_budgets() -> Result<()>
    {
        runtime()?.block_on(async {
            for reply in [(200, metadata("replacement")), (403, serde_json::json!({"apiVersion":"v1","kind":"Status","status":"Failure","code":403,"reason":"Forbidden","message":"CANARY"}).to_string())] {
                let (api, requests) = resume_api(vec![reply]);
                let mut feed = broken_feed(api, "2026-10-22T12:00:56Z first\n")?;
                feed.next().await?;
                assert!(feed.next().await?.is_some_and(|line| line.contains("interrupted")));
                let error = feed.next().await.err().ok_or("expected stopped reconnect")?;
                assert!(!error.contains("CANARY"));
                assert!(error.contains("replaced") || error.contains("403"));
                assert_eq!(feed.next().await?, None);
                assert_eq!(requests.lock().unwrap_or_else(|e| panic!("{e}")).len(), 1);
            }
            for budget in [0, 1, 2, 3] {
                let (api, requests) = resume_api(vec![]);
                let mut feed = broken_feed(api, if budget == 0 {"not timestamped\n"} else {"2026-10-22T12:00:56Z first\n"})?;
                feed.next().await?;
                match budget {
                    1 => feed.total_bytes = crate::transport::LOG_STREAM_BYTES,
                    2 => feed.deadline = Instant::now() + Duration::from_millis(10),
                    3 => feed.reconnects = MAX_RECONNECTS,
                    _ => {},
                }
                let result = feed.next().await;
                let error = if matches!(result, Ok(Some(_))) {
                    feed.next().await.err().ok_or("expected deferred local log stop")?
                } else {
                    result.err().ok_or("expected local log stop")?
                };
                assert!(!error.contains("CANARY"));
                assert_eq!(feed.next().await?, None);
                assert!(requests.lock().unwrap_or_else(|e| panic!("{e}")).is_empty());
            }
            Ok(())
        })
    }
    #[test]
    fn head_and_tail_preserve_the_requested_end_and_snapshot_bounds() -> Result<()> {
        let input = (0..10)
            .map(|index| format!("line-{index}\n"))
            .collect::<String>();
        let mut options = ReadOptions {
            tail_lines: 3,
            ..Default::default()
        };
        assert_eq!(snapshot_text(&input, options), "line-7\nline-8\nline-9\n");
        options.head = true;
        assert_eq!(snapshot_text(&input, options), "line-0\nline-1\nline-2\n");
        let huge = format!("first\n{}\nlast\n", "é".repeat(MAX_LOG_BYTES));
        let text = snapshot_text(&huge, options);
        assert!(text.starts_with("first\n"));
        assert!(text.contains(LINE_NOTICE));
        assert!(text.contains("may be incomplete"));
        assert!(text.len() <= MAX_LOG_BYTES);
        let mut rolling = RollingLog::default();
        rolling.push(&"é".repeat(MAX_LINE_BYTES));
        assert!(rolling.text().len() <= MAX_LINE_BYTES + 1);
        assert!(rolling.text().contains(LINE_NOTICE));
        runtime()?.block_on(async {
            let mut feed = LogFeed::with_read_options(
                Cursor::new(input.into_bytes()),
                FollowOptions::default(),
                options,
            )?;
            assert_eq!(feed.next().await?, Some("line-0".into()));
            assert_eq!(feed.next().await?, Some("line-1".into()));
            assert_eq!(feed.next().await?, Some("line-2".into()));
            assert_eq!(feed.next().await?, None);
            Ok(())
        })
    }
    #[test]
    fn range_shortcuts_produce_distinct_bounded_server_requests() -> Result<()> {
        for (key, since) in [
            ('0', None),
            ('2', Some(60)),
            ('3', Some(300)),
            ('4', Some(900)),
            ('5', Some(1800)),
            ('6', Some(3600)),
        ] {
            let options = ReadOptions::for_key(key).ok_or("missing range shortcut")?;
            let params = options.params(Some("app".into()), false, true)?;
            assert_eq!(params.since_seconds, since);
            assert_eq!(params.tail_lines, Some(200));
            assert!(params.follow);
            assert!(params.timestamps);
            assert_eq!(params.limit_bytes, None);
        }
        let head = ReadOptions::for_key('1').ok_or("missing head shortcut")?;
        let params = head.params(None, false, true)?;
        assert!(!params.follow);
        assert_eq!(params.tail_lines, None);
        assert_eq!(params.limit_bytes, Some(MAX_LOG_BYTES as i64));
        assert!(params.since_seconds.is_none());
        assert!(ReadOptions::for_key('7').is_none());
        assert!(
            ReadOptions {
                tail_lines: 0,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            ReadOptions {
                tail_lines: 2001,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            ReadOptions {
                since_seconds: Some(0),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            ReadOptions {
                since_seconds: Some(86_401),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            ReadOptions {
                head: true,
                since_seconds: Some(60),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(ReadOptions::default().params(None, true, true).is_err());
        assert!(ReadOptions::default().params(None, true, false)?.previous);
        Ok(())
    }
    #[test]
    fn timestamps_toggle_locally_without_hiding_application_text() {
        assert_eq!(
            without_timestamp("2026-10-22T12:03:04.123456789Z payload"),
            "payload"
        );
        assert_eq!(
            without_timestamp("2026-10-22T13:03:04+01:00 payload"),
            "payload"
        );
        assert_eq!(
            without_timestamp("2026-02-30T12:00:00Z payload"),
            "2026-02-30T12:00:00Z payload"
        );
        assert_eq!(
            without_timestamp("app 2026-10-22T12:00:00Z"),
            "app 2026-10-22T12:00:00Z"
        );
        assert_eq!(
            without_timestamp("2026-10-22T12:00:00Z"),
            "2026-10-22T12:00:00Z"
        );
        let mut buffer = RollingLog::default();
        buffer.push("2026-10-22T12:03:04Z INFO served");
        buffer.push("2026-10-22T12:03:05Z ERROR failed\x1b[31m");
        let filtered = buffer.filtered_text(false, |line| line.contains("ERROR"));
        assert_eq!(filtered, "ERROR failed[31m\n");
        assert_eq!(
            buffer.filtered_text(false, |line| line.contains("2026-10-22")),
            ""
        );
        assert_eq!(buffer.line_count(), 2);
        assert!(buffer.text().contains("2026-10-22T12:03:04Z"));
        buffer.clear();
        assert!(buffer.text().is_empty());
        assert_eq!(buffer.dropped(), 0);
    }
    #[test]
    fn truncated_notice_survives_the_rolling_ui_pipeline() -> Result<()> {
        runtime()?.block_on(async {
            let mut feed =
                LogFeed::new(Cursor::new(format!("{}\n", "x".repeat(5000)).into_bytes()));
            let line = feed.next().await?.ok_or("missing long line")?;
            assert!(line.chars().count() <= MAX_LINE_BYTES);
            let mut buffer = RollingLog::default();
            buffer.push(&safety::safe_text(&line, MAX_LINE_BYTES));
            assert!(buffer.text().contains("[line truncated at 4096 bytes]"));
            Ok(())
        })
    }
    use super::*;
    use futures_util::io::Cursor;
    fn runtime() -> Result<tokio::runtime::Runtime> {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(|e| e.to_string())
    }
    #[test]
    fn invalid_utf8_and_expired_session_are_safe() -> Result<()> {
        runtime()?.block_on(async {
            let mut feed = LogFeed::new(Cursor::new(vec![0xc3, b'\n', 0xff]));
            assert_eq!(feed.next().await?, Some("�".into()));
            assert_eq!(feed.next().await?, Some("�".into()));
            let mut expired = LogFeed::new(Cursor::new(b"never displayed".to_vec()));
            expired.deadline = Instant::now();
            assert!(expired.next().await.is_err());
            assert_eq!(expired.next().await?, None);
            Ok(())
        })
    }
    #[test]
    fn sanitizes_controls_and_preserves_utf8_across_chunks() -> Result<()> {
        runtime()?.block_on(async {
            let mut bytes = vec![b'a'; 4095];
            bytes.extend_from_slice("é\n".as_bytes());
            let mut too_long = LogFeed::new(Cursor::new(bytes));
            let truncated = too_long.next().await?.ok_or("missing truncated line")?;
            assert!(truncated.ends_with("[line truncated at 4096 bytes]"));
            assert!(truncated.len() < 13_000);
            let mut feed = LogFeed::new(Cursor::new(
                "ok\x1b[31m\r\nlasté\u{202e}".as_bytes().to_vec(),
            ));
            assert_eq!(feed.next().await?, Some("ok[31m".into()));
            assert_eq!(feed.next().await?, Some("lasté".into()));
            assert_eq!(feed.next().await?, None);
            Ok(())
        })
    }
    #[test]
    fn follows_beyond_old_total_limit_and_stops_floods() -> Result<()> {
        runtime()?.block_on(async {
            let line = format!("{}\n", "x".repeat(1000));
            let mut feed = LogFeed::new(Cursor::new(line.repeat(300).into_bytes()));
            let mut count = 0;
            while feed.next().await?.is_some() {
                count += 1;
            }
            assert_eq!(count, 300); // More than 256 KiB; fixed memory is independent of total traffic.
            let mut flood = LogFeed::new(Cursor::new(vec![b'\n'; MAX_RATE_LINES + 1]));
            for _ in 0..MAX_RATE_LINES {
                assert!(flood.next().await?.is_some());
            }
            assert!(flood.next().await.is_err());
            assert_eq!(flood.next().await?, None);
            let mut bytes = LogFeed::new(Cursor::new(vec![b'x'; MAX_RATE_BYTES + 1]));
            assert!(bytes.next().await.is_err());
            Ok(())
        })
    }
    #[test]
    fn rolling_cache_evicts_old_data_and_keeps_utf8_and_sanitization() {
        let mut buffer = RollingLog::default();
        for i in 0..3000 {
            buffer.push(&format!("line {i}: {}\x1b]52;secret\x07", "é".repeat(1000)));
        }
        let text = buffer.text();
        assert!(text.len() <= MAX_LOG_BYTES);
        assert!(buffer.line_count() <= MAX_LOG_LINES);
        assert!(buffer.dropped() > 0);
        assert!(text.contains("line 2999:"));
        assert!(!text.contains("line 0:"));
        assert!(!text.contains('\x1b'));
        assert!(text.contains("Rolling buffer"));
    }
    #[test]
    fn validates_lifetime_and_quiet_stream_deadline() -> Result<()> {
        assert!(FollowOptions { seconds: 0 }.validate().is_err());
        assert!(FollowOptions { seconds: 86_401 }.validate().is_err());
        assert_eq!(FollowOptions::default().seconds, 3600);
        assert!(FollowOptions { seconds: 86_400 }.validate().is_ok());
        runtime()?.block_on(async {
            struct Quiet;
            impl futures_util::io::AsyncRead for Quiet {
                fn poll_read(
                    self: Pin<&mut Self>,
                    _: &mut std::task::Context<'_>,
                    _: &mut [u8],
                ) -> std::task::Poll<std::io::Result<usize>> {
                    std::task::Poll::Pending
                }
            }
            impl AsyncBufRead for Quiet {
                fn poll_fill_buf(
                    self: Pin<&mut Self>,
                    _: &mut std::task::Context<'_>,
                ) -> std::task::Poll<std::io::Result<&[u8]>> {
                    std::task::Poll::Pending
                }
                fn consume(self: Pin<&mut Self>, _: usize) {}
            }
            let mut feed = LogFeed::with_options(Quiet, FollowOptions { seconds: 1 })?;
            let started = Instant::now();
            assert!(feed.next().await.is_err());
            assert!(started.elapsed() >= Duration::from_millis(900));
            assert!(started.elapsed() < Duration::from_secs(3));
            assert_eq!(feed.next().await?, None);
            Ok(())
        })
    }
    #[test]
    fn maximum_line_and_empty_lines_are_supported() -> Result<()> {
        runtime()?.block_on(async {
            let mut data = vec![b'x'; MAX_LINE_BYTES];
            data.extend_from_slice(b"\n\nend");
            let mut feed = LogFeed::new(Cursor::new(data));
            assert_eq!(feed.next().await?.map(|x| x.len()), Some(MAX_LINE_BYTES));
            assert_eq!(feed.next().await?, Some(String::new()));
            assert_eq!(feed.next().await?, Some("end".into()));
            Ok(())
        })
    }
}
