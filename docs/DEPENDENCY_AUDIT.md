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
