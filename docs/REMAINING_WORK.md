# KBorg remaining work and acceptance register

Reviewed: 2026-10-10. Current iteration: KBorg 0.3.0. This register supersedes historical remaining-work statements. Implemented does not mean independently certified for production; the external gates below remain open.

Status definitions: **Done** means an implemented workflow with retained evidence; **Partial** means useful behavior exists but the stated requirement is not fully met; **Missing** means no supported workflow; **External gate** means code alone cannot supply the necessary environment, identity, signing authority, or operational acceptance. A passing mock or configured CI job is not live acceptance. Automated independent agent reviews are not an external security audit.

## Current product inventory

| ID | Requirement | 0.3 status and exact gap | Completion evidence required |
|---|---|---|---|
| B01 | KBorg name, executable, logo, private repository | Done: KBorg name/executable/logo and private c0dewars/kborg repository; legacy settings supported | New binary/help/logo; old settings remain recoverable; private repository URL and origin verified |
| R01 | Direct Kubernetes TLS client, contexts/namespaces, 17 common kinds | Done for certificate/token authentication, one selected context at a time | Existing Docker Desktop and Kubernetes compatibility reports |
| R02 | Discovery/custom resources | Done: discovery limits requests, per-document and aggregate bytes before catalog commit; forbidden/incomplete refresh retains prior catalog | Bound discovery requests and accumulated bytes before catalog construction; partial/forbidden groups reported |
| R03 | Tables, search, details, events, offline demo | Done: bounded cache search, redacted JSON, events as a resource; demo is isolated | Existing user acceptance; retain permanent identity and stale/demo labels |
| R04 | Watch/reconnect/cancellation | Implemented bounded LIST/WATCH, 410 relist, authorization stop, polling/stale epochs; quiet normal renewals and successful no-event resumes now recover freshness without growing backoff | Existing lifecycle tests plus quiet/empty200 and control-error-before-fresh regression; final Docker Desktop revalidation; longer soak remains T05 |
| R05 | YAML inspection | Done: redacted JSON and readable quoted YAML; bounded depth/output, no generic YAML apply | Serialize the same redacted object into safe, bounded YAML; escaping/round-trip tests; no new raw-object export path |
| R06 | Logs and container selection | Implemented snapshots/previous logs and longer follow: default 1h, configurable 1..86400s; rolling 256 KiB/2000 lines, 64 MiB wire and1MiB/5000-lines per-second caps | Real 300 KiB line and 15-second quiet interval, composed truncation-marker test, cancellation/UTF-8/flood/cache bounds; prolonged floods and reconnection acceptance remain |
| R07 | Metrics | Implemented bounded history and optional separately read pod request/limit or node allocatable/capacity comparisons; missing/zero/invalid bases have no percentage | Exact integer math, >100%/rounding and UID/recreation/missing-UID qualification tests; extra-read403 retains usage; live provider history still needs an installed Metrics API |
| R08 | Simultaneous multi-context overview | Implemented CLI snapshots 1..100 with 4 concurrency/30s budget; continuous CLI/TUI pod watches for 1..4 explicit contexts, independent errors/caches and coalesced mailbox | Bounds, separate scope/cancellation and late-reply tests; quiet renewal fix and real watch revalidation. Independent multi-cluster/identity soak remains external acceptance |
| W01 | Scale/restart/delete | Done for limited typed families with dry-run, UID/RV, expiring one-use approval, durable local intent | Existing real RBAC, conflict and operation reports; broader admission/unknown-outcome gates remain |
| W02 | Controlled edit/patch and readable diff | Implemented allowlisted deployment image and four-field CPU/memory request/limit changes; generic YAML/arbitrary patch and other operation families remain excluded | Fresh raw spec, preserved other fields/containers/resources, full diff, UID/RV, sealed approval, dry-run/admission/GitOps checks; only equivalent reviewed quantity spellings normalized; real resource edit acceptance |
| W03 | GitOps guidance | Implemented for image and resource-budget edits: recognized Argo/Flux metadata blocks live edit with source-repository guidance; detection is advisory, not universal | Detect known management metadata conservatively; show advisory source hints; default refusal of live edit when managed policy requires GitOps; never execute metadata URLs |
| I01 | Structured CLI output | Done for bounded core reads/watch/metrics; documented command-specific output contract v1, exit statuses and unsupported JSON rejection; finite read sinks tolerate broken pipes | docs/CLI_OUTPUT.md and real-process closed-downstream test cover 16 read output paths; watch/follow stop immediately on pipe closure |
| I02 | Profiles/configuration/completions | Partial: strict schema1 JSON read-only/operator constraints and four shell completions implemented; organization-managed enforcement/automatic migrations not supplied | Strict versioned JSON; explicit precedence; duplicate/unknown-key rejection; managed capabilities cannot be enabled by later flags; bash/zsh/fish/PowerShell static completions |
| I03 | Approved editor/pager/tool handoff | Partial: bounded reviewed Unix native filters and built-in interactive redacted JSON/YAML pager implemented; external interactive editors/pagers and privileged tool containment remain unsupported | Native pager search/navigation/resize/finite lifetime/restoration; external filters retain absolute immutable argv, exact transfer review,15s/256 KiB and process-group cleanup. Foreground TTY plus bounded output/platform containment remain gates |
| I04 | Exec/attach | Implemented optional native stdin/TTY for ordinary containers, 1..3600s, Ctrl-] disconnect,resize, 16 MiB output/1MiB input; output-only 30s mode retained; remote full-screen applications unsupported | Real BusyBox input/resize/11s quiet pause/attach/cancel/expiry/nonprinting flood; terminal restoration and escape filters. Remote termination after disconnect and API name-recreation race remain limitations |
| I05 | Port forwarding | Implemented: explicit capability plus write opt-in, target review,127.0.0.1 only,1..300seconds,4active clients; name-recreation and adversarial protocol gates remain | Separate capability; loopback-only listener, explicit ports and finite lifetime, target/session binding, cleanup on exit/context change, bounded connection count |
| I06 | Generic command extensions | Implemented optional data-only local view registration with private schema 1, max 64, immutable SHA/metadata/argv, explicit flags and fresh input approval; no in-process plugin | Registration/update/schema/permissions/concurrency/literal-argv tests and real native filter smoke. OS-user filesystem/network permissions remain; independent containment required before production promotion |
| A01 | GitHub Copilot login and minimized local context | Partial: trusted CLI login handoff and reviewed local export exist | Real github.com and Enterprise Cloud *.ghe.com identity/subscription acceptance; no GHES support claim |
| A02 | In-app Copilot advisory diagnosis | Missing/gated optional phase: minimized local preview and fail-closed SDK/OS preflight implemented; all model requests remain disabled | Actual native SDK signature/hash and fresh fake-file/process/network canaries; hostname-bound inference egress, dedicated host-bound credentials, publisher-pin provenance and independent review remain unvalidated |
| U01 | Themes and first-use startup | Done: 18 bundled themes, Catppuccin Mocha default, first-run picker, 2-second skippable/disableable animation | Existing theme/startup reports |
| U02 | Custom theme workflow | Implemented CLI and TUI create-from-preview, explicit no-overwrite creation and backup-preserving replacement; atomic preferences and legacy fallback; Windows ACL acceptance remains | t then N captures actual session/preview colors; Enter creates/selects, Esc restores prior theme; pure projection and capture/cancellation tests plus native PTY evidence |
| U03 | Visible action menu and capability feedback | Implemented visible action picker with explicit OFF reasons, keyboard paths, captured-target edits and wrapped review; broader accessibility acceptance remains | Action reasons reflect logs, write and resource-family capabilities; hidden confirmation text blocks approval; resize, focus, late replies and UID-preserving selection tests retained |
| U04 | Terminal compatibility/accessibility | Partial: conservative ASCII, truecolor, nearest 256-color and genuine ANSI16 degradation, monochrome, explicit selection markers, TERM=dumb rejection, keyboard-only --no-mouse; native macOS PTY/resize added | Depth/palette/SGR tests and actual PTY terminal restoration retained; Unicode widths/graphemes, host palette contrast, screen readers and broader real terminal matrix remain gates |
| U05 | Modern visual refinement | Implemented polished masthead/health ribbon, grouped navigation, styled table, selected-object dock, centered forms/themes, readable overview, buffered frames without full-screen clearing, sorting/filtering and native workflows | Actual 14-state PTY captures and viewport 28×10..120×24 tests, full-review approval guard, live column separation and terminal restoration; retained CPU/RSS sample; long p95/p99/soak still T05 |

