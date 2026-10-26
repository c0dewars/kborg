# Performance comparison: KBorg, k9s and k10s

Measured 2026-10-24 on an Apple M2 Pro, macOS 27.0.1, against local Docker Desktop Kubernetes 1.36.1 and minikube 1.35.9. Each tool ran in a 130×40 pseudo-terminal with an isolated HOME and config directory, then sat idle on its view for 60 seconds while memory, CPU time and network traffic were sampled once per second (`ps`, `nettop`). These are single local runs, not a statistical benchmark; treat differences under ~20% as noise.

| Tool | Version | Binary |
|---|---|---|
| KBorg | 0.3.0 (release build) | 15.4 MB |
| k9s | 0.51.0 (Homebrew) | 140.5 MB |
| k10s | latest `go install` | 64.9 MB |

## Resource table: KBorg vs k9s

Start in `kube-system` Pods, then switch to a namespace holding 1,000 ConfigMaps (`:cm kborg-perf`) and stay there.

| Metric | KBorg | k9s |
|---|---|---|
| Time to first data | **0.06 s** | 0.69 s |
| Switch to 1,000 ConfigMaps | **0.18 s** | 0.37 s |
| Memory, median | **28 MiB** | 118 MiB |
| CPU, average | **0.2–0.5 %** | 1.8 % |
| Network in | **4.8 KiB/min** | 80 KiB/min |

## Company scale: 5,000 live objects with churn

A namespace with 5,001 ConfigMaps while one of them is updated every second, so the watch keeps streaming changes. Switch with `:cm kborg-scale`, then 30 s idle.

| Metric | KBorg | k9s |
|---|---|---|
| Switch to 5,000 ConfigMaps | **0.22 s** | 0.38 s |
| Memory, median | **71 MiB** | 174 MiB |
| CPU, average | **2.8 %** | 6.8 % |
| Network in | 101 KiB/min | 56 KiB/min |

### Watch traffic diet (re-measured 2026-10-25)

Same scenario in a fresh `kborg-p1` namespace (5,000 ConfigMaps plus `kube-root-ca.crt`, `cm-00000` patched once per second with `kubectl patch`), `:cm kborg-p1`, then 60 s idle; KBorg 0.5.0-rc.3 release build and k9s 0.51.0, median of 3 runs with `scripts/bench-tui.py compare --repeat 3 --seconds 60`. The table above is the older 0.3.0 run; current `main` before this change already measured 41.6 KiB/min (k9s 36.5) in a single run.

| Metric | KBorg | k9s |
|---|---|---|
| Switch to 5,000 ConfigMaps | **0.19 s** | 0.39 s |
| Memory, median | **53 MiB** | 142 MiB |
| CPU, average | **0.5 %** | 2.8 % |
| Network in | **36.4 KiB/min** | 36.7 KiB/min |
| Network out | **0.0 KiB/min** | 0.7 KiB/min |

What changed:

- Watches ask the server to close after 55 s instead of 8 s and request bookmarks. The API server sends a bookmark 2 s before it closes (and about every 60 s otherwise), so each renewal resumes from a fresh resourceVersion. Renewals fell from about 7.5 to about 1 per minute, which removed the request overhead (network out 1.7 → 0.0 KiB/min).
- A dropped connection or API outage keeps the cache (marked stale) and resumes at the last applied event or bookmark. Only 410 Gone, or a second unreadable stream with no progress in between, relists. Through a TCP proxy that cut the API for 30 s, a 5,001-object watch reconnected 1.7 s after the API returned with **0 relists** (the previous build cleared the table and relisted all 5,001 objects).
- GET reads send `Accept-Encoding: gzip`. A 5,001-ConfigMap LIST (11 pages) took **195 KiB instead of 1.8 MiB** on the wire. The API server does not compress watch streams, so steady churn traffic is the change events themselves: both tools now receive the same events, and the remaining gap is noise. Going lower needs smaller events (server-side tables, P2).
- Decoding is bounded like raw bodies: compressed and decoded bytes both count against the 8 MiB budget, input is decoded in 4 KiB slices, and truncated, corrupt, padded or unrequested encodings are errors.

Measured through the same proxy (raw TLS bytes, `kborg watch configmaps`, 60 s after a 10 s warm-up), the CLI watch went from 82.6 to 78.5 KiB/min; this counts TLS records differently from `nettop`, so compare it only with itself.

Demo mode can generate thousands of synthetic Pods (`KBORG_DEMO_SCALE=N`): 5,000 Pods load in 0.13 s, filter in 0.17 s and sort in 0.07 s; 20,000 Pods load in 0.38 s.

## Multi-cluster dashboard: KBorg vs k10s

Both tools watch Docker Desktop and minikube across all namespaces (`kborg -n all --contexts …`, `k10s -c …`), from the same flattened kubeconfig. Time to data is measured to the Pod health line on both cards.

