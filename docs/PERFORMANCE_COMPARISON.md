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
| Time to first data | 0.73 s | 0.69 s |
| Switch to 1,000 ConfigMaps | **0.18 s** | 0.37 s |
| Memory, median | **28 MiB** | 118 MiB |
| CPU, average | **0.5 %** | 1.8 % |
| Network in | **4.8 KiB/min** | 80 KiB/min |

## Multi-cluster dashboard: KBorg vs k10s

Both tools watch Docker Desktop and minikube across all namespaces (`kborg -n all --contexts …`, `k10s -c …`), from the same flattened kubeconfig.

| Metric | KBorg | k10s |
|---|---|---|
| Time to both clusters' health | 0.33 s | 0.12 s |
| Memory, median | **16 MiB** | 38 MiB |
| CPU, average | **0.15 %** | 1.5 % |
| Network in | **154 KiB/min** | 2,095 KiB/min |

KBorg keeps Pod and Event data live through watches and polls Node/workload readiness every 15 s; k10s re-polls cluster state. Lower steady traffic matters on shared API servers and for users watching several production clusters all day.

## What changed during this comparison

The first run exposed a scale problem: a namespace with more than 1,000 objects exceeded the live-watch cache and the table went empty ("Access unavailable"). Fixes, all bounded:

- Objects are redacted (`managedFields`, data, annotations…) **before** they are measured, so the cap reflects what is actually held in memory.
- The live cache now holds up to 5,000 objects / 32 MiB (previously 1,000 / 8 MiB); list pages are 500 objects.
- Above the cap, the view falls back to a bounded list of the first 5,000 objects, refreshed every 15 s, with a clear banner instead of an empty table.
- The CLI list uses the same bounds with a shorter page pause.

Result for the 1,000-ConfigMap view: switch time 1.32 s → 0.18 s, network 3.8 MB/min → 4.8 KiB/min.

Other findings: k10s does not accept a multi-file `KUBECONFIG` (it treats the list as one path); KBorg merges the files the way kubectl does.

## Reproduce

The harness (Python, PTY, `ps`, `nettop`) answers terminal queries such as background-color requests the way a real emulator does, which k10s requires to start. It is kept outside the repository; the scenarios are:

```sh
kborg --context docker-desktop -n kube-system   # then :cm kborg-perf
k9s   --context docker-desktop -n kube-system --splashless   # then :cm kborg-perf
kborg -n all --contexts docker-desktop,kborg-soak-v1
k10s  -c docker-desktop,kborg-soak-v1
```
