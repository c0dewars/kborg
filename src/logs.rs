//! Pull-driven, time-bounded log follow with a rolling display and flood guards.
use crate::{Result, safety};
use futures_util::io::{AsyncBufRead, AsyncReadExt};
use k8s_openapi::api::core::v1::Pod;
use kube::{Api, api::LogParams};
use std::{collections::VecDeque, pin::Pin, time::Duration};
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
                Err(ReopenFailure::Stop(message)) => return Err(message),
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
                        Err(ReopenFailure::Stop(message)) => return Err(message),
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
        if self.ended {
            return Ok(None);
        }
        if self.reconnect_pending {
            return match self.reconnect().await {
                Ok(message) => {
                    self.reconnect_pending = false;
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

#[cfg(test)]
mod tests {
    type Requests = std::sync::Arc<std::sync::Mutex<Vec<String>>>;
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
