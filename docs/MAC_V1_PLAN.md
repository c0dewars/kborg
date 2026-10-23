# Mac + Docker Desktop v1 plan

Reviewed 2026-10-23. A v1 release means the documented Mac workflow is installable, useful and repeatably tested. It does not mean every k9s feature or every Kubernetes/Helm extension is supported.

## Current implementation batch

1. Existing-resource YAML spec editing: safe editable projection, original raw baseline, changed-path patch, server dry-run, complete review and explicit confirmation.
2. Node cordon/uncordon/drain: isolated capability, exact identity and scheduling-content checks, Kubernetes eviction API, disruption-budget enforcement and partial-state reporting.
3. Exports: retained filtered logs, selected redacted resource YAML, selected Helm metadata/resource references; private files and no overwrites.
4. Multiple forwards: eight independent loopback sessions, collision prevention, selected cancellation and complete scope/exit cleanup.
5. UI: quiet header with only `?` help, stable identity/status, continuous frames, visible mode, log severity text filters and clearer editor/review screens. Familiar commands remain available through the keyboard.
6. Dashboard cards: independent context health and warning evidence, responsive layout, numbered focus and a contextual legend.
7. Saved selections: opt-in context groups and resource/workload scopes, private bounded storage and destination/UID revalidation.
8. Approval hardening: distinct Unicode/backslash review text, bounded expansion, minimum visible export dimensions and refusal when approval-time terminal size is unknown.
9. Mac terminal evidence: 10 PTY profiles, narrow/wide resize recovery and an isolated real tmux session; persistent saved-picker controls.
10. Native packaging: clean-source verification, fresh native build, honest smoke-test records and archives without macOS filesystem metadata.

Acceptance and screenshots: [current hardening report](MAC_V1_HARDENING_ACCEPTANCE.md), [dashboard implementation](DASHBOARD_ACCEPTANCE.md). Exact limits: [operations](MAC_V1_OPERATIONS.md).

## k10s comparison and next implementation batches

Reference inspected: [mpiorowski/k10s](https://github.com/mpiorowski/k10s/tree/16e53054183f76683eb4fb58b6f3734365f44eab), README and `ui/app.go`. This is a design comparison, not a claim of identical feature semantics.

| Feature | KBorg status | Planned implementation and acceptance |
|---|---|---|
| Quiet top header | Implemented to user preference | Visible command labels removed; `?` opens keymap, while typed k9s navigation remains |
| Error/warning log emphasis | Implemented this batch | Word-based highlighting and combinable filters; preserve wrapping key and export timestamps; no inferred diagnosis |
| Multi-context grid and numbered focus | Implemented | Up to four explicit contexts, responsive cards and context-scoped number keys; slow/forbidden peers stay independent |
| Stable health/warning sections | Implemented | Independent Pod/Event states and stable sections; live logs remain a separate view |
| OOM/restart/recent-warning summary | Implemented within documented scope | Current/last container OOM evidence, current restart counters, 15-minute Warning Event records tied to current Pod UIDs; Enter opens the context Pod list |
| Saved context/workload selections | Implemented | Explicit bounded private preferences; revalidate active kubeconfig, destination resource catalog and workload UID; no stored operation approvals |
| Unified live workload tail | Later | Explicit dynamic membership opt-in, visible join/leave events, UID replacement protection, concurrency/memory budgets and rollout tests |
| In-app dashboard legend | Implemented | `?` explains controls, evidence and freshness without closing independent watches |

Next feature batch: explicitly opted-in dynamic log membership, then direct warning-to-event drilldown and optional dashboard log panes. Release hardening below remains required; dashboard acceptance does not imply full k10s parity.

## Release gates after workflows

- Publisher signing/notarization and authenticated provenance still require a signing identity and release authority. Native unsigned packaging and its offline first-run checks now have retained evidence.
- Continuous local-cluster soak results are tracked in [the hardening acceptance](MAC_V1_HARDENING_ACCEPTANCE.md). Production-scale floods, provider token/certificate rotation, reconnect storms and deployment-specific latency remain separate acceptance work; synthetic HTTPS cases cover bounded protocol behavior only.
- Native Terminal/iTerm font checks, remote SSH, screen readers and broader Unicode handling remain. The computer-use tool explicitly blocked native terminal applications; the PTY matrix and actual isolated tmux checks passed.
- Independent external security assessment and a realistic organizational pilot remain. The internal review, known-advisory scan, reviewed operations guide and local tests support these gates without replacing them.

Windows/cloud/enterprise Copilot expansion, arbitrary chart compatibility and advanced CRD conversion follow the scoped Mac release. Privileged node shell, automatic AI remediation and arbitrary in-process plugins remain excluded.