| Metric | KBorg | k10s |
|---|---|---|
| Time to both clusters' health | **0.12 s** | 0.11–0.20 s |
| Memory, median | **16 MiB** | 38 MiB |
| CPU, average | **0.15–0.3 %** | 1.5 % |
| Network in | **154 KiB/min** | 2,095 KiB/min |

KBorg keeps Pod and Event data live through watches and polls Node/workload readiness every 15 s; k10s re-polls cluster state. Lower steady traffic matters on shared API servers and for users watching several production clusters all day.

## What changed during this comparison

The first run exposed a scale problem: a namespace with more than 1,000 objects exceeded the live-watch cache and the table went empty ("Access unavailable"). Fixes, all bounded:

- Objects are redacted (`managedFields`, data, annotations…) **before** they are measured, so the cap reflects what is actually held in memory.
- The live cache now holds up to 10,000 objects / 64 MiB (previously 1,000 / 8 MiB); list pages are 500 objects.
- Above the cap, the view falls back to a bounded list of the first 10,000 objects, refreshed every 30 s, with a clear banner instead of an empty table.
- The CLI list uses the same bounds with a shorter page pause.

Result for the 1,000-ConfigMap view: switch time 1.32 s → 0.18 s, network 3.8 MB/min → 4.8 KiB/min.

Startup work in the same round:

- The first watch starts before API discovery instead of queueing behind it, and a discovery result no longer restarts a watch whose resource is unchanged (first data 0.73 s → 0.06 s).
- `--contexts` opens the dashboard immediately; cards never wait for the main view's connection.
- The event loop runs at 60 fps only while a request is in flight, the view is loading or the user just typed, and at 100 ms otherwise, so updates reach the screen without a 100 ms hop per step and idle CPU stays near zero.
- The table's filter-and-sort order is memoized and sorts with precomputed keys, so a 5,000-object view is not re-sorted on every frame.

Other findings: k10s does not accept a multi-file `KUBECONFIG` (it treats the list as one path); KBorg merges the files the way kubectl does.

## Regression gate

`scripts/bench-tui.py` is the harness behind these numbers (Python standard library only; Pillow just for `--screenshots`). It starts each tool in a 130×40 pseudo-terminal with its own controlling tty and a throwaway HOME, `KBORG_CONFIG_DIR`, `K9S_CONFIG_DIR` and `XDG_*` directories, so your settings are never read or changed. It answers terminal queries (OSC 10/11 colors, cursor position, device attributes) the way a real emulator does, waits for regex markers on the emulated screen, then samples memory and CPU (`ps` on macOS, `/proc` on Linux) and, on macOS only, network (`nettop`) over an idle window.

`gate` runs three demo-mode scenarios that need no cluster and fails (exit 1) when a median exceeds its budget in `scripts/bench-budgets.json`:

| Scenario | What it does |
|---|---|
| `demo-startup` | `kborg --demo --no-splash`, waits for Pod `api-7d4f9`, 10 s idle |
| `demo-scale` | the same with `KBORG_DEMO_SCALE=5000` (5,000 extra Pods); SKIPPED if the binary ignores the variable |
| `demo-dashboard` | demo, then `D` until `CONTEXT OVERVIEW` |

Budgets are about 3× the baseline measured on an Apple M2 Pro (stored next to each budget), with floors of 1 s and 1 % CPU, because CI runners are slower and noisier. The `bench` CI job runs the gate after `checks` on `ubuntu-24.04` and uploads the JSON results.

```sh
cargo build --release --locked
python3 scripts/bench-tui.py gate --repeat 3            # uses target/release/kborg and scripts/bench-budgets.json
python3 scripts/bench-tui.py run --name kborg --first api-7d4f9 --seconds 30 --out /tmp/bench \
  -- target/release/kborg --demo --no-splash
python3 scripts/bench-tui.py compare --context docker-desktop -n kube-system --first coredns \
  --step ':cm kborg-perf\r' 'kborg-perf' --k9s k9s --k10s ~/go/bin/k10s   # live cluster; prints a Markdown table
```

If a change raises a metric on purpose, re-measure with `gate --repeat 3` and update both `baseline` and `budget` in `scripts/bench-budgets.json` in the same commit.

## Reproduce

The harness (`scripts/bench-tui.py`, see above) answers terminal queries such as background-color requests the way a real emulator does, which k10s requires to start. The scenarios are:

```sh
kborg --context docker-desktop -n kube-system   # then :cm kborg-perf
k9s   --context docker-desktop -n kube-system --splashless   # then :cm kborg-perf
kborg -n all --contexts docker-desktop,kborg-soak-v1
k10s  -c docker-desktop,kborg-soak-v1
```
