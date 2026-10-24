# Mac v1 real-cluster soak

The reusable [`test-soak-live.py`](../scripts/test-soak-live.py) harness exercises three continuous application processes in parallel on an explicitly selected local cluster:

- A namespace-scoped Pod resource watch emitting JSONL.
- A namespace-scoped dashboard watch emitting JSONL from independent Pod/Event feeds.
- The actual interactive dashboard inside a native Unix PTY, including completed terminal frames, close/quit and terminal restoration.

The harness copies the requested binary into a private temporary directory before execution. Reports identify its SHA-256, the harness SHA-256, exact monotonic continuous duration, wall-clock start/end, Kubernetes version, namespace UID and process measurements. A release rebuilt during a run cannot silently change the processes being measured.

## Fixture and measurement boundaries

Setup creates a unique `kborg-soak-*` namespace with a random `kborg.dev/run-id` label and the restricted Pod Security policy. One small, unprivileged BusyBox Pod stays Ready. Periodic addition/deletion of a second, unschedulable fixture Pod changes the expected health counts; a bounded, same-name Warning Event exercises Event updates. These are fixture warnings, not diagnosed failures in ordinary workloads.

The application receives only a short-lived service-account token permitting `get`, `list` and `watch` on Pods and Events in that namespace. The issued token expiry is checked in memory to cover the requested run plus at least two minutes; a cluster that caps it too short fails setup. Credentials and the temporary kubeconfig are not recorded in evidence. The harness retains counters and samples, not raw resource or terminal transcripts. It rechecks namespace UID and run label before mutation and cleanup, and checks the changing Pod's UID before deletion. It does not change ordinary workloads or node scheduling settings.

Samples record resident memory, cumulative CPU time, reported instantaneous CPU percentage, output bytes and JSON record counts. Gates are deliberately explicit:

| Measurement | Gate |
|---|---|
| Continuous lifetime | At least the requested duration without unexpected exit/panic |
| Sampled resident memory | Less than 256 MiB per app process |
| Sustained memory growth | Last-quarter median minus first-quarter median below 64 MiB |
| Cumulative CPU | Below 25% of one core after the first 30 seconds |
| Total output | At most 512 MiB per process, continuously drained |
| One JSON record | At most 16 MiB |
| Fixture updates | Both CLI watches and the visible TUI converge to expected scoped counts |
| Shutdown | Dashboard closes, app quits, exact original terminal attributes restored |

These budgets detect gross regressions during the measured workload. They do not prove absence of every leak, characterize huge production clusters or establish a performance service-level objective. The CPU gate uses cumulative process CPU divided by elapsed time; a noisy instantaneous `ps` sample does not alone fail the run.

An optional bounded fault pauses only a Docker container whose exact name, ID and minikube ownership labels match the explicitly selected `kborg-*` profile. It cannot target `docker-desktop`. The harness requires stale resource/dashboard evidence during the interruption, then correct live counts in both CLI feeds and the TUI after unpause. It permits correctly classified transient unready health during recovery, requires the steady Pod to become Ready again, and waits at least 20 seconds after unpause before recording confirmed recovery. The recorded recovery interval includes this settle/check window; it is not a claim about the minimum network reconnection latency. An error path attempts to unpause that exact container before cleanup. Other users of the disposable cluster must be coordinated around this interval.

## Evidence

The pre-export-fix setup smokes passed for 60.013 seconds on Docker Desktop and 60.097 seconds on minikube, with four fixture mutations each, clean terminal shutdown and verified fixture cleanup. See [Desktop smoke](evidence/soak-v1-pre-export-fix/smoke-desktop/report.json) and [minikube smoke](evidence/soak-v1-pre-export-fix/smoke-minikube/report.json). These short checks validate harness setup and are not substitutes for the sustained runs.

The [180.016-second minikube fault smoke](evidence/soak-v1-pre-export-fix/fault-smoke-minikube/report.json) also passed, including a 45.366-second measured pause interval, stale evidence in both CLI streams, confirmed recovery after its 20-second settle/check window, exact final TUI counts and cleanup.

