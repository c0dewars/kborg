# KBorg installation, recovery and configuration

## Install and update

Use an archive matching your OS and CPU from the private release. Obtain its expected digest through a trusted channel and run `scripts/Verify-Package.ps1 -Archive FILE -ExpectedSha256 HASH` before extraction. A checksum delivered beside an archive detects corruption but does not authenticate a publisher. Current packages are unsigned; production distribution requires your organization's signing and verification process.

Keep binaries in versioned directories and place only the selected directory on PATH. Run `kborg --version`, `kborg demo`, then a least-privilege `list pods --json` with an explicit kubeconfig/context/namespace before enabling operations. No cluster component or second Docker daemon is required. Docker Desktop's Kubernetes endpoint is accessible to the native Windows and WSL2 clients with verified CA material.

For rollback, stop KBorg and select the previous versioned binary. This does not undo Kubernetes operations. Keep a protected backup of user settings before upgrades. Config schema 1 rejects unknown versions; migration is deliberate and never executes a config file. Uninstall by removing the chosen binary directory; retain or explicitly remove your settings/audit spool according to retention policy. No automatic cleanup of kubeconfig or cluster objects occurs.

## Configuration

`kborg config init settings.json` creates a new strict JSON file. Pass it explicitly with `--config`; no project-local executable configuration is auto-loaded. `config validate` validates without connecting. CLI context/namespace/theme values override file defaults. Files can constrain capabilities but never enable writes or log/subresource access without their CLI flags. Profile `read-only` refuses `--write`; profile `operator` still requires `--write`, separate capability opt-ins, and target confirmation. These are local client controls, not a substitute for RBAC/admission or a policy against a user running another binary.

Supported fields: `schema_version` (1), `profile` (`read-only` or `operator`), optional `context`, `namespace`, `theme`, and booleans `no_splash`, `poll`, `allow_logs`, `allow_helm`, `allow_helm_write`, `allow_exec`, `allow_attach`, `allow_forward`, `allow_tools`, `allow_extensions`, `allow_auth_helper`. Unknown/duplicate fields fail. Default generated config prohibits sensitive capabilities. Config never stores credentials. Helm lifecycle requires an `operator` profile permitting both Helm capabilities when a config file is supplied, plus all three explicit CLI flags.

`kborg completions bash|zsh|fish|powershell` emits a static script; review and load it through your shell's normal configuration. It makes no cluster calls.

## Unknown operations and audit

Run `kborg audit pending --json` after a crash or uncertain response. Missing, incomplete and failed outcomes remain uncertain. Do not automatically replay a write. Use the recorded context, namespace, UID and operation to inspect current API state and correlate with API-server audit. A later state observation cannot prove exactly which request took effect. Local records are operator-mutable and supplementary; organizations requiring protected audit must supply a protected server-side sink and test outages before promoting writes.

## Support and production rollout

Kubernetes 1.34 is the minimum binding. Previously tested minors are 1.34, 1.35 and 1.36; future minors require compatibility runs. Certificate/token authentication is supported. ExecCredential helpers require a reviewed immutable native executable and host/context/CA-bound policy; legacy providers remain blocked. Real provider identity rotation, proxy support, enterprise account policies and terminal combinations need deployment-specific acceptance.

Start with a dedicated read-only identity and narrow namespace, then a small staging pilot. Grant only reviewed operation verbs; enforce allowed images/fields and Pod Security on the API server. Test dry-run changes, stale resources, revocation, endpoint outage and audit failures. Promote only after the current register's required gates have evidence. Report security issues privately through the process in SECURITY.md; do not attach tokens, kubeconfig, Secrets or production logs.

## Local builds without paid CI

GitHub Actions is manual-only at the owner's request; pushes and pull requests do not start hosted jobs. Run `./scripts/Check.ps1` and `cargo build --release --locked` locally, then `./scripts/Package-Release.ps1 -Target <installed-native-target> -SourceCommit <exact-clean-source-commit>` on the target operating system. The packaging script runs the binary, so use a native machine/VM with that architecture; a cross-compiled executable is not equivalent to native acceptance. Windows and WSL cover Windows x64 and Linux x64 here. Verified earlier Linux ARM64 and both macOS packages remain available with their original source records. New native macOS/ARM builds need access to those machines or a separately chosen free runner; no paid service is required for local development.

## Reviewed external views

