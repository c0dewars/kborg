# Independent first-look review

Reviewer: independent fresh-release challenger. Date: 2026-10-09. This report was recorded before reading SECURITY_REVIEW, USER_ACCEPTANCE, ADVERSARIAL, IMPLEMENTATION_STATUS or previous reviewer conclusions. Inputs were README, Rust source, Cargo.toml, and the Windows/Linux performance measurement scripts. No network, real kubeconfig, model call, login subprocess, or cluster mutation was used. Rust source and workflows were not changed.

## Verdict

Suitable for continued isolated local testing, with meaningful security boundaries and an honest README. This is not independent evidence for production certification, compatibility with every Kubernetes 1.34+ server, native macOS behavior, or large independent-cluster capacity. No critical/high security defect was reproduced within this offline review. One CLI feature gap and three low-impact UX/demo defects were found below. Absence of a reproduced exploit is not a full security assessment.

## Independently observed defects

### P2: CLI discovery results cannot be used for arbitrary discovered kinds

Source evidence: src/gateway.rs Gateway::connect initializes kinds() (the fixed built-in set); Gateway::find only searches that set. src/cli.rs invokes discover() only in the resources command. list/get and copilot context create a new gateway and immediately call list/get without discovery. There is no CLI command combination preserving the gateway between resources and list. A live CRD or built-in resource omitted from kinds() can therefore be reported by resources but cannot be read by a subsequent CLI list/get, even with its fully qualified key. TUI R discovery has a separate working path.

Offline reproduction of the lookup boundary: `k8borg --demo --json list widgets.example.com` exits with `resource kind not available; Secrets are never exposed`. This alone does not prove presence of a CRD; the live-path conclusion is source-confirmed, not live reproduced. Fix by discovering on a missing resource key with the existing time/body budgets, or explicitly document that custom-resource reads are TUI-only. No such fix was made during the frozen release build.

### P3: Read-only and nondeployment rows still offer a deployment scale form

Native Windows PTY reproduction: fresh isolated `k8borg --demo --no-splash --color=never`, Enter to accept Mocha, press s while pods are selected. The UI shows `SCALE DEPLOYMENT ... enter replicas 0..20` despite read-only mode and a pod target. Entering 2 then Enter correctly fails with `READ ONLY: mutations are disabled; opt in with --write`. Security enforcement works, but the form is misleading. Disable/inform on the action before opening it and enforce the deployment kind in the UI.

### P3: A denied local action incorrectly labels otherwise current data STALE

Same PTY sequence above: after the read-only policy rejection, cached demo pods remain visible but the header says STALE. src/tui.rs App::receive marks stale for every Reply::Failure, including policy and input failures that do not invalidate cached API reads. The next successful poll clears it. Distinguish failed data refresh from refused local action.

### P3: Demo get ignores an explicitly requested namespace

Offline reproduction: `k8borg --demo -n other get pods api-7d4f9` returns the object with namespace `demo`. src/gateway.rs demo connect hardcodes namespace `demo` instead of honoring options.namespace, and demo raw_get additionally matches kind and name without checking ns. This does not cross a real-cluster namespace boundary: the real backend uses a namespace-qualified API. It weakens the fidelity of offline scope testing and should reject or consistently honor the requested namespace.

## Actual smoke evidence

Tested Windows release executable: target/release/k8borg.exe, 9,374,720 bytes, SHA-256 `62e0d1ace7ead86c9708db97da83b4804a540fe86370f8af2891b5b405521069`, reports version 0.1.0. It was the existing binary at the time of review; a concurrently built final artifact must be matched separately.

Settings were redirected to `.local/first-look-isolated` for CLI and `.local/first-look-pty` for TUI. KUBECONFIG was redirected to an intentionally missing path. This prevented accidental use of the operator's cluster configuration.

- --demo --json list pods: valid two-object JSON, expected Running/CrashLoopBackOff diagnostics, no styling/splash.
- --demo get secrets anything: rejected before exposing an object.
- --demo --write delete pods <name> without a terminal: rejected with noninteractive-writes message.
- theme list: 18 bundled choices, Mocha selected by default.
- --demo noninteractive TUI: actionable terminal-required error.
- copilot login host https://github.com@evil.example: rejected as unsupported before executable startup.
- Native Windows PTY TUI: fresh first-run picker, Catppuccin Mocha highlighted, Enter persisted choice, offline pods rendered, action denial worked, q restored terminal and exited 0. Default PTY was approximately 80x24; some footer/resource-key text is clipped there. No graphic screenshot fidelity claim is made from terminal output.

The native PTY launch used --no-splash, so two-second timing is source-inspected only in this review. src/tui.rs startup_animation polls input in 100 ms intervals against a two-second clock and the CLI dispatch bypasses animation. Full mouse, all themes, small-terminal resize, long-target approval and interactive demo-write execution were not exercised here.

## Security assessment from source

Read-only is the default through Connection::default. Real mutation preparation gates write mode before fetching, restricts groups and namespaced targets, blocks system namespaces, limits scale to 0..20, retains UID/resourceVersion and performs dry-run. Execution checks the immutable reviewed target/action/summary, session, preparation age, exact confirmation and consumed approval; real operations have no automatic retries and audit intent precedes the API write. Timeouts report unknown outcomes. This is strong defense in depth for the narrow implemented operations.

