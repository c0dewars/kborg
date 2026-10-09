# K8Borg 0.1 implementation and release evidence

Date: 2026-10-09. Target: Windows / Docker Desktop Kubernetes 1.36.1.

This first iteration is a working local-testing release. The development guide remains the full product specification; this document records the implemented subset. Completion of this iteration does not certify production readiness or feature parity with K9s.

## Delivered

| Area | Implemented behavior |
| --- | --- |
| Rust application | Release executable; modular gateway, policy, CLI, TUI, themes and safety code; unsafe code forbidden in project code |
| Terminal interface | Resource/object navigation, cached filtering, mouse selection, context and namespace switching, details, stale/truncated indicators, five-second bounded refresh |
| Kubernetes | Direct HTTPS API client, 17 common resource kinds, discovery including CRDs, paginated reads, explicit kubeconfig selection, certificate/token authentication |
| CLI integration | Resource tables and redacted JSON for terminal pipelines, meaningful exit status, bounded logs with explicit opt-in; no shell interpolation |
| Guarded writes | Explicit write mode; deployment scale/restart and selected individual deletions; server dry-run; admission-effect checks; target-bound expiring one-use approval; UID/resourceVersion preconditions; no retry |
| Security defaults | Read-only, TLS verification, Secret exclusion, blocked authentication helpers/impersonation/proxies, terminal control sanitization, bounded API bodies and display data, redaction of known sensitive fields |
| Audit | Durable local intent before mutation and outcome afterward; finite spool quota; unknown-outcome reporting on timeouts |
| Appearance | 18 bundled themes, live picker and preference, validated custom JSON, install/export/remove, duplicate-key and unknown-field rejection, contrast checks |
| Docker Desktop | Explicit endpoint verification; owned isolated namespace; restricted service accounts; short-lived credential files; digest-pinned synthetic fixtures; repeatable tests and cleanup |
| Development | Locked dependencies, pinned Rust toolchain, fmt/clippy/test/release checks, CI baseline, dependency/license inventory, theme source/license snapshots |

## Evidence

- `scripts/Check.ps1`: formatting, all-target Clippy with warnings denied, 23 unit/security tests, documentation tests and optimized release build.
- `scripts/Test-DockerDesktop.ps1`: live acceptance passed on the installed Docker Desktop cluster. Latest successful Windows run: `8e7ed664f6c6`, namespace `k8borg-tests-8e7ed664f6c6`; sanitized evidence in `docs/evidence/compat-kubernetes-1.36.json`. WSL native Linux acceptance also passed against this isolated fixture.
- Live checks exercise reader access and logs, real authorization denial, ConfigMap redaction, blocked Secret access, server-side dry-run authorization, scale/restart/delete, and stale resourceVersion rejection.
- Unit checks exercise read-only policy, write scope/scale limits, replay/expiry/target mismatch, malicious authentication configuration, invalid TLS, oversized response rejection before JSON decoding, terminal controls, redaction, and theme validation.
- Theme CLI lifecycle checks cover creation, validation, preference, export, installation, removal and reset. Terminal smoke checks exercise startup, theme dialog, complete wrapped write-review display at 80 columns (canceled), and normal shutdown with terminal restoration. This is not a terminal compatibility matrix.

The live test is intentionally ignored by ordinary `cargo test`; the harness supplies isolated credentials and runs it explicitly. Do not interpret an ignored live test as a Kubernetes acceptance pass.

## Architectural and security review

The implementation was reviewed against its trust boundaries during development. Changes made during that review include bounded response bodies before deserialization, disabling automatic write retries, admission-effect checks, strict API-group selection for writes, single-use approval binding, rejection of truncated confirmation displays, and owned-namespace/cluster-CA verification for cleanup. Tests cover several of these failure paths.

