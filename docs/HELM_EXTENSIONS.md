# OCI charts, signatures, release tests and CRDs

These workflows extend the reviewed [Helm lifecycle](HELM_LIFECYCLE.md). They use
the same explicit trusted Helm 4.3.1 executable, complete review, exact
`namespace/release` confirmation and final drift checks. Explicit CRD deletion
requires the stronger `DELETE CRDS namespace/release` phrase. No capability is enabled
by a chart or configuration file.

## OCI charts

Chart operations accept a local `.tgz`, an explicit OCI tag, or an immutable OCI
reference:

```sh
kborg --context docker-desktop -n staging \
  --write --allow-helm --allow-helm-write --helm-tool /absolute/path/to/helm \
  helm install api oci://registry.example.com/charts/api@sha256:FULL_64_HEX_DIGEST
```

For example, `oci://registry.example.com/charts/api:1.2.3` resolves the tag once
before review. The review shows the requested reference and resolved digest. A
tag moved during confirmation does not change the sealed chart; a fresh review
resolves the tag again. There is no implicit `latest` or version-range resolution.

Obtain pinned digests from a trusted publisher. KBorg downloads the OCI manifest,
config and chart with a separate native HTTPS client before review, verifies
their declared SHA-256 digests and sizes, and seals a private chart snapshot.
The review includes the registry reference, manifest digest and chart layer
digest. Execution uses that snapshot without contacting the registry again.
Integrity against a supplied digest does not establish publisher identity or
verify a provenance signature unless `--helm-keyring` is selected.

HTTP, TLS bypass, OCI indexes, external descriptor URLs, automatic
dependency downloads, Helm/Docker credential stores and registry login helpers
are unsupported. Dependencies must already be packaged. Limits include 60
seconds overall, 10 seconds per response/body idle period, three redirects,
256 KiB manifest/config and an 8 MiB chart. Public blob CDN redirects require
verified HTTPS; authorization is stripped across origins. The redirect guard rejects literal private addresses and recognized local hostnames from public origins; it does not resolve DNS to enforce a network egress policy. This is not an OS/network sandbox.

For a private registry, supply an absolute private JSON file with
`--helm-registry-auth /absolute/path/auth.json`. Its permissions must be 0600 or
0400, its `registry` must exactly match the reference host and optional port,
and it must contain exactly one credential form:

```json
{"registry":"registry.example.com","username":"reader","password":"TOKEN"}
```

```json
{"registry":"registry.example.com","token":"REGISTRY_SCOPED_BEARER_TOKEN"}
```

Anonymous Bearer challenge exchange and same-origin username/password token
exchange request only the selected repository's pull scope. Private credentials
are never sent to another token-service origin; use a registry-scoped token for
that setup. Token endpoint redirects are rejected. Registry credentials are
separate from Kubernetes credentials and never passed to the Helm child.
`--helm-registry-ca /absolute/path/public-ca.pem` supplies a verified private CA
bundle for the exact registry origin. No insecure mode is provided.

## Chart signatures

Pass `--helm-keyring /absolute/path/trusted-public.gpg` to require standard Helm
OpenPGP provenance. The keyring is an explicit trust decision: every public key
in it is trusted. Binary exported OpenPGP public keyrings are the validated format. Bounded
public-only packet framing also recognizes armored public blocks and OpenPGP
keyboxes; the exact trusted Helm verifier must accept their format. Secret-key
packets are rejected before handoff. No personal/default keyring, keyserver, login or network trust
resolution is used. This verifies Helm `.prov` signatures, not Sigstore/Cosign
transparency-log attestations.

The keyring applies to acquired chart archives (install, upgrade and explicit schema
operations). Stored-release test, rollback and uninstall do not reacquire an
archive or reverify provenance; their reviews say this explicitly.

For a local `api-1.2.3.tgz`, place `api-1.2.3.tgz.prov` beside it. OCI charts use
the manifest's Helm provenance layer. The exact trusted Helm executable verifies
the signed archive filename and content before rendering. Missing provenance,
wrong keys, corrupt signatures or content mismatches stop preparation. Private
snapshots bind chart, provenance and keyring bytes through execution. The review
reports the verified signer fingerprint and trusted keyring/provenance hashes; it never treats
an unverified digest as a publisher signature.

```sh
kborg --context docker-desktop -n staging \
  --write --allow-helm --allow-helm-write --helm-tool /absolute/path/to/helm \
  --helm-keyring /absolute/path/trusted-public.gpg \
  helm install api oci://registry.example.com/charts/api:1.2.3
```

## Hooks

Add `--allow-helm-hooks` to the existing lifecycle flags. If a strict operator
configuration exists, it must also permit `allow_helm_hooks: true`. The review
shows each selected event, weight, object identity, readiness requirement and
before-creation/success/failure deletion policy, followed by redacted manifests.

Install, upgrade, rollback and uninstall support their pre/post hook events.
Hook Jobs and Pods must complete successfully. Other supported namespaced kinds
are considered ready when loaded by Helm. Hook workloads may affect external
systems; cancellation cannot reverse those effects. Retained hooks are not
ordinary release resources and may survive uninstall. Helm 4.3.1 hook deletion
policies take precedence over a hook's keep annotation.

Hooks must declare these ownership markers in their templates:

```yaml
metadata:
  labels:
    app.kubernetes.io/managed-by: Helm
  annotations:
    meta.helm.sh/release-name: {{ .Release.Name }}
    meta.helm.sh/release-namespace: {{ .Release.Namespace }}
```

