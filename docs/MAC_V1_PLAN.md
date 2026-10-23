# Mac + Docker Desktop v1 plan

Reviewed 2026-10-23. A v1 release means the documented Mac workflow is installable, useful and repeatably tested. It does not mean every k9s feature or every Kubernetes/Helm extension is supported.

## Current implementation batch

1. Existing-resource YAML spec editing: safe editable projection, original raw baseline, changed-path patch, server dry-run, complete review and explicit confirmation.
2. Node cordon/uncordon/drain: isolated capability, exact identity and scheduling-content checks, Kubernetes eviction API, disruption-budget enforcement and partial-state reporting.
3. Exports: retained filtered logs, selected redacted resource YAML, selected Helm metadata/resource references; private files and no overwrites.
4. Multiple forwards: eight independent loopback sessions, collision prevention, selected cancellation and complete scope/exit cleanup.
5. UI: compact clickable command menu, stable three-row header, continuous frames, visible mode, log severity text filters and clearer editor/review screens.

Acceptance and screenshots: [current report](MAC_V1_ACCEPTANCE.md). Exact limits: [operations](MAC_V1_OPERATIONS.md).

## k10s comparison and next implementation batches

Reference inspected: [mpiorowski/k10s](https://github.com/mpiorowski/k10s/tree/16e53054183f76683eb4fb58b6f3734365f44eab), README and `ui/app.go`. This is a design comparison, not a claim of identical feature semantics.

| Feature | KBorg status | Planned implementation and acceptance |
|---|---|---|
| Compact top command menu | Implemented this batch | Preserve k9s commands and responsive identity/mode; mouse and keyboard tests |
| Error/warning log emphasis | Implemented this batch | Word-based highlighting and combinable filters; preserve wrapping key and export timestamps; no inferred diagnosis |
| Dynamic multi-cluster grid and numbered focus | Next | Extend independent overview feeds into responsive cluster cards; number keys scoped to overview only; one slow/forbidden cluster must not block another |
| Stable health/warnings/log sections | Next | Fixed section heights and independent freshness indicators; no shifting controls when events arrive |
| OOM/restart/recent-warning summary | Next | Explicitly bounded event/status collection, evidence links and freshness; distinguish unknown from zero; drill into affected Pods/events |
| Saved context/workload selections | Next | Save opt-in non-secret preferences; revalidate context membership and identity on reopen; never persist authorization to write or send AI data |
| Unified live workload tail | Later | Explicit dynamic membership opt-in, visible join/leave events, UID replacement protection, concurrency/memory budgets and rollout tests |
| In-app dashboard legend | Next | Contextual explanation of metrics, text matches, retention and freshness; accessible without leaving overview |

Next batch order: dashboard cards/focus and health evidence, then saved selections and legend, then dynamic log membership. These features are planned; they are not included in the completed-feature list until accepted.

## Release gates after workflows

- Repeatable native Mac package build; publisher signing/notarization and authenticated provenance require a configured signing identity.
- First-run setup and diagnostics for optional tools; documented update/rollback/uninstall.
- Multi-hour real-cluster soak: API outages, 429s, reconnects, token/certificate rotation, floods, memory growth and interruption recovery.
- Actual Terminal/iTerm/SSH/tmux and narrow-font checks, accessibility review and broader Unicode handling.
- Independent security review and realistic organizational pilot; dependency audit and local tests remain supporting evidence.

Windows/cloud/enterprise Copilot expansion, arbitrary chart compatibility and advanced CRD conversion follow the scoped Mac release. Privileged node shell, automatic AI remediation and arbitrary in-process plugins remain excluded.
