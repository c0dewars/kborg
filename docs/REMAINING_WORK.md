# KBorg remaining work and acceptance register

Reviewed: 2026-10-23. Current iteration: KBorg 0.3.0 Mac v1 workflow batch (reviewed YAML spec editing, node maintenance, explicit exports, eight TUI port forwards and a compact command menu), building on OCI tags/digests, OpenPGP and Sigstore chart verification, reviewed CRD storage migrations, GitHub Copilot advisory integration and a refined terminal UI, plus fixed-membership workload logs and Docker Desktop metrics. This register supersedes historical remaining-work statements. Implemented does not mean independently certified for production; the external gates below remain open. Exact command compatibility is recorded in [K9S_COMPATIBILITY.md](K9S_COMPATIBILITY.md); current batch evidence is in [MAC_V1_ACCEPTANCE.md](MAC_V1_ACCEPTANCE.md); the preceding trust iteration is recorded in [TRUST_MIGRATION_ACCEPTANCE.md](TRUST_MIGRATION_ACCEPTANCE.md). The previous [lifecycle report](HELM_LIFECYCLE_ACCEPTANCE.md) preserves its historical evidence. The earlier [Helm/Pod-log/metrics](HELM_LOGS_METRICS_ACCEPTANCE.md) and [navigation](K9S_ACCEPTANCE.md) reports retain their original evidence.

Status definitions: **Done** means an implemented workflow with retained evidence; **Partial** means useful behavior exists but the stated requirement is not fully met; **Missing** means no supported workflow; **External gate** means code alone cannot supply the necessary environment, identity, signing authority, or operational acceptance. A passing mock or configured CI job is not live acceptance. Automated independent agent reviews are not an external security audit.

## Current product inventory

