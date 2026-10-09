# K8Borg 0.2 watch and metrics: Linux validation and idle footprint

On 2026-10-09, the native Linux release passed `cargo test --all-targets` (48 unit tests and the independent metrics quantity contract), `cargo clippy --all-targets -- -D warnings`, and `cargo build --release` under Ubuntu-26.04 / WSL2. Three opt-in cluster tests were ignored by this generic test run; their Windows and secure-cluster acceptance is recorded separately. This measurement used the fresh isolated Docker Desktop reader fixture and made no cluster writes.

## Results

One 30-second PTY run per mode, at 120 columns by 35 rows, included initial connection and rendering. The fixture contained one pod. The filesystem cache was warm. RSS came from `/proc` high-water/current RSS sampled every 100 ms; CPU came from kernel process user plus system tick counters. Both sessions displayed their expected LIVE/POLL label and exited successfully on `q`.

| Default LIVE | Explicit `--poll` | Measurement |
|---:|---:|---|
| 10,340 KiB (10.10 MiB) | 10,548 KiB (10.30 MiB) | Maximum observed RSS/high-water mark |
| 0 ticks | 1 tick (10 ms) | Accounted CPU over 30 seconds |
| Below accounting resolution | 0.0333% | Average of one CPU core |
| 68,231 bytes | 73,912 bytes | Terminal output, including startup and redraws |

A zero CPU tick count means below the kernel accounting resolution, not zero work. The two CPU counters have 10 ms granularity, and these small observations do not establish a statistically significant CPU difference between the modes. The host was shared with other acceptance work, including a separate resource-limited Kind fixture; Linux Clippy also ran during part of this measurement. This is a small-fixture footprint check, not a controlled comparison or production capacity benchmark. Both observations fit the existing provisional 64 MiB live-memory and 2% one-core idle-CPU budgets.

HTTP request counts are **not measured**. The connection uses verified TLS and this harness does not have API audit logs or instrumented requests. The source sets polling refresh to five seconds and the watch server timeout to eight seconds; these settings alone do not prove an observed request count or a reduction in requests. Watch reconnects, bookmarks, and startup traffic must be included in any future request-level comparison.

## CLI acceptance

The Linux JSON watch returned its initial coherent snapshot in 12.113 ms, with one item, a nonempty resource version, and no stale, terminal, or truncated flag. Closing its stdout consumer produced exit code 0 after 6.276 seconds. The CLI discovers consumer closure on the next write, so this test does not promise immediate cancellation when an idle output pipe closes. Actual resource additions, updates, deletions and RBAC revocation are tested by the separate watch acceptance suite; this performance harness only reads its fixture.

`--demo --json metrics pods` produced the labeled deterministic sample with exact quantity and measurement-time fields. Real pod metrics through this reader returned the explicit RBAC-forbidden error, exit code 1, and zero stdout bytes. This is authorization-denial evidence; it does not establish whether the cluster serves the Metrics API. API-unavailable behavior and real Metrics API interoperability are covered separately.

The measured Linux binary is 9,514,912 bytes with SHA256 `73fbee359c680d631206353cf350aa5e5909d73e3ebc9c98cd23fa3e1acaebec`. This hash identifies this local benchmark binary, not a later package or CI artifact. Full non-sensitive counters are in [phase2-linux-performance.json](evidence/phase2-linux-performance.json).

## Reproduce

Build the native Linux binary, then run from the project directory under WSL:

```sh
CARGO_TARGET_DIR="$HOME/.cache/k8borg-target" "$HOME/.cargo/bin/cargo" build --release
python3 scripts/Test-WatchPerformance.py \
  --binary "$HOME/.cache/k8borg-target/release/k8borg" \
  --fixture .local/<fresh-run-id>/reader.kubeconfig \
  --output docs/evidence/phase2-linux-performance.json
```

The Python harness uses only the standard library. It selects a theme in a temporary application-config directory, starts two finite TUI sessions, reads a CLI watch snapshot, closes the consumer, and checks demo metrics plus the restricted reader's metrics denial. Use the project's fresh reader fixture with pod LIST/WATCH permission and no Metrics API permission; the harness does not create or mutate workloads. It intentionally stores no tokens, kubeconfig contents, Kubernetes objects, or terminal captures in its report. A temporary application-config directory is removed when the run completes.
