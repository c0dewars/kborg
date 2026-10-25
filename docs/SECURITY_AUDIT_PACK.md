# KBorg external security audit pack

Prepared 2026-10-24 for an independent security assessment of **KBorg 0.4.0**
(branch `release/0.4.0`, head `0a11c8f` at the time of writing), updated the same day for
the **0.5.0** additions on `feature/0.5.0` (apply, node shell, file/directory copy, TUI
extensions, lint, benchmark, hotkeys; see section 7a). This pack is a scoping
aid. It is not a security claim or a certification. Every statement below is meant to be
checkable against the source; file paths are relative to the repository root. Where a
control is missing or only partial, this pack says so.

Related material: [SECURITY.md](../SECURITY.md), [REMAINING_WORK.md](REMAINING_WORK.md),
[MAC_V1_SECURITY_REVIEW.md](MAC_V1_SECURITY_REVIEW.md),
[TRUST_SECURITY_REVIEW.md](TRUST_SECURITY_REVIEW.md), [SECURITY_REVIEW.md](SECURITY_REVIEW.md),
[SECURITY_REVIEW_0.3.md](SECURITY_REVIEW_0.3.md), [COPILOT_INTEGRATION.md](COPILOT_INTEGRATION.md),
[OPERATIONS.md](OPERATIONS.md), [HELM_LIFECYCLE.md](HELM_LIFECYCLE.md),
[HELM_EXTENSIONS.md](HELM_EXTENSIONS.md), [STORAGE.md](STORAGE.md).

## 1. Product summary and deployment model

KBorg is a single-binary Rust Kubernetes terminal UI and CLI with k9s-style navigation.
Engineers inside companies install it themselves and run it on their own laptops.

| Aspect | Fact |
|---|---|
| Platforms | macOS ARM64 is the primary target. Linux (x86_64/ARM64) and Windows are built (`.github/workflows/ci.yml`). Some features only work on Unix (auth helpers, Helm lifecycle, tool handoff) or only on macOS ARM64 (Copilot inference). |
| Distribution | Unsigned archives with checksums, a CycloneDX SBOM and unsigned build records; a Homebrew formula (`packaging/`, `docs/HOMEBREW.md`). No notarization. |
| Identity | Uses the invoking user's kubeconfig (`--kubeconfig`, `$KUBECONFIG` with up to 16 files merged kubectl-style, or `~/.kube/config`): `src/gateway/kubeconfig.rs::config_paths`, `merge_config_files`. |
| Server component | None. KBorg talks to the Kubernetes API server directly. It runs no daemon, no listener except opt-in loopback port forwards and the per-run Copilot broker, no update channel and no telemetry. |
| Default posture | Read-only. Secrets are excluded. Every mutating or sensitive capability needs an explicit flag (see section 4). |
| Persistence | Per-user config directory (`src/safety.rs::config_dir`): `~/Library/Application Support/KBorg` on macOS, `$XDG_CONFIG_HOME/kborg` or `~/.config/kborg` on Linux, `%APPDATA%\KBorg` on Windows. `KBORG_CONFIG_DIR` (absolute path) overrides it. A legacy `K8Borg` directory is reused if present. |
| Authority | The API server's RBAC, admission and audit are the authoritative controls. Client-side checks are defense in depth and help prevent operator mistakes. |

Organization-managed policy: commit `0cb27f2` added a root-owned machine policy. Commit `c859eb6` removed it again because KBorg runs on engineer-owned laptops, not company-managed machines. The only remaining policy file is the optional per-user `--config` profile (`src/config.rs`).

## 2. Assets and security objectives

| Asset | Where it lives | Objective |
|---|---|---|
| Cluster credentials (bearer tokens, client keys, CA, ExecCredential output) | User kubeconfig; process memory; Helm lifecycle temp kubeconfig (`src/helm_lifecycle.rs::StaticConnection::write`) | Never displayed, logged, exported or sent anywhere except the selected API server over verified TLS (Helm: to the trusted native Helm child only). |
| Kubernetes Secret values | API server; process memory only when `--allow-secret-reveal` or `--allow-helm` is set | Not displayed unless the user reveals one key explicitly (30 s), and never exported, copied or written to disk. |
| Cluster integrity | API server | No write without `--write` plus a capability flag, a fresh review, server dry-run where applicable, UID/resourceVersion binding, exact typed approval and no automatic retry. |
| Other confidential object data (env values, annotations, CRD strings, logs) | API responses | Known-key redaction for display and export. Logs only with `--allow-logs`. KBorg does not claim to classify arbitrary confidential data. |
| Local files | Config directory, exports, audit spool, saved selections, themes | Private modes (0700 directories, 0600 files), no symlinks, create-only or atomic replacement, size limits, strict schemas. |
| Copilot / GitHub tokens | `gh` keychain entry or 0600 credential file; process memory; child environment | Bound to one host and account. Only the sandboxed Copilot child receives them, and only through the authenticated egress broker. |
| User terminal | The TTY KBorg draws on | Untrusted bytes never reach the terminal as control sequences (no OSC 52 clipboard writes, hyperlinks, title changes, queries or bidi spoofing). Approval screens must be unambiguous. |
| Third-party executables (Helm, Cosign, gh, Copilot native, auth helpers, tools) | User-selected absolute paths | Run only after explicit review, with pinned hashes where implemented, a scrubbed environment, deadlines and process-group cleanup. |

