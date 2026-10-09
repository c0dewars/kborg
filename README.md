# K8Borg

Kubernetes operations, under your control.

Rust terminal UI and CLI, read-only by default. Local-testing release **0.1.0**. This is a working first iteration, not a production-certified implementation of every requirement in the development guide.

## Run

Requirements: Rust 1.99 (pinned), the platform linker (Visual Studio C++ tools on Windows), and a terminal. Docker Desktop with Kubernetes and kubectl are needed only for local integration tests. Cargo downloads the pinned dependencies on first build. Git is useful for development. No Node/Python runtime, AI account, Helm, or kubectl runtime backend is required.

```powershell
cd C:\Users\Garcia\Desktop\projects\k8borg
cargo build --release
.\target\release\k8borg.exe demo
```

For a real cluster, use an explicitly selected trusted kubeconfig and a least-privilege identity:

```powershell
.\target\release\k8borg.exe --kubeconfig <file> --context <context> -n <namespace>
.\target\release\k8borg.exe --kubeconfig <file> list pods --json
.\target\release\k8borg.exe --help
```

Existing Docker Desktop admin kubeconfig works, but the fixture reader is preferable for local testing. No kubeconfig changes are made. Certificate/token authentication is supported; exec-based cloud helpers and legacy auth providers are rejected until their trust model is implemented. All real clusters are labeled PRODUCTION.

## Docker Desktop acceptance and local fixtures

Run checks, inspect the local endpoint, then explicitly approve that endpoint for the test harness:

```powershell
.\scripts\Check.ps1
kubectl --context docker-desktop config view --minify -o 'jsonpath={.clusters[0].cluster.server}'
.\scripts\Test-DockerDesktop.ps1 -ExpectedServer 'https://127.0.0.1:64170' -KeepFixtures
.\scripts\Start-Local.ps1
.\scripts\Start-Local.ps1 -Theme catppuccin-mocha
.\scripts\Start-Local.ps1 -Write
```

The example port is this machine's verified endpoint at authoring; Docker Desktop can choose a different port. Supply the endpoint you inspected. The harness rejects nonlocal hosts, non-HTTPS, missing CA data, or a mismatch. It creates only a unique owned namespace with digest-pinned synthetic workloads and restricted service accounts. It never resets Docker Desktop or switches the current context. Test credentials expire after one hour; rerun the harness to get fresh credentials. Old retained runs should be cleaned up using their run IDs.

Live tests cover resource reads, real RBAC-denied access, logs, ConfigMap redaction, Secret rejection, scale/restart/delete and stale-version conflict. They use the API through Rust, not kubectl for application operations. The operator fixture has workload patch rights for local tests; it is not a recommended production role, because server-side policy must independently constrain patch fields.

```powershell
.\scripts\Cleanup-Local.ps1 -RunId <12-character-run-id>
```

Omit `-KeepFixtures` for automatic cleanup. Failed runs clean up even when retention was requested. `test-results/` has sanitized reports; `.local/` holds credential files with restricted Windows directory ACLs and is ignored by Git. Deleting the namespace invalidates its service-account identities. Never commit these credentials or use real production data in fixtures.

## Interface

Tab changes focus between resources and objects. Up/down or j/k navigate, Enter opens redacted JSON, `/` filters cached names, `r` refreshes, `c` selects context, `n` changes namespace (`*` means all namespaces), `R` discovers resources/CRDs, `l` shows bounded pod logs and `L` previous logs, `t` opens themes, `?` shows help, Esc closes and `q` exits. Click resource/object rows or scroll using the mouse. ASCII rendering is conservative: non-ASCII display characters become `?`; raw JSON retains safely escaped Unicode.

Updates use bounded polling every five seconds, not watches. Failed requests mark data STALE. Reads are limited to 1,000 objects/8 MiB retained data, with 200-object pages and 8 MiB response bodies. Narrow namespace scope when TRUNCATED appears. Logs require `--allow-logs`, are capped at 200 lines/256 KiB, and can still contain sensitive data.

Mutations require `--write`, a server dry-run and exact `namespace/name` confirmation within 60 seconds. `s` scales deployments (0..20), `x` restarts, Delete deletes an allowed individual object. UID/resourceVersion preconditions reject stale targets; no automatic mutation retries. System namespaces and cluster-scoped mutations are blocked. Generic edits, force deletion and finalizer manipulation are absent. Confirm the complete displayed target before typing; on small terminals enlarge the window to inspect long targets. Noninteractive writes are rejected.

## Themes

