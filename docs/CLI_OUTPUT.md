# KBorg read-output contract v1

This document stabilizes the existing 0.3 command-specific machine projections.
The contract version is documented here, not inserted into raw Kubernetes objects.
Consumers should accept additional object fields, avoid relying on key order, and
treat missing usage or count values as unavailable rather than zero. Changing the
meaning or required shape of these projections requires a new contract version.

Use `--json` only with a supported read command. Unsupported commands reject it
instead of silently emitting plain text. Diagnostics go to stderr; themes, splash
animations and ANSI styling never enter machine output.

| Command | stdout shape |
|---|---|
| `contexts --json` | Array of context-name strings; no connection required |
| `resources --json` | Array of discovered kind records: `plural`, `kind`, `group`, `version`, `namespaced`, `short_names`; Secrets and subresources excluded |
| `list RESOURCE --json` | Object containing `items` (redacted Kubernetes objects), `truncated` (boolean), and `source` (`cluster` or `demo`) |
| `get RESOURCE NAME --json` | One redacted Kubernetes object; `kind` and `apiVersion` supplied by the selected catalog |
| `watch RESOURCE --json` | JSONL snapshots: `items`, `resourceVersion`, `stale`, `truncated`, `terminal`, `status`; one complete JSON object per line |
| `metrics pods\|nodes --json` | `api_version`, `scope`, `namespace`, `source`, `items`, `truncated`; exact quantity details described below |
| `overview CONTEXT... --follow --json` | JSONL arrays of the complete selected1..4 context summary set; errors use null counts; independent watches stop on scope close |
| `extensions list --json` | Array of data-only registrations: id,absolute executable,literal arguments,SHA-256 |
| `overview CONTEXT... --json` | Array of independent context summaries: `demo`, `context`, `namespace`, `pods`, `unhealthy`, `truncated`, `error`, plus additive `state`, `observed_unix_seconds` and `health` evidence fields; see [health semantics](DASHBOARD_HEALTH.md) |
| `logs POD [CONTAINER] --json` | Object with `logs` (sanitized string) and `bounded: true` |
| `logs POD CONTAINER --follow --json` | JSONL objects containing `line` (sanitized string); follow remains opt-in and time/byte bounded |
| `logs RESOURCE/NAME --json` or `logs --selector LABELS --json` | Object with `logs`, `bounded: true`, `container_scope: all-ordinary`, `pod_scope: fixed-membership`, and `membership` described below |
| Workload/selector logs with `--follow --json` | JSONL objects containing `line`, `container_scope: all-ordinary`, `pod_scope: fixed-membership`, and the same captured `membership` on every record |
| `helm list\|history RELEASE --json` | Envelope: `schema_version: 1`, `kind: helm-list` or `helm-history`, `bounded: true`, and `data` containing `releases`, `truncated`, `issues`, `source` |
| `helm detail\|resources RELEASE REVISION --json` | Same envelope with `kind: helm-detail` or `helm-resources`; `data` contains selected `release`, safe `resources` identities, `resources_truncated`, `issues` |
| `helm values RELEASE REVISION --json` | Same envelope with `kind: helm-values`; `data` contains selected `release`, redacted stored override `values`, and sensitivity `notice` |
| `audit pending --json` | Array of local pending/uncertain operation records; this is not proof of server outcome |

Metrics rows contain `name`, `namespace`, `container`, `timestamp`, `window`,
`cpu_raw`, `memory_raw`, `cpu_nanocores`, `cpu_millicores` (exact decimal string),
`memory_bytes`, and `rounded_up`. Node rows have null namespace/container.
Timestamp/window come from the Metrics API. JSON integer consumers should use
integer-capable decoders for the full u64 range. TUI history is local transient
state and does not alter this projection or invent utilization percentages.

`get --yaml` returns readable quoted YAML from the same redacted object; it cannot
combine with `--json`. Text tables and human diagnostic strings may evolve and
are not intended for parsing.

Exit status **0** means the command completed, including a downstream reader
closing stdout normally. Status **1** means invalid arguments, denied access,
unsupported capability, transport/serialization failure or a bounded session
stopping with an error. A terminal watch failure can emit its final stale snapshot
before returning 1. Watch/follow stop reading immediately on a broken pipe.
Other finite read commands tolerate broken pipes without a panic. Real-process regression coverage includes the five new human-only navigation reads.

Redaction covers known fields, not every kind of confidential application data.
Logs remain sensitive and require `--allow-logs`. General resource projections exclude Secrets. The separate `--allow-helm`
capability permits narrowly projected Helm storage reads; it never emits raw
storage, chart files, notes, rendered Secret contents or credentials intentionally.

Opt-in `--metrics-resources` adds `allocation_status` and per-row `allocations`;
base output omits both. Comparisons contain current snapshot bases, exact amounts,
qualified generation status and checked two-decimal percentage strings with a
rounding indicator. Missing/zero/invalid/mismatched bases omit percentages. This
is usage versus current configuration, not proof of historical quota utilization.

`copilot preflight` always emits a local JSON report with `diagnosis_enabled:false`
and explicit blockers; it makes no inference/login request and reads no token
contents. Human local preview remains text-only.

Human-only navigation snapshots `pulses`, `xray RESOURCE`, `workloads`, `can SUBJECT` and `dir [PATH]` reject `--json`. `dir` is local and does not connect to Kubernetes. Their text is bounded and may evolve.

`list`/`watch` accept `--selector` (or `-l`) for Kubernetes label selection; the same selector is sent on each LIST page and WATCH/relist. Existing output shapes are unchanged. Log reads accept `--tail 1..2000`, `--head`, and `--since-seconds 1..86400`; head cannot combine with follow. Timestamped log text remains a sanitized string. Follow reconnects are bounded, retain the original session budget, and recheck Pod UID; duplicates or gaps are possible and explicitly reported.

`--all-containers` retains the existing logs JSON/string and JSONL shapes, adding
source labels within each sanitized line and `container_scope: all-ordinary`,
`namespace`, `pod`, and captured `uid` fields. It conflicts with an explicit container.
Per-source notices are included in the stream; a successful finite snapshot may
contain partial-data notices, so inspect them rather than assuming every source
succeeded. `truncated` and `issues` in Helm list/history similarly identify partial
storage reads. Duplicate Secret/ConfigMap release names require an explicit
`--helm-storage`; duplicate namespaces require narrowing `-n`. Detail/values bind
to the selected storage UID and reject replacement. Helm SQL storage remains
unsupported.

Workload/selector `membership` contains `namespace`, `description`, and `pods`;
each Pod has `name`, captured `uid`, and complete ordinary `containers`. Internal
connection and parent identity guards are not serialized. Capture is bounded to
eight Pods/32 total streams in one namespace and rejects incomplete lists.
Explicit container arguments are invalid for group logs; init/ephemeral sources
are excluded. Range changes and reconnects retain this membership, and newly
matching Pods are not silently added. Identity/deletion failures stop the group;
per-source endpoint failures remain attributed notices within sanitized text.
Snapshot head/tail applies once across all sources, not once per Pod.

Helm install/upgrade/rollback/uninstall/test/crds/crds-upgrade/crds-delete are human-reviewed commands, separately
requiring `--write --allow-helm --allow-helm-write` and trusted Unix Helm 4.3.1.
They reject `--json` and piped confirmations. Their review, fresh validation,
uncertain outcomes and supported chart subset are documented in
[HELM_LIFECYCLE.md](HELM_LIFECYCLE.md); they do not change the read envelopes above.