## 3. Trust boundaries and threat model

**Trusted:** the invoking user and their interactive keystrokes, CLI flags, environment
variables (`KUBECONFIG`, `KBORG_CONFIG_DIR`, `VISUAL`/`EDITOR`, `KBORG_DEMO_SCALE`), and
local files the user owns and selects: kubeconfig, `--config`, policies, keyrings, the
alias, hotkey and theme files. Executables the user pins are trusted at the hash the user
pins. The OS, the Rust toolchain and the crates.io dependencies are also trusted (see
section 6).

**Untrusted (attacker-controlled in the threat model):**

- **Kubernetes API responses:** object names, labels, annotations, status messages, events, CRD fields, discovery documents, error bodies, pagination tokens, watch streams. A compromised or hostile API server, or a lower-privileged tenant who can write objects the user views, is in scope.
- **Container output** from logs, exec and attach streams.
- **Helm material:** release storage Secrets and ConfigMaps (gzip+base64 JSON), chart archives (tar.gz), OCI registries (manifests, redirects, bearer challenges), provenance files.
- **Copilot model output,** and anything returned through the GitHub/Copilot services.
- **Pasted terminal input** (bracketed paste), which may be crafted text the user copied from elsewhere.
- **Network paths** between KBorg and the API server, registries and GitHub, which are protected by TLS verification.

**Out of scope: hostile same-user local processes.** A process running as the same OS user
can already read the kubeconfig and keychain-accessible tokens, ptrace or replace the
KBorg binary, edit the config directory, and inject keystrokes into the terminal. Client
code cannot defend against it, and the earlier internal reviews made the same assumption
(`SECURITY.md`, `docs/TRUST_SECURITY_REVIEW.md`). Path checks and hash pins in KBorg narrow
accidental or time-of-check/time-of-use (TOCTOU) problems; they are not a defense against
this adversary. **Other local users** (multi-user hosts) remain in scope for file
permissions, temp directories and loopback listeners: port forwards accept connections
from any local user (documented), and the Copilot broker requires a per-run nonce.

## 4. Attack surface inventory

### 4.1 Capabilities and enforcement points

Flags are parsed in `src/cli.rs::parse`. An optional `--config FILE` profile
(`src/config.rs::apply`) can only *forbid* flags. It never enables them.

| Capability | Opt-in | Enforcement point(s) |
|---|---|---|
| Any mutation | `--write` (`--readonly` resets) | `src/policy.rs::Policy::check`; `src/gateway/operations.rs::prepare`/`execute`/`operation_api` |
| Scale / restart / set-image / set-resources / edit-container / delete | `--write` | `Policy::check` checks resource families and the scale range 0..20, and refuses `kube-system`, `kube-public` and `kube-node-lease` |
| Pod force kill (grace 0) | `--write --allow-force-kill` | `operations.rs::prepare` (`Action::Kill`), `delete_params` |
| YAML spec edit | `--write --allow-yaml-edit` | `operations.rs::yaml_edit_access`, `src/manifest_edit.rs::check_scope` |
| External editor for YAML drafts | `--allow-external-editor` | `src/tui/keys.rs` (Ctrl-E), `src/external_editor.rs::edit` |
| Node cordon/uncordon/drain | `--write --allow-node-maintenance` | `operations.rs::node_maintenance_access`, `src/node_maintenance.rs` |
| Logs (snapshot, follow, merged, workload, dynamic) | `--allow-logs` (`--dynamic-membership` for live membership) | `src/gateway/log_streams.rs` (`if !self.logs`) |
| Exec / attach / port-forward | `--write` + `--allow-exec` / `--allow-attach` / `--allow-forward`; `--interactive` for stdin/TTY | `operations.rs::operation_api`, `src/ops.rs::prepare`/`verify_review`, `src/session.rs` |
| Pod file / directory copy (0.5.0) | `--write --allow-file-copy` (`--recursive` / `-r`) | `ops.rs` (`Request::CopyFrom`/`CopyTo`, capability 3), `src/file_copy.rs`, `src/archive.rs` |
| Node shell (0.5.0) | `--write --allow-node-shell`, `--node-shell-image` | `src/node_shell.rs::prepare`/`start`, `ops.rs` (`Request::NodeShell`, capability 4; Pod deleted in `run`) |
| Server-side apply (0.5.0) | `--write --allow-apply` | `src/gateway/apply.rs::prepare_apply`/`execute_apply`, `src/manifest_apply.rs` |
| Port-forward benchmark (0.5.0) | an active reviewed forward | `src/bench.rs` (127.0.0.1 only; 2000 requests, 20 connections, 60 s) |
| Hotkeys (0.5.0) | `hotkeys.yaml` in the config directory | `src/hotkeys.rs` (types a command into the normal parser; never a shell) |
| Secret listing, masked YAML + per-key reveal (TUI) | `--allow-secret-reveal` (does **not** require `--write`) | `src/gateway/reads.rs::secret_api`, `secret_value`, `secret_yaml`; `src/secrets.rs`; `src/ui_secrets.rs` (30 s) |
| Helm release storage reads (reads Secrets) | `--allow-helm` | `src/gateway.rs::helm_reader`, `src/helm.rs` |
| Helm lifecycle | `--write --allow-helm --allow-helm-write`, `--helm-tool ABS` (Helm 4.3.1, Unix) | `gateway.rs::helm_lifecycle_access`, `src/helm_lifecycle.rs` |
| Helm hooks/tests, CRDs | add `--allow-helm-hooks`, `--allow-helm-crds` | `gateway.rs::helm_extension_access`, `src/helm_extensions.rs`, `src/helm_crds.rs`, `src/helm_migration.rs` |
| Chart provenance | `--helm-keyring ABS` (OpenPGP), `--helm-sigstore-policy ABS` (Cosign) | `src/helm_provenance.rs`, `src/helm_sigstore.rs` |
| OCI registry | `--helm-registry-ca ABS`, `--helm-registry-auth ABS` | `src/helm_oci.rs` |
| Reviewed external tool views | `--allow-tools --tool ABS [--tool-arg]` | `src/handoff.rs` |
| Registered extensions | `--allow-extensions --allow-tools` | `src/extensions.rs`, `cli.rs::extensions_command`; TUI `:ext` in `src/tui/commands.rs::run_extension` (runs outside the TUI screen); `extensions::import_k9s` only reports |
| ExecCredential helper | `--allow-auth-helper --auth-helper-policy FILE` (reviewed via `auth-helper review`) | `gateway.rs::connect`, `src/auth.rs::prepare`/`acquire` |
| Copilot advice | `--copilot-policy ABS` + per-transfer typed approval | `src/advice.rs`, `src/copilot*.rs` |
| Multi-context dashboard | `--contexts A,B` (1..9), `dashboard` | `src/overview.rs::read_only`, `src/tui/commands.rs::pane_connection` (removes write, exec, YAML, node, kill, secret-reveal and Helm-write flags; keeps `--allow-logs`/`--allow-helm`) |
| Demo | `kborg demo` / `--demo` | `src/gateway/demo.rs`: no network and no credentials; `KBORG_DEMO_SCALE` adds up to 20,000 synthetic Pods |

