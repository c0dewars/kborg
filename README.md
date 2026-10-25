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
    <img alt="Release 0.4.0" src="https://img.shields.io/badge/release-0.4.0-FF7A1A">
    <img alt="Preview 0.5.0-rc.3" src="https://img.shields.io/badge/preview-0.5.0--rc.3-FFA41B">
    <img alt="Rust" src="https://img.shields.io/badge/built%20with-Rust-0FA968">
    <img alt="macOS and Linux" src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-0B1220">
    <img alt="Apache 2.0" src="https://img.shields.io/badge/license-Apache%202.0-3BE08A">
  </p>
</div>

![KBorg feature tour in demo mode: k9s-style browsing, filters, the errors-only view, sorting, describe, logs, :lint, live metrics charts, a reviewed scale, masked Secret YAML, the multi-cluster dashboard and its trend charts, live theme preview, per-screen help and the remappable keymap](docs/screenshots/tour.gif)

<p align="center"><sub>50-second tour recorded in <code>kborg demo</code> (no cluster needed). Regenerate it with <code>scripts/make-demo-gif.py --tour</code>.</sub></p>

![KBorg dashboard on a live Docker Desktop cluster: Pod health, Node usage, degraded workloads and restart hotspots](docs/screenshots/dashboard-health.png)

## Why KBorg

- **You already know the keys.** `:pods`, `:deploy`, `/filter`, `l` logs, `s` shell, Ctrl-D delete — k9s navigation, commands, aliases and `hotkeys.yaml` work the way you expect.
- **See all your clusters at once.** Up to nine clusters side by side: Pod health, restarts, OOM kills, Node readiness and CPU/memory, degraded Deployments and StatefulSets, and recent warnings, each updating independently.
- **Follow one workload across every cluster.** Pick Deployments from any cluster and tail them together in one pane, merged in timestamp order and tagged by cluster.
- **Change things without fear.** Read-only by default. Every change, from a scale to a multi-object `kborg apply`, gets a server dry run, a readable diff, identity checks and typed confirmation. Nothing is applied by surprise.
- **Ask Copilot, safely.** Ask GitHub Copilot about the screen you are looking at (`:ask`) or get advice on a resource (`:copilot`), on GitHub.com, GHE.com or GitHub Enterprise Server. You review the exact text first, and it runs in a sandboxed runtime.
- **Fast, and light on your laptop and your API server.** In side-by-side tests KBorg showed data in 0.06 s (k9s 0.69 s), used about a quarter of k9s's memory and half its CPU at 5,000 live objects, and a fraction of k10s's network traffic ([numbers](docs/PERFORMANCE_COMPARISON.md)).
- **Looks good in any terminal.** 20 hand-tuned themes that meet WCAG AA contrast everywhere they're drawn (plus your terminal's own palette), 24-bit color with graceful fallback, and a quiet layout with contextual key hints.
- **Make it yours.** Like an Omarchy desktop, everything is data you control: remap any key (`keymap.yaml`), restyle the UI (`ui.yaml`: borders, header, density, colors, chart style), choose columns per resource (`views.yaml`), add hotkeys and aliases. `?` always shows the keys for the screen you're on, with your bindings.
- **See trends, not just snapshots.** Live CPU and memory charts per Pod or Node, and trend charts for every cluster on the dashboard.

## Features

