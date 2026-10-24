//! Shared byte, line and rate budget for merged log sessions.
use super::{MAX_RATE_BYTES, MAX_RATE_LINES};
use crate::Result;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::Instant;

#[derive(Default)]
pub(super) struct MergeBudget {
    pub(super) total_bytes: usize,
    rate_epoch: Option<Instant>,
    rate_bytes: usize,
    rate_lines: usize,
    pub(super) stopped: Option<String>,
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
pub(super) type SharedMergeBudget = Arc<Mutex<MergeBudget>>;
pub(super) fn charge_merge(budget: &SharedMergeBudget, bytes: usize, lines: usize) -> Result<()> {
    budget
        .lock()
        .map_err(|_| "merged log budget unavailable")?
        .charge(bytes, lines)
}
