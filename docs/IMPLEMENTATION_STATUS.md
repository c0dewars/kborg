# KBorg current implementation status

Updated 2026-10-10 for 0.3.0. This file and REMAINING_WORK.md supersede older progress notes. Historical release reports retain their original source/artifact claims.

## Implemented

- KBorg product name, `kborg` executable/crate, updated logo and private `c0dewars/kborg` repository. Legacy settings/env/theme IDs remain compatible.
- Direct verified Kubernetes TLS client; read-only default; bounded lists, live watches, discovery and redacted JSON/YAML inspection; context/namespace/resource browsing and cache search.
- Container chooser, bounded snapshots/previous logs and up to24-hour rate-limited follow with rolling256KiB/2000-line display and cancellation; demo covers17 resource kinds.
- Optional pod/node metrics, bounded history and explicit request/limit/allocatable/capacity comparisons with exact quantities and unavailable/forbidden states. Independent snapshots for1..100 contexts and continuous pod watches for up to4 explicit contexts.
- Scale/restart/delete, deployment container-image and four-field CPU/memory request/limit edits; target-bound approval, UID/resourceVersion, dry-run/admission review, GitOps refusal for detected managed workloads, durable audit intent and pending-outcome reporting.
- Native explicitly gated bounded exec/output attach and optional stdin/TTY with resize/cancel/terminal restoration, plus finite loopback forwarding. Read-only mode rejects all three; separate capability flags and typed target approval are mandatory.
- Strict schema1 capability-constraining profiles; reviewed Unix ExecCredential helpers; static completions; visible actions; polished health/navigation/selection panels, centered forms, readable object overview, native pager, keyboard-only `--no-mouse` and16/256/truecolor/monochrome.
- 18 built-in themes, default Catppuccin Mocha, first-use picker, 2-second skippable startup, custom theme workflows including current-color cloning and backup-preserving replacement; atomic appearance replacement.
- Explicit local read-only extension registry with immutable native executable SHA/argv and fresh transfer review; no in-process plugins.
- Local minimized Copilot context and official CLI login handoff for github.com / Enterprise Cloud *.ghe.com. Preflight/minimized preview supplied; no automated model requests.
- Native six-platform packaging workflow, checksums, CycloneDX SBOM and unsigned source/build records; dependency notice coverage reported by packaging.

## Evidence and limits

Windows unit/contract tests, independent security/PTY review, owned Docker Desktop image/log/YAML acceptance, and native exec/forward acceptance have been exercised. Reports are in SECURITY_REVIEW_0.3.md, READ_WORKFLOWS.md, NATIVE_OPERATIONS.md and docs/evidence. WSL/performance and renamed platform artifacts must be read from their final reports rather than inferred from prior 0.2 builds.

This is a testing prerelease, not a completed production certification. See REMAINING_WORK.md for unfinished features and external gates. Privileged external-tool containment, cloud-provider identity/chained-helper acceptance, automated Copilot, remote full-screen programs, interactive external editors/pagers, Unicode/accessibility and the full production acceptance program remain incomplete. Local audit files and client policy do not replace server-side RBAC/admission or protected organizational auditing.

Distribution update: the final six-platform rerun was blocked before jobs by GitHub account billing/spending limits. Four Linux/macOS packages from the preceding runtime-identical commit passed; Windows packaging required a license-byte preservation fix. Exact verified artifacts, source revisions and local Windows evidence are recorded in PLATFORM_BUILDS_0.3.md. A successful final six-platform run is not claimed.

Current macOS priority iteration evidence: [PRIORITIES_ACCEPTANCE.md](PRIORITIES_ACCEPTANCE.md)
and the exact source hashes in docs/evidence/priorities-local-macos.json. Historical
Windows/Linux/platform reports do not certify this new source. Optional Copilot
preflight makes no model/login request or real-token read, and diagnosis remains
unconditionally disabled while egress/credential/publisher gates are open.
