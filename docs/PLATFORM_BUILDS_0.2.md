# K8Borg 0.2 native platform evidence

## Initial attempt and release gate finding

Run [38002039954](https://github.com/c0dewars/k8borg/actions/runs/38002039954), source `c57adcf0c6e5cc1cfdd364b92c98cb7c8aca3ec8`, passed the common formatting, Clippy and test gates. macOS Apple Silicon passed native tests and release compilation but failed packaging at the new metrics smoke assertion.

Independent log inspection identified a test-contract mismatch: actual demo metrics JSON reports `source: "demo (deterministic sample)"`; the packaging script incorrectly required exactly `"demo"`. The CPU normalization was correct at `125000000` nanocores. This is a packaging assertion defect, not evidence of successful distributable artifacts. No artifacts from this attempt are approved for release.

A corrected packaging script and successful six-target native rerun are required. Final exact source, run, checksums and archive verification will be recorded after that gate passes.

## Scope of evidence

Native builds must cover Windows x64/ARM64, Linux x64/ARM64 and macOS Intel/Apple Silicon. Every target must run unit and independent contract tests, build the optimized executable, and pass offline resource/theme/metrics packaging smoke tests. Kubernetes API acceptance and WSL performance/UI evidence are separate reports, not substitutes for native builds.

Binaries remain unsigned and macOS packages are not notarized. Checksums establish archive integrity when trusted, not publisher identity. No production-readiness certification or exhaustive terminal compatibility claim follows from these checks.
