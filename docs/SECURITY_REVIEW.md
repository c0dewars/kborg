# Independent implementation security review

Review date: 2026-10-09. Reviewer: a separate security-review agent, independent of the implementation agent. This is a source review and targeted regression exercise, not an external certification or penetration test. Reviewed gateway, policy, safety, CLI output handling and existing security/release documents.

## Disposition

The implemented local-testing boundary is conservative: verified HTTPS, direct Kubernetes access, no helper execution, no arbitrary shell evaluation, read-only default, no Secret resource access, bounded bodies, selected mutation families and server-authoritative authorization. The project must retain its production release gates. Passing these tests does not establish production readiness across organizations, operating systems or admission configurations.

## Findings corrected

| Finding | Impact | Correction and evidence |
| --- | --- | --- |
| Pagination bounded items but not pages/time | A faulty or hostile API server could repeatedly return empty pages with continuation tokens, keeping work alive indefinitely. | At most 10 pages and a 15-second total budget; repeated continuation tokens fail. In-memory API regression demonstrates stopping after two repeated pages. Partial page-limit results explicitly report truncation. |
| Discovery had no gateway deadline | Serial group discovery could monopolize the worker on slow or extensive API surfaces; UI callers did not necessarily apply the CLI's outer timeout. | Gateway discovery receives its own 15-second timeout. Further byte/group concurrency limits remain a release gate. |
| Public prepared-operation review fields were mutable | A caller could change action, target or summary after dry-run, creating a discrepancy between reviewed operation and the private patch. | Store immutable private copies of the reviewed target/action/summary; execution rejects any discrepancy before approval consumption, audit or network writes. Regression changes a scale into delete and proves the object remains present. |
| Duplicate kubeconfig named entries were accepted | Ambiguous selection could bind visible names to different credentials/servers depending on parser selection rules. | Reject duplicates independently for contexts, clusters and users before selection; reject empty, excessively long or control-bearing names. Regression rejects two identically named contexts. |
| URL query strings could contain credentials | The server identity is printed to the terminal; an access token embedded in a cluster URL query could leak through this path. | Require HTTPS with neither userinfo nor a query. Regression rejects a synthetic access-token query. |
| Path inspection ignored inspection failures | Access-denied or other metadata errors were treated similarly to missing files, weakening fail-closed path verification. | Only NotFound is tolerated; other inspection errors fail. Reads inspect regular-file type before opening and after opening. This narrows, but does not close, replacement races. |
| New Unix configuration directories used ambient directory modes | A permissive umask could expose configuration/audit directory listings to other users. | New directory trees created through the writer request 0700; files already request 0600. Existing directories and Windows inherited ACLs still require ownership/permissions review. |
| Relative OS configuration variables were accepted | Configuration persistence could be redirected by the working directory when an environment path was relative. | Resolved default configuration directories must be absolute and pass path checks, matching the explicit K8BORG_CONFIG_DIR rule. |

Validation immediately after core fixes: 19 library tests passed and all-target Clippy with warnings denied passed on Windows. The later URL regression passed with all six gateway tests. Final whole-project validation is recorded separately because other agents may subsequently change UI, themes and integration modules.

## Required production evidence

1. **Local filesystem safety:** race-resistant handle-based path operations and owner/permission enforcement; prove Windows reparse handling, protected audit/config ACLs, macOS/Linux ownership, and behavior in hostile shared directories. Current metadata checks have a time-of-check/time-of-use window. A same-user compromised process can alter credentials and application files.
2. **Authoritative audit and least privilege:** organizational Kubernetes audit, protected retention and deployment-specific RBAC/admission restrictions. Local JSONL audit is an operational aid, not tamper-evident compliance evidence. Test the final roles rather than deploying the fixture operator role unchanged.
3. **Admission behavior:** dry-run and actual admission can differ over time or because of nondeterministic webhooks. UID/resourceVersion preconditions reduce stale writes but do not certify that the real mutation has exactly the dry-run effect. Exercise webhook changes, denied writes, response loss, cancellation and expiry/revocation. Never automatically repeat an uncertain mutation.
4. **Confidentiality:** known-field redaction is not a complete sensitive-data classifier. Labels, status fields, arbitrary CRD strings and opted-in logs may contain secrets. Secret resource exclusion does not make all Kubernetes objects safe to share. AI must receive only a separately reviewed, minimized projection with explicit transfer authorization.
5. **Denial of service:** discovery's deadline limits elapsed work, not peak memory across every accumulated API group; test large schemas, high group counts, slow streams, parse depth and malformed kubeconfigs. Body/file limits are necessary but not sufficient memory budgets.
6. **Platforms and release supply chain:** native Windows/Linux/macOS terminal and file-permission testing, dependencies/advisories/licenses, artifact checksums/provenance/signing, vulnerability contact, CI permissions and rollback. A cross-compilation build is not runtime acceptance.
7. **AI isolation:** never provide the GitHub/Copilot process a production kubeconfig, ambient cloud credentials, write tools or unrestricted filesystem as an implicit consequence of login. An authentication handoff and a sanitized offline context bundle are narrower than integrated automated diagnosis; do not conflate them. GitHub Enterprise authentication requires the official supported host flow, not a token pasted into K8Borg.

## Preserved safeguards

Read-only operations cannot prepare writes. Approval expires and binds session/resource/namespace/name/UID/resourceVersion/action; consumption is single-use. Writes require fresh immutable preparation and server preconditions, a durable audit intent, finite execution timeout and no automatic retry. Secret reads, insecure TLS, authentication exec/provider helpers and impersonation are blocked. API errors suppress server-controlled messages and credential initialization suppresses underlying credential details. Terminal text is sanitized before display. These safeguards should remain regression-tested as features are added.

## Copilot follow-up review

The new integration exports a bounded allowlist of diagnostics for one selected object. Names, namespaces, cluster identity, workload environment values and arbitrary status messages are excluded. It performs no model request, no automatic tool call and no mutation based on AI output. Strings such as condition reasons remain untrusted and can contain confidential text or prompt injection; the bundle expressly requires inspection before sharing.

The optional login command validates github.com or an Enterprise Cloud *.ghe.com hostname and requires an interactive terminal plus an absolute executable path selected by the user. It passes fixed arguments using the native process API. The executable path is a user trust decision; it is not a publisher signature check and does not establish arbitrary GitHub Enterprise Server support.

Corrections from this follow-up: remove common Kubernetes, cloud, alternative-model credentials and dynamic-loader/shell injection variables from the child environment; launch in a dedicated configuration directory rather than the application checkout; describe the credential boundary precisely. The external CLI still runs with the user's filesystem and OS permissions, so this is not process isolation. Future automated AI diagnosis remains gated on stronger isolation and reviewed data-transfer controls.

Validation: both Copilot host/context minimization and environment-filter tests passed; all-target Clippy with warnings denied passed after this change. An actual OAuth login was not exercised because it requires the user's interactive identity and trusted installed Copilot CLI.
