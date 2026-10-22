# Helm lifecycle and workload-log acceptance

Date: 2026-10-22. Local continuation of `ca15b02`; this report covers the
`codex/helm-lifecycle-workload-logs` source. Exact file and executable hashes are
retained in [validation evidence](evidence/lifecycle-validation-summary.json).
No release was published and no changes were pushed.

## Delivered behavior

- CLI and terminal UI support reviewed Helm install, upgrade, explicit-revision
  rollback and uninstall. All three capabilities, a trusted Helm **4.3.1** native
  executable and terminal confirmation are required. Install/upgrade accept
  trusted local packaged charts and optional local values.
- `:helm` keeps the full-width command-driven layout. `i` installs, `u` upgrades,
  Ctrl-D uninstalls, and `r` in revision history rolls back. Forms preserve the
  active Helm namespace/storage. Reviews require every wrapped line to be painted
  before exact `namespace/release` confirmation; narrow/resize views retain the
  form and reset review coverage when necessary.
- Lifecycle actions prepare private static credentials and input snapshots,
  compare rendered plans, validate server dry-runs and record durable audit
  intent/outcome. Source/tool hashes, release history and affected UID/resource
  versions are checked again before submission. Cancellation is supervised;
  uncertain results require reconciliation. Uninstall verifies disappearance of
  reviewed resource UIDs before reporting success, except explicit keep policies.
- Workload/Service `l`/`p`, `:logs LABELS`, CLI workload targets and explicit
  selectors capture up to eight Pods and 32 ordinary-container sources. Source
  labels, complete fixed membership, shared budgets, range changes and identity
  checks carry through the log view and JSON output. CronJob capture follows
  controller UIDs through Jobs. New rollout Pods require reopening the view.

Usage, constraints and exact CLI examples are in
[HELM_LIFECYCLE.md](HELM_LIFECYCLE.md). Command compatibility remains explicitly
bounded in [K9S_COMPATIBILITY.md](K9S_COMPATIBILITY.md).

## Verification

The final source passed the following checks. Reports retain exact source and
binary hashes, fixture ownership and cleanup outcomes.

| Check | Result | Evidence |
|---|---|---|
| macOS ARM64 | 272 automated tests, strict Clippy, formatting and release build passed | [Build](evidence/lifecycle-build-macos.json) |
| Linux ARM64, pinned Rust 1.99.0 container | 272 automated tests, strict Clippy, formatting and release build passed | [Build](evidence/lifecycle-linux-arm64.json) |
| Native terminal sessions | 43 macOS and 43 Linux states; exact terminal restoration; offline/SIMULATED lifecycle | [macOS](evidence/lifecycle-terminal-macos.json), [Linux](evidence/lifecycle-terminal-linux-arm64.json) |
| Helm CLI, Desktop 1.36.1 and minikube 1.35.9 | Install → upgrade → rollback → uninstall with both Secret and ConfigMap storage; expected revisions 1/2/3 | [Two clusters](evidence/lifecycle-local.json) |
| Helm negative checks, both clusters/drivers | Wrong confirmation, changed resource/history/chart and dynamic-template rejection; Secret canary absent; inherited Helm endpoint overrides ignored | [Lifecycle](evidence/lifecycle-local.json) |
| Workload logs, both clusters | Deployment, Service, explicit selector and CronJob capture; two Pods/four sources; global tail; namespace isolation; third Pod exists during fixed follow; captured-Pod deletion stops group | [Logs](evidence/workload-logs-local.json) |
| Final real Docker Desktop UI | All four Helm actions, canceled review, full painted confirmation, two-Pod logs and range reload; 20 captured panels; terminal restored | [Live UI](evidence/lifecycle-live-ui.json) |
| Existing Desktop regression suites | Six live auth/helper, watch, metrics-denial and native-operation tests passed; three suites cleaned up | [Regressions](evidence/lifecycle-regression-local-macos.json) |
| Final Desktop metrics reads | Live Pod and Node samples available from the retained verified-TLS provider | [Metrics](evidence/lifecycle-desktop-metrics.json) |
| Dependency audit | 203 locked packages; zero known vulnerabilities or warnings; no ignored advisories | [Audit](evidence/lifecycle-cargo-audit.json) |

The automated test totals exclude 12 ignored entries: six subprocess fixture
entry points are exercised by parent tests, and six cluster-gated integration
entries are separately exercised by the Desktop harness. Linux cases ran with
`RUST_TEST_THREADS=1` after a parallel run encountered transient snapshot-lock
contention during unrelated subprocess spawning; concurrency inside individual
async tests remains exercised. The Linux build used two build jobs, a 5 GiB
memory limit and a dedicated disk-backed Docker volume. The container, volume,
owned namespaces and temporary credentials were removed. The isolated minikube
profile was [deleted](evidence/lifecycle-minikube-cleanup.json); Docker Desktop's
current context and the existing metrics provider were preserved.

