# KBorg current implementation status

Updated 2026-10-26 for the `release/next` working branch (0.5.0-rc.3 package version; 0.5.0 final remains unreleased). This file and [REMAINING_WORK.md](REMAINING_WORK.md) supersede older progress notes. Historical acceptance reports keep their original scope and evidence.

## Implemented

**Browse and inspect**
- Direct Kubernetes client; operator mode by default, with `--read-only` and company-profile restrictions; 35 built-in kinds plus discovered CRDs; context/namespace switching; regex, inverse, fuzzy and label filters; sorting; drilldown; redacted describe, YAML and JSON.
- Live LIST/WATCH with relist, bounded caches (10,000 objects / 64 MiB), polling fallback and memoized filter/sort for large tables.
- Existing `KUBECONFIG` files or `~/.kube/config`, current context or `--context`; startup names the connection and asks Yes/No before any client or helper starts. No selects an existing context; no tokens are copied and the user's kubeconfig is not rewritten. Normal kubeconfig exec authentication is reused; a separate reviewed Unix helper mode remains available.
- `:pulses`, `:xray`, `:workloads`, `:can`, `:subjects`, `:lint` (read-only misconfiguration report) and Ctrl-Z errors-only view.
- k9s-format `views.yaml` columns (built-in and `TITLE:.json.path`), sort by any column in either direction, wide mode, recent namespaces on digits and recent contexts.

**Dashboard**
- 1–9 clusters with independent watches: Pod health, restart hotspots, OOM evidence, warning Events with exact-Pod drilldown, Node readiness and CPU/memory, the busiest or unhealthy Nodes by name, degraded Deployments/StatefulSets, and 15-minute trend sparklines.
- Single-Pod log panes and a cross-cluster workload tail; saved context groups; `kborg dashboard` reopens the last one.

**Logs and metrics**
- Container, workload and selector logs with fixed or live (`:logs-live`) membership, ranges, search, error/warning filters and export.
- Pod/Node metrics with exact quantities, request/limit comparisons and bounded history.

**Operate** (available capabilities are on by default; company configuration can deny them. Review before execution, server dry-run where supported, local audit record. Quick confirmation is `y`/Enter; `strict_confirmations` restores typed confirmation.)
- Scale, restart, rollback, delete, force kill; CronJob trigger and suspend/resume; image/resource forms; YAML spec editing in-app or in `$EDITOR`.
- Server-side apply of local manifests (`--allow-apply`), never forcing field ownership.
- Cordon/uncordon/drain (`--allow-node-maintenance`); node shell through a reviewed, self-expiring privileged Pod (`--allow-node-shell`); ephemeral debug containers (`--allow-debug`).
- Exec/attach with optional TTY, eight loopback port forwards with a bounded benchmark, file and directory copy (`--allow-file-copy`).
- Helm browsing and a reviewed lifecycle subset with OCI, OpenPGP and Sigstore verification.
- Secret key listing and masked YAML; revealing one value requires a separate action and expires.

**Workspace and integrations**
- k9s-format aliases and `hotkeys.yaml`; registered read-only extensions runnable from the CLI and the TUI (`:ext`), with a k9s `plugins.yaml` importer.
- Strict schema-1 configuration profiles that can forbid every capability flag.
- Discoverable `:ask`/Copilot setup and status. Contained inference is macOS ARM64 only; real GitHub.com evidence exists. GHE.com/GHES compatibility is implemented but live enterprise acceptance remains open. Every transfer is reviewed; platform availability and account entitlement are prerequisites.
- 20 themes plus custom palettes with WCAG AA contrast checks, animated startup, truecolor/256/16/monochrome output, contextual key hints, shell completions.
- Homebrew formula, release archives with checksums, CycloneDX SBOM.

## Evidence

- More than 600 unit tests plus integration suites; clippy with `-D warnings`; lints deny `unwrap`, `expect` and `unsafe`.
- Live checks on Docker Desktop 1.36.1 and minikube 1.35.9, including 2-hour soaks ([MAC_V1_SOAK.md](MAC_V1_SOAK.md)).
- Side-by-side performance against k9s and k10s ([PERFORMANCE_COMPARISON.md](PERFORMANCE_COMPARISON.md)) and a CI regression gate.
- Internal security reviews and an [external audit pack](SECURITY_AUDIT_PACK.md).

## Not done

Independent security audit, code signing and notarization, live GHE.com/GHES Copilot verification, broader native screenshots in Ghostty/iTerm2/Terminal.app/Windows Terminal, screen-reader acceptance, Windows file-trust hardening and Linux/Windows Copilot containment. Native Windows x64/ARM64 builds passed on the pre-onboarding fix; new source requires its own validation. See [REMAINING_WORK.md](REMAINING_WORK.md).