The kubeconfig is refused if it sets `insecure-skip-tls-verify`, `proxy-url`, a legacy `auth-provider` or impersonation, or uses a non-HTTPS, userinfo-bearing or query-bearing server URL (`src/gateway.rs::connect`). There is **no flag that disables TLS verification**.

### 4.2 Local files read and written

| Path (under config dir unless absolute) | Access | Protection |
|---|---|---|
| kubeconfig(s), CA/cert/key/token files | read | `safety::read_small` (1 MiB, no symlink in any ancestor, regular file before and after open), strict UTF-8, duplicate/control-bearing names refused per file |
| `--config` profile | read | 16 KiB, strict schema 1, `deny_unknown_fields` |
| `audit/` intent/outcome JSONL spool | create | `safety::write_new` (0600, `create_new`), bounded count; read by `kborg audit pending` (`src/audit.rs`) |
| `exports/`, `screendumps/` | create | create-only, private, size/count limits, redacted, Secret export disabled (`src/exports.rs`, `src/screendump.rs`) |
| `selections/v1.json`, `last-dashboard.json`, `appearance.json` | replace | `safety::atomic_write` (same-directory staged `create_new` file, then rename), lock file for selections |
| `themes/`, `aliases.yaml`, `hotkeys.yaml` | read/create | strict parsing and limits (`src/themes.rs`, `src/aliases.rs`, `src/hotkeys.rs`) |
| `extensions/` registry | read/create | requires mode with no group/other bits (`src/extensions.rs`) |
| `edit/spec-*.yaml` | create, then removed | 0600 create-only; removed on drop (`src/external_editor.rs`) |
| `$TMPDIR/kborg-helm-*` | create | 0700 directory; static-credential kubeconfig written 0400; removed on drop (`src/helm_lifecycle.rs::Workspace`) |
| `$TMPDIR/kborg-copilot-*` | create | random 128-bit suffix, 0700; pinned-executable snapshot, sandbox state; removed after run (`src/copilot_runtime.rs`) |
| Auth helper policy, Copilot policy and credential file, Sigstore policy, keyring | read | absolute paths only; mode checks (auth policy 0600; Copilot credential 0600 and single link; Sigstore/Copilot authorities with no group/other write) |

New directories get mode 0700 and files 0600 (`safety::ensure_private_dir`, `write_new`). Existing parent directories are not chmod'ed. Windows ACL provisioning and handle-based race resistance are open gates (`REMAINING_WORK.md` S01).

### 4.3 Network egress

