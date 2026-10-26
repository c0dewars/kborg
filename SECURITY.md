# Security policy

## Reporting a vulnerability

Please report vulnerabilities privately through [GitHub private vulnerability reporting](https://github.com/c0dewars/kborg/security/advisories/new), not in a public issue or pull request. Include the KBorg version (`kborg --version`), your OS and terminal, and steps to reproduce. Never include credentials, kubeconfigs or real cluster objects; synthetic reproductions are enough. You should get an acknowledgement within 7 days. Fixes are released for the latest version only.

## Status

KBorg has had internal security reviews but no independent audit; do not describe it as audited. The audit scope and known limitations are in [docs/SECURITY_AUDIT_PACK.md](docs/SECURITY_AUDIT_PACK.md).

Use dedicated least-privilege identities. Never use CI production credentials. Do not assume read-only access protects confidentiality. Never publish `.local/`, kubeconfigs, logs, audit metadata or crash dumps containing private data. TLS verification cannot be disabled through KBorg flags; like kubectl, a kubeconfig cluster that sets `insecure-skip-tls-verify` is honored (refused in reviewed helper mode). Kubernetes reads accept gzip; the decoder is the maintained pure-Rust `flate2`, and both compressed and decoded bytes count against the same body budget, so a compressed response cannot expand past it. Truncated, corrupt or unrequested encodings are errors.

## Default capability model

Like k9s, everything is available by default: changes, exec/attach, port
forwards, file copy, apply, node shell, debug containers, node maintenance,
YAML editing, force kill, Secret reveal, Helm reads and writes, scans,
extensions and Copilot screen questions. What protects you is the review, not a
flag: every change is dry-run where the API allows it, shown in full, bound to
the object's UID/resourceVersion and confirmed with `y`/Enter (or cancelled with
`n`/Esc). The executor still checks the exact target, confirmation phrase and
expiry, so a stale or swapped review cannot run.

- `--read-only` turns every change off for a session.
- An operator `--config` file only takes capabilities away: any `allow_*` field
  set to `false`, `"profile": "read-only"`, and `"strict_confirmations": true`
  (typed confirmation phrases and read-every-line reviews, as in earlier
  releases). Command-line flags cannot loosen it.
- Kubeconfig is trusted as kubectl trusts it: exec credential helpers run from
  PATH with your environment, and auth providers, impersonation, `proxy-url`
  and `insecure-skip-tls-verify` are honored. `--allow-auth-helper
  --auth-helper-policy FILE` selects the reviewed helper mode instead: a
  SHA-256-pinned helper, a cleared environment, and refusal of providers,
  impersonation, proxies and insecure TLS.
- Helm, Trivy/Grype and a `copilot-policy.json` in the config folder are found
  automatically; pass `--helm-tool`, `--scanner` or `--copilot-policy` to pin a
  specific file.

Before production: independent sensitive-code review, dependency advisories/licenses, cloud-auth trust, protected audit, OS file-permission/race hardening, denial-of-service/fuzz tests, revocation/uncertain-result drills, supported platform matrix, release authenticity and deployment-specific RBAC/admission evidence.

Cluster text is sanitized before it is drawn: control, bidi and invisible format characters are removed (a zero-width joiner is kept only between two emoji). In UTF-8 terminals normal views then show real Unicode glyphs, so look-alike characters can appear in names; approval and typed-confirmation screens always show non-ASCII as reversible `\u{...}` escapes so a reviewer can tell them apart. `text: ascii` in `ui.yaml` draws `?` everywhere instead.

See docs/REMAINING_WORK.md and the linked current acceptance report for requirements and known limitations. Client safeguards do not constrain a hostile same-user process. The API server and organizational controls are authoritative.

Error messages show cluster text only where it is the actionable part:
admission webhook and policy denials, Pod Security, quota and field validation
messages. That text has control and bidirectional characters removed,
credential-like values (`token=`, `password:`, bearer tokens, `Invalid value`
payloads, long mixed-case opaque strings) replaced with `[withheld]`, and is cut
to 160 characters. RBAC denials are parsed into validated verb, resource and
namespace tokens; the user name in the server message is never shown. Other
server messages and raw transport errors are classified, not echoed, and the
server URL is reduced to its host.

