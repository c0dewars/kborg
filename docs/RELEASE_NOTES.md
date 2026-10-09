# K8Borg 0.1

Security-focused Kubernetes terminal UI and CLI, read-only by default. This build provides browsing, bounded logs, redacted JSON, guarded scale/restart/delete and bundled/custom themes. See the included README for startup and the development guide for the full roadmap.

Archives are built and unit-tested natively on Windows, Linux and macOS for x64 and ARM64. Each archive includes source commit/build metadata, dependency inventory and upstream theme notices. SHA-256 files detect corruption; they do not replace a trusted signature. These binaries are unsigned and macOS packages are not notarized.

Local Docker Desktop acceptance and WSL evidence are recorded separately. Native unit tests and offline smoke checks do not certify terminal compatibility or production cluster safety. Production readiness remains subject to the documented security, operational and user-acceptance release gates. Optional AI access, when present, requires separate explicit consent and confers no Kubernetes mutation authority.
