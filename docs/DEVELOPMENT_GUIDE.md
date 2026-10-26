# KBorg — Architecture and Development Guide

Status: 0.4.0 released, 0.5.0 in development; production validation (external audit, signing) remains incomplete. Updated: 2026-10-26. Current capabilities: [IMPLEMENTATION_STATUS.md](IMPLEMENTATION_STATUS.md). Authoritative gaps: [REMAINING_WORK.md](REMAINING_WORK.md). The design sections below were written for earlier versions; dated appendices describe their original versions.

## Current release-next contract

This branch reuses the user's existing kubeconfig/login tooling. Interactive startup asks Yes/No for the named context before any connection/helper; No opens the context picker and Esc exits. Machine-readable CLI and demo remain noninteractive. Operator capabilities are available by default, with per-operation review and quick confirmation; `--read-only` and company profiles constrain access. `strict_confirmations` restores typed phrases. Kubeconfig TLS/proxy/exec options follow the current source, so the older deny-by-default design below is historical, not a claim about today's defaults.

Copilot has discoverable setup/status and an explicit transfer review; contained inference currently requires macOS ARM64 and an entitled supported GitHub account/runtime. A status panel is not proof of real enterprise acceptance. The [current status](IMPLEMENTATION_STATUS.md), [remaining gates](REMAINING_WORK.md) and [release-next review](RELEASE_NEXT_REVIEW.md) take precedence over dated architecture/acceptance sections below.

## Source map

| Area | Modules |
|---|---|
| Entry and CLI | `main.rs`, `cli.rs` (flags, commands), `completions.rs`, `config.rs` (profiles) |
| Cluster access | `gateway.rs` with `gateway/` (`catalog`, `kubeconfig`, `reads`, `log_streams`, `operations`, `apply`, `errors`, `demo`), `discovery.rs`, `watch.rs`, `transport.rs`, `auth.rs` |
| Safety | `safety.rs` (sanitizing, redaction, private files), `policy.rs`, `audit.rs` |
| Writes | `edit.rs`, `manifest_edit.rs` (YAML spec edit), `manifest_apply.rs` (apply), `node_maintenance.rs`, `ops.rs` (exec, attach, forward, copy, node shell), `file_copy.rs`, `archive.rs`, `node_shell.rs`, `session.rs` |
| Reads and checks | `logs.rs` with `logs/`, `metrics.rs`, `insights.rs` (pulses, xray), `lint.rs`, `navigation_views.rs` (`:can`, `:dir`), `secrets.rs`, `overview.rs`, `cluster_health.rs`, `dashboard_health.rs`, `bench.rs` |
| Helm | `helm*.rs` |
| Copilot | `advice.rs`, `copilot*.rs` |
| TUI | `tui.rs` with `tui/` (`worker`, `commands`, `keys`, `replies`, `render`, `format`, `startup`, `tty`), `ui_*.rs`, `themes.rs`, `splash.rs`, `hotkeys.rs`, `aliases.rs` |
| Tools | `handoff.rs`, `extensions.rs`, `external_editor.rs`, `pager.rs` |

Every new write path follows the same shape: a capability flag (and `config.rs` field), a prepare step that dry-runs and builds a complete review, a typed confirmation with expiry, a recheck of identity before the write, and an audit record. Run `cargo fmt`, `cargo clippy --locked --all-targets -- -D warnings` and `cargo test --locked` before committing.

## 1. Objective and assumptions

