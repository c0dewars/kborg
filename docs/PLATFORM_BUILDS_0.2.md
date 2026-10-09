# K8Borg 0.2 native platform evidence

## Final verified build

Run [38002963080](https://github.com/c0dewars/k8borg/actions/runs/38002963080) completed successfully for exact source `6d56d4f4374c0583657eb0d1a14273a7c3ca78cb`, version **0.2.0**. Verification occurred on 2026-10-09 at 23:20 UTC (2026-10-10 Lisbon local time). This source is authoritative for these release archives; later evidence-only documentation commits do not alter the compiled runtime.

| Native target | Result |
| --- | --- |
| Windows x64 | Native tests, release, package/metrics smoke and upload: PASS |
| Windows ARM64 | Native tests, release, package/metrics smoke and upload: PASS |
| Linux x64 | Native tests, release, package/metrics smoke and upload: PASS |
| Linux ARM64 | Native tests, release, package/metrics smoke and upload: PASS |
| macOS Intel | Native tests, release, package/metrics smoke and upload: PASS |
| macOS Apple Silicon | Native tests, release, package/metrics smoke and upload: PASS |

Common formatting and Clippy gates passed. CI logs independently confirm **49 library unit tests and one independent contract test passed, zero failed**. Three isolated live tests were deliberately ignored by ordinary CI and have separate fixture evidence. Every native target independently runs the test suite before release packaging.

The packaging gate exercises offline resource JSON, theme inventory and pod metrics JSON. It checks the deterministic sample label and **125000000 CPU nanocores (125m)** normalization. All six targets passed this gate. Every downloaded archive also contains the corrected `K8Borg 0.2` release notes. The tag-only automatic draft-release job was correctly skipped for this branch run.

## Independently verified artifacts

All six archives were downloaded to `dist/ci-38002963080/`. Every SHA-256 matched its supplied checksum file, and all six build records matched source `6d56d4f4374c0583657eb0d1a14273a7c3ca78cb` and version `0.2.0`. Local `VERIFIED.json` records the check timestamp and exact hashes.

| Archive target | SHA-256 |
| --- | --- |
| macOS ARM64 | `e55120af45a1822dd840a2f98b793dabbaff4054d01baa476674fed34e025754` |
| Windows ARM64 | `7681a180e5a8ad091401df6a02a806f9ae3e80fc2bc3471dde6a0baf6d5d8875` |
| Linux ARM64 | `1a074102616d135392f86f99196d124f39a2353f027036fc8ee55f11f58e41da` |
| macOS x64 | `0108487bd7ac2c7f7aefd3aa18d9f58ea000c393f2f08a1bb5a0f2d5e2167a0d` |
| Windows x64 | `ca13e9179668b3c58aaad796969049881c826c17a2d11b043a075ebcef7c1e68` |
| Linux x64 | `b34257204aec531a6c6978c4c818867007fa78890cc28c891eeb94ca5ac1c8c8` |

These exact artifacts are suitable for attachment to the authorized private prerelease draft. No agent release publication or Git push was performed as part of this validation.

## Defect found and resolved during independent validation

The initial run [38002039954](https://github.com/c0dewars/k8borg/actions/runs/38002039954), source `c57adcf0c6e5cc1cfdd364b92c98cb7c8aca3ec8`, failed macOS ARM64 packaging despite successful native tests/compilation. Independent log inspection identified a packaging test-contract mismatch: actual demo metrics JSON reports `source: "demo (deterministic sample)"`; the script incorrectly required `"demo"`. CPU normalization was already correct. The expectation was corrected and validated in the final six-target run. Initial/intermediate artifacts are not the approved release artifacts.

## Limits

Live Kubernetes API acceptance, secure Metrics Server interoperability, WSL performance and PTY checks are separate evidence reports. Native CI compilation and offline smoke do not substitute for those checks. Binaries remain unsigned and macOS packages are not notarized. Checksums establish integrity when their source is trusted, not publisher identity. No production certification, exhaustive terminal compatibility or 100 independent-cluster capacity follows from these results.
