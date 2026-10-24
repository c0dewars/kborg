//! Single-source log follow with flood guards and identity-bound resume.
use super::{
    CONNECT_BUDGET, FollowOptions, IdentityGuard, MAX_LINE_BYTES, MAX_RATE_BYTES, MAX_RATE_LINES,
    MAX_RECONNECTS, ReadOptions,
    budget::{SharedMergeBudget, charge_merge},
    identity::{ReopenFailure, check_uid, reopen_failure, verify_guards},
    rolling::{LINE_NOTICE, bounded_line},
};
use crate::{Result, safety};
use futures_util::io::{AsyncBufRead, AsyncReadExt};
use k8s_openapi::api::core::v1::Pod;
use kube::{Api, api::LogParams};
use std::{pin::Pin, sync::Arc, time::Duration};
use tokio::time::Instant;

pub struct LogFeed {
    reader: Pin<Box<dyn AsyncBufRead + Send>>,
    pending: Vec<u8>,
    chunk: [u8; 4096],
    offset: usize,
    available: usize,
    pub(super) deadline: Instant,
    seconds: u32,
    rate_epoch: Instant,
    rate_bytes: usize,
    rate_lines: usize,
    discarding: bool,
    ended: bool,
    remaining_lines: Option<u32>,
    pub(super) total_bytes: usize,
    pub(super) resume: Option<ResumeSource>,
    pub(super) reconnects: u32,
    last_timestamp: Option<String>,
    reconnect_pending: bool,
    pub(super) merge_budget: Option<SharedMergeBudget>,
    pub(super) last_control: bool,
    /// Dynamic membership: a replaced or deleted Pod ends only its own source.
    pub(super) isolate_identity: bool,
}

#[derive(Clone)]
pub(super) struct ResumeSource {
    api: Api<Pod>,
    name: String,
    uid: String,
    params: LogParams,
    pub(super) guards: Arc<Vec<IdentityGuard>>,
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
