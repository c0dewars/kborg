# Independent bounded resource load test

Measured source: `99e32c5a2ab62691fdeeb7c4928978d294623e50`, Linux release SHA-256 `f0c4c78a0a0aa4670da62f70895ecbe67974cfbb6d99899b85dade5994ea5fdd`.

This test used the existing Docker Desktop Kubernetes **v1.36.1** endpoint `https://127.0.0.1:64170`. It created **1001 synthetic ConfigMaps**, each with a 4096-byte data payload and two short labels, in a unique owned `k8borg-load-*` namespace. No pods or workload containers were created. Provisioning ran serially in batches of 100, with about 3.91 MiB of raw data overall. The application used a ten-minute service-account token with only namespace-scoped ConfigMap get/list permission, held in a Windows ACL-restricted file. TLS retained the embedded CA. Endpoint, CA fingerprint, and namespace ownership label were checked again before deletion. Both experiment namespaces were removed, their credential and manifest files deleted, and the default `docker-desktop` context retained.

The first attempt found a harness expectation error: sensitive field keys remain present with the value `[REDACTED]`. The corrected assertion passed; this was not a product redaction failure.

| Measurement | Result |
|---|---:|
| CLI first process, wall time | 1068.39 ms |
| CLI subsequent five processes, wall range | 1061.30–1071.08 ms |
| CLI maximum peak RSS across six runs | 16224 KiB (15.84 MiB) |
| CLI returned objects / configured bound | 1000 / 1000 |
| Positive overflow test | 1001 objects → `truncated=true` |
| Text CLI overflow warning | `TRUNCATED` present |
| Data redaction | canary absent; every `data` is `[REDACTED]` |
| TUI 1000-object idle duration | 15.04 s |
| TUI sampled maximum RSS | 22260 KiB (21.74 MiB) |
| TUI idle process CPU | 0.10 s (~0.66% of one core) |
| Cached filter acknowledgement, median / max | 5.50 / 6.85 ms |

TUI navigation selected ConfigMaps; readiness required the rendered `1000 objects` status. Idle sampling included the normal five-second API refresh. Five filter interactions submitted `load-0999` and waited for its rendered filter indicator, then reset it. CLI memory uses GNU time maximum RSS, whereas TUI memory samples procfs every approximately 100 ms and can miss transient peaks. CLI timing includes process startup, kubeconfig parsing, API reads, redaction, and serialization. CPU resolution is coarse. The first process is a cool process, not a globally cold machine: OS and server caches were not evicted. Later processes use warm caches. PTY acknowledgement excludes physical display latency; network audit instrumentation did not independently prove zero API calls during filtering.

This is a bounded real-cluster experiment near the object-count cap, not proof of worst-case behavior for maximal Kubernetes objects, CRDs, many clusters, or adversarial metadata. It complements the lower-level response-body, pagination, and safety tests. It represents **one cluster**, irrespective of context aliases elsewhere.

Reproduce only on this verified local endpoint, with native Linux release built from the desired source:

```powershell
wsl.exe -d Ubuntu-26.04 -- bash -lc 'cd /mnt/c/Users/Garcia/Desktop/projects/k8borg && CARGO_TARGET_DIR=$HOME/.cache/k8borg-target $HOME/.cargo/bin/cargo build --release --locked'
.\scripts\Test-ResourceLoad.ps1
```

The harness fails closed when the endpoint or CA changes and cleans its owned resources in `finally`. Sanitized detailed samples are in `docs/evidence/load-test.json`. Running it replaces that report. The binary path is the WSL user's `~/.cache/k8borg-target/release/k8borg`.
