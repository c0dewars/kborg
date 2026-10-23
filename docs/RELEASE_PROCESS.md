# Platform build and release process

`Checks and platform builds` is manual-only (`workflow_dispatch`), following the owner's decision to avoid automatic hosted Actions charges. When deliberately dispatched, it runs formatting, Clippy and tests, then builds and runs unit tests/offline CLI checks on six native GitHub-hosted targets:

| OS | Architecture | Runner | Rust target |
| --- | --- | --- | --- |
| Windows | x64 | windows-2025 | x86_64-pc-windows-msvc |
| Windows | ARM64 | windows-11-arm | aarch64-pc-windows-msvc |
| Linux | x64 | ubuntu-24.04 | x86_64-unknown-linux-gnu |
| Linux | ARM64 | ubuntu-24.04-arm | aarch64-unknown-linux-gnu |
| macOS | Intel | macos-15-intel | x86_64-apple-darwin |
| macOS | Apple Silicon | macos-15 | aarch64-apple-darwin |

Runner availability for private repositories is listed in [GitHub's official runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners). Private builds consume the account's Actions allowance. GitHub runner labels select maintained images, not immutable operating-system snapshots. The Rust version and action commit revisions are pinned; dependency resolution uses Cargo.lock.

Pushes, pull requests and tags do not automatically run the workflow. A successful manual run uploads archives retained for 30 days. No Kubernetes credentials or production endpoints are supplied to CI. Ordinary tests deliberately skip live suites that require explicit fixtures. Checkout does not persist credentials. Workflows use read-only permissions except the final tag-only draft-release job.

An authorized maintainer can create a tag matching Cargo.toml after reviewing all evidence, then manually dispatch the workflow at that tag. Only that tag-scoped run, after every native build passes, creates a **draft** release containing the six archives, checksum files and build metadata. Publishing the draft is a separate maintainer decision. A manual branch run can provide downloadable CI artifacts without a tag or release. No signing/notarization claim is made.

`scripts/Package-Release.ps1` produces a native package from a clean checkout. Windows uses ZIP; Linux/macOS use tar.gz to retain executable permissions. The package includes dependency inventory, a CycloneDX 1.6 SBOM with dependency edges, collected license notices and coverage, and unsigned build/source records. An SBOM is not a vulnerability audit or an authenticity guarantee. Archive checksums establish integrity only when the checksum's provenance is trusted.

The supplied `-SourceCommit` must equal actual HEAD; dirty and untracked files are rejected. Recursively packaged documentation/assets must be tracked regular files, including files otherwise hidden by ignore rules. The Rust host and PowerShell architecture must match `-Target`. Packaging builds in a new empty target directory, runs version/offline/configuration checks and verifies the copied binary digest. It checks the clean source again after building and before creating the archive. Mac archives omit AppleDouble/xattr/ACL/file-flag metadata.

`BUILD.json` schema 2 records source tree, actual compiler details, build start/finish and packaging times, native target, binary digest and performed packaging checks. Unit tests are explicitly recorded as outside the packager; consult the exact source-specific acceptance evidence. Fresh native building is not a bit-for-bit reproducibility guarantee, and local records are not signed publisher provenance. The operator still trusts local Git, Cargo/Rust, PowerShell, tar, dependencies and the host environment.

Pin verification on 2026-10-09 used GitHub's official REST Git refs endpoint:

- actions/checkout v4.3.1: `34e114876b0b11c390a56381ad16ebd13914f8d5`
- actions/upload-artifact v4.6.2: `ea165f8d65b6e75b540449e92b4886f43607fa02`
- actions/download-artifact v4.3.0: `d3f86a106a0bac45b974a628896c90dbdf5c8093`

The v4 artifact action targets github.com; GitHub Enterprise Server workflow compatibility needs separate validation. This distribution choice is independent of the application's optional enterprise Copilot login support.
