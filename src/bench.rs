//! k9s-style HTTP benchmark of a reviewed port-forward. Only loopback ports
//! of KBorg's own forwards are targeted; requests, concurrency, time and
//! bytes read are bounded so a benchmark cannot become a load test of a
//! production service by accident.
use crate::Result;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const MAX_REQUESTS: u32 = 2000;
pub const MAX_CONCURRENCY: u32 = 20;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const TOTAL_BUDGET: Duration = Duration::from_secs(60);
const MAX_RESPONSE: u64 = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub port: u16,
    pub path: String,
    pub requests: u32,
    pub concurrency: u32,
}
impl Plan {
    pub fn new(port: u16, requests: u32, concurrency: u32, path: &str) -> Result<Self> {
        if !(1..=MAX_REQUESTS).contains(&requests) || !(1..=MAX_CONCURRENCY).contains(&concurrency)
        {
            return Err(format!(
                "bench uses 1..{MAX_REQUESTS} requests and 1..{MAX_CONCURRENCY} concurrent connections"
            ));
        }
        if !path.starts_with('/')
            || path.len() > 1024
            || !path.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err("bench path must start with / and contain no spaces or controls".into());
        }
        Ok(Self {
            port,
            path: path.into(),
            requests,
            concurrency,
        })
    }
}

#[derive(Debug, Default)]
pub struct Report {
    pub sent: u32,
    pub statuses: std::collections::BTreeMap<u16, u32>,
    pub errors: u32,
    pub latencies: Vec<Duration>,
    pub elapsed: Duration,
    pub stopped_early: bool,
}
impl Report {
    fn percentile(&self, p: usize) -> Duration {
        if self.latencies.is_empty() {
            return Duration::ZERO;
        }
        let index = (self.latencies.len() * p).div_ceil(100).saturating_sub(1);
        self.latencies[index.min(self.latencies.len() - 1)]
    }
    pub fn text(&self, plan: &Plan) -> String {
        let ms = |d: Duration| format!("{:.1} ms", d.as_secs_f64() * 1000.0);
        let seconds = self.elapsed.as_secs_f64().max(0.001);
        let statuses = self
            .statuses
            .iter()
            .map(|(code, count)| format!("{code}: {count}"))
            .collect::<Vec<_>>()
            .join("   ");
        format!(
            "BENCH 127.0.0.1:{}{}\n{} requests, {} concurrent{}\n\nCompleted   {} in {:.2} s ({:.1} req/s)\nStatus      {}\nErrors      {} (connection, timeout or malformed response)\n\nLatency     p50 {}   p90 {}   p99 {}   max {}\n\nGET over the reviewed loopback forward with Connection: close. Results include the forward tunnel and are not a substitute for an in-cluster load test.",
            plan.port,
            plan.path,
            plan.requests,
            plan.concurrency,
            if self.stopped_early {
                " (stopped at the 60-second budget)"
            } else {
                ""
            },
            self.sent,
            seconds,
            f64::from(self.sent) / seconds,
            if statuses.is_empty() {
                "none".into()
            } else {
                statuses
            },
            self.errors,
            ms(self.percentile(50)),
            ms(self.percentile(90)),
            ms(self.percentile(99)),
            ms(self.latencies.last().copied().unwrap_or_default()),
        )
    }
}

async fn one(port: u16, path: &str) -> Result<u16> {
    let mut stream = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .map_err(|_| "connect failed")?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: localhost:{port}\r\nUser-Agent: kborg-bench\r\nAccept: */*\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|_| "write failed")?;
    let mut response = Vec::new();
    (&mut stream)
        .take(MAX_RESPONSE)
        .read_to_end(&mut response)
        .await
        .map_err(|_| "read failed")?;
    let line = response.split(|b| *b == b'\n').next().unwrap_or_default();
    let line = String::from_utf8_lossy(line);
    let mut parts = line.split_whitespace();
    match (
        parts.next(),
        parts.next().and_then(|code| code.parse::<u16>().ok()),
    ) {
        (Some(version), Some(code)) if version.starts_with("HTTP/1.") => Ok(code),
        _ => Err("not an HTTP/1.x response".into()),
    }
}

pub async fn run(plan: &Plan) -> Report {
    let started = Instant::now();
    let next = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let mut workers = tokio::task::JoinSet::new();
    for _ in 0..plan.concurrency {
        let next = next.clone();
        let plan = plan.clone();
        workers.spawn(async move {
            let mut results = Vec::new();
            while next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < plan.requests {
                if started.elapsed() > TOTAL_BUDGET {
                    break;
                }
                let begin = Instant::now();
                let result = tokio::time::timeout(REQUEST_TIMEOUT, one(plan.port, &plan.path))
                    .await
                    .unwrap_or_else(|_| Err("timeout".into()));
                results.push((result, begin.elapsed()));
            }
            results
        });
    }
    let mut report = Report::default();
    while let Some(joined) = workers.join_next().await {
        for (result, latency) in joined.unwrap_or_default() {
            report.sent += 1;
            match result {
                Ok(code) => {
                    *report.statuses.entry(code).or_default() += 1;
                    report.latencies.push(latency);
                }
                Err(_) => report.errors += 1,
            }
        }
    }
    report.latencies.sort();
    report.elapsed = started.elapsed();
    report.stopped_early = report.sent < plan.requests;
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bench_counts_statuses_and_latency_against_a_local_server() -> Result<()> {
        crate::cli::runtime()?.block_on(async {
            let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .await
                .map_err(|_| "bind failed")?;
            let port = listener.local_addr().map_err(|_| "addr failed")?.port();
            tokio::spawn(async move {
                let mut served = 0u32;
                while let Ok((mut socket, _)) = listener.accept().await {
                    served += 1;
                    let status = if served.is_multiple_of(4) {
                        "503 Busy"
                    } else {
                        "200 OK"
                    };
                    let mut buffer = [0u8; 1024];
                    let _ = socket.read(&mut buffer).await;
                    let _ = socket
                        .write_all(
                            format!("HTTP/1.1 {status}\r\nContent-Length: 2\r\n\r\nok").as_bytes(),
                        )
                        .await;
                }
            });
            let plan = Plan::new(port, 40, 4, "/healthz")?;
            let report = run(&plan).await;
            assert_eq!(report.sent, 40);
            assert_eq!(report.errors, 0);
            assert_eq!(report.statuses.get(&200), Some(&30));
            assert_eq!(report.statuses.get(&503), Some(&10));
            assert!(report.text(&plan).contains("p99"));
            Ok(())
        })
    }
    #[test]
    fn plans_are_bounded_and_paths_plain() {
        assert!(Plan::new(8080, 0, 1, "/").is_err());
        assert!(Plan::new(8080, MAX_REQUESTS + 1, 1, "/").is_err());
        assert!(Plan::new(8080, 10, MAX_CONCURRENCY + 1, "/").is_err());
        assert!(Plan::new(8080, 10, 2, "/a b").is_err());
        assert!(Plan::new(8080, 10, 2, "relative").is_err());
        assert!(Plan::new(8080, 10, 2, "/ok?x=1").is_ok());
    }
}
