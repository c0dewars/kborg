# Mac v1 hardening acceptance

Validation date: 2026-10-23, macOS ARM64. This batch targets soak testing, terminal compatibility and security review. The Rust source is frozen in [the source manifest](evidence/mac-v1-hardening-source.json); the tested release SHA-256 is `32a6b514bce9ab3edf247248c91e84abbce363b82980a06a6b440e9450bd4cd5`.

## Completed checks

| Check | Result | Evidence |
|---|---|---|
| Locked offline native release build | Passed | [Build log](evidence/mac-v1-hardening-release.txt) |
| Formatting and strict all-target Clippy | Passed | [Formatting](evidence/mac-v1-hardening-fmt.txt), [Clippy](evidence/mac-v1-security-clippy.txt) |
| Full serial Rust suite | 411 passed, 0 failed, 18 ignored | [Test log](evidence/mac-v1-hardening-tests.txt) |
| Existing terminal regression | 63 states passed; terminal restored | [Report](evidence/mac-v1-hardening-terminal.json) |
| Terminal compatibility matrix | 10 profiles, 71 checks and 50 resize transitions; 2 TERM=dumb checks | [Matrix](evidence/mac-v1-terminal-compatibility.json) |
| Real isolated tmux 3.8 | 5 checks passed | [Matrix](evidence/mac-v1-terminal-compatibility.json) |
| Live Unicode approval review | 4 checks passed on Docker Desktop; review cancelled without forwarding | [Report](evidence/ui-mac-v1-hardening-final/unicode-review/report.json) |
| Deterministic HTTPS fault scenarios | 9 passed | [Fault report](evidence/mac-v1-transport-faults.json) |
| Focused security boundary review | 96 tests passed, including 32 approval-focused tests | [Review](MAC_V1_SECURITY_REVIEW.md) |
| Refreshed RustSec audit | 203 packages, 0 known vulnerabilities, 0 warnings | [Audit provenance](evidence/mac-v1-dependency-audit-provenance.json) |
| Explicit Docker Desktop live regressions | 6 ignored live tests passed across three suites; scoped fixtures cleaned | [Live report](evidence/mac-v1-live-regressions.json) |
| Live YAML editing | 1 ignored Rust test plus 6 UI/CLI checks passed; scoped fixtures cleaned | [Rust report](evidence/mac-v1-yaml-live.json), [UI report](evidence/ui-mac-v1-hardening-final/yaml/report.json) |
| Export approval before/after | Previous binary reproduced hidden approval; corrected binary passed 7 PTY checks | [Regression](evidence/mac-v1-export-approval-regression.json), [Final UI report](evidence/ui-mac-v1-hardening-final/exports/report.json) |
| Package provenance rejection | 9 checks passed with isolated Git/tool fixtures | [Negative tests](evidence/mac-v1-package-negative-tests.json) |
| Fresh native unsigned package | 8 archive/content checks; extracted binary passes 63 PTY states, 10 terminal profiles, tmux, 9 TLS cases and 7 export checks | [Package acceptance](evidence/mac-v1-package-acceptance.json) |

The focused security logs were collected during review; the final 411-test suite revalidates those cases after all corrections. Their overlapping counts are not added together. Seven of the 18 normally ignored Rust tests were separately invoked with explicit live fixtures. They exercised namespace-scoped mutations, RBAC/admission conflicts, watch revocation, native exec/attach/forwarding, reviewed authentication helpers and YAML editing. Their counts are reported separately from the ordinary suite. No Copilot account test, node drain or Helm lifecycle mutation suite was repeated in this batch.

The native package executable is separately identified as `36f6eeecba162df7e687e55425eacec19b9542a37347e9cea3a95e75caf32def`. All 70 frozen source files match, but its bytes differ from the binary used for the cluster soaks. Its own terminal/export/TLS reports bind that exact packaged executable; they do not claim an exact-package two-hour soak. The normal local launcher points to the corrected `32a6b514…` executable used by the cluster runs.