Real cluster connections reject insecure TLS, exec/auth-provider helpers, impersonation, explicit proxies and query/userinfo-bearing endpoints. Response bodies, object counts, pagination loops and operation time are bounded. Paths reject symlink/reparse ancestors; new output creation refuses overwrite. Secret kinds are excluded from both fixed and discovered catalogs. Display sanitation drops terminal/bidi controls; common literal secret-bearing fields are redacted. These are source observations, not live malicious-server tests in this pass.

Known limitations are correctly disclosed: redaction cannot classify arbitrary business strings/CRDs; same-user path replacement races remain; Windows ordinary configuration ACLs are inherited; audit records are operator mutable; admission and RBAC remain server responsibilities. Trusted Copilot CLI login has OS-user authority and is not an isolation sandbox. The host allowlist is GitHub.com/*.ghe.com; no AI requests or AI write capability exist in implemented commands. No production claim should be inferred from these local checks.

## Performance methodology assessment

Independently read scripts/Test-Performance.ps1 and scripts/Test-Performance.py rather than relying on report conclusions. Windows measures CLI process wall time/CPU with 1 ms sampled peak working set and explicitly names that peak a lower bound. Linux uses GNU time for CLI and procfs/PTY for TUI; it disables splash, saves Mocha and includes initial connection/render in TUI CPU. Both record a binary hash and describe warmed-cache behavior. Both 100-context scenarios use aliases to one cluster, not 100 independent clusters.

This methodology is reasonable for the described narrow scenarios. It does not demonstrate cold-start latency, 1,000-object/8 MiB worst-case rendering, large CRD discovery, sustained multi-cluster operation, latency percentiles of user actions, slow API responsiveness, or native macOS performance. Windows lacks a TUI resource measurement. Discovery caps the retained catalog after kube Discovery has fetched results, so a pathological server API surface still needs memory profiling. Linux average CPU uses configured duration rather than actual elapsed wall time, a small possible measurement error under load. No numeric performance results were independently regenerated in this pass; CLI/PTY behavior was tested, not benchmarked.

## Requirements and release limits

The core Rust terminal app, read-only default, guarded narrow writes, optional splash, Mocha onboarding and optional provider-restricted Copilot preparation are implemented. Kubernetes bindings select v1_34, but schema selection is not live compatibility proof and there is no observed server-version enforcement in this pass. Cross-platform conditional paths exist; only native Windows was exercised here. Automated AI diagnosis, watch-based updates, full metrics, simultaneous multi-context view, arbitrary editing and signed packaging are absent by design. These are incomplete production requirements, not hidden defects in this declared local-testing iteration.

## Release recommendation

Keep the local-testing designation. Reconcile the final artifact hash, explicitly document/fix the CLI discovery gap, and track the three small UX/demo defects. For any wider release, require distinct native Windows/Linux/macOS evidence and live supported-version/RBAC/admission tests, plus realistic large-object and interaction performance evidence. This independent review neither accessed nor certified those prior reports.


## Independent corrective follow-up (2026-10-09, before final scope rebuild)

This is distinct from the original findings above. The original assessment remains recorded rather than overwritten. Retested the current Windows candidate, 9,389,568 bytes, SHA-256 `0f49247822362014d2d0faf92fd84f13194e39727a5b382b60929b30f370ece1`. Did not read other reviewer conclusions. No Rust edits or network/cluster requests were made by this reviewer.

- P2 CLI discovery: source now calls discover_if_needed for list/get and Copilot context; it validates syntax, rejects Secret keys and uses bounded gateway discovery. Offline unknown-key reproduction now reports `resource kind unavailable after bounded discovery`. The control-flow defect is corrected by source inspection, while successful real CRD use still needs the separate live test.
- P3 misleading scale form and false STALE after local denial: independently retested native Windows PTY. In read-only demo on pods, s now immediately shows `READ ONLY: enable --write before requesting a mutation.` without opening a form or marking STALE. In --write demo on pods, s immediately says `Scale applies to deployments; select a deployment.` with no form or false STALE. Both sessions exited cleanly with q. These findings are closed on this candidate.
- P3 explicit demo namespace: the exact original CLI reproduction still returned the demo pod: `--demo -n other get pods api-7d4f9`. Although raw_get/logs now correctly check ns, demo connect still hardcoded `namespace: demo` and discarded options.namespace. Reported this remaining cause to the root agent before final rebuild. Keep this finding open until the exact CLI reproduction fails or yields the requested scope.

The mouse/write-inflight and stale-epoch write-result corrections were not interactive reproduced by this reviewer; do not attribute their regression-test evidence to this independent PTY pass.

## Final exact CLI closure (2026-10-09)

Independently verified the rebuilt release executable SHA-256 equals `23c6695f76be3d269ebc2d292736aa84f85d7722b34f34046997467956b815be` (root identifies source commit 99e32c5). No PTY or long-running process was opened for this final check.

Exact original reproduction `--demo -n other get pods api-7d4f9` now fails with exit 1 and `demo object not found`. `--demo -n other --json list pods` returns an empty items array with exit 0. `--demo -n demo get pods api-7d4f9` succeeds with the expected demo pod. Wrong-namespace logs and missing-pod logs both fail with exit 1. The original demo scope defect is closed on this hash.

All original reproduced defects have now received a correction: the two UI defects were independently PTY-verified on the preceding 0f492478 candidate; the final namespace behavior is exact-command verified here; CLI discovery's control flow is independently source-verified. The root reports separate live CRD and supported-version tests, but this reviewer did not run/read those tests and does not claim them as independent evidence. Preserve the original local-testing verdict and the stated coverage limitations.
