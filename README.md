<div align="center">
  <img src="assets/kborg-logo.png" alt="KBorg" width="96">
  <h1>KBorg</h1>
  <p>A Kubernetes terminal workspace for exploring clusters and making reviewed changes.</p>
  <p><a href="#quick-start">Get started</a> · <a href="#navigation">Navigation</a> · <a href="docs/MAC_V1_PLAN.md">v1 roadmap</a> · <a href="docs/REMAINING_WORK.md">Feature status</a></p>
</div>

![KBorg dashboard in Rosé Pine — actual terminal capture with demo data](docs/evidence/ui-dashboard-final/demo/dashboard-context-cards.png)

Familiar `:pods`, `:nodes`, `:deploy` navigation. A full-width table, a quiet header with `?` help, continuous borders, and 20 themes. Built in Rust, with a native Kubernetes client and a CLI alongside the terminal UI.

**Development release 0.3.0, targeting Mac + Docker Desktop v1.** See the [current acceptance report](docs/MAC_V1_HARDENING_ACCEPTANCE.md) for what was tested and the [remaining-work register](docs/REMAINING_WORK.md) for open release gates. Packages are currently unsigned.

## Features

- **Browse your cluster.** Built-in resource types, API discovery, CRDs, context and namespace switching, filters, sorting, resource inspection, and live watches.
- **See workload health.** A k10s-style dashboard for up to four clusters: Pod readiness, restarts, OOM kills, UID-linked recent warnings, Node readiness with CPU/memory, and degraded Deployments/StatefulSets by name. Full-screen focus per cluster, plus Pod/Node metrics tables, bounded history and ownership trees.
- **Tail across clusters.** Pick workloads from every dashboard cluster and follow them in one pane, merged in timestamp order and tagged by cluster, with error/warning filters.
- **Work with logs.** Container, workload and selector logs; merged source labels; search, timestamps, wrapping, error/warning text filters, and explicit local export.
- **Save your workspace.** Explicitly save context groups, namespace/resource scopes and selected workload identities; revalidate them when reopening.
- **Edit with a review.** An in-memory YAML spec editor, readable diffs, server dry-run, identity checks, and exact target confirmation. Hidden fields are preserved. Focused image, resource-budget, scale and restart forms remain available.
- **Maintain nodes.** Reviewed cordon, uncordon and bounded drain using Kubernetes eviction requests. Disruption budgets are respected; retained and blocked workloads are reported.
- **Keep several connections open.** Up to eight independently managed loopback port forwards, with per-session review, status and stop controls.
- **Manage Helm releases.** Reviewed install, upgrade, rollback, uninstall and tests for supported packaged charts, including OCI tags/digests, optional OpenPGP/Sigstore verification and scoped CRD workflows.
- **Make it yours.** Rosé Pine, Flexoki, Catppuccin, Tokyo Night, Nord, Dracula, Gruvbox, Solarized and accessible KBorg palettes, plus custom themes.
- **Optional Copilot advice.** Explicit GitHub account selection and a minimized diagnostic preview before an approved advisory request on supported macOS ARM64 setups, for GitHub.com, GHE.com tenants and GitHub Enterprise Server 3.22+.

## Quick start

Build from source with the pinned Rust toolchain and macOS command-line developer tools:

```sh
git clone https://github.com/c0dewars/kborg.git
cd kborg
cargo build --release --locked
```

Explore without a cluster:

```sh
./scripts/start-local.sh --demo --theme rose-pine --no-splash
```

Connect to Docker Desktop Kubernetes:

```sh
./scripts/start-local.sh --context docker-desktop --allow-logs --no-splash
```