| Destination | Client | Notes |
|---|---|---|
| Kubernetes API server | `kube` 4.2 with rustls/ring, `accept_invalid_certs=false`, no proxy, compression off, no retry, 5 s connect timeout | `src/transport.rs::BodyBudgetLayer`: 8 MiB object bodies, 64 MiB log streams, 15 s header and 10 s idle budgets |
| OCI registries | Separate verified HTTPS client (`src/helm_oci.rs`) | Explicit host-bound credentials only (no Docker/Helm credential stores). At most 3 redirects; manifest redirects must keep the origin; credentials are not sent cross-origin; blobs are digest-checked and size-bounded |
| GitHub / Copilot | Only through the sandboxed native child and the loopback broker (`src/copilot_proxy.rs`) | Exact per-deployment authorities on port 443 (`src/copilot.rs::Deployment::authorities`); see section 5.6 |
| Loopback | Port-forward listeners on `127.0.0.1` (ports 1024+, 1..300 s, 4 clients); Copilot broker on `127.0.0.1:0` | |

### 4.4 Subprocesses

| Program | Spawned by | Containment |
|---|---|---|
| ExecCredential helper (absolute native binary, SHA-256 pinned) | `src/auth.rs::run_helper` | `env_clear` plus approved environment, process group, 15 s, 256 KiB output; interpreters, shells and PATH lookup refused. **Not sandboxed** |
| Helm 4.3.1 (`--helm-tool`) | `src/helm_lifecycle.rs` | Scrubbed environment (no PATH), private HOME/cache/config, a static-credential kubeconfig, deadlines, process group. **Not sandboxed** |
| Cosign (Sigstore policy) | `src/helm_sigstore.rs` | Pinned executable and input snapshots, scrubbed environment, output/deadline bounds. **Not sandboxed** |
| `gh auth token --hostname H --user L` | `src/advice.rs` | Pinned snapshot; the only step that sees the user's HOME and gh config |
| Copilot native 1.0.91 (macOS ARM64) | `src/copilot_runtime.rs` via `/bin/sh` (ulimits) then `/usr/bin/sandbox-exec` | SBPL sandbox, empty HOME, broker-only network |
| `/usr/bin/dscacheutil` | broker DNS (`copilot_proxy.rs::resolve`) | Fixed argv, empty environment, 3 s, 4 KiB |
| Registered tools / `--tool` | `src/handoff.rs::supervise` | Redacted view on stdin, scrubbed environment, private HOME/cwd, 15 s, 256 KiB, output sanitized and never applied. **Not sandboxed** |
| `$VISUAL`/`$EDITOR` | `src/external_editor.rs` | Split on whitespace, no shell; **inherits the full user environment and PATH** |
| `copilot login` (legacy) | `src/copilot.rs::login` | Interactive, fixed argv, filtered environment |
| `/bin/kill`, `/usr/bin/mkfifo` | supervision helpers | Fixed absolute paths |

## 5. Key controls to verify

### 5.1 Review, typed approval and preconditions

- **Workload writes** (`src/gateway/operations.rs`):
  - `prepare` fetches the live object, builds a patch embedding `metadata.uid` and `resourceVersion`, renders a summary through `safety::review_text` (rejects controls and truncation), then runs the same request with `dryRun=All`.
  - The dry-run response is checked for admission drift: `edit::check_admission`, `check_resource_admission`, `check_metadata`, replica equality, restart spec equality.
  - `execute` re-checks the private reviewed copies of target, action and summary, `Policy::check`, a 60 s expiry and the session, then consumes a single-use `policy::Approval`. The typed confirmation must equal `namespace/name`.
  - It writes an audit intent, performs the write with a 15 s timeout and records an "UNKNOWN" outcome on timeout. Deletes use `Preconditions{uid, resourceVersion}`.
- **YAML spec edits** (`src/manifest_edit.rs`):
  - Parsing uses strict `serde-saphyr` budgets: no aliases, anchors, merge keys or duplicate keys; depth 32; one document.
  - Only `spec` may change, and hidden or sensitive placeholders are locked.
  - The JSON Patch starts with `test` ops on `/metadata/uid` and `/metadata/resourceVersion`.
  - Execution re-runs the dry-run and checks the applied result (`check_applied`).
- **Exec, attach and forward** (`src/ops.rs`): argv is limited to 1..64 arguments and 2048 bytes and refused if `safe_text` changes it. UID and resourceVersion are re-verified before connecting (`verify_review`), but Kubernetes `connect` subresources have no UID precondition, so a narrow name-recreation race remains (documented). Interactive sessions carry 16 MiB output and 1 MiB input limits.
- **Approval display:** TUI approval screens use the reversible `safety::review_display` (MAC-REVIEW-001) and refuse approval when the review cannot be fully painted (MAC-REVIEW-002, commit `2a33306`).
- **Helm lifecycle:** 15-minute full review, fresh plan/history/UID checks before execution. Helm's own requests carry no UID preconditions, which leaves a documented race.
- **Verify:** that scale via the `scale` subresource honors the `metadata.uid` and `resourceVersion` in the merge patch; that summary and target binding cannot drift; the expiry and replay behavior; that CLI review prompts (`cli.rs` prints `summary()` directly) are as unambiguous as the TUI review.

### 5.2 Redaction and terminal safety