18 bundled choices: K8Borg Dark/Light/High Contrast; all four Catppuccin flavors; Dracula; Nord; Gruvbox Dark/Light; Tokyo Night Night/Storm/Moon/Day; Solarized Dark/Light; host Terminal mode. Safety-critical low-contrast upstream colors are adapted to meet the project's fixed-palette targets. Theme families are credited in `THIRD_PARTY_NOTICES.md`; this is an unofficial adaptation.

```powershell
.\target\release\k8borg.exe theme list
.\target\release\k8borg.exe theme use catppuccin-mocha
.\target\release\k8borg.exe theme new my-theme --base catppuccin-mocha
.\target\release\k8borg.exe theme validate <file.json>
.\target\release\k8borg.exe theme preview <file.json>
.\target\release\k8borg.exe theme install <file.json>
.\target\release\k8borg.exe theme export catppuccin-mocha --output .\custom.json
```

See `docs/themes.md`. Themes never execute code or access the network. Install refuses overwrites; edit your existing custom JSON and validate it to update locally. Built-in IDs cannot be replaced. The TUI previews with arrows, saves on Enter and restores on Esc. Selection under `--theme` is session-only. `NO_COLOR` and `--color=never` disable colors. JSON output contains no theme styling.

## Security and scope

TLS validation is required. Secrets are excluded from built-in and discovered kinds. Common secret-bearing fields (data, annotations, literal environment values, tokens/credentials and managed fields) are masked for display/export. Redaction is not a complete classifier of business data. Terminal controls are removed from text and diagnostic content. No telemetry or object persistence is enabled.

Writes use a local durable intent/outcome audit spool; this is supplementary and mutable by the operator, not centralized protected audit. Windows config directories outside the test harness inherit their existing user ACLs; verify them before using writes on sensitive clusters. Same-user path replacement races are not eliminated by symlink checks. Authentication revocation, malicious local software and cluster admission remain outside the client trust boundary.

AI, arbitrary plugins, shell handoff, exec/attach, forwarding, general YAML editing, metrics dashboard, simultaneous multi-context view, true server watches, signed packaging, centralized audit and non-Windows platform verification are not implemented. They remain gated in the development plan. This iteration is suitable for isolated local testing; production use needs the remaining evidence and review in `docs/IMPLEMENTATION_STATUS.md`.

## Startup and first use

Interactive startup shows a two-second ASCII K8Borg animation. Any key skips it. Use `--no-splash`, set `K8BORG_NO_SPLASH`, or run `k8borg startup off` to disable it persistently (`startup on` restores it). Animation never runs in CLI/JSON commands. The first interactive launch opens the theme picker with Catppuccin Mocha selected; Enter chooses a theme and Esc keeps Mocha. Explicit `--theme` overrides onboarding for that session.

## Optional GitHub Copilot preparation

K8Borg supports only GitHub Copilot as the planned AI provider. Current commands prepare a small local diagnostic JSON file and hand login to an explicitly selected trusted official Copilot CLI. K8Borg sends no model request and never gives AI write authority. Copilot CLI is optional and is not installed or downloaded by K8Borg.

```powershell
k8borg --kubeconfig <file> -n <namespace> copilot context pods <name> --output <new-file.json>
k8borg copilot login --cli <absolute-trusted-copilot-executable> --host github.com
k8borg copilot login --cli <absolute-trusted-copilot-executable> --host acme.ghe.com
```

Context export omits object names, cluster identity, workload specs, logs, labels and arbitrary status messages. Bounded type/reason strings can still contain confidential text or prompt injection: inspect before manually sharing. Copilot manages login credentials and may have its own telemetry/storage. Do not assume GitHub Enterprise Server login provides a Copilot service; supported login hosts are GitHub.com and GitHub Enterprise Cloud data-residency `*.ghe.com`. See [official Copilot authentication](https://docs.github.com/en/enterprise-cloud%40latest/copilot/how-tos/copilot-cli/set-up-copilot-cli/authenticate-copilot-cli).

Automatic AI diagnosis remains a production gate requiring isolation, tested tool denial, explicit per-request payload review and policy evidence. Other AI providers are excluded. UI visual refinement follows feature completion and must preserve measured CPU/memory budgets.

## Platform, security and performance evidence

See `docs/SECURITY_REVIEW.md`, `docs/USER_ACCEPTANCE.md`, `docs/RELEASE_PROCESS.md`, and the WSL/performance reports. Build automation targets Windows, Linux and macOS on x64/ARM64. Platform artifacts count as built only when the corresponding native job succeeds; packaging is unsigned. Multiple kubeconfig aliases to one cluster test context handling, not independent cluster capacity.
