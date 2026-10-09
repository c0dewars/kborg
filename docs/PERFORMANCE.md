# Performance and capacity evidence

Measurements use the release executable. They are observations of one development host, not guarantees for arbitrary clusters. CLI measurements have warm filesystem caches; they exclude Rust compilation and the optional startup animation. The TUI baseline includes initial rendering and connection, automatic refresh and terminal output over a 120 × 35 pseudo-terminal (PTY). The synthetic demo contains only a few objects, so its memory usage does not establish worst-case cache behavior.

## Reproduce

```powershell
.\scripts\Test-Performance.ps1
.\scripts\Test-Performance.ps1 -ReaderConfig .\.local\<run-id>\reader.kubeconfig
.\scripts\Test-WSL.ps1 -FixtureDirectory .\.local\<run-id>
```

For read-only Linux benchmarks after the initial WSL build:

```sh
python3 scripts/Test-Performance.py \
  --binary "$HOME/.cache/k8borg-target/release/k8borg" \
  --output test-results/performance-linux.json \
  --fixture /mnt/c/Users/Garcia/Desktop/projects/k8borg/.local/<run-id>/reader.kubeconfig
```

Python uses only its standard library. GNU time reports per-process peak resident memory and CPU for CLI invocations; short commands often consume less CPU than its 10 ms timing resolution. TUI memory and CPU come from `/proc`, sampled every 100 ms. Windows uses a stopwatch, process CPU time and 1 ms peak-working-set sampling. Windows memory is a sampled lower bound; Windows working set and Linux RSS are not identical metrics. Output reports record binary SHA256, byte size, timestamp, kernel, method and sample counts. Reports are written under ignored `test-results`; published summaries do not contain fixture tokens, CA data or Kubernetes objects.

## 100 contexts versus 100 clusters

The harness constructs 100 distinct kubeconfig context names. Without a fixture it tests configuration enumeration with synthetic credentials and makes no network calls. With a fixture all 100 aliases point to **one existing Docker Desktop cluster and one namespace**. It performs 100 sequential pod-list calls and 100 additional calls with at most 8 concurrent CLI processes. These calls exercise context resolution, TLS, RBAC and bounded client work; they do not establish scalability across 100 independent clusters, shared long-lived-client performance or behavior with large numbers of objects.

The app operates on one selected cluster at a time. It does not maintain 100 parallel watch streams or credentials in active sessions. A switch invalidates earlier responses. We did not create 100 real control planes: WSL exposes 15.6 GiB of shared memory, and the existing cluster has no Metrics API for a reliable control-plane capacity baseline. Launching 100 control planes without measured capacity would risk disrupting the user's running Desktop cluster. A dedicated runner and an explicit control-plane budget are needed for that experiment.

## Resource budgets

Provisional budgets on the reference host are 32 MiB RSS for the demo TUI, 64 MiB for a small live namespace, and 2 percent of one CPU core during an idle 30-second session. These budgets apply to the measured fixtures. They do not replace worst-case load tests at the 1,000-object and response-body limits, long-running soak tests, network-loss tests, or measurements with maximum-size log, redaction and admission responses. Before a production release, record those workloads and investigate regressions against a stable host and fixed fixture data.

## Measured results, 2026-10-09

Memory below is MiB (1,048,576 bytes). CLI latency includes creating a new process for every call. Windows and Linux builds are separate native binaries; all Linux runs use Ubuntu under WSL2, not Windows executable interop.

| Native platform and workload | Samples | Median wall time | p95 wall time | Maximum measured memory |
|---|---:|---:|---:|---:|
| Windows demo pod-list CLI | 30 | 20.45 ms | 22.99 ms | 8.18 MiB sampled working set |
| Windows enumerate 100 context aliases | 30 | 22.24 ms | 24.79 ms | 8.96 MiB sampled working set |
| Windows read pods through 100 aliases sequentially | 100 | 30.81 ms | 32.52 ms | 11.61 MiB sampled working set |
| Linux demo pod-list CLI | 30 | 1.87 ms | 2.18 ms | 5.54 MiB peak RSS |
| Linux enumerate 100 context aliases | 30 | 2.43 ms | 2.80 ms | 6.79 MiB peak RSS |
| Linux read pods through 100 aliases sequentially | 100 | 5.64 ms | 6.41 ms | 9.45 MiB peak RSS |
| Linux 100 reads, maximum 8 separate processes | 100 | 11.29 ms | 21.32 ms | 9.42 MiB peak RSS per process |

The bounded Linux batch finished in 0.178 seconds. Its concurrency is 8 independent CLI processes, each selecting one context; it is not one application managing 100 active contexts. Combined process memory during the batch was not sampled, so the per-process figure is not a total-memory measurement.

The 30-second Linux demo TUI reached 5.83 MiB RSS; the small live namespace TUI reached 9.76 MiB RSS. Both accumulated 0.01 seconds of measured CPU, approximately 0.033 percent of one core, including startup, rendering and refresh. CPU accounting has 10 ms granularity; a reported 0.0 for a short CLI sample means below measurement resolution, not zero computation. These observed TUI values meet the provisional fixture budgets above.

The measured Windows binary is 9,377,280 bytes (8.94 MiB), SHA256 `30127dcc5d262c589530ffad8c6f90588e721bac1d682ce17791b07354d23766`; the measured Linux binary is 8,947,952 bytes (8.53 MiB), SHA256 `c617b8fe44de253871867e521bd0ff7591952a60d8e2470ba4cf8406ed4579a8`. Exact counters and timestamps are preserved in [Windows evidence](evidence/performance-windows.json) and [Linux evidence](evidence/performance-linux.json).

These hashes identify the benchmark binaries built before the subsequent spinner-only UI refinement. They are not checksums for later packaged releases. Packaged artifacts are built from their exact CI commit and have their own checksums. The benchmark reports establish the measured small-fixture footprint; they do not prove production readiness or capacity across 100 independent clusters.
