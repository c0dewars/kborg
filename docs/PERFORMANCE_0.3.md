# KBorg 0.3 native WSL performance and read acceptance

Measured on 2026-10-09 at 23:44–23:47 UTC (2026-10-10 local time).
The Linux release binary was compiled natively in Ubuntu 26.04 under WSL2 using
Rust 1.99.0. The WSL environment exposes 16 logical CPUs and 15,937,180 KiB RAM.
The backing cluster is Docker Desktop Kubernetes 1.36.1. Other development and
Docker activity were present; this is a small local acceptance measurement, not an
isolated benchmark, long soak, or production capacity qualification.

The native Linux library suite passed **81 tests** and strict library Clippy passed
before the release build. The measured binary is **11,049,904 bytes** (10.54 MiB),
SHA-256 `be49aa0ad312f0d8dadfb03860669249987746bbf01d2db2d3918601391695c2`.
It was built from the 0.3 working tree; subsequent source edits require new release
validation. The hash identifies the actual tested artifact.

## Results

| Workload | Duration | Sampled peak RSS | CPU time | Result |
|---|---:|---:|---:|---|
| Live-watch TUI, one pod | 60 s | 11,328 KiB (11.06 MiB) | 0.01 s | Expected LIVE mode, clean exit |
| Polling TUI, one pod | 60 s | 11,396 KiB (11.13 MiB) | 0.01 s | Expected POLL mode, clean exit |
| Overview of 100 explicit context aliases | 0.131 s | 11,404 KiB (11.14 MiB) | 0.09 s | All 100 rows succeeded |
| Explicit-container live log follow | 17.532 s | 9,928 KiB (9.70 MiB) | Below 0.01 s accounting resolution | 31 JSON log lines, explicit bounded timeout |

Both idle TUI measurements used approximately 0.0167% of one CPU core during the
sample interval. A difference cannot be established at this resolution. The short
overview burst used about 68.5% of one core over 0.131 seconds; that is active work,
not an idle cost. No extrapolation to a sustained request rate is supported.

The first real CLI watch snapshot arrived in **100.411 ms**, contained one pod and a
resource version, and was neither stale nor truncated. Closing its consumer caused
exit code 0 after **5.224 seconds**, at the next attempted output. No writes were
performed by these measurements. A missing Metrics API permission produced a clear
forbidden error and empty stdout, rather than false metric values.

## What the 100-context test establishes

The test creates 100 unique context aliases referencing **one existing cluster and
one short-lived read-only service-account identity**. Each overview connection reads
the fixture namespace independently, with a configured maximum concurrency of four.
Every row reported one pod and no error or truncation. This exercises configuration
lookup, independent client creation, bounded concurrent reads, aggregation, and JSON
output. It does **not** establish operation against 100 independent clusters, varied
network latencies, credential providers, trust roots, or failure domains.

The alias kubeconfig is written into a mode-0700 native Linux temporary directory
using a mode-0600 file from creation, and removed after the run. No credential values
or raw log contents appear in evidence. The original fixture was preserved.

## Live logs and limits

The explicit container read succeeded and returned 31 timestamped lines without
terminal control characters. The request then stopped with its existing idle/transport
timeout after 17.532 seconds. This fixture emits approximately every 10 seconds,
which is close to the 10-second idle deadline. Consequently this run confirms the
bounded timeout behavior, but does **not** demonstrate an uninterrupted 60-second log
session. Missing `--allow-logs` was independently denied with no stdout.

## Reproduction and evidence

`scripts/Test-WatchPerformance.py` samples `/proc` every 100 ms with a 120×35 PTY,
one sequential 60-second run per mode, including startup and connection.
`scripts/Test-ReadPerformance.py` samples overview and follow subprocesses every 2 ms.
Both use only Python's standard library. CPU accounting resolves to 10 ms, and RSS
sampling can miss short peaks. Filesystem caches were warm. API request counts were
not instrumented and are not inferred from encrypted traffic.

- `docs/evidence/performance-0.3-watch.json`
- `docs/evidence/performance-0.3-read.json`

Still required for broader qualification: many independent control planes, large
resource sets on the current build, slow/unreliable networks, token expiry during
reads, longer soak periods, and repeatable isolated measurements on each supported OS.

## Final Windows binary sample

At 2026-10-10 00:02 UTC, the final production-runtime Windows executable (11,665,920 bytes; SHA-256 `d4f68b561c38e82dc35723b92fa59fe17122ed15c9d3bbc857c884601ef82761`) passed `Test-Performance.ps1 -Samples 30` with the existing least-privilege Desktop fixture. Thirty warmed demo invocations had median 19.38 ms and p95 22.43 ms. Thirty 100-context configuration listings had median 20.48 ms and p95 21.97 ms. One hundred sequential live read invocations against aliases of one cluster had median 28.35 ms, p95 31.26 ms, 1.469 CPU seconds in aggregate and maximum sampled working set 12,177,408 bytes (11.61 MiB).

These are short CLI invocations, not sustained interactive sessions or 100 independent clusters. One-millisecond working-set sampling is a lower bound on true peak memory; Windows working set and Linux RSS are not identical measurements. Warm caches and local API latency limit generalization. Exact evidence is in `evidence/performance-0.3-windows.json`; temporary alias credentials were removed by the benchmark's cleanup path.