| ID | Requirement | 0.3 status and exact gap | Completion evidence required |
|---|---|---|---|
| B01 | KBorg name, executable, logo, private repository | Done: KBorg name/executable/logo and private c0dewars/kborg repository; legacy settings supported | New binary/help/logo; old settings remain recoverable; private repository URL and origin verified |
| R01 | Direct Kubernetes TLS client, contexts/namespaces, 35 built-in kinds | Done for certificate/token authentication, one selected connection at a time; reviewed Unix helpers are covered by S03 | Docker Desktop and Kubernetes compatibility reports; exact newer-source evidence in K9S_ACCEPTANCE.md |
| R02 | Discovery/custom resources | Implemented bounded discovery with singular/plural/short-name and qualified resource navigation; built-in aliases remain after discovery, ambiguous aliases require qualification, and forbidden/incomplete refresh retains the prior catalog | Atomic catalog and response/request bounds, discovered alias ambiguity, denied refresh and built-in fallback tests; live CRD/removed-version compatibility remains environment-specific |
| R03 | Tables, search, details, events, offline demo | Implemented full-width tables, regex/inverse/fuzzy cached filters, actual server-side label selectors, and Pod/container/workload/Service/Node/Namespace drilldown; demo is isolated and covers all built-in kinds | Selector encoding and scope tests, retained selectors across LIST/WATCH/relist, UID-preserving cancellation/selection, redacted details, and current native UI evidence |
| R04 | Watch/reconnect/cancellation | Implemented bounded LIST/WATCH, 410 relist, authorization stop, polling/stale epochs and quiet renewal. HTTP errors can no longer masquerade as successful empty watches or supply a false Status code through malformed bodies | HTTP 401/403 clear cached data and stop even with empty/malformed/misleading bodies; HTTP 410 rebuilds, HTTP 503 retains the version and retries, with server text suppressed. Lifecycle and Docker Desktop checks are recorded in K9S_ACCEPTANCE.md; longer soak remains T05 |
| R05 | YAML inspection | Done: redacted JSON and readable quoted YAML; bounded depth/output, no generic YAML apply | Serialize the same redacted object into safe, bounded YAML; escaping/round-trip tests; no new raw-object export path |
| R06 | Logs and container selection | Implemented snapshots/previous logs, head/tail/lookback ranges and selected-container follow; default 1h, configurable 1..86400s, rolling 256 KiB/2000 lines. Ordinary-container Pod merges and workload/selector fixed memberships are supported: at most eight Pods, 16 containers per Pod and 32 total streams in one namespace. Actual selectors and CronJob→Job controller UIDs determine capture; no new rollout Pods are added implicitly | Four concurrent openings; attributed per-source errors, global head/tail, shared cache/deadline/64 MiB consumed-byte/1 MiB and 5000-lines per-second guards. Range/reconnect retains captured membership; periodic quiet-stream checks, pre/post-open and resume UID guards reject deletion/replacement. Each source permits at most five transport reconnects within the original shared session bounds and discloses overlap/gaps; clean EOF stops. Partial lists are rejected; init/ephemeral sources are excluded from merges. Current owned-fixture evidence belongs to HELM_LIFECYCLE_ACCEPTANCE.md. Prolonged adversity/soak and lossless-delivery claims remain unestablished |
| R07 | Metrics | Implemented Pod/Node CPU-memory tables, exact numeric sorting, bounded history and optional separate pod request/limit or node allocatable/capacity reads; missing/zero/invalid bases have no percentage. Docker Desktop now has an explicitly installed, Helm-owned Metrics Server 0.9.0, with verified aggregation and kubelet TLS. The application never installs a provider automatically | Exact quantities, rounding, UID/recreation/name-based comparisons, preservation of usage after a denied extra resource read, and actionable absent/forbidden/provider-unready guidance are covered. Positive live Pod/Node samples, comparisons and session history are covered by the current acceptance report. The public kubelet certificate pin and node address require renewal after replacement; provider serving TLS also has a finite lifetime |
| R09 | Helm releases | Implemented native Helm v3/v4 Secret/ConfigMap browsing and an explicitly reviewed lifecycle subset: install/upgrade/rollback/uninstall/test with trusted native Helm 4.3.1 on Unix, deterministic local/OCI packaged charts, separately opted-in hooks and schema workflows, and all three base flags `--write --allow-helm --allow-helm-write`. General chart/platform parity remains partial | OCI tags resolve once before review; digests/sizes seal content and optional `--helm-keyring` requires trusted OpenPGP provenance; `--helm-sigstore-policy` requires an exact pinned verifier, offline v0.3 bundle and explicit key/root or identity/issuer/root trust over chart archive bytes. Tests run exact stored test/test-success hooks and inspect completed results. CRD create, compatible schema upgrade and explicit deletion have separate reviews, bounded complete cluster-wide instance inventory and native UID/RV guards; normal release uninstall retains schemas. Foreign/GitOps-managed instances block destructive schema deletion. At most 128 affected ordinary resources, 32 hooks and 16 schemas; schema workflows also bound instance inventory. Explicit CRD storage migration supports identical schemas and owned namespaced instances; finalization requires official StorageVersionMigration controller proof. Docker Desktop without that API stages the switch/rewrites and retains historical storedVersions. Repository/version-range resolution, OCI manifest/referrer attestations, conversion webhooks, cluster-scoped custom instances, arbitrary dynamic charts, SQL and Windows lifecycle remain unsupported. See HELM_EXTENSIONS.md and current acceptance for limits and exact evidence |
| R08 | Simultaneous multi-context overview | Implemented CLI snapshots 1..100 with 4 concurrency/30s budget; continuous CLI/TUI pod watches for 1..4 explicit contexts, independent errors/caches and coalesced mailbox | Bounds, separate scope/cancellation and late-reply tests; quiet renewal fix and real watch revalidation. Independent multi-cluster/identity soak remains external acceptance |
| W01 | Scale/restart/delete | Implemented Deployment/StatefulSet scale (0..20), Deployment/StatefulSet/DaemonSet template restart, and explicitly allowlisted individual deletion families; server dry-run, UID/RV, expiring one-use approval and durable local intent remain mandatory | Fresh target and server admission/RBAC/conflict checks for each family; restart review explains controller strategy, including OnDelete behavior. New owned-fixture acceptance is recorded in K9S_ACCEPTANCE.md; broader admission and unknown-outcome gates remain |
| W02 | Controlled edit/patch and readable diff | Implemented prefilled ordinary-container image and optional four-field CPU/memory budget editing for Deployments, StatefulSets, DaemonSets and CronJobs, including a combined edit in one reviewed patch; existing-resource namespaced YAML spec editing now uses a private raw baseline, changed-path patch, server dry-run and full review. Arbitrary creation/apply, metadata/status/hidden-field edits and cluster-scoped YAML remain unsupported | Fresh raw spec, preserved unrelated fields/containers/resources, full diff, UID/RV, sealed approval, server dry-run/admission/GitOps checks and equivalent-quantity normalization. Focused-form evidence is in K9S_ACCEPTANCE.md; current YAML evidence is in MAC_V1_ACCEPTANCE.md |
| W03 | GitOps guidance | Implemented for image/resource-budget edits and Helm lifecycle: recognized Argo/Flux metadata blocks live changes with source-repository guidance; detection is advisory, not universal | Detect known management metadata conservatively; show advisory source hints; default refusal of live edit when managed policy requires GitOps; never execute metadata URLs |
| W04 | Node maintenance | Implemented reviewed cordon/uncordon and bounded PDB-respecting eviction drain with separate --allow-node-maintenance | Complete bounded inventory, status-only Node version refresh, guarded write/eviction identities, explicit retained/blocked Pods and partial outcomes; Desktop review/cancel and isolated minikube worker evidence in MAC_V1_ACCEPTANCE.md. No forced eviction or node shell |
| I07 | Explicit exports | Implemented selected redacted resource YAML, retained filtered logs with source identity, and selected Helm metadata/reference exports | Explicit destination/sensitivity preview, private exclusive file creation, 512 KiB/file and 100 retained files; local snapshot Ctrl-S retained. Logs/custom fields can contain confidential data |
| I01 | Structured CLI output | Done for bounded core reads/watch/metrics and Helm projections; documented command-specific output contract v1, exit statuses and unsupported JSON rejection; finite read sinks tolerate broken pipes | docs/CLI_OUTPUT.md and real-process closed-downstream test cover 29 read output paths; watch/follow stop immediately on pipe closure |
| I02 | Profiles/configuration/completions | Partial: strict schema1 read-only/operator constraints, separate Helm read/write capabilities, four shell completions and bounded data-only custom aliases implemented; k9s context-specific alias/hotkey/plugin files, organization-managed enforcement and automatic migrations are not supplied | Strict schemas and precedence; duplicate/unknown-key rejection; profiles cannot be loosened by later flags. Custom aliases are limited to 256 entries/64 KiB/2048 expanded bytes/16 steps, reject cycles and never invoke a shell |
| I03 | Approved editor/pager/tool handoff | Partial: bounded reviewed Unix native filters and built-in interactive redacted JSON/YAML pager implemented; external interactive editors/pagers and privileged tool containment remain unsupported | Native pager search/navigation/resize/finite lifetime/restoration; external filters retain absolute immutable argv, exact transfer review,15s/256 KiB and process-group cleanup. Foreground TTY plus bounded output/platform containment remain gates |
| I04 | Exec/attach | Implemented optional native stdin/TTY for ordinary containers, 1..3600s, Ctrl-] disconnect,resize, 16 MiB output/1MiB input; output-only 30s mode retained; remote full-screen applications unsupported | Real BusyBox input/resize/11s quiet pause/attach/cancel/expiry/nonprinting flood; terminal restoration and escape filters. Remote termination after disconnect and API name-recreation race remain limitations |
| I05 | Port forwarding | Implemented native TUI preparation/status/cancellation with explicit capability and write opt-in; one reviewed session on 127.0.0.1, 1..300 seconds and four active clients. Closing its view preserves the session; explicit stop, scope/context change or exit cancels it | Captured target/UID/session review, loopback/port/lifetime bounds, listener release and cancellation, per-client UID recheck and isolated local disconnect handling. Name-recreation and adversarial protocol gates remain |
| I06 | Generic command extensions | Implemented optional data-only local view registration with private schema 1, max 64, immutable SHA/metadata/argv, explicit flags and fresh input approval; no in-process plugin | Registration/update/schema/permissions/concurrency/literal-argv tests and real native filter smoke. OS-user filesystem/network permissions remain; independent containment required before production promotion |
| A01 | GitHub Copilot login and minimized local context | Implemented local status minimization, trusted login handoff and explicit pinned gh account reuse or private credential file for github.com advice | Selected account authority is bound and rechecked, ambient tokens excluded; Enterprise Cloud inference and GHES remain unsupported |
| A02 | In-app Copilot advisory diagnosis | Implemented optional macOS ARM64 native advisory workflow through `:copilot` and `copilot diagnose`, with exact payload/account approval, pinned signed native tool, fresh containment canaries and bounded read-only output | Current local containment and live-account evidence is recorded in TRUST_MIGRATION_ACCEPTANCE.md. Linux/Windows inference, Enterprise Cloud inference, broader account/subscription coverage and external security audit remain open. No automatic remediation |
| U01 | Themes and first-use startup | Done: 20 bundled themes including Flexoki Dark and Rose Pine; new-install Flexoki Dark default, saved preferences preserved, first-run picker, 2-second skippable/disableable animation | Current theme contrast tests and actual palette captures in TRUST_MIGRATION_ACCEPTANCE.md; historical startup reports |
| U02 | Custom theme workflow | Implemented CLI and TUI create-from-preview, explicit no-overwrite creation and backup-preserving replacement; atomic preferences and legacy fallback; Windows ACL acceptance remains | :themes then N captures actual session/preview colors; Enter creates/selects, Esc restores prior theme; pure projection and capture/cancellation tests plus native PTY evidence |
| U03 | Visible action menu and capability feedback | Implemented visible action picker with explicit OFF reasons, keyboard paths, captured-target edits and wrapped review; broader accessibility acceptance remains | Action reasons reflect logs, write and resource-family capabilities; hidden confirmation text blocks approval; resize, focus, late replies and UID-preserving selection tests retained |
| U04 | Terminal compatibility/accessibility | Partial: conservative ASCII data, UTF-8 box frames with ASCII fallback, truecolor, nearest 256-color and genuine ANSI16 degradation, monochrome, explicit selection markers, TERM=dumb rejection, keyboard-only --no-mouse; native macOS PTY/resize added | Depth/palette/SGR tests and actual PTY terminal restoration retained; Unicode widths/graphemes, host palette contrast, screen readers and broader real terminal matrix remain gates |
| U05 | Modern visual refinement | Implemented a compact clickable command menu, full-width resource table without a persistent sidebar, compact context/namespace/status identity, contextual hints and command navigation; buffered frames, sorting, palette-derived surfaces, stronger table headers, centered forms/themes, typed operation overviews, readable inspection, Helm review/progress and fixed-group log views | Current evidence belongs to MAC_V1_ACCEPTANCE.md; earlier trust/Helm/log/metrics captures and navigation/viewport/approval regressions remain in their historical reports. Accessibility, broader terminal matrix and long p95/p99/soak remain T05/T06 |
| U06 | k9s command/key familiarity | Partial: command grammar, aliases, completion/history, back/forward/last views, contextual keys, selectors/drilldown, bounded workload logs, reviewed Helm lifecycle subset, pulses/xray/workloads snapshots, declared-subject RBAC evidence, local manifests and saved snapshots are implemented | [K9S_COMPATIBILITY.md](K9S_COMPATIBILITY.md) is the exact mapping. Arbitrary Helm charts/SQL storage, benchmark/scanner integrations, RBAC subject-list/reference views, context-specific hotkeys/plugins, arbitrary manifest creation/apply, generic Secret reads, force kill and privileged node shell remain unsupported; YAML spec editing and node maintenance are now implemented within the scopes above. Familiar names are not a claim of full k9s parity |