At most 32 hooks are accepted. Cross-namespace, cluster-scoped, recognized GitOps,
generated-name and foreign-owned hooks are refused. Existing hook deletion
requires the stored reviewed template and matching live object, with UID/RV
rechecked before execution. The stored hook array order and actual event, weight
and deletion fields are sealed, including stable ordering for equal-weight/name
hooks. Replacing an existing hook must also pass a conservative update dry-run;
a changed immutable field can require a new hook name. Ordinary-resource/hook identity collisions are
rejected. Automatic hook-log output is unsupported.

`helm test RELEASE` and `t` in the release table (or `:helm-test` with a selected
release) run the stored `test`/legacy `test-success` hooks with
`--allow-helm-hooks`. The reviewed plan includes execution/deletion policies and
existing hook replacement. Tests do not create a new release revision or run
normal install/upgrade hooks. Completed stored test results are inspected; a
failed test is reported without claiming that the release deployment failed.
No automatic retry, log dumping or ordinary-resource deletion is requested.

## CRDs and custom resources

CRDs add APIs to the cluster. A dry-run cannot make a new API available for
validating a custom resource, so KBorg uses two separate reviews:

```sh
# 1. Review/create missing schemas and wait for Established.
kborg --context docker-desktop -n staging \
  --write --allow-helm --allow-helm-write --allow-helm-crds \
  --helm-tool /absolute/path/to/helm \
  helm crds api /absolute/path/api.tgz

# 2. Prepare a fresh release review against the registered APIs.
kborg --context docker-desktop -n staging \
  --write --allow-helm --allow-helm-write --allow-helm-crds \
  --helm-tool /absolute/path/to/helm \
  helm install api /absolute/path/api.tgz
```

Both steps also accept OCI tags or digests, with optional required signatures. Add `--allow-helm-hooks` if the
chart contains hooks. In the UI, `:helm-crds` or `c` in the Helm table opens the
schema form. After schema creation, use `i` to install the release. The schema
step does not create a Helm release record. Existing matching schemas are
retained; this bootstrap action never upgrades or deletes a CRD. Matching compares the complete
specification, allowing only documented Kubernetes defaults and empty optional
fields; extra schema properties or validation rules are rejected.

The explicit `--allow-helm-crds` capability (and matching configuration
permission) is required for schema creation and lifecycle use of custom
resources. Supported CRDs are structural `apiextensions.k8s.io/v1` schemas for
**Namespaced** resources, at most 16 per chart. Conversion webhooks,
cluster-scoped custom resources, reserved Kubernetes API groups, arbitrary
finalizers and templated CRDs are unsupported. Schema creation is cluster scoped
even though the custom resources are namespaced; the review states this.

Custom resource resolution reads only the selected group/version discovery and
the exact `plural.group` CRD, rather than listing all schemas. Established state,
schema UID/RV and matching served version/kind/plural are bound to the release
review. The Kubernetes API performs resource admission dry-runs. Schema drift
requires a fresh review. CRDs remain installed after release rollback/uninstall;
deleting a schema can erase instances and requires the distinct explicit workflow below.

## Explicit schema upgrade and deletion

`helm crds-upgrade RELEASE CHART` and `:helm-crds-upgrade` review the existing and
desired schema separately from release upgrade. Supported changes expand or
relax a schema without migrating its API identity, served/storage versions,
subresources or defaults. Complete bounded cluster-wide instance inventories
are sealed with UID/resource versions; incomplete/truncated/forbidden reads
refuse preparation. The update preserves metadata/identity and uses native
server dry-run and optimistic concurrency. Incompatible schema changes and
storage/version migrations require a dedicated migration workflow.

`helm crds-delete RELEASE CHART` and `:helm-crds-delete` are destructive schema
operations, never part of ordinary release uninstall. The review includes the
cluster-wide schema and instance impact. Empty schemas can be removed; nonempty
schemas require exact matching Helm release ownership on every instance, with
no foreign/GitOps/owner-reference/finalizer conflicts. Captured instances are
deleted with UID/RV preconditions, followed by a complete empty re-list and
UID/RV-guarded schema deletion. Type `DELETE CRDS namespace/release` only after
reviewing the complete impact. A concurrent writer can still create an instance
between the last check and schema deletion; use server RBAC/admission to quiesce
writes when that race is unacceptable. Partial effects require inspection.

These are KBorg native schema operations; Helm itself does not implement CRD upgrade or deletion. The instance inventory is limited to 1,000 objects and 8 MiB across the complete operation.

Both operations need `--write --allow-helm --allow-helm-write --allow-helm-crds`
and the trusted Helm tool for chart inspection. Schema update needs get/update permissions on each exact CRD; deletion needs exact-CRD delete and reviewed instance
delete permissions across the affected namespaces, plus complete instance
listing. No wildcard administrative role is installed by KBorg.

The short-lived test identity needs namespaced Helm/resource permissions and
`get` on each exact CRD. Bootstrap additionally needs CRD `create`; Kubernetes
RBAC cannot constrain `create` using `resourceNames`. Use an appropriate schema
administrator identity for that stage. Bootstrap does not need CRD update or deletion.

Helm still runs with the OS user's permissions and its own writes do not carry
the reviewed UID preconditions. Partial, canceled or timed-out writes report an
uncertain outcome and require inspection. These extensions do not add a
transaction, automatic rollback or general chart/platform parity.

Upstream semantics: [OCI registries](https://helm.sh/docs/topics/registries/),
[provenance](https://helm.sh/docs/topics/provenance/),
[chart tests](https://helm.sh/docs/topics/chart_tests/),
[hooks](https://helm.sh/docs/topics/charts_hooks/),
[CRD lifecycle and dry-run limits](https://helm.sh/docs/chart_best_practices/custom_resource_definitions/).
