# Kubernetes compatibility evidence

The client targets Kubernetes 1.34 and newer, using the `k8s-openapi` 1.34 schema baseline. The following live server versions passed on 2026-10-09. This is evidence for the tested versions and operations, not a guarantee that every future minor release or vendor distribution behaves identically.

| Server | Local environment | Result | Evidence |
|---|---|---|---|
| 1.34.11 | One temporary kind control plane on Docker Desktop | Passed | [1.34 evidence](evidence/compat-kubernetes-1.34.json) |
| 1.35.8 | One temporary kind control plane on Docker Desktop | Passed | [1.35 evidence](evidence/compat-kubernetes-1.35.json) |
| 1.36.1 | Existing Docker Desktop Kubernetes cluster; isolated test namespace | Passed | Earlier Docker Desktop and native WSL acceptance runs; see [WSL validation](WSL.md) |

Each temporary cluster used the official kind v0.33.0 Windows binary, checked against its published SHA256, and an official `kindest/node` image pinned by digest from the [kind release notes](https://github.com/kubernetes-sigs/kind/releases/tag/v0.33.0). Only one compatibility cluster existed at a time. The existing Docker Desktop daemon was reused; no second Docker Engine was installed.

## What passed

The actual Rust integration suite `tests/docker_desktop.rs::isolated_cluster_acceptance` ran against each server with short-lived reader, operator and denied service-account credentials. It verified:

- Pod reads and sanitized, explicitly enabled logs.
- Refusal to expose Secrets and redaction of the ConfigMap canary.
- Read-only mutation refusal, denied-account RBAC refusal and reader-account write refusal.
- Server dry-run and confirmed deployment scale and restart.
- Refusal of a stale resourceVersion after a competing mutation.
- Confirmed deletion of the disposable test service.

The native Windows CLI also completed `doctor` and produced parseable pod-list JSON against each server. The sample pod ran as a non-root user with a read-only root filesystem, dropped capabilities, disabled token automount and bounded CPU/memory. Kubernetes Pod Security restricted admission was enabled on the test namespace.

The final 1.34 run also passed all 23 unit/security tests and an actual `DiagnosticProbe` custom-resource smoke test. A restricted reader with only get/list permission for that CRD successfully discovered it, listed and retrieved `diagnosticprobes.k8borg.dev`, and exported minimized local Copilot context. The test verified that the exported diagnostics retained the Pending status while omitting the synthetic token, workload-spec and message canaries, object name and namespace. No AI request was made. The 1.35 evidence covers the common-resource suite; its earlier run did not include this added CRD smoke test.

These tests cover the current common-resource gateway and guarded mutations. They do not cover every CRD, admission webhook, authentication provider, enterprise distribution, aggregated API, large-cluster scenario or GitHub Copilot entitlement. Those remain separate release checks.

## Isolation and cleanup

Clusters used random `k8borg-compat-<run-id>` names and explicit admin kubeconfigs under ignored `.local/` directories with access restricted to the current Windows user. The user's default context remained `docker-desktop`. No context change was written to the user's default kubeconfig.

Compatibility nodes were capped at 4 GiB memory, no additional swap, and 4 CPUs. One observed 1.34 control plane used approximately 690 MiB after acceptance. This observation reinforces that 100 real control planes would exceed the available 15.6 GiB WSL allocation; it is not a production sizing estimate.

After each suite, the harness checked the owned namespace label, Docker daemon identity, exact owned node name, loopback HTTPS endpoint and embedded CA before deletion. Temporary clusters and admin/short-lived credentials were removed; sanitized evidence remains in `docs/evidence/`.

## Reproduce

From PowerShell 7 in the project root:

```powershell
.\scripts\Test-KubernetesCompatibility.ps1

# Or run one supported minor at a time:
.\scripts\Test-KubernetesCompatibility.ps1 -Versions '1.34'
.\scripts\Test-KubernetesCompatibility.ps1 -Versions '1.35'
```

The harness requires the existing Docker daemon to expose at least 10 GiB memory and 4 CPUs. It downloads the pinned, checksum-verified kind tool and official kubectl matching the selected server patch version into ignored `.local/tools/`. It provisions the restricted fixtures through `Test-KindFixture.ps1`, runs the unit and live suites, checks custom-resource diagnostics through `Test-KindDiagnosticCRD.ps1`, and verifies cleanup before proceeding to the next version. These tools are test prerequisites; K8Borg itself does not use Docker or kubectl to access Kubernetes.

## Final 1.36 regression

The final 1.34-schema candidate, source99e32c5, also passed native Windows acceptance against Docker Desktop1.36.1 using fresh owned namespace k8borg-tests-8e7ed664f6c6. The restricted RBAC, logs/redaction, scale/restart/delete, stale-version rejection and doctor/JSON checks passed. Sanitized evidence is in evidence/compat-kubernetes-1.36.json. This namespace is retained for local reader testing; short-lived credentials expire after one hour.
