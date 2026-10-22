# k9s navigation compatibility

Reviewed against upstream k9s commit `e51ac53cb4c3a9a96666297581d309792f8faf8c` on 2026-10-22. The KBorg interface now uses a full-width resource table and command navigation, without a persistent sidebar. Familiar command names and resource shortcuts retain their purpose. This is a compatibility register, not a claim of complete k9s feature parity.

References: [command guide](https://k9scli.io/topics/commands/), [command interpreter](https://github.com/derailed/k9s/blob/e51ac53cb4c3a9a96666297581d309792f8faf8c/internal/view/cmd/types.go), [resource registry](https://github.com/derailed/k9s/blob/e51ac53cb4c3a9a96666297581d309792f8faf8c/internal/dao/registry.go), [default aliases](https://github.com/derailed/k9s/blob/e51ac53cb4c3a9a96666297581d309792f8faf8c/internal/config/templates/aliases.yaml), [browser keys](https://github.com/derailed/k9s/blob/e51ac53cb4c3a9a96666297581d309792f8faf8c/internal/view/browser.go).

## Commands

| k9s command family | KBorg behavior |
|---|---|
| `:pod`, `:pods`, `:po`; singular/plural/discovered short names | Browse resources. 35 built-in kinds plus bounded API discovery and CRDs. Ambiguous aliases require a qualified resource. Secrets remain excluded. |
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
| `:dir`, `:dirs`, `:d`, `:ls [PATH]` | Bounded local manifest browser and redacted YAML/JSON preview. No arbitrary manifest apply/delete or external editor. |
| `:pf`, `:portforward`, `:portforwards` | View/cancel this session's reviewed loopback forward. One active forward; four simultaneous clients; bounded duration. |
| `:container`, `:containers` | Container drilldown for a selected Pod. |
| `:cow` | Local novelty view; never a process invocation. |

`:helm`, Helm `:history`, benchmarks (`:be`), scans/Popeye, RBAC subject-list pseudo-resources and references do not have full native equivalents. They must not be presented as implemented integrations. Ordinary Kubernetes Roles/Bindings are browsable, and `:can` supplies explicit-subject evidence. Secret reads, force kill, node cordon/drain/shell, Pod file transfer/sanitization, arbitrary manifest editing, plugins and benchmark execution remain unsupported. These are material parity gaps, not alternate meanings for the same commands.

## Navigation and contextual keys

`:` opens the command prompt. Tab completes, Up/Down recall in-session commands, and Enter runs the command. Esc cancels input or returns to the previous view. `[`/`]` navigate backward/forward, and `-` swaps to the last view. `/` applies a regex; `!` inverts it, `-f` requests fuzzy matching and `-l` applies a Kubernetes label selector. Filtering retains UID-based selection when canceled.

Enter drills from Pods to containers, from supported workloads/Services to selector-matched Pods, from Nodes to Pods using `spec.nodeName`, and from Namespaces to their Pod view. `d` describes, `y` views YAML, `l` opens logs and `p` opens previous logs. Ctrl-R refreshes. Shift-N/A/S sort name/age/status. Space marks rows; marks do not authorize bulk writes. Clipboard copy uses an explicit `c` keypress and terminal OSC52 support.

Context matters: Pod/container `s` is shell and `a` is attach; scalable workload `s` opens scale and supported workload `r` prepares restart. `F` prepares port forwarding and `f` shows it. Node administration keys do not invoke unrelated actions. Reviewed operations retain KBorg's separate opt-ins, captured UID, dry-run and full target confirmation.

The log view supports `0` tail, `1` head, `2` one minute, `3` five minutes, `4` fifteen minutes, `5` thirty minutes and `6` one hour. `s` toggles autoscroll, `w` wrapping, `t` timestamps, `f` fullscreen, Enter filtering, Shift-C clearing and `m` a local mark. Range changes are separate bounded server reads. All-container merged streams and k9s plugin-specific log behavior remain incomplete. Reconnection is UID-checked, bounded and explicitly warns that overlap/restarts/rotation may duplicate or omit records.

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

The broader release gates, platform coverage and external integrations remain in [REMAINING_WORK.md](REMAINING_WORK.md). Current validation is recorded separately in [K9S_ACCEPTANCE.md](K9S_ACCEPTANCE.md).
