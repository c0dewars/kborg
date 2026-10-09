# KBorg installation, recovery and configuration

## Install and update

Use an archive matching your OS and CPU from the private release. Obtain its expected digest through a trusted channel and run `scripts/Verify-Package.ps1 -Archive FILE -ExpectedSha256 HASH` before extraction. A checksum delivered beside an archive detects corruption but does not authenticate a publisher. Current packages are unsigned; production distribution requires your organization's signing and verification process.

Keep binaries in versioned directories and place only the selected directory on PATH. Run `kborg --version`, `kborg demo`, then a least-privilege `doctor` with an explicit kubeconfig/context/namespace before enabling operations. No cluster component or second Docker daemon is required. Docker Desktop's Kubernetes endpoint is accessible to the native Windows and WSL2 clients with verified CA material.

For rollback, stop KBorg and select the previous versioned binary. This does not undo Kubernetes operations. Keep a protected backup of user settings before upgrades. Config schema 1 rejects unknown versions; migration is deliberate and never executes a config file. Uninstall by removing the chosen binary directory; retain or explicitly remove your settings/audit spool according to retention policy. No automatic cleanup of kubeconfig or cluster objects occurs.

## Configuration

`kborg config init settings.json` creates a new strict JSON file. Pass it explicitly with `--config`; no project-local executable configuration is auto-loaded. `config validate` validates without connecting. CLI context/namespace/theme values override file defaults. Files can constrain capabilities but never enable writes or log/subresource access without their CLI flags. Profile `read-only` refuses `--write`; profile `operator` still requires `--write`, separate capability opt-ins, and target confirmation. These are local client controls, not a substitute for RBAC/admission or a policy against a user running another binary.

Supported fields: `schema_version` (1), `profile` (`read-only` or `operator`), optional `context`, `namespace`, `theme`, and booleans `no_splash`, `poll`, `allow_logs`, `allow_exec`, `allow_attach`, `allow_forward`. Unknown/duplicate fields fail. Default generated config prohibits sensitive capabilities. Config never stores credentials.

`kborg completions bash|zsh|fish|powershell` emits a static script; review and load it through your shell's normal configuration. It makes no cluster calls.

## Unknown operations and audit

Run `kborg audit pending --json` after a crash or uncertain response. Missing, incomplete and failed outcomes remain uncertain. Do not automatically replay a write. Use the recorded context, namespace, UID and operation to inspect current API state and correlate with API-server audit. A later state observation cannot prove exactly which request took effect. Local records are operator-mutable and supplementary; organizations requiring protected audit must supply a protected server-side sink and test outages before promoting writes.

## Support and production rollout

Kubernetes 1.34 is the minimum binding. Previously tested minors are 1.34, 1.35 and 1.36; future minors require compatibility runs. Certificate/token authentication is supported. Exec auth helpers remain blocked. Real provider identity rotation, proxy support, enterprise account policies and terminal combinations need deployment-specific acceptance.

Start with a dedicated read-only identity and narrow namespace, then a small staging pilot. Grant only reviewed operation verbs; enforce allowed images/fields and Pod Security on the API server. Test dry-run changes, stale resources, revocation, endpoint outage and audit failures. Promote only after the current register's required gates have evidence. Report security issues privately through the process in SECURITY.md; do not attach tokens, kubeconfig, Secrets or production logs.
