# Using the KBorg TUI

For exact keys, use `kborg keymap --json`: the person may have remapped some, and the defaults below can change between versions. In the TUI, `?` shows the current view's keys and `:keymap TEXT` searches all of them.

## Getting around (k9s grammar)

| Type | Does |
|---|---|
| `:pods`, `:po`, `:dp`, `:svc`, `:nodes`, `:v1/pods`, `:deployments.apps` | Open a resource (CRDs and discovered short names too) |
| `:pod NAMESPACE`, `:pod all` | That namespace, or every namespace |
| `:pod /REGEX`, `:pod app=web,tier!=batch` | Filter, or a server-side label selector |
| `:pod @CONTEXT` | Switch context, same resource |
| `:ctx`, `:ns` | Context or namespace picker |
| `/TEXT`, `/!TEXT`, `/-f TEXT`, `/-l LABELS` | Filter rows: regex, inverse, fuzzy, label selector |
| Ctrl-P | Command palette: fuzzy search over commands and actions |
| `:q` | Quit |

The usual table keys:
- **Look:** `d` describe, `y` YAML, `J` JSON, `l` logs, `p` previous logs, Enter drill down, Esc back.
- **Act:** `e` edit YAML, `s` shell (Pods) or scale (workloads), `F` port-forward (Pods, Services, workloads), Ctrl-D delete.
- **Jump:** `U` owner, `u` used by (ConfigMaps, PVCs, ServiceAccounts), `o` the Pod's Node.
- **Mark and sort:** Space marks rows for a batch; `Shift-Left`/`Shift-Right` move the sort column, and `N` `A` `S` sort by name, age, status.
- **Scope:** `0` all namespaces, `1`..`9` recent namespaces, Ctrl-W wide columns, Ctrl-Z only objects that need attention.
- **Copy:** Ctrl-Y copy mode, which releases the mouse so the terminal can select text.

Other views worth knowing:
- `D` dashboard
- `:secrets` masked Secrets
- `:helm` releases
- `:metrics` metrics
- `:pulses` health
- `:xray RESOURCE` relationships
- `:lint` misconfiguration report
- `:apply FILE` server-side apply
- `:themes` theme picker
- `:columns` columns of this table
- `:ctx-config` this context's settings

## Status words in the header

| Word | Means |
|---|---|
| `LIVE` | The table follows a watch and is current |
| `LOADING` | The first list is still arriving |
| `STALE` | The connection dropped; the rows are the last good data and reconnect is automatic |
| `POLL` | Refreshed by polling instead of a watch (`--poll`) |
| `CACHED` | The rows of the last list, with no live watch; Ctrl-R refreshes |
| `SAMPLE` | Demo data (`kborg demo`), no cluster |
| `TRUNCATED` | More objects exist than were read (the cap is 10,000); narrow the namespace or selector |
| `READ ONLY` | Changes are off (`--read-only`, operator config or the context's settings) |
| `COPY MODE` | The mouse is released to the terminal (Ctrl-Y again takes it back) |

In metric columns, `-` means unknown, never zero.

## Reviews

Every change opens a review before anything is written:
- **Contents:** the context, the server and the exact target (namespace/name, UID), what will change, and the result of a server dry run.
- **Approving:** `y` or Enter approves and `n` or Esc cancels. With `strict_confirmations` the person types the phrase shown instead.
- **Read before approving:** a long review scrolls with PgUp/PgDown. Tell the person to check the `Context:` and target lines before they approve. With typed confirmations the whole review has to be scrolled through first.
- **Expiry:** most reviews expire after 60 seconds (Helm plans after 15 minutes), and the write is cancelled if the object changed after the review.

If the person asks you what a review means, explain each line. Do not tell them to approve it. That decision is theirs.

## Everyday changes

Each of these opens a review first.

| Task | How |
|---|---|
| Edit an object's YAML | `e`: fields, labels, annotations, ConfigMap data; Ctrl-S shows the diff and review, Ctrl-E opens `$EDITOR` |
| Change an image or resources | `:image`, `:edit-container`; `kborg set-image`, `kborg set-resources` |
| Scale, restart, roll back | `s` (scale) and `r` (restart) on workloads, `:rollback [REV]` |
| Delete | Ctrl-D, or mark rows with Space for one batch review; Ctrl-K kills a Pod with zero grace |
| Apply a manifest | `:apply FILE`, or `a` on a file in `:dir PATH`; dry run and field-level diff, conflicts never forced |
| CronJobs | `t` triggers a Job now, `s` suspends or resumes |
| Nodes | `c` cordon, `u` uncordon, `r` drain; `s` opens a root node shell through a reviewed, self-deleting Pod |

## Shells, port-forwards and files

| Task | How |
|---|---|
| Shell or attach | `s` or `a` on a Pod or container: a full terminal like `kubectl exec -it` (vim and top work); Ctrl-] returns to KBorg |
| Debug container | `:debug [CONTAINER]`: an ephemeral debug container and shell |
| Port-forward | `F` on a Pod, Service or workload opens the form (local port, remote port, duration up to 12 h); `:pf` lists forwards (`s` stops one, `b` benchmarks it), `f` shows the selected one |
| Copy files | `:cp-from /PATH [LOCAL]` and `:cp-to LOCAL /PATH` (`-r` for a directory, up to 64 MiB); never replaces an existing file |

## Looking deeper

| View | Shows |
|---|---|
| `:metrics` | CPU and memory of Pods or Nodes with history; `g` for a chart (needs metrics-server) |
| `:pulses` | Health across resources |
| `:xray RESOURCE` | Ownership and Service relationships |
| `:lint` | Misconfigurations in the namespace (like popeye) |
| `:can u:NAME` | RBAC bindings of a user, group or ServiceAccount |
| `:helm` | Releases: Enter history, `i` install, `u` upgrade, `t` test, Ctrl-D uninstall; in history, `r` rolls back |
| `:events`, `:ev` | Events, like any resource |

## Secrets (`:secrets`)

The list shows names, types, key names and sizes, never values.

| Key | Does |
|---|---|
| Enter | List the keys |
| `x` | Reveal one key for 30 seconds |
| `y` | Masked YAML |
| `u` | Pods that use the Secret |
| `n` | Create a Secret |
| `e` | Set a key's value |
| `a` | Add a key |
| Ctrl-D | Remove a key, or delete the Secret |

Values are typed into a hidden prompt (Tab shows them) or read from `@/path/to/file`. They never appear in a review, an export or an audit record.

## Logs

`l` opens logs, and `a` merges every container. Other keys:
- **Range:** `0` tail, `1` head, `2`..`6` the last 1m, 5m, 15m, 30m or 1h.
- **Display:** `s` autoscroll, `w` wrap, `t` timestamps, `f` fullscreen.
- **Filter:** `e` errors only, `W` warnings only.
- **Export:** Ctrl-S opens a private export preview of the logs.

## Copilot (optional)

`kborg copilot setup` connects GitHub Copilot. Then Ctrl-G or `:ask QUESTION` asks about the screen in front of the person, and `:copilot` asks about the selected object. KBorg shows the exact text it will send and waits for approval. Answers are advice only: KBorg never runs a suggested command.
