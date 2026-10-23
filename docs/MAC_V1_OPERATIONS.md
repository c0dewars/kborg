# Mac v1 operator workflows

Development milestone, 2026-10-23. See [acceptance](MAC_V1_ACCEPTANCE.md) and [roadmap](MAC_V1_PLAN.md). This batch does not declare production certification or a signed 1.0 release.

## Start

```sh
./scripts/start-local.sh --context docker-desktop --allow-logs \
  --write --allow-yaml-edit --allow-node-maintenance --allow-forward --no-splash
```

Capabilities default off. An enabled capability does not bypass the review or Kubernetes authorization. Read-only browsing remains the default.

## YAML editing

Select an existing namespaced resource and press `e` or type `:edit`. Move with arrows/Home/End; Tab inserts spaces; bracketed paste supports multiline ASCII YAML. Ctrl-S prepares the server dry-run and readable diff. PgUp/PgDown reads the entire review; all wrapped lines must have actually been painted before typed target confirmation can execute. Esc returns to the draft; validation errors retain edits. Reviews expire after 60 seconds. `:edit-container` opens the original focused form.

Only spec paths change. The private raw baseline preserves omitted/redacted content; identity, status and metadata cannot be edited. No Secret editing, resource creation, ConfigMap data/RBAC editing, arbitrary cluster-scoped apply, or external editor. Unicode values use YAML escapes. See [YAML editing](YAML_EDITING.md).

CLI: `yaml-template RESOURCE NAME` emits the editable projection; `yaml-edit RESOURCE NAME FILE` loads it and requests terminal confirmation. Templates contain UID/version; stale templates are rejected.

## Nodes

In `:nodes`, `c` cordons, `u` uncordons, and `r` prepares a 120-second drain. Equivalent commands are `:cordon`, `:uncordon`, `:drain`. CLI supports `cordon NODE`, `uncordon NODE`, `drain NODE [SECONDS]`; drain budget 1..300 seconds.

Node identity and non-status content are bound to the review. Status-only heartbeat/version updates may be refreshed; the resulting write uses guarded current identity/version tests. Drain reviews a complete bounded node Pod inventory and uses native policy/v1 eviction, with Pod UID/version preconditions and no forced deletion. Disruption budgets can refuse the operation.

System, mirror, DaemonSet and completed Pods are explicitly retained. Unmanaged/unsupported controllers, critical user Pods and potentially lossy local storage block drain. Evacuation is bounded to reviewed eligible Pods; it is not proof that a node with retained Pods can be powered off. Newly bound Pods prevent a successful final evacuation result.

Partial failures leave the node cordoned. Read the outcome, reconcile pending audit entries and inspect workloads before another action. Uncordon is a separate reviewed operation. Interruptions do not authorize an automatic rollback or retry.

## Multiple port forwards

Select a live Pod and press `F`. Review loopback address, Pod identity, remote port and duration before confirmation. Local ports 1024..65535, remote ports 1..65535, duration 1..300 seconds.

`:pf` opens up to 8 concurrent sessions, each with up to 4 clients. `j/k` selects a session, `s` stops that session and `F` prepares another. A port remains reserved while its previous session is stopping. Context/resource-scope changes and exit stop every listener and client. The CLI `forward` command continues to run one reviewed session per invocation.

## Exports

- `:export` from a resource table: selected redacted YAML.
- Ctrl-S from logs: the current filtered retained window, with original timestamps/source attribution, without viewport wrapping/clipping. A paused view exports its frozen window.
- Ctrl-S from a Helm table: selected revision metadata and resource identities. Values, notes, raw manifests and chart files are excluded.
- Resource Ctrl-S retains the existing visible-resource snapshot workflow, opened with `:sd`.

The export preview shows destination, size and sensitivity notice. Enter saves; Esc cancels. Export files use the private KBorg configuration `exports/` directory, 0600 permissions on Unix, generated filenames and exclusive creation. Existing files are never overwritten. Maximum 512 KiB per export and 100 retained files; archive them manually when full. Application logs and custom fields may contain confidential data; redaction is not a universal classifier.

## Top menu and log filters

The top menu uses familiar `:pods`, `:deploy`, `:svc`, `:nodes`, `:helm`, `:pf`, `:pulses`, `:ctx` and `:themes` commands. Click with mouse enabled or type the command. Narrow terminals retain context/namespace/mode and shorten the menu.

In logs, `e` toggles error-word matches and `W` warning-word matches; enabling both includes either. Matches use whole ASCII words and can be combined with regex/fuzzy filtering. `w` still wraps. Matching lines receive semantic colors, but these are text hints, not an application diagnosis. Local retention/stream notices remain visible even when filtering.