## Security and operational acceptance

| ID | Gate | Status and work |
|---|---|---|
| S01 | Local file trust | Improved: same-directory synced atomic replacement; Unix0700/0600 tested. Windows ACL provisioning and handle-based race resistance remain gates |
| S02 | Audit integrity and crash reconciliation | Partial: bounded pending-intent/crash reporting includes Helm intent/outcome; no automatic retry or rollback. Partial/unknown operations require inspection. Protected external audit sink and operational reconciliation drills remain |
| S03 | Credential lifecycle and cloud helpers | Implemented opt-in reviewed Unix native ExecCredential v1/v1beta1 token or cert/key with bounded supervision, context/endpoint/CA/TLS/argv/env/SHA binding, expiry and 401/403 stop; legacy providers/interpreters/PATH chains rejected | Mock rotation/expiry/leak tests plus real Docker Desktop helper/403/reconnect/trust-tamper test; Windows helpers are unsupported; actual cloud identities, chained CLIs and handle-based trust remain acceptance gates |
| S04 | Server authorization/admission | External gate plus test fixtures: use least-privilege RBAC and admission policies independently of client controls; retain forbidden/dry-run/admission mutation cases for each new write family. |
| S05 | Transport edge cases | Partial: mandatory TLS, bounded bodies and no credential-bearing redirects/proxies. Watch error handling preserves actual HTTP status and suppresses untrusted bodies, including malformed authorization and expiry responses. Broader rotation, custom CA, redirect, slow response, 429 and expiry coverage remains; proxy support requires an explicit design. |
| S06 | Secret and terminal safety | Partial: known-field redaction and terminal sanitation remain mandatory. New local manifest previews validate bounded Kubernetes documents/List items, omit Secret documents/items and reject plain credential JSON; aliases and YAML parsing have depth/event/scalar/document/expansion bounds. Saved resource snapshots are explicitly requested and re-redacted when read. Broader sink canaries/parser fuzzing remain; logs and arbitrary CRDs can still contain confidential fields outside the known projection. |
| S07 | Copilot containment | Implemented and locally validated on macOS ARM64: actual OS denial canaries, signed pinned native, authenticated bounded TLS-host broker, explicit gh account binding and minimized-transfer approval. Other platforms fail closed. Broader native/platform/provider testing and independent security review remain gates; no autonomous remediation. |
| S08 | Dependency minimization | Partial: October 22 adds direct use of existing locked regex and bounded YAML parser packages for command filters/aliases/manifests. Review identified six advisories in the older locked TLS components; compatible updates to rustls 0.23.46, rustls-webpki 0.103.16 and webpki-root-certs 1.0.10 were applied. Final cargo-audit reports zero known vulnerabilities and no warnings, with no ignored advisories; exact lockfile/database revisions are retained in DEPENDENCY_AUDIT.md and K9S_ACCEPTANCE.md. Dependency and platform security review remain ongoing gates. |
| T01 | Supported Kubernetes versions | Partial evidence: historical 1.34.11/1.35.8 and current Desktop 1.36.1 plus isolated minikube 1.35.9. User target is 1.34+. Future minors require continuing compatibility runs; this is not a guarantee for untested releases. |
| T02 | OS packages | Partial for 0.3: all six native suites passed on d4d377; Windows line endings were fixed in e6de8e8. CI run 38007979257 was blocked before jobs by account billing/spending limits; owner declined paid Actions, so future runs are manual-only. Five earlier distributions and exact revisions are in PLATFORM_BUILDS_0.3.md; Windows ARM64 remains unverified. The October 10 priority iteration retained macOS ARM64/Linux ARM64 builds, 162 automated checks on each and PTY restoration evidence in PRIORITIES_ACCEPTANCE.md. Those counts and other older platform records do not validate October 22 source; the latest iteration's checks belong to MAC_V1_ACCEPTANCE.md. Helm lifecycle is Unix-only. Native builds do not prove every terminal combination. |
| T03 | WSL2/Docker Desktop | Done for native WSL app accessing existing Desktop Kubernetes. No second Docker engine is required. Keep explicit server/CA ownership checks and token cleanup in harnesses. |
| T04 | Scale | Partial: 1,001 objects and 100 aliases to one cluster. 100 independent clusters were not tested and exceed measured local resources. 0.3 added a real concurrent 100-alias overview: 100 successful rows, four connections, 0.131 seconds; see PERFORMANCE_0.3.md. This remains one cluster/identity. |
| T05 | Performance/fault/soak | Partial: historical 60-second WSL live/poll samples measured about 11.1 MiB RSS; 100 real context aliases completed with concurrency4. The October 10 priority report retains 14-state macOS/Linux ARM64 PTY samples with quiet idle output and terminal restoration; the latest source-specific checks belong to MAC_V1_ACCEPTANCE.md. An earlier parallel snapshot test encountered lock contention; the final serial suite passed. Parallel process-test isolation remains a harness follow-up. Sustained floods, request counts, reconnect storms, slow networks, longer soak, p95/p99 latency and hostile WebSocket allocations remain. |
| T06 | Real terminal matrix | External gate: Windows Terminal/PowerShell/cmd, Linux VT/bash, macOS Terminal/zsh, SSH, tmux/screen, narrow/resize/signal/handoff/process cleanup. Native CI and PTYs cover only parts. |
| D01 | Distribution trust | Partial: CycloneDX SBOM, dependency notices/coverage,checksums and unsigned source records implemented. Publisher signing/notarization and authenticated provenance remain external gates |
| D02 | Operations and maintenance | Implemented operational installation/update/rollback/uninstall/config/recovery guide; organizational pilot and disaster drills remain external gates |
| D03 | Documentation consistency | Current register, HELM_LIFECYCLE.md and K9S_COMPATIBILITY.md distinguish implemented commands/subsets from partial/unsupported k9s behavior; MAC_V1_ACCEPTANCE.md records the latest source-specific evidence. Historical reports remain dated evidence, not current-source certification |

