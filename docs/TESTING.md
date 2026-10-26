# Testing KBorg 0.5.0

Thanks for testing. This guide gets you running in a few minutes and lists what to try. Report anything odd as a [bug](https://github.com/c0dewars/kborg/issues/new?template=bug.yml) or [feedback](https://github.com/c0dewars/kborg/issues/new?template=feedback.yml) issue. Report security problems privately through [vulnerability reporting](https://github.com/c0dewars/kborg/security/advisories/new), never in an issue.

## Ground rules

- **Use a disposable or test cluster** (Docker Desktop, minikube, kind or a shared dev cluster you're allowed to change). Everything is enabled; use `--read-only` if you only want to look.
- Every change shows a review; `y`/Enter applies, `n`/Esc cancels. Read the review: if it's unclear or wrong, that's a bug worth reporting.
- Don't paste kubeconfigs, tokens or real object names from production into issues.

## Install

Release candidate from source (needs Rust; `rustup` installs the pinned toolchain):

```sh
cargo install --locked --git https://github.com/c0dewars/kborg --tag v0.5.0-rc.4
kborg --version
```

No cluster? `kborg demo` opens an offline sample cluster.

For changes newer than the RC tag, install `--branch release/next` instead. Match reports to `kborg --version` and the exact Git commit; historical screenshots and acceptance reports are not evidence for a newer binary.

## Connection acceptance

Prepare a context with your normal company/cloud CLI, then start KBorg in that terminal. Confirm the correct context name appears. Exit with Esc and verify no auth helper/API call occurred. Start again, choose No, filter for another existing context, review it and choose Yes. Check that your original kubeconfig current-context is unchanged. Repeat with explicit `--kubeconfig`, merged `KUBECONFIG` and `--context`.

Automated local-only negative-path checks (Unix/WSL PTY; synthetic kubeconfig and a loopback listener, no real credentials):

```sh
python3 scripts/test-connection-startup.py --binary target/release/kborg \
  --output test-results/connection-startup-consent.json
```

## Fifteen-minute tour

1. `kborg --context YOUR_CONTEXT`, then Enter/Y to connect (or `kborg doctor` first if something does not connect): browse with k9s keys (`:pods`, `:deploy`, `/filter`, `Enter`, `d`, `y`, `l`). `?` lists every key.
2. `D`: the dashboard for this cluster. `kborg --contexts a,b` shows several at once; `1`–`9` focus a card, `w` then Enter drills into a warning, `t` tails workloads across clusters.
3. `Ctrl-Z`: only things that need attention. `Ctrl-W`: wide columns. `N` twice: sort by name descending. `:sort AGE desc`.
4. `:lint`: misconfiguration report for the namespace.
5. Switch namespaces with `:ns`, then press `1`–`9` to jump back to recent ones.

## Workflows to exercise (on a test cluster)

| Area | Try |
|---|---|
| Workloads | `s` scale and `r` restart a Deployment; `:rollback`; CronJob `t` trigger and `s` suspend/resume; Ctrl-D delete |
| Apply | `:apply some.yaml` (or `:dir`, then `a`): create, change and re-apply; edit the object with kubectl between review and confirm and check that nothing is written |
| File copy | `:cp-from /etc/hosts`, `:cp-to -r ./dir /tmp/dir` |
| Shell and forwards | Pod `s` shell, `F` port forward, then `:pf` and `b` to benchmark it |
| Debug | `:debug` on a Pod (ephemeral container) |
| Scans | `:scan` on a Pod/workload, `:scan ns` (needs Trivy or Grype on PATH; uses the network) |
| Node shell | `s` on a Node (privileged Pod; only on a cluster you own) |
| YAML edit | `e` on a Deployment, change replicas or an image, Ctrl-S to review |
| Secrets | `:secrets`, then `x` on one key |
| Copilot | Start with `:ask` or `:copilot setup`; a visible next step must appear. On supported macOS ARM64, complete guided setup/login and review the exact transfer. On Windows/Linux/Intel Mac, verify that account-only sign-in is clearly distinguished from unsupported in-app inference and no model request is made. |
| Exec-auth clusters | EKS/GKE/AKS or kubelogin contexts connect without extra steps; `kborg doctor` shows the helper it runs |
| Company policy | `--config` with `"strict_confirmations": true` or `"allow_node_shell": false` |

## Customize like k9s

Put these in the config directory (macOS: `~/Library/Application Support/KBorg`, Linux: `~/.config/kborg`, or `KBORG_CONFIG_DIR`):

- `views.yaml`: per-resource columns and default sort ([format](K9S_COMPATIBILITY.md#custom-views-columns-and-sort)).
- `hotkeys.yaml`: keys that run commands ([format](K9S_COMPATIBILITY.md#custom-hotkeys)).
- `aliases.yaml`: your own command names.
- `ui.yaml`: borders, header and hint rows, table style, dashboard cards and color overrides (`kborg ui init` writes a commented example; `:ui reload` applies edits; [options](themes.md#uiyaml)).
- `keymap.yaml`: remap any built-in key, including Helm releases/history/forms, RBAC subjects, pickers, saved selections, themes, the dashboard tail picker and the port-forward form; `?` in each of those views and `:keymap` show your bindings ([format](K9S_COMPATIBILITY.md#custom-keymap)).

Your existing k9s `views.yaml`, `hotkeys.yaml` and `aliases.yaml` should work as-is. If one doesn't, please report it.

## What we especially want to hear

- Anything slower than k9s or k10s on the same cluster, with the resource count.
- Keys that behave differently from k9s in a way that surprised you.
- Reviews that were hard to read or didn't match what happened.
- How it looks in your terminal (name, theme, font) — screenshots help.

When reporting, include `kborg --version`, OS, terminal, the command you ran and what you expected.
