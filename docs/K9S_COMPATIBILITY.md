# k9s navigation compatibility

Reviewed against upstream k9s commit `e51ac53cb4c3a9a96666297581d309792f8faf8c` on 2026-10-22. The KBorg interface now uses a full-width resource table and command navigation, without a persistent sidebar. Familiar command names and resource shortcuts retain their purpose. This is a compatibility register, not a claim of complete k9s feature parity.

References: [command guide](https://k9scli.io/topics/commands/), [command interpreter](https://github.com/derailed/k9s/blob/e51ac53cb4c3a9a96666297581d309792f8faf8c/internal/view/cmd/types.go), [resource registry](https://github.com/derailed/k9s/blob/e51ac53cb4c3a9a96666297581d309792f8faf8c/internal/dao/registry.go), [default aliases](https://github.com/derailed/k9s/blob/e51ac53cb4c3a9a96666297581d309792f8faf8c/internal/config/templates/aliases.yaml), [browser keys](https://github.com/derailed/k9s/blob/e51ac53cb4c3a9a96666297581d309792f8faf8c/internal/view/browser.go).

## Commands

| k9s command family | KBorg behavior |
|---|---|
| `:pod`, `:pods`, `:po`; singular/plural/discovered short names | Browse resources. 35 built-in kinds plus bounded API discovery and CRDs. Ambiguous aliases require a qualified resource. Secrets are excluded from generic browsing; see `:secrets`. |
| `:secrets`, `:secret`, `:sec`; `x` | Opt-in with `--allow-secret-reveal`. Lists names, types, key names and byte sizes for the current namespace (500 max); values stay masked until `x` reveals one key for 30 seconds over a fresh UID-checked read. Binary or >16 KiB values are reported by size. Values are never exported, copied, screen-dumped or kept after Esc. |
| Ctrl-K; `:kill` | Opt-in with `--write --allow-force-kill`. Pods only. Same dry-run, UID/resourceVersion preconditions, review and typed approval as Ctrl-D, with `gracePeriodSeconds=0`. |
| `:dp`, `:deploy`, `:svc`, `:no`, `:sts`, `:ds`, `:rs`, `:cj`, `:jo`, `:cr`, `:crb`, `:ro`, `:rb`, `:np` | Familiar built-in aliases, retained after discovery. |
| `:v1/pods`, `:apps/v1/deployments`, `:deployments.apps` | Qualified resource navigation; only catalogued versions are available. |
| `:pod NAMESPACE`, `:pod -n NAMESPACE`, `:pod all` | Namespace selection. `all` and `*` mean all namespaces. |
| `:pod /REGEX`, `:pod app=web,tier!=batch`, `:pod 'app in (api,web)'` | Regex or actual server-side label selectors; selectors persist through watch reconnect/relist. |
| `:pod @CONTEXT` | Explicit context switch, retaining the requested resource. |
| `:ctx`, `:context`, `:contexts`, with optional name | Context picker or direct switch. Credentials and caches remain scoped to the chosen connection. |
| `:ns`, `:namespace`, `:namespaces`, with optional name | Namespace picker or switch. |
| `:a`, `:alias`, `:aliases`; Ctrl-A | Navigable resource/custom alias picker. |
| `:q`, `:q!`, `:qa`, `:Q`, `:quit`, `:exit`; Ctrl-C | Exit. An in-flight mutation must finish before exit so its outcome can be recorded. |
| `:?`, `:h`, `:help`; `?` | In-app command/key help. |
| `:pulse`, `:pulses`, `:hz`, `:pu` | Native health snapshot, including unavailable/truncated/unknown state. Not k9s's continuously animated pulse dashboard. |
| `:xray`, `:xrays`, `:xr`, `:x RESOURCE [NAMESPACE]` | Bounded UID ownership tree; Service-to-Pod label edges and Node-to-Pod field relationships. Text snapshot rather than an interactive graph. |
| `:workload`, `:workloads`, `:wk` | Mixed workload snapshot. Individual workloads remain accessible through their normal resource tables. |
| `:screendump`, `:screendumps`, `:sd`; Ctrl-S | Explicitly save/browse historical redacted resources. Private JSON storage and YAML preview; not k9s CSV files. Maximum 50 files, 256 KiB each; no automatic deletion. |
| `:can u:NAME`, `:can g:NAME`, `:can s:NAME` | Read-only declared Role/Binding evidence. No impersonation, external group inference or effective-authorization guarantee. `s:NAMESPACE:NAME` explicitly selects an SA namespace. |
| `:subjects`, `:users`, `:groups`, `:rbacs`, `:policies` | Read-only list of subjects named by RoleBindings (current namespace) and ClusterRoleBindings, with binding counts and roles; `/` filters, Enter opens that subject's `:can` evidence. Declared bindings only; group membership is not inferred. |
| `:dir`, `:dirs`, `:d`, `:ls [PATH]` | Bounded local manifest browser and redacted YAML/JSON preview. `a` on a manifest (or `:apply FILE`) reviews a server-side apply with `--write --allow-apply`; no manifest delete. `e` edits an existing selected resource spec in the built-in editor, with separate capability and review. |
| `:pf`, `:portforward`, `:portforwards` | View/cancel this session's reviewed loopback forwards. Up to eight independent TUI forwards; four simultaneous clients each; 1..300s per session. `j/k` select, `s` stops selected, `b` benchmarks it, `F` reviews another. Scope changes/exit cancel all. |
| `:be`, `:bench`, `:benchmark [REQUESTS CONCURRENCY [PATH]]` | Native HTTP GET benchmark of the selected listening forward (k9s uses `hey`): 127.0.0.1 only, up to 2000 requests, 20 connections and 60 s; reports throughput, status counts and p50/p90/p99 latency. |
| `:lint`, `:popeye`, `:pop` | Read-only misconfiguration report for the namespace (k9s popeye's role): requests/limits/probes, unpinned images, privileged or host-namespace Pods, single-replica or degraded Deployments, crash/pull loops, restarts, bare Pods, Services without Pods. `kborg lint --json` for scripts. |
| `:ext [NAME]` | Run a registered read-only extension on the selection (KBorg's plugin model; see below). |
| `:container`, `:containers` | Container drilldown for a selected Pod. |
| `:helm`; `:history`; `:helm-install`; `:helm-test`; `:helm-crds`; `:helm-crds-upgrade`; `:helm-crds-delete`; `:helm-crds-migrate` | Native Helm v3/v4 Secret/ConfigMap browsing with `--allow-helm`. List Enter/r → history; revision Enter → redacted stored overrides; d metadata, m resource identities, y safe YAML. Reviewed list i/u/Ctrl-D install/upgrade/uninstall and history r rollback require all three write/Helm flags and trusted Unix Helm 4.3.1. Deterministic local/OCI charts, opt-in reviewed hooks, t/release tests, c/CRD bootstrap and explicit schema update/delete/storage migration; no SQL storage. |
| `:logs LABELS`; workload/Service `l`/`p` | Fixed membership of up to eight Pods/32 ordinary-container streams in one namespace, using actual selectors and CronJob→Job controller UIDs. Range/reconnect retains captured identities; reopen to include new rollout Pods. `:logs-live [LABELS]` instead recaptures membership every 10s and announces joins, departures and waiting joins. |
| `:cow` | Local novelty view; never a process invocation. |

Helm lifecycle supports an explicitly reviewed subset, not arbitrary chart or
storage parity: exact Helm 4.3.1 on Unix, deterministic local/OCI packaged charts, separately reviewed hooks/tests and namespaced-schema CRD operations. Other cluster/system targets and ordinary cross-namespace release resources, recognized GitOps resources and foreign adoption remain unsupported. Image vulnerability scans have no native equivalent. Ordinary
Kubernetes Roles/Bindings are browsable, and `:can` supplies explicit-subject
evidence. Generic Secret YAML/describe views remain unsupported by design (`:secrets` lists keys and reveals one value at a time). Node shell, file and directory copy, server-side apply, benchmarks and plugins have reviewed KBorg equivalents described below. Existing-resource namespaced YAML spec editing and reviewed node cordon/uncordon/drain are implemented in the [Mac v1 workflow batch](MAC_V1_OPERATIONS.md). Where KBorg's behavior differs, it is stricter, never an alternate
meaning for the same command.

Copilot is a KBorg-specific extension: `:copilot` previews selected minimized status and, with an explicit macOS ARM64 policy, requests read-only advice after full account-bound review. It does not redefine k9s resource commands.

## Navigation and contextual keys

`:` opens the command prompt. Tab completes, Up/Down recall in-session commands, and Enter runs the command. Esc cancels input or returns to the previous view. `[`/`]` navigate backward/forward, and `-` swaps to the last view. `/` applies a regex; `!` inverts it, `-f` requests fuzzy matching and `-l` applies a Kubernetes label selector. Filtering retains UID-based selection when canceled.

Enter drills from Pods to containers, from supported workloads/Services to selector-matched Pods, from Nodes to Pods using `spec.nodeName`, and from Namespaces to their Pod view. `d` describes, `y` views YAML, `l` opens logs and `p` opens previous logs. Ctrl-R refreshes. Shift-N/A/S sort name/age/status. C/M sort CPU/memory on Pods and Nodes; compact table values are rounded, while d/m and numeric sorting retain exact measurements. Missing samples are `-`; `~` identifies a creation-time checked sample whose API omitted its UID. Space marks rows (Ctrl-Space marks up to the cursor, Ctrl-\ clears). With marks, Ctrl-D, `r` on Deployments/StatefulSets/DaemonSets and `s` on CronJobs act on every marked object instead of the selected row, as in k9s, but through one reviewed batch: at most 50 objects of the current kind in the current context, each policy-checked and server-dry-run individually, listed with kind, namespace/name, UID and resourceVersion, and approved by typing the verb and exact count (for example `delete 7 objects`) within 60 seconds. Execution rechecks each target's UID and resourceVersion before its write, stops at the first failure and reports which objects succeeded, which failed and which were not attempted; one audit record is written per object. Marked CronJobs must all be active (suspend) or all suspended (resume). Other actions (scale, kill, trigger, logs, shell) still use the selected row. Clipboard copy uses an explicit `c` keypress and terminal OSC52 support.

Context matters: Pod/container `s` is shell and `a` is attach; scalable workload `s` opens scale and supported workload `r` prepares restart. `F` prepares port forwarding and `f` shows it. Node administration keys do not invoke unrelated actions. Reviewed operations retain KBorg's separate opt-ins, captured UID, dry-run and full target confirmation.

The log view supports `0` tail, `1` head, `2` one minute, `3` five minutes, `4` fifteen minutes, `5` thirty minutes and `6` one hour. `s` toggles autoscroll, `w` wrapping, `t` timestamps, `f` fullscreen, Enter filtering, Shift-C clearing and `m` a local mark. Range changes are separate bounded server reads. Pod logs select the valid default-container annotation when present, otherwise merge multiple ordinary containers. The picker has an All containers entry and log `a` toggles the Pod merged view. Workload/Service logs and `:logs LABELS` merge complete captured Pod memberships; group source toggling is disabled. All merges preserve source labels, share time/byte/rate/cache and global head/tail bounds, and exclude init/ephemeral containers. New rollout Pods require an explicit new capture unless the session was opened with `:logs-live`, which recaptures membership every 10 seconds within the same caps. Reconnection and periodic group checks verify UIDs; bounded retries explicitly warn that overlap/restarts/rotation may duplicate or omit records. k9s plugin-specific log behavior remains unsupported.

Helm reviews expire after 15 minutes, require every wrapped review line to be
displayed and exact `namespace/release` confirmation, and revalidate the plan,
history and live UID/resourceVersion before execution. This differs from an
unrestricted Helm terminal. See [HELM_LIFECYCLE.md](HELM_LIFECYCLE.md) for the
supported subset and post-check race/partial-write limits.

Additional KBorg views use explicit commands such as `:themes`, `:metrics`, `:metrics resources`, `:overview`, `:actions` and `:discover` so they do not take over contextual k9s keys.

## Custom aliases

Place this data-only file at `aliases.yaml` inside the KBorg configuration directory (`KBORG_CONFIG_DIR` can select that directory):

```yaml
aliases:
  pp: v1/pods
  apis: "pp 'app in (api,web)'"
  prodpods: "pod default @production"
```

Aliases return to the command parser and never execute a shell. Bounds are 64 KiB, 256 aliases, 2,048 expanded command bytes and 16 expansion steps. Cycles, duplicate keys, YAML anchors and replacement of identity/exit commands are rejected. Context-specific k9s alias files and plugin files are not imported automatically.

## Pod file copy

k9s relies on `kubectl cp` through plugins. KBorg has a reviewed single-file copy behind `--write --allow-file-copy`: `:cp-from /PATH [LOCAL_FILE_OR_DIR]` and `:cp-to LOCAL_FILE /PATH` act on the selected Pod's default container (its `kubectl.kubernetes.io/default-container` annotation, otherwise the first container); `kborg cp-from POD CONTAINER /PATH LOCAL` and `kborg cp-to POD CONTAINER LOCAL /PATH` choose any container. Downloads default to `~/Downloads`. Neither side ever replaces an existing file, transfers are capped at 64 MiB, and the container needs `cat` and `sh`. `-r` (CLI `--recursive`) copies a directory as a strict tar stream: regular files and directories only; links, devices, absolute paths and `..` are refused; uploads create a new container directory and verify the file count. See [SECURITY.md](../SECURITY.md).

## Debug containers

`:debug [CONTAINER]` on a Pod (or `kborg debug POD [CONTAINER]`) with `--write --allow-debug` adds an ephemeral debug container, KBorg's `kubectl debug -it`. The review shows the container after a server dry run; on approval KBorg adds it, waits until it runs and opens `sh -i`. With a target container it shares that container's process namespace. The image is `--node-shell-image` (pinned busybox by default).

## Node shell

k9s's node shell creates a privileged Pod on the Node. KBorg does the same behind `--write --allow-node-shell`: `s` on a Node (or `kborg node-shell NODE`) shows the complete Pod (host PID/network/IPC, privileged, pinned busybox, override with `--node-shell-image`) after a server dry run, and on `shell NODE` creates it in the current namespace (or `default`), waits for it and opens `nsenter -t 1 … /bin/sh -i`. The Pod is deleted when the session ends and has `activeDeadlineSeconds`, so it also ends if KBorg is killed. Pod Security admission or RBAC may refuse it; choose a namespace that permits privileged Pods.

## Apply

`:apply FILE`, `a` on a manifest in `:dir`, or `kborg apply FILE` (`--write --allow-apply`) server-side-applies up to 16 namespaced objects with field manager `kborg`. Every object is dry-run first; the review lists the fields a create sets and every changed path of an update (values under data, env and credential-like keys are hidden). Execution rechecks each object and writes nothing if one changed; updates are locked to the reviewed resourceVersion; field-manager conflicts are reported and never forced. Secrets, cluster-scoped kinds, `kube-system`/`kube-public`/`kube-node-lease` and GitOps-managed objects are refused.

## Plugins and extensions

k9s plugins run shell commands with `$NAME`-style substitution. KBorg's equivalent is a registered read-only extension: an absolute executable with literal arguments that receives the selected object as redacted JSON on stdin after a whole-input review (`RUN TOOL`), with a 15-second limit and captured output that is never applied. Register with `kborg extensions register`, run with `:ext NAME` (`--allow-extensions --allow-tools`), and bind a key in `hotkeys.yaml` (`command: ext NAME`). `kborg extensions import-k9s ~/.config/k9s/plugins.yaml` reports which k9s plugins fit this model, prints their register commands and a hotkeys snippet, and explains the rest.

## Custom views (columns and sort)

`views.yaml` in the configuration directory uses the k9s format, so an existing k9s file works:

```yaml
views:
  v1/pods:
    sortColumn: RSTS:desc
    columns:
      - NAMESPACE
      - NAME
      - READY
      - STATUS
      - RSTS
      - IMAGE:.spec.containers[0].image
      - TEAM:.metadata.labels['team']|W
      - AGE
  apps/v1/deployments:
    columns: [NAME, READY, IMAGES, AGE]
```

Keys can be `v1/pods`, `apps/v1/deployments`, `deployments.apps` or `deployments`. Built-in columns: `NAMESPACE`, `NAME`, `READY`, `STATUS`, `RSTS`, `AGE`, `NODE`, `IP`, `CPU`, `MEM`, `LABELS`, `IMAGES`. A custom column is `TITLE:.json.path`: dotted fields, `[N]` indexes, `[*]` for every element and `['key.with.dots']`. Attributes after `|`: `W` shows the column only in wide mode (Ctrl-W), `H` hides it, `N` sorts it numerically, `T` shows a timestamp as an age. Paths are only read, never evaluated as code; bounds are 64 KiB, 128 views and 24 columns per view. Columns that don't fit are dropped from the right; NAME always stays.

Sorting: `N` `A` `S` `R` `C` `M` sort by name, age, status, readiness, CPU and memory, and pressing the same key again reverses the order. `:sort COLUMN [asc|desc]` sorts by any column in the header, including custom ones. The header marks the sorted column with `^` (ascending) or `v` (descending). A view's `sortColumn` applies when you open that resource.

Other k9s table keys: Ctrl-W wide columns, Ctrl-Z (or `:faults`) only objects that need attention, `0` all namespaces and `1`–`9` your nine most recently used namespaces in this context. `:ctx` lists recently used contexts first.

## CronJobs and rollbacks

On CronJobs, `t` (or `:trigger`, `kborg trigger`) creates a Job now from the jobTemplate, owned by the CronJob, and `s` suspends or resumes the schedule (`:suspend`, `:resume`). `:rollback [REVISION]` (or `kborg rollback`) restores a Deployment, StatefulSet or DaemonSet to a previous revision, by default the one before the current, showing the image change in the review. All of these need `--write`, are dry-run first and are locked to the reviewed UID and resourceVersion.

## Custom hotkeys

`hotkeys.yaml` in the same directory uses the k9s format, so an existing k9s `hotkeys.yaml` can be copied in unchanged when its shortcuts are in the supported set:

```yaml
hotKeys:
  shift-0:
    shortCut: Shift-0
    description: Viewing pods
    command: pods
  shift-1:
    shortCut: Shift-1
    description: Production deployments
    command: "dp @production"
  f5:
    shortCut: F5
    description: Nodes
    command: no
```

A hotkey types its `command` as if it were entered after `:`, so aliases, completion rules and every review gate still apply; nothing runs a shell. Hotkeys fire only in the main resource view, never while a prompt, filter, form or review is open. Supported shortcuts are `Shift-0`…`Shift-9`, `Alt-<letter or digit>`, `F1`…`F12`, and `Ctrl-<letter>` except keys KBorg or terminals already use (`a c d e f h i j k m n p q r s u w z`). `keepHistory` and `override` are accepted and ignored. Bounds are 64 KiB and 64 hotkeys; unknown fields, duplicate shortcuts, YAML anchors and control characters are rejected, and an invalid file shows a notice while built-in keys keep working. `Ctrl-A` (`:aliases`) lists the loaded hotkeys. `Shift-N` matches the US-layout symbol (`)`, `!` … `(`) or a terminal that reports Shift with the digit.

## Custom keymap

Every built-in key is a named action in one registry: a name, the view it works in, its default keys and a one-line description. `?` opens help for the view you are in (the resource table of the current kind, logs, dashboard, port forwards, Pod containers, describe/YAML documents, metrics); `Tab` inside help switches to every key of every view plus the command reference, and `/` filters. `:keymap` (or `:keys`, optionally `:keymap TEXT`) lists every action with its keys and view. The bottom hint rows, the dashboard legend and help are generated from the registry with your actual keys, and remapped keys are marked `*`. `hotkeys.yaml` entries are listed too.

`keymap.yaml` in the same directory remaps actions. It is data only and can move keys between registered actions; it never adds behavior:

```yaml
keys:
  logs: L                  # table: follow logs on L instead of l
  logs.previous: [p, Alt-p] # a list binds several keys
  sort.name: Shift-X
  delete: [Ctrl-X, Delete]
  toggle.wide: Alt-w
  ns.recent: [Alt-1, Alt-2, Alt-3, Alt-4, Alt-5, Alt-6, Alt-7, Alt-8, Alt-9]
  log.wrap: Alt-w          # other views use a prefix: log., dashboard., forward., ...
  quit: []                 # an empty list unbinds an action (:q still quits)
scopes:                    # optional per-view sections without the prefix
  dashboard:
    tail: T
  editor:
    review: Ctrl-R         # the YAML editor types plain keys; only Ctrl/Alt/F keys bind
```

Keys are written `L`, `Shift-N`, `Ctrl-D`, `Alt-x`, `F1`…`F12`, `Space`, `Ctrl-Space`, `Delete`, `Backspace`, `Tab`, `Shift-Tab`, arrows, `PgUp`, `PgDown`, `Home` or `End`. A binding replaces all of the action's default keys, and the replaced keys stop working in that view. Views: `global` (`help`, `command`), `table`, `log`, `dashboard`, `forward`, `containers`, `detail` (describe, YAML and documents; also active in `overview`, `metrics` and `help`), `editor` and `help`. Table actions that share a letter across resource kinds (`s` is shell on Pods, node shell on Nodes, suspend on CronJobs and scale on workloads) are separate actions and can be remapped independently.

Validation is strict: unknown actions or views, unknown fields, the same key twice in one view (including against defaults you kept), more than 9 keys per action, YAML anchors, duplicate YAML keys, more than 256 bindings or 64 KiB are rejected, as is a remapped table key that `hotkeys.yaml` already uses. Esc, Enter, Ctrl-C, Ctrl-] and the Ctrl keys terminals fold into Enter/Tab/Backspace/Esc can never be bound, and review, approval, command-prompt and filter keys are fixed (shown in help as `(fixed)`), so remapping can never weaken a review or a typed confirmation. An invalid file shows a notice at start-up and KBorg keeps the default keys. Helm release views and forms keep their built-in keys.

The broader release gates, platform coverage and external integrations remain in [REMAINING_WORK.md](REMAINING_WORK.md). Current validation is in [HELM_COMPLETION_ACCEPTANCE.md](HELM_COMPLETION_ACCEPTANCE.md); historical [Helm/Pod-log/metrics](HELM_LOGS_METRICS_ACCEPTANCE.md) and [navigation](K9S_ACCEPTANCE.md) reports preserve their original scope.

## Mac v1 workflow additions (2026-10-23)

`e` opens the existing-resource YAML spec editor; Ctrl-S prepares the diff and full painted review. With `--allow-external-editor`, Ctrl-E in that editor hands the draft to `$VISUAL`/`$EDITOR` (k9s `e` behavior); the edited text returns to the same validation and review. `:edit-container` retains the focused image/resource form. Node `c/u/r` reviews cordon/uncordon/drain. `--write` and each separate capability are required. `:export` prepares selected redacted resource YAML; log/Helm Ctrl-S opens a private export preview. Log `e` and `W` toggle word-based error/warning filters; `w` remains wrap. These labels identify text matches, not inferred severity or diagnosis.

The compact top menu preserves command names and accepts mouse navigation. Exact supported scopes, bounds, cancellation and remaining gaps are in [Mac v1 operations](MAC_V1_OPERATIONS.md).


## Dashboard and saved selections

The persistent header displays the view title and `?`, without colon-command labels. Typed commands and completions remain supported. `D` opens the current context dashboard; card `1..4`/arrows focus, Enter opens the focused context's Pods, `c` chooses contexts and `?` shows the legend. These dashboard keys are KBorg additions, scoped to that view.

Resource `b` or dashboard `s` explicitly saves navigation; `B` and `:selections` reopen it, and resource `:save` is the command equivalent. Reopen checks active kubeconfig membership, destination resource discovery and any saved workload UID. Credentials, cached filters and operation approvals are not restored. See [dashboard guide](DASHBOARD.md) and [acceptance](DASHBOARD_ACCEPTANCE.md).
