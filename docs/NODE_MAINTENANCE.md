# Node maintenance

KBorg can cordon, uncordon and evacuate reviewed workloads from a node. These operations require both `--write` and `--allow-node-maintenance`. Cluster RBAC must separately permit the reads, node patch and Pod eviction operations; KBorg does not install permissions.

```sh
kborg --context docker-desktop --write --allow-node-maintenance cordon NODE
kborg --context docker-desktop --write --allow-node-maintenance uncordon NODE
kborg --context CONTEXT --write --allow-node-maintenance drain NODE 120
```

Use the selected Node's actions in the TUI for the same reviewed operations. The CLI requires an interactive terminal. Review the complete target and effects, then type `nodes/NAME`. Approval expires after 60 seconds.

## Scheduling changes

Cordon sets `spec.unschedulable` to true; existing Pods continue running. Uncordon permits new ordinary scheduling. Neither action prevents direct Pod bindings or Pods that tolerate unschedulable nodes.

Before approval, the API server receives a dry-run JSON patch. The review binds the Node UID and its complete non-status content. Kubernetes status heartbeats may refresh the resourceVersion without invalidating the approval; metadata or spec changes require a new review. The actual patch tests the bound UID and the freshly read resourceVersion. The API response is checked for additional admission changes. A failure after submission can have an unknown or partial outcome and must be inspected before retrying.

## Drain behavior

Drain inventories Pods on the selected node across **all namespaces**, validates their controllers, cordons the node and submits explicit `policy/v1` Eviction requests. Each eviction includes the reviewed Pod UID and resourceVersion. KBorg never falls back to direct Pod deletion, reduces termination grace periods or bypasses PodDisruptionBudgets. A PDB refusal stops the operation with an incomplete result; KBorg does not automatically retry.

The [Kubernetes Eviction API](https://kubernetes.io/docs/concepts/scheduling-eviction/api-eviction/) applies the Pod's disruption budget and termination grace period. Accepted eviction is distinct from completed termination: KBorg waits until the reviewed Pod UID disappears and then checks the node's complete inventory again.

The review lists every Pod as either `EVICT` or `RETAIN`:

- Mirror Pods, DaemonSet Pods, completed Pods and Pods in `kube-system`, `kube-public` or `kube-node-lease` are retained explicitly.
- Unmanaged Pods, unsupported or missing controllers, terminating eligible Pods and critical-priority application Pods block the review.
- Eligible Pods using `emptyDir`, `hostPath` or generic ephemeral volumes block the review. There is no data-loss override.
- Supported application controllers are ReplicaSets, StatefulSets, Jobs and ReplicationControllers, with matching current controller UIDs.

A successful result means the reviewed eligible workloads were evacuated. Retained system or DaemonSet Pods still need separate maintenance planning. KBorg does not prove replacement capacity, PVC relocation, workload availability or that a node can safely be powered off. A single-node cluster can lose application availability. Consult the [Kubernetes node drain guidance](https://kubernetes.io/docs/tasks/administer-cluster/safely-drain-node/) when planning maintenance.

## Bounds and recovery

The complete Pod inventory is limited to 1,000 Pods, 10 pages and 8 MiB. Each operation supports at most 64 eligible evictions. The drain deadline is 1–300 seconds, defaulting to 120 seconds in the CLI/TUI. Individual API requests are also bounded.

Drain **never automatically uncordons the node**, including after a PDB refusal, timeout or partial failure. The result reports the scheduling state, confirmed evacuations, retained Pods and pending targets. A scheduling state of `unknown` requires inspection. Accepted evictions can continue after the client exits.

The private audit spool records durable node-operation and per-eviction intents before writes. An accepted eviction without confirmed disappearance remains pending. `kborg audit pending` presents incomplete/unknown records for reconciliation; it does not retry mutations. Review the live node and affected Pods before explicitly uncordoning or preparing another drain.

## Acceptance coverage

`scripts/test-node-maintenance.py` provides two modes:

- `review-only`: Docker Desktop node dry-run and canceled approval; verifies its scheduling spec is unchanged.
- `owned-worker`: dedicated `kborg-v1-m02` minikube worker only; rejects unrelated workloads, creates an isolated Deployment/PDB, checks cordon and uncordon, proves PDB refusal, performs successful eviction, restores scheduling and removes its fixture namespace.

Test audit records use a private `.local/node-acceptance-*` directory, separate from personal settings. Mock-client tests additionally cover wire preconditions, status-only refresh, drift, admission changes, retained/blocked Pod classifications, deadlines, audit persistence and newly bound Pods after eviction.