Browsing is read-only by default. It uses your kubeconfig directly; Helm, kubectl, Python, Node.js and an AI account are not needed for ordinary browsing. Metrics require a working Metrics API; see [Metrics](#metrics-cpu-and-memory).

Enable the v1 operator workflows explicitly:

```sh
./scripts/start-local.sh --context docker-desktop --allow-logs \
  --write --allow-yaml-edit --allow-node-maintenance --allow-forward --no-splash
```

These flags make actions available. Each mutation still requires its own review and confirmation; server-side RBAC and admission remain authoritative.

Open straight into the multi-cluster dashboard (up to four contexts):

```sh
./target/release/kborg --contexts docker-desktop,staging --allow-logs
```

## Metrics (CPU and memory)

CPU/memory columns, `:metrics`, and the dashboard's Node usage need the Kubernetes Metrics API (metrics-server). Check whether a cluster has it:

```sh
kubectl --context docker-desktop top nodes
./target/release/kborg --context docker-desktop metrics nodes
```

If `kubectl top` fails, install a provider:

- **Docker Desktop:** run the pinned, TLS-verified helper described in [Docker Desktop metrics](docs/DESKTOP_METRICS.md).
- **minikube:** `minikube addons enable metrics-server`, then wait a minute for the first samples. This addon uses `--kubelet-insecure-tls`, which is acceptable for a disposable local cluster only.
- **Other clusters:** the upstream [metrics-server](https://github.com/kubernetes-sigs/metrics-server#installation) manifests or Helm chart.

Without a provider KBorg keeps working and shows `-` or `metrics UNAVAILABLE`, never zeros.

## Navigation

| Command or key | Action |
|---|---|
| `:pods`, `:po`, `:deploy`, `:svc`, `:nodes` | Open a resource table |
| `:ctx`, `:ns` | Choose context or namespace |
| `:pods NAMESPACE`, `:pods @CONTEXT` | Navigate directly |
| `/`, `:pods app=api` | Filter visible rows or use a server-side label selector |
| `Enter`, `d`, `y` | Drill down, describe, inspect YAML |
| `e`, `Ctrl-S` in the editor | Edit YAML spec, then prepare a reviewed diff |
| `:edit-container` | Open the focused container editor |
| Node `c`, `u`, `r` | Review cordon, uncordon, drain |
| Pod `F`, `:pf` | Add a port forward, open the session list |
| `l`, `p` | Follow logs or read previous logs |
| Log `e`, `W`, `w` | Toggle error matches, warning matches, wrapping |
| Log/Helm `Ctrl-S` | Review and save a local export |
| `:export`, resource `Ctrl-S` | Export selected redacted YAML, save visible resource snapshot |
| `:helm`, `:pulses`, `:xray po`, `:overview` | Releases, health, relationships, multiple contexts |
| `D`, `b`, `B` | Dashboard, save current selection, reopen saved selections |
| Dashboard `1..4`, `Enter`, `c`, `s` | Focus card (same number again: full screen), open Pods, choose contexts, save dashboard |
| Dashboard `w`, then `Enter` | Select a recent warning, open that exact Pod's Events |
| `:logs-live [SELECTOR]` | Follow workload logs with announced Pod joins and departures |
| Dashboard `w`, then `L` | Follow that warning Pod's logs in a pane under the cards |
| Dashboard `t` | Tail chosen workloads across every dashboard cluster, merged in timestamp order |
| `:secrets` (`--allow-secret-reveal`) | List Secret keys; `x` reveals one value for 30 seconds |
| Ctrl-K (`--write --allow-force-kill`) | Reviewed zero-grace Pod kill |
| `:themes`, `?`, `:q` | Themes, help, quit |

The header keeps only `?` for the keymap; it also supports mouse clicks. The bottom row lists the keys for what is selected, and only the ones your session can actually use: for example `l logs` appears only with `--allow-logs`, and `s shell` only with `--write --allow-exec`. `?` always opens the complete keymap. Familiar `:` commands remain available when you type them. The [compatibility map](docs/K9S_COMPATIBILITY.md) records exact behavior and remaining k9s differences.

## Reviewed changes

YAML editing applies to the `spec` of existing namespaced non-Secret resources. It does not create resources or expose hidden fields for editing. Metadata, status, ConfigMap data, RBAC objects and cluster-scoped YAML edits are excluded. See [YAML editing](docs/YAML_EDITING.md).

Drain first reviews the complete bounded Pod inventory. System, mirror and DaemonSet Pods are retained; unmanaged workloads and potentially lossy local storage block the operation. Failed drains leave the node cordoned and report remaining work. Node shell and forced eviction are excluded. See [v1 operations](docs/MAC_V1_OPERATIONS.md).

Helm lifecycle requires `--write --allow-helm --allow-helm-write`, a trusted native Helm executable and a supported chart. See [Helm lifecycle](docs/HELM_LIFECYCLE.md), [extensions](docs/HELM_EXTENSIONS.md), and [trust and CRD migrations](docs/HELM_TRUST_AND_MIGRATION.md).

## Themes

Choose `:themes` to preview and save a palette, or use `--theme rose-pine` for one session. Saved preferences survive updates. `NO_COLOR` and `--color=never` disable styling.

```sh
./target/release/kborg theme list
./target/release/kborg theme new my-theme --base rose-pine
```

See the [theme guide](docs/themes.md) for custom palettes and contrast validation. Every built-in theme is tested for WCAG AA contrast (4.5:1) on each surface the UI paints, including dashboard cards and selected rows; where an upstream color falls short, KBorg nudges its lightness while keeping its hue.

## Running in Ghostty

KBorg works in [Ghostty](https://ghostty.org) without extra setup. Ghostty sets `COLORTERM=truecolor`, and KBorg also recognizes `TERM=xterm-ghostty` directly, so themes render in full color even over SSH. KBorg does not use terminfo.

From a Ghostty window:

```sh
cd kborg
cargo build --release --locked
./target/release/kborg --context docker-desktop --allow-logs
```

Or open a dedicated Ghostty window for KBorg (macOS launches Ghostty through `open`):

```sh
open -na Ghostty.app --args -e "$PWD/target/release/kborg" --context docker-desktop --allow-logs
```

Notes:

- Ghostty's own shortcuts use Cmd on macOS, so KBorg's Ctrl keys (Ctrl-D, Ctrl-K, Ctrl-S, Ctrl-R) reach KBorg.
- `c` / `n` copy names through OSC 52; Ghostty allows clipboard writes by default (`clipboard-write = allow`).
- Over SSH to a host without Ghostty's terminfo, other tools may complain; KBorg does not, but you can run it with `TERM=xterm-256color COLORTERM=truecolor` for consistency.
- Pick a KBorg theme that matches your Ghostty theme (for example Ghostty `Catppuccin Mocha` with `--theme catppuccin-mocha`), or `--theme terminal` to use Ghostty's palette directly.

## CLI

```sh
./target/release/kborg --context docker-desktop -n default list pods --json
./target/release/kborg --context docker-desktop -n default metrics pods --json
./target/release/kborg --context docker-desktop -n default yaml-template deployments web > web.yaml
./target/release/kborg --context docker-desktop -n default --write --allow-yaml-edit \
  yaml-edit deployments web web.yaml
```

The edit command requires an interactive terminal for confirmation. [CLI help](docs/CLI_HELP.txt) and [output contracts](docs/CLI_OUTPUT.md) cover scripting and bounded reads.

## Documentation

| Guide | Contents |
|---|---|
| [v1 plan](docs/MAC_V1_PLAN.md) | Milestones and features inspired by k10s |
| [Mac v1 hardening](docs/MAC_V1_HARDENING_ACCEPTANCE.md) | Soak, terminal compatibility, security and current screenshots |
| [Dashboard acceptance](docs/DASHBOARD_ACCEPTANCE.md) | Dashboard and saved-selection implementation evidence |
| [Dashboard guide](docs/DASHBOARD.md) | Health meanings, freshness and card controls |
| [Saved selections](docs/SAVED_SELECTIONS.md) | Explicit persistence and revalidation |
| [Operator acceptance](docs/MAC_V1_ACCEPTANCE.md) | YAML, node, export and forwarding evidence |
| [Operations](docs/OPERATIONS.md) | Configuration, installation, update, rollback and recovery |
| [Copilot](docs/COPILOT_INTEGRATION.md) | Optional account setup, privacy and platform limits |
| [Security review](docs/MAC_V1_SECURITY_REVIEW.md) | Current findings, fixes and outstanding assessment work |
| [Remaining work](docs/REMAINING_WORK.md) | Implemented, partial and unsupported capabilities |

No telemetry is enabled. Known sensitive API fields are masked, but logs and custom resource fields can contain confidential data. Exports are explicit private files. Local audit records support reconciliation and are not a tamper-proof external audit service.

## Design and credits

Navigation follows [k9s](https://github.com/derailed/k9s). The dashboard layout takes inspiration from [k10s](https://github.com/mpiorowski/k10s); the palette direction also draws from [Omarchy](https://omarchy.org/themes/). Theme attribution and dependency notices are in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

Licensed under [Apache 2.0](LICENSE).