## Security and operational acceptance

| ID | Gate | Status and work |
|---|---|---|
| S01 | Local file trust | Improved: same-directory synced atomic replacement; Unix0700/0600 tested. Windows ACL provisioning and handle-based race resistance remain gates |
| S02 | Audit integrity and crash reconciliation | Partial: bounded pending-intent/crash reporting implemented; no automatic retry. Protected external audit sink and operational reconciliation drills remain |
| S03 | Credential lifecycle and cloud helpers | Implemented opt-in reviewed Unix native ExecCredential v1/v1beta1 token or cert/key with bounded supervision, context/endpoint/CA/TLS/argv/env/SHA binding, expiry and 401/403 stop; legacy providers/interpreters/PATH chains rejected | Mock rotation/expiry/leak tests plus real Docker Desktop helper/403/reconnect/trust-tamper test; Windows helpers are unsupported; actual cloud identities, chained CLIs and handle-based trust remain acceptance gates |
| S04 | Server authorization/admission | External gate plus test fixtures: use least-privilege RBAC and admission policies independently of client controls; retain forbidden/dry-run/admission mutation cases for each new write family. |
| S05 | Transport edge cases | Partial: mandatory TLS, bounded bodies, no credential-bearing redirects/proxies. Expand rotation, custom CA, redirect, slow response, 429, expiry and malicious response fixtures. Supporting a proxy requires explicit design rather than removing the block. |
| S06 | Secret and terminal safety | Partial: known-field redaction and terminal sanitation exist. Add canary checks for every new output/export/integration/AI sink and bounded parser fuzzing. Logs and arbitrary CRDs may contain confidential data outside known fields. |
| S07 | Copilot containment | External/platform gate: environment scrubbing and a separate process alone are insufficient. Validate filesystem/network/tool isolation per platform; disable automated advisor in strict profile where unavailable. Real login requires appropriate identity/subscription, never committed credentials. |
| S08 | Dependency minimization | Partial: reviewed narrow graph and advisory scan exist. Record reason/transitive impact for every new dependency; use std where suitable; do not hand-roll TLS/crypto/protocol parsers. Retain exact lockfile/source in new audit evidence. |
| T01 | Supported Kubernetes versions | Partial evidence: real 1.34.11, 1.35.8, Desktop 1.36.1. User target is 1.34+. Future minors require continuing compatibility runs; this is not a guarantee for untested releases. |
| T02 | OS packages | Partial for 0.3: all six native test suites passed on d4d377; four Linux/macOS packages passed. Windows notice checks caught checkout line-ending changes, fixed in e6de8e8. Final CI run 38007979257 is blocked before jobs by GitHub account billing/spending limits; Windows x64 local packaging passed; five verified distributions and exact source revisions are in PLATFORM_BUILDS_0.3.md. Windows ARM64 packaging remains unverified. Owner declined paid Actions; future runs are manual-only. The current priority iteration has native macOS ARM64 and Linux ARM64 builds, 162 automated checks on each and native PTY restoration evidence in PRIORITIES_ACCEPTANCE.md; Windows and other architectures retain only their earlier source evidence. Native builds do not prove every terminal combination. |
| T03 | WSL2/Docker Desktop | Done for native WSL app accessing existing Desktop Kubernetes. No second Docker engine is required. Keep explicit server/CA ownership checks and token cleanup in harnesses. |
| T04 | Scale | Partial: 1,001 objects and 100 aliases to one cluster. 100 independent clusters were not tested and exceed measured local resources. 0.3 added a real concurrent 100-alias overview: 100 successful rows, four connections, 0.131 seconds; see PERFORMANCE_0.3.md. This remains one cluster/identity. |
| T05 | Performance/fault/soak | Partial: 60-second native WSL live/poll samples measured about 11.1 MiB RSS; 100 real context aliases completed with concurrency4. The current priority iteration retains 14-state native PTY samples on macOS and Linux ARM64, with quiet idle output and terminal restoration. Sustained floods, request counts, reconnect storms, slow network, longer soak, p95/p99 latency and hostile WebSocket allocations remain. |
| T06 | Real terminal matrix | External gate: Windows Terminal/PowerShell/cmd, Linux VT/bash, macOS Terminal/zsh, SSH, tmux/screen, narrow/resize/signal/handoff/process cleanup. Native CI and PTYs cover only parts. |
| D01 | Distribution trust | Partial: CycloneDX SBOM, dependency notices/coverage,checksums and unsigned source records implemented. Publisher signing/notarization and authenticated provenance remain external gates |
| D02 | Operations and maintenance | Implemented operational installation/update/rollback/uninstall/config/recovery guide; organizational pilot and disaster drills remain external gates |
| D03 | Documentation consistency | Current guide/status/register consolidated for 0.3; historical reports preserved |