`--allow-tools view RESOURCE NAME --tool ABSOLUTE_PATH [--tool-arg ARG]` previews
the exact redacted JSON input and immutable argument vector before `RUN TOOL`
approval. Use `--yaml` for quoted YAML. Every literal argument uses its own
`--tool-arg`, including arguments that resemble KBorg capability flags. The
executable must be a regular native binary with no symlink path and no group/world
write permissions; its contents are rechecked after approval. A displayed change
identifier detects replacement and does not authenticate a publisher.

The Unix supervisor supplies a private working directory/HOME and clears the
inherited environment, bounds runtime to 15 seconds and combined stdout/stderr to
256 KiB, and cleans up the process group. Captured output is terminal-sanitized.
The trusted program retains OS-user filesystem/network access, and hostile tools
can escape a process group; this is supervision, not a sandbox. Executable path
races, independently constrained privileged credentials, Windows containment and
interactive pager/editor terminal handoff remain gates. Profile files constrain
`allow_tools`; neither a file nor tool arguments can opt in to the capability.

## Native local acceptance

On macOS/Linux, run `python3 scripts/test-local.py --expected-server ENDPOINT`
with the exact inspected Docker Desktop HTTPS endpoint. Rust, kubectl and Docker
Desktop must already be installed. The stdlib-only harness verifies the selected
context and embedded CA, digest-pins BusyBox, uses unique restricted namespaces
and scoped ten-minute service-account tokens, and checks ownership before cleanup.
It runs six live integration tests covering real RBAC denials, resource
version conflicts, watch revocation, loopback forwarding, long logs, interactive
sessions and reviewed authentication helpers. It does not reset
Docker, alter the current context, or reuse preexisting workloads.

`scripts/test-terminal.py --binary target/release/kborg` exercises demo keyboard
navigation and terminal restoration in a native PTY. `--screenshots DIRECTORY`
optionally captures PNGs using Pillow. PTY evidence does not certify every terminal
emulator, screen reader or OS.

## Reviewed authentication helpers

`auth-helper review --executable ABS [--env NAME=VALUE] --output NEW_POLICY`
reviews the selected kubeconfig's exact ExecCredential argv and environment,
selected user/context, endpoint, CA and native executable SHA-256. Type
`TRUST AUTH HELPER` to create a private policy without executing the helper.
Enable with `--allow-auth-helper --auth-helper-policy FILE`; profile files can
forbid this capability. Approved provider configuration paths/profile values may
be supplied explicitly. PATH, generic interpreters and raw secret environment
variables are rejected, so chained provider CLIs may require a native adapter.

Runtime supervision uses 15 seconds/256 KiB, no shell, private working directory
and cleared inherited environment. The helper remains a trusted OS-user program,
not a sandbox. Token or certificate/key v1/v1beta1 replies require a future
expiration within 24 hours. Credentials are transient; expiry or 401/403 stops further
requests until explicit reconnect. Any endpoint/CA/TLS-name/context/argv/env or
executable change invalidates the policy. Relative certificate/token/CA file
paths are resolved against the explicit kubeconfig file. Real cloud identities,
provider-specific chains and stronger handle-based path trust remain gates.

## Interactive local workflows

CLI `--interactive exec/attach` and TUI `E` acquire one terminal owner after exact
review. `Ctrl-]` disconnects; `Ctrl-C` goes to the remote process. Resize is sent
through the native Kubernetes protocol. Finite time/input/output budgets and
terminal restoration apply. Remote full-screen applications and privileged node
shells are unsupported. Interactive APIs have the same residual subresource
name-recreation race as noninteractive operations.

`V`/`set-resources` reviews four CPU/memory request/limit values for one ordinary
deployment container. Requests must be positive and not exceed limits; limits
are capped at 64 cores/128 GiB. Other resource keys, claims, containers and workload
fields are preserved. Only equivalent spellings of these four quantities are
normalized during dry-run comparison; all other spec/metadata effects remain
strict. Generic YAML edits, resource removal and custom-resource writes remain
excluded. Every approval displays the complete bounded review; oversized/control
text is refused instead of clipped.

Long follow keeps a rolling display and marks discarded/truncated lines. End
resumes tail; Home/Page keys inspect older retained lines. Quiet streams remain
open within the configured duration. Transport distinguishes 64 MiB follow streams
from 8 MiB ordinary documents, with 15-second headers and 10-second ordinary-body idle
budgets. Quiet eight-second watch intervals renew without permanently losing
fresh counts; transient faults still reconnect and authorization denial stops.

