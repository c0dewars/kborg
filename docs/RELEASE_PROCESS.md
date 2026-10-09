# Platform build and release process

`Checks and platform builds` runs formatting, Clippy and tests, then builds and runs unit tests/offline CLI checks on six native GitHub-hosted targets:

| OS | Architecture | Runner | Rust target |
| --- | --- | --- | --- |
| Windows | x64 | windows-2025 | x86_64-pc-windows-msvc |
| Windows | ARM64 | windows-11-arm | aarch64-pc-windows-msvc |
| Linux | x64 | ubuntu-24.04 | x86_64-unknown-linux-gnu |
| Linux | ARM64 | ubuntu-24.04-arm | aarch64-unknown-linux-gnu |
| macOS | Intel | macos-15-intel | x86_64-apple-darwin |
| macOS | Apple Silicon | macos-15 | aarch64-apple-darwin |

Runner availability for private repositories is listed in [GitHub's official runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners). Private builds consume the account's Actions allowance. GitHub runner labels select maintained images, not immutable operating-system snapshots. The Rust version and action commit revisions are pinned; dependency resolution uses Cargo.lock.

Pushes and pull requests upload archives to the workflow run, retained for 30 days. No Kubernetes credentials or production endpoints are supplied to CI. Ordinary tests deliberately skip the isolated live Kubernetes test. Checkout does not persist credentials. Workflows use read-only permissions except the final tag-only draft-release job.

An authorized maintainer can push a tag matching Cargo.toml, such as `v0.1.0`, after reviewing all evidence. The tag workflow requires every native build to pass and creates a **draft** release containing the six archives, checksum files and build metadata. Publishing the draft is a separate maintainer decision. The initial branch push provides downloadable CI artifacts without requiring a tag or a public release. No signing/notarization claim is made.

`scripts/Package-Release.ps1` packages an already built target from a clean checkout. Windows uses ZIP; Linux/macOS use tar.gz to retain executable permissions. Included dependency metadata is an inventory, not a complete SPDX/CycloneDX SBOM or vulnerability audit. Archive checksums establish integrity only when the checksum's provenance is trusted.

Pin verification on 2026-10-09 used GitHub's official REST Git refs endpoint:

- actions/checkout v4.3.1: `34e114876b0b11c390a56381ad16ebd13914f8d5`
- actions/upload-artifact v4.6.2: `ea165f8d65b6e75b540449e92b4886f43607fa02`
- actions/download-artifact v4.3.0: `d3f86a106a0bac45b974a628896c90dbdf5c8093`

The v4 artifact action targets github.com; GitHub Enterprise Server workflow compatibility needs separate validation. This distribution choice is independent of the application's optional enterprise Copilot login support.
