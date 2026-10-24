//! Server-side read ranges and bounded follow lifetimes.
use super::{DEFAULT_FOLLOW_SECONDS, MAX_FOLLOW_SECONDS, MAX_LOG_BYTES, MAX_LOG_LINES};
use crate::Result;

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
