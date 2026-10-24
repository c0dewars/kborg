# KBorg 0.4.0 — 2026-10-24

**Every cluster at a glance. Every change reviewed.** Install with `brew install c0dewars/tap/kborg`.

## Highlights

- **Multi-cluster dashboard** for up to nine clusters: Pod health, restart hotspots, OOM evidence, Node readiness with CPU/memory, degraded Deployments/StatefulSets, Pod and non-Pod warning events. Full-screen focus (`1`–`9`), `kborg --contexts a,b`, and `kborg dashboard` to reopen the last one.
- **Logs**: warning → exact-Pod Events drilldown (`w`, Enter), single-Pod log panes (`L`), a cross-cluster workload tail merged in timestamp order (`t`), and `:logs-live` that follows rollouts.
- **k9s parity**: `:secrets` with per-key reveal (`--allow-secret-reveal`), reviewed Ctrl-K force kill (`--allow-force-kill`), `:subjects`/`:users`/`:groups`, Ctrl-E `$EDITOR` handoff (`--allow-external-editor`), multi-file `KUBECONFIG` merged like kubectl.
- **Copilot** advice for GitHub.com, GHE.com tenants and GitHub Enterprise Server 3.22+, each with an exact egress allowlist.
- **Speed**: first data in 0.06 s; at 5,000 live objects with churn, about 40 % of k9s's memory and CPU. See [performance comparison](PERFORMANCE_COMPARISON.md).
- **UX**: contextual key hints, WCAG AA contrast on every surface for all fixed themes, Ghostty/kitty/WezTerm truecolor detection, an animated startup splash, and clearer connection errors.

## Upgrade notes

- New opt-in flags: `--allow-secret-reveal`, `--allow-force-kill`, `--allow-external-editor`, `--contexts`, `--dashboard`. Nothing new is enabled by default.
- Live watches now hold up to 10,000 objects (64 MiB); larger scopes fall back to a bounded 30 s list with a banner.
- Unsigned builds. Homebrew formula installs are not quarantined by macOS; archive downloads still need the usual checksum verification.

# Helm completion and terminal UI — 2026-10-22

## Helm completion and terminal refinement (2026-10-22)

Adds one-time OCI tag resolution, explicitly required trusted OpenPGP chart provenance, reviewed stored release tests, compatible CRD schema upgrades and separately confirmed schema deletion. Refines app surfaces, table headers, operation forms and review hierarchy. See [workflow guide](HELM_EXTENSIONS.md) and [source-specific acceptance](HELM_COMPLETION_ACCEPTANCE.md) for supported bounds, tests and screenshots.

The preceding extension iteration added digest-pinned OCI chart acquisition with verified HTTPS and explicit registry credentials, reviewed pre/post lifecycle hooks, and a separate create-only CRD bootstrap followed by schema-bound custom-resource lifecycle. The full-width Helm UI adds `:helm-crds` and `c`, source fields accept OCI digests, and capability states remain visible. See [extension guide](HELM_EXTENSIONS.md) for supported scope and limits.

# Local development update — 2026-10-22

This earlier iteration added reviewed Helm install, upgrade, rollback and uninstall
from the CLI and Helm view, plus fixed-membership workload/selector logs across
up to eight Pods and 32 ordinary-container streams. Lifecycle requires all three
flags `--write --allow-helm --allow-helm-write`, exact trusted Unix Helm 4.3.1 and
local deterministic packaged charts. A complete 15-minute review and fresh
plan/history/UID/resourceVersion checks precede execution. Hooks, CRDs,
cluster/system/cross-namespace targets, recognized GitOps resources and foreign
adoption are rejected. SQL storage, repository/OCI fetching and Windows lifecycle
remain unsupported. Source membership and shared log limits remain fixed across
range changes/reconnects; init and ephemeral sources are excluded from merges.
See [workflow guide](HELM_LIFECYCLE.md),
[current acceptance](HELM_LIFECYCLE_ACCEPTANCE.md),
[exact command compatibility](K9S_COMPATIBILITY.md) and
[remaining work](REMAINING_WORK.md). Full k9s parity and external production gates
remain open. No new release is published by this local session.

