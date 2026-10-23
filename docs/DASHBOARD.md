# Dashboard and saved workspace

Press `D` from a resource table to open the current context and namespace. The header shows a plain view title and `?`; familiar typed commands such as `:pods`, `:nodes` and `:overview` remain available without a persistent command menu.

## Controls

| Key | Action |
|---|---|
| `1`..`4`, arrows, Tab | Focus a context card; number keys apply only inside the dashboard |
| Enter | Open current Pods in the focused context and displayed namespace |
| `c` | Choose one to four explicit context names, comma separated; Ctrl-U clears the input |
| `s` | Name and save the dashboard's context group and namespace |
| `B` | Open saved selections |
| `?` | Show the dashboard keymap and evidence legend; watches keep running |
| Esc | Close help, or close the dashboard and cancel its watches |

All cards use the selected namespace. Change namespace in the normal resource view before opening the dashboard, or reopen an explicitly saved dashboard scope. Wide terminals show a grid; narrow terminals retain the focused card and numbered navigation. A wide single-context view places health and warnings side by side.

Each card has independent Pod health and recent Warning Event evidence. Counts describe bounded observations, not an overall cluster health guarantee. Permission failures, incomplete lists and stale data remain explicit; an unknown count is `--`, never a healthy zero. Restarts are current counters, and OOM is current/previous container termination evidence. Warnings cover recent Event records linked to current Pod UIDs. See [exact health semantics and limits](DASHBOARD_HEALTH.md).

## Saved selections

Press `b` in a current resource table to save its context, namespace, resource type and server label selector. A selected supported workload also retains its namespace/name/UID for focus after reopening. Cached text filters, relationship filters, credentials and approvals are not saved. Dashboard `s` saves only its context group and namespace.

`B` opens the picker; Enter revalidates against the active kubeconfig and destination discovery. A replaced workload is reported instead of inheriting the old object's focus. `d` asks for the exact saved name before deleting a preference. Nothing is restored automatically at startup. Corrupt or nonprivate storage produces an in-app notice and is preserved.

See [saved-selection format and privacy](SAVED_SELECTIONS.md), [current acceptance](DASHBOARD_ACCEPTANCE.md), and [remaining v1 work](MAC_V1_PLAN.md).
