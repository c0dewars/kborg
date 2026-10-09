# Independent developer and end-user acceptance

Review date: 2026-10-09. Reviewer: separate release-validation agent. This is an independent implementation review within the development team, not an external certification or a representative human usability study.

## Checks executed

Windows x64 optimized binary, PowerShell pseudo-terminal at approximately 80 columns and 24 rows. Test configuration lives under ignored `test-results/user-acceptance/`; no production credentials or cluster writes were involved.

| Check | Result | Evidence and limits |
| --- | --- | --- |
| Offline demo with missing kubeconfig | PASS | Two sample pods; parseable JSON; no ANSI ESC in JSON. Offline mode did not require credentials. |
| Unknown global option | PASS | Nonzero exit with `unknown option` diagnostic. |
| Noninteractive mutation | PASS | Nonzero exit; explicit diagnostic that terminal confirmation is required. |
| Theme lifecycle | PASS | Create from Mocha, select, export, validate, install, select exported theme, remove and reset completed in an isolated configuration. |
| First UI start with `--no-splash` | PASS | Immediate theme chooser, Mocha selected; Esc retained and persisted Mocha; demo resource list appeared; q exited. |
| Subsequent UI start | PASS | Existing preference retained; no first-run picker; demo list appeared. |
| Animated logo | PASS | ASCII wheel/logo and moving spinner rendered, then resource view appeared. Implementation deadline is two seconds; exact wall-clock duration was not measured by this pseudo-terminal test. |
| Animation preference | PASS | `startup off` persisted false; selecting Dracula preserved false; `startup on` persisted true. |
| Terminal cleanup | PASS for normal exit | Observed alternate-screen exit, mouse tracking disabled, cursor restored, process exit 0. Panic/forced-kill recovery and all terminals remain separate gates. |
| Navigation | SMOKE | Tab/Enter/Esc and help dialog accepted input. No claim of exhaustive key/mouse/resize coverage. |
| Packaging script | Syntax PASS | PowerShell parser reported no errors. End-to-end native packaging is enforced by the six-target workflow. |

`test-results/user-acceptance/cli-smoke.json` records the initial pre-startup-change CLI checks and binary hash. Terminal checks above used the release rebuilt after startup changes. Ignored reports are local evidence and are not distributed as signed attestations.

## Developer review findings

- Release checks use locked Cargo dependencies and pinned Rust. Action commit hashes were verified against official action repositories. Minimal permissions and no checkout credential persistence reduce the release workflow's credential exposure.
- Each native target must pass tests and offline JSON/theme smoke before an archive is uploaded. Linux/macOS use tar.gz so executable permissions survive packaging. Windows uses ZIP. No cross-compiled binary is labeled as natively tested.
- The tag-only aggregation job requires all native jobs to succeed, validates the version and produces a draft. Checksums and build metadata identify the artifact and source commit; binaries are unsigned. Signing and notarization remain unfulfilled release gates.
- Demo and theme commands support useful local testing without Kubernetes access. Normal data browsing remains read-only by default and noninteractive write commands fail closed.
- A 100-context file or synthetic API benchmark is not evidence of 100 independent clusters operating concurrently. The current product opens one active context. Scale-test reports must identify the topology and workload explicitly.

## User-visible limitations and remaining acceptance

At 80 columns several long resource kind names and the bottom deletion hint truncate; selection and safety flags remain visible, but an untrained user could miss the full shortcut. The first-run picker uses the same general theme-picker heading rather than a distinct welcome heading. These are polish issues, not evidence of completed accessibility validation.

The onboarding picker displays roughly 17 of 18 themes at a 24-row terminal; arrow navigation remains available for the final item. Retain visible selection during scrolling and verify tiny terminals, resizing and 16/256-color environments before declaring broad compatibility. Conservative ASCII substitution prevents complex-width layout errors but reduces international text readability.

Required before a production-readiness claim:

1. All six remote native build results and artifact installation checks on the actual operating systems; verify macOS Gatekeeper behavior and document unsigned-package installation.
2. Real human acceptance on Windows Terminal, Linux terminals, macOS Terminal, SSH, tmux and screen; keyboard-only and color-vision/accessibility tests.
3. Live least-privilege Kubernetes acceptance after final security changes, including admission, transient faults, expired credentials and stale mutation targets.
4. WSL 2 Linux execution plus independently recorded CPU, memory, latency and resource-size measurements; specify whether Docker Desktop integration is active.
5. Optional Copilot login and prompt-context consent tests using real authorized GitHub.com and configured enterprise identities, without leaking Kubernetes tokens or secret/log data. Unavailable credentials/subscriptions mean this remains unverified.
6. External security review, threat-model validation, fuzz/adversarial testing and production RBAC/admission/audit controls from `SECURITY.md` and the development guide.

The checks here support a local-testable build. They do not justify removing the production gates or claiming full K9s parity.