## Safe implementation order

1. Complete rename/migration and reconcile current docs. Establish profile/capability configuration so later features share one enforcement boundary.
2. Add read-only workflows: redacted YAML, bounded log follow/container selection, multi-context snapshot overview with explicit limits, CLI completions and visible actions.
3. Implement individually scoped edits with immutable prepared requests and readable dry-run diffs; add GitOps restrictions and approval race tests. Editing rendered/redacted objects directly is unsafe because it may overwrite omitted data; prepare the operation from a fresh trusted object and only the explicitly approved fields.
4. Implement lifecycle-bound forwarding and exec/attach behind distinct capabilities, followed by trusted process handoff. Keep their authorization separate from read-only resource browsing. Reuse the Kubernetes client's reviewed protocol support rather than invoking kubectl as an unbounded substitute.
5. Harden storage, discovery, audit recovery and authentication; expand fault/load and platform evidence. Add automated Copilot only after its isolation and transfer-review contract can actually be enforced.
6. Refine visual layout/accessibility and rerun performance, then create renamed native packages. Publish a release only at its demonstrated readiness level; incomplete external gates remain visible.

The current Helm lifecycle workflow validates an executable named `helm`;
renamed executables and CSA-to-SSA field ownership migration remain compatibility
gaps. Native admission does not force field ownership. See
[HELM_LIFECYCLE.md](HELM_LIFECYCLE.md).

