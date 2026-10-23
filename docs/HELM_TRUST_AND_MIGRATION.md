# Chart trust and CRD storage migration

KBorg adds two explicit workflows to its reviewed Helm subset. Neither changes
normal release uninstall: schemas and custom data remain unless separately
reviewed for deletion. See [Helm lifecycle](HELM_LIFECYCLE.md) for the base
capabilities and concurrency limits.

## Sigstore verification of chart archives

`--helm-sigstore-policy /absolute/policy.json` requires verification of the exact
local or OCI-acquired chart archive before preparing the review and again before
execution. A failed or changed verifier, chart, policy, bundle or trust input
stops the operation. If `--helm-keyring` is also supplied, both verification
policies must pass.

The policy is strict JSON. Unknown and duplicate fields are rejected. Example:

```json
{
  "schema": 1,
  "executable": "/absolute/cosign",
  "sha256": "REPLACE_WITH_INDEPENDENTLY_VERIFIED_64_LOWERCASE_HEX_DIGEST",
  "bundle": "/absolute/chart.sigstore.json",
  "trust": {
    "mode": "public-key",
    "key": "/absolute/trusted-public.pem",
    "trusted_root": "/absolute/trusted-root.json"
  }
}
```

For certificate identity verification, replace `trust` with:

```json
{
  "mode": "identity",
  "trusted_root": "/absolute/trusted-root.json",
  "identity": "EXACT_CERTIFICATE_SUBJECT_ALTERNATIVE_NAME",
  "issuer": "https://EXACT_OIDC_ISSUER"
}
```

An optional top-level `expires_at` is a Unix timestamp. It expires the policy;
review lifetime remains bounded independently. Both modes require an explicitly
trusted root and verifiable transparency evidence. Identity mode also requires
exact identity/issuer and applicable certificate, SCT and time checks. Public-key
mode proves possession of the explicitly selected signing key, not a certificate
identity. The operator must establish the authority of the tool pin, root and
key independently. Policy/root/key files cannot be group/world writable.

Only native **Cosign 3.1.3**, an independently supplied SHA-256 executable pin,
and Sigstore v0.3 `messageSignature` bundles using SHA2-256 are accepted. Inputs
are bounded and privately sealed. The verifier receives a scrubbed environment
and a directory containing public verification inputs; no Kubernetes config or
registry credential is passed. Its invocations have bounded output/deadlines and
process cleanup. The trusted executable retains OS-user permissions: offline
verification is not an OS sandbox.

This verifies **chart archive bytes**. It does not discover or verify OCI
manifest/referrer signatures, DSSE attestations or arbitrary supply-chain policy.
A registry tag still resolves once to immutable content before review. Stored
release test/rollback/uninstall do not acquire an archive and explicitly report
archive verification as not applicable. No insecure verifier bypass, keyserver,
implicit trust-root discovery or automatic TUF refresh is used. Operators own
trust-root rotation and policy expiry.

Upstream references: [Cosign blob verification](https://docs.sigstore.dev/cosign/verifying/verify/)
and [Sigstore bundle format](https://docs.sigstore.dev/about/bundle/).

## Storage-version migration

Use `helm crds-migrate RELEASE CHART` with the existing
`--write --allow-helm --allow-helm-write --allow-helm-crds --helm-tool ABS` flags,
or open `:helm-crds-migrate` in the UI. Approval is
`MIGRATE CRDS namespace/release`. The namespace/release is an ownership and
confirmation label; schema and instance inventory spans all namespaces.

The supported subset promotes a storage version while preserving identical
structural schemas, `None` conversion, existing served versions, metadata and
instance content. Conversion webhooks, data transformations, removal of old
versions, cluster-scoped custom instances and arbitrary schema evolution are
refused. Existing instances need exact Helm release ownership and cannot be
GitOps-managed, controller-owned, terminating or protected by other finalizers.
The complete inventory is bounded to 1,000 instances and 8 MiB; an incomplete
inventory never becomes an executable plan.

The review includes current/desired schemas, instance UID/resourceVersion,
content, storage versions, temporary write markers and any official controller
object to be created. Final validation checks the sealed chart and live plan.
The native stage switches storage and sets/removes a temporary annotation via
UID/resourceVersion-bound writes through the target API. Both writes must
advance resourceVersion and preserve content/status. Reading an object with a
new `apiVersion` is conversion, and is never treated as evidence of disk storage.

KBorg discovers the official `StorageVersionMigration` API in `v1`, `v1beta1` or
`v1alpha1` form. When available, it creates the exact reviewed migration, waits
for a fresh matching-UID `Succeeded` result with a resource checkpoint, rechecks
the schema and complete instance inventory, then performs a guarded
`status.storedVersions` update. A failed, replaced or changed migration cannot
authorize finalization. The controller object remains for inspection and explicit
cleanup; its name/API and target schema are recorded in audit intent.

Without that API, the result is **STAGED / finalization pending**. Historical
`storedVersions` remain intact. Stock Docker Desktop currently exercises this
path; the isolated test minikube profile enables the official controller. KBorg
does not change cluster feature gates or claim that native rewrites alone prove
an HA migration complete.

Quiesce writers and schema controllers before approval. This is not an atomic
transaction or an HA coordination service. The official controller can touch
concurrent additions and continue retrying after KBorg cancels. Cancellation,
timeout or conflict can leave a switched schema, rewritten objects, a temporary
marker or a running controller. Inspect audit targets and live state, then make
a fresh complete review; KBorg never automatically retries or rolls back. A
fresh review can rewrite an interrupted native stage and clean its recognized
marker. Old API versions are retained even after successful finalization.

RBAC requires exact schema `get/update`, all-namespace custom-resource
`get/list/update`, and—when the official controller is available—migration
`create/get` plus exact schema `status` update. It does not need custom-instance
create/delete. Cluster test cleanup uses separate fixture authority.

Upstream procedure and controller semantics:
[Kubernetes CRD versioning](https://kubernetes.io/docs/tasks/extend-kubernetes/custom-resources/custom-resource-definition-versioning/)
and [Storage Version Migration](https://kubernetes.io/docs/tasks/manage-kubernetes-objects/storage-version-migration/).