Secret inspection is on by default (an operator config can set `allow_secret_reveal: false`): `:secrets` lists keys,
which lists names, types, key names and sizes for one namespace and decodes a
single key only on an explicit `x`, for 30 seconds; values are never exported,
copied, logged, audited or written to disk. `y`/`d` there show a masked YAML of
one Secret: metadata (credential-like annotation values redacted, the
`last-applied-configuration` annotation dropped because it embeds data), type,
immutable and every key as `<hidden: N bytes>`, plus structure only: TLS
certificate subject/expiry, docker registry hostnames (no usernames or auth),
and whether a service-account token exists. `x` in that view reveals one key
inline over a fresh UID-checked read; nothing reveals the whole object. Generic
resource views, lists, watches and exports still refuse Secrets and point to
`:secrets`. The listing and the YAML read still transfer Secret payloads into
process memory; KBorg overwrites the payload bytes it holds as soon as the
index or masked view is built and when a revealed value is hidden, but copies
inside the HTTP client are outside its control, so grant `get`/`list secrets`
only where that is acceptable. Ctrl-K
force kill is on by default (an operator config can set `allow_force_kill: false`).

Marked rows never widen an approval on their own. With marks, delete,
workload restart and CronJob suspend/resume prepare one batch of at most 50
objects of the current resource kind in the current context; every target goes
through the same policy checks (system namespaces refused) and server dry run
as a single-object operation, and any refusal refuses the whole batch before a
review is shown. The combined review lists each target's kind, namespace/name,
UID and resourceVersion and requires typing the verb and exact count (for
example `delete 7 objects`) within 60 seconds. Execution is sequential: each
target's UID and resourceVersion are rechecked before its write, the batch
stops at the first failure without rolling back earlier writes, and one audit
record is written per object.

Pod file copy is on by default (an operator config can set `allow_file_copy: false`) (`kborg cp-from`,
`kborg cp-to`, `:cp-from`, `:cp-to`). Each copy moves one regular file of at
most 64 MiB after a typed `namespace/pod` approval, with the target UID and
resourceVersion rechecked and an audit record written. The container runs a
fixed argv (`cat` to read; a `set -C` shell redirect to write, with the path
passed as an argument), so existing container files are never replaced and the
written size is verified. Downloads are held in memory and written only after
the transfer succeeds, as new owner-only files that never replace or follow a
local symlink. Downloaded bytes are untrusted container data: KBorg does not
execute or display them. Directory copies (`-r`) use a strict tar stream: only
regular files and directories are extracted, into a new owner-only directory
that is removed on failure; links, devices, absolute paths and `..` are
refused, and uploads create a new container directory and verify the file
count.

Server-side apply is on by default (an operator config can set `allow_apply: false`). Every object is
dry-run before review, updates are locked to the reviewed resourceVersion,
nothing is written if any object changed after review, and field-manager
conflicts are never forced. Secrets, cluster-scoped kinds, system namespaces
and GitOps-managed objects are refused. A multi-object apply is not atomic: a
failure part-way leaves earlier objects applied, and the result says which.

Interactive sessions (Pod shell `s`, attach `a`, node shell, debug and
`--interactive exec/attach`) are a full terminal, like `kubectl exec -it`. They are
on by default like k9s (`--read-only`, or an operator config with
`allow_exec: false` and the attach, node-shell or debug fields, turns them
off). Each one first shows a review of the exact Pod, container, UID and argv
and needs a confirmation (typed with `strict_confirmations`); the UID is
rechecked before connecting and each session is audited. While
connected the remote program controls your terminal: its output, including
escape sequences, is not filtered, so it can change the window title, draw
anywhere, query the terminal and, if your terminal allows OSC 52, write the
clipboard. Only open sessions to containers you trust that much. Ctrl-] (or
`--session-escape`) always disconnects, also when the remote enables kitty or
modifyOtherKeys keyboard modes. On every exit, including errors and panics,
KBorg writes a reset that aborts any unfinished escape sequence and turns off
mouse tracking, bracketed paste, application keypad/cursor keys, the alternate
screen, scroll regions, line-drawing charsets, palette changes and hidden
cursor, restores the saved title, then redraws. Sessions end at the configured
lifetime (1..3600 s), 16 MiB received output or 8 MiB typed input. Output-only
exec/attach and the controlled-input API still filter output to text and safe
colors.

Node shell is on by default (an operator config can set `allow_node_shell: false`) and is root on the
Node. The review shows the complete privileged Pod (host PID, network and IPC
namespaces). The session only targets KBorg's own node-shell Pod on the
reviewed Node; the Pod is deleted afterwards and has `activeDeadlineSeconds` as
a backstop. Grant `create pods` in a namespace that permits privileged Pods
only to people who may administer Nodes.

Ephemeral debug containers are on by default (an operator config can set `allow_debug: false`). The
review shows the exact container after a dry run; it is unprivileged and gets
no host access, though with a target container it shares that container's
process namespace. Kubernetes cannot remove ephemeral containers: the
container exits after its sleep but stays listed in the Pod. CronJob trigger,
suspend/resume and workload rollback go through the same
dry run, review, UID/resourceVersion lock and audit as other writes.