An independent agent source review and user/developer acceptance pass are recorded in docs/SECURITY_REVIEW.md and docs/USER_ACCEPTANCE.md. These are automated team reviews, not an external penetration test or independent organizational audit. A client policy cannot constrain an operator's use of other clients. Production roles and admission policies must independently restrict allowed write fields and scopes.

## Production release gates and remaining work

1. **Independent review and adversarial testing:** threat-model review, Kubernetes admission behavior matrix, fuzzing of configuration/redaction/rendering, response and discovery stress tests, cancellation/timeout/unknown-outcome tests, and crash/panic terminal recovery evidence.
2. **Authentication:** implement an explicit trust model for cloud exec helpers before enabling them; validate credential rotation/expiry and revocation behavior. Current helper rejection prevents use with many cloud kubeconfigs.
3. **Local storage:** harden Windows configuration/audit ACLs outside the harness and close same-user path replacement races; implement stronger atomic preference replacement. Local audit remains operator-mutable and must complement protected Kubernetes audit.
4. **Scale and responsiveness:** replace polling with bounded watch/reconnect handling; bound discovery cost across many API groups; prove budgets and UI latency on large clusters and slow networks.
5. **Terminal support:** verify Windows Terminal, PowerShell, cmd, Linux/macOS terminals, SSH and multiplexers; add reliable 256/16-color mapping, Unicode width handling and accessibility evidence. Current conservative ASCII rendering and true-color palettes have limitations.
6. **Supply chain and distribution:** validate all transitive dependency licenses/advisories, generate release SBOM and provenance, sign/package artifacts, document update/rollback and supported Kubernetes versions. CI now builds/tests/packages six native Windows/Linux/macOS x64/ARM64 targets; verify each job and retain artifact evidence.
7. **Product scope:** metrics, general editing, true watches, simultaneous multi-context views and advanced workflows remain unimplemented. Exec/attach, port forwarding, arbitrary plugins and shell handoff remain disabled until their security designs and tests are complete.
8. **Optional AI:** GitHub Copilot alone is eligible. Local minimized diagnostic export and explicit trusted CLI login are implemented; automatic model requests remain absent. GitHub.com and Enterprise Cloud *.ghe.com host selection are supported; an actual identity/subscription login requires user acceptance. Automatic diagnosis must have an isolated opt-in boundary, per-request transfer review and no autonomous write authority.

Redaction masks known fields; arbitrary application text, labels and logs can still contain confidential information. Secrets are blocked by resource name, not by a universal classifier of all CRD data. Use synthetic fixtures for this iteration.

## Start locally

No additional runtime is required beyond the installed Rust/linker and Docker Desktop tools. PowerShell 7 is used by the provided scripts. Run `scripts/Start-Local.ps1 -Demo` for offline browsing, or `scripts/Start-Local.ps1` for the latest retained reader fixture. Add `-Write` only to use the fixture operator. Credentials expire after one hour; rerun the acceptance harness with the inspected local endpoint to refresh them.

See `README.md` for exact commands and `docs/DEVELOPMENT_GUIDE.md` for the full roadmap.




## Follow-up release work

Two-second skippable ASCII startup and persisted disabling are implemented. First-run selection defaults to Catppuccin Mocha; disabling animation before first launch preserves onboarding. CLI output never animates. The private source repository is https://github.com/c0dewars/k8borg.

Native WSL/Linux, Windows unit/lint checks, Docker Desktop acceptance, advisory audit and measured performance are recorded in their dedicated reports. One hundred aliases to a single API server are explicitly distinct from 100 independent Kubernetes clusters. Visual refinement follows feature completion and must retain these measured resource budgets.



Final runtime source99e32c5 passed all six native build/test/package jobs; downloaded archives were checksum-verified. Independent large-resource testing passed the 1,001-object positive truncation/redaction case with about22MiB loaded Linux UI RSS. The final release is a private draft prerelease, not production certification. See RELEASE_REPORT.md and PLATFORM_BUILDS.md for exact artifact identity and remaining scope.
