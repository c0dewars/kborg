# Performance and capacity evidence

Measurements use the release executable. They are observations of one development host, not guarantees for arbitrary clusters. CLI measurements have warm filesystem caches; they exclude Rust compilation and the optional startup animation. The TUI baseline includes initial rendering/connection, automatic refresh and terminal output over a120x35PTY. The synthetic demo contains only a few objects, so its memory usage does not establish worst-case cache behavior.

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

Python uses only its standard library. GNU time reports per-process peak resident memory and CPU for CLI invocations; short commands often consume less CPU than its10ms timing resolution. TUI memory/CPU come from `/proc`, sampled every100ms. Windows uses a stopwatch, process CPU time and1ms peak-working-set sampling. Windows memory is a sampled lower bound; Windows working set and Linux RSS are not identical metrics. Output reports record binary SHA256, byte size, timestamp, kernel, method and sample counts. Reports are written under ignored `test-results`; published summaries do not contain fixture tokens, CA data or Kubernetes objects.

##100contexts versus100clusters

The harness constructs100distinct kubeconfig context names. Without a fixture it tests configuration enumeration with synthetic credentials and makes no network calls. With a fixture all100aliases point to **one existing Docker Desktop cluster and one namespace**. It performs100sequential pod-list calls and100additional calls with at most8concurrent CLI processes. These calls exercise context resolution, TLS, RBAC and bounded client work; they do not establish100independent-cluster scalability, shared long-lived-client performance or large-object-count behavior.

The app operates on one selected cluster at a time. It does not maintain100parallel watch streams or credentials in active sessions. A switch invalidates earlier responses. No100real control planes were created: WSL exposes15.6GiB shared memory, and the existing cluster has no Metrics API for a reliable control-plane capacity baseline. Launching100control planes without measured capacity would risk disrupting the user's running Desktop cluster. A dedicated runner and an explicit control-plane budget are needed for that experiment.

## Resource budgets

Provisional reference-host budgets are32MiB RSS for demo TUI,64MiB for a small live namespace, and2percent of one CPU core during an idle30second session. These budgets apply to the measured fixtures. They do not replace worst-case1,000object/body-bound load tests, long-running soak tests, network-loss tests, or measurements under maximum-size log/redaction/admission responses. Before a production release, record those workloads and investigate regressions against a stable host and fixed fixture data.

## Measured results,2026-10-09

Memory below is MiB (1,048,576bytes). CLI latency includes creating a new process for every call. Windows and Linux builds are separate native binaries; all Linux runs use Ubuntu under WSL2, not Windows executable interop.

| Native platform and workload | Samples | Median wall time | p95 wall time | Maximum measured memory |
|---|---:|---:|---:|---:|
| Windows demo pod-list CLI |30|20.45ms|22.99ms|8.18MiB sampled working set|
| Windows enumerate100context aliases |30|22.24ms|24.79ms|8.96MiB sampled working set|
| Windows read pods through100aliases sequentially |100|30.81ms|32.52ms|11.61MiB sampled working set|
| Linux demo pod-list CLI |30|1.87ms|2.18ms|5.54MiB peak RSS|
| Linux enumerate100context aliases |30|2.43ms|2.80ms|6.79MiB peak RSS|
| Linux read pods through100aliases sequentially |100|5.64ms|6.41ms|9.45MiB peak RSS|
| Linux100reads, maximum8separate processes |100|11.29ms|21.32ms|9.42MiB peak RSS per process|

The bounded Linux batch finished in0.178seconds. Its concurrency is8independent CLI processes, each selecting one context; it is not one application managing100active contexts. Combined process memory during the batch was not sampled, so the per-process figure is not a total-memory measurement.

The30second Linux demo TUI reached5.83MiB RSS; the small live namespace TUI reached9.76MiB RSS. Both accumulated0.01seconds of measured CPU, approximately0.033percent of one core, including startup/rendering and refresh. CPU accounting has10ms granularity; a reported0.0for a short CLI sample means below measurement resolution, not zero computation. These observed TUI values meet the provisional fixture budgets above.

The Windows binary is9377280bytes (8.94MiB), SHA256`30127dcc5d262c589530ffad8c6f90588e721bac1d682ce17791b07354d23766`; the Linux binary is8947952bytes (8.53MiB), SHA256`c617b8fe44de253871867e521bd0ff7591952a60d8e2470ba4cf8406ed4579a8`. Exact counters and timestamps are preserved in [Windows evidence](evidence/performance-windows.json) and [Linux evidence](evidence/performance-linux.json). These reports establish the measured small-fixture footprint. They do not prove production readiness or100independent-cluster capacity.