Image scanning uses Trivy or Grype found on PATH or `--scanner ABS_PATH` (and `allow_scan`
in a `--config` profile). KBorg ships no scanner or vulnerability database: it
runs the Trivy or Grype executable you name, by absolute path without PATH
lookup, after checking it is a regular, non-symlinked, non-group/world-writable
native executable and identifying it from a bounded `--version` run; the file
identity is rechecked before every process. Image references come from cluster
objects and are untrusted: only plain registry references are accepted
(`A-Za-z0-9./:-`, starting with a letter or digit, no scheme forms such as
`dir:/`), each passed as one argument after `--`; Grype also gets an explicit
`registry:` source. Each scanner process runs in its own process group with a
300-second first-image (database download) and 120-second per-image budget, a
20-minute total, a 32 MiB output cap and cancellation, and the whole group is
killed and reaped. The environment is cleared except `HOME`, `PATH`, `USER`,
proxy, TLS-certificate, Docker and scanner-cache variables, so Kubernetes and
cloud credentials are not passed; the scanner reads and writes its normal cache
under your HOME and uses your registry credentials (for example
`~/.docker/config.json`). It may download its vulnerability database and pulls
image layers from registries; the review says so before anything runs.
Scanner output is parsed as JSON and every displayed string is sanitized. The
scanner keeps your OS user's permissions; it is supervised, not sandboxed.
Nothing is written to the cluster.

Extensions run from the TUI (`:ext`) use the same contract as the CLI: an
absolute registered executable with literal arguments, a whole-input review
and `RUN TOOL` approval, 15 seconds and 256 KiB of captured output, run outside
the TUI screen. They keep your OS user's file and network access; they are
not sandboxed.

k9s plugins (`register-k9s`) add variables to that contract. Trust model: you
trust the executable you register (absolute path, SHA-256 pinned, re-checked
on every run); you do not trust cluster data. Variables expand inside a single
argv element, never through a shell, and never split or glob. A value with
control or formatting characters is refused, and a value starting with `-` is
refused where it would begin an argument (before a literal `--`), so object
names cannot inject options. Environment variables are never expanded. A
registered shell (`sh -c` scripts) is allowed only by explicit registration;
its values are limited to letters, digits and `._:/@=+,-`, and every run needs
`RUN DANGEROUS NAME`, as do plugins marked `dangerous` and arguments with a
kubectl/helm write verb. The review shows the exact expanded argv. These checks
add friction; they do not make a tool read-only.

Copilot advice can target GitHub.com, a GHE.com tenant or GitHub Enterprise
Server. Each deployment has its own exact egress allowlist, and credentials are
bound to the policy host. Two GHES-only policy options widen trust and should be
set deliberately: `allow_private_network` lets the egress broker reach RFC 1918,
CGNAT and IPv6 ULA addresses for that one host (loopback, link-local and
metadata addresses stay blocked), and `tls_ca_file` makes TLS for that host
trust the supplied internal CA. In GHES offline mode the instance forwards the
reviewed bundle to its administrator-configured model provider, which KBorg
cannot see or verify.

`:ask` (on by default, and refusable in the
operator configuration with `allow_copilot_screen: false`) sends the visible
screen text, which is not minimized: it can include context, namespace and
object names, statuses, events and log lines. Display redaction (masked
Secrets, hidden credentials) applies first, and the review shows the exact
bytes. Screen text is passed to the model as data on stdin under a fixed
instruction; a hostile log line can still try to steer the answer, which is
why answers are advice only and never executed.

Helm storage access is on by default (an operator config can set `allow_helm: false`). Generic Secret
YAML/describe views remain unavailable, but Helm metadata/history and selected values are
read from Kubernetes Secret or ConfigMap storage. Kubernetes RBAC cannot restrict
Secret list permission by label selector; the API identity must be scoped
accordingly. Browsing never exports raw release payloads, notes, chart files or
rendered manifest bodies. Lifecycle review separately shows redacted affected
objects. Known-key and private-key redaction cannot classify every
confidential custom value. Logs and explicitly requested values remain sensitive.

Helm lifecycle is on by default (refusable with `allow_helm_write: false`) and uses an explicitly
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

## Verifying releases

Releases from the `Signed release` workflow carry a keyless Sigstore bundle (`FILE.sigstore.json`) and GitHub build provenance for every asset, plus a signed `SHA256SUMS`. Verify with `gh attestation verify FILE --repo c0dewars/kborg` or `cosign verify-blob`; see [Release process](docs/RELEASE_PROCESS.md#verify-a-release-asset). Releases up to 0.5.0-rc.3 are unsigned. Dependencies are checked with cargo-deny (`deny.toml`).
