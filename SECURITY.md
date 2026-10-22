# Security policy

The current 0.3 development build is a local-testing release. Do not report it as audited/production-ready. Public vulnerability reporting infrastructure is not configured yet; discuss security findings privately with the project owner and avoid placing credentials or real cluster objects in reports.

Use dedicated least-privilege identities. Never use CI production credentials. Do not assume read-only access protects confidentiality. Never publish `.local/`, kubeconfigs, logs, audit metadata or crash dumps containing private data. TLS verification cannot be disabled through application flags.

Before production: independent sensitive-code review, dependency advisories/licenses, cloud-auth trust, protected audit, OS file-permission/race hardening, denial-of-service/fuzz tests, revocation/uncertain-result drills, supported platform matrix, release authenticity and deployment-specific RBAC/admission evidence.

See docs/REMAINING_WORK.md and the linked current acceptance report for requirements and known limitations. Client safeguards do not constrain a hostile same-user process. The API server and organizational controls are authoritative.

Helm storage access is separately opt-in with `--allow-helm`. General Secret
browsing remains unavailable, but Helm metadata/history and selected values are
read from Kubernetes Secret or ConfigMap storage. Kubernetes RBAC cannot restrict
Secret list permission by label selector; the API identity must be scoped
accordingly. Browsing never exports raw release payloads, notes, chart files or
rendered manifest bodies. Lifecycle review separately shows redacted affected
objects. Known-key and private-key redaction cannot classify every
confidential custom value. Logs and explicitly requested values remain sensitive.

Helm lifecycle requires `--write --allow-helm --allow-helm-write` and an explicitly
trusted native Helm executable at exactly version 4.3.1 on Unix. It accepts deterministic packaged charts from local files or digest-pinned OCI registries. Hooks and create-only CRD bootstrap require separate explicit capabilities and full review. Ordinary resources and custom resources are namespaced; cluster/system targets beyond the reviewed CRD bootstrap, recognized GitOps resources and foreign adoption are refused. The complete
15-minute review includes redacted current/desired resource bodies, exact
arguments and input hashes. Fresh plan, history and UID/resourceVersion checks
precede execution, but Helm's requests do not carry those UID preconditions; a
replacement after the final check remains a race. The supervised Helm process
receives a private static credential and retains OS-user filesystem/network
permissions. It is not sandboxed. Cancellation or failure may leave partial
writes; never retry automatically. See [lifecycle limits](docs/HELM_LIFECYCLE.md).

Workload/selector logs capture at most eight Pods and 32 ordinary-container
streams in one namespace, with shared time/byte/rate/cache bounds. Membership is
fixed and identity-checked, including quiet streams; newly matching or replacement
Pods are not silently admitted. Init and ephemeral sources are excluded from
merges. These controls do not classify confidential log content or guarantee
lossless ordering, reconnection or rotation history.

OCI acquisition uses an isolated verified HTTPS client, bounded digest-checked downloads and explicitly host-bound registry credentials. It never uses Kubernetes credentials or Helm/Docker credential stores. Digests do not verify publisher identity. Hook workloads may affect external systems and retained hooks may survive uninstall. CRDs are created in a separate review, never upgraded/deleted; their UID/RV and validated namespaced schemas bind custom resource reviews. See [extension boundaries](docs/HELM_EXTENSIONS.md).
