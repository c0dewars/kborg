# KBorg remaining work and acceptance register

Reviewed: 2026-10-24. Current iteration: KBorg 0.5.0 (unreleased) on top of the 0.4.0 release. 0.5.0 closes the remaining k9s workflow gaps behind separate opt-ins: reviewed server-side apply, node shell, file and directory copy, `:lint`, a port-forward benchmark, extensions in the TUI with a k9s `plugins.yaml` importer, and k9s-format hotkeys; dashboard cards name the busiest Nodes; configuration profiles now gate every capability; and audit-pack findings were hardened. 0.4.0 added the 1–9 cluster dashboard, Secret reveal, force kill, external editor, multi-file KUBECONFIG, GHE/GHES Copilot and the performance work. This register supersedes historical remaining-work statements. Implemented does not mean independently certified for production; the external gates below remain open. Exact command compatibility is recorded in [K9S_COMPATIBILITY.md](K9S_COMPATIBILITY.md); the external review scope is in [SECURITY_AUDIT_PACK.md](SECURITY_AUDIT_PACK.md); dated acceptance reports ([MAC_V1_HARDENING_ACCEPTANCE.md](MAC_V1_HARDENING_ACCEPTANCE.md), [DASHBOARD_ACCEPTANCE.md](DASHBOARD_ACCEPTANCE.md), [MAC_V1_ACCEPTANCE.md](MAC_V1_ACCEPTANCE.md) and earlier) keep the evidence of their exact builds.

Status definitions: **Done** means an implemented workflow with retained evidence; **Partial** means useful behavior exists but the stated requirement is not fully met; **Missing** means no supported workflow; **External gate** means code alone cannot supply the necessary environment, identity, signing authority, or operational acceptance. A passing mock or configured CI job is not live acceptance. Automated independent agent reviews are not an external security audit.

## Current product inventory