## Continuous cluster soak

The final two-hour runs are in progress on Docker Desktop Kubernetes 1.36.1 and disposable minikube Kubernetes 1.35.9. Their [soak guide](MAC_V1_SOAK.md) records the exact workload and acceptance budgets; final results will replace this in-progress statement after both runs and cleanup complete. Earlier 60-second smokes passed and are harness checks, not a substitute for the requested continuous duration.

## Security and UI corrections

Review found two medium-severity issues and one low-severity issue. The approval display-integrity issue came from the ASCII-only TUI renderer converting non-ASCII characters to `?`, so distinct reviewed values could look identical. All approval presentations now escape non-ASCII values and literal backslashes before wrapping and counting displayed lines. The original operation bytes are preserved; an expanded review over 1 MiB cannot be approved. Coverage includes mutation/session, YAML/node, Helm, export and Copilot transfer reviews.

The export confirmation also omitted the renderer's minimum terminal dimensions. At 20×100, a review could satisfy the line-count check while only the resize notice was painted. Approval now requires at least 28×10, and export, YAML/node, Helm and Copilot confirmations refuse unknown terminal dimensions instead of assuming 80×24. The actual PTY Enter regression reproduced file creation on the previous binary before the fix; the corrected binary passes all seven export PTY checks and the focused event-handler regression, including a valid save after restoring a usable screen size.

The packaging provenance issue allowed caller-supplied commit attribution for an existing binary and unconditionally claimed unit tests had run in CI. The packager now verifies actual clean HEAD, checks all recursively packaged inputs are tracked and unlinked, builds into a fresh native target directory, checks the exact application version, binds the copied binary hash, and lists only performed smoke checks. It removes macOS filesystem metadata from archives. These are unsigned local source/build records, not authenticated provenance or a bit-for-bit reproducibility claim. This internal review does not constitute an external security audit.

The saved-selection picker now keeps `Enter open | d remove | Esc back` inside its own frame, so a background status update cannot hide the available actions. The quiet header, continuous borders and existing dashboard layout remain intact.

![Saved selection controls](evidence/ui-mac-v1-hardening-final/terminal/truecolor-saved-picker.png)

![Live escaped operation review](evidence/ui-mac-v1-hardening-final/unicode-review/unicode-forward-review.png)

These are renders of the running binary's captured PTY cells. They establish application layout and content, not native font rendering. The [terminal guide](MAC_V1_TERMINALS.md) links the narrow blocked review, ASCII and monochrome screenshots and documents the full matrix.

## Fault-test scope

The isolated HTTPS harness generates temporary local CAs and a synthetic token; it never reads a user's kubeconfig or credentials. It verifies HTTP 429 backoff and recovery, HTTP 410 relisting, HTTP 503 recovery, HTTP 401/403 clearing and stopping the affected watch, cross-namespace response refusal, a 1,001-object cache limit, a stalled response body's idle timeout, and server CA rotation. Updating the kubeconfig trust material does not silently alter an existing client's trust; an explicit reconnect using the new CA restores access.

These are executable protocol tests, separate from the real-cluster soak. They do not establish cloud-provider credential refresh, real certificate-rotation operations, APF throttling in a live cluster or a production-scale event flood. Existing auth-helper policy tests cover expiry and immutable binding; no live Copilot account request was sent in this batch.

## Remaining gates

Native Terminal and iTerm computer-use validation was attempted and explicitly blocked by the tool's safety restrictions; no alternate mechanism bypassed that block. PTY and real tmux evidence therefore leave native fonts, screen readers and real remote SSH paths unvalidated. The terminal guide contains a manual checklist tied to the exact binary.

Publisher signing/notarization, independent external security assessment and an organizational deployment pilot remain release gates. Longer and larger workload soaks, provider identity rotation and deployment-specific RBAC/admission/protected audit acceptance remain outside the bounded local evidence. The [v1 plan](MAC_V1_PLAN.md) and [remaining-work register](REMAINING_WORK.md) retain those limits.