## Explicit exclusions

Privileged node shells, autonomous AI remediation, arbitrary in-process plugins, automatic cluster-admin setup, an embedded terminal multiplexer and a self-updating binary remain outside this production design. “All terminal programs” means documented standards-based CLI and approved process interoperability, not universal compatibility. Optional AI and generic extensions must not block a safe core release.

## Historical implementation notes

The following dated reports preserve their original scope and results. Their
remaining-work statements do not override the October 23 inventory above.

### Bounded discovery implementation (2026-10-10)

R02 implementation updated after the baseline review. `Gateway::discover` now delegates to a bounded preferred-version collector instead of accumulating `kube::Discovery`. Requests are sequential: `/api/v1`, `/apis`, then one preferred version for each validated group. Limits are 100 groups, 200 browsable kinds, 102 requests, 1 MiB per document, 16 MiB aggregate response bytes and a 15-second overall deadline. Limits apply before JSON decoding and before requesting further groups. Resource subresources and Secrets are excluded; group/version path segments are validated before URL construction.

The catalog commits atomically. Forbidden discovery, malformed responses, transport failures and exceeded limits produce explicit errors and retain the previous catalog; a partial result is never reported as a complete refresh. This permits known built-in operations to remain available while unsupported discovery is visible. Discovery does not try alternate versions after preferred-version failure. Decoder storage is bounded by each capped document; this is a finite memory bound rather than an exact RSS claim.

