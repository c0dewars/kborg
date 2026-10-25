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

Pushes, pull requests and tags do not automatically run the workflow. A successful manual run uploads archives retained for 30 days. No Kubernetes credentials or production endpoints are supplied to CI. Ordinary tests deliberately skip live suites that require explicit fixtures. Checkout does not persist credentials. Workflows use read-only permissions except the jobs that sign, attest or create a draft release.

An authorized maintainer can create a tag matching Cargo.toml after reviewing all evidence, then manually dispatch the workflow at that tag. Only that tag-scoped run, after every native build passes, creates a **draft** release containing the six archives, checksum files and build metadata. Publishing the draft is a separate maintainer decision. A manual branch run can provide downloadable CI artifacts without a tag or release. That path is unsigned; signed releases come from the `Signed release` workflow below.

`scripts/Package-Release.ps1` produces a native package from a clean checkout. Windows uses ZIP; Linux/macOS use tar.gz to retain executable permissions. The package includes dependency inventory, a CycloneDX 1.6 SBOM with dependency edges, collected license notices and coverage, and unsigned build/source records. An SBOM is not a vulnerability audit or an authenticity guarantee. Archive checksums establish integrity only when the checksum's provenance is trusted.

The supplied `-SourceCommit` must equal actual HEAD; dirty and untracked files are rejected. Recursively packaged documentation/assets must be tracked regular files, including files otherwise hidden by ignore rules. The Rust host and PowerShell architecture must match `-Target`. Packaging builds in a new empty target directory, runs version/offline/configuration checks and verifies the copied binary digest. It checks the clean source again after building and before creating the archive. Mac archives omit AppleDouble/xattr/ACL/file-flag metadata.

`BUILD.json` schema 2 records source tree, actual compiler details, build start/finish and packaging times, native target, binary digest and performed packaging checks. Unit tests are explicitly recorded as outside the packager; consult the exact source-specific acceptance evidence. Fresh native building is not a bit-for-bit reproducibility guarantee, and local records are not signed publisher provenance. The operator still trusts local Git, Cargo/Rust, PowerShell, tar, dependencies and the host environment.

Pin verification on 2026-10-09 used GitHub's official REST Git refs endpoint:

- actions/checkout v4.3.1: `34e114876b0b11c390a56381ad16ebd13914f8d5`
- actions/upload-artifact v4.6.2: `ea165f8d65b6e75b540449e92b4886f43607fa02`
- actions/download-artifact v4.3.0: `d3f86a106a0bac45b974a628896c90dbdf5c8093`

The v4 artifact action targets github.com; GitHub Enterprise Server workflow compatibility needs separate validation. This distribution choice is independent of the application's optional enterprise Copilot login support.

## Signed release workflow

`.github/workflows/release.yml` (`Signed release`) is the release path. It is manual only. Dispatch it from the tag, with the same tag as input:

```sh
git tag -a v0.6.0 -m "KBorg 0.6.0" && git push origin v0.6.0
gh workflow run release.yml --ref v0.6.0 -f tag=v0.6.0
```

It refuses to run unless the dispatch ref is `refs/tags/<tag>` and the tag equals `v` plus the Cargo.toml version, so the signing identity and the provenance name the tag. Then:

1. **build** (six native runners, `contents: read`): `cargo test`, then `Package-Release.ps1` as above. Linux runners also build and install-test the `.deb` (`scripts/package-linux.py`) and the `.rpm` (`packaging/rpm/kborg.spec`, `rpmbuild` from Ubuntu's `rpm` package). There is no musl build; the packager supports glibc targets only.
2. **sign** (`contents: read`, `id-token: write`, `attestations: write`; the only job with an OIDC token): checks every `.sha256`, writes `SHA256SUMS` over all assets, signs every asset (including `SHA256SUMS`) with keyless cosign into `<file>.sigstore.json`, verifies each bundle with the same command users run, and records GitHub build provenance (SLSA) for every file listed in `SHA256SUMS`. A `~` in Debian/RPM pre-release file names becomes `.` before signing, because GitHub rewrites it in asset names.
3. **draft-release** (`contents: write`): creates a **draft** release (marked pre-release for `-rc` versions) with the archives, per-target CycloneDX SBOM (`*.sbom.cdx.json`), build and provenance records, `.deb`, `.rpm`, `SHA256SUMS` and all bundles. Publishing stays a maintainer decision.

Actions are pinned by full commit SHA (resolved 2026-10-25 through the GitHub API): sigstore/cosign-installer v4.1.2 `6f9f17788090df1f26f669e9d70d6ae9567deba6` (installs cosign v3.1.3), actions/attest-build-provenance v4.2.2 `4d101475d8b20a2381f78447822ac1eab6504dd8`, ossf/scorecard-action v2.4.4 `2d1146689b8cda280b9bc96326124645441f03bc`, github/codeql-action/upload-sarif v4.38.3 `24c54180a607b1449ed407dd24f251e4e9147c8d`. GitHub artifact attestations need a public repository (or GitHub Enterprise Cloud for a private one).

### macOS signing and notarization (owner action)

The macOS jobs codesign (`--options runtime --timestamp`, Developer ID Application) and notarize (`notarytool submit --wait`) only when all five repository secrets exist; otherwise they log a notice and ship unsigned macOS archives. Add them under Settings > Secrets and variables > Actions:

| Secret | Value |
|---|---|
| `APPLE_DEVELOPER_ID_CERT_P12` | Base64 of the exported Developer ID Application certificate and key (`base64 -i cert.p12`) |
| `APPLE_CERT_PASSWORD` | Password of that .p12 |
| `APPLE_ID` | Apple ID email used for notarization |
| `APPLE_TEAM_ID` | 10-character team ID |
| `APPLE_APP_PASSWORD` | App-specific password for that Apple ID |

The certificate goes into a temporary keychain that is deleted at the end of the job. `Package-Release.ps1 -CodesignIdentity` signs the fresh binary before its smoke checks, so the archived binary is the signed one, and `BUILD.json` records `signed: true`. The notarized submission is that same binary. A bare command-line binary cannot be stapled (stapling applies to .app, .pkg and .dmg), so Gatekeeper checks the ticket online on first run. These credentials have not been used yet; the first run with them is the acceptance test.

### Verify a release asset

Each asset `FILE` has `FILE.sigstore.json` next to it.

```sh
# GitHub build provenance (gh CLI)
gh attestation verify FILE --repo c0dewars/kborg \
  --signer-workflow c0dewars/kborg/.github/workflows/release.yml

# Sigstore signature (cosign v3)
cosign verify-blob FILE --bundle FILE.sigstore.json \
  --certificate-identity-regexp '^https://github\.com/c0dewars/kborg/\.github/workflows/release\.yml@refs/tags/v' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com

# Every checksum at once, after verifying SHA256SUMS itself as above
sha256sum --check --ignore-missing SHA256SUMS
```

For signed macOS archives, `codesign --verify --strict --verbose=2 kborg` checks the Developer ID signature.

## Supply chain gates

- **cargo-deny**: `deny.toml` fails on RustSec advisories (vulnerable, unsound, yanked, and unmaintained crates in our tree), licenses not compatible with Apache-2.0, wildcard version requirements, and any source other than crates.io. Duplicate crate versions are warnings. Run locally with `cargo install --locked --version 0.20.2 cargo-deny`, then `cargo deny --locked check`. On 2026-10-25 it passed (`advisories ok, bans ok, licenses ok, sources ok`) with duplicate warnings for base64, getrandom, granit-parser, serde-saphyr, syn and windows-sys. The `supply-chain` job in `Checks and platform builds` runs the same check, so a dispatched run fails on a new advisory. To accept an advisory, add it to `[advisories] ignore` with a reason.
- **OpenSSF Scorecard**: `.github/workflows/scorecard.yml` is manual only (`gh workflow run scorecard.yml`), matching the owner's no-automatic-Actions policy, although Scorecard suggests a weekly schedule. It keeps the SARIF as a run artifact and, while the repository is public, publishes to the Scorecard API and code scanning. No score exists yet. Expect lost points on checks that look for CI on every PR and for code review on a single-maintainer repository.
- **cargo vet**: deferred. A useful `cargo vet` needs audits for our ~200 crates or imported audit sets plus exemptions for the rest; `cargo vet init` alone exempts every current crate and only flags new ones. Revisit once the dependency set is stable after 0.6.
