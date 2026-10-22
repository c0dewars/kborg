# Local development update — 2026-10-22

The latest iteration adds reviewed Helm install, upgrade, rollback and uninstall
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
