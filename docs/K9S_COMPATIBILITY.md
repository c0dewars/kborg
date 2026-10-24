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
| `:dir`, `:dirs`, `:d`, `:ls [PATH]` | Bounded local manifest browser and redacted YAML/JSON preview. No arbitrary manifest apply/delete or external editor. `e` edits an existing selected resource spec in the built-in editor, with separate capability and review. |
| `:pf`, `:portforward`, `:portforwards` | View/cancel this session's reviewed loopback forwards. Up to eight independent TUI forwards; four simultaneous clients each; 1..300s per session. `j/k` select, `s` stops selected, `F` reviews another. Scope changes/exit cancel all. |
| `:container`, `:containers` | Container drilldown for a selected Pod. |
| `:helm`; `:history`; `:helm-install`; `:helm-test`; `:helm-crds`; `:helm-crds-upgrade`; `:helm-crds-delete`; `:helm-crds-migrate` | Native Helm v3/v4 Secret/ConfigMap browsing with `--allow-helm`. List Enter/r → history; revision Enter → redacted stored overrides; d metadata, m resource identities, y safe YAML. Reviewed list i/u/Ctrl-D install/upgrade/uninstall and history r rollback require all three write/Helm flags and trusted Unix Helm 4.3.1. Deterministic local/OCI charts, opt-in reviewed hooks, t/release tests, c/CRD bootstrap and explicit schema update/delete/storage migration; no SQL storage. |
| `:logs LABELS`; workload/Service `l`/`p` | Fixed membership of up to eight Pods/32 ordinary-container streams in one namespace, using actual selectors and CronJob→Job controller UIDs. Range/reconnect retains captured identities; reopen to include new rollout Pods. `:logs-live [LABELS]` instead recaptures membership every 10s and announces joins, departures and waiting joins. |
| `:cow` | Local novelty view; never a process invocation. |

Helm lifecycle supports an explicitly reviewed subset, not arbitrary chart or
storage parity: exact Helm 4.3.1 on Unix, deterministic local/OCI packaged charts, separately reviewed hooks/tests and namespaced-schema CRD operations. Other cluster/system targets and ordinary cross-namespace release resources, recognized GitOps resources and foreign adoption remain unsupported. Benchmarks (`:be`) and scans/Popeye do not have native equivalents. Ordinary
Kubernetes Roles/Bindings are browsable, and `:can` supplies explicit-subject
evidence. Generic Secret YAML/describe views, node shell, Pod file transfer/sanitization, arbitrary manifest creation/apply, k9s plugins and benchmark execution remain unsupported. Existing-resource namespaced YAML spec editing and reviewed node cordon/uncordon/drain are implemented in the [Mac v1 workflow batch](MAC_V1_OPERATIONS.md). These are material parity gaps, not alternate
meanings for the same commands.

Copilot is a KBorg-specific extension: `:copilot` previews selected minimized status and, with an explicit macOS ARM64 policy, requests read-only advice after full account-bound review. It does not redefine k9s resource commands.

## Navigation and contextual keys

`:` opens the command prompt. Tab completes, Up/Down recall in-session commands, and Enter runs the command. Esc cancels input or returns to the previous view. `[`/`]` navigate backward/forward, and `-` swaps to the last view. `/` applies a regex; `!` inverts it, `-f` requests fuzzy matching and `-l` applies a Kubernetes label selector. Filtering retains UID-based selection when canceled.

Enter drills from Pods to containers, from supported workloads/Services to selector-matched Pods, from Nodes to Pods using `spec.nodeName`, and from Namespaces to their Pod view. `d` describes, `y` views YAML, `l` opens logs and `p` opens previous logs. Ctrl-R refreshes. Shift-N/A/S sort name/age/status. C/M sort CPU/memory on Pods and Nodes; compact table values are rounded, while d/m and numeric sorting retain exact measurements. Missing samples are `-`; `~` identifies a creation-time checked sample whose API omitted its UID. Space marks rows; marks do not authorize bulk writes. Clipboard copy uses an explicit `c` keypress and terminal OSC52 support.

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

Aliases return to the command parser and never execute a shell. Bounds are 64 KiB, 256 aliases, 2,048 expanded command bytes and 16 expansion steps. Cycles, duplicate keys, YAML anchors and replacement of identity/exit commands are rejected. Context-specific k9s alias files, hotkeys and plugin files are not imported automatically.

The broader release gates, platform coverage and external integrations remain in [REMAINING_WORK.md](REMAINING_WORK.md). Current validation is in [HELM_COMPLETION_ACCEPTANCE.md](HELM_COMPLETION_ACCEPTANCE.md); historical [Helm/Pod-log/metrics](HELM_LOGS_METRICS_ACCEPTANCE.md) and [navigation](K9S_ACCEPTANCE.md) reports preserve their original scope.

## Mac v1 workflow additions (2026-10-23)

`e` opens the existing-resource YAML spec editor; Ctrl-S prepares the diff and full painted review. With `--allow-external-editor`, Ctrl-E in that editor hands the draft to `$VISUAL`/`$EDITOR` (k9s `e` behavior); the edited text returns to the same validation and review. `:edit-container` retains the focused image/resource form. Node `c/u/r` reviews cordon/uncordon/drain. `--write` and each separate capability are required. `:export` prepares selected redacted resource YAML; log/Helm Ctrl-S opens a private export preview. Log `e` and `W` toggle word-based error/warning filters; `w` remains wrap. These labels identify text matches, not inferred severity or diagnosis.

The compact top menu preserves command names and accepts mouse navigation. Exact supported scopes, bounds, cancellation and remaining gaps are in [Mac v1 operations](MAC_V1_OPERATIONS.md).


## Dashboard and saved selections

The persistent header displays the view title and `?`, without colon-command labels. Typed commands and completions remain supported. `D` opens the current context dashboard; card `1..4`/arrows focus, Enter opens the focused context's Pods, `c` chooses contexts and `?` shows the legend. These dashboard keys are KBorg additions, scoped to that view.

Resource `b` or dashboard `s` explicitly saves navigation; `B` and `:selections` reopen it, and resource `:save` is the command equivalent. Reopen checks active kubeconfig membership, destination resource discovery and any saved workload UID. Credentials, cached filters and operation approvals are not restored. See [dashboard guide](DASHBOARD.md) and [acceptance](DASHBOARD_ACCEPTANCE.md).
