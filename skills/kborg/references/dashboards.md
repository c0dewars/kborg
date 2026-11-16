# KBorg dashboards and saved selections

The dashboard shows one card per cluster (1 to 9 contexts) with live health: Pods that need attention, restart hotspots, OOM kills, recent Warning events, Node readiness with CPU and memory, and the busiest Nodes by name. Every card has its own read-only watch. Counts are bounded observations: an unknown count is `--`, never a healthy zero.

## Opening one

| How | What |
|---|---|
| `D` in a resource table | The current context and namespace |
| `kborg --contexts dev,staging,prod` | Opens straight into a dashboard of those contexts, and remembers them |
| `kborg dashboard` (or `kborg dash`) | Reopens the last dashboard; the first time, asks which contexts |
| `c` on the dashboard | Type 1 to 9 context names, comma separated |

Context names are exactly those of `kborg contexts`. All cards share one namespace scope: pick the namespace in the table (`:ns NAME`, or `0` for all) before pressing `D`, or reopen a saved dashboard.

## Keys on the dashboard

Check `kborg keymap --json` with `"view": "dashboard"` for the person's actual keys. The defaults:

| Key | Does |
|---|---|
| `1`..`9`, arrows, Tab | Focus a card; the same number again shows it full screen |
| Enter | Open the focused cluster's Pods |
| `w` | Select the next recent warning; Enter on it opens Events for that exact Pod |
| `L` on a warning | Follow that Pod's logs in a pane under the cards |
| `t` | Tail Deployments or StatefulSets across every card's cluster (Space selects up to six) |
| `G` | Trend charts for the focused card (15 minutes, one sample per 15 s) |
| `H` | Chart range: last 15 minutes, 24 hours or 7 days. Real clusters keep 7 days of counts and percentages on this machine (`ui.yaml` `dashboard.history`) |
| `s` | Save this dashboard (its contexts and namespace) under a name |
| `B` | Open saved selections |
| `?` | Legend and keys; the watches keep running |
| Esc | Close help or panes first, then the dashboard |

## Saving and reopening

- **Save:** `s` on the dashboard saves its context group and namespace, and `b` in a resource table saves that view (context, namespace, resource and label selector).
- **Reopen:** `B` opens the saved selections. Choose one with the arrows and press Enter; `d` or Delete removes one after asking.
- **Limits:** a saved dashboard holds 1 to 4 contexts, and the store holds 16 selections named with 1 to 64 plain characters. A dashboard wider than four contexts is reopened with `kborg --contexts a,b,...` instead.
- **What is stored:** names only, in `selections/v1.json`, which KBorg writes itself. No credentials, results or approvals are stored. Create selections from the TUI rather than by writing the file.
- **Reopening checks:** the contexts must still exist in kubeconfig, and the resource must still exist on the cluster.

A good setup to suggest: one saved dashboard per environment group (`dev`, `staging`, `prod`), each with its namespace, and a shell alias such as `alias kprod='kborg --contexts prod-eu,prod-us'` for the larger groups.

## Layout (ui.yaml)

```yaml
# FILE: ui.yaml
version: 1
dashboard:
  columns: 0            # 0 picks 1 to 3 from the terminal width; 1..3 fixes it
  min_card_width: 50    # with columns: 0, the narrowest card allowed
  min_card_height: 9    # below this a grid shows only the focused card
  trend: true           # sparkline on expanded cards
  node_details: true    # busiest-Node lines under NODES
  history: true         # keep 7 days of chart history on this machine
charts:
  style: auto           # auto, braille, blocks or ascii
```

Wide terminals (150 columns or more) use three columns once five or more clusters are shown. Run `kborg config check` after editing.

## Per-cluster look

A `contexts/NAME.yaml` with `label: PROD` and `color: danger` puts a red badge in the header whenever that context is selected, and `read_only: true` keeps that cluster browse-only. The dashboard itself is always read-only. See clusters-and-safety.md.

## Without the TUI

`kborg overview CTX1 CTX2 ...` prints the same per-cluster summary once (1 to 100 contexts). `--follow` keeps watching up to 4 contexts, and `--json` gives machine-readable output for a script or another dashboard.
