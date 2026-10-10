# KBorg 0.3 independent security review

Date: 2026-10-10. Scope: source and targeted executable behavior during the current development iteration. This is an independent agent review, not an external penetration test or production certification. Changes were still being integrated; final release source/artifact identity must be recorded separately.

## Findings and verified corrections

1. **Approval display mismatch for exec arguments.** Exec validation originally rejected ASCII control characters but allowed Unicode bidi/zero-width characters removed from the displayed approval summary. The remote argv could differ from the visible reviewed argv. Corrected validation rejects any argument changed by the application's safe-text projection. Independently reran `rejects_invisible_arguments_and_truncated_reviews`: passed for U+202E, U+200B and U+FEFF plus review-size bounds.
2. **Unreviewed dry-run metadata changes.** Controlled image editing originally checked admission changes only in the Deployment spec. Added comparison of labels, annotations, owner references, finalizers and identity fields against the original object. Independent test `independent_admission_metadata_changes_are_not_silently_approved` passed: injected controller/security labels, annotations, owners and finalizers are refused; ordinary server-managed metadata remains permissible. A different admission decision at real execution still requires authoritative server policy; dry-run is not a reservation.
3. **Preference delete/rename gap.** The previous implementation removed appearance.json before rename based on an incorrect Windows assumption. Replaced with same-directory synced staging and native replacement; Windows tests and native WSL Unix mode tests passed. See STORAGE.md for remaining ACL/path-race/durability limitations.
4. **Discovery accumulated before limiting.** Replaced broad discovery accumulation with per-document, aggregate, group, kind, request and deadline bounds. Five focused tests and read-only real Docker Desktop discovery passed. Failed/incomplete refresh retains the previous catalog and returns an explicit error.

No unresolved high-severity finding was established by this limited review. That statement is not an assertion that none exists.

## Independent checks

- Actual CLI refused a read-only profile plus `--write`.
- Actual CLI refused duplicate profile keys and unknown executable configuration fields.
- Actual CLI refused Secret YAML export, including in the demo backend.
- Actual CLI refused incompatible previous-log and follow options.
- Inspected gateway enforcement: exec, attach and forwarding need writable mode, a separate explicit capability and an allowed namespace. Read-only browsing does not grant those capabilities.
- Inspected sealed operation approval: private request/target/session state; expiry and exact typed target; fresh UID/resourceVersion comparison before subresource connection. Kubernetes connect endpoints do not provide UID preconditions, so the documented narrow name-recreation race remains.
- Checked installed kube-client 4.2 AttachedProcess Drop implementation: it aborts its owned task. Timeout returns that drop the value do not leave its local task intentionally detached. This does not guarantee termination of a remote command.
- Forwarding binds IPv4 loopback, caps simultaneous clients and lifetime, and aborts owned forwarding transports on drop. Other local users can access the listener, as the review states.
- Log follow bounds input, line size and lifetime; TUI uses a single capped mailbox and generation cancellation. Read-only logs remain explicit opt-in and are not claimed universally secret-free.
- Context overview creates independent gateways and forces read-only behavior; results retain summaries rather than complete object caches. Four concurrent sessions are bounded. A global deadline was requested because per-context deadlines alone allow 100 unreachable contexts to take roughly 500 seconds.
- Pending audit preserves unknown status for partial outcomes. The local spool remains mutable by the operator and cannot replace protected server audit.

## Acceptance still required

Run final combined tests/lint and real disposable-cluster mutation, subresource and log lifecycle checks against the final source. Test authentication expiry, network failures, cancellation and terminal restoration on the supported native platforms. Production gates remain for managed Windows ACL provisioning, race-resistant filesystem operations where hostile local writers are in scope, protected audit/server admission, cloud auth helper trust, advisor OS isolation, signing/notarization, broad terminal/accessibility coverage and staged organizational acceptance. No Copilot model request or real identity login was performed in this review.

## Subsequent integrated validation

The combined Windows (83 library tests plus metrics contract) and WSL (84 library tests) suites and strict all-targets Clippy passed. Disposable Docker Desktop mutation and native Windows/WSL subresource lifecycle checks also passed, as recorded in RELEASE_REPORT_0.3.md and NATIVE_OPERATIONS.md. These results close the local integration checks above; they do not close the explicitly listed production or broader platform acceptance gates. Final native artifact identity is recorded in PLATFORM_BUILDS_0.3.md.
