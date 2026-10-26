# KBorg 0.5.0 — unreleased

**Every k9s workflow, reviewed.** This release closes the remaining k9s gaps and works out of the box: everything is on, and every change is reviewed.

## Highlights

- **Works out of the box, like k9s.** No flags needed: shell, logs, edits, apply, port forwards, file copy, node shell, debug containers, Helm, Secrets, scans, extensions and Copilot are all on. Every change still shows its review; `y`/Enter applies and `n`/Esc cancels. `--read-only` turns changes off, and an operator `--config` can switch any capability off or require typed confirmations (`strict_confirmations`). The old `--write`/`--allow-*` flags are accepted and change nothing.
- **Kubeconfig works as in kubectl.** Exec credential helpers (`aws eks get-token`, `gke-gcloud-auth-plugin`, `kubelogin`, ...), auth providers, impersonation, `proxy-url` and `insecure-skip-tls-verify` are honored; the reviewed helper mode is still available with `--allow-auth-helper --auth-helper-policy`. helm, Trivy/Grype and a saved `copilot-policy.json` are found automatically.
- **`kborg doctor`** checks terminal, tools, kubeconfig authentication (is the exec helper on PATH?) and cluster access, and gives a next step for each problem.
- **The header names the screen you're on**: Nodes, Deployments, Namespaces, Contexts, Logs, `Pod / details`, Review change. `READ ONLY` appears only in read-only mode.
- **Server-side apply** (`--write --allow-apply`): `kborg apply FILE`, `:apply FILE` or `a` in `:dir`. Up to 16 namespaced objects, every one dry-run first; the review shows the fields a create sets and every changed path of an update. Updates are locked to the reviewed resourceVersion, nothing is written if any object changed since review, and field-manager conflicts are never forced.
- **Node shell** (`--write --allow-node-shell`): `s` on a Node or `kborg node-shell NODE`. Review the complete privileged Pod, get an `nsenter` root shell, and the Pod is deleted afterwards (and self-expires if KBorg is killed).
- **Pod file and directory copy** (`--write --allow-file-copy`): `:cp-from`, `:cp-to`, `-r` for directories. Never overwrites on either side; sizes and file counts verified; directory archives extracted strictly.
- **`:lint`** (also `:popeye`, `kborg lint --json`): read-only misconfiguration report — requests/limits/probes, unpinned images, privileged Pods, degraded or single-replica Deployments, crash loops, Services without Pods.
- **Extensions in the TUI**: `:ext NAME` hands the selected object to a registered read-only tool after the whole-input review. `kborg extensions import-k9s plugins.yaml` shows which k9s plugins fit and prints the commands.
- **k9s plugins with variables**: `kborg extensions register-k9s plugins.yaml NAME --tool ABS` registers a k9s plugin with its `shortCut`, `description`, `scopes` and `dangerous` flag. `$NAMESPACE`, `$NAME`, `$CONTEXT`, `$COL-IMAGE` and the other k9s variables expand inside one argument, never through a shell; cluster values with control characters, or starting with `-` where an option could begin, are refused. Shortcuts bind only on free keys and show in `?` help and `:actions`; the review lists the exact argv, and dangerous runs need `RUN DANGEROUS NAME`. `import-k9s` now accepts stern, dive, log-full and helm-values unchanged.
- **k9s-format `hotkeys.yaml`**: `Shift-0`…`9`, `Alt-<key>`, `F1`…`F12` and free `Ctrl-<letter>` run KBorg commands through the same parser and review gates.
- **Columns and sorting like k9s**: `views.yaml` (k9s format) sets per-resource columns, including custom `TITLE:.json.path` columns; sort by any column with `:sort COLUMN [asc|desc]`, and pressing a sort key again reverses it. Ctrl-W wide columns, `1`–`9` recent namespaces, recent contexts first in `:ctx`.
- **CronJobs and rollbacks**: `t` triggers a CronJob now, `s` suspends or resumes it; `:rollback [REVISION]` restores a Deployment, StatefulSet or DaemonSet revision with the image change in the review.
- **Ephemeral debug containers** (`--write --allow-debug`): `:debug [CONTAINER]` or `kborg debug POD`, reviewed and sharing the target's process namespace.
- **Keys and looks you control**: `keymap.yaml` remaps any key (confirmation keys stay fixed), `?` opens help for the current view with your real keys, `:keymap` lists every action; `ui.yaml` sets borders, header and hint rows, table style, color overrides and dashboard layout (`:ui reload`, `kborg ui init`).
- **Masked Secret YAML** in `:secrets` (`y`/`d`, `--allow-secret-reveal`): the whole Secret with every value hidden, TLS certificate subject and expiry, registry hostnames for pull secrets; `x` reveals one key for 30 s. Generic views still refuse Secrets and point to `:secrets`.
- **Live charts**: `g` in `:metrics` charts CPU and memory over time for the selected Pod or Node (with % of request, limit or allocatable); `G` on the dashboard shows four trend charts for the focused cluster. Braille, block or ASCII rendering, set with `ui.yaml` `charts:`.
- **Command palette** (Ctrl-P): fuzzy search over the current screen's actions with their keys (remapped keys included), every `:` command, resource kinds and aliases, recent contexts and namespaces, and the last 50 opened objects. Enter runs the entry exactly like its key or command, so every opt-in, review and confirmation still applies; unavailable actions show why. Remap it as `palette` in `keymap.yaml`.
- **Every screen remappable**: Helm, pickers, themes, saved selections, RBAC subjects, the tail picker, forms and the Secrets view are in the keymap, with their own `?` help.
- **Ask Copilot about the screen** (`--copilot-policy ABS --allow-copilot-screen`): `:ask QUESTION` sends your question and the exact text of the view behind the prompt (table, describe, logs, dashboard) after you read it all and type `SEND SCREEN TO HOST AS ACCOUNT`. Same sandbox and egress allowlist as `:copilot`; answers are advice only.
- **Copilot payload fix**: the reviewed payload now reaches the model. The pinned Copilot CLI ignores stdin when `-p` is given, so earlier `:copilot` answers were produced without the bundle; instruction and data now go together on stdin, never in the argument list.
- **Bulk actions on marked rows**: Ctrl-D, `r` and CronJob `s` act on every marked object (up to 50) through one combined, dry-run review; type e.g. `delete 7 objects`; stops at the first failure and reports what ran.
- **Image vulnerability scanning** with your own Trivy or Grype (`--scanner ABS_PATH --allow-scan`): `:scan`, `:scan ns`, `kborg scan … [--json]`; severity counts and top findings per image.
- **Linux packages** (`.deb`, `.rpm` spec) and a README demo GIF; Homebrew tap live (`brew install c0dewars/tap/kborg`).
- **Fixes**: Windows test suite (and demo Helm forms on Windows), PgUp/PgDown with custom layouts, an intermittent snapshot-lock failure; serde-saphyr 2.0.
- **Ctrl-Z errors-only view** and **15-minute trend sparklines** on dashboard cards.
- **Fix**: `-n all` now means every namespace (it was read as a namespace named `all`); `-A`/`--all-namespaces` work too.
- **Port-forward benchmark**: `b` in `:pf` or `:bench` — bounded HTTP GETs over the loopback forward with p50/p90/p99 latency.
- **Busiest Nodes on dashboard cards**: up to three Nodes per cluster, NotReady and cordoned first, then the highest CPU or memory share of each Node's own allocatable.
- **Actionable errors**: Kubernetes, network and kubeconfig errors now say what failed, why and what to do next. A denied request names the verb, resource and namespace and suggests the exact `kubectl auth can-i` check or a context switch; TLS, DNS, refused and timeout errors name the API server host and point to kubeconfig or VPN/proxy; conflicts ask you to reopen and review again; admission webhook, Pod Security and quota denials show the policy's message (sanitized and bounded). The status bar keeps the next step visible when the message is long.
- **Full Unicode display**: names, logs, YAML, describe and the dashboard show real glyphs in UTF-8 terminals, with Japanese, emoji, ZWJ families, flags and combining accents aligned by terminal cell; truncation never splits a character. `text: ascii` in `ui.yaml` (or a non-UTF-8 locale) keeps `?`. Approval screens still escape non-ASCII as `\u{...}`.
- **Watch traffic diet**: watches renew every 55 s instead of 8 s, request bookmarks, and keep their cache through a dropped connection or API outage, resuming at the last seen resourceVersion; only 410 Gone triggers a full relist. LIST and other reads negotiate gzip (a 5,000-ConfigMap LIST drops from 1.8 MiB to 195 KiB); compressed and decoded bytes both stay within the existing 8 MiB body budget. `kborg watch --json` reports `relists`. See [PERFORMANCE_COMPARISON.md](PERFORMANCE_COMPARISON.md).
- **Full interactive shell**: `s`/`a`, node shell, `:debug` and `--interactive exec/attach` are now a real terminal, like `kubectl exec -it`: vim, top, less, htop and arrow keys work, resizes are forwarded, and Ctrl-] returns to KBorg (`--session-escape ctrl-q` picks another key). The remote program controls your terminal while connected; on return KBorg resets alternate screen, mouse, paste, keypad, cursor, colors and title, then redraws. Same exact review, UID recheck and audit; `--read-only` or an operator config turns sessions off. Limits: lifetime, 16 MiB output and 8 MiB input (was 1 MiB); hitting one ends the session and the status line says which.
- **Security**: external [audit pack](SECURITY_AUDIT_PACK.md); configuration profiles now gate every capability; stricter sanitizing of invisible Unicode and object keys; OCI redirects and token realms that resolve to private addresses are refused; stale Helm credential workspaces are swept after a crash.
- **Signed, attested releases**: a manual `Signed release` workflow builds every archive plus `.deb`/`.rpm`, writes `SHA256SUMS`, signs each asset with keyless cosign, records GitHub build provenance and opens a draft release; macOS binaries are codesigned and notarized once the Apple secrets are configured. Verify with `gh attestation verify` or `cosign verify-blob` (see [Release process](RELEASE_PROCESS.md#verify-a-release-asset)).
- **Supply chain gates**: `deny.toml` with cargo-deny (advisories, licenses, duplicate crates, crates.io only) in CI, and a manual OpenSSF Scorecard workflow.
- **Fuzzing**: 12 fuzz targets for the parsers that read untrusted input (kubeconfig, manifests, tar extraction, terminal sanitizer, k9s views/hotkeys/aliases/plugins, keymap/ui.yaml, masked Secrets and the certificate walker, Helm release storage). They check invariants, not only crashes, run on every `cargo test`, and run coverage-guided with cargo-fuzz (manual **Fuzz** workflow). Fixes they led to: `extensions import-k9s` now shell-quotes the commands it prints (a hostile `plugins.yaml` could add `;` or backticks) and reports instead of clipping; duplicate kubeconfig names in separate YAML documents are refused; apply refuses a non-string `metadata.namespace` instead of using the selected namespace; an invalid apply namespace is sanitized in the error; `safe_lines` is idempotent.

## Upgrade notes

- A `--config` file now also constrains `--allow-secret-reveal`, `--allow-force-kill`, `--allow-yaml-edit`, `--allow-node-maintenance`, `--allow-external-editor`, `--allow-file-copy`, `--allow-apply`, `--allow-node-shell`, `--allow-debug` and `--allow-scan`. If you use those flags with a configuration file, add the matching `allow_*` field set to `true`. Without a configuration file nothing changes.

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