The first sustained pair was deliberately stopped after a separate code review found an export approval visibility defect at narrow/tall terminal sizes. Its [archived index](evidence/soak-v1-pre-export-fix/index.json) records 1,248.100 seconds on Docker Desktop and 857.339 seconds on minikube, both `passed: false` with verified fixture cleanup. These are historical partial runs, not completed soak acceptance.

The corrected native release SHA-256 is `32a6b514bce9ab3edf247248c91e84abbce363b82980a06a6b440e9450bd4cd5`. Its final setup smokes passed seven checks each: [Docker Desktop, 60.146 seconds](evidence/soak-v1-smoke-desktop/report.json) and [minikube, 60.135 seconds](evidence/soak-v1-smoke-minikube/report.json), both with verified cleanup. The sustained runs against this binary passed: [Docker Desktop, 7,200.13 seconds, 120 mutations](evidence/soak-v1-desktop/report.json) and [minikube, 7,200.08 seconds, 118 mutations, 45.303-second owned-container pause at 1,800 seconds with observed staleness and recovery](evidence/soak-v1-minikube/report.json), both with `fixture_cleanup: true`. Peak RSS stayed below 15.2 MiB per process and growth below 0.5 MiB. A pair interrupted earlier the same day is archived in [its index](evidence/soak-v1-interrupted-20261023/index.json) as `passed: false`. `progress.json` is an atomic live progress record, not proof of a completed run. `report.json` requires `passed: true`, the requested continuous duration and `fixture_cleanup: true` before the run is accepted. `samples.jsonl` contains bounded process measurements. Screenshots show actual captured PTY cells; they do not substitute for native-terminal font checks.

The 0.4.0 release binary (SHA-256 `16f874c337b5df4465a097781de1ce001ef6925bac1b5f2798d9446d9e9f755c`) was re-soaked on 2026-10-24 with the same harness. Both runs passed with `fixture_cleanup: true`: [Docker Desktop 1.36.1, 7,200.08 seconds, 119 mutations](evidence/soak-v0.4.0-desktop/report.json) and [minikube 1.35.9, 7,200.13 seconds, 117 mutations, a 45.37-second owned-container pause at 1,800 seconds with staleness observed in both CLI feeds and confirmed recovery](evidence/soak-v0.4.0-minikube/report.json). Peak RSS was 13.0 MiB for the resource watch, 14.5 MiB for the dashboard feed and 15.9 MiB for the TUI; every process ended about 1.1 MiB smaller than its early median. Peak instantaneous CPU stayed at or below 1.2 %.

The soak does not claim to exercise HTTP 429, expired resource versions, token rotation, certificate rotation or production organizational access policies unless separate evidence explicitly identifies those scenarios. In particular, a long-lived token intentionally spanning the test is not token-rotation coverage.

## Reproduction

Use the pinned release build and Python with Pillow available for PTY cell captures. Run only against deliberately selected test clusters:

```sh
python3 scripts/test-soak-live.py \
  --binary target/aarch64-apple-darwin/release/kborg \
  --kubeconfig "$HOME/.kube/config" --context docker-desktop \
  --seconds 7200 --output docs/evidence/soak-v1-desktop

python3 scripts/test-soak-live.py \
  --binary target/aarch64-apple-darwin/release/kborg \
  --kubeconfig .local/minikube-soak.kubeconfig --context kborg-soak-v1 \
  --seconds 7200 --pause-minikube-profile kborg-soak-v1 \
  --pause-at-seconds 1800 --pause-seconds 45 \
  --output docs/evidence/soak-v1-minikube
```

The profile is provisioned and eventually removed separately; the harness owns only its fixture namespace. Do not stop Docker Desktop to inject a test fault. A 60-second smoke with `--mutation-seconds 15 --sample-seconds 5` can verify local harness setup before the sustained run.
