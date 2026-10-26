# Release-next independent security and performance review

Reviewed 2026-10-26. Scope: the current integrated working tree on `release/next`, based on `adb067e`, with startup connection consent and guided Copilot onboarding integrated. This is an independent agent review and local validation, not an external audit or production certification. Final commit identity and combined acceptance belong to the release record.

## Findings corrected in this pass

1. **Operator authentication-helper denial could downgrade trust (security).** `allow_auth_helper:false` previously cleared the reviewed-mode selector while the default kubeconfig path still executed native helpers. A separate monotonic `deny_auth_helpers` flag now rejects configured ExecCredential and legacy auth-provider identities before client construction or helper execution. Selecting reviewed mode is preserved. Static certificate/token identities remain available. Regression fixtures use a helper that would create a sentinel file; denial and a changed post-consent configuration must leave it absent.
2. **Startup approval needed to bind the later connection (security).** The local preview and worker otherwise reread kubeconfig at different times. The catalog now carries an internal SHA-256 fingerprint of the canonical parsed, merged configuration. The approved context is pinned and the worker compares the fingerprint against the same parsed object it uses to construct its client, before authentication. Changes to endpoint/helper/credentials in kubeconfig require a fresh review. The digest and credential-bearing serialized content are never logged. External referenced token/certificate file contents can rotate; this is a kubeconfig identity binding, not an immutable filesystem snapshot.
3. **Navigation shortened reported benchmark samples (measurement integrity).** The benchmark reused `seconds` for navigation-step latency, so a requested 10-second dashboard sample could last roughly one second. Step latency now uses its own variable. Reports record actual sampled duration and sample count, and a real local PTY regression test proves navigation cannot shorten the requested idle window.
4. **Missing 5,000-object fixture could silently pass the gate (measurement integrity).** A missing scale marker now fails the gate instead of returning a skipped success. A regression test injects the missing marker and verifies exit status 1.
5. **Authentication recovery guidance was unnecessarily manual (UX).** Expired/missing credentials now direct users to rerun their existing cluster login CLI (for example `orbit-use`) in the same terminal and retry; the application reuses that kubeconfig. No manual token-copy instruction or claim that login bypasses organizational policy is introduced.

## Static boundary checks

The startup prompt occurs before the worker is created. Its local catalog does not construct a Kubernetes client or execute authentication helpers. Explicit Yes permits connection; No selects another existing context; cancellation returns before worker startup. Demo mode remains offline. Final integrated PTY acceptance passed the five cases recorded below.

The Copilot setup path is distinct from model requests. Account login can require a browser/device-code flow and subscription/enterprise entitlement. A successful login alone does not establish that in-app AI containment is supported on Windows, Linux/WSL or Intel macOS. Requests must retain exact data preview, transfer approval, credential separation, cancellation and the validated platform boundary. The dedicated Copilot reviewer owns the final setup implementation; this report does not claim a real enterprise login or model answer was exercised.

`#![forbid(unsafe_code)]` remains active. Existing transport response/decompression, log, discovery, cache and write-approval limits remain in place. The runtime's current operator/quick-confirm default is a deliberate branch behavior, not the earlier read-only default. User-facing docs must state it accurately. Operator configuration is a local client policy, not a substitute for server RBAC/admission or OS-level restrictions against running a different client.

## Validation evidence

- Benchmark-harness regressions: two tests passed in native WSL Python using only a local PTY and mocked gate failure; no cluster was contacted.
- Final security and performance measurements are appended below. Earlier green CI source `adb067e` does not certify later uncommitted changes.

## Remaining acceptance limits

- Existing budget ceilings are machine-tolerant absolute bounds, approximately 2.5–3 times the recorded baseline with timing floors. They are **not** the roadmap's same-hardware 15% regression comparison.
- The gate covers demo startup, 5,000 synthetic Pods and a demo dashboard. It does not measure real 100-cluster connections, production network churn, 24-hour soak, key-to-frame p95/p99, all terminal emulators or screen readers.
- Windows owner-only ACL/handle-race validation, external protected audit, publisher signing/notarization, enterprise Copilot acceptance and independent audit remain separate production gates.
- Default kubectl-compatible kubeconfig authentication trusts configured external credential helpers, proxy and TLS options. The new prompt makes that connection explicit; it does not sandbox arbitrary company helper programs. Reviewed-helper mode remains available, and configured helper denial is enforced independently.

## Additional login and compatibility review

The final account-only login handoff validates the host and bounded account names, displays the reversible executable path for trust approval, checks native executable headers and SHA-256, and rechecks the executable immediately before launch. It strips token/cloud-provider/debug variables, uses a private working directory, and bounds its direct child to five minutes with kill/wait cleanup. This is an explicitly trusted official GitHub CLI handoff with user permissions, not OS containment of that CLI or the browser. It does not read Kubernetes resources or send a model prompt. Real GitHub.com and Enterprise Cloud device authorization and entitlement still require acceptance by the account owner.