| ID | Requirement | Status and exact gap | Completion evidence required |
|---|---|---|---|
| B01 | KBorg name, executable, logo, repository | Done: KBorg name/executable/logo and the c0dewars/kborg repository (being made public for Homebrew); legacy settings supported | New binary/help/logo; old settings remain recoverable; repository URL and origin verified |
| R01 | Direct Kubernetes TLS client, contexts/namespaces, 35 built-in kinds | Done for certificate/token authentication, one selected connection at a time; reviewed Unix helpers are covered by S03 | Docker Desktop and Kubernetes compatibility reports; exact newer-source evidence in K9S_ACCEPTANCE.md |
| R02 | Discovery/custom resources | Implemented bounded discovery with singular/plural/short-name and qualified resource navigation; built-in aliases remain after discovery, ambiguous aliases require qualification, and forbidden/incomplete refresh retains the prior catalog | Atomic catalog and response/request bounds, discovered alias ambiguity, denied refresh and built-in fallback tests; live CRD/removed-version compatibility remains environment-specific |
| R03 | Tables, search, details, events, offline demo | Implemented full-width tables, regex/inverse/fuzzy cached filters, actual server-side label selectors, and Pod/container/workload/Service/Node/Namespace drilldown; demo is isolated and covers all built-in kinds | Selector encoding and scope tests, retained selectors across LIST/WATCH/relist, UID-preserving cancellation/selection, redacted details, and current native UI evidence |
| R04 | Watch/reconnect/cancellation | Implemented bounded LIST/WATCH, 410 relist, authorization stop, polling/stale epochs and quiet renewal. HTTP errors cannot masquerade as successful empty watches or supply a false Status code through malformed bodies | Current deterministic HTTPS checks verify 429 backoff, 410 relist, 503 recovery, 401/403 cache clearing and stop, namespace refusal, cache bounds and idle timeout. Live Desktop watch revocation passed. Continuous local-cluster evidence and its limits are tracked in T05 and MAC_V1_HARDENING_ACCEPTANCE.md |
| R05 | YAML inspection | Done: redacted JSON and readable quoted YAML; bounded depth/output, no generic YAML apply | Serialize the same redacted object into safe, bounded YAML; escaping/round-trip tests; no new raw-object export path |
| R06 | Logs and container selection | Implemented snapshots/previous logs, head/tail/lookback ranges and selected-container follow; default 1h, configurable 1..86400s, rolling 256 KiB/2000 lines. Ordinary-container Pod merges and workload/selector fixed memberships are supported: at most eight Pods, 16 containers per Pod and 32 total streams in one namespace. Actual selectors and CronJob→Job controller UIDs determine capture; no new rollout Pods are added implicitly. Explicit `:logs-live` / `--dynamic-membership` opt-in recaptures every 10s and announces joins, departures and waiting joins; it is live-checked on Docker Desktop but not soak-tested through long rollouts | Four concurrent openings; attributed per-source errors, global head/tail, shared cache/deadline/64 MiB consumed-byte/1 MiB and 5000-lines per-second guards. Range/reconnect retains captured membership; periodic quiet-stream checks, pre/post-open and resume UID guards reject deletion/replacement. Each source permits at most five transport reconnects within the original shared session bounds and discloses overlap/gaps; clean EOF stops. Partial lists are rejected; init/ephemeral sources are excluded from merges. Current owned-fixture evidence belongs to HELM_LIFECYCLE_ACCEPTANCE.md. Prolonged adversity/soak and lossless-delivery claims remain unestablished |
| R07 | Metrics | Implemented Pod/Node CPU-memory tables, exact numeric sorting, bounded history and optional separate pod request/limit or node allocatable/capacity reads; missing/zero/invalid bases have no percentage. Docker Desktop now has an explicitly installed, Helm-owned Metrics Server 0.9.0, with verified aggregation and kubelet TLS. The application never installs a provider automatically | Exact quantities, rounding, UID/recreation/name-based comparisons, preservation of usage after a denied extra resource read, and actionable absent/forbidden/provider-unready guidance are covered. Positive live Pod/Node samples, comparisons and session history are covered by the current acceptance report. The public kubelet certificate pin and node address require renewal after replacement; provider serving TLS also has a finite lifetime |
| R09 | Helm releases | Implemented native Helm v3/v4 Secret/ConfigMap browsing and an explicitly reviewed lifecycle subset: install/upgrade/rollback/uninstall/test with trusted native Helm 4.3.1 on Unix, deterministic local/OCI packaged charts, separately opted-in hooks and schema workflows, and all three base flags `--write --allow-helm --allow-helm-write`. General chart/platform parity remains partial | OCI tags resolve once before review; digests/sizes seal content and optional `--helm-keyring` requires trusted OpenPGP provenance; `--helm-sigstore-policy` requires an exact pinned verifier, offline v0.3 bundle and explicit key/root or identity/issuer/root trust over chart archive bytes. Tests run exact stored test/test-success hooks and inspect completed results. CRD create, compatible schema upgrade and explicit deletion have separate reviews, bounded complete cluster-wide instance inventory and native UID/RV guards; normal release uninstall retains schemas. Foreign/GitOps-managed instances block destructive schema deletion. At most 128 affected ordinary resources, 32 hooks and 16 schemas; schema workflows also bound instance inventory. Explicit CRD storage migration supports identical schemas and owned namespaced instances; finalization requires official StorageVersionMigration controller proof. Docker Desktop without that API stages the switch/rewrites and retains historical storedVersions. Repository/version-range resolution, OCI manifest/referrer attestations, conversion webhooks, cluster-scoped custom instances, arbitrary dynamic charts, SQL and Windows lifecycle remain unsupported. See HELM_EXTENSIONS.md and current acceptance for limits and exact evidence |
| R08 | Simultaneous multi-context overview | Implemented CLI snapshots 1..100 with 4 concurrency/30s budget; continuous CLI/TUI Pod/Event watches for 1..9 explicit contexts, independent errors/caches, responsive numbered context cards and coalesced mailbox | Bounds, separate scope/cancellation and late-reply tests; quiet renewal fix and real watch revalidation. Independent multi-cluster/identity soak remains external acceptance |
| R10 | Dashboard health evidence | Implemented explicit readiness/lifecycle counts, current restart counters, current/last OOM evidence and recent Warning Event records linked to exact current Pod UIDs | Independent Pod/Event denial/stale/incomplete states, bounded caches, no Event message bodies; Desktop/minikube isolated acceptance in DASHBOARD_ACCEPTANCE.md. Direct warning-to-event drilldown is implemented (`w` then Enter; exact name+UID field selector). Dashboard log panes (`w` then `L`) follow the exact warning Pod over a separate read-only connection; `t` tails up to six workloads across clusters in timestamp order (24-Pod cap); cards also show Node and Deployment/StatefulSet readiness. Durable history remains future work |
| I08 | Saved selections | Implemented explicitly saved context groups, resource scopes and workload focus | Private bounded schema, no credentials/approvals, destination catalog and workload UID revalidation; corrupt/nonprivate stores retained with an in-app notice; no automatic startup restore |
| W01 | Scale/restart/delete | Implemented Deployment/StatefulSet scale (0..20), Deployment/StatefulSet/DaemonSet template restart, and explicitly allowlisted individual deletion families, plus Pod-only zero-grace kill behind `--allow-force-kill`; server dry-run, UID/RV, expiring one-use approval and durable local intent remain mandatory | Fresh target and server admission/RBAC/conflict checks for each family; restart review explains controller strategy, including OnDelete behavior. New owned-fixture acceptance is recorded in K9S_ACCEPTANCE.md; broader admission and unknown-outcome gates remain |
| W02 | Controlled edit/patch, apply and readable diff | Implemented focused image/resource forms, existing-resource YAML spec editing (private raw baseline, changed-path patch, dry run, full review) and, in 0.5.0, server-side apply of local manifests: up to 16 namespaced objects, dry-run per object, field-level review, resourceVersion-locked updates, no-op detection, nothing written if any object changed after review, conflicts never forced. Secrets, cluster-scoped kinds, system namespaces and GitOps-managed objects are refused; a multi-object apply is not atomic | Unit tests for flattening, identity, protected namespaces, GitOps refusal and diffs; live Docker Desktop create, update, no-op, stale-review and field-manager-conflict runs (2026-10-24). Cluster-scoped apply, prune and delete remain unsupported |
| W03 | GitOps guidance | Implemented for image/resource-budget edits and Helm lifecycle: recognized Argo/Flux metadata blocks live changes with source-repository guidance; detection is advisory, not universal | Detect known management metadata conservatively; show advisory source hints; default refusal of live edit when managed policy requires GitOps; never execute metadata URLs |
| W04 | Node maintenance and node shell | Implemented reviewed cordon/uncordon and bounded PDB-respecting eviction drain with separate --allow-node-maintenance. 0.5.0 adds a reviewed node shell behind --allow-node-shell: the complete privileged Pod is shown after a dry run, the session only targets KBorg's own Pod on the reviewed Node, and the Pod is deleted afterwards with activeDeadlineSeconds as a backstop | Drain evidence in MAC_V1_ACCEPTANCE.md; node shell live on Docker Desktop (host hostname and kubelet state visible, Pod deleted). Pod Security admission must permit privileged Pods in the chosen namespace |
| W05 | Pod file and directory copy | Implemented in 0.5.0 behind --allow-file-copy: one file or directory up to 64 MiB, typed approval, UID/resourceVersion recheck, audit. Fixed container argv (`cat`/`tar` to read; `set -C` or `mkdir` with size or file-count verification to write); strict tar extraction into a new owner-only directory | Unit tests for paths, no-overwrite, symlinks and hostile archives; live BusyBox round trips (3 MiB file, nested directory) on Docker Desktop. The container needs `sh`, `cat` and `tar` |
| I10 | Views, sorting and favorites | Implemented in 0.5.0: k9s-format `views.yaml` (built-in and JSON-path columns, W/H/N/T attributes, default sort), sort by any column in either direction, Ctrl-W wide, Ctrl-Z errors only, recent namespaces on 1–9 and recent contexts | Parser and rendering tests with custom columns, sort toggles and favorites; demo capture |
| W06 | CronJob actions, rollback and debug containers | Implemented in 0.5.0: CronJob trigger (owned manual Job) and suspend/resume; rollback of Deployments (ReplicaSets) and StatefulSets/DaemonSets (ControllerRevisions); ephemeral debug containers behind --allow-debug | Unit tests; live Docker Desktop trigger, suspend/resume, rollback to revision 1, unknown-revision refusal and a debug session sharing the target's process namespace |
| I09 | Checks and benchmark | Implemented in 0.5.0: `:lint` / `kborg lint --json` read-only misconfiguration report, and a bounded loopback HTTP benchmark of reviewed port forwards | Unit tests with fixtures and a local HTTP server; live lint on Docker Desktop. Lint rules are heuristics, not a policy engine |
| I07 | Explicit exports | Implemented selected redacted resource YAML, retained filtered logs with source identity, and selected Helm metadata/reference exports | Explicit destination/sensitivity preview, private exclusive file creation, 512 KiB/file and 100 retained files; local snapshot Ctrl-S retained. Logs/custom fields can contain confidential data |
| I01 | Structured CLI output | Done for bounded core reads/watch/metrics and Helm projections; documented command-specific output contract v1, exit statuses and unsupported JSON rejection; finite read sinks tolerate broken pipes | docs/CLI_OUTPUT.md and real-process closed-downstream test cover 29 read output paths; watch/follow stop immediately on pipe closure |
| I02 | Profiles/configuration/completions | Implemented strict schema-1 read-only/operator profiles that can forbid every capability flag (0.5.0 added Secret reveal, force kill, YAML edit, node maintenance, external editor, file copy, apply and node shell), four shell completions, data-only aliases and k9s-format `hotkeys.yaml`. Context-specific k9s files and automatic migrations are not supplied | Strict schemas and precedence; duplicate/unknown-key rejection; profiles cannot be loosened by later flags. Aliases: 256 entries/64 KiB/2048 bytes/16 steps. Hotkeys: 64 entries, supported shortcuts only, main view only, never a shell |
| I03 | Approved editor/pager/tool handoff | Partial: bounded reviewed Unix native filters and built-in interactive redacted JSON/YAML pager implemented. Opt-in `--allow-external-editor` hands the YAML spec draft to `$VISUAL`/`$EDITOR` (Ctrl-E) through a private create-only file that is always removed; the result re-enters plain-text validation, dry run and review. External pagers and privileged tool containment remain unsupported | Native pager search/navigation/resize/finite lifetime/restoration; external filters retain absolute immutable argv, exact transfer review,15s/256 KiB and process-group cleanup. Foreground TTY plus bounded output/platform containment remain gates |
| I04 | Exec/attach | Implemented optional native stdin/TTY for ordinary containers, 1..3600s, Ctrl-] disconnect,resize, 16 MiB output/1MiB input; output-only 30s mode retained; remote full-screen applications unsupported | Real BusyBox input/resize/11s quiet pause/attach/cancel/expiry/nonprinting flood; terminal restoration and escape filters. Remote termination after disconnect and API name-recreation race remain limitations |
| I05 | Port forwarding | Implemented native TUI preparation/status/cancellation with explicit capability and write opt-in; one reviewed session on 127.0.0.1, 1..300 seconds and four active clients. Closing its view preserves the session; explicit stop, scope/context change or exit cancels it | Captured target/UID/session review, loopback/port/lifetime bounds, listener release and cancellation, per-client UID recheck and isolated local disconnect handling. Name-recreation and adversarial protocol gates remain |
| I06 | Generic command extensions | Implemented optional data-only local view registration with private schema 1, max 64, immutable SHA/metadata/argv, explicit flags and fresh input approval, runnable from the CLI and (0.5.0) the TUI with `:ext`; `kborg extensions import-k9s` classifies k9s plugins. No in-process plugin, no variable substitution | Registration/update/schema/permissions/concurrency/literal-argv tests, importer classification test and real native filter smoke. OS-user filesystem/network permissions remain; independent containment required before production promotion |
| A01 | GitHub Copilot login and minimized local context | Implemented local status minimization, trusted login handoff and explicit pinned gh account reuse or private credential file for GitHub.com, GHE.com tenants and GHES 3.22+ offline provider mode | Selected account authority is host-bound and rechecked, ambient tokens excluded; per-deployment exact broker allowlists; GHES private ranges and internal CA are separate opt-ins. Live GHE.com/GHES inference not yet demonstrated without an enterprise account |
| A02 | In-app Copilot advisory diagnosis | Implemented optional macOS ARM64 native advisory workflow through `:copilot` and `copilot diagnose`, with exact payload/account approval, pinned signed native tool, fresh containment canaries and bounded read-only output | Current local containment and live-account evidence is recorded in TRUST_MIGRATION_ACCEPTANCE.md. Linux/Windows inference, live GHE.com/GHES inference evidence, broader account/subscription coverage and external security audit remain open. No automatic remediation |
| U01 | Themes and first-use startup | Done: 20 bundled themes including Flexoki Dark and Rose Pine; new-install Flexoki Dark default, saved preferences preserved, first-run picker, 2-second skippable/disableable animation | Current theme contrast tests and actual palette captures in TRUST_MIGRATION_ACCEPTANCE.md; historical startup reports |
| U02 | Custom theme workflow | Implemented CLI and TUI create-from-preview, explicit no-overwrite creation and backup-preserving replacement; atomic preferences and legacy fallback; Windows ACL acceptance remains | :themes then N captures actual session/preview colors; Enter creates/selects, Esc restores prior theme; pure projection and capture/cancellation tests plus native PTY evidence |
| U03 | Visible action menu and capability feedback | Implemented visible action picker with explicit OFF reasons, keyboard paths, captured-target edits and wrapped review; broader accessibility acceptance remains | Action reasons reflect logs, write and resource-family capabilities; hidden confirmation text blocks approval; resize, focus, late replies and UID-preserving selection tests retained |
| U04 | Terminal compatibility/accessibility | Partial: conservative ASCII data, continuous UTF-8 frames with ASCII fallback, truecolor/256/ANSI16, monochrome, explicit selection markers, TERM=dumb rejection and keyboard-only --no-mouse. Sensitive approval values now receive reversible ASCII escapes before layout and coverage | Current Mac matrix passes 10 PTY profiles, 71 checks, 50 resize transitions, two dumb-terminal checks and five real tmux checks. Native Terminal/iTerm automation was explicitly blocked; fonts, screen readers, wide/grapheme Unicode and remote SSH remain unvalidated. See MAC_V1_TERMINALS.md |
| U05 | Modern visual refinement | Implemented a quiet header with only keymap help, responsive dashboard cards, full-width tables, stable context/namespace identity, continuous frames, contextual controls and typed command navigation. Saved-picker actions now persist inside its frame during background updates; escaped approval content stays distinguishable | Current screenshots and regressions are in MAC_V1_HARDENING_ACCEPTANCE.md; DASHBOARD_ACCEPTANCE.md retains dashboard implementation evidence. Native font/accessibility acceptance and production-scale p95/p99 remain open |
| U06 | k9s command/key familiarity | Implemented for the documented workflows: command grammar, aliases, hotkeys, completion/history, views, contextual keys, selectors/drilldown, logs, Helm subset, pulses/xray/workloads, RBAC subjects, local manifests with reviewed apply, saved snapshots, node shell, file/directory copy, port-forward benchmark, `:lint` in popeye's role and extensions in place of plugins. Generic Secret YAML/describe views are excluded by design (`:secrets` instead); image vulnerability scanning (k9s vulscan) runs through an explicitly trusted external Trivy or Grype with `:scan` / `:scan ns` / `kborg scan` (`--scanner ABS_PATH --allow-scan`) | [K9S_COMPATIBILITY.md](K9S_COMPATIBILITY.md) is the exact mapping, including where KBorg is stricter |
| U07 | Actionable errors | Done in 0.5.0: `src/errors.rs` classifies Kubernetes Status objects (RBAC with verb/resource/namespace, unauthorized, not found, conflict, admission, Pod Security, quota, invalid, expired, throttled, unavailable) and transport failures (refused, DNS, TLS, timeout, credentials, network) and renders "what failed: why. Next: step" for gateway calls, watches, logs, Helm, metrics, node operations, exec/forward and discovery. Kubeconfig and connection errors carry a next step | Classification-table and exhaustive-cause tests; source lints: error literals in connection modules need `Next:`, and hand-written "HTTP {}" / "server details suppressed" texts are rejected anywhere in `src`. Live Docker Desktop checks of 403 (namespaced, API group, cluster scope), 404, refused, wrong CA and DNS (2026-10-25). Error literals in other modules are converted as they are touched |