- `safety::redact` replaces values under these keys with `[REDACTED]`: `data`, `stringData`, `binaryData`, `annotations`, `managedFields`, `value`, `token`, `password`, `client-key-data`, and any key containing `secret` or `credential`. It also passes every string value through `safe_lines`. It applies on `get`/`list` (`gateway/reads.rs`). It is not a general classifier.
- `safety::safe_text` drops Unicode `Cc` controls (C0, DEL, C1) and specific bidi and zero-width code points (U+061C, U+200B–200F, U+202A–202E, U+2060–206F, U+FEFF).
- The TUI cell renderer (`src/tui/format.rs::display`) additionally maps all non-ASCII to `?`.
- Interactive session output passes through a fixed-storage escape filter (`src/session.rs::OutputFilter`). It keeps only approved SGR sequences and drops OSC (including 52 and 8), DCS, cursor, private-mode and query sequences.
- Remote exec output (non-interactive) is bounded to 256 KiB and passed through `safe_lines`.

### 5.3 Bounded resources

| Area | Limits |
|---|---|
| Watch / list | 10,000 objects, 64 MiB, 500-object pages, 15 s list / 12 s watch budgets (`src/watch.rs`) |
| Discovery | 100 groups, 200 kinds, 102 requests, 1 MiB per document, 16 MiB aggregate, 15 s (`src/discovery.rs`) |
| Pagination | repeated continue tokens fail; page and time caps (`SECURITY_REVIEW.md`) |
| Logs | 256 KiB / 2000-line rolling view, 4 KiB lines, 1 MiB/s and 5000 lines/s, 1..86400 s follow, ≤8 Pods / 32 streams, ≤5 reconnects (`src/logs.rs`, `src/logs/budget.rs`) |
| Dashboard tail | 6 workloads, 24 Pods (`src/workload_tail.rs`) |
| Metrics | 1000 objects/rows, 8 MiB, 10 pages, 15 s (`src/metrics.rs`) |
| Secrets | 500 per namespace, 16 KiB reveal (`src/secrets.rs`) |

### 5.4 Kubeconfig and credential helper

- See section 4.1 for the refused configurations.
- Exec helpers need a separately reviewed trust policy (`auth-helper review` → `TRUST AUTH HELPER`). The policy binds context, user, server, TLS name, CA SHA-256, executable SHA-256 and exec/info hashes. Credentials must expire within 24 h. Expiry, 401 or 403 stop the session (`src/auth.rs::AuthGate`).
- Kube's automatic exec refresh is never configured.
- Multi-file `KUBECONFIG` merge gives first-defined-wins semantics (`kubeconfig.rs::merge_config_files`). Relative paths resolve per file.

### 5.5 Secret reveal path

- `:secrets` lists name, UID, type and key byte lengths. The listing does transfer every Secret payload of the namespace into process memory.
- `x` re-fetches one Secret, refuses a different UID, validates the key charset, decodes at most 16 KiB of control-free UTF-8 and shows it for 30 s (`ui_secrets::REVEAL_SECONDS`).
- Values are never exported, copied or persisted. There is no zeroization of memory.

### 5.6 Copilot containment (macOS ARM64 only)

- **Policy:** strict JSON policy with no group/other-write; pinned native and gh SHA-256; exactly one credential source; host-bound tokens; classic `ghp_` tokens refused. Every review starts with fresh token-free sandbox canaries.
- **Payload:** the bundle is an allowlist of fixed status words plus counters (`src/copilot.rs::minimize`). Unknown strings become `Other`. Every transfer needs an exact host and account confirmation, expiring after 5 minutes.
- **Sandbox** (`src/copilot_runtime.rs::profile`): `(deny default)` with denials for mach-lookup, network, fork, file read/write and executable mapping. Only the pinned snapshot may exec, only the private state directory is writable, and the only outbound destination is `localhost:<broker port>`. Ulimits cover CPU, core, file size and descriptors.
- **Broker** (`src/copilot_proxy.rs`):
  - Accepts only `CONNECT host:443 HTTP/1.1` with a per-run 256-bit nonce in Basic proxy auth and a matching Host header; bodies are refused.
  - The host must be an exact approved authority.
  - The parsed ClientHello must carry exactly one DNS SNI equal to the authority; ECH (0xfe0d) is refused.
  - DNS goes through bounded `dscacheutil`, and every resolved address must be public. Under `allow_private_network` (GHES only), RFC 1918, CGNAT and ULA ranges are also accepted; loopback, link-local and metadata addresses stay blocked.
  - Limits: 24 connections, 4 concurrent, 4 MiB, plus the run deadline.
- **Deployments:** GitHub.com, `TENANT.ghe.com` and GHES (offline provider mode, explicit `deployment: server`, optional `tls_ca_file`). Live GHE.com and GHES inference has **not** been demonstrated (`COPILOT_INTEGRATION.md`).
- **Output:** bounded and terminal-sanitized. It never authorizes commands, and output containing the credential plaintext or base64 is suppressed.

### 5.7 External tools, editor and Helm provenance

