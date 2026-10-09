//! Opt-in, pull-driven logs with fixed memory and time limits. Drop cancels the request.
use crate::{Result, safety};
use futures_util::io::{AsyncBufRead, AsyncReadExt};
use std::{pin::Pin, time::Duration};
use tokio::time::Instant;

pub const MAX_LOG_BYTES: usize = 256 * 1024;
pub const MAX_LINE_BYTES: usize = 4096;
const MAX_LIFETIME: Duration = Duration::from_secs(60);
const IDLE_TIMEOUT: Duration = Duration::from_secs(10);

pub struct LogFeed {
    reader: Pin<Box<dyn AsyncBufRead + Send>>,
    pending: Vec<u8>,
    chunk: [u8; 4096],
    offset: usize,
    available: usize,
    received: usize,
    deadline: Instant,
    ended: bool,
}
impl LogFeed {
    pub(crate) fn new(reader: impl AsyncBufRead + Send + 'static) -> Self {
        Self {
            reader: Box::pin(reader),
            pending: Vec::with_capacity(MAX_LINE_BYTES),
            chunk: [0; 4096],
            offset: 0,
            available: 0,
            received: 0,
            deadline: Instant::now() + MAX_LIFETIME,
            ended: false,
        }
    }
    /// Returns one sanitized line. Errors terminate the feed; there is no automatic retry.
    pub async fn next(&mut self) -> Result<Option<String>> {
        if self.ended {
            return Ok(None);
        }
        loop {
            if Instant::now() >= self.deadline {
                self.ended = true;
                return Err(
                    "log follow reached its 60-second limit; start a new session to continue"
                        .into(),
                );
            }
            while self.offset < self.available {
                let byte = self.chunk[self.offset];
                self.offset += 1;
                if byte == b'\n' {
                    let line =
                        safety::safe_text(&String::from_utf8_lossy(&self.pending), MAX_LINE_BYTES);
                    self.pending.clear();
                    return Ok(Some(line));
                }
                if self.pending.len() == MAX_LINE_BYTES {
                    self.ended = true;
                    self.pending.clear();
                    return Err("log line exceeded 4096 bytes; follow stopped".into());
                }
                self.pending.push(byte);
            }
            let timeout = self.deadline.min(Instant::now() + IDLE_TIMEOUT);
            let read = tokio::time::timeout_at(timeout, self.reader.read(&mut self.chunk)).await;
            match read {
                Ok(Ok(0)) => {
                    self.ended = true;
                    if self.pending.is_empty() {
                        return Ok(None);
                    }
                    let line =
                        safety::safe_text(&String::from_utf8_lossy(&self.pending), MAX_LINE_BYTES);
                    self.pending.clear();
                    return Ok(Some(line));
                }
                Ok(Ok(count)) => {
                    self.received += count;
                    if self.received > MAX_LOG_BYTES {
                        self.ended = true;
                        self.pending.clear();
                        return Err(
                            "log follow reached its 256 KiB limit; start a new session to continue"
                                .into(),
                        );
                    }
                    self.offset = 0;
                    self.available = count;
                }
                Ok(Err(_)) => {
                    self.ended = true;
                    return Err("log stream transport failure; follow stopped".into());
                }
                Err(_) => {
                    self.ended = true;
                    return Err("log stream idle or session timeout; follow stopped".into());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
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
            assert!(too_long.next().await.is_err());
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
    fn rejects_total_budget_and_never_retries() -> Result<()> {
        runtime()?.block_on(async {
            let mut feed = LogFeed::new(Cursor::new(vec![b'\n'; MAX_LOG_BYTES + 1]));
            let mut count = 0;
            while let Ok(Some(_)) = feed.next().await {
                count += 1;
            }
            assert_eq!(count, MAX_LOG_BYTES);
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
