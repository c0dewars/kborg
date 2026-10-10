# KBorg 0.3 delivery report

Updated 2026-10-10. Product: **KBorg**. Executable: `kborg`. Private repository: https://github.com/c0dewars/kborg. The original local checkout remains `C:\Users\Garcia\Desktop\projects\k8borg`; this avoids breaking existing local paths. The new logo is saved in the project and `outputs/KBorg-logo.png`. Existing settings and theme identifiers remain compatible.

## Delivered workflows

- Redacted YAML inspection, container selection, snapshot/previous logs and bounded live follow, with cancellation.
- Explicit context overview: up to 100 selected contexts, four independent simultaneous connections, 30-second global deadline. Demo results are explicitly marked synthetic.
- Controlled deployment image editing with readable before/after review, GitOps guidance, UID/resourceVersion checks, server dry-run, spec/metadata admission validation and one-use approval.
- Native bounded exec, output-only attach and loopback forwarding. Each requires write mode plus its own capability and typed target approval. Forwarding supports four simultaneous clients and a finite lifetime; it never binds a public address.
- Strict read-only/operator JSON configuration, four shell completions, pending-audit reporting, visible TUI actions, keyboard-only mode and TERM=dumb guidance.
- Atomic appearance updates, custom-theme cloning/replacement with a preserved backup, bounded discovery before catalog accumulation, CycloneDX SBOM and dependency notice coverage.

## Validation

- Final Windows source: 83 library tests plus the independent metrics contract; strict Clippy passed. WSL2: 84 library tests including the extra Unix permissions test, plus strict all-targets Clippy.
- 44 adversarial CLI cases passed on the final Windows release binary. Independent reviewers verified fixes for hidden exec arguments, unexpected admission metadata, demo provenance and completion omissions.
- Owned Docker Desktop fixture passed real logs/YAML/image-update/RBAC/conflict testing. Native operation fixtures passed on Windows and WSL2, including actual HTTP forwarding, four-client saturation, fifth-client queuing, abrupt disconnect, session cleanup, readonly rejection and same-name Pod recreation rejecting old approval. Temporary operation namespaces and credentials were removed.
- Native Linux performance sample: approximately 11.1 MiB peak RSS and 0.01 CPU seconds over each 60-second live/poll session. 100 aliases of ONE cluster returned 100 successful summaries in 0.131 seconds at 11.14 MiB RSS. This does not establish 100 independent-cluster capacity. The measured binary hash and sampling limitations are retained in PERFORMANCE_0.3.md.
- Final Windows short-read benchmark: 100 sequential local API reads had median 28.35 ms, p95 31.26 ms and maximum sampled working set 11.61 MiB. Warm-cache, one-millisecond sampling limits are recorded in PERFORMANCE_0.3.md.
- 198 locked packages: zero known RustSec vulnerabilities and zero warnings at the recorded advisory revision. This does not prove absence of undisclosed vulnerabilities. Packages include source-attributed dependency license/notice files and coverage metadata.

## Local testing

From the checkout, run `./scripts/Start-Local.ps1 -Demo` or `./scripts/Start-Local.ps1` for the retained least-privilege fixture. Fixture credentials expire after one hour; refresh using `Test-DockerDesktop.ps1` with the inspected Desktop API endpoint. WSL2 reuses Docker Desktop Kubernetes; a second Docker daemon is unnecessary.

Useful keys: `a` actions, `y` YAML, `l/L/f` logs/previous/follow, `o` overview, `e` image edit, `m` metrics, `t` themes. Writes require `--write`. See CLI_HELP.txt and OPERATIONS.md before opting into exec/attach/forwarding.

## Completion boundary

This iteration does **not** complete every item in the original roadmap and is **not production-certified**. Remaining implementation includes trusted editor/pager/tool handoff, interactive stdin/TTY, cloud authentication helpers, advanced terminal/metrics behavior and validated isolated Copilot diagnosis. Automatic AI remains disabled. Windows ACL/handle-race hardening, protected organizational audit, hostile WebSocket/fault/long-soak testing, broader real-terminal acceptance, signing/notarization and a production pilot remain gates. WebSocket helpers currently use upstream 16 MiB frame/64 MiB message caps; the small idle RSS measurement does not bound hostile-stream memory.

REMAINING_WORK.md is the authoritative detailed register. The updated development guide preserves the architecture and makes current status explicit. Publisher signing needs the owner's signing authority; actual Enterprise Cloud Copilot acceptance needs an entitled account; a production pilot needs the organization's deployment environment and sign-off.

## Final distribution status

Private draft: https://github.com/c0dewars/kborg/releases (KBorg 0.3.0 private draft). Final packaging-fix source: `e6de8e834edc665b04255384d34d725d782df87b`. GitHub Actions run `38007979257` was blocked before any steps by an account payment/spending-limit error. No billing settings or repository privacy were changed. The exact service error is retained in `evidence/platform-build-billing-block-0.3.json`.

The preceding source `d4d377bb5d62a728a184988f51c834fb61591212` passed native tests on all six targets and produced four Linux/macOS packages. Both Windows package checks correctly rejected line-ending-altered vendored licenses. The final `.gitattributes` fix preserves their exact upstream bytes; strict digest checks remain enabled. The production Rust runtime is unchanged between those two commits. See PLATFORM_BUILDS_0.3.md for actual verified artifacts and their individual source revisions. A final six-platform green run remains blocked; do not infer it from the earlier tests.

The owner declined paid GitHub Actions. The workflow is now manual-only; pushes and pull requests do not trigger hosted jobs. Five verified packages are attached to the private draft: Windows x64 built locally from e6de8e8, and four Linux/macOS native CI packages from d4d377. Their Rust runtime source is identical; the later commit changes only license-file checkout rules. Windows ARM64 packaging remains unverified. No billing settings were changed.
