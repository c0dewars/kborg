# K8Borg 0.1 validated first build

Private repository: https://github.com/c0dewars/k8borg.

Runtime source: `99e32c5a2ab62691fdeeb7c4928978d294623e50`. Native CI: https://github.com/c0dewars/k8borg/actions/runs/37997317697. All six native targets passed tests, optimized builds, CLI smoke checks and packaging; downloaded archive checksums and source records were verified.

## Delivered and tested

- Two-second ASCII logo animation with a rotating hub; any key skips. `--no-splash`, `K8BORG_NO_SPLASH` and `startup off/on` control it. CLI/JSON commands never animate.
- First-use theme selection defaults to Catppuccin Mocha; Enter chooses, Esc keeps the default. Disabling animation does not suppress onboarding. Eighteen bundled themes and validated custom themes remain available.
- Kubernetes 1.34 baseline, with real-server acceptance on 1.34.11, 1.35.8 and Docker Desktop 1.36.1. Restricted readers/operators, authorization denial, redaction, logs, guarded scale/restart/delete and stale-object refusal were exercised. CRD discovery/list/get and minimized context export passed on 1.34.
- Native Windows and WSL 2 Linux execution. Docker Desktop's existing engine was reused; no second daemon installed. Ordinary Ubuntu Docker CLI integration remains optional because K8Borg talks directly to the Kubernetes HTTPS API.
- 23 unit/security tests, strict formatting/Clippy, 44 bounded adversarial CLI cases, neutral first-look UI/feature review, and an advisory scan of 183 locked dependencies with zero known vulnerabilities or warnings at the recorded database revision.
- Independent real load test: 1,001 ConfigMaps → exactly 1,000 with truncation warning; payload canary redacted. Linux UI reached 21.74 MiB sampled RSS, 0.10 CPU seconds over roughly 15 seconds, and cached-filter acknowledgement median 5.50 ms / maximum 6.85 ms.
- 100 context aliases tested against one cluster, including eight bounded concurrent CLI processes. This is not 100 independent clusters. A sampled control plane used approximately 690 MiB; 100 such nodes exceed the available 15.6 GiB WSL allocation.

## Native archives

Windows x64 and ARM64 use ZIP files; Linux x64/ARM64 and macOS Intel/Apple Silicon use tar.gz files. Archives and checksums are under `dist/ci-37997317697/` in the project and copied into the chat outputs `builds/` directory. Each archive records its exact target, source commit and binary checksum. Private draft release assets provide the same verified builds.

## Start locally

```powershell
cd C:\Users\Garcia\Desktop\projects\k8borg
.\scripts\Start-Local.ps1 -Demo
.\scripts\Start-Local.ps1
```

The retained reader fixture is `k8borg-tests-8e7ed664f6c6`. Credentials expire approximately one hour after provisioning; rerun `Test-DockerDesktop.ps1 -ExpectedServer <inspected endpoint> -KeepFixtures` to refresh them. `Start-Local.ps1 -Write` selects only the fixture operator. Do not use fixture permissions as a production role.

## What remains before production release

This is a validated prerelease, not the complete production-ready product. Metrics, real watches, generic editing, simultaneous multi-context views and advanced workflows remain in the roadmap. Exec/attach, port forwarding, arbitrary plugins and shell handoff remain gated. Modern visual refinement follows feature completion and must retain the measured resource budgets.

Copilot is the only eligible AI provider. Implemented commands perform local minimized context export and explicit trusted CLI login for GitHub.com / Enterprise Cloud *.ghe.com. Automatic AI diagnosis, actual identity/enterprise login acceptance and isolated model/tool execution are not completed; no model request or autonomous AI mutation was enabled.

Other production gates include filesystem/ACL hardening, authoritative server audit and admission policy, discovery-memory/fault/soak testing, broad terminal/accessibility acceptance, complete license/SBOM/provenance checks, signing and macOS notarization. The independent agent reviews and finite test corpus are not an external certification. The binaries are unsigned.

Detailed evidence: `docs/IMPLEMENTATION_STATUS.md`, `SECURITY_REVIEW.md`, `FIRST_LOOK_REVIEW.md`, `USER_ACCEPTANCE.md`, `KUBERNETES_COMPATIBILITY.md`, `PLATFORM_BUILDS.md`, `RESOURCE_LOAD_TEST.md`, `PERFORMANCE.md`, `WSL.md`, `ADVERSARIAL.md` and `DEPENDENCY_AUDIT.md`.
