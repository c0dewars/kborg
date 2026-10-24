# Security policy

The current 0.3 development build is a local-testing release. Do not report it as audited/production-ready. Public vulnerability reporting infrastructure is not configured yet; discuss security findings privately with the project owner and avoid placing credentials or real cluster objects in reports.

Use dedicated least-privilege identities. Never use CI production credentials. Do not assume read-only access protects confidentiality. Never publish `.local/`, kubeconfigs, logs, audit metadata or crash dumps containing private data. TLS verification cannot be disabled through application flags.

Before production: independent sensitive-code review, dependency advisories/licenses, cloud-auth trust, protected audit, OS file-permission/race hardening, denial-of-service/fuzz tests, revocation/uncertain-result drills, supported platform matrix, release authenticity and deployment-specific RBAC/admission evidence.

See docs/REMAINING_WORK.md and the linked current acceptance report for requirements and known limitations. Client safeguards do not constrain a hostile same-user process. The API server and organizational controls are authoritative.

Secret inspection is off by default. `--allow-secret-reveal` enables `:secrets`,
which lists names, types, key names and sizes for one namespace and decodes a
single key only on an explicit `x`, for 30 seconds; values are never exported,
copied or written to disk. The listing still transfers Secret payloads into
process memory, so grant `list secrets` only where that is acceptable. Ctrl-K
force kill is separately opt-in with `--write --allow-force-kill`.

Copilot advice can target GitHub.com, a GHE.com tenant or GitHub Enterprise
Server. Each deployment has its own exact egress allowlist, and credentials are
bound to the policy host. Two GHES-only policy options widen trust and should be
set deliberately: `allow_private_network` lets the egress broker reach RFC 1918,
CGNAT and IPv6 ULA addresses for that one host (loopback, link-local and
metadata addresses stay blocked), and `tls_ca_file` makes TLS for that host
trust the supplied internal CA. In GHES offline mode the instance forwards the
reviewed bundle to its administrator-configured model provider, which KBorg
cannot see or verify.

Helm storage access is separately opt-in with `--allow-helm`. Generic Secret
YAML/describe views remain unavailable, but Helm metadata/history and selected values are
read from Kubernetes Secret or ConfigMap storage. Kubernetes RBAC cannot restrict
Secret list permission by label selector; the API identity must be scoped
accordingly. Browsing never exports raw release payloads, notes, chart files or
rendered manifest bodies. Lifecycle review separately shows redacted affected
objects. Known-key and private-key redaction cannot classify every
confidential custom value. Logs and explicitly requested values remain sensitive.

Helm lifecycle requires `--write --allow-helm --allow-helm-write` and an explicitly
trusted native Helm executable at exactly version 4.3.1 on Unix. It accepts deterministic packaged charts from local files or OCI tags/digests resolved before review. Hooks/tests and explicit CRD operations require separate explicit capabilities and full review. Ordinary resources and custom resources are namespaced; cluster/system targets beyond the reviewed CRD workflows, recognized GitOps resources and foreign adoption are refused. The complete
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

OCI acquisition uses an isolated verified HTTPS client, bounded digest-checked downloads and explicitly host-bound registry credentials. It never uses Kubernetes credentials or Helm/Docker credential stores. Digests do not verify publisher identity. Selecting --helm-keyring requires valid Helm OpenPGP provenance from an explicit public trust keyring; it does not verify Sigstore attestations. Hook workloads may affect external systems and retained hooks may survive uninstall. CRDs have separate create/update/delete reviews; normal release uninstall retains them. Updates restrict schema changes to supported compatible changes and seal complete cluster-wide instance inventories. Deletion requires exact instance ownership, explicit destructive confirmation and native UID/RV-preconditioned instance/schema writes. A concurrent create after the last empty check remains a race; quiescence requires server-side controls. CRD UID/RV and validated namespaced schemas bind ordinary custom resource reviews. See [extension boundaries](docs/HELM_EXTENSIONS.md).
