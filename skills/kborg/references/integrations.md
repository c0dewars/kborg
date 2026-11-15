# Integrations: k9s, plugins, editors, scanners, Copilot, shells

## Coming from k9s

Most k9s habits work unchanged: the `:` commands and aliases, `/` filters, `d` `y` `l` `s` `e`, Ctrl-D, Shift sort keys, `:pf`, `:xray`, `:pulses`, `:popeye` (as `:lint`). The k9s config folder is `~/.config/k9s`, or `~/Library/Application Support/k9s` on macOS.

| k9s file | In KBorg |
|---|---|
| `hotkeys.yaml` | Copy to the KBorg config folder unchanged. Shortcuts outside the supported set are reported by `kborg config check` |
| `views.yaml` | Copy unchanged |
| `aliases.yaml` | Copy, keeping the `aliases:` map (see keys.md) |
| `plugins.yaml` | `kborg extensions import-k9s PLUGINS.yaml` reports each plugin and prints the `register-k9s` command to run for it (see below) |
| skins | Not imported. Make a theme instead: map the skin's background, foreground and highlight colors to theme roles (themes-and-looks.md) |
| `clusters/CONTEXT/config.yaml` | Write `contexts/NAME.yaml` (clusters-and-safety.md) |
| `config.yaml` | Not imported. Its useful settings live in `ui.yaml` and `contexts/NAME.yaml` |

The differences people notice:
- **Reviews:** every change opens a review and is confirmed with `y` or Enter.
- **Secrets:** they are a separate `:secrets` view with masked values.
- **Shells:** `s` and `a` are a full terminal, like `kubectl exec -it`; Ctrl-] returns to KBorg.

## Plugins and extensions

KBorg runs a plugin as a registered extension:
- **What runs:** an absolute executable, pinned by its SHA-256, that receives the selected object as redacted JSON on stdin.
- **Review:** each run is reviewed first, with the exact argument list shown and approved with `RUN TOOL` (`RUN DANGEROUS NAME` for a plugin marked dangerous).
- **Output:** captured and shown, never applied.
- **Time limit:** 15 seconds.

Managing them:
- **List:** `kborg extensions list`.
- **Import from k9s:** `kborg extensions import-k9s PLUGINS.yaml`, then `kborg extensions register-k9s PLUGINS.yaml NAME --tool /ABS/PATH`.
- **Register any tool:** `kborg extensions register NAME --tool /ABS/PATH`.
- **Remove:** `kborg extensions remove NAME`.

Running them:
- **In the TUI:** `:ext NAME`, the plugin's `shortCut`, or `:actions`.
- **On the command line:** `kborg extensions run NAME RESOURCE OBJECT`.

k9s variables (`$NAMESPACE`, `$NAME`, `$CONTEXT`, `$POD`, `$CONTAINER`, `$COL-<COLUMN>` and so on) expand inside one argument. There is no shell and no word splitting, and environment variables are never read.

## Editors

- **In KBorg:** `e` opens the reviewed YAML editor (fields, labels, annotations, ConfigMap data), and Ctrl-S prepares the diff and review.
- **External editor:** Ctrl-E in that editor hands the draft to `$VISUAL` or `$EDITOR` (k9s-style `e`), and the edited text comes back through the same review. Set `EDITOR` in the shell, for example `export EDITOR=nvim` or `EDITOR="code --wait"`.
- **Per-cluster settings:** `:ctx-config` uses the same editor.

## Image scanning

`:scan` (selected object) or `:scan ns` (namespace), and `kborg scan RESOURCE NAME | ns`, run Trivy or Grype if one is installed. KBorg finds it on PATH, or takes `--scanner /ABS/PATH`. It shows a short review first. KBorg contains no scanner itself.

## Copilot inside KBorg

KBorg has its own optional GitHub Copilot integration, separate from whatever assistant is reading this:
- **Set up:** `kborg copilot setup` (about a minute). It finds the Copilot CLI and the `gh` login.
- **Check:** `kborg copilot status`, or `kborg doctor`.
- **Use:**

  | Key or command | Does |
  |---|---|
  | Ctrl-G or `:ask QUESTION` | Asks about the screen in front |
  | `f` on an answer | Asks a follow-up |
  | `:copilot` | Asks about the selected object |
  | `:explain` | Asks about the error on the status line; unbound out of the box, so bind `explain` in `keymap.yaml` |

- **Approval:** each request shows the exact text to be sent and waits for approval. Answers are advice and are never run.
- **Platforms:** answers inside KBorg work on Apple Silicon Macs. On other platforms setup only completes the GitHub sign-in.
- **Enterprise:** `TENANT.ghe.com` hosts are supported.

Never suggest copying cluster credentials into any AI tool to work around a limit.

## Shell completions

`kborg completions zsh` (or `bash`, `fish`, `powershell`) prints a completion script. For zsh, for example, add `source <(kborg completions zsh)` to `~/.zshrc`.

## AI assistants

`kborg skill install` installs this skill for Claude Code, GitHub Copilot, OpenAI Codex, Gemini CLI and `~/.agents/skills`. `kborg skill print` prints it as one file for chat apps. See SKILL.md.