Existing filesystem safety rejects symbolic links and Windows reparse points in kubeconfig paths or their ancestors. Corporate login CLIs that publish kubeconfig through such paths are not plug-and-play without selecting an approved regular-file path; do not weaken this trust boundary implicitly. Filesystem replacement/handle races remain a separate platform-hardening acceptance item.

## Final integrated measurements (2026-10-26)

Measured the final local release binary on Ubuntu 26.04 / WSL2 x86_64, Rust 1.99.0. The binary is 21,591,504 bytes (20.6 MiB), SHA-256 `f9209d9aa038cb051f0b6abc513d459eb2b9f750f2de86820e3f4d8ceaa54793`. It includes the uncommitted integrated branch changes after `adb067eff43a210f109c8734f42315d5af104b7d`, including the final offline Copilot-preview correction. [Source fingerprints and provenance](evidence/release-next-perf-2026-10-26/provenance.json) identify the reviewed files. Base-commit CI alone does not validate these changes.

The corrected offline gate ran three samples per scenario, each with a requested 10-second idle window, 130x40 PTY, splash disabled and the gate's fixed Flexoki Dark theme. All nine configured budget checks passed. Results below are medians across runs; CPU 0.0% means rounded below the measurement resolution, not literally zero work. This shared developer-machine measurement is not a controlled comparison with another product. The Catppuccin Mocha feature tour is a separate visual test.

| Offline scenario | First data | Median RSS | Average CPU |
| --- | ---: | ---: | ---: |
| Demo startup | 53 ms | 12.1 MiB | 0.0% rounded |
| 5,000 synthetic Pods | 181 ms | 121.5 MiB | 1.8% |
| Demo context dashboard | 53 ms | 13.4 MiB | 0.0% rounded |

The largest observed scale-run RSS peak was 167.6 MiB. Dashboard navigation to context overview measured 59 ms median. These are marker-based observations, not key-to-frame p95/p99. Network throughput is unavailable in this Linux harness. [Full measurements](evidence/release-next-perf-2026-10-26/gate.json) retain individual run metrics and sampled duration/count fields.

Final-source `cargo clippy --locked --all-targets -- -D warnings` and release compilation passed in WSL. Focused security regressions passed 2/2 (configured authentication denial and startup fingerprint binding, including no-helper sentinel), and guided Copilot setup tests passed 5/5. Earlier Windows focused authentication tests also passed 2/2. The two standalone benchmark-harness tests passed, and the seven authentication/error-guidance tests passed in WSL. [Final validation log](evidence/release-next-perf-2026-10-26/validation.log) contains the final Clippy/build/focused-tests/gate output. The root reviewer separately owns broad Windows tests, integrated terminal acceptance and final combined release decisions.

Final full offline WSL library run: **648 passed, 0 failed, 8 explicitly ignored** (5.10 seconds). The two benchmark-harness regressions were rerun afterward and passed (4.273 seconds). No ignored live integration test was enabled. [Full WSL test log](evidence/release-next-perf-2026-10-26/wsl-library-tests.log).

## Integrated Windows and startup acceptance

The final Windows offline suite passed 613 library tests plus the metrics contract (2 library tests explicitly ignored). Native Windows x64 release compilation passed. Formatting and whitespace checks passed.

[Startup consent acceptance](evidence/connection-startup-consent.json) passed five synthetic WSL PTY cases against the measured release binary: cancel the current context, select another context and cancel, approve the current context, approve another context, and reject kubeconfig replacement between review and approval. No helper or connection occurred before approval; only the approved context's helper ran; the application never changed the kubeconfig. The replacement case displayed restart guidance and executed no helper. These fixtures do not prove real company-provider login or browser OAuth.


## Final offline terminal acceptance

The final Linux x86_64 release binary passed three WSL PTY sessions covering 63 asserted frames, including demo Copilot local preview/send denial, read-only action denial, metrics, logs, navigation, narrow layouts, operator-disabled Helm hooks/CRDs, strict painted-review coverage, and simulated Helm lifecycle/schema actions. [Terminal acceptance evidence](evidence/terminal-acceptance.json) records binary SHA-256 `f9209d9aa038cb051f0b6abc513d459eb2b9f750f2de86820e3f4d8ceaa54793`. All sessions restored terminal state; the two-second quiet interval emitted 0 bytes.

The harness now explicitly selects read-only and strict-confirmation fixture policies instead of relying on earlier defaults. Dynamic demo metrics are checked for positive bounded CPU/memory values, numeric resource percentages, and `-` for the unmeasured worker; it no longer requires one fixed phase of the synthetic usage wave. No runtime code changed in this acceptance pass. These are offline synthetic UI checks, not real company login, browser OAuth, cluster/registry/Helm execution, model inference, or a native-terminal/platform matrix.