| Area | What you get |
|---|---|
| **Browse** | 35 built-in resource types, CRDs via discovery, context/namespace switching, regex/fuzzy/label filters, sorting, redacted describe/YAML/JSON, live watches |
| **Dashboard** | 1–9 clusters, full-screen focus, Pod health, restart hotspots, OOM evidence, Node readiness + CPU/memory, degraded workloads, Pod and non-Pod warning events, warning → exact Pod Events drilldown |
| **Logs** | Container, workload and selector logs; merged multi-container streams; live rollout following (`:logs-live`); cross-cluster workload tail; time ranges, search, error/warning filters, export |
| **Operate** | Scale, restart, rollback, delete, force kill, reviewed batches on marked rows (up to 50), CronJob trigger and suspend/resume, YAML spec editing (in-app or `$EDITOR`), server-side apply of local manifests, image/resource forms, cordon/uncordon/drain, node shell, ephemeral debug containers, port forwards, shell/attach, file and directory copy — each behind its own opt-in and review |
| **Errors** | Every Kubernetes, network and kubeconfig error says what failed, why (RBAC verb/resource/namespace, TLS, DNS, timeout, conflict, admission, quota) and the next step, such as the exact `kubectl auth can-i` command |
| **Check** | `:lint` misconfiguration report, `:scan` image vulnerability scan through your Trivy or Grype, Ctrl-Z errors-only view, `:pulses` health, `:xray` relationships, busiest Nodes on dashboard cards, port-forward benchmark |
| **Helm** | Release browsing and history; reviewed install, upgrade, rollback, uninstall and tests for packaged and OCI charts with optional OpenPGP/Sigstore verification |
| **Secrets** | Opt-in key listing and masked YAML/describe (`y`/`d`: values shown as sizes, TLS subject/expiry, registry hostnames); reveal one value for 30 seconds; never exported or copied |
| **Metrics & charts** | Pod/Node CPU and memory tables with exact values and request/limit comparisons; live charts (`g` in `:metrics`) with units and % of request, limit or allocatable; dashboard trend charts (`G`) and 15-minute sparklines; braille, block or ASCII rendering |
| **Copilot** | Minimized diagnostic preview, `:ask` about the current screen, exact account approval, contained native runtime (macOS ARM64) |
| **Workspace** | Saved context groups and scopes, remembered dashboard (`kborg dashboard`), k9s-format `views.yaml` columns, sort by any column, recent namespaces on `1`–`9`, custom aliases and k9s-format hotkeys, every key remappable on every screen (`keymap.yaml`, `?` help per screen, `:keymap`), UI styling (`ui.yaml`), reviewed read-only extensions (`:ext`, k9s `plugins.yaml` import), 20 themes plus custom palettes, shell completions |