Validation: all five focused discovery tests passed on Windows (2026-10-10), covering fixed-path success, Secret/subresource exclusion, group/path injection and duplicates, resource/group caps, forbidden/malformed/oversized documents, aggregate/request limits and expired deadlines. A real read-only `kborg --context docker-desktop resources --json` completed successfully against the installed Docker Desktop cluster. Final combined Windows and WSL strict Clippy passed after integration; broader live custom-CRD compatibility remains an acceptance check. The historical discovery accumulation gap is addressed in code; these bounded checks do not replace long-duration resource measurements.

### Native macOS implementation and validation session (2026-10-10)

See [session acceptance](SESSION_ACCEPTANCE.md), [read-output contract](CLI_OUTPUT.md),
[local live evidence](evidence/local-macos-session.json) and
[native PTY evidence](evidence/terminal-macos-session.json). This session implements
core usability/history/tool-view gaps, revalidates four real integration tests on
Docker Desktop Kubernetes 1.36.1, and builds a native macOS ARM64 release binary.
The live evidence retains the exact source file hashes used by those tests. A
subsequent CLI-only color override fix is independently covered by the final PTY
check with inherited NO_COLOR and explicit --color=always.

Optional AI/extensions, continuous simultaneous multi-context watches, interactive
stdin/TTY, cloud credential helper trust, external audit, signing/notarization,
Windows tool containment, long soaks and the broader terminal/platform matrix are
still open. Neither the visual refinement nor local tests close external gates.

### Priority completion and UI refinement (2026-10-10)