## Safe implementation order

1. Complete rename/migration and reconcile current docs. Establish profile/capability configuration so later features share one enforcement boundary.
2. Add read-only workflows: redacted YAML, bounded log follow/container selection, multi-context snapshot overview with explicit limits, CLI completions and visible actions.
3. Implement individually scoped edits with immutable prepared requests and readable dry-run diffs; add GitOps restrictions and approval race tests. Editing rendered/redacted objects directly is unsafe because it may overwrite omitted data; prepare the operation from a fresh trusted object and only the explicitly approved fields.
4. Implement lifecycle-bound forwarding and exec/attach behind distinct capabilities, followed by trusted process handoff. Keep their authorization separate from read-only resource browsing. Reuse the Kubernetes client's reviewed protocol support rather than invoking kubectl as an unbounded substitute.
5. Harden storage, discovery, audit recovery and authentication; expand fault/load and platform evidence. Add automated Copilot only after its isolation and transfer-review contract can actually be enforced.
6. Refine visual layout/accessibility and rerun performance, then create renamed native packages. Publish a release only at its demonstrated readiness level; incomplete external gates remain visible.

## Explicit exclusions

Privileged node shells, autonomous AI remediation, arbitrary in-process plugins, automatic cluster-admin setup, an embedded terminal multiplexer and a self-updating binary remain outside this production design. “All terminal programs” means documented standards-based CLI and approved process interoperability, not universal compatibility. Optional AI and generic extensions must not block a safe core release.

