# Controlled deployment image edits

KBorg supports one reviewed field-edit operation: changing the image of exactly one existing ordinary Deployment container. General manifest patches, privilege changes, init-container edits, Secret edits and RBAC edits are outside this operation.

Use `kborg set-image DEPLOYMENT CONTAINER IMAGE --write` with explicit context and namespace options as appropriate. The default remains read only. Review the before/after image reference, endpoint, context, namespace, name, UID and resourceVersion before typing the displayed namespace/name confirmation. The approval is single use, expires after 60 seconds and is bound to the exact image, container and target.

Images accept a conservative bounded ASCII registry/repository reference with tag or a complete SHA-256 digest. Digest pinning gives a stable image identity; tags can change remotely and KBorg does not attest image provenance. The container must already exist exactly once. Known Argo CD and Flux labels/annotations block direct edits and direct the user to the source repository. Unrecognized custom GitOps conventions require organization policy.

The gateway sends a strategic merge patch containing only UID/resourceVersion and the selected container name/image. It never copies environment values or the complete workload into the patch or review. The server performs dry-run admission first; the resulting complete Deployment spec must exactly equal the original spec with the requested image change. Additional admission mutations block review. The actual write repeats UID/resourceVersion preconditions and has no automatic retry. Admission can change between dry-run and execution; server-side policy remains authoritative.

Audit records store the stable operation name `set-image`, target identity and outcome, without image, container, patch or workload content. Action debug formatting also omits action values. Existing unknown-outcome handling and durable intent/outcome storage apply. Existing local audit filesystem/ACL hardening gates still apply.

## Verification

Unit tests exercise reference validation, GitOps detection, full-spec admission comparison, exact approval binding, read-only rejection, and exclusion of a sensitive environment canary from the review and outgoing patch. A mock Kubernetes HTTP server verifies strategic merge content type, UID/resourceVersion fields, the dry-run path and stale-resource 409 rejection. Live Kubernetes admission/RBAC acceptance remains a release gate until exercised against an owned test deployment.

Dry-run metadata protection also compares labels, annotations, ownerReferences, finalizers, name, namespace and UID against the original. Technical resourceVersion/managedFields differences are permitted. Unit coverage changes each protected field independently and verifies rejection. The same metadata protection applies to rollout restart.
