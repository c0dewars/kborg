# Helm lifecycle and workload logs

KBorg can prepare and execute Helm install, upgrade, rollback and uninstall from
its CLI or Helm view. Browsing remains native; lifecycle actions use an explicitly
selected trusted **Helm 4.3.1** executable on macOS/Linux x86-64 or ARM64.
They require `--write`, `--allow-helm`, and `--allow-helm-write` together. A strict
configuration file must separately permit `allow_helm` and `allow_helm_write` in
an `operator` profile; configuration never enables those capabilities by itself.

## Start and navigate

```sh
./scripts/start-local.sh --context docker-desktop -n YOUR_NAMESPACE \
  --write --allow-helm --allow-helm-write \
  --helm-tool /absolute/path/to/trusted/helm --allow-logs --no-splash
```

Type `:helm`. The release table keeps Enter/r for history, adds `i` for install,
`u` for upgrade and Ctrl-D for uninstall. In history, `r` rolls back to the selected
revision. Forms capture the release, namespace, local chart or digest-pinned OCI source and optional values
file. The `c` key and `:helm-crds` open a separate create-only schema review. Use the executable basename `helm`: native dry-run admission uses the `helm`
field manager, while upstream derives its manager from the executable basename.
Renamed executables are outside this validated workflow. The executable is
supplied when launching KBorg, not by chart data.

Preparation is read-only and runs asynchronously. Review the context, endpoint,
release/storage identity, exact inputs, affected resources, current objects and
desired objects. Secret data and known sensitive fields are redacted. The UI
requires every wrapped review line to have been displayed before confirmation;
scrolling straight to the end cannot skip the review. Type the exact
`namespace/release` and press Enter. Esc cancels before submission. Ctrl-C during
execution cancels the supervised process and reports the outcome.

Reviews expire after 15 minutes. Source changes, release/history changes,
resource replacements or resource-version changes require a new review. Other
native operation families retain their own shorter approval lifetimes.

## CLI examples

KBorg accepts a local `.tgz` or a digest-pinned OCI reference, with dependencies already bundled. OCI acquisition finishes and verifies every digest before review; execution uses a sealed local snapshot. The namespace must already exist. Hooks and create-only CRD bootstrap require separate opt-ins; see [HELM_EXTENSIONS.md](HELM_EXTENSIONS.md) for examples and boundaries.

```sh
kborg --context docker-desktop -n staging \
  --write --allow-helm --allow-helm-write --helm-tool /absolute/path/to/helm \
  helm install api ./api-1.0.0.tgz --helm-values ./staging.yaml

kborg --context docker-desktop -n staging \
  --write --allow-helm --allow-helm-write --helm-tool /absolute/path/to/helm \
  helm upgrade api ./api-1.1.0.tgz --helm-values ./staging.yaml

kborg --context docker-desktop -n staging \
  --write --allow-helm --allow-helm-write --helm-tool /absolute/path/to/helm \
  helm rollback api 1

kborg --context docker-desktop -n staging \
  --write --allow-helm --allow-helm-write --helm-tool /absolute/path/to/helm \
  helm uninstall api
```

Each command prints the complete review and requires terminal confirmation.
Piped confirmations and `--json` lifecycle actions are rejected. `--helm-storage
configmap` selects the alternative Kubernetes storage driver; Secret storage is
the default. Both stores must be readable during lifecycle preflight so a release
with the same name in the other driver cannot be confused with the target.
`--helm-timeout` sets the mutation budget, including uninstall disappearance
verification, to 1–900 seconds (default 300). Preparation and final validation
each have a separate 120-second ceiling.

## Scope and guarantees

The executable, chart and explicit values receive content hashes. Inputs are
copied to a private workspace and rechecked before execution. Helm receives a
private static kubeconfig containing only the selected verified connection;
original kubeconfig paths, exec helpers and inherited Helm endpoint overrides
are not forwarded. Helper-derived credentials retain their expiry/revocation
checks. The Helm process retains the operating system user's permissions; this
supervision is not an OS sandbox and does not impose an OS memory/CPU limit.
Use trusted chart sources. Local inputs must be regular, non-symlink files. Charts are
limited to 8 MiB compressed and 32 MiB expanded; reviews are limited to 512 KiB.