`o`/`overview --follow` watches at most four explicitly named contexts. Each owns
its credentials/cache/error and one cancellable watch; summaries coalesce in one
mailbox and close/scope changes abort all feeds. No object data or credentials are
shared. Broader independent-cluster soak tests remain open.

## Helm lifecycle, workload logs and Desktop metrics

Launch `./scripts/start-local.sh --context docker-desktop --allow-helm --allow-logs --no-splash`.
`:helm` lists Helm release storage in the current namespace; Enter/r opens history,
and Enter on a revision shows redacted stored overrides. `d` shows metadata,
`m` lists rendered resource identities, and `y` shows the safe YAML projection.
`--helm-storage secret|configmap` disambiguates duplicate driver names. This reads
Helm v3/v4 Kubernetes storage directly; no Helm executable is needed for browsing.
`--allow-helm` is separate from logs and general resource access. Kubernetes RBAC
must allow the requested Secret/ConfigMap reads; Kubernetes cannot enforce a
label selector as a Secret permission boundary. Use a dedicated namespace and
least-privilege identity. Generic Secret browsing remains disabled. Known-key
redaction cannot identify every confidential custom chart value.

Helm install, upgrade, rollback and uninstall are separately available on Unix
with `--write --allow-helm --allow-helm-write --helm-tool ABSOLUTE_PATH`.
The trusted executable must be exactly Helm 4.3.1. Install/upgrade accept local
deterministic `.tgz` charts with bundled dependencies and an optional explicit
values file. They do not fetch repositories/OCI or create namespaces.
`:helm` uses `i` to install, `u` to upgrade and Ctrl-D to uninstall; history `r`
prepares rollback to the selected revision.

Review the complete context/endpoint, exact arguments and hashes, current and
desired resources, and release history. Type the exact `namespace/release`.
Reviews expire after 15 minutes and final checks reject changed input, history,
UID or resourceVersion. Lifecycle commands require a terminal and reject JSON or
piped approval. Hooks, CRDs, cluster/system/cross-namespace resources, recognized
GitOps targets and foreign-resource adoption are refused. The allowlisted chart
subset, private static credential transfer and remaining post-check race are
documented in [HELM_LIFECYCLE.md](HELM_LIFECYCLE.md). Trusted Helm retains OS-user
permissions; it is not sandboxed. Timeouts, cancellation or partial failure
require inspecting release state and `audit pending` before another action;
there is no automatic retry or rollback. SQL storage and Windows lifecycle
supervision remain unsupported.

`logs POD --all-containers` reads ordinary containers with namespace/Pod/container
labels. Add `--follow --log-seconds 60` for a finite live merge. Init and ephemeral
containers are excluded; at most 16 ordinary containers and four simultaneous
connection openings are supported. Follow is arrival-ordered; snapshots sort by
Kubernetes timestamp and apply one shared head/tail limit. Source failures remain
visible while other sources continue. Every session shares deadline, rate and
64 MiB consumed-byte limits; each HTTP response also has its own transport cap.
The visible cache remains 256 KiB/2,000 lines. This does not guarantee lossless
ordering across containers, rotation, restarts or reconnects.

`logs deploy/NAME`, `logs service/NAME` and `logs --selector LABELS` merge a
captured group in one explicit namespace. Supported workload families also
include StatefulSet, DaemonSet, ReplicaSet, ReplicationController, Job and
CronJob; CronJobs follow exact controller UIDs through Jobs. Workload `l`/`p`
and `:logs LABELS` provide the same UI workflow. Capture rejects partial or
oversized membership: at most eight Pods, 16 ordinary containers per Pod and
32 streams in total. An explicit container argument is reserved for single-Pod
logs. Source membership stays fixed across range changes and reconnects;
reopen explicitly to include new rollout Pods. Pod/workload deletion,
replacement or authorization loss stops the group. Periodic checks cover quiet
streams, and transient verification failures are visible and bounded. The
group shares the existing time, byte, rate, cache and global head/tail limits.

Desktop metrics installation and certificate renewal are documented in
[DESKTOP_METRICS.md](DESKTOP_METRICS.md). KBorg itself never installs a provider.
The explicit setup helper installs one Helm-owned Metrics Server release; the
ordinary application only reads the Metrics API. Current-session acceptance and
remaining gaps are in [HELM_LIFECYCLE_ACCEPTANCE.md](HELM_LIFECYCLE_ACCEPTANCE.md).
The earlier [Helm/Pod-log/metrics report](HELM_LOGS_METRICS_ACCEPTANCE.md)
preserves its original source and screenshots as historical evidence.