- **Tools and extensions:** absolute native executables, hash and mode re-checked immediately before spawn, `RUN TOOL` approval, literal argv. Object metadata is never substituted into argv.
- **Editor:** private create-only scratch file. The edited text re-enters the YAML validation, dry-run and review path; the editor never writes to the cluster directly.
- **Helm provenance:** `--helm-keyring` requires `helm verify`-style OpenPGP provenance from an explicit public keyring (KBorg performs no signature cryptography itself, `src/helm_provenance.rs`). `--helm-sigstore-policy` binds the chart archive, Cosign executable, policy, bundle and key/root or identity/issuer, and verifies both before review and before execution.
- **Gaps:** OCI manifest/referrer signatures and general attestations are not verified. Digest pinning does not establish who published a chart.

## 6. Known limitations and accepted risks

| # | Limitation | Source |
|---|---|---|
| L1 | Binaries are unsigned and not notarized. Build records are unsigned; archive bytes are not proven reproducible. | `REMAINING_WORK.md` D01, `RELEASE_NOTES.md` |
| L2 | Copilot containment relies on Apple's deprecated/private `sandbox-exec`/SBPL. Canaries show observed denials, not a proof. Other platforms have no inference. | `COPILOT_INTEGRATION.md` |
| L3 | No protected external audit sink. The local JSONL spool is user-mutable and is an operational aid only. | `REMAINING_WORK.md` S02 |
| L4 | Helm, Cosign, auth helpers, tools and the editor run with the user's full OS permissions (supervision, not sandboxing). | `SECURITY.md`, `auth.rs`, `handoff.rs` |
| L5 | Name-recreation races: Pod `connect` subresources and Helm requests have no UID preconditions; CRD create-after-check races. | `SECURITY.md`, `ops.rs` |
| L6 | Dry-run is not a reservation. Admission can differ at the real write. Uncertain outcomes are recorded and never retried. | `SECURITY_REVIEW.md` |
| L7 | Redaction is key-based; logs, labels, status and CRD strings may hold secrets. Copilot counters can still disclose state. | `SECURITY_REVIEW.md` §4 |
| L8 | Secret listing and Helm storage reads pull full Secret payloads into memory. RBAC cannot scope `list secrets` by label. | `SECURITY.md` |
| L9 | Filesystem checks are metadata-based (TOCTOU windows). Windows ACL and handle-based hardening are open. | `REMAINING_WORK.md` S01 |
| L10 | Port forwards are reachable by other local users on `127.0.0.1`. | `ops.rs` review text |
| L11 | No HTTP proxy support (refused), no Windows auth helpers, no cloud-identity acceptance testing. | `REMAINING_WORK.md` S03/S05 |
| L12 | No fuzzing campaign and no protected-audit or revocation drills yet. Dependency scanning is a point-in-time check, and CI is manual (`workflow_dispatch`) with no automated `cargo audit`/`cargo deny` job. | `REMAINING_WORK.md` S06, `ci.yml`, `DEVELOPMENT_GUIDE.md` |
| L13 | Ending an interactive session does not guarantee the remote process terminates. | `session.rs::review` |

## 7. Prior internal reviews and fixes

All prior reviews were internal or agent-performed. **None is an external assessment.**

| Date / doc | Key findings → fixes |
|---|---|
| 2026-10-09 `SECURITY_REVIEW.md` | Unbounded empty-page pagination → 10-page/15 s cap. Discovery had no deadline → 15 s. Mutable prepared-review fields → private immutable copies. Duplicate kubeconfig names → rejected. Credential-bearing URL query/userinfo → rejected. Path inspection errors ignored → fail closed. Ambient directory modes → 0700. Relative config env paths → rejected. Copilot login environment filtering. |
| 2026-10-10 `SECURITY_REVIEW_0.3.md` | Invisible Unicode in exec argv versus the displayed review → argv refused if sanitization changes it. Unreviewed admission metadata changes → metadata comparison. Preference delete-before-rename → atomic replace. Discovery accumulated before limiting → per-document/aggregate caps. |
| 2026-10-23 `TRUST_SECURITY_REVIEW.md` | Sigstore authority-file permissions; stalled-stdin/descendant pipe supervision; broker private/special range gaps (CGNAT, benchmarking, `3fff::/20`); explicit account-bound credential reuse; UI queue/late-response locks; native TLS metadata allowance; unknown diagnostic strings → `Other`. |
| 2026-10-23 `MAC_V1_SECURITY_REVIEW.md` | MAC-REVIEW-001 (Medium): ambiguous Unicode in approvals → reversible escaped display. MAC-PACKAGE-001 (Medium): unchecked source labels and overstated package evidence → clean-HEAD/fresh-build binding. MAC-REVIEW-002 (Low): export approved while hidden → minimum-size and unknown-size refusal (`2a33306`). |
| 2026-10-24 commits | `0cb27f2` added the org policy, binding it to the resolved API server rather than the user-chosen context name (which would have been bypassable by renaming a context). It also bumped tokio 1.53.2 → 1.54.1 to clear **RUSTSEC-2026-0386** (recorded in the commit message only; no 0.4.0 cargo-audit evidence file exists yet). `c859eb6` removed the org policy entirely. `8060e1f` fixed an ignore rule that had silently dropped `src/gateway/kubeconfig.rs` from the repository. |