## Security and operational acceptance

| ID | Gate | Status and work |
|---|---|---|
| S01 | Local file trust | Improved: same-directory synced atomic replacement; Unix0700/0600 tested. Windows ACL provisioning and handle-based race resistance remain gates |
| S02 | Audit integrity and crash reconciliation | Partial: bounded pending-intent/crash reporting includes Helm intent/outcome; no automatic retry or rollback. Partial/unknown operations require inspection. Protected external audit sink and operational reconciliation drills remain |
| S03 | Credential lifecycle and cloud helpers | Implemented opt-in reviewed Unix native ExecCredential v1/v1beta1 token or cert/key with bounded supervision, context/endpoint/CA/TLS/argv/env/SHA binding, expiry and 401/403 stop; legacy providers/interpreters/PATH chains rejected | Mock rotation/expiry/leak tests plus real Docker Desktop helper/403/reconnect/trust-tamper test; Windows helpers are unsupported; actual cloud identities, chained CLIs and handle-based trust remain acceptance gates |
| S04 | Server authorization/admission | External gate plus test fixtures: use least-privilege RBAC and admission policies independently of client controls; retain forbidden/dry-run/admission mutation cases for each new write family. |
| S05 | Transport edge cases | Partial: mandatory TLS, bounded bodies and no credential-bearing redirects/proxies. Current isolated TLS scenarios pass 429 backoff, 410 relist, 503 recovery, 401/403 clearing/stop, namespace and object caps, slow-body timeout and CA rotation requiring explicit reconnect. These are protocol fixtures, not a live certificate rotation or provider-identity test. Deployment-specific rotation, APF throttling and proxy design remain open; see MAC_V1_HARDENING_ACCEPTANCE.md. |
| S06 | Secret and terminal safety | Partial: bounded known-field redaction, terminal sanitation, strict local-manifest parsing and re-redacted saved snapshots remain. Internal review fixed ambiguous Unicode approval displays: Unicode/control values and literal backslashes now have distinct bounded presentations before wrapping and read-all coverage. Expanded reviews over 1 MiB block approval. Broader sink canaries/parser fuzzing remain; logs and arbitrary CRDs may contain confidential fields outside known projections. |
| S07 | Copilot containment | Implemented and locally validated on macOS ARM64: actual OS denial canaries, signed pinned native, authenticated bounded TLS-host broker, explicit gh account binding and minimized-transfer approval. Other platforms fail closed. Broader native/platform/provider testing and independent security review remain gates; no autonomous remediation. |
| S08 | Dependency minimization | Partial: the earlier TLS advisory fixes remain locked. The October 23 RustSec refresh reports 203 packages, zero known vulnerabilities, zero warnings and no advisory ignores/platform filters; exact database and lockfile hashes are retained in MAC_V1_SECURITY_REVIEW.md. This is a known-advisory snapshot, not a whole-product or native-tool security certification. |
| T01 | Supported Kubernetes versions | Partial evidence: historical 1.34.11/1.35.8 and current Desktop 1.36.1 plus isolated minikube 1.35.9. User target is 1.34+. Future minors require continuing compatibility runs; this is not a guarantee for untested releases. |
| T02 | OS packages | Partial: the current Mac ARM64 clean native candidate passes eight archive/content checks, CLI/configuration smokes, 63 PTY states, 10 terminal profiles, real tmux and nine TLS fault cases. The packager now verifies actual clean HEAD and recursively tracked inputs, builds fresh natively and binds the copied binary hash. Exact candidate/package source and distinct executable hashes are in MAC_V1_SECURITY_REVIEW.md. Earlier Windows/Linux/Intel Mac evidence remains historical in PLATFORM_BUILDS_0.3.md; Windows ARM64 remains unverified. Actions stays manual-only at the owner's request. |
| T03 | WSL2/Docker Desktop | Done for native WSL app accessing existing Desktop Kubernetes. No second Docker engine is required. Keep explicit server/CA ownership checks and token cleanup in harnesses. |
| T04 | Scale | Partial: 1,001 objects and 100 aliases to one cluster. 100 independent clusters were not tested and exceed measured local resources. 0.3 added a real concurrent 100-alias overview: 100 successful rows, four connections, 0.131 seconds; see PERFORMANCE_0.3.md. This remains one cluster/identity. |
| T05 | Performance/fault/soak | Partial: historical 60-second WSL live/poll samples measured about 11.1 MiB RSS; 100 real context aliases completed with concurrency4. The October 10 priority report retains 14-state macOS/Linux ARM64 PTY samples with quiet idle output and terminal restoration; the latest source-specific checks belong to DASHBOARD_ACCEPTANCE.md. An earlier parallel snapshot test encountered lock contention; the final serial suite passed. Parallel process-test isolation remains a harness follow-up. Sustained floods, request counts, reconnect storms, slow networks, longer soak, p95/p99 latency and hostile WebSocket allocations remain. |
| T06 | Real terminal matrix | Partial: the current macOS PTY matrix and isolated real tmux 3.8 checks pass, including narrow resize, Ctrl-C, shell return and terminal restoration. Native Terminal/iTerm computer use was blocked by the tool; no native screenshot or font pass is claimed. Actual SSH, screen readers, alternate fonts and current Windows/Linux terminal application checks remain open; see MAC_V1_TERMINALS.md. |
| D01 | Distribution trust | Partial: CycloneDX SBOM, verified dependency notices, checksums and unsigned source/build records are present. Native packaging now binds clean source, fresh build and checked binary; Mac archives exclude filesystem metadata. These local records do not authenticate a publisher or prove reproducible archive bytes. The manual `Signed release` workflow adds keyless cosign bundles, GitHub build provenance and `SHA256SUMS` for every asset, and macOS codesign/notarization when the Apple secrets exist; cargo-deny runs in CI. External gate: the first dispatched signed run, the Apple Developer ID secrets and a first Scorecard result |
| D02 | Operations and maintenance | Implemented operational installation/update/rollback/uninstall/config/recovery guide; organizational pilot and disaster drills remain external gates |
| D03 | Documentation consistency | This register, HELM_LIFECYCLE.md and K9S_COMPATIBILITY.md distinguish supported subsets from missing behavior. MAC_V1_HARDENING_ACCEPTANCE.md is the current source-specific report; dashboard/operator and earlier platform reports remain dated evidence. Release instructions describe manual-only CI, fresh native packaging and unsigned provenance accurately |

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

The node shell that earlier versions excluded is now an explicit, reviewed opt-in (W04).


Autonomous AI remediation, arbitrary in-process plugins, automatic cluster-admin setup, an embedded terminal multiplexer and a self-updating binary remain outside this production design. “All terminal programs” means documented standards-based CLI and approved process interoperability, not universal compatibility. Optional AI and generic extensions must not block a safe core release.

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

## Dashboard and saved-selection continuation (2026-10-23)

The current header removes visible command labels at the user's request. Typed k9s navigation remains; `?` opens help, `D` opens context cards, and `b`/`B` save/reopen resource selections. Dashboard `s` saves its contexts and namespace. Health summaries and warnings use independent bounded evidence; saved workload identities are revalidated rather than transferred to replacement objects.

[Current acceptance](DASHBOARD_ACCEPTANCE.md) binds the final binary and source manifest to serial tests, actual PTY captures, Docker Desktop/minikube live checks and fixture cleanup. Earlier acceptance remains evidence of its exact build. Dynamic workload-log membership, direct warning drilldown, packaging, longer soak and independent security/terminal acceptance remain in [the v1 plan](MAC_V1_PLAN.md).
