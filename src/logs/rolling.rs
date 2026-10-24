//! Bounded line sanitisation, snapshots and the rolling display buffer.
use super::{MAX_LINE_BYTES, MAX_LOG_BYTES, MAX_LOG_LINES, ReadOptions};
use crate::safety;
use std::collections::VecDeque;

/// Strip only the Kubernetes RFC3339 timestamp prefix, leaving application text
/// and malformed timestamp-looking prefixes intact. No shell or regex parsing.
pub fn without_timestamp(line: &str) -> &str {
    match line.split_once(' ') {
        Some((timestamp, message)) if crate::metrics::timestamp_key(timestamp).is_some() => message,
        _ => line,
    }
}

pub(super) const LINE_NOTICE: &str = " [line truncated at 4096 bytes]";

pub(super) fn bounded_line(line: &str) -> String {
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
