# Testing KBorg 0.5.0

Thanks for testing. This guide gets you running in a few minutes and lists what to try. Report anything odd as a [bug](https://github.com/c0dewars/kborg/issues/new?template=bug.yml) or [feedback](https://github.com/c0dewars/kborg/issues/new?template=feedback.yml) issue. Report security problems privately through [vulnerability reporting](https://github.com/c0dewars/kborg/security/advisories/new), never in an issue.

## Ground rules

- **Use a disposable or test cluster** (Docker Desktop, minikube, kind or a shared dev cluster you're allowed to change). Start read-only; add `--write` and the specific `--allow-*` flag only for the workflow you're testing.
- Every change shows a review and asks you to type a confirmation. Read the review: if it's unclear or wrong, that's a bug worth reporting.
- Don't paste kubeconfigs, tokens or real object names from production into issues.

## Install

Release candidate from source (needs Rust; `rustup` installs the pinned toolchain):

```sh
cargo install --locked --git https://github.com/c0dewars/kborg --tag v0.5.0-rc.2
kborg --version
```

No cluster? `kborg demo` opens an offline sample cluster.

## Fifteen-minute tour

1. `kborg --context YOUR_CONTEXT --allow-logs`: browse with k9s keys (`:pods`, `:deploy`, `/filter`, `Enter`, `d`, `y`, `l`). `?` lists every key.
2. `D`: the dashboard for this cluster. `kborg --contexts a,b --allow-logs` shows several at once; `1`–`9` focus a card, `w` then Enter drills into a warning, `t` tails workloads across clusters.
3. `Ctrl-Z`: only things that need attention. `Ctrl-W`: wide columns. `N` twice: sort by name descending. `:sort AGE desc`.
4. `:lint`: misconfiguration report for the namespace.
5. Switch namespaces with `:ns`, then press `1`–`9` to jump back to recent ones.

## Workflows to exercise (on a test cluster)

| Flags | Try |
|---|---|
| `--write` | `s` scale and `r` restart a Deployment; `:rollback`; CronJob `t` trigger and `s` suspend/resume; Ctrl-D delete |
| `--write --allow-apply` | `:apply some.yaml` (or `:dir`, then `a`): create, change and re-apply; edit the object with kubectl between review and confirm and check that nothing is written |
| `--write --allow-file-copy` | `:cp-from /etc/hosts`, `:cp-to -r ./dir /tmp/dir` |
| `--write --allow-exec` / `--allow-forward` | Pod `s` shell, `F` port forward, then `:pf` and `b` to benchmark it |
| `--write --allow-debug` | `:debug` on a Pod (ephemeral container) |
| `--scanner ABS_PATH --allow-scan` | `:scan` on a Pod/workload, `:scan ns` (external Trivy or Grype; uses the network) |
| `--write --allow-node-shell` | `s` on a Node (privileged Pod; only on a cluster you own) |
| `--write --allow-yaml-edit` | `e` on a Deployment, change replicas or an image, Ctrl-S to review |
| `--allow-secret-reveal` | `:secrets`, then `x` on one key |

## Customize like k9s

Put these in the config directory (macOS: `~/Library/Application Support/KBorg`, Linux: `~/.config/kborg`, or `KBORG_CONFIG_DIR`):

- `views.yaml`: per-resource columns and default sort ([format](K9S_COMPATIBILITY.md#custom-views-columns-and-sort)).
- `hotkeys.yaml`: keys that run commands ([format](K9S_COMPATIBILITY.md#custom-hotkeys)).
- `aliases.yaml`: your own command names.
- `ui.yaml`: borders, header and hint rows, table style, dashboard cards and color overrides (`kborg ui init` writes a commented example; `:ui reload` applies edits; [options](themes.md#uiyaml)).
- `keymap.yaml`: remap any built-in key; `?` and `:keymap` show your bindings ([format](K9S_COMPATIBILITY.md#custom-keymap)).

Your existing k9s `views.yaml`, `hotkeys.yaml` and `aliases.yaml` should work as-is. If one doesn't, please report it.

## What we especially want to hear

- Anything slower than k9s or k10s on the same cluster, with the resource count.
- Keys that behave differently from k9s in a way that surprised you.
- Reviews that were hard to read or didn't match what happened.
- How it looks in your terminal (name, theme, font) — screenshots help.

When reporting, include `kborg --version`, OS, terminal, the command you ran and what you expected.
