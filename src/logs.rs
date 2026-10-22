//! Pull-driven, time-bounded log follow with a rolling display and flood guards.
use crate::{Result, safety};
use futures_util::{
    StreamExt,
    io::{AsyncBufRead, AsyncReadExt},
};
use k8s_openapi::api::core::v1::Pod;
use kube::{Api, api::LogParams};
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
const MERGED_OPEN_CONCURRENCY: usize = 4;

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
}

#[derive(Clone)]
struct ResumeSource {
    api: Api<Pod>,
    name: String,
    uid: String,
    params: LogParams,
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
            match check_uid(&source.api, &source.name, &source.uid, deadline).await {
                Ok(()) => {}
                Err(ReopenFailure::Retry) => continue,
                Err(ReopenFailure::Stop(message)) => {
                    return Err(self.stop_merge_for_identity(message));
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
                            return Err(self.stop_merge_for_identity(message));
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
    header: Option<String>,
    ended: bool,
    snapshot_mode: bool,
}
impl MergedLogFeed {
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
        let budget: SharedMergeBudget = Arc::new(Mutex::new(MergeBudget::default()));
        let mut names = Vec::with_capacity(sources.len());
        let mut merged = Vec::with_capacity(sources.len());
        for (container, result) in sources {
            if !safety::valid_id(&container) || names.contains(&container) {
                return Err("invalid or duplicate merged log source".into());
            }
            names.push(container.clone());
            let identity = format!("{ns}/{name}:{container}");
            let (pending, notice) = match result {
                Ok(mut feed) => {
                    feed.deadline = deadline;
                    feed.merge_budget = Some(budget.clone());
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
            header: Some(format!(
                "[Log stream merge: {} ordinary containers; pod UID {uid}; delivery in arrival order, source timestamps preserved; init/ephemeral excluded]",
                names.len()
            )),
            ended: false,
            snapshot_mode: false,
        })
    }
    pub async fn next(&mut self) -> Result<Option<String>> {
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
        if let Some(header) = self.header.take() {
            return Ok(Some(header));
        }
        let count = self.sources.len();
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
            if active {
                Poll::Pending
            } else {
                Poll::Ready(None)
            }
        });
        let ready = match tokio::time::timeout_at(self.deadline, ready).await {
            Ok(ready) => ready,
            Err(_) => {
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
        match result {
            Ok(Some(line)) => {
                let output = source_line(&self.sources[index].identity, &line, feed.last_control);
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
        const NOTICE_BYTES: usize = 16 * 1024;
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
                if notices.len() + line.len() < NOTICE_BYTES - 1024 {
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
            while lines.len() > options.tail_lines as usize || bytes > MAX_LOG_BYTES - NOTICE_BYTES
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

#[cfg(test)]
mod tests {
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
