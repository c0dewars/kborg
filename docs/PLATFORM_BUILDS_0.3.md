# KBorg 0.3 platform and packaging validation

## Local packaging review

Independent release-validation review completed on 2026-10-10. Local Windows x64 packaging smoke passed for KBorg 0.3.0. This smoke archive uses a pre-release-development source label and is **not** the approved release artifact. Local test package source label: `76320faf432fcca2c28c082b9d751057a5003927`; archive SHA-256: `ad868453452b2d370dbec5904917e469e8811864824ef7dc247099b8e67dabe9`. The compiled source included uncommitted development changes, so this label does not establish release provenance. Final native CI source and six verified distributions will be recorded below after the authorized release commit.

The package smoke covers offline pod JSON, normalized pod metrics (125m / 125000000 nanocores), the Mocha theme inventory, all four static shell completion outputs and strict schema-1 configuration creation/validation. The default configuration is read-only and exec/attach/forward opt-ins remain disabled. This smoke does not invoke remote operations or a real cluster.

## Dependency notices and SBOM

The tested package includes notice coverage for all **198 packages** in Cargo metadata, including platform-specific and development dependencies. Included files are bounded regular LICENSE/LICENCE/COPYING/NOTICE/AUTHORS/COPYRIGHT files, with content hashes in `dependency-licenses/COVERAGE.json`. Ten package sources lack standalone notices; their upstream texts are vendored in `assets/dependency-licenses/` and copied with `PROVENANCE.json`. r-efi's published AUTHORS file includes its MIT copyright and permission text and is copied directly.

Eight vendored entries are tied to each published crate's `.cargo_vcs_info.json` commit. The two GNU winapi import-library crates omit VCS metadata. Their original Cargo.toml files matched the historical upstream manifests at commit `9497609ef44cc9bcd16cd2411c0ee6ccaf5483aa` exactly; notices come from that revision. This establishes a documented notice source, but does not independently prove the full published crate source revision. Preserve that limitation in attribution reviews.

The local package had 384 notice/provenance files, all 198 package coverage entries present, 197 library SBOM components and one application component. Every CycloneDX dependency reference resolved to a unique component reference. Application references use package URLs instead of leaking absolute builder paths. Legacy `MIT/Apache-2.0` metadata is normalized to the SPDX `MIT OR Apache-2.0` expression. These structural checks are not a complete external CycloneDX schema validation or legal certification.

## Final native build

Pending the authoritative 0.3 source commit and native GitHub Actions run. Required targets: Windows x64/ARM64, Linux x64/ARM64 and macOS Intel/Apple Silicon. Each must pass tests, optimized compilation and packaging smoke. Downloaded archives must match supplied SHA-256 files, version, exact source revision, SBOM, unsigned provenance and included dependency notice coverage before draft release attachment.

## Distribution limits

Binaries are unsigned and macOS packages are not notarized. Unsigned provenance is an explicit source/build record, not a verified publisher attestation. Archive checksums establish integrity only when obtained from a trusted source. Native CI does not certify Kubernetes production use, terminal compatibility or 100 independent-cluster capacity; the separate security, live acceptance and operational reports define those limits.