Build **KBorg**, a responsive Kubernetes terminal application inspired by [p10node/k10s](https://github.com/p10node/k10s), with production security as the primary constraint. The product name is confirmed; the proposed executable and repository name is `kborg`. This is the assumed reference because it includes context-aware AI. Another project also uses the k10s name; confirm the reference before finalizing feature parity.

Deliver the useful operational workflows first, without AI. Add an optional advisory AI feature only after the core passes production acceptance. Rust is the proposed implementation language; memory safety and predictable resource control help, but language choice does not prevent authorization errors, credential theft, insecure dependencies, or operator mistakes.

Prefer Rust standard-library facilities. Every external dependency requires an architectural decision recording necessity, alternatives, transitive cost, maintenance health, license, security history, and removal strategy. Do not implement custom cryptography, TLS, certificate verification, or a general-purpose terminal emulator merely to avoid dependencies.

The application is a local client using the operator's identity, not a cluster agent or privileged service. Initial targets: Linux, macOS, and Windows; Linux is the first validated production platform, with other platforms promoted only after their integration and security tests pass. No installation of cluster components is required.

## 2. Product scope

The reference supplies resource browsing, search, logs, exec, port forwarding, themes, and command extensions. Our feature choices below are proposed requirements, not an assertion of complete parity. Maintain a feature inventory against a pinned reference revision before implementation.

| Capability | Delivery | Production constraint |
|---|---|---|
| Context and namespace selection; resource tables; object details | MVP | Persistent cluster identity and environment label on every view |
| Keyboard navigation, visible action menu, optional mouse, search | MVP | Every mouse action also works by keyboard |
| Events, bounded live logs, YAML/JSON inspection | MVP | Sanitize terminal control sequences; no automatic persistence |
| Offline demo | MVP | Separate backend, permanent DEMO marker, no real credentials |
| Discovery and custom resources | MVP | Lazy access; bounded discovery; tolerate forbidden kinds |
| Metrics and multi-context overview | Next | Optional metrics API; explicit context limits and opt-in watches |
| Scale, restart, controlled patch/edit, delete | Next | Typed requests, target-bound approval, server-side preconditions |
| Exec/attach and port forwarding | Next | Separate capabilities; not enabled by read-only mode |
| CLI output and integrations | Next | Structured formats, explicit target, documented contracts |
| External command extensions | Later | Disabled by default in production; managed approvals |
| Bundled themes, picker/live preview and custom theme installation | MVP | Data-only schemas, protected warning visibility and offline built-ins; see Section 18 |
| Configuration and completions | Incremental | Strict schemas; no executable configuration |
| Advisory AI | Optional final phase | No execution credentials or write tools |

No privileged node shell, automatic remediation, cluster-admin bootstrap, arbitrary in-process plugins, embedded terminal multiplexer, or self-updating binary in the first production release. Use managed package/release distribution. GitOps-managed workloads should direct edits back to the source repository; offer live changes only under an explicit operational policy.

## 3. Security invariants

1. The API server's RBAC and admission policies are authoritative. The application never grants itself additional rights.
2. Start in read-only mode. Its policy rejects mutations, exec/attach, forwarding, and arbitrary command launches at the execution boundary, regardless of UI visibility. Credential helpers are a separately trusted authentication prerequisite, not evidence that read-only means zero local execution.
3. Never implicitly switch context for an operation. Bind each operation to a session identity, server endpoint, namespace, resource identity, and operation parameters.
4. All writes require an explicit workflow showing target, effect, and diff or exact operation. No automatic retry of ambiguous non-idempotent writes.
   In the initial managed production profile, allow only reviewed operation families: scale within approved bounds, restart through a specific annotation patch, and explicitly scoped deletion. Generic YAML editing, arbitrary patches, RBAC changes, and custom-resource writes remain disabled until individually threat-modeled and approved. RBAC permitting an operation does not make its effects safe.
5. Never fetch Secrets in bulk. Secret contents, kubeconfig credentials, and authentication material never enter logs, exports, crash reports, plugins, or AI by default.
6. Treat resource text, logs, filenames, configuration, plugin output, and AI output as untrusted. None can authorize another action.
7. No shell interpolation for built-in operations or extensions. Use argument arrays, explicit executables, and bounded input.
8. TLS verification is mandatory in the production profile. Reject insecure-skip verification there; never silently downgrade.
9. All queues, caches, stream buffers, concurrency, and subprocess lifetimes have limits.
10. Application controls protect supported application workflows; they are not a security boundary against the same user replacing the binary or running another client. Strong organizational restrictions require least-privilege credentials and server-side enforcement.

## 4. Language and dependency decision

Use Rust stable with a pinned toolchain and committed lockfile. Forbid unsafe code in application crates. Any unavoidable platform/FFI wrapper is isolated, documented, reviewed, and tested; dependency unsafe code still requires risk review. Avoid panic-driven control flow and secret-bearing debug representations.

Use std for processes, filesystem operations, paths, synchronization, bounded channels, threads, time, and basic data structures. Start with a bounded worker architecture rather than a custom async executor. Evaluate an established runtime if the Kubernetes transport library requires it or measured stream concurrency justifies it.

| Concern | Native-first approach | Dependency exception |
|---|---|---|
| Business logic, policy, state, caching | std and local typed modules | None initially |
| Kubernetes API, discovery, watches, exec, forwarding | Narrow internal adapter | Evaluate kube/k8s-openapi rather than rebuilding protocol support |
| TLS/HTTP | No custom implementation | Maintained transport and TLS stack with audited feature selection |
| JSON/YAML/config | Strict schemas and limits | Established serializers; YAML compatibility is not available in std |
| Terminal I/O | Local interface, conservative text mode | Small portable backend for raw mode/input/resize; FFI if necessary |
| Rich TUI layout | Simple local rendering if manageable | Ratatui only if prototype shows enough benefit to justify its graph |
| Crypto and signature verification | No custom algorithm | Established verifier or trusted package distribution |

Rust std has neither a Kubernetes client nor portable raw terminal controls nor TLS. A zero-dependency production implementation would shift a large protocol and security burden onto us. Keep direct dependencies few, record the total transitive graph, disable unnecessary default features, and benchmark before rejecting dependencies on performance grounds. Do not invoke kubectl as the core backend to claim a zero-dependency architecture; that moves dependencies and credential behavior outside our control.

Prototype two narrow slices: native-first rendering with a minimal terminal backend, and a mature TUI backend. Measure startup, memory, Unicode handling, resize, accessibility, and maintenance complexity. Select one through an ADR. Prototype the Kubernetes adapter separately and verify required protocols before committing to its library version.

## 5. Architecture

```text
Terminal / noninteractive CLI
          |
    Input -> UI reducer -> immutable view snapshots
          |                    ^
      Typed intent             | bounded events
          v                    |
    Operation coordinator ---- store / stream manager
          |
    Policy + approval validation
          |
    Kubernetes gateway ------ authenticated HTTPS API
          |
    outcome + redacted audit event

Optional boundaries:
  Integration runner -> trusted external process
  Sanitized snapshot -> AI advisory process -> untrusted suggestion
```

Use a modular monolith, not distributed microservices. Separate processes only where capability isolation provides real value. Rust module boundaries prevent accidental coupling; they do not sandbox malicious code in the same process.

Suggested Cargo workspace:

```text
crates/
  domain/        resource identifiers, intents, outcomes, errors
  policy/        profiles, capability checks, approval validation
  kube_gateway/  auth, TLS, discovery, reads, watches, typed writes
  application/   reducer, sessions, coordinator, bounded scheduling
  terminal/      render/input adapters, sanitization, restoration
  integrations/  subprocess contracts and lifecycle
  audit/         redaction, event schema, sink abstraction
  cli/           main, config, subcommands, structured output
  ai_advisor/    optional separate binary; no kube client dependency
docs/adr/        decisions and rejected alternatives
tests/          integration, security, compatibility, load fixtures
```

UI code never receives raw credentials or a general-purpose Kubernetes client. It submits typed intents. Only the gateway executes API operations, and only after the coordinator validates policy. Avoid a generic arbitrary-method request interface exposed to the UI or plugins. Apply the same path to interactive and noninteractive commands.

### Reads, watches, and responsiveness

Create watchers lazily for visible resource kinds and explicitly requested scopes. Share a watch among matching consumers. Cache keys include context/session identity, resource group/version/kind, namespace, and selectors. Secrets use a separate non-caching policy. Bound connections, discovery requests, cached bytes, objects, and log streams per session and globally.

Use bounded worker pools and channels. Coalesce replaceable UI updates, but preserve operation outcomes and audit events. Render only when state changes or a stream needs a throttled refresh. Virtualize resource rows; searching the current cache must be labeled differently from querying the server.

Handle expired resource versions by relisting, transient disconnects through capped exponential backoff with jitter, and authorization failures without endless retry. Mark cached results stale. Do not infer that absence from a stale cache means deletion. Cancellation on context change drops streams and clears sensitive state; late events carry generation IDs and cannot update a new session.

### Mutation lifecycle

1. Capture exact target and session generation; fetch current object where needed.
2. Check local capability and provide RBAC feedback; the server still makes the final authorization decision.
3. Build a typed operation. Where supported, perform dry-run and show admission/defaulting changes. Dry-run is neither a reservation nor proof the final operation will succeed.
4. Show exact context/server, namespace, kind/name, impact, diff, and preconditions. Require deliberate confirmation; deletion and high-impact changes require typing the target identifier.
5. Bind approval to the operation digest, object UID/resourceVersion where applicable, session, and short expiry. Any edit or target change invalidates approval.
6. Execute using operation-specific concurrency controls: delete UID/resourceVersion preconditions, guarded patch tests or resourceVersion handling, and scale subresource concurrency where supported. Document unsupported cases rather than claiming one universal precondition.
7. On conflict, refresh and request a new review. On timeout, reconcile outcome before offering another attempt. Report unknown outcomes honestly.
8. Emit a redacted result and correlation ID. Never promise rollback for deletion or arbitrary patches. Capture a sanitized prior state only when policy permits and explain recovery limitations.

## 6. Threat model and controls

| Threat / trust boundary | Control | Required evidence |
|---|---|---|
| Wrong cluster or stale selected object | Explicit target binding, generation checks, UID/preconditions | Context-switch race and delete/recreate tests |
| Malicious kubeconfig or exec authentication helper | Treat kubeconfig as executable trust input; show and approve helper executable/args; managed allowlist; timeout | Unknown helper blocked; approved OIDC/cloud helper works |
| Credential leakage | Keep credentials internal, redact at source, no default object persistence, protected config permissions | Canary secrets absent from every sink |
| Terminal escape/clipboard/title injection | Parse/sanitize untrusted text; block OSC/DCS/control output, bound line lengths | Escape-sequence fuzzing and terminal replay |
| Malicious extension | Disabled default, executable identity validation, restricted environment, capability limits | Path hijack, symlink, argument injection and timeout tests |
| API outage / oversized responses | Timeouts, byte/object caps, fair queues, jitter, stale labels | Slow-server and sustained-stream load tests |
| AI prompt injection or exfiltration | Allowlisted snapshot, separate process, no tools, restricted egress | Adversarial logs/manifests cannot trigger tools or leak canaries |
| Compromised build/dependency | Locked graph, vulnerability/license review, SBOM, signed provenance/releases | Release verification and tampered artifact rejection |
| Local audit tampering | Protected local spool plus centralized protected sink where required | Sink outage and correlation with API audit tested |

Credential helpers run as the local user and can perform arbitrary local actions. Approval is a trust decision, not a sandbox. Respect approved interactive authentication workflows; never hang indefinitely waiting for hidden prompts. Endpoint/CA/helper changes invalidate prior trust decisions. Preserve identity context in cache keys and clear caches on reauthentication to a different principal.

Validate config size, schema version, unknown fields, permissions/ACLs, paths, and symlink behavior. No automatic loading of executable project-local configuration. Application configuration uses one reviewed serialization format; reading standard kubeconfig still requires its supported formats. Do not modify kubeconfig implicitly. TLS client defaults must be explicitly verified, including proxy and custom-CA behavior.

Avoid persisting logs and object bodies by default. Treat all logs as potentially secret-bearing; redaction cannot guarantee removal of every secret. Secret values require a dedicated explicit capability and transient view, with export and clipboard disabled by default. Managed profiles may forbid displaying them entirely. Minimize secret lifetime; zeroization is defense in depth, not proof all copies disappear from memory. Disable body-bearing crash telemetry and document core-dump exposure.

Production audit includes actor reference, target, intent, approval ID, timestamps, result, and request correlation where available. Exclude credentials, resource bodies, and command output. API-server auditing records server-observed activity; local records are supplementary. For managed writes, require a durable audit enqueue before execution, block new writes if that fails, and reconcile pending operations after interruption. A centralized signed/transport-protected audit destination is deployment-specific work, not a capability supplied by an ordinary local file.

## 7. Terminal and external-program integration

“All terminal programs” becomes standards-based interoperability, not a guarantee that every executable or emulator works. Support documented tiers:

* Interactive: VT-compatible terminal emulators, keyboard-first operation, optional mouse, Unicode with ASCII fallback, resize, SSH, tmux and screen. Negotiate capabilities and degrade gracefully.
* Noninteractive: CLI subcommands with JSON output on stdout, diagnostics on stderr, stable exit codes, bounded reads, explicit context and namespace, and no prompts when stdin is not a terminal. Writes need an explicit reviewed plan workflow; never default to yes.
* Process handoff: approved editor, pager, kubectl, Helm, and other tools through executable-plus-argv adapters. Suspend raw/alternate-screen mode before handoff and restore it on exit, panic, signal, or cancellation.
* PTY integration: defer embedded interactive programs until required. Unix PTYs and Windows ConPTY need different implementations and acceptance tests. Simple terminal handoff is the first implementation.

Test representative combinations: Windows Terminal/PowerShell, a Linux VT emulator/bash, macOS Terminal/zsh, SSH, tmux, screen, redirected output, small terminals, TERM=dumb, interrupted commands, Unicode widths, and disabled mouse. Publish supported combinations and known limits.

Subprocesses use absolute approved executable paths and direct argv. Reject arbitrary executable names derived from objects. Separate user-reviewed static arguments from untrusted values; validate option-like values and use separators where the tool supports them. Environment inheritance is allowlisted; avoid passing tokens, AI keys, or kubeconfig paths by default. Set an explicit working directory, bound output, enforce timeout, and clean up process trees using platform-specific handling. Validate executable identity at launch; record residual path-replacement races and use stronger OS launch mechanisms where available.

Two extension modes: credential-free processing of sanitized JSON on stdin, or privileged tool handoff. Privileged handoff can bypass application policy through its own credential discovery, network access, or home directory access. Therefore disable it under managed production/read-only profiles unless it uses independently constrained identities and a validated OS sandbox. An environment allowlist alone is not a sandbox. Generic native extensions are trusted code, not safely restricted plugins.

Exec/attach can modify a workload despite using a connect-style API operation; classify it as privileged. Show pod/container identity, require distinct authorization, and do not record session contents by default. Port forwarding binds loopback only by default, reports address/port visibly, requires an explicit lifetime, and closes on context change or application exit. Wildcard/public binding is separately gated.

## 8. Optional AI: advisory-only architecture

The core ships and remains fully usable without AI. Initial releases contain no provider SDK, model calls, or AI credentials. Later AI is opt-in and organization-disableable. A local model reduces external data transfer but does not eliminate injection, malicious suggestions, or local model supply-chain risks.

Data flow: user selects an object -> application creates an allowlisted snapshot -> deterministic filtering/redaction and byte limits -> exact payload preview -> explicit send approval -> separate advisor process -> plain-text sanitized response labeled as advice.

The advisor has no kubeconfig, cluster client, shell/exec tools, or write capability. No automatic fetching of related resources and no automatic export of manifests or logs. Start with selected status, event reasons, and approved non-sensitive metadata. Logs and free-form annotations are excluded unless specifically selected and previewed. Never send Secrets, tokens, service-account material, credentials, or arbitrary environment values. Field filtering and redaction reduce risk; they cannot guarantee safe treatment of arbitrary business data. Strict deployments should allow only schema-approved fields and prohibit free-form text.

Use a versioned size-bounded stdin/stdout protocol with request IDs and timeout. Do not assume a separate process provides containment: remove inherited credentials, restrict filesystem access and network destinations through a validated OS sandbox, and test on each platform. If equivalent isolation is unavailable, disable AI in the strict production profile. Provider endpoint changes need review; retention, training use, residency, contract, and authentication storage are explicit prerequisites. GitHub Copilot is the only approved provider direction: github.com and Enterprise Cloud *.ghe.com; no other provider or Enterprise Server support is assumed.

Treat model output and cited evidence as untrusted. Do not execute generated commands or promote suggestions directly into mutation requests. A user may manually start an existing typed operational workflow; the usual policy, fresh reads, diff, and approval apply independently. Evaluate injected instructions in logs and object text, canary data leakage, oversized output, unreliable advice, cancellation, and accidental cross-context reuse. AI acceptance is a separate release gate and cannot delay the secure core.

## 9. Development process and milestones

Use small vertical slices, protected branches, required review, and traceable requirements. One developer can build the prototype, but sensitive production changes need an independent reviewer. A self-review improves quality without replacing independent security assurance.

| Phase | Deliverable | Exit gate |
|---|---|---|
| 0: requirements and decisions | Feature inventory, threat model, RBAC map, terminal/platform matrix, dependency ADRs | Exact MVP, production profiles and trust boundaries approved |
| 1: feasibility | Demo TUI, native/dependency comparison, auth/discovery/read/watch spike, Docker Desktop Kubernetes test environment | TLS/auth work; bounded concurrency; platform restoration demonstrated; isolated local test identities validated |
| 2: read-only MVP | Navigation, search, details, events, logs, discovery, structured reads, bundled themes and custom theme workflow | No write/connect/extension bypass; leakage, terminal-injection and theme validation/accessibility tests pass |
| 3: controlled operations | Typed write workflows, diff/dry-run, conflict handling, audit | Target races, ambiguous outcomes and audit failure tested |
| 4: interoperability | CLI contracts, approved handoff, exec/forward, remaining platforms | Compatibility matrix and subprocess/stream cleanup pass |
| 5: production release | Hardened packaging, docs, operational drills, independent assessment | Release checklist satisfied; staged pilot accepted |
| 6: optional AI | Sandboxed advisory process and reviewed data policy | Independent isolation/exfiltration evaluation; default remains off |

Do not commit dates until Phase 1 produces evidence. Estimate each phase from actual platform and authentication complexity. Multi-context breadth, PTY embedding, privileged extensions, and AI are optional scope cuts when time is constrained.

For each feature: document actors and required Kubernetes verbs/subresources; define abuse cases and limits; implement domain behavior; add meaningful tests; review security and dependency changes; demonstrate in a disposable cluster; update the guide and user documentation. Never use production credentials in CI. Any additional API verb requires policy and test changes before merging.

Maintain ADRs for Rust/dependencies, terminal backend, Kubernetes transport, session identity, mutation approval/preconditions, config trust, plugin privileges, audit availability, and AI isolation. Each ADR contains options, evidence, decision, consequences, and reconsideration triggers.

## 10. Verification and measurable budgets

The following are proposed budgets, not measured claims. Establish reference hardware and fixtures during Phase 1: 4 CPU cores, 8 GiB RAM, local API fixture; separate real-cluster runs with 100 ms RTT. Record OS, build flags, object sizes, API versions and workload distribution.

| Metric | Initial target |
|---|---|
| First interactive frame without waiting for API | p95 <= 300 ms |
| Cached navigation/input response | p95 <= 50 ms |
| Idle CPU after initial sync | <= 1% of one logical core over 60 seconds |
| Memory with 10,000 cached representative objects | <= 200 MiB, with a hard configurable cap |
| Render frequency under heavy streams | <= 30 frames/second; preserve input responsiveness |
| Active list/watch streams | Default <= 8 per context and <= 24 globally |
| Log buffering | Default <= 4 MiB per stream and <= 32 MiB globally |
| Active log streams | Default <= 4 globally |
| Requests | Default <= 5 requests/second/context, burst 10; separately cap connections |

Measure startup, first useful data, p50/p95/p99 latency, RSS, CPU, dropped/coalesced updates, API request rate, reconnection behavior, and shutdown latency. Caps need visible overflow/truncation indicators; paginate lists, evict inactive caches, and stop streams rather than grow indefinitely. Tune defaults against real workloads. A small cluster passing does not establish large-cluster performance.

Test layers:

* Domain/policy unit tests for approvals, read-only restrictions, config trust and sensitive data handling.
* Property tests and fuzzing for untrusted terminal text, bounded parsers, malformed objects, config, extension protocols, and approval mismatch.
* Disposable-cluster integration tests for RBAC-denied operations, authentication expiry, custom CAs, discovery, CRDs, watch expiration, dry-run, admission changes, update conflicts and pod replacement.
* Fault injection for disconnects, 429/Retry-After, certificate errors, slow/huge responses, expired approval, failed audit storage and uncertain write results.
* Security tests with canary secrets in logs/objects, executable path hijacking, forbidden extension launches, misleading context labels, and hostile AI content if enabled.
* Terminal replay and real-platform tests for raw-mode restoration, control-character sanitization, Unicode, resizing, PTY/process-tree cleanup and forwarding lifetime.
* Load tests across concurrent contexts, sustained log floods, large lists and reconnect storms. Test caps, fairness and stale labeling, not only throughput.

CI requires format, clippy with reviewed exceptions, tests, dependency vulnerability/license checks, lockfile consistency, secret scanning, and release-build checks. Evaluate tools such as cargo-audit and cargo-deny as development tools rather than runtime dependencies. Schedule dependency review and reevaluate advisories before each release. Fuzz continuously within a defined compute budget. Release gates fail for unresolved critical/high exploitable findings; accepted lower-risk findings require owner, justification and expiry.

### Fuzzing

`src/fuzzing.rs` holds one entry point per parser that reads untrusted input. Each takes arbitrary bytes and panics when an invariant breaks, not only on a crash:

| Target | Parser | Invariants checked |
|---|---|---|
| `sanitizer` | `safety::safe_text`, `safe_lines`, `review_text`, `review_display` | no control/bidi/zero-width output, bounded, idempotent; review text is exact or refused; review display is ASCII and reversible |
| `kubeconfig` | kubeconfig text (`gateway::parse_config_text`) | names bounded, plain, unique across YAML documents; file references resolved |
| `manifest` | local manifest parse, `flatten`, `body` (apply) | no Secret survives; ≤16 objects; body keeps the file's namespace or the default only when unset; protected namespaces refused |
| `archive` | strict tar extraction (file copy) | nothing written outside the new directory, no links or special files, private modes, counts match, failure leaves nothing |
| `k9s_views`, `k9s_hotkeys`, `k9s_aliases`, `k9s_plugins` | k9s `views.yaml`, `hotkeys.yaml`, `aliases.yaml`, `plugins.yaml` import | bounded, plain text; shortcuts round-trip; aliases expand within limits; every suggested register line is one shell-quoted argv |
| `keymap`, `ui_config` | `keymap.yaml`, `ui.yaml` | keys round-trip through their names; errors are plain text |
| `secret_masked` | masked Secret YAML, PEM/DER walker, registry hosts, reveal | output depends on hidden values only through their length; notes have fixed shapes; registry userinfo never shown |
| `helm_storage` | Helm release decoding (base64, gzip, JSON, manifest YAML) | decode budget held; projections bounded and plain |

Every error message must also be plain text. The screen snapshot (`src/tui/snapshot.rs`) renders app state rather than parsing input, so it has no target.

**Always on (stable):** `cargo test` runs every target over its seeds in `fuzz/seeds/TARGET/` plus a few hundred deterministic mutations (well under a second in total), and a generated hostile-tar test.

**Coverage-guided (nightly):** `fuzz/` is a separate cargo-fuzz crate; its dependencies never reach the kborg crate. With the project-local toolchain:

```sh
rustup toolchain install nightly --profile minimal
cargo +1.99.0 install cargo-fuzz --version 0.13.2 --locked
cd fuzz
cargo +nightly fuzz run -O archive corpus/archive seeds/archive -- -dict=yaml.dict -max_total_time=3600
```

`corpus/` (grown by libFuzzer) and `artifacts/` (crashes) are ignored by git; copy a small, interesting input into `seeds/TARGET/` when it is worth keeping. Reproduce a crash with `cargo +nightly fuzz run TARGET artifacts/TARGET/crash-…`, fix it, and add a regression test next to the parser. The manual **Fuzz** workflow (`.github/workflows/fuzz.yml`, workflow_dispatch) runs every target on its own runner for a chosen time, one CPU-hour by default, and uploads any crash.

## 11. Production rollout and maintenance

Release artifacts include checksums, signatures with documented trust/bootstrap, SBOM, provenance, licenses, changelog, supported platform/API matrix and installation/uninstallation instructions. A checksum hosted beside a binary detects corruption but alone does not establish publisher authenticity. No automatic updates while operating a cluster. Protect release credentials; separate build permissions from publishing permissions and review signing-key recovery.

Pilot stages: demo -> disposable cluster -> staging with least-privilege identity -> production read-only -> small explicitly authorized write pilot -> broader rollout. Require independent review of auth/TLS, mutation boundaries, extension execution, and any AI isolation before exposing them to production.

Publish minimal RBAC examples per feature and namespace; avoid cluster-admin, wildcard rules and secret access. Kubernetes RBAC cannot express every semantic safety rule, so use admission policies and organizational controls for necessary server-enforced restrictions. A local policy file controlled by the same operator cannot enforce an organizational prohibition alone.

Operational runbooks cover lost authentication, wrong-context prevention, stale data, uncertain mutation result, audit destination failure, hung subprocess, leaked credential revocation, forwarding cleanup, emergency disabling of integrations/AI, release rollback, and config migration recovery. Rolling back the client does not roll back cluster changes.

Maintain SECURITY.md and a private vulnerability-reporting channel, assign a security owner, define response targets (triage within 2 business days; urgent mitigation immediately after validation), and support the latest stable release plus the previous minor for a defined transition period. Final support windows must be approved before v1. Monitor upstream Kubernetes compatibility, dependency advisories and platform behavior. Telemetry is off by default and opt-in diagnostics contain no object bodies or tokens.

## 12. Implementation checklist and review gate

- [ ] Confirm reference repository and freeze initial feature inventory.
- [ ] Approve target platforms, Kubernetes versions and authentication providers.
- [ ] Define local vs server-enforced policy and least-privilege identities.
- [ ] Record dependency exceptions and select transport/terminal through spikes.
- [ ] Prove policy rejection at every execution entry point.
- [ ] Prove context cancellation and object preconditions prevent target confusion.
- [ ] Verify Secret exclusion and control-sequence filtering in all sinks.
- [ ] Validate auth helper trust, TLS/proxy behavior and permissions on each platform.
- [ ] Prove all caches, parsers, streams, queues and subprocesses remain bounded.
- [ ] Meet measured performance budgets or document accepted changes with evidence.
- [ ] Complete compatibility/fault/security tests and independent sensitive-code review.
- [ ] Validate release authenticity, operational drills and staged rollout.
- [ ] If AI exists, prove data policy and OS isolation independently before activation.

## 13. Fresh architecture/development review

Review method: reassess the drafted content from the perspective of an implementer and production security reviewer, challenge trust-boundary claims, and trace each risky feature to an enforcement point and acceptance test. This is a separate self-review, not an independent third-party audit. Local shell and file-read verification were unavailable during authoring because the execution sandbox failed to initialize; file edits were applied through the patch tool.

Findings incorporated into this version:

1. **High: read-only does not make credential helpers harmless.** Authentication helper execution is now an explicit trust boundary. Privileged external tools are blocked in managed read-only profiles.
2. **High: process separation alone does not isolate AI or plugins.** OS sandbox evidence is required; strict deployments disable these features if isolation cannot be established.
3. **High: confirmation can approve a stale or different target.** Approval now binds session, target and parameters; each mutation uses its own supported concurrency controls.
4. **High: local policy and audit cannot constrain a hostile local operator.** Server-side identity/admission controls and protected external audit are explicitly distinguished from UX safeguards.
5. **Medium: zero dependencies encourages unsafe protocol reimplementation.** Native-first policy now includes reviewed exceptions for transport, serialization and terminal APIs.
6. **Medium: generic terminal integration has unbounded scope.** Handoff precedes embedding; platform compatibility and privileged credential behavior have explicit contracts.
7. **Medium: performance goals without workloads are unverifiable.** Fixtures, hardware, stream caps, metrics and gates are specified as targets to validate.
8. **Medium: dry-run and client rollback can imply false safety.** Their limits and uncertain-result reconciliation are explicit.
9. **High: an unclassified cluster could receive permissive development settings.** Unknown clusters use the strict production profile. A context name alone never determines trust; classification binds server endpoint and CA trust information. Moving to a less restrictive profile is an explicit reviewed action. Managed policy distribution/authenticity is a Phase 0 decision and remains subordinate to server-side enforcement.
10. **Medium: response caps after parsing do not prevent allocation attacks.** Enforce compressed and decompressed byte limits while receiving data, before deserialization, alongside nesting/field limits and bounded list pages. Include oversized compressed responses in fault tests.
11. **Medium: a canceled helper or subprocess may retain descendants.** Define platform-specific process groups/job objects and kill semantics; test descendant cleanup. Network requests and helpers need their own deadlines, not only a UI cancellation flag.

Verdict: suitable as a development baseline, not a production security certification. Before implementation commitment, resolve reference/version inventory, exact supported Kubernetes versions, auth-provider matrix, managed policy distribution, platform sandbox choices, and centralized audit requirements in Phase 0. They do not block the credential-free demo prototype. Production deployment remains gated on empirical tests and independent review.

## 14. Primary sources and verification notes

Sources consulted 2026-10-09. Project-specific architecture, limits, phases and controls above are recommendations, not guarantees made by these sources. Pin dependency versions and verify actual behavior during spikes.

* [k10s reference repository](https://github.com/p10node/k10s) — feature inspiration; inspect its license before reusing code/assets.
* [Kubernetes security checklist](https://kubernetes.io/docs/concepts/security/security-checklist/) — baseline for cluster controls; a checklist alone is insufficient.
* [Kubernetes authentication](https://kubernetes.io/docs/reference/access-authn-authz/authentication/) — authentication mechanisms, including client credential helpers.
* [kube-runtime watcher documentation](https://docs.rs/kube-runtime/latest/kube_runtime/watcher/fn.watcher.html) — candidate watch adapter to validate against required semantics.
* [Ratatui](https://ratatui.rs/) — optional TUI implementation candidate, not a selected requirement.
* [Kubernetes RBAC good practices](https://kubernetes.io/docs/concepts/security/rbac-good-practices/) — indirect escalation paths and least-privilege design.
* [Kubernetes Secrets good practices](https://kubernetes.io/docs/concepts/security/secrets-good-practices/) — secret access and workload-related exposure.
* [Kubernetes auditing](https://kubernetes.io/docs/tasks/debug/debug-cluster/audit/) — audit policy and backend configuration.

## 15. Security review amendments — mandatory implementation requirements

Reviewed: 2026-10-09. These requirements refine the earlier sections and take precedence over broader feature descriptions. This review assesses the proposed design; no implementation exists here to penetration-test. It is a second self-review, not independent assurance. The sandbox continued to prevent local file readback, so amendments were reviewed against the drafted content retained in the conversation and applied through contextual patches.

### 15.1 Privilege escalation and sensitive reads

Kubernetes permissions must be assessed by their consequences, not only their verb. Creating or modifying workloads may expose namespace Secrets or service-account privileges. Assign a security owner to review each operation family against [Kubernetes RBAC guidance](https://kubernetes.io/docs/concepts/security/rbac-good-practices/).

* Deny generic workload creation/editing in the initial production profile. If later enabled, review changes to image, command, environment, volume mounts, serviceAccountName, host namespaces, hostPath, privilege/capabilities and security context. Enforce organizational restrictions through admission and restricted identities; UI warnings cannot replace these controls.
* Deny application workflows for impersonation, RBAC bind/escalate, service-account token issuance, certificate approval, node proxy, pod proxy, ephemeral containers and privileged debugging by default. If a feature needs one, it requires a separate capability, threat model, minimal RBAC and independent review.
* Treat custom-resource mutation as unknown-impact until a controller-specific policy exists. A seemingly harmless field can make a controller create privileged resources or call external services.
* Read-only describes cluster mutation restrictions, not data confidentiality. Pod logs, events, ConfigMaps, annotations, environment literals and CRDs may contain sensitive business data. Permit them by capability and namespace; mask configured sensitive fields before constructing generic views, exports or snapshots. Document that masking cannot identify every secret.
* Secret metadata browsing is optional and separately authorized. If implemented, explicitly request metadata-only representations and reject a full-object response before exposing it to generic state. Do not silently fall back to ordinary Secret list/watch. RBAC list/watch rights can reveal Secret contents to other clients regardless of our metadata request; strict identities should omit those rights. Prefer an explicit single-object metadata request where organizational policy permits it.
* Model deletion consequences: namespace deletion, cascading owner deletion, PVC deletion and force/finalizer changes have distinct risk. Block namespace deletion, bulk deletion, force deletion and finalizer removal initially. Never add force or zero-grace flags implicitly. Show propagation/grace semantics on allowed deletions.

Acceptance: tests prove forbidden operation families cannot execute through CLI, UI, extensions, replayed plans or generic gateway calls; fixtures contain sensitive data in non-Secret resources; deletion tests verify exact propagation and grace semantics.

### 15.2 Transport, authentication and credential routing

Build separate HTTP clients and credential stores for each cluster session and for any AI provider. Never reuse a cluster-authenticated client for arbitrary URLs. Disable automatic API redirects by default; any required redirect must stay within an explicitly approved origin and must not forward credentials to a different origin. Discovery/object fields and AI output must never choose authentication destinations.

In the production profile require HTTPS, hostname validation and trusted CA validation; handle client certificates and explicitly configured TLS server names through reviewed identity settings. Review proxy configuration and credential handling rather than blindly inheriting ambient proxy variables. Preserve legitimate private/loopback API endpoints through explicit cluster configuration, not a universal public-address rule. Network policy must independently constrain AI egress.

At import, enumerate kubeconfig files and merged entries, referenced certificate/key/token files, helper executable and arguments, helper environment, server endpoint, TLS identity and proxy. Avoid resolving relative helper commands through an untrusted working directory or PATH. Credential helper output, stderr, errors and refresh responses must be size-bounded and redacted. Do not put tokens in argv. Prefer short-lived identities; validate expiration and isolate refresh so concurrent views do not launch many helpers.

Capability checks are advisory and may become stale. Reevaluate application policy at execution and after policy/identity changes; the API server authorizes every actual request. Authorization failure cancels relevant streams and clears protected cached content. Maintain a bounded retention window for cached authorized reads, clear on identity change, and document that already displayed data cannot be remotely revoked. Avoid claiming immediate revocation from a long-lived watch; use bounded stream renewal where policy requires it.

Acceptance: a test server attempts cross-origin redirects and returns malicious URLs; no credential leaves its configured origin. Exercise custom CA/server-name configurations, changed identities, expired credentials, changed RBAC, oversized helper output and concurrent refresh.

### 15.3 Approvals, noninteractive plans and crash recovery

Approval is single-use, short-lived, and stored in memory. Use a monotonic clock for expiry. A failed or uncertain execution consumes approval; retries need outcome reconciliation and fresh review. Serialize approval consumption with execution so concurrent inputs cannot submit the same approved action twice. Policy changes, credential identity changes and context-generation changes invalidate pending approvals.

A saved CLI plan is an untrusted proposal, not authorization. Its schema includes explicit target identity, operation type/parameters, preconditions, schema version and creation time. It contains no credentials and grants no general API request capability. Applying it revalidates schema, current policy, target and state. Interactive execution displays it and requires new approval. Initial noninteractive production writes are disabled; future automation requires a separately reviewed automation identity/policy and authenticated approval mechanism. Never accept an arbitrary plan hash or a '--yes' flag as proof of organizational authorization.

Protect output files and any approved temporary editor files against symlink/reparse-point attacks and unintended overwrites. Create files atomically with restrictive permissions/ACLs, verify the destination and owner, limit size, and delete temporary content after use. Avoid sensitive exports by default. Reliable secure erasure is not assumed on SSDs or copy-on-write filesystems. External editors and pagers can retain history, swaps and backups; allow sensitive content handoff only under a reviewed data policy.

Crash-recovery records hold minimal intent/target/outcome metadata, not object bodies or reusable approvals. Recovery never automatically resumes writes. Query current state, label ambiguity and require an operator decision. If protected audit is mandatory, define whether acceptance means durable local spool or acknowledged remote delivery; deployment policy must choose explicitly. Neither provides an atomic transaction with the Kubernetes API. Test the crash window between API execution and outcome recording.

### 15.4 Terminal, local service and supply-chain hardening

Render all untrusted text as text. Filtering must cover C0/C1 controls, ESC/OSC/DCS/APC sequences, BEL, carriage-return overwriting, hostile hyperlinks and misleading bidirectional text. Allow only internally generated styling sequences. In target confirmations show a safe escaped form for ambiguous/control characters and the canonical target separately. JSON output uses proper serialization; plain diagnostics never print raw server/helper/plugin messages. Auth-helper terminal prompts are trusted helper handoffs, not sanitized cluster text.

Do not expose a local HTTP service, IPC command listener or debug endpoint by default. If later needed, require a separate threat model covering caller authentication, file/socket permissions, origin/CSRF controls for HTTP, capability checks and input limits. Loopback is not proof that a caller is trusted.

Dependency review covers build scripts, proc macros, native libraries and build-time tooling as well as runtime crates. These can execute in CI even if the application forbids unsafe Rust. PR builds run without release secrets or production access. Pin CI actions/tooling, restrict workflow permissions, build release artifacts from reviewed immutable revisions, and make package/signature verification resistant to rollback according to the managed distribution policy. A signed vulnerable older release remains vulnerable.

### 15.5 Security acceptance evidence and residual risks

Before Phase 5, maintain a security evidence register with requirement ID, owner, enforcement location, test case, artifact/result, residual risk and review expiry. Each mandatory requirement above needs demonstrated evidence rather than a checked box alone. Assign owners for the following blockers:

| Blocker | Evidence required to enable production feature |
|---|---|
| Credential-origin isolation and helper trust | Redirect/proxy/auth matrix passes on each supported platform |
| Sensitive reads and indirect escalation | Capability/RBAC mapping, approved operation families, admission policy verification |
| Mutation approval and uncertain outcomes | Single-use concurrency tests, object replacement races, crash-recovery drill |
| Audit/data lifecycle | Approved sink semantics, protected spool, retention/deletion policy, outage drill |
| External execution or AI | Platform sandbox evidence and demonstrated credential/data exclusion; otherwise disabled |
| Release distribution | Independent verification of provenance/signatures, CI isolation and rollback policy |

Remaining boundaries: a compromised operator OS can observe credentials and terminal content; an authorized workload/controller may have effects beyond a client diff; a malicious same-user process can bypass client policy; server-side audit/admission configuration is a deployment responsibility. Record these in the deployment threat model and constrain identities accordingly. Rust and minimal dependencies improve some risks but do not resolve them.

Security-review verdict: proceed with the credential-free prototype and narrowly scoped read-only implementation. Enable writes, extensions and AI only after their evidence gates pass. Do not describe the design as production-secure solely because this document has been reviewed.

## 16. Docker Desktop Kubernetes development tests

Use Docker Desktop Kubernetes for local integration and security regression tests during Phases 1–4. This is a planned test environment; Docker Desktop has not been installed or configured by this document, and no tests have been run yet. Docker Desktop and kubectl are development tools, not application runtime dependencies.

### Setup and reproducibility

1. Install an organization-approved Docker Desktop release and review its applicable license. On Windows use its supported Linux-container backend and verify virtualization prerequisites for that release.
2. In Docker Desktop's Kubernetes view, create a cluster. Prefer the **kind provisioner** where available for selectable Kubernetes versions and multiple nodes; kubeadm is a single-node fallback. Record the selected provisioner rather than assuming every installation has the same configuration. [Docker Kubernetes documentation](https://docs.docker.com/desktop/use-desktop/kubernetes/)
3. Pin the chosen Kubernetes version within our approved compatibility matrix. Start with one node; use a multi-node profile for scheduling and node-failure tests. Proposed initial resource allocation: 4 CPUs and 8 GiB RAM, adjusted to measured host capacity.
4. Record Docker Desktop version, Kubernetes version, provisioner, node count, allocated resources, OS/backend and fixture image digests with every test result. Control application fixture versions in Git; do not automatically upgrade them between benchmark runs.
5. Verify the cluster using explicit context arguments. Do not globally change the current context as part of automated setup. Docker documents the default context as `docker-desktop`; if the installation differs, validate its actual endpoint and identity before adapting commands. Never select a context by a loose name substring.

```powershell
# Local checks only; these do not change the active context.
kubectl config get-contexts docker-desktop
kubectl --context docker-desktop cluster-info
kubectl --context docker-desktop version
kubectl --context docker-desktop get nodes
```

The test harness must additionally require a dedicated test kubeconfig containing only the verified local cluster, compare its server/CA identity with the approved test manifest, and abort on mismatch. Do not log raw kubeconfig or flattened credentials. Keep test credentials outside Git with restrictive ACLs. A context name alone is insufficient protection against accidentally reaching production.

### Fixtures and credentials

Create version-controlled test manifests and a harness during Phase 1. They are development artifacts, not shipped cluster components. Use a per-run namespace such as `terminal-tests-<run-id>`, label it with the run ID, and validate the ID against a strict safe-character pattern. Seed disposable Deployments, Services, ConfigMaps, fake canary Secrets, events, a crash-loop workload, Unicode/control-sequence log fixtures, and a simple test CRD/controller where custom-resource behavior needs testing. Pin image digests; no privileged pods, host mounts, real secrets or production data.

Use the Docker Desktop administrator identity only for deliberate fixture/RBAC provisioning and cleanup. Run the application tests with separately issued short-lived identities:

* **Reader:** specific get/list/watch access within the test namespace, no Secrets, writes, exec or forwarding. Add log access only in the test cases that need it.
* **Operator:** only approved scale/restart/delete operations on disposable fixtures, with explicit resource/subresource rules. Broad workload patch rights are not inherently limited to a restart; enforce allowed changes with tested admission policy where server enforcement is required.
* **Denied identity:** no test-resource permissions, for authorization and error-handling cases.

Test RBAC-denied execution with these real identities. Impersonation using the administrator identity is useful for provisioning checks but is not a substitute for testing actual application authentication. Never grant the application cluster-admin or the Docker socket for convenience.

### Local test suite and acceptance

| Suite | Required local cases |
|---|---|
| Read/discovery | Namespaced resources, forbidden kinds, CRDs, namespace isolation, cache caps, no Secret fetch |
| Streams | Logs/events/watches, cancellation, stale labels, restart/disconnect, bounded flood handling |
| Mutations | Scale/restart/delete approvals, dry-run differences, resourceVersion conflict, delete/recreate UID race, denied actions |
| Security | Canary leaks, terminal injection, reader execution rejection, malicious config/helper fixtures in an isolated harness |
| Connect features | Explicit exec authorization, loopback forwarding, expiry, context-change and exit cleanup |
| Resilience | Cluster unavailable, credential expiry, audit spool failure, crash during mutation, unknown-result reconciliation |

Use a controllable API fixture for deterministic 429 responses, oversized compressed bodies, redirects, TLS failures and watch expiration when Docker Desktop cannot reproduce them reliably. Keep fault-injection traffic and credentials isolated from the real local cluster. Do not assume its default network plugin enforces NetworkPolicy; verify enforcement before using it as security evidence.

After each run, collect redacted results and clean up only the verified run namespace and tracked run-owned resources. CRDs and other cluster-scoped fixtures need a dedicated test cluster, explicit ownership checks and separate teardown. Never reset Docker Desktop or purge shared Docker data automatically. Any cluster reset is a deliberate developer action after checking for other local workloads.

### CI and production limits

Docker Desktop is the developer environment. CI uses disposable headless kind clusters with pinned node images on isolated runners, plus the same fixtures and acceptance suite. This adds test tooling, not a dependency on Docker in the released Rust client.

Local success does not establish production readiness. Repeat the relevant suites in staging with actual authentication helpers, admission policies, audit configuration, networking, Kubernetes versions and platform integrations. Test multi-context behavior with separate verified disposable clusters and distinct credentials. Large-cluster scale, cloud identity, managed control-plane behavior and cross-platform sandboxing require separate evidence; a Docker Desktop cluster cannot establish all of them.

- [ ] Local endpoint/CA verified; dedicated kubeconfig and short-lived identities created.
- [ ] Fixture images and environment versions recorded; synthetic data only.
- [ ] Reader/operator/denied identity suites pass without administrator fallback.
- [ ] Redacted results retained; scoped teardown succeeds without touching unrelated workloads.
- [ ] Remaining staging/CI coverage gaps recorded in the security evidence register.

## 17. KBorg identity and logo direction

Product name confirmed by the project owner: **KBorg**. Use that capitalization in documentation and the interface; use `kborg` for the executable, repository and proposed package names, subject to registry availability. The name combines K8s with Borg as a historical reference, without implying Google, Kubernetes or CNCF affiliation. Initial web searches found no obvious indexed software collision; package/domain availability and trademark clearance remain unverified.

Proposed tagline: **Kubernetes operations, under your control.** The tagline communicates deliberate operations rather than promising absolute security.

Suggested logo: an angular **B** formed from a small set of connected cluster nodes, paired with a terminal `>` cursor and the KBorg wordmark. Keep the mark compact, flat and legible at small sizes. Avoid Kubernetes' wheel, Google branding, Star Trek characters, and lock/shield imagery that could imply a security certification.

Suggested palette: teal `#2DD4BF`, graphite `#0B1220`, off-white `#F8FAFC`. These are proposed brand colors; generated artwork may approximate them. Interface status colors remain distinct: the brand accent must not make an operation look safe or obscure production warnings.

The first generated logo is a concept for review, not a final vector asset. Once the direction is accepted, create and verify SVG masters, a monochrome variant, light/dark variants and small raster exports. Test silhouette and legibility at 16, 24 and 32 pixels, and provide plain-text KBorg branding in terminals without image support. Raster generation does not establish originality or rights clearance; review the final mark before public release.

## 18. Bundled themes and easy custom theme setup

### 18.1 Default theme collection

Ship an offline theme collection inside the binary. Users can choose a familiar developer palette without downloading anything or installing a plugin. These are established theme families selected for familiarity and variety, not a claimed definitive popularity ranking. Adapt palettes to KBorg's semantic roles rather than importing executable editor/terminal configurations.

| Family | Bundled variants | Source |
|---|---|---|
| KBorg | Dark, Light, High Contrast | Original product palettes; Dark is the initial default |
| Catppuccin | Latte, Frappe, Macchiato, Mocha | [Catppuccin](https://github.com/catppuccin/catppuccin) |
| Dracula | Standard open-source dark palette | [Dracula](https://github.com/dracula/dracula-theme); exclude paid PRO variants |
| Nord | Dark | [Nord](https://github.com/nordtheme/nord) |
| Gruvbox | Dark, Light | [Gruvbox](https://github.com/morhetz/gruvbox) |
| Tokyo Night | Night, Storm, Moon, Day | [Tokyo Night](https://github.com/folke/tokyonight.nvim) |
| Solarized | Dark, Light | [Solarized](https://github.com/altercation/solarized) |
| Terminal | Host terminal colors with text/style cues | Compatibility option; actual palette/contrast is host-controlled |

Provide a family/variant picker in Settings -> Appearance and through the command palette. Display a synthetic preview containing resource rows, selection, logs, diffs, PRODUCTION/READ ONLY labels and confirmation/error states. Arrow keys preview; Enter applies and persists; Escape restores the previous theme. The same actions are mouse-accessible. Preview uses synthetic data and changes appearance only: it never reconnects clusters, changes identity, or cancels/reissues operations.

No mandatory first-run wizard. Start with KBorg Dark and make the picker easy to discover. Store the preference per user, not in kubeconfig. Support a one-session `--theme` override; explicit saved settings take precedence over optional automatic light/dark matching. Automatic matching is opt-in and falls back to the selected theme when terminal detection is unavailable or unreliable. Do not switch appearance during a pending mutation confirmation; defer until the dialog ends.

### 18.2 Semantic architecture and dependencies

Add a small `themes` module/crate with typed `ThemeId`, validated RGB values, semantic roles, a built-in registry and resolved immutable theme snapshots. Domain/policy layers do not depend on this module. Rendering receives validated styles only, never raw escape sequences or user-provided executable configuration.

Roles cover background/surface, primary and muted text, border/focus, selection foreground/background, accent, info/success/warning/danger, production banner foreground/background, stale/disconnected state, and diff-added/diff-removed foreground/background. Themes change colors and a small allowed set of styles (bold/underline); they cannot change labels, icons, layout, hotkeys, visibility, approval wording, safety capabilities or semantic meanings. Syntax colors must not override sensitive-field masking.

Use std for registry/storage paths and immutable data structures; reuse the existing reviewed JSON parser rather than adding a theme framework, CSS engine, plugin interpreter or runtime palette SDK. Built-ins are embedded, versioned assets compiled into the release. Cache resolved styles outside the render loop; load/validate user files only when requested, not on each frame.

### 18.3 Easy custom-theme workflow

Proposed commands below are interface contracts to implement, not currently available commands:

```text
kborg theme list
kborg theme use catppuccin-mocha
kborg --theme tokyo-night-storm
kborg theme new my-theme --base catppuccin-mocha
kborg theme validate ./my-theme.json
kborg theme preview ./my-theme.json
kborg theme install ./my-theme.json
kborg theme export my-theme --output ./my-theme.json
kborg theme remove my-theme
kborg theme reset
```

`new` writes a documented editable template to the user theme directory and prints its path. A matching UI action, “Create from current theme,” offers the same workflow. The user edits ordinary JSON, validates it, previews it, and installs it. Errors identify the field and correction; an invalid theme leaves the active theme untouched. `install` validates and copies a local file into the managed user directory; activation is a separate choice. Updates to an existing custom theme require an explicit replace action and preserve a recoverable previous copy. Removing the active custom theme switches to KBorg Dark. Reset changes appearance only.

Use platform-appropriate config locations: Linux `$XDG_CONFIG_HOME/kborg/themes` (fallback `~/.config/kborg/themes`), macOS `~/Library/Application Support/KBorg/themes`, Windows `%APPDATA%\KBorg\themes`. Resolve and validate these paths using the existing config rules; do not scan working directories for themes. Built-ins are immutable and cannot be shadowed by custom IDs.

Example schema-v1 custom file, based only on a known embedded theme:

```json
{
  "schema_version": 1,
  "id": "my-theme",
  "name": "My Theme",
  "base": "catppuccin-mocha",
  "colors": {
    "accent": "#2DD4BF",
    "focus": "#2DD4BF"
  }
}
```

`base` accepts an embedded ID only; no arbitrary file inheritance or recursive chains. Resolve all missing roles from that base and validate the complete result. `export` emits a complete resolved theme to make sharing stable across built-in palette updates. Document schema, role names, ID rules and examples in `docs/themes.md`; ship a JSON Schema as development/documentation support without requiring a new runtime schema library. IDs are bounded ASCII lowercase/digit/hyphen strings, with reserved built-in IDs. Names and metadata are bounded sanitized text.

### 18.4 Data-only security and readability

Limit each JSON file to 64 KiB, nesting depth and field count; reject duplicate/unknown keys, unsupported schema versions, invalid colors and unsafe identifiers. Theme files contain no scripts, shell commands, expressions, URLs, executable hooks, external includes, fonts, binary assets, ANSI sequences or filesystem paths. Installation uses protected atomic writes and rejects unexpected symlink/reparse-point destinations. Export refuses overwrites by default. Theme errors and metadata follow the existing terminal sanitization rules.

Initial theme installation is local-file only. Users may download a JSON file themselves and validate/install it; KBorg neither executes it nor follows links inside it. Defer remote catalogs, archives, dependency resolution and automatic theme updates. A future online catalog requires a separate distribution/trust design. Do not import Lua/Vim/tmux scripts even if they come from a popular theme repository.

Set a project contrast target of at least 4.5:1 for essential text against its rendered background and 3:1 for essential non-text boundaries/focus cues. Check selections, dialogs and status banners, not just main foreground/background. These are engineering targets, not a claim of terminal accessibility certification. Muted text may not carry critical operational information. Disable blinking, dimming or transparency for confirmations and safety banners. Use labels and symbols as well as color; `PRODUCTION`, `READ ONLY`, `STALE`, `DISCONNECTED`, and dangerous-operation wording remain explicit.

Preserve upstream base palettes where feasible, but adjust semantic foreground/background pairs to meet the safety targets and disclose adaptations. Unsafe custom pairs are rejected for critical roles; provide suggested repairs and a built-in high-contrast fallback. The picker shows any noncritical readability warning. Managed deployments may restrict selection to validated themes. In 256-color mode validate the mapped palette as well; in 16-color/host-palette mode the actual colors may be unknown, so never claim guaranteed contrast. Keep text and focus cues, offer monochrome/high-contrast modes, and make unsupported capabilities visible. Never alter the host terminal's global palette with OSC commands.

Respect explicit `--color=never`, noninteractive output and the conventional `NO_COLOR` setting. Interactive monochrome still shows focus and safety labels with supported non-color attributes. Theme selection never modifies JSON output. Terminal handoff restores terminal state and does not apply KBorg's palette to external programs.

### 18.5 Attribution, delivery and acceptance

Before bundling each adaptation, pin its upstream revision and exact source assets, verify their license, retain required copyright/license notices in `THIRD_PARTY_NOTICES`, and record source/hash in the asset manifest. Inspect the specific assets rather than assuming every port shares the parent project's license. Use descriptive theme-family names without implying official endorsement; do not reuse third-party logos. Theme assets update through reviewed releases, not live network fetches.

Phase 1: define roles/schema and demonstrate truecolor, 256-color and monochrome rendering. Phase 2: deliver the whole bundled collection, picker, preference persistence, template/validate/install/export/remove commands and documentation. Phase 5: ship pinned asset attribution and theme acceptance evidence.

Required verification:

- [ ] Every bundled variant resolves all roles and passes defined contrast checks for its supported fixed palettes.
- [ ] Preview/apply/cancel, restart persistence, missing custom-file fallback and active-theme removal work.
- [ ] Invalid/oversized/malicious files, path traversal, duplicate IDs and executable configuration are rejected safely.
- [ ] All variants keep production identity, forbidden/disabled actions, stale state and confirmation text legible.
- [ ] Truecolor, quantized 256-color, 16-color, monochrome, small terminals and mouse-disabled navigation are exercised.
- [ ] Color-vision simulation and real terminal review cover selection, warnings, diff views and focus; color is never the sole cue.
- [ ] Loading/installing/previewing themes causes no network traffic, cluster API calls, credential access or subprocess execution.
- [ ] Theme switching adds no network latency, stays within the cached-input response budget and does not allocate unbounded assets.
- [ ] License/source manifest, user guide and security evidence register include the theme collection.

Theme-design review: a data-only format avoids executable theme plugins; embedded assets keep startup offline; fixed safety semantics prevent themes from disguising risk. Remaining limitations are host-controlled ANSI palettes and real terminal rendering differences, which require compatibility evidence and truthful fallback behavior.

## 19. First implementation status (2026-10-09)

KBorg 0.1 is implemented as a Windows local-testing release. Read docs/IMPLEMENTATION_STATUS.md for delivered scope, test evidence and outstanding production gates. The unchecked requirements above remain the roadmap and must not be interpreted as implemented. Docker Desktop acceptance passed; production certification and complete K9s feature parity are not claimed.


## 20. Follow-up priorities (2026-10-09)

Implemented: two-second skippable ASCII logo; per-session, environment and persisted disabling; first-run theme selection with Catppuccin Mocha default; GitHub Copilot-only minimized local diagnostic export and explicit trusted CLI login handoff. Automatic AI requests remain gated, with no other provider planned. Copilot GitHub.com and Enterprise Cloud data-residency hosts are distinct from arbitrary GitHub Enterprise Server installations.

Independent agents performed developer/user acceptance, security review, locked dependency advisory audit, WSL2 testing and performance measurements. Cross-platform native builds/package automation targets Windows, macOS and Linux x64/ARM64. The source is in the private c0dewars/kborg repository. See the evidence reports rather than assuming a configured workflow has completed successfully.

Feature completion and security acceptance precede the requested modern visual redesign. Styling must keep the app lightweight, preserve explicit safety labels and confirmation visibility, and be measured against CPU/memory baselines. A 100-context test against one API server must never be described as 100 independent clusters. The full roadmap and production release gates remain in force.

## 21. Supported targets and neutral review (2026-10-09)

User support target: Kubernetes 1.34 and newer; GitHub.com and GitHub Enterprise Cloud *.ghe.com. Generate typed bindings from the minimum 1.34 schema and continue real-server minor-version tests. Initial compatibility evidence covers 1.34.11, 1.35.8 and Docker Desktop 1.36.1; read KUBERNETES_COMPATIBILITY.md for exact run scope.

A fresh independent reviewer, without prior findings, exercised CLI and Windows terminal flows and identified additional gaps. Corrections include CLI CRD lookup, read-only/unsupported mutation prompts, demo namespace fidelity and write-result preservation under stale terminal events. Keep first-look and corrective findings separate in the evidence. Visual design follows remaining feature completion, with measured lightweight budgets retained.

## 22. Watches and metrics iteration

KBorg0.2 implements bounded real watches, explicit polling fallback and pod/container/node Metrics API views. See WATCH_METRICS_IMPLEMENTATION.md and independent review/UX/performance reports for semantics and evidence. Cancellable per-view feeds, one coalesced snapshot slot, strict epochs and identity-preserving selection replace the polling-only first iteration. Core mutation authorization remains separate and unchanged. Metrics errors do not fabricate zero usage or invalidate ordinary resource data. Remaining product/security gates continue; visual redesign follows feature completion.
