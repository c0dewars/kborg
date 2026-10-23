# Locked dependency advisory audit

Executed on Windows on 2026-10-09 at approximately 21:43 UTC, using official RustSec `cargo-audit 0.22.2` installed from crates.io with its published lockfile. Installation tools and advisory database are outside the project checkout.

Command:

```text
cargo-audit audit --file Cargo.lock --json --db <scratch>/rustsec-advisory-db
```

- Lockfile SHA-256: `dd53a59109a80774e5900723db75181158e17a0fce8db0393b2728a04e2d203f`.
- Dependencies examined: 183 locked packages.
- Advisory database: official [RustSec advisory database](https://github.com/RustSec/advisory-db).
- Database revision: `7eebec69c352c7191b1f13eb95dd510eeca5d1de`.
- Database advisory count: 1296; latest database commit timestamp reported as `2026-10-09T10:12:02+02:00`.
- Exit status: 0.
- Known vulnerabilities reported: 0.
- Informational warnings reported: none.
- No advisory IDs were ignored. No platform or severity filters were configured.

Machine-readable tool output: [dependency-audit.json](evidence/dependency-audit.json).

This records the database's published advisories against the exact lockfile at one point in time. It does not prove absence of undisclosed vulnerabilities, cover vulnerabilities in OS/runtime tooling, audit source code or establish license compliance. Re-run on dependency changes and release candidates; record new lockfile/database revisions and review any findings rather than suppressing them automatically.

## 0.2 lockfile recheck

The updated lockfile was audited with cargo-audit0.22.2 against official RustSec revision7eebec69c352c7191b1f13eb95dd510eeca5d1de. Lock SHA-256:c0b8c72fbad6fc0ef913661a73aa631c0e1e9c64cf163fd7692634b4e26ae6e6. All183lockedpackages were examined; zero known vulnerabilities and zero warnings; no ignored IDs or platform/severity filters. Evidence: evidence/dependency-audit-0.2.json. futures-util is newly direct but already transitive; only std/async-await features are selected, without adding macro dependencies.

## 0.3 native WebSocket dependency review

Native pod exec, attach and forwarding enable the existing kube `ws` and Tokio `io-util` features. The upstream protocol implementation is preferred to custom WebSocket handshake/framing code. A pinned dependency remains necessary for native Kubernetes remote streams. No shell/subprocess adapter is introduced.

Lockfile SHA-256: `f01d03dde1b12d231c895f753a14436eab54453f14eeba9cf835cbf74ba290c2`. Locked packages: 198. Newly present package names (including rename): `block-buffer`, `cpufeatures`, `crypto-common`, `data-encoding`, `digest`, `generic-array`, `kborg`, `ppv-lite86`, `rand`, `rand_chacha`, `rand_core`, `sha1`, `tokio-macros`, `tokio-tungstenite`, `tungstenite`, `typenum`.

RustSec cargo-audit completed successfully with zero known vulnerabilities and no advisory ignores; machine-readable report: [dependency-audit-0.3.json](evidence/dependency-audit-0.3.json). This point-in-time check does not prove absence of undisclosed issues. WebSocket message allocation and long-duration/fault testing remain release acceptance gates.

## k9s navigation iteration (2026-10-22)

The current lockfile was checked with cargo-audit 0.22.2 against the official RustSec database. Initial results identified six advisories affecting three previously locked TLS packages. Updated `rustls` 0.23.45 → 0.23.46, `rustls-webpki` 0.103.15 → 0.103.16, and `webpki-root-certs` 1.0.9 → 1.0.10. No advisories were suppressed. The final scan exits 0 with zero known vulnerabilities and no informational warnings.

- Lockfile SHA-256: `685f04a72bdd78e2893a4f42fbfd52dda82ee6b868bafe0a3ce17a534f895d8c`.
- Locked packages: 198.
- Database revision: `640bf5a9f06e66d8721e8772766531fe77c8630d`; 1349 advisories.
- Evidence: [k9s-dependency-audit.json](evidence/k9s-dependency-audit.json).

`regex` and `serde-saphyr` are now direct dependencies, both already present transitively at the same versions. The former implements bounded k9s-compatible regex filtering; the latter parses local manifest/alias YAML using explicit depth, input, node, alias and document budgets. Deserialize-only features are selected for local YAML; no process execution or file inclusion is enabled. These direct references introduce no new transitive package names.

## Helm storage iteration (2026-10-22)

Helm's published release storage encoding requires gzip. Added `flate2 1.1.10`
with default features disabled and its pure Rust backend; no native zlib linking.
New transitive packages are `adler2 2.0.1`, `crc32fast 1.5.3`,
`miniz_oxide 0.9.1`, and `simd-adler32 0.3.10`. The decoder enforces a 1 MiB
encoded record limit, 8 MiB decompressed record limit, and bounded per-driver
aggregate decode budgets before safe typed projections. CRC and gzip size/error
regressions accompany the change; this does not substitute for independent fuzzing.

- Lockfile SHA-256: `6707a4b7efbb1f53c2d78595de335675a5b451758b8fa3b76ba6d84e18493a8e`.
- Locked packages examined: 203.
- cargo-audit 0.22.2; official RustSec database revision
  `640bf5a9f06e66d8721e8772766531fe77c8630d`, 1,349 advisories.
- Exit 0; zero known vulnerabilities or informational warnings; no ignored IDs.
- [Machine report](evidence/helm-dependency-audit.json).


## Trust, migration and UI iteration (2026-10-23)

The unchanged lockfile was checked again with cargo-audit 0.22.2 after refreshing the official RustSec database. All 203 locked packages passed with zero known vulnerabilities and zero warnings, without advisory ignores. Lockfile SHA-256: `6707a4b7efbb1f53c2d78595de335675a5b451758b8fa3b76ba6d84e18493a8e`. Database revision: `7ad117a308b144f2f7661fa555e4db5db9c5586e`. [Machine report](evidence/trust-cargo-audit.json). The review also covers bounded native-tool supervision and the optional Copilot process/broker boundary; see [security review](TRUST_SECURITY_REVIEW.md).