[PRIORITIES_ACCEPTANCE.md](PRIORITIES_ACCEPTANCE.md) records the next local
iteration. Priorities were interactive sessions, longer rolling logs, reviewed
helpers and a visibly improved UI. Docker Desktop was the user-selected cloud
scope. Optional local extension views were added; automated Copilot was attempted
through SDK/OS preflight and remains blocked by explicit containment/credential/
publisher gates. Native platform and signing evidence must be read at the exact
source/host level; production gates are not waived by local feature completion.

## k9s command navigation and workflow iteration (2026-10-22)

This iteration replaces the persistent sidebar with a full-width table and
command navigation. Resource singular/plural names, built-in and discovered short
names, qualified names and bounded custom aliases reach the same catalog. Context
and namespace selection, completion/history, prior/next/last views, regex/inverse/
fuzzy search and actual label selectors support the documented k9s command grammar.
Selectors remain attached to both LIST and WATCH through reconnect and relist.
Enter drills into Pod containers or related Pods; Node relationships use a
server-side node field selector. Pulses, xray and mixed workload views are bounded
snapshots; unknown, unavailable and truncated reads remain explicit.

Contextual keys retain their purpose: Pod/container shell and attach, workload
scale/restart, log display controls and port-forward management do not invoke
unrelated actions. A prefilled controlled container editor covers Deployment,
StatefulSet, DaemonSet and CronJob templates; scale and restart have their smaller
explicit family allowlists. Every operation keeps its capability checks, captured
identity, fresh validation and approval. Forwarding is visible and cancellable in
the native UI, with finite lifetime and client bounds. Generic YAML apply, force
kill and node administration remain excluded.

Log ranges now distinguish first retained records, newest records and lookback
windows. Timestamp display and filters reproject the bounded cache without extra
API reads; paused readers retain a stable window while collection continues.
Identity-checked TUI reconnect retries transport failures only, at most five
attempts within the original session limits. A whole-second resume may repeat
records, rotation/restarts may omit them, and Kubernetes does not supply rotated
history through this endpoint. Clean EOF is final. The consumer budget remains
64 MiB for the session, while the transport budget applies to each response. These
bounds and warnings do not establish lossless log delivery or prolonged live
reconnect acceptance. This paragraph records the earlier navigation iteration. Ordinary-container merges were completed in the later Helm/log/metrics iteration below.

At the earlier navigation iteration, the Docker Desktop Metrics API probe returned HTTP 404. Resource browsing
continues with actionable guidance; missing metrics are not zero usage. No metrics
provider was installed automatically, and successful provider-backed history and
comparisons remain a live acceptance requirement. Docker Desktop is the selected
live integration scope; real cloud credentials and other platforms retain their
separate gates.

The exact supported mappings and material differences are in
[K9S_COMPATIBILITY.md](K9S_COMPATIBILITY.md). Native checks, owned Docker Desktop
fixtures, terminal captures, dependency audit and any measured results for this
source are recorded in [K9S_ACCEPTANCE.md](K9S_ACCEPTANCE.md). Earlier session and
priority reports remain historical evidence and are not substitutes for those
results. This earlier iteration left Helm/history and all-container logs open; the later
Helm/log/metrics iteration below supersedes those gaps. Benchmarks/scans, full RBAC
pseudo-resource/reference navigation, k9s plugin/hotkey import and external
production gates remain open.

## Historical Helm, Pod logs and Desktop metrics iteration (2026-10-22)

Native Helm reads inspect release metadata/history in Secret or ConfigMap storage,
with explicit selected-revision values and safe resource identities. `--allow-helm`
permits these sensitive storage reads separately; raw storage, chart files, notes
and rendered bodies are never returned. Known-key redaction is not a universal
classifier of custom values. General Secret navigation remains excluded.

Merged Pod logs retain each ordinary-container source label and timestamp, with
one finite session, consumer/rate limits and display cache. Missing previous logs
for one source do not hide another source's valid records. Cancellation drops all
readers. Initial and reconnect identity checks reject Pod replacement. Multi-Pod
workload merging, init/ephemeral sources and prolonged lossless reconnect claims
remain outside this implementation.

The user explicitly authorized installing a metrics provider and using minikube.
The owned Desktop Metrics Server release verifies both TLS hops, uses a pinned
upstream chart/image and a small documented chart extension for hostname routing
and public kubelet certificate trust. Its namespace/release remain installed for
use; test fixtures and credentials are cleaned up separately. Node/certificate
rotation requires the documented trust refresh. Current tests, screenshots,
independent reviews and remaining limitations are retained in
[HELM_LOGS_METRICS_ACCEPTANCE.md](HELM_LOGS_METRICS_ACCEPTANCE.md).