**Not covered by any prior review:** the 0.4.0 features `--allow-secret-reveal`, `--allow-force-kill`, `--allow-external-editor`, multi-file `KUBECONFIG` merge, GHE.com/GHES Copilot deployments (`dd8806e`), dashboard log panes and cross-cluster tail (`401ce72`, `920cfa1`), RBAC subject views, and the module splits of gateway, logs and tui (`879ea0a`, `b728bc0`, `e332f51`).

## 7a. Changes after this pack was written (0.5.0)

Findings from section "Concerns" in the first draft that are now fixed on `feature/0.5.0`:

| Concern | Fix |
|---|---|
| `--config` did not constrain the newer capabilities; a read-only profile could not block Secret reveal | `src/config.rs::apply` now gates Secret reveal, force kill, YAML edit, node maintenance, external editor, file copy, apply and node shell (`0ff346f`) |
| `safe_text` missed soft hyphen, U+2028/2029, tag characters and U+FFF9–FFFB | added to the filter (`1bffaaf`) |
| JSON/YAML object keys were not sanitized | `safety::redact` rewrites unsafe keys (`1bffaaf`) |
| OCI private-host check saw only literal hosts | cross-origin redirect and token-realm hosts reached from a public registry are resolved and refused if any answer is private, loopback or CGNAT; DNS rebinding between check and connect remains (`1bffaaf`) |
| A crash could leave the Helm temp credential kubeconfig | stale `kborg-helm-*` workspaces owned by the user and older than two hours are removed before a new one is created (`1bffaaf`) |
| `SECURITY.md` described a 0.3 build; help text said four dashboard contexts | corrected |