![Cross-cluster tail: two clusters' checkout services merged in timestamp order](docs/screenshots/cross-cluster-tail.png)

## Install

**Requirements:** macOS or Linux, a kubeconfig, and a terminal with at least 256 colors. Building from source needs Rust (the pinned toolchain installs automatically through `rustup`) and on macOS the command-line developer tools (`xcode-select --install`).

### With Homebrew (macOS and Linux)

```sh
brew install c0dewars/tap/kborg
```

The formula builds KBorg from the tagged source with Homebrew's Rust and installs bash, zsh and fish completions. Upgrade with `brew upgrade kborg`.

### Try the 0.5.0 preview

```sh
cargo install --locked --git https://github.com/c0dewars/kborg --tag v0.5.0-rc.3
```

The [testing guide](docs/TESTING.md) has a 15-minute tour and what to report.

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

Download the archive for your OS and CPU (for example `kborg-0.4.0-aarch64-apple-darwin.tar.gz`), get its SHA-256 through a trusted channel, and compare before extracting:

```sh
shasum -a 256 kborg-0.4.0-aarch64-apple-darwin.tar.gz
tar -xzf kborg-0.4.0-aarch64-apple-darwin.tar.gz
```

Releases built by the signed release workflow carry a Sigstore bundle (`FILE.sigstore.json`) and GitHub build provenance for every asset. Verify before installing:

```sh
gh attestation verify kborg-0.6.0-aarch64-apple-darwin.tar.gz --repo c0dewars/kborg
cosign verify-blob kborg-0.6.0-aarch64-apple-darwin.tar.gz \
  --bundle kborg-0.6.0-aarch64-apple-darwin.tar.gz.sigstore.json \
  --certificate-identity-regexp '^https://github\.com/c0dewars/kborg/\.github/workflows/release\.yml@refs/tags/v' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

Releases up to 0.5.0-rc.3 are unsigned. See [Release process](docs/RELEASE_PROCESS.md#verify-a-release-asset) for details and [Operations](docs/OPERATIONS.md) for updates and rollback.

### Debian/Ubuntu (.deb) and Fedora/RHEL (.rpm)

```sh
sudo apt install ./kborg_0.5.0~rc.3_amd64.deb      # Debian / Ubuntu
sudo dnf install ./kborg-0.5.0~rc.3-1.x86_64.rpm   # Fedora / RHEL-compatible
```

Both install `/usr/bin/kborg` and bash, zsh and fish completions, and need glibc 2.39 or newer. CI builds the .deb; the .rpm is built from `packaging/rpm/kborg.spec`. See [Operations](docs/OPERATIONS.md#install-from-a-deb-or-rpm).

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

More opt-ins, each reviewed:

| Flag | Adds |
|---|---|
| `--allow-apply` | `kborg apply FILE` / `:apply FILE`: dry run, field-level diff, server-side apply that never forces ownership |
| `--allow-file-copy` | `:cp-from` / `:cp-to` (`-r` for directories); neither side ever overwrites |
| `--allow-node-shell` | `s` on a Node: a reviewed privileged Pod and a root shell, deleted afterwards |
| `--allow-debug` | `:debug [CONTAINER]`: a reviewed ephemeral debug container and a shell in it |
| `--scanner ABS_PATH --allow-scan` | `:scan` / `:scan ns` / `kborg scan`: image vulnerability scan with your installed Trivy or Grype (network: database and image pulls) |
| `--allow-extensions --allow-tools` | `:ext NAME`: hand the selected object to a registered read-only tool |

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
| `Space` then Ctrl-D / `r` / `s` | Mark rows, then one reviewed batch delete, restart or CronJob suspend/resume (max 50; type e.g. `delete 7 objects`) |
| `c` `u` `r` on Nodes | Cordon, uncordon, drain |
| `:secrets`, `x`, `y` | Secret keys, reveal one value, masked YAML |
| `:helm`, `:pulses`, `:xray`, `:lint` | Releases, health, relationships, misconfigurations |
| `s` on Nodes, `:apply FILE`, `:dir` then `a` | Node shell, reviewed apply of a manifest |
| `:cp-from` `:cp-to` (`-r`) | Copy files or directories to and from the selected Pod |
| `:pf` then `b`, `:bench` | Benchmark a port forward |
| `:ext NAME`, `hotkeys.yaml` | Run a registered extension; bind keys like k9s |
| `t` `s` on CronJobs, `:rollback [REV]` | Trigger now, suspend/resume; roll back a workload |
| `:debug [CONTAINER]` | Ephemeral debug container in the selected Pod |
| Ctrl-Z, Ctrl-W, `1`–`9` | Errors only, wide columns, recent namespaces |
| `N` `A` `S` `R` (again to reverse), `:sort COL` | Sort by any column; `views.yaml` sets columns |
| `:subjects` `:users` `:groups`, `:can` | RBAC subjects with their roles; one subject's bindings and rules |
| `D` / `kborg dashboard` | Dashboard |
| Dashboard `1`–`9`, `w`, `L`, `t`, `G` | Focus (again = full screen), select warning, Pod logs, cross-cluster tail, trend charts |
| `:metrics` then `g` | Live CPU and memory chart for the selected Pod or Node |
| `?`, `:keymap` | Keys for the screen you're on (Tab: every screen); every action and its key |

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

To style and lay out the UI to your taste, run `kborg ui init` and edit the generated `ui.yaml`: borders (rounded, square, ASCII or none), a compact header, hidden key hints, zebra rows, column separators, long ages, local log timestamps, dashboard card columns and per-role color overrides on top of any theme. It is data only and validated; `:ui reload` applies changes live. Every option is listed in the [ui.yaml reference](docs/themes.md#uiyaml).

## Make it yours

Everything below lives in KBorg's configuration directory (macOS `~/Library/Application Support/KBorg`, Linux `~/.config/kborg`, or `KBORG_CONFIG_DIR`). All files are data only, strictly validated and bounded; a broken file shows a notice and KBorg falls back to its defaults.

| File | Controls |
|---|---|
| `ui.yaml` | Borders, header and hint rows, compact mode, table density, zebra rows, separators, markers, age format, color overrides, chart style, dashboard layout (`kborg ui init`, `:ui reload`) — [reference](docs/themes.md#uiyaml) |
| `keymap.yaml` | Any key on any screen; Enter, Esc and typed confirmations stay fixed — [format](docs/K9S_COMPATIBILITY.md#custom-keymap) |
| `views.yaml` | Columns and default sort per resource, including `TITLE:.json.path` columns (k9s format) — [format](docs/K9S_COMPATIBILITY.md#custom-views-columns-and-sort) |
| `hotkeys.yaml`, `aliases.yaml` | Keys and names that run commands (k9s format) |
| Themes | 20 built-ins, live preview in `:themes`, `kborg theme new` for your own |

## Terminals

KBorg detects color support from the environment and works without terminfo.

| Terminal | Color |
|---|---|
| Ghostty, kitty, WezTerm, Alacritty, iTerm2, VS Code, Windows Terminal | 24-bit |
| macOS Terminal.app, tmux, screen | 256 colors (24-bit when `COLORTERM=truecolor` is passed through) |
| Linux console, plain `xterm` | 16 colors with ASCII fallback |

**Unicode:** in a UTF-8 locale, object names, logs, YAML and describe text show real glyphs: Japanese and other wide characters and emoji (including ZWJ families and flags) take two cells, combining accents none, and columns stay aligned. Truncation never cuts a character in half. Control, bidi and invisible format characters are still removed. Without a UTF-8 locale, or with `text: ascii` in `ui.yaml`, non-ASCII characters are drawn as `?`. Approval screens always show non-ASCII as `\u{...}` escapes so look-alike characters can be told apart.

**Ghostty:** run `kborg` from any Ghostty window, or open a dedicated one:

```sh
open -na Ghostty.app --args -e "$HOME/.local/bin/kborg" --context docker-desktop --allow-logs
```

Ghostty's shortcuts use Cmd on macOS, so Ctrl-D/K/S/R reach KBorg, and `c` copies names through OSC 52. Over SSH KBorg still recognizes `TERM=xterm-ghostty` as 24-bit. Match your Ghostty theme with the same KBorg theme (for example `--theme catppuccin-mocha`), or use `--theme terminal` for Ghostty's own palette.

## Copilot advice (optional)

`:copilot` previews a minimized status bundle for the selected resource: no names, messages, manifests or logs. With an explicit policy file, KBorg sends that exact bundle to Copilot after you approve the host and account, through a sandboxed native runtime whose network is limited to the deployment's Copilot endpoints. Works with GitHub.com, GHE.com tenants and GitHub Enterprise Server 3.22+ (the enterprise targets follow GitHub's documented endpoints but have not yet been verified against a live tenant). Advice is never executed. See [Copilot](docs/COPILOT_INTEGRATION.md).

`:ask QUESTION` asks Copilot about what you are looking at. KBorg captures the view behind the `:` prompt as plain text (table, describe, logs, dashboard), shows you exactly what will be sent, and sends it only after you type `SEND SCREEN TO HOST AS ACCOUNT`. It needs `--allow-copilot-screen` on top of the policy, because screen text includes names and log lines; without it `:ask` shows a local preview.

```sh
kborg --copilot-policy "$PWD/copilot-policy.json" --allow-copilot-screen
# then, on any screen:  :ask why is this pod restarting?
```

## Security model

- Read-only until you pass `--write`; every risky capability (exec, attach, port forward, file copy, apply, node shell, debug containers, image scanning, node maintenance, YAML edit, force kill, Secret reveal, Helm writes, external editor, extensions) has its own opt-in, and a `--config` profile can forbid any of them.
- Every mutation: server dry run, UID/resourceVersion preconditions, full review that must be displayed, exact typed confirmation, local intent record.
- Known sensitive fields are masked; Secrets are excluded unless you opt in. No telemetry.
- Dashboard and log-pane connections strip all write and interactive capabilities.
- Dependencies are checked against RustSec on every release review (currently zero known advisories).

Details: [SECURITY.md](SECURITY.md) and the [security review](docs/MAC_V1_SECURITY_REVIEW.md).

## Documentation

| Guide | Contents |
|---|---|
| [Testing guide](docs/TESTING.md) | For colleagues trying a release candidate: install, tour, what to report |
| [Dashboard](docs/DASHBOARD.md) | Health meanings, freshness, cross-cluster tail, card controls |
| [k9s compatibility](docs/K9S_COMPATIBILITY.md) | Commands, keys and differences |
| [Operations](docs/OPERATIONS.md) | Configuration, install, update, rollback, recovery |
| [YAML editing](docs/YAML_EDITING.md) · [v1 operations](docs/MAC_V1_OPERATIONS.md) | Reviewed edits, node maintenance |
| [Helm lifecycle](docs/HELM_LIFECYCLE.md) | Supported chart workflows |
| [Copilot](docs/COPILOT_INTEGRATION.md) | Account setup, enterprise deployments, containment |
| [Roadmap](docs/ROADMAP.md) | Planned performance, security, feature, UI and UX work with measurable targets |
| [CLI help](docs/CLI_HELP.txt) · [CLI output](docs/CLI_OUTPUT.md) | Scripting with `--json` |
| [Hardening acceptance](docs/MAC_V1_HARDENING_ACCEPTANCE.md) | Soak, terminal and security evidence |
| [Performance comparison](docs/PERFORMANCE_COMPARISON.md) | Memory, CPU and API traffic vs k9s and k10s |
| [Remaining work](docs/REMAINING_WORK.md) | What's done, partial and planned |

**Release 0.4.0** (Homebrew), with **0.5.0-rc.3** in testing: see the [release notes](docs/RELEASE_NOTES.md) for apply, node shell, debug containers, file and directory copy, bulk actions, image scanning, `:lint`, live charts, masked Secret YAML and full keymap and UI customization. CI builds and tests Linux, macOS and Windows (x86_64 and ARM). Packages up to 0.5.0-rc.3 are unsigned; the signed, attested release workflow is in place for the next release (first run pending). An external security audit and an organizational pilot are open release gates.

## Credits

Navigation follows [k9s](https://github.com/derailed/k9s); the dashboard is inspired by [k10s](https://github.com/mpiorowski/k10s); palettes draw on [Omarchy](https://omarchy.org/themes/) and the upstream theme authors in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

Licensed under [Apache 2.0](LICENSE).