Supported charts contain at most 128 affected ordinary namespaced resources.
Explicit allowlisted families include core configuration/workload networking,
apps workloads, batch workloads, autoscaling, disruption budgets and namespaced
RBAC. The explicit extensions add reviewed hooks, a separate create-only CRD stage and schema-bound namespaced custom resources; see [HELM_EXTENSIONS.md](HELM_EXTENSIONS.md). Other cluster-scoped resources, system/cross-namespace targets, plugins, post-renderers, foreign-resource adoption and recognized GitOps-managed resources are rejected.

Time/random/credential-generating functions, cluster lookups and dynamic template
evaluation are rejected, as are unordered `keys`/`values` iteration and dynamic
or pointer-based `printf` formats. The conservative whole-template scans can
also reject harmless comments or variable names. This permits comparing the canonical rendered plan
again immediately before execution. Chart archive links, special entries,
traversal and excessive expansion are also rejected. Chart templates may still
contain confidential application configuration beyond known redaction fields.

The review combines Helm's server-connected simulation with native per-resource
API dry-runs. Server-side apply simulation is an admission/authorization check;
it is not a promise that Helm's merge behavior or later admission result is
identical. An existing field owned by another manager can fail this check even
when a Helm merge would overwrite it; KBorg does not force field ownership.
Existing resources and release history are revalidated before writes.
Helm's own API requests do not carry KBorg's captured UID preconditions, so a
concurrent replacement after the final check remains possible.

There is no automatic retry or automatic rollback. A timeout, cancellation,
connection failure or partial write can leave a changed or pending release.
Inspect `:helm`, revision history and `kborg audit pending` before another action.
KBorg writes a durable local intent before starting the mutation; an absent
successful outcome remains pending for manual reconciliation. Local audit files
are not a protected external audit service.

## Multi-Pod logs

Press `l` on a workload or Service to merge its matching Pods; `p` selects their
previous logs. CronJob capture follows controller UID relationships through Jobs.
To capture an explicit label selector, use `:logs app=api` in the current namespace.
CLI examples:

```sh
kborg --context docker-desktop -n staging --allow-logs logs deploy/api
kborg --context docker-desktop -n staging --allow-logs logs statefulset/database --follow
kborg --context docker-desktop -n staging --allow-logs logs --selector app=api --follow --log-seconds 60
kborg --context docker-desktop -n staging --allow-logs logs job/import --previous --tail 100
```

One capture contains 1–8 Pods and at most 32 ordinary-container streams. A partial
or oversized Pod list is rejected; narrow the workload or selector. Init and
ephemeral containers are excluded. Namespace, Pod, container and captured UID
identify the sources. Each source retains failures as attributed notices.

Membership stays fixed across range changes and reconnects. New rollout Pods are
not added automatically. Pod/workload UID replacement, deletion or authorization
loss stops the group. Periodic identity checks also protect quiet streams;
transient verification failures are reported and bounded. Reopen the workload or
selector explicitly to capture new Pods.

Opening concurrency is four. All streams share one session deadline, a 64 MiB
consumed-byte budget, 1 MiB/second and 5,000-lines/second guards. The displayed
window remains 256 KiB/2,000 lines, and head/tail applies across the complete group.
Snapshots order timestamped records; follow displays arrival order. Kubernetes
rotation and reconnects can still produce gaps or duplicates. A fixed membership
is a source identity guarantee, not a lossless distributed log system.

Single-Pod and selected-container syntax remains available. `--json` workload
snapshots and follow records identify `pod_scope: fixed-membership` and include
the captured membership. Logs can contain confidential application data.

## Reference and validation

The integration follows the upstream command contracts for
[install](https://helm.sh/docs/helm/helm_install/),
[upgrade](https://helm.sh/docs/helm/helm_upgrade/),
[rollback](https://helm.sh/docs/helm/helm_rollback/) and
[uninstall](https://helm.sh/docs/helm/helm_uninstall/).
Read the [dated acceptance report](HELM_LIFECYCLE_ACCEPTANCE.md) for exact binaries, tested environments and
remaining limitations; implementation does not establish independent security
certification or every Helm chart/platform combination.
