---
name: kborg
description: Use KBorg, the k9s-style Kubernetes terminal UI and CLI, to inspect and debug Kubernetes clusters for a person (crashing or pending Pods, failed rollouts, unreachable Services, Node problems, logs, events, metrics, Helm) with read-only commands, and to help them use and customize KBorg. Covers finding and remapping keys (keymap.yaml), hotkeys and aliases, creating or changing themes and the layout (themes/*.json, ui.yaml), table columns (views.yaml), per-cluster settings and safety (contexts/NAME.yaml, read-only, PROD badges), multi-cluster dashboards and saved selections, k9s migration, plugins, Copilot setup, install and troubleshooting, and explaining any KBorg screen or change review. Use whenever the person mentions kborg, its keys, themes or config files.
license: Apache-2.0
compatibility: Needs the kborg command (KBorg 0.7 or later) on PATH, or a person who runs it and pastes the output.
---

# KBorg

KBorg is a terminal UI for Kubernetes that works like k9s (`:pods`, `/filter`, `d` describe, `l` logs, `s` shell) plus a CLI (`kborg list pods --json`). Everything is on by default, and every change shows a review that the person approves with `y` or Enter. It also has a dashboard of up to nine clusters, a reviewed YAML editor, Helm, masked Secrets and 20 themes, and all of it can be customized with small data files.

You help a person who runs KBorg in two ways:
- **Investigate their cluster.** Find out what is wrong by running KBorg's read-only commands yourself, or by asking them to run the commands.
- **Help them use KBorg.** Tell them what to press, write their config files correctly, and explain what they see.

Changes to the cluster are always theirs to make, through KBorg's reviews.

## How to work

KBorg can describe itself, and that description beats anything you remember, because keys and formats change between versions.

- **You can run commands** (a coding agent or CLI assistant): run them yourself and read the output.
- **You are in a chat without a terminal**: ask the person to run the command and paste the output. Give them the exact command line.

Either way, never guess a key or a field name. These four commands answer most questions:

| Command | Tells you |
|---|---|
| `kborg --version` | The version the person has |
| `kborg keymap --json` | Every action: its view, the keys it has now (their `keymap.yaml` applied), its default keys, what it does, and whether it can be remapped |
| `kborg config check` | Whether every file in the config folder is valid, with the file and line of each error; also prints where the folder is |
| `kborg --help` | Every command and option |

In the TUI, `?` shows the keys of the current view and `:keymap TEXT` searches all of them.

## Ground rules

1. **Always ask before any change to a live cluster, and let the person make it.** A change is anything that writes to a cluster: scale, restart, delete, edit, apply, rollback, image or resource changes, exec or shells, port-forwards, file copies, node cordon or drain, debug containers, Helm actions, Secret changes. Before suggesting one:
   1. **Describe it:** say exactly what it changes, on which context, namespace and object, what could go wrong, and how to undo it.
   2. **Ask:** get a clear yes from the person for that one change. A yes to an earlier change, or a general "fix it", is not a yes to this one.
   3. **Hand it over:** give them the command or TUI keys to run themselves. KBorg then shows them its review, which they read and approve with `y` or Enter: a second, separate confirmation.

   Never run a change yourself, even when asked to. Never type into a KBorg prompt or review, never pipe input into one, and never suggest a way to skip a review. If they insist, explain that KBorg leaves approvals to the person at their own terminal.
2. **Read-only, always.** Every KBorg command you run yourself carries `--read-only` and an explicit `--context`. To try something without a cluster, use `kborg demo`. Recommend that the person set `KBORG_READ_ONLY=1` in the environment of the shell you work in: then KBorg refuses every change from that shell, whatever command you run. Say which context a command will touch, and never read a cluster the person did not name.
3. **Credentials and Secret values stay with the person.** Never ask for a kubeconfig, a token or a Secret value, and never put one in a file or a message. In `:secrets` KBorg reveals or changes values locally. If you see something that looks like a credential anyway (in a log line, for example), do not repeat it. Tell the person where it is and suggest rotating it. What KBorg masks and what it cannot is in references/investigate-a-cluster.md.
4. **Edit config files with care.**
   - **Merge, don't replace:** merge into an existing file and keep the person's comments. If you cannot see the file, ask them to paste it first.
   - **Check:** run `kborg config check` (or ask them to) after every change.
   - **What happens next:** a running KBorg applies a saved file within a second. An invalid file is refused, and KBorg keeps the last good settings and shows a notice.
5. **Leave kubeconfig alone.** KBorg reads it as kubectl does and never rewrites it. Per-cluster preferences go in `contexts/NAME.yaml`.
6. **Stay inside what KBorg offers.** Config files only move keys, name commands and change looks; they never add behavior. If something is not possible, say so, and suggest the closest supported way.

## Where configuration lives

The config folder is:
- **macOS:** `~/Library/Application Support/KBorg`
- **Linux:** `~/.config/kborg` (or `$XDG_CONFIG_HOME/kborg`)
- **Windows:** `%APPDATA%\KBorg`

`KBORG_CONFIG_DIR` (an absolute path) overrides it, and `kborg config check` prints the folder it uses.

| File | Controls | Start from |
|---|---|---|
| `keymap.yaml` | The key of any action in any view | `kborg keymap` |
| `hotkeys.yaml` | Shift-0..9, Alt, F1..F12 or free Ctrl keys that run a `:` command | |
| `aliases.yaml` | Your own command names | |
| `views.yaml` | Columns and sort per resource | `kborg views init` |
| `ui.yaml` | Header, borders, table style, colors, dashboard layout, charts | `kborg ui init` |
| `themes/ID.json` | A custom theme | `kborg theme new ID --base BUILTIN` |
| `contexts/NAME.yaml` | Per-cluster namespace, first view, theme, badge, read-only, typed confirmations | `:ctx-config` in the TUI |

## What people ask for

| Request | Read |
|---|---|
| Debug or inspect a cluster: why a Pod crashes or is Pending, a rollout is stuck, a Service does not answer, a Node is unhealthy | [references/investigate-a-cluster.md](references/investigate-a-cluster.md) (read it before running any command against a cluster) |
| Which key does X, remap a key, add a shortcut or a command name | [references/keys.md](references/keys.md) |
| Create or change a theme, colors, borders, header, layout, the startup animation | [references/themes-and-looks.md](references/themes-and-looks.md) |
| Table columns, sorting, filters, saved views | [references/tables-and-views.md](references/tables-and-views.md) |
| A PROD badge, read-only or careful clusters, team restrictions, kubeconfig and connecting | [references/clusters-and-safety.md](references/clusters-and-safety.md) |
| Multi-cluster dashboard, saved dashboards | [references/dashboards.md](references/dashboards.md) |
| What a screen, status word or review means; logs, shells, edits, Secrets, Helm | [references/using-the-tui.md](references/using-the-tui.md) |
| Coming from k9s, plugins, an external editor, scanners, Copilot, shell completions | [references/integrations.md](references/integrations.md) |
| Installing, upgrading, first run, something not working | [references/install-and-troubleshoot.md](references/install-and-troubleshoot.md) |

Each reference has working examples, and KBorg's own test suite parses every one of them.

## Scripting

The read commands take `--json`:
- `kborg list RESOURCE`, `kborg get RESOURCE NAME`, `kborg describe RESOURCE NAME`, `kborg watch RESOURCE`
- `kborg logs POD`, `kborg metrics pods`
- `kborg overview CTX...`, `kborg contexts`, `kborg resources`
- `kborg lint`, `kborg helm list`
- `kborg keymap`, `kborg config check`

Pass `--context NAME` and `-n NAMESPACE` to be explicit. Output is redacted: Secrets are never listed and credential-like fields are masked. Commands that change something (`kborg scale`, `kborg apply` and so on) show the same review in the terminal and wait for the person.

## Getting this skill into an assistant

- **`kborg skill install`** writes this skill, matching the installed version, into every skill folder it finds: Claude Code `~/.claude/skills`, GitHub Copilot `~/.copilot/skills`, OpenAI Codex `~/.codex/skills`, Gemini CLI `~/.gemini/skills`, and the shared `~/.agents/skills`.
- **One assistant or folder:** `kborg skill install claude` (or `copilot`, `codex`, `gemini`, `agents`), or `kborg skill install DIR`.
- **Without KBorg on this machine yet:** Claude Code takes `/plugin install kborg --marketplace c0dewars/kborg`. The GitHub CLI (2.90 or later) installs it for about 50 assistants: `gh skill install c0dewars/kborg kborg --agent github-copilot --scope user`, with `--agent claude-code`, `codex`, `cursor`, `gemini-cli` and so on for the others.
- **When asked to install this skill:** run the command yourself if you can, then tell the person to restart or reload you so it loads.
- **Chat apps** (ChatGPT, Gemini, Claude or Copilot in a browser): `kborg skill print > kborg-skill.md` writes everything as one Markdown file. Add it as a file to a project, a custom GPT, a Gem or a Claude project; it is too long for a short custom-instructions box. Apps that accept skill uploads take the installed `kborg` folder as a zip.
- **After upgrading KBorg:** run the install again.
