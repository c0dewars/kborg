# K8Borg 0.2: live watches and metrics

This iteration implements the next two roadmap features. It remains a prerelease with the existing production gates.

## Behavior

The terminal uses real LIST/WATCH for live clusters by default. Each selected context/namespace/resource view owns one cancellable feed. The initial paginated list commits only after consistent resource versions have been checked; live events update a bounded UID-aware cache. Bookmarks provide resume versions, HTTP 410 causes relisting, and retryable transport failures use bounded backoff with jitter. Server watch leases are eight seconds to fit the verified client's read timeout and to reauthorize regularly. An already-open stream is not revoked instantaneously.

The cache and response stream are bounded to 1,000 objects and 8 MiB. A failed or oversized/incomplete watch clears its cache, remains stale and stops rather than implying objects are absent. Authorization denial stops further requests. Unsupported watches and removed resources stop with explicit polling/catalog guidance. `--poll` or `w` selects the existing bounded five-second polling behavior; offline demo stays synthetic polling.

A background Tokio task consumes the feed while the API command worker remains available. A single snapshot slot coalesces bursts; no per-event UI queue grows. Scope changes abort obsolete feeds and invalidate their epoch. Only the matching pending connection may update session identity, and a namespace chosen during connection is preserved. Selection follows namespace/name/UID. Scale forms capture their original target and the worker refuses replacement UIDs. Unknown write outcomes remain visible through subsequent watch updates until acknowledged.

`metrics pods|nodes` and UI `m` read the standard Metrics API. Fixed resource paths preserve the existing TLS, authorization and response limits. CPU uses checked integer nanocores and exact decimal millicores; memory uses checked bytes. Raw quantities, timestamp/window and upward rounding are explicit. No percentages or zero measurements are invented. Missing, denied or malformed metrics produce a wrapped diagnostic while core resource data remains available. Pod output uses container rows, not an inferred aggregate.

`watch RESOURCE --json` emits bounded snapshot JSONL. Closing a consumer pipe exits cleanly. No credentials, extra kubeconfig providers, AI requests, telemetry or mutation families were added.

## Validation

- 49 unit tests passed, plus the separate independent metrics quantity contract; formatting and all-target Clippy with warnings denied passed.
- Controlled watch transport cases cover UID replacement/deletion, bookmark resume, 410 rebuild, 401/403 terminal revocation, malformed/oversized bodies, incomplete/inconsistent pagination, cache limits, event coalescing and unsupported 404/405/501 endpoints.
- Controlled metrics HTTP cases cover pod/node/scoped paths, exact quantities, measurement time, pagination/duplicates/inconsistent versions, sanitized errors and object/container/page/body budgets. These are mocks rather than claims of an installed metrics server.
- UI reducer cases cover obsolete sessions, a namespace changed during connection, stale scope snapshots, revoked-view clearing, a producer burst with no UI consumption, retained unknown write outcomes, preserved selection/captured scale identity and metrics errors that leave resource data intact.
- Real Docker Desktop lifecycle and RoleBinding-revocation tests passed in owned namespaces. Existing guarded scale/restart/delete/RBAC/redaction acceptance passed again on Kubernetes 1.36.1 with fresh retained local credentials.
- Independent Windows PTY tests passed LIVE/POLL toggling, scope changes, metrics demo/absence, wrapping and terminal restoration. The reported clipped error dialog was corrected and retested.
- The updated 183-package lockfile advisory scan reported zero known vulnerabilities and warnings, without ignored advisories. The newly direct futures-util dependency was already transitive; std/async-await features avoid adding macro dependencies.

Additional secure metrics-server, minimum-version, WSL/performance and platform-build results are recorded in their separate reports as those runs complete. An unavailable API or a mock response is never counted as real metrics-server success.

## Commands

```powershell
k8borg --kubeconfig <trusted-file> -n <namespace> watch pods --json
k8borg --kubeconfig <trusted-file> -n <namespace> metrics pods --json
k8borg --kubeconfig <trusted-file> metrics nodes
k8borg --kubeconfig <trusted-file> --poll
k8borg --demo metrics pods
```

Remaining work includes broader fault/soak/discovery-memory testing, authoritative server audit/admission and filesystem hardening, editing and simultaneous multi-context workflows, optional isolated Copilot diagnosis, signing/notarization and wider terminal/accessibility acceptance. Visual redesign still follows feature completion.

## Final independent evidence

Secure Metrics Server 0.8.1 interoperability passed on a dedicated Kubernetes 1.34.11 cluster: two container samples, one node sample and a real ConfigMap watch update. Kubelet scraping used the verified cluster CA and serving certificates; API aggregation used a custom SAN certificate and matching caBundle without insecureSkipTLSVerify. The node was capped at 4 GiB / 4 CPUs and deleted with credentials/private keys. See SECURE_METRICS_INTEROP.md and evidence/secure-metrics-kubernetes-1.34.json.

WSL native Linux compiled and passed 48 unit tests before the final reconnect regression plus the independent metrics contract and Clippy. Two sessions of 30 seconds measured 10.10 MiB LIVE / 10.30 MiB POLL RSS; CPU was below tick resolution / 10 ms, so no meaningful efficiency advantage is claimed. NDJSON first snapshot and consumer-close exit passed. See WATCH_METRICS_PERFORMANCE.md for environment, contention, measurement and request-count limitations.


Final reconnect regression: explicit reauthentication pins the current context and retains namespace/resource identity across a refreshed catalog. A missing custom resource stops for explicit catalog refresh instead of silently reading another resource. The corresponding 49th unit test passed.