## Reviewed Helm lifecycle and workload logs iteration (2026-10-22)

The latest source adds reviewed install, upgrade, rollback and uninstall through
an explicitly trusted Helm 4.3.1 executable on Unix. Browsing still uses the
native storage reader. Lifecycle requires all three CLI flags and separately
permitted configuration capabilities. A full review binds the selected
namespace/release, executable/chart/values hashes, complete storage history,
current/desired resources and live UID/resourceVersion. It expires after
15 minutes; fresh final checks reject drift before execution.

This historical iteration was a local deterministic packaged-chart subset, with at most 128 affected
allowlisted namespaced resources. Hooks, CRDs, cluster/system/cross-namespace
targets, recognized GitOps resources and foreign adoption are rejected. SQL,
repository/OCI fetching, arbitrary chart behavior and Windows lifecycle remain
unsupported. Trusted Helm receives a private static credential and retains
OS-user permissions. Its own API requests lack KBorg's captured UID
preconditions, so a post-check replacement race remains. Timeout/cancellation
and partial writes require explicit reconciliation; no automatic retry or
rollback occurs.

Workload and explicit-selector logs capture complete ordinary-container
memberships in one namespace: eight Pods/32 total streams, at most 16 per Pod,
with four concurrent openings and shared session/cache/byte/rate/head/tail bounds.
Actual workload selectors and CronJob→Job controller UIDs determine membership.
New rollout Pods are not added silently; range changes and reconnects retain the
same capture. Periodic checks protect quiet streams, and deletion/replacement
stops the group. Attributed per-source failures do not hide valid peer records.
Init/ephemeral merges and lossless distributed-log guarantees are not supplied.

The [workflow guide](HELM_LIFECYCLE.md) defines controls and constraints;
[current acceptance](HELM_LIFECYCLE_ACCEPTANCE.md) retains exact source/platform,
live-fixture, review and terminal evidence. Earlier reports remain historical,
and independent security certification, broader platform/terminal acceptance and
organizational production gates are still open.

## Historical OCI, hooks and CRD continuation (2026-10-22)

That extension added digest-pinned OCI chart acquisition, explicit registry CA/auth files, all eight pre/post lifecycle hook events, and a separate create-only CRD bootstrap. Custom resource lifecycle requires a sealed, Established Namespaced schema. The UI keeps command navigation, adds `:helm-crds`/`c`, and displays source and capability state.

At that revision, remaining gaps included mutable tags/repository resolution, signature verification, automatic dependency fetching, credential-store/cloud registry helpers, arbitrary hook ownership and side effects, `helm test`, CRD schema upgrades/deletion/conversion webhooks, cluster-scoped custom resources, SQL storage and Windows lifecycle. See [extension guide](HELM_EXTENSIONS.md) and [current acceptance](HELM_COMPLETION_ACCEPTANCE.md).

## Tags, provenance, tests, schema operations and UI continuation (2026-10-22)

The current source adds explicit tag resolution, required OpenPGP provenance via a trusted public keyring, stored Helm test hooks, compatible schema upgrades and separately confirmed CRD deletion. The UI adds operation overviews, clearer surfaces and forms, and explicit test/schema commands. Exact final source, cluster acceptance, UI iterations and remaining boundaries are recorded in [current acceptance](HELM_COMPLETION_ACCEPTANCE.md).

## Mac v1 workflow continuation (2026-10-23)

The current TUI supports eight independent reviewed loopback port-forward sessions (four clients each, 1..300 seconds), per-session stop and complete scope/exit cancellation. Earlier singleton-forward limitations describe historical builds. YAML `e`, focused `:edit-container`, node `c/u/r`, local export previews and k10s-inspired top navigation are documented in [Mac v1 operations](MAC_V1_OPERATIONS.md).

[Mac v1 acceptance](MAC_V1_ACCEPTANCE.md) records this source revision's tests, code review and actual terminal captures. Prior Linux/platform and trust-feature reports remain evidence of their exact dated builds; they do not automatically certify this new source. [The v1 plan](MAC_V1_PLAN.md) prioritizes the next dashboard/grid/health/selection features and the outstanding packaging, soak, accessibility and independent-security gates.