## Earlier Helm, Pod-log and metrics iteration — 2026-10-22

This earlier iteration added full-width k9s command navigation, native Helm release
and revision browsing, redacted selected values, merged ordinary-container Pod
logs, and CPU/memory tables with numeric sorting and session history. Docker
Desktop now has an explicitly installed Metrics Server with verified TLS on both
hops. Helm/log workflows also run against an isolated minikube cluster. See
[historical acceptance](HELM_LOGS_METRICS_ACCEPTANCE.md). Helm lifecycle was still
open in that source and is superseded by the latest iteration above.
The dated reports below preserve their original artifact scope.

# Local development update — 2026-10-10

The current working iteration adds interactive stdin/TTY sessions, longer rolling
logs, reviewed ExecCredential helpers, continuous context watches, explicit
metrics allocation comparisons, controlled resource edits, a native pager, opt-in
view extensions and a second UI refinement. See [current acceptance](PRIORITIES_ACCEPTANCE.md)
and [remaining gates](REMAINING_WORK.md). Automated Copilot stays disabled after
preflight; current packages are unsigned. The notes and artifacts below describe
the original0.3 release source and are preserved as historical evidence.

# KBorg 0.3.0 — renamed client and operational workflows

K8Borg is now **KBorg**, executable `kborg`, private repository `c0dewars/kborg`. Existing pre-rename settings directories and `K8BORG_CONFIG_DIR` / `K8BORG_NO_SPLASH` remain compatible. Built-in theme identifiers remain stable. The updated logo is `assets/kborg-logo.png`.

New workflows: redacted YAML inspection; container selection and finite live log following; explicitly selected multi-context pod summaries; reviewed deployment container-image edits with GitOps guidance; native bounded exec/output attach and loopback forwarding behind separate capabilities; strict JSON read-only/operator configuration; shell completions; pending-audit reporting; recoverable custom-theme replacement and copying current colors. TUI adds visible actions, YAML, follow, image-edit and overview workflows.

Security changes: discovery now limits bytes/requests before decoding the complete catalog; image edits use UID/resourceVersion, server dry-run, spec/metadata admission checks and value-bound approval; exec preview rejects invisible/control arguments; native operations have finite lifetimes and protected audit intent; settings use staged same-directory replacement with restrictive Unix creation. Overview has four independent connections and a 30-second global budget.

Release packages include CycloneDX SBOM, dependency inventory, checksums and unsigned source/build records. These are not signed provenance or publisher authentication.

This remains a local-testing prerelease. Exec is noninteractive (no stdin/TTY), attach is output-only, and pod subresource APIs have a residual name-recreation race. AI requests, generic executable extensions and cloud authentication helpers remain disabled. Windows ACL/handle-race hardening, validated Copilot isolation/account acceptance, publisher signing/notarization, broader real-terminal acceptance and a production pilot remain release gates. `docs/REMAINING_WORK.md` is the authoritative register; historical 0.1/0.2 reports describe their original artifacts.

Available packages: Windows x64 (locally rebuilt and smoke-tested from `e6de8e834edc665b04255384d34d725d782df87b`), Linux x64/ARM64 and macOS Intel/Apple Silicon (native CI packages from `d4d377bb5d62a728a184988f51c834fb61591212`). The only difference between these source commits is exact-byte checkout rules for attributed license files; Rust production code is identical. All five packages passed checksum, source/version, SBOM, dependency-notice and branding verification. See attached `VERIFIED-MIXED-SOURCES.json` and `PLATFORM_BUILDS_0.3.md`.

Windows ARM64 native tests passed on the earlier commit, but no verified 0.3 package is supplied. GitHub blocked the final hosted run because of account billing/spending limits. At the owner's request, no paid Actions work was pursued and future CI triggers are manual-only. Local native builds remain supported. This draft has five platform packages, not a successful final six-platform release matrix.
