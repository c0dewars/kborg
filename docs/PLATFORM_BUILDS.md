# Native platform build evidence

Status recorded on 2026-10-09. Native artifacts are produced by the [private repository workflow](https://github.com/c0dewars/k8borg/actions).

## Initial six-platform build

Run [37995707394](https://github.com/c0dewars/k8borg/actions/runs/37995707394), source `4e20eb2b1a6c56826665b1f8131f899ab6ff5c36`, used the earlier Kubernetes 1.36 schema binding. It must not be presented as the final Kubernetes 1.34+ baseline build.

| Native target | Observed status |
| --- | --- |
| macOS Apple Silicon | Tests, release build, packaging, offline smoke and upload passed |
| macOS Intel | Tests, release build, packaging, offline smoke and upload passed |
| Linux x64 | Tests, release build, packaging, offline smoke and upload passed |
| Linux ARM64 | Tests, release build, packaging, offline smoke and upload passed |
| Windows ARM64 | Tests, release build, packaging, offline smoke and upload passed |
| Windows x64 | Tests, release build, packaging, offline smoke and upload passed |

All six native jobs and common formatting, Clippy and test gates passed. All six archives were downloaded to `dist/ci-37995707394/` and their SHA-256 checksum files matched on 2026-10-09 at 22:02 UTC. Every build metadata source revision matched `4e20eb2b1a6c56826665b1f8131f899ab6ff5c36`. The local `VERIFIED.json` records exact archive hashes. The tag-only draft-release job was correctly skipped for this branch push.

## Final baseline build

Run [37997317697](https://github.com/c0dewars/k8borg/actions/runs/37997317697), exact source `99e32c5a2ab62691fdeeb7c4928978d294623e50`, compiled against the Kubernetes 1.34 schema and completed successfully on 2026-10-09. This is the authoritative runtime build for the Kubernetes 1.34+ baseline. Later evidence-only documentation commits do not change the compiled runtime.

| Native target | Final baseline result |
| --- | --- |
| Windows x64 | Native tests, release build, packaging, offline smoke, upload: PASS |
| Windows ARM64 | Native tests, release build, packaging, offline smoke, upload: PASS |
| Linux x64 | Native tests, release build, packaging, offline smoke, upload: PASS |
| Linux ARM64 | Native tests, release build, packaging, offline smoke, upload: PASS |
| macOS Intel | Native tests, release build, packaging, offline smoke, upload: PASS |
| macOS Apple Silicon | Native tests, release build, packaging, offline smoke, upload: PASS |

Common formatting and Clippy gates passed. The common CI log records **23 unit tests passed, zero failed**; the live Kubernetes test remains deliberately ignored by ordinary tests and has separate isolated fixture evidence. All six native build jobs independently ran tests before packaging. The branch run correctly skipped the tag-only draft-release job.

All six final archives were downloaded to `dist/ci-37997317697/`. On 2026-10-09 at 22:16 UTC, every archive SHA-256 matched its checksum file, and all six build records matched the exact source revision above. `VERIFIED.json` in that local directory records the full hashes. These verified artifacts can be attached to a private draft release without recompiling or mixing revisions.

Live API compatibility reports for Kubernetes 1.34+, where available, remain separate evidence from binary compilation and offline CLI tests. Native CI does not connect to a cluster.

## Distribution limits

Artifacts are unsigned, and macOS packages are not notarized. CI evidence covers native compilation, unit tests and offline CLI smoke; it does not cover all terminal emulators, production Kubernetes operations or real Copilot authentication. Checksums detect corruption when their source is trusted; they are not code-signing attestations. Archives include exact source revision, target, binary checksum, dependency inventory and upstream theme notices. See `RELEASE_PROCESS.md` for the workflow and draft release policy.


