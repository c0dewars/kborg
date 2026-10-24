<div align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/kborg-logo-dark.png">
    <img src="assets/kborg-logo.png" alt="KBorg" width="360">
  </picture>
  <h3>Every cluster at a glance. Every change reviewed.</h3>
  <p>A fast Kubernetes terminal workspace with k9s muscle memory, a k10s-style multi-cluster dashboard, and a safety model built for production.</p>
  <p>
    <a href="#install">Install</a> ·
    <a href="#run">Run</a> ·
    <a href="#keys-for-k9s-users">Keys</a> ·
    <a href="docs/MAC_V1_PLAN.md">Roadmap</a> ·
    <a href="docs/REMAINING_WORK.md">Feature status</a>
  </p>
  <p>
    <img alt="Version 0.3.0" src="https://img.shields.io/badge/version-0.3.0-FF7A1A">
    <img alt="Rust" src="https://img.shields.io/badge/built%20with-Rust-0FA968">
    <img alt="macOS and Linux" src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-0B1220">
    <img alt="Apache 2.0" src="https://img.shields.io/badge/license-Apache%202.0-3BE08A">
  </p>
</div>

![KBorg dashboard on a live Docker Desktop cluster: Pod health, Node usage, degraded workloads and restart hotspots](docs/screenshots/dashboard-health.png)

## Why KBorg

- **You already know the keys.** `:pods`, `:deploy`, `/filter`, `l` logs, `s` shell, Ctrl-D delete — k9s navigation, commands and aliases work the way you expect.
- **See all your clusters at once.** Up to nine clusters side by side: Pod health, restarts, OOM kills, Node readiness and CPU/memory, degraded Deployments and StatefulSets, and recent warnings, each updating independently.
- **Follow one workload across every cluster.** Pick Deployments from any cluster and tail them together in one pane, merged in timestamp order and tagged by cluster.
- **Change things without fear.** Read-only by default. Every change gets a server dry run, a readable diff, identity checks and typed confirmation. Nothing is applied by surprise.
- **Ask Copilot, safely.** Optional advice from GitHub Copilot on GitHub.com, GHE.com or GitHub Enterprise Server, with a minimized, reviewed payload and a sandboxed runtime.
- **Fast, and light on your laptop and your API server.** In side-by-side tests KBorg showed data in 0.06 s (k9s 0.69 s), used about a quarter of k9s's memory and half its CPU at 5,000 live objects, and a fraction of k10s's network traffic ([numbers](docs/PERFORMANCE_COMPARISON.md)).
- **Looks good in any terminal.** 19 hand-tuned themes that meet WCAG AA contrast everywhere they're drawn (plus your terminal's own palette), 24-bit color with graceful fallback, and a quiet layout with contextual key hints.

## Features

| Area | What you get |
|---|---|
| **Browse** | 35 built-in resource types, CRDs via discovery, context/namespace switching, regex/fuzzy/label filters, sorting, redacted describe/YAML/JSON, live watches |
| **Dashboard** | 1–9 clusters, full-screen focus, Pod health, restart hotspots, OOM evidence, Node readiness + CPU/memory, degraded workloads, Pod and non-Pod warning events, warning → exact Pod Events drilldown |
| **Logs** | Container, workload and selector logs; merged multi-container streams; live rollout following (`:logs-live`); cross-cluster workload tail; time ranges, search, error/warning filters, export |
| **Operate** | Scale, restart, delete, force kill, YAML spec editing (in-app or `$EDITOR`), image/resource forms, cordon/uncordon/drain, port forwards, shell/attach — each behind its own opt-in and review |
| **Helm** | Release browsing and history; reviewed install, upgrade, rollback, uninstall and tests for packaged and OCI charts with optional OpenPGP/Sigstore verification |
| **Secrets** | Opt-in key listing; reveal one value for 30 seconds; never exported or copied |
| **Metrics** | Pod/Node CPU and memory tables, exact values, requests/limits comparisons, bounded history |
| **Copilot** | Minimized diagnostic preview, exact account approval, contained native runtime (macOS ARM64) |
| **Workspace** | Saved context groups and scopes, remembered dashboard (`kborg dashboard`), custom aliases, 20 themes plus custom palettes, shell completions |

