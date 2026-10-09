# Security policy

0.1.0 is a local-testing release. Do not report it as audited/production-ready. Public vulnerability reporting infrastructure is not configured yet; discuss security findings privately with the project owner and avoid placing credentials or real cluster objects in reports.

Use dedicated least-privilege identities. Never use CI production credentials. Do not assume read-only access protects confidentiality. Never publish `.local/`, kubeconfigs, logs, audit metadata or crash dumps containing private data. TLS verification cannot be disabled through application flags.

Before production: independent sensitive-code review, dependency advisories/licenses, cloud-auth trust, protected audit, OS file-permission/race hardening, denial-of-service/fuzz tests, revocation/uncertain-result drills, supported platform matrix, release authenticity and deployment-specific RBAC/admission evidence.

See docs/DEVELOPMENT_GUIDE.md and docs/IMPLEMENTATION_STATUS.md for requirements and known limitations. Client safeguards do not constrain a hostile same-user process. The API server and organizational controls are authoritative.
