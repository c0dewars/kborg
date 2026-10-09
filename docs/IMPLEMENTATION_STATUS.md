# KBorg current implementation status

Updated 2026-10-10 for 0.3.0. This file and REMAINING_WORK.md supersede older progress notes. Historical release reports retain their original source/artifact claims.

## Implemented

- KBorg product name, `kborg` executable/crate, updated logo and private `c0dewars/kborg` repository. Legacy settings/env/theme IDs remain compatible.
- Direct verified Kubernetes TLS client; read-only default; bounded lists, live watches, discovery and redacted JSON/YAML inspection; context/namespace/resource browsing and cache search.
- Container chooser, snapshot/previous logs and finite live log following with cancellation; isolated offline demo.
- Optional pod/node metrics with exact quantity normalization and unavailable/forbidden states; independent snapshots for 1..100 explicitly named contexts, four concurrent, 30-second global deadline.
- Scale/restart/delete and deployment container-image edit; target-bound approval, UID/resourceVersion, dry-run/admission review, GitOps refusal for detected managed images, durable audit intent and pending-outcome reporting.
- Native explicitly gated exec (no stdin/TTY), output-only attach, and finite loopback port forwarding. Read-only mode rejects all three; separate capability flags and typed target approval are mandatory.
- Strict schema-1 JSON read-only/operator configuration, static shell completions, visible TUI actions, keyboard-only `--no-mouse`, TERM=dumb CLI guidance.
- 18 built-in themes, default Catppuccin Mocha, first-use picker, 2-second skippable startup, custom theme workflows including current-color cloning and backup-preserving replacement; atomic appearance replacement.
- Local minimized Copilot context and official CLI login handoff for github.com / Enterprise Cloud *.ghe.com. No automated model requests.
- Native six-platform packaging workflow, checksums, CycloneDX SBOM and unsigned source/build records; dependency notice coverage reported by packaging.

## Evidence and limits

Windows unit/contract tests, independent security/PTY review, owned Docker Desktop image/log/YAML acceptance, and native exec/forward acceptance have been exercised. Reports are in SECURITY_REVIEW_0.3.md, READ_WORKFLOWS.md, NATIVE_OPERATIONS.md and docs/evidence. WSL/performance and renamed platform artifacts must be read from their final reports rather than inferred from prior 0.2 builds.

This is a testing prerelease, not a completed production certification. See REMAINING_WORK.md for unfinished features and external gates. Privileged subprocess integrations, cloud auth helpers, isolated in-app Copilot, interactive TTY sessions, advanced terminal rendering/accessibility and the full production acceptance program remain incomplete. Local audit files and client policy do not replace server-side RBAC/admission or protected organizational auditing.
