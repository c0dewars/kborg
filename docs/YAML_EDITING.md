# Reviewed YAML editing

Select a resource and press `e` to open the built-in YAML editor. Start KBorg with
`--write --allow-yaml-edit` to permit applying edits. The editor runs in memory;
it does not invoke a shell or save a temporary resource file. `Ctrl-S` prepares a
review, `Esc` cancels, and navigation keys move the cursor. `Tab` inserts spaces.
A failed preparation retains the text so it can be corrected.

The CLI supports the same workflow using an explicit local file:

```sh
kborg --context docker-desktop -n my-app yaml-template deployments api > api.yaml
# Edit api.yaml locally, then:
kborg --context docker-desktop -n my-app --write --allow-yaml-edit \
  yaml-edit deployments api api.yaml
```

Both paths show the changed JSON-pointer paths and old/new values, run server
admission in dry-run mode, and require the exact `namespace/name` confirmation.
The review expires after 60 seconds. The source must still have the same UID and
resourceVersion. Preparation and execution reject concurrent changes; reload the
template instead of retrying the old edit. Execution repeats admission validation
and sends atomic UID/resourceVersion tests with the actual JSON Patch. There is
no automatic retry, rollback, or readiness guarantee. A timeout may have an unknown
outcome; inspect the cluster before another write. Local audit records contain
identity and outcome, not manifest content.

## Scope

This edits `spec` on existing namespaced, non-Secret resources. Deployments,
StatefulSets, DaemonSets, CronJobs, Services, and suitable discovered custom
resources are supported, subject to Kubernetes immutability and admission rules.
It does not create resources or edit ConfigMap data, Secrets, RBAC objects without
a `spec`, cluster resources, system namespaces, terminating objects or resources
with recognized Argo CD / Flux ownership markers. Use `:edit-container` for the
smaller existing image/resource-budget editor.

The document is an editable projection backed by the private original object.
It is never used as a replacement manifest. Known sensitive fields appear as
`[REDACTED]`; they cannot be added, removed or changed through this editor. All
metadata and status are protected, including nested workload-template metadata.
Arrays with protected values cannot be resized or have their item identities
changed. Unchanged fields are preserved, and the patch contains only changed,
reviewed paths. This deliberately favors preservation over arbitrary edits.
Known-field redaction cannot identify every confidential business value in a
custom resource; inspect the resource type before opening it.

The editor and each local file are limited to 64 KiB, one document and 128 changed
fields. YAML aliases, anchors, duplicate keys and merge keys are rejected. Unicode
is displayed using explicit escapes to preserve exact values in the current
terminal renderer; enter Unicode using YAML escapes. Admission changes outside
the expected specification cause preparation to fail. Stateful admission can
still behave differently on the actual write, so server-side authorization and
admission policies remain the enforcement boundary.

## Acceptance test

`tests/manifest_edit_live.rs` is opt-in and restricted to `docker-desktop` and the
owned `kborg-v1` minikube context. Supply `KBORG_YAML_TEST_KUBECONFIG` and
`KBORG_YAML_TEST_CONTEXT`, then run:

```sh
cargo test --locked --test manifest_edit_live -- --ignored --nocapture
```

It creates a uniquely named namespace and a zero-replica Deployment, verifies
read-only/capability gates, dry-run non-persistence, a real reviewed spec edit,
hidden-field preservation and concurrent-edit rejection, and deletes its owned
namespace using a UID precondition. It never schedules the Deployment's Pods.
