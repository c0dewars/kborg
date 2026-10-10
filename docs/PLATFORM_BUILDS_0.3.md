# KBorg 0.3 platform and packaging validation

## Local packaging review

Independent release-validation review completed on 2026-10-10. Local Windows x64 packaging smoke passed for KBorg 0.3.0. This smoke archive uses a pre-release-development source label and is **not** the approved release artifact. Local test package source label: `76320faf432fcca2c28c082b9d751057a5003927`; archive SHA-256: `ad868453452b2d370dbec5904917e469e8811864824ef7dc247099b8e67dabe9`. The compiled source included uncommitted development changes, so this label does not establish release provenance. Final native CI source and six verified distributions will be recorded below after the authorized release commit.

The package smoke covers offline pod JSON, normalized pod metrics (125m / 125000000 nanocores), the Mocha theme inventory, all four static shell completion outputs and strict schema-1 configuration creation/validation. The default configuration is read-only and exec/attach/forward opt-ins remain disabled. This smoke does not invoke remote operations or a real cluster.

## Dependency notices and SBOM

The tested package includes notice coverage for all **198 packages** in Cargo metadata, including platform-specific and development dependencies. Included files are bounded regular LICENSE/LICENCE/COPYING/NOTICE/AUTHORS/COPYRIGHT files, with content hashes in `dependency-licenses/COVERAGE.json`. Ten package sources lack standalone notices; their upstream texts are vendored in `assets/dependency-licenses/` and copied with `PROVENANCE.json`. r-efi's published AUTHORS file includes its MIT copyright and permission text and is copied directly.

Eight vendored entries are tied to each published crate's `.cargo_vcs_info.json` commit. The two GNU winapi import-library crates omit VCS metadata. Their original Cargo.toml files matched the historical upstream manifests at commit `9497609ef44cc9bcd16cd2411c0ee6ccaf5483aa` exactly; notices come from that revision. This establishes a documented notice source, but does not independently prove the full published crate source revision. Preserve that limitation in attribution reviews.

The local package had 384 notice/provenance files, all 198 package coverage entries present, 197 library SBOM components and one application component. Every CycloneDX dependency reference resolved to a unique component reference. Application references use package URLs instead of leaking absolute builder paths. Legacy `MIT/Apache-2.0` metadata is normalized to the SPDX `MIT OR Apache-2.0` expression. These structural checks are not a complete external CycloneDX schema validation or legal certification.

## Final native build

**Final six-platform acceptance: BLOCKED.** Run [38007979257](https://github.com/c0dewars/kborg/actions/runs/38007979257), source `e6de8e834edc665b04255384d34d725d782df87b`, did not execute any steps. GitHub's official check annotation states: “The job was not started because recent account payments have failed or your spending limit needs to be increased.” No final-source CI pass is claimed.

Run [38007025823](https://github.com/c0dewars/kborg/actions/runs/38007025823), source `d4d377bb5d62a728a184988f51c834fb61591212`, passed common checks and all four native Linux/macOS builds. Unix ran 84 library tests; Windows ran 83; the independent contract test passed. Both Windows executables compiled, but their packaging failed because Git's Windows checkout converted vendored license newlines and invalidated strict provenance hashes. `.gitattributes` now preserves exact notice bytes; all 17 Git index/source license blobs matched their upstream hashes. This fix changes checkout metadata, not production runtime code. Final CI remains blocked by billing.

The five available distributions were independently verified on 2026-10-10 at 00:14 UTC. Four Unix packages come from the successful predecessor native jobs; Windows x64 was rebuilt and packaged locally from the final committed source. Windows ARM64 has no verified final package.

| Verified filename | Build source / method | SHA-256 |
| --- | --- | --- |
| `kborg-0.3.0-aarch64-apple-darwin.tar.gz` | `d4d377bb` / native CI | `ce331456466b2fba4070818f10382d34019635aab77c68c6a1e52e2d8bba4c6b` |
| `kborg-0.3.0-x86_64-apple-darwin.tar.gz` | `d4d377bb` / native CI | `8902ec08c1ab664142aaf5e7f801cf8dbeae8b5e8a63f53455ecd0a07f4d3019` |
| `kborg-0.3.0-aarch64-unknown-linux-gnu.tar.gz` | `d4d377bb` / native CI | `56832dfe328a1e7f5fd25e4d0335411e39b8938cbb0a6af28bc50b10f48e8132` |
| `kborg-0.3.0-x86_64-unknown-linux-gnu.tar.gz` | `d4d377bb` / native CI | `f8e91ddcdd8904089554f64caca0f65ecb025ceb34a276702a109180695092c7` |
| `kborg-0.3.0-x86_64-pc-windows-msvc.zip` | `e6de8e83` / native local rebuild | `ede8720f07cb42d0554aca64009899d6a3668fd6c5f78ee447bc7de785e7ffbc` |

Every listed archive matched its SHA-256 file, exact source/version in its internal BUILD record and external unsigned provenance. Internal and external CycloneDX SBOMs matched, all 198 component references resolved, all 198 dependency notice entries and every included notice/provenance file hash verified, and packaged README logo/branding assets and 0.3 release notes were present. The local Windows rebuild passed resource/metrics/theme/configuration/completion packaging smoke; it is not a hosted-runner pass. Preserve per-artifact source and method when attaching to the private draft.

Verified files and their checksum/build/SBOM/provenance companions are copied to the original chat's `outputs/builds-0.3/`; `VERIFIED-MIXED-SOURCES.json` records the five checks. Project evidence is in `dist/ci-38007025823/VERIFIED.json` and `dist/local-e6de8e8/VERIFIED.json`. The older precommit local smoke archive above is excluded from these deliverables.

## Distribution limits

Binaries are unsigned and macOS packages are not notarized. Unsigned provenance is an explicit source/build record, not a verified publisher attestation. Archive checksums establish integrity only when obtained from a trusted source. Native CI does not certify Kubernetes production use, terminal compatibility or 100 independent-cluster capacity; the separate security, live acceptance and operational reports define those limits.
