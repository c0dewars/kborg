# Dashboard and saved workspace

Press `D` from a resource table to open the current context and namespace. The header shows a plain view title and `?`; familiar typed commands such as `:pods`, `:nodes` and `:overview` remain available without a persistent command menu.

## Controls

| Key | Action |
|---|---|
| `1`..`4`, arrows, Tab | Focus a context card; pressing the focused card's number again toggles full-screen focus |
| Enter | Open current Pods in the focused context and displayed namespace |
| `w` | Select the next recent warning in the focused card; cycling past the last clears it |
| Enter on a warning | Open Events for that exact Pod name and UID in its namespace |
| `L` on a warning | Follow that exact Pod's logs (all ordinary containers) in a pane under the cards; `L` again closes it |
| Esc on a warning | Close an open log pane first, then clear the warning selection without closing the dashboard |
| `c` | Choose one to four explicit context names, comma separated; Ctrl-U clears the input |
| `s` | Name and save the dashboard's context group and namespace |
| `B` | Open saved selections |
| `?` | Show the dashboard keymap and evidence legend; watches keep running |
| Esc | Close help, or close the dashboard and cancel its watches |

The log pane opens its own read-only connection to the focused card's context, so it works for any card, not only the connected one. It requires `--allow-logs`, follows the warning's Pod name and UID with the normal follow budget and shared byte/rate limits, and stops when it is closed, when Enter opens Pods or Events, or when the dashboard closes. Only one log session runs at a time, so opening the pane ends any other log view.

Expanded cards also show `NODES ready/total` (with cordoned Nodes and cluster CPU/memory as a percentage of summed allocatable when the Metrics API answers) and `APPS` with Deployment/StatefulSet counts and up to four named degraded workloads. A workload is degraded when it wants replicas that are not available or ready, or its rollout reports `ProgressDeadlineExceeded`; scaled-to-zero workloads are not. These come from a separate 15-second read-only poll per card, so a slow or denied read never delays the Pod and Event watches. Nodes are cluster-scoped and often denied to namespace-scoped identities; the card then shows `NODES DENIED` rather than zero.

`kborg --contexts a,b` opens the TUI directly in this dashboard for one to four contexts.

All cards use the selected namespace. Change namespace in the normal resource view before opening the dashboard, or reopen an explicitly saved dashboard scope. Wide terminals show a grid; narrow terminals retain the focused card and numbered navigation. A wide single-context view places health and warnings side by side.

Warning drilldown sends a server-side field selector binding `involvedObject.kind=Pod`, the Pod name and its UID, so a same-name replacement never inherits the old Pod's Events. The selection follows the same record across refreshes and clears if the record disappears or another card is focused. Esc in the Events view returns to the previous view.

Each card has independent Pod health and recent Warning Event evidence. Counts describe bounded observations, not an overall cluster health guarantee. Permission failures, incomplete lists and stale data remain explicit; an unknown count is `--`, never a healthy zero. Restarts are current counters, and OOM is current/previous container termination evidence. Warnings cover recent Event records linked to current Pod UIDs. See [exact health semantics and limits](DASHBOARD_HEALTH.md).

## Saved selections

Press `b` in a current resource table to save its context, namespace, resource type and server label selector. A selected supported workload also retains its namespace/name/UID for focus after reopening. Cached text filters, relationship filters, credentials and approvals are not saved. Dashboard `s` saves only its context group and namespace.

`B` opens the picker; Enter revalidates against the active kubeconfig and destination discovery. A replaced workload is reported instead of inheriting the old object's focus. `d` asks for the exact saved name before deleting a preference. Nothing is restored automatically at startup. Corrupt or nonprivate storage produces an in-app notice and is preserved.

See [saved-selection format and privacy](SAVED_SELECTIONS.md), [current acceptance](DASHBOARD_ACCEPTANCE.md), and [remaining v1 work](MAC_V1_PLAN.md).