![Cross-cluster tail: two clusters' checkout services merged in timestamp order](docs/screenshots/cross-cluster-tail.png)

## Install

**Requirements:** macOS or Linux, a kubeconfig, and a terminal with at least 256 colors. Building from source needs Rust (the pinned toolchain installs automatically through `rustup`) and on macOS the command-line developer tools (`xcode-select --install`).

### From source

```sh
git clone https://github.com/c0dewars/kborg.git
cd kborg
cargo build --release --locked
```

Put the binary on your `PATH`:

```sh
mkdir -p ~/.local/bin
cp target/release/kborg ~/.local/bin/
kborg --version
```

Add shell completions (zsh shown; `bash`, `fish` and `powershell` also work):

```sh
kborg completions zsh > ~/.zfunc/_kborg
```

### From a release archive

Download the archive for your OS and CPU (for example `kborg-0.3.0-aarch64-apple-darwin.tar.gz`), get its SHA-256 through a trusted channel, and compare before extracting:

```sh
shasum -a 256 kborg-0.3.0-aarch64-apple-darwin.tar.gz
tar -xzf kborg-0.3.0-aarch64-apple-darwin.tar.gz
```

Packages are currently unsigned; see [Operations](docs/OPERATIONS.md) for verification, updates and rollback.

## Run

Try it without a cluster:

```sh
kborg demo
```

Browse your current cluster, read-only:

```sh
kborg --context docker-desktop --allow-logs
```

Open the multi-cluster dashboard. KBorg remembers it, so next time `kborg dashboard` brings it straight back:

```sh
kborg --contexts prod-eu,prod-us,staging --allow-logs
kborg dashboard
```

Enable operator workflows only when you need them. Each still asks for review and confirmation:

```sh
kborg --context docker-desktop --allow-logs \
  --write --allow-yaml-edit --allow-node-maintenance --allow-forward --allow-exec
```

KBorg uses your kubeconfig directly: the current context unless you pass `--context`, and every file in `$KUBECONFIG` merged the way kubectl does (first definition wins). kubectl, Helm, Python and an AI account are not needed for browsing. Server-side RBAC and admission always have the final say.

## Keys for k9s users

| Key / command | Action |
|---|---|
| `:pods` `:deploy` `:svc` `:nodes` … | Open a resource table (k9s aliases, CRDs and `:apps/v1/deployments` work) |
| `:ctx` `:ns`, `:pods @CTX`, `:pods NS` | Switch context or namespace |
| `/` `/!` `/-f` `/-l` | Regex, inverse, fuzzy, label-selector filter |
| `Enter` `d` `y` | Drill down, describe, YAML |
| `l` `p` | Logs, previous logs |
| `s` `a` `F` `f` | Shell, attach, new port forward, forward list |
| `s` `r` on workloads | Scale, restart |
| `e` | Edit YAML spec (Ctrl-E opens `$EDITOR` with `--allow-external-editor`) |
| Ctrl-D, Ctrl-K | Reviewed delete, reviewed force kill |
| `c` `u` `r` on Nodes | Cordon, uncordon, drain |
| `:secrets`, `x` | Secret keys, reveal one value |
| `:helm`, `:pulses`, `:xray` | Releases, health, relationships |
| `:subjects` `:users` `:groups`, `:can` | RBAC subjects with their roles; one subject's bindings and rules |
| `D` / `kborg dashboard` | Dashboard |
| Dashboard `1`–`9`, `w`, `L`, `t` | Focus (again = full screen), select warning, Pod logs, cross-cluster tail |
| `?` | The complete keymap |

The bottom row always shows the keys that work for what's selected and the flags you started with. The [k9s compatibility map](docs/K9S_COMPATIBILITY.md) documents every command and the few deliberate differences.

## Metrics (CPU and memory)

CPU/memory columns, `:metrics` and the dashboard's Node usage need the Kubernetes Metrics API (metrics-server). Check a cluster:

```sh
kubectl --context docker-desktop top nodes
kborg --context docker-desktop metrics nodes
```

If `kubectl top` fails, install a provider:

- **Docker Desktop:** the pinned, TLS-verified helper in [Docker Desktop metrics](docs/DESKTOP_METRICS.md).
- **minikube:** `minikube addons enable metrics-server`, then wait a minute for samples. The addon uses `--kubelet-insecure-tls`, fine for a disposable local cluster only.
- **Other clusters:** upstream [metrics-server](https://github.com/kubernetes-sigs/metrics-server#installation).

Without a provider KBorg keeps working and shows `-` or `metrics UNAVAILABLE`, never zeros.

## Themes

![All built-in themes rendered by KBorg](docs/screenshots/themes.png)

KBorg greets you with a short animated logo that decrypts out of glyph noise, catches a light beam and fades into your workspace. Any key skips it; `kborg startup preview` replays it, and `kborg startup off`, `--no-splash` or `KBORG_NO_SPLASH=1` turn it off.

Pick a theme with `:themes` (live preview) or `--theme rose-pine`. Every fixed built-in theme is tested for 4.5:1 contrast on each surface the UI paints, including dashboard cards and selected rows. Create your own with `kborg theme new my-theme --base rose-pine`; see the [theme guide](docs/themes.md). `NO_COLOR` and `--color=never` turn styling off.

## Terminals

KBorg detects color support from the environment and works without terminfo.

| Terminal | Color |
|---|---|
| Ghostty, kitty, WezTerm, Alacritty, iTerm2, VS Code, Windows Terminal | 24-bit |
| macOS Terminal.app, tmux, screen | 256 colors (24-bit when `COLORTERM=truecolor` is passed through) |
| Linux console, plain `xterm` | 16 colors with ASCII fallback |

**Ghostty:** run `kborg` from any Ghostty window, or open a dedicated one:

```sh
open -na Ghostty.app --args -e "$HOME/.local/bin/kborg" --context docker-desktop --allow-logs
```

Ghostty's shortcuts use Cmd on macOS, so Ctrl-D/K/S/R reach KBorg, and `c` copies names through OSC 52. Over SSH KBorg still recognizes `TERM=xterm-ghostty` as 24-bit. Match your Ghostty theme with the same KBorg theme (for example `--theme catppuccin-mocha`), or use `--theme terminal` for Ghostty's own palette.

## Copilot advice (optional)

`:copilot` previews a minimized status bundle for the selected resource: no names, messages, manifests or logs. With an explicit policy file, KBorg sends that exact bundle to Copilot after you approve the host and account, through a sandboxed native runtime whose network is limited to the deployment's Copilot endpoints. Works with GitHub.com, GHE.com tenants and GitHub Enterprise Server 3.22+ (the enterprise targets follow GitHub's documented endpoints but have not yet been verified against a live tenant). Advice is never executed. See [Copilot](docs/COPILOT_INTEGRATION.md).

## Security model

- Read-only until you pass `--write`; every risky capability (exec, attach, port forward, node maintenance, YAML edit, force kill, Secret reveal, Helm writes, external editor) has its own opt-in.
- Every mutation: server dry run, UID/resourceVersion preconditions, full review that must be displayed, exact typed confirmation, local intent record.
- Known sensitive fields are masked; Secrets are excluded unless you opt in. No telemetry.
- Dashboard and log-pane connections strip all write and interactive capabilities.
- Dependencies are checked against RustSec on every release review (currently zero known advisories).

Details: [SECURITY.md](SECURITY.md) and the [security review](docs/MAC_V1_SECURITY_REVIEW.md).

## Documentation

| Guide | Contents |
|---|---|
| [Dashboard](docs/DASHBOARD.md) | Health meanings, freshness, cross-cluster tail, card controls |
| [k9s compatibility](docs/K9S_COMPATIBILITY.md) | Commands, keys and differences |
| [Operations](docs/OPERATIONS.md) | Configuration, install, update, rollback, recovery |
| [YAML editing](docs/YAML_EDITING.md) · [v1 operations](docs/MAC_V1_OPERATIONS.md) | Reviewed edits, node maintenance |
| [Helm lifecycle](docs/HELM_LIFECYCLE.md) | Supported chart workflows |
| [Copilot](docs/COPILOT_INTEGRATION.md) | Account setup, enterprise deployments, containment |
| [CLI help](docs/CLI_HELP.txt) · [CLI output](docs/CLI_OUTPUT.md) | Scripting with `--json` |
| [Hardening acceptance](docs/MAC_V1_HARDENING_ACCEPTANCE.md) | Soak, terminal and security evidence |
| [Performance comparison](docs/PERFORMANCE_COMPARISON.md) | Memory, CPU and API traffic vs k9s and k10s |
| [Remaining work](docs/REMAINING_WORK.md) | What's done, partial and planned |

**Development release 0.3.0**, targeting macOS + Docker Desktop for v1. Packages are unsigned; signing, an external security audit and an organizational pilot are open release gates.

## Credits

Navigation follows [k9s](https://github.com/derailed/k9s); the dashboard is inspired by [k10s](https://github.com/mpiorowski/k10s); palettes draw on [Omarchy](https://omarchy.org/themes/) and the upstream theme authors in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

Licensed under [Apache 2.0](LICENSE).
