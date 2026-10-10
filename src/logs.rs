//! Pull-driven, time-bounded log follow with a rolling display and flood guards.
use crate::{Result, safety};
use futures_util::io::{AsyncBufRead, AsyncReadExt};
use std::{collections::VecDeque, pin::Pin, time::Duration};
use tokio::time::Instant;

pub const MAX_LOG_BYTES: usize = 256 * 1024;
pub const MAX_LINE_BYTES: usize = 4096;
pub const MAX_LOG_LINES: usize = 2000;
pub const DEFAULT_FOLLOW_SECONDS: u32 = 3600;
pub const MAX_FOLLOW_SECONDS: u32 = 86_400;
const MAX_RATE_BYTES: usize = 1024 * 1024;
const MAX_RATE_LINES: usize = 5000;

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
        let line = safety::safe_text(line, MAX_LINE_BYTES);
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
        let mut text = String::with_capacity(self.bytes + 96);
        if self.dropped > 0 {
            text.push_str(&format!(
                "[Rolling buffer: {} older lines discarded]\n",
                self.dropped
            ));
        }
        for line in &self.lines {
            text.push_str(line);
            text.push('\n');
        }
        text
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
        const NOTICE: &str = " [line truncated at 4096 bytes]";
        let prefix_limit = if self.discarding {
            MAX_LINE_BYTES - NOTICE.len()
        } else {
            MAX_LINE_BYTES
        };
        let mut line = safety::safe_text(&String::from_utf8_lossy(&self.pending), prefix_limit);
        self.pending.clear();
        if self.discarding {
            line.push_str(NOTICE);
        }
        self.discarding = false;
        Ok(line)
    }
    /// One sanitized line at a time. Oversized lines are truncated explicitly.
    /// Quiet streams wait until the configured deadline; dropping cancels the body.
    pub async fn next(&mut self) -> Result<Option<String>> {
        if self.ended {
            return Ok(None);
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
                    self.ended = true;
                    self.pending.clear();
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