Still open from that list: the Copilot broker inspects only the first ClientHello (the TCP destination stays the approved host's own addresses), cross-file kubeconfig shadowing, the external editor's full environment, multi-line paste into interactive sessions, the small system-namespace denylist, and dashboard panes keeping `--allow-logs`/`--allow-helm` across contexts.

New surface to include in the audit:

- **Apply**: manifest parsing (`navigation_views::manifest_documents`), kind lookup by exact apiVersion/kind, the review diff (`manifest_apply::diff`, which hides data/env/credential-like values), the recheck-before-write and resourceVersion lock, conflict handling (409 never forced) and partial failure across objects.
- **Node shell**: the privileged Pod spec, the guard that sessions only target KBorg node-shell Pods on the reviewed Node, deletion after the session, `activeDeadlineSeconds`, and Pod Security interaction.
- **File/directory copy**: the fixed container argv (path passed as `$1`), upload verification (`wc -c` / file count), local no-overwrite and symlink refusal, and `archive::extract` against hostile tar streams (path traversal, links, PAX/GNU long names, sizes).
- **Benchmark**: confirm it can only reach 127.0.0.1 ports of the session's own listening forwards.

## 8. Suggested audit scope and priorities

Ranked by estimated risk × exposure:

| # | Area | Why | Suggested approach |
|---|---|---|---|
| 1 | Hostile API server → terminal/approval integrity | Every object string is attacker-controlled; spoofed approvals or escapes are the highest-leverage abuse | Run a hostile API server fixture (extend `scripts/test-transport-faults.py`). Feed a terminal-escape corpus (OSC 52/8, DCS, CSI queries, C1 as UTF-8, bidi, tag chars U+E0000–E007F, soft hyphen, U+2028, homoglyphs) into names, labels, events, logs and CRD keys **and values**. Test the TUI, `--json` and `--yaml` output, CLI review prompts and exports |
| 2 | Write-path binding (review → dry-run → execute) | Integrity of production clusters | Property and differential tests on `Prepared`/`Approval`. Race replacements between review and execute. Mutating webhook fixtures that alter dry-run versus the real write. Check the scale-subresource precondition. Exercise the YAML JSON-Patch `test` ops with JSON-pointer edge cases |
| 3 | Copilot sandbox and broker | Only path where tokens and cluster data leave toward a third party | Attempt sandbox escape (mach services from `system.sb`, file-metadata leaks, exec of a copied binary, Unix sockets, DNS). Fuzz `copilot_proxy::sni`, `client_hello` and `authority` (cargo-fuzz). Test HelloRetryRequest second ClientHello, domain fronting on approved CDN fronts, DNS rebinding between check and connect, IPv4-mapped/NAT64 addresses, nonce brute force from another local user |
| 4 | Secret handling | Highest-value data | Verify no Secret bytes reach exports, screendumps, the audit spool, panic output, crash dumps or logs. Check reveal timeout and UID binding. Assess memory exposure (no zeroize). Confirm Helm storage reads never surface raw payloads |
| 5 | Kubeconfig / exec helper / TLS | Credential theft or MITM | Fuzz kubeconfig YAML and merge. Look for paths that bypass `connect` refusals (TLS server name, CA data versus file). Check env-proxy influence on `kube`/hyper. Swap the helper hash or argv between review and use (TOCTOU) |
| 6 | Helm chart, OCI and provenance parsing | Untrusted archives and registries | Fuzz `helm_lifecycle::inspect_chart` (tar headers, sizes, PAX/long names, gzip bombs, `MultiGzDecoder`), Helm release decoding (`src/helm.rs`) and OCI manifests. Test redirects and token realms to hostnames resolving to private IPs (`helm_oci::private_host` checks literals and suffixes only). Check keyring/Sigstore downgrade |
| 7 | Exec/attach/session stream handling | Remote container output and pasted input reach the user's TTY | Fuzz `session::OutputFilter` (split sequences, 8-bit C1, long OSC). Run a paste-injection corpus. Exercise resize and Ctrl-] restoration under floods |
| 7b | File copy, apply and node shell (0.5.0) | New write paths, one of them root on a Node | Hostile tar corpus against `archive::extract`; container paths with quotes, newlines and leading dashes; replace the target Pod or object between review and execute; webhook that changes apply results between dry run and write; confirm the node-shell session cannot target a non-KBorg Pod |
| 8 | Local file handling | Other-local-user and permission issues | Shared `/tmp` on Linux, pre-created directories and permissive umask. Symlink races on `config_dir`, `atomic_write` and `$TMPDIR/kborg-helm-*`. Check the temp credential kubeconfig left behind after SIGKILL |
| 9 | Resource exhaustion | Hostile server or noisy cluster | Slow-loris bodies, huge watch events, many-group discovery, log floods, 9-context dashboards. Check the resident-memory peak against the documented caps |
| 10 | RBAC/admission assumptions and capability gating | Documented reliance on server controls | Confirm each flag in section 4.1 gates every code path (CLI and TUI, including dashboard pane connections). Confirm `--config` profiles constrain all capabilities. Test with least-privilege roles |

Out of scope unless agreed: the GitHub/Copilot services themselves, the Kubernetes API server and controllers, Apple sandbox internals, and same-user attackers.

## 9. Build, run and test

- **Toolchain:** `rust-toolchain.toml` pins 1.99.0 (rustfmt, clippy); edition 2024. Lints in `Cargo.toml`: `unsafe_code = "forbid"`, `clippy::unwrap_used = "deny"`, `clippy::expect_used = "deny"`. TLS uses rustls with ring (no OpenSSL); gzip uses flate2's Rust backend.
- **Build and check:**
  ```sh
  cargo build --release --locked
  cargo fmt --check
  cargo clippy --locked --all-targets -- -D warnings
  cargo test --locked
  cargo audit                       # cargo-audit 0.22.x; prior JSON in docs/evidence/*cargo-audit*.json
  ```
- **Ignored and explicit tests:** `cargo test advice:: --lib` (Copilot policy, broker, supervision). `actual_native_policy_canaries` (ignored; needs a pinned local Copilot native). `cargo test --test copilot_live -- --ignored` (explicitly authorized synthetic transfer). `tests/auth_helper_live.rs`, `tests/native_operations.rs`, `tests/manifest_edit_live.rs` and `tests/docker_desktop.rs` need a disposable cluster.
- **Demo mode (no cluster, no credentials):** `kborg demo`, or `KBORG_DEMO_SCALE=10000 kborg demo` for scale. Demo writes are simulated locally.
- **Harnesses (`scripts/`):**
  - Protocol and adversarial: `test-transport-faults.py` (isolated verified-TLS fixture: 401/403/410/429/503, slow body, CA rotation), `Test-Adversarial.ps1` (44 black-box CLI cases, PowerShell 7).
  - Soak: `test-soak-live.py` (owned namespace; bounded real-cluster soak, optional minikube pause fault).
  - Terminal: `test-terminal*.py`, `test-unicode-review-ui.py`.
  - Live UI flows: `test-live-*-ui.py`, `test-node-maintenance.py`.
  - Helm and supply chain: `test-helm-*.py`, `fixture-helm-*.py` (OCI/signature/Sigstore fixtures), `test-package-provenance.py`.
  - Performance: `bench-tui.py` (CI performance gate).
- **Evidence:** `docs/evidence/` holds JSON/text per batch: cargo-audit, clippy, tests, terminal matrices, transport faults, soak samples. The `soak-v0.4.0-desktop/` and `soak-v0.4.0-minikube/` directories are in progress and not yet committed at the time of writing. Acceptance reports: `MAC_V1_HARDENING_ACCEPTANCE.md`, `TRUST_MIGRATION_ACCEPTANCE.md`, `HELM_COMPLETION_ACCEPTANCE.md`.
- **Auditor environment tips:**
  - Use a disposable cluster (kind or minikube) and a dedicated least-privilege kubeconfig.
  - Set `KBORG_CONFIG_DIR` to an empty absolute directory.
  - Never point KBorg at production credentials.

## 10. Engagement contacts and logistics (owner to fill)

| Item | Value |
|---|---|
| Product owner / security contact | _TBD_ |
| Engineering point of contact | _TBD_ |
| Secure channel for findings | _TBD_. `SECURITY.md` states no public reporting channel exists yet |
| Source access (repository, commit to audit) | _TBD_ (suggest pinning a 0.4.0 tag) |
| Test cluster and identities provided | _TBD_ |
| Copilot test account / GHE.com / GHES tenant | _TBD_ |
| Engagement window and report deadline | _TBD_ |
| Severity scheme and retest policy | _TBD_ |
| Rules of engagement (no production clusters, no real Secrets) | _TBD_ |
