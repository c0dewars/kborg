# Security policy

The current 0.3 development build is a local-testing release. Do not report it as audited/production-ready. Public vulnerability reporting infrastructure is not configured yet; discuss security findings privately with the project owner and avoid placing credentials or real cluster objects in reports.

Use dedicated least-privilege identities. Never use CI production credentials. Do not assume read-only access protects confidentiality. Never publish `.local/`, kubeconfigs, logs, audit metadata or crash dumps containing private data. TLS verification cannot be disabled through application flags.

Before production: independent sensitive-code review, dependency advisories/licenses, cloud-auth trust, protected audit, OS file-permission/race hardening, denial-of-service/fuzz tests, revocation/uncertain-result drills, supported platform matrix, release authenticity and deployment-specific RBAC/admission evidence.

See docs/REMAINING_WORK.md and the linked current acceptance report for requirements and known limitations. Client safeguards do not constrain a hostile same-user process. The API server and organizational controls are authoritative.

Helm storage access is separately opt-in with `--allow-helm`. General Secret
browsing remains unavailable, but Helm metadata/history and selected values are
read from Kubernetes Secret or ConfigMap storage. Kubernetes RBAC cannot restrict
Secret list permission by label selector; the API identity must be scoped
accordingly. Raw release payloads, notes, chart files and rendered manifest bodies
are not exported. Known-key and private-key redaction cannot classify every
confidential custom value. Logs and explicitly requested values remain sensitive.