The two-cluster CLI/log and existing-regression runs preceded the final status
footer/group-header polish and test-only debug-profile adjustment. Every backend
source file and release-build setting is unchanged from those runs. The exact
[UI delta](evidence/lifecycle-ui-progress.diff) and
[test-profile delta](evidence/lifecycle-test-profile.diff) are retained and
hash-checked in the [validation summary](evidence/lifecycle-validation-summary.json).
Both native builds and the final real Docker Desktop UI use the final source.
This is explicit unchanged-backend evidence, not a claim that every earlier run
used the final executable.

Visually reviewed panels include the
[real Helm review](evidence/ui-lifecycle-live/live-ui-upgrade-review.png),
[successful uninstall](evidence/ui-lifecycle-live/live-ui-uninstall-result.png),
[two-Pod range view](evidence/ui-lifecycle-live/live-ui-workload-log-range.png) and
[narrow review](evidence/ui-lifecycle-terminal/helm-review-narrow.png).

## Product and code review

Three agents and the integrating agent reviewed the backend, CLI/config/audit,
workload streams, UI state/keyboard handling, acceptance harnesses and docs.
Concrete findings corrected during this iteration include:

- Long workload identities pushed the active log range off-screen; group headers
  now place captured source counts and log controls before descriptive identity.
- GNU test debug symbols pushed the self-spawn test executable beyond the existing
  128 MiB helper bound. Test-only debug symbols are disabled; production limits
  and release compilation settings are unchanged.
- The previous operation's success footer persisted during a new review/execution;
  each Helm draft, preparation, review and execution now reports its current state.
  Live UI assertions also require the current outcome panel title.
- Install prefill used the main view namespace after `:helm OTHER_NAMESPACE`;
  it now uses the visible Helm table namespace and selected storage driver.
- Revision arithmetic could overflow; lifecycle rejects an exhausted counter
  while still permitting uninstall.
- Chart replacement by a FIFO could block synchronous validation; bounded
  regular-file checks, nonblocking open and opened-file identity checks reject it.
- Helm success alone could precede resource deletion; uninstall now waits within
  its operation budget and reports pending/replaced resources as uncertain.
- Aggregate live-resource review bytes and final validation duration needed
  explicit caps; reviews now enforce both and remain cancelable.
- Template nondeterminism needed broader checks: time aliases, credential/random
  functions, unordered map iteration and dynamic/pointer printf formats are
  rejected conservatively.
- Harness error cleanup now requests product cancellation before bounded
  termination. The live UI harness canonicalizes macOS temporary paths; an
  unresolved symlink path had correctly failed the application's file-trust
  checks before startup. Successful CLI/TUI paths verify terminal restoration.
  Active-operation cancellation is covered by supervisor tests, not a live
  canceled production write.

The first final Helm run uncovered a fixture restoration issue: kubectl restored
an externally changed ConfigMap value but retained its field ownership. Native
SSA preflight correctly rejected the next upgrade. Even an update using the same manager name has distinct ownership from SSA. The
final fixture tests the successful lifecycle before deliberately transferring
field ownership for the stale-review check; no reset or forced takeover is used.
No product conflict check was weakened. A
preliminary log test also corrected its parser to distinguish control notices
from attributed application records; final checks validate captured Pod and
container membership.

## Practical limits

This is acceptance of a restricted workflow, not full Helm/k9s parity or a
production certification. Lifecycle supports macOS/Linux x86-64/ARM64 with exact
Helm 4.3.1 named `helm`, Secret/ConfigMap storage and bounded deterministic
namespaced charts. Renamed Helm executables can use a different field manager
and are outside this validated workflow.
Windows lifecycle, SQL storage, repository/OCI fetching, hooks, CRDs,
cluster-scoped resources, plugin/post-renderer execution and foreign adoption
remain unsupported. Whole-template scans can reject harmless comments/names.

Native SSA admission checks may reject another manager's field ownership even
when a Helm merge would overwrite it. Helm's own API writes lack captured UID
preconditions, so final-check races and partial writes remain possible. Trusted
Helm retains OS-user privileges; there is no OS memory/CPU sandbox. Logs retain
fixed membership and bounded buffers, not lossless delivery or automatic rollout
admission. Linux build/PTY evidence does not establish every real terminal, and
local clusters do not establish cloud identities, long soak/scale or production
operational readiness. Remaining gates are listed in
[REMAINING_WORK.md](REMAINING_WORK.md).
