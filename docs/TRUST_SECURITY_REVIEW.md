# Sigstore, CRD migration and Copilot security review

Date: 2026-10-23. This is a local code, dependency and owned-fixture review,
including independent agent cross-reviews. It is not an external penetration test
or security certification. Exact final build and live evidence is linked from
[acceptance](TRUST_MIGRATION_ACCEPTANCE.md).

## Changes reviewed

Sigstore trust binds the exact chart archive, verifier executable, policy,
bundle and explicit key/root or identity/issuer/root. Verification happens both
before review and before execution. Missing log evidence, changed inputs and
wrong trust fail closed. Group/world-writable authority files are rejected.
Private public-input snapshots, scrubbed environment, output/deadline bounds and
process-group cleanup limit execution. The selected native Cosign executable
retains OS-user permissions; it is not sandboxed. Trust-root freshness and the
independent authority of executable/key/root pins remain the operator's duty.
OCI manifest/referrer signatures and general attestations are outside this scope.

Storage migration binds the complete bounded instance inventory and schema to
UID/resourceVersion and exact ownership. Native writes preserve content and
require acknowledged RV changes, but never authorize clearing storage history on
their own. Finalization requires an official migration controller's fresh exact
object success/checkpoint, repeated complete inventory and a guarded status
update. Stateful tests model persisted versions separately from GET conversion.
Foreign/GitOps/controller-owned instances, incomplete inventories, schema/data
transformations and stale approvals are rejected. Cancellation and partial
writes require reconciliation; existing controller work may continue. Quiescing
writers and stable API-server configuration remain required operational controls.

Copilot requires a pinned signed native executable and fresh actual-policy
canaries before credential lookup. Policy chooses exactly one private credential
file or separately pinned GitHub CLI with explicit host/account/config directory.
No ambient token or account fallback is used. Selected token/host/account are
hashed in memory and rechecked before execution. The user-authorized gh credential
lookup alone receives access to its HOME/config/keychain; the Copilot child gets
an empty private HOME and cannot access them. Stored gh authority may be broader
than Copilot permissions, which is disclosed in the review.

The macOS ARM64 child is denied outside reads/writes, process creation, extra executables
and direct networking. An authenticated per-run loopback broker only permits
approved CONNECT:443 authorities and a matching parsed TLS ClientHello SNI;
unknown hosts, ECH, malformed requests and private/special IPs are refused.
Connection, bytes, subprocess input/output, CPU and wall time are bounded.
Cancellation closes connections and terminates the process group. The broker
checks destination hosts, not encrypted API paths; the selected native client
must validate TLS certificates and remains trusted. Apple's deprecated/private
sandbox interface and provider/client changes can break this integration, in
which case checks fail closed. Other platforms have no inference fallback.

Only fixed allowlisted status terms and bounded counters enter Copilot requests.
Names, namespaces, messages, specs, logs and Kubernetes credentials are excluded.
Counters can still disclose workload state. Every transfer requires an unexpired
exact host/account confirmation; the UI additionally requires every review line
to have been painted at the current geometry. Returned text is bounded and
terminal-sanitized; no response authorizes commands or Kubernetes mutations.
Selected credential plaintext/base64 appearing in output suppresses that output.
There is no claim that arbitrary encodings, malicious same-user software or all
forms of information leakage are detectable. Tokens necessarily exist in process
memory during use; no token is saved in the policy or evidence.

## Findings resolved during review

- Sigstore policy/root/key permissions now reject group/world writes before
  preparation and final use.
- Both verifier and Copilot process supervision now handle stalled stdin,
  descendant-held pipes, cancellation and dropped futures within finite bounds.
- Broker destination filtering rejects private, special-use and transition ranges,
  including CGNAT and benchmarking ranges, and the full `3fff::/20` documentation block ([IANA IPv6 registry](https://www.iana.org/assignments/iana-ipv6-special-registry/)).
- Copilot credential reuse is explicit, immutable and account-bound, with active
  cancellation propagated into the final gh lookup.
- A closed/full UI queue no longer leaves Copilot on a permanent preparing screen.
  Late/stale preview responses and cancellation retain the correct UI locks.
  Widening a terminal at the end of a review now renders the same clamped lines
  that are acknowledged, preventing an unseen last line from being counted.
- Native TLS startup now permits only metadata for the pinned executable's ancestor directories, which CoreFoundation needs to construct its TLS policy. No Mach/keychain service allowance or certificate-validation bypass was added; outside file contents remain denied.
- Unknown diagnostic strings become `Other`, preventing arbitrary status text
  from entering the minimized bundle.

## Retained local checks

The refreshed locked dependency audit covers 203 packages with zero known vulnerabilities and zero warnings. The [changed-file known-pattern scan](evidence/trust-secret-pattern-review.json) found three deliberate public test canaries and no actual credentials; it is not a universal secret detector. Native platform, terminal, owned-cluster and synthetic Copilot evidence is recorded in [acceptance](TRUST_MIGRATION_ACCEPTANCE.md).

## Remaining gates

Least-privilege Kubernetes RBAC/admission, protected external audit, publisher
signing/notarization, broader terminal/platform acceptance, cloud identities,
long-running fault/load tests and an independent security audit remain open.
Client checks do not defeat a malicious same-user process or a compromised
cluster/provider. Same-user path replacement races are reduced by snapshots,
hashes and repeated checks, not universally eliminated by file metadata checks.
The locked dependency audit is a current known-advisory check, not proof of all
transitive/native-tool or future supply-chain security.