## Current iteration: bounded discovery implementation

R02 implementation updated after the baseline review. `Gateway::discover` now delegates to a bounded preferred-version collector instead of accumulating `kube::Discovery`. Requests are sequential: `/api/v1`, `/apis`, then one preferred version for each validated group. Limits are 100 groups, 200 browsable kinds, 102 requests, 1 MiB per document, 16 MiB aggregate response bytes and a 15-second overall deadline. Limits apply before JSON decoding and before requesting further groups. Resource subresources and Secrets are excluded; group/version path segments are validated before URL construction.

The catalog commits atomically. Forbidden discovery, malformed responses, transport failures and exceeded limits produce explicit errors and retain the previous catalog; a partial result is never reported as a complete refresh. This permits known built-in operations to remain available while unsupported discovery is visible. Discovery does not try alternate versions after preferred-version failure. Decoder storage is bounded by each capped document; this is a finite memory bound rather than an exact RSS claim.

Validation: all five focused discovery tests passed on Windows (2026-10-10), covering fixed-path success, Secret/subresource exclusion, group/path injection and duplicates, resource/group caps, forbidden/malformed/oversized documents, aggregate/request limits and expired deadlines. A real read-only `kborg --context docker-desktop resources --json` completed successfully against the installed Docker Desktop cluster. Final combined Windows and WSL strict Clippy passed after integration; broader live custom-CRD compatibility remains an acceptance check. The historical discovery accumulation gap is addressed in code; these bounded checks do not replace long-duration resource measurements.

## Native macOS implementation and validation session (2026-10-10)

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

## Priority completion and UI refinement (2026-10-10)

[PRIORITIES_ACCEPTANCE.md](PRIORITIES_ACCEPTANCE.md) records the next local
iteration. Priorities were interactive sessions, longer rolling logs, reviewed
helpers and a visibly improved UI. Docker Desktop was the user-selected cloud
scope. Optional local extension views were added; automated Copilot was attempted
through SDK/OS preflight and remains blocked by explicit containment/credential/
publisher gates. Native platform and signing evidence must be read at the exact
source/host level; production gates are not waived by local feature completion.
