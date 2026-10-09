# K8Borg 0.2 — watches and metrics

The private repository is https://github.com/c0dewars/k8borg. This iteration implements live watches and basic pod/container/node metrics; it remains a prerelease with production gates.

## Changes

- Real LIST/WATCH is the default for live clusters, with resource-version bookmarks, relisting after expiry, bounded reconnect/backoff, UID-aware updates and strict scope cancellation.
- One coalesced snapshot slot prevents event queues growing. Authorization denial and incomplete/oversized views clear data and stop. `w` or `--poll` explicitly selects polling; demo data stays synthetic.
- `metrics pods|nodes` and `m` show checked CPU/memory quantities with timestamp/window. Missing/forbidden/malformed metrics produce clear errors without fabricated zero usage or lost resource data.
- Selection follows namespace/name/UID; scale forms capture their target and reject replacement identities. Reauthentication keeps context/namespace/resource scope. Late events cannot overwrite a newer view, and unknown write outcomes remain visible until acknowledged.

## Independent evidence

49 unit tests, the separate metrics quantity contract, strict Clippy/formatting, and 44 bounded adversarial CLI cases passed. Real Docker Desktop lifecycle, authorization/revocation and existing guarded-write tests passed on Kubernetes 1.36.1.

Actual Metrics Server 0.8.1 interoperability passed on an owned Kubernetes 1.34.11 cluster: two container samples, one node sample and a ConfigMap watch update. Both kubelet scraping and API aggregation verified TLS; no insecure flag was introduced. The node was capped at 4 GiB / 4 CPUs and removed with credentials/private keys. Existing Docker Desktop was untouched.

Independent Windows PTY tests passed LIVE/POLL toggling, resource/namespace changes, metric dialogs and clean exits. Native WSL/Linux tests and lint passed. Thirty-second LIVE/POLL runs used approximately 10.10 / 10.30 MiB RSS; CPU measurements were below resolution / 10 ms, so no meaningful efficiency difference or measured request-count reduction is claimed.

The 183-package updated lockfile advisory scan reported zero known vulnerabilities or warnings with no ignored advisories. Direct futures-util uses only std/async-await and was already transitive; no macro dependency or UI framework was added.

All six native Windows/Linux/macOS x64/ARM64 build/test/package jobs passed in run38002963080, source6d56d4f4374c0583657eb0d1a14273a7c3ca78cb. Every downloaded archive checksum, BUILDsource/version and packaged0.2release notes were independently verified. Details: docs/PLATFORM_BUILDS_0.2.md and https://github.com/c0dewars/k8borg/actions/runs/38002963080. Binaries remain unsigned.

## Try it

```powershell
cd C:\Users\Garcia\Desktop\projects\k8borg
.\scripts\Start-Local.ps1
.\target\release\k8borg.exe --demo metrics pods
.\target\release\k8borg.exe --kubeconfig <trusted-file> -n <namespace> watch pods --json
.\target\release\k8borg.exe --kubeconfig <trusted-file> -n <namespace> metrics pods --json
```

In the TUI, use `m` for metrics and `w` for watch/polling. Docker Desktop currently lacks a Metrics API, so `m` correctly reports unavailable until one is securely configured. K8Borg does not install cluster components. The real provider test was isolated and removed.

Local fixture credentials expire after one hour. Refresh them with `scripts/Test-DockerDesktop.ps1 -ExpectedServer <inspected-local-endpoint> -KeepFixtures` when necessary.

## Remaining scope

Resource editing, simultaneous multi-context workflows, advanced metrics charts, isolated optional Copilot diagnosis, filesystem/audit/admission hardening, broader faults/soak/accessibility testing, signing and macOS notarization remain in the roadmap. Visual redesign continues to follow feature completion. Independent agent reviews and finite tests do not constitute external production certification.
