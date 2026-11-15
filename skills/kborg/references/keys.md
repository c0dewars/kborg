# Keys, hotkeys and command names

## Finding a key

`kborg keymap --json` lists every action. Each entry has these fields:

| Field | Holds |
|---|---|
| `action` | The name to use in `keymap.yaml` |
| `view` | Where it works (`table`, `log`, `dashboard`, `secrets`, ...) |
| `keys` | Its keys now |
| `default_keys` | Its keys out of the box |
| `custom` | Whether `keymap.yaml` set it |
| `fixed` | Whether it can never be remapped |
| `description` | What it does |

To answer "how do I X", match `description` against X and answer with the key and the view: "press `l` in a resource table". `kborg keymap` without `--json` prints the same as text, one section per view.

In the TUI:
- `?` shows the keys of the current view, and Tab in it shows every view.
- `:keymap TEXT` (or `:keys`) searches every action.
- Ctrl-P opens a palette that fuzzy-searches every action, command and recent object, so the person can run something without knowing its key.

The mouse works too: a click selects a row, a double click opens it, a click on a column title sorts, and every key hint in the header is a button. Ctrl-Y (copy mode) releases the mouse so the terminal can select and copy text.

## keymap.yaml: move keys between actions

<!-- FILE: keymap.yaml -->
```yaml
keys:
  logs: L                     # resource table: logs on L instead of l
  logs.previous: [p, Alt-p]   # a list binds several keys
  delete: [Ctrl-X, Delete]
  toggle.wide: Alt-w
  explain: F2                 # an action that is unbound out of the box
  log.wrap: Alt-w             # other views use a prefix: log., dashboard., forward., ...
scopes:
  dashboard:
    tail: T
  picker:
    down: J
```

How it works:
- **Action names:** take them from `kborg keymap --json`. Under `keys:` use the full name. Under `scopes: VIEW:` leave out the view prefix (`dashboard.tail` becomes `tail` under `dashboard:`).
- **Replacing:** a binding replaces all of the action's default keys, and `[]` unbinds it.
- **Key names:** `L`, `Shift-N`, `Ctrl-D`, `Alt-x`, `F1`…`F12`, `Space`, `Ctrl-Space`, `Delete`, `Backspace`, `Tab`, `Shift-Tab`, arrows, `Shift-Left`/`Right`/`Up`/`Down`, `PgUp`, `PgDown`, `Home`, `End`. Single letters need no quotes, so `n` and `y` are letters, not booleans.
- **Your key wins:** if a built-in action of the same view used your key, the built-in gives it up (keeping its other keys or becoming unbound), and KBorg says so on the status line and in `:keymap`. So a default added in a later release never breaks the file. The exception is a key that works in every view (`?`, `:`, Ctrl-P): a binding in one view cannot take it.
- **Never remappable** (`"fixed": true`): reviews, approvals, prompts, filter typing, Esc, Enter, Ctrl-C and Ctrl-]. Remapping can never weaken a review.
- **Forms and the YAML editor:** plain keys are typed text there, so only Ctrl, Alt and F keys bind.
- **Shared letters:** a table key with different meanings per kind is a separate action for each kind. `s` is shell on Pods, scale on workloads, suspend on CronJobs and node shell on Nodes, and each can be remapped on its own.
- **Views:** `global`, `table`, `log`, `dashboard`, `tail`, `forward`, `forward.form`, `containers`, `detail`, `overview`, `metrics`, `editor`, `help`, `helm`, `helm.history`, `helm.form`, `subjects`, `picker`, `selections`, `themes`, `secrets`, `review`, `prompt`, `filter`.
- **Limits:** 256 bindings, 9 keys per action, 64 KiB.

Recipes:
- **Vim-style picker:** `scopes: {picker: {down: J, up: K}}`.
- **Free a letter for a hotkey:** unbind the action with `ACTION: []`, then use the key in `hotkeys.yaml`.
- **Go back to the defaults:** delete the line, or the whole file.

## hotkeys.yaml: a key that runs a command

This is the k9s format, so an existing k9s `hotkeys.yaml` usually works as it is.

<!-- FILE: hotkeys.yaml -->
```yaml
hotKeys:
  shift-0:
    shortCut: Shift-0
    description: Pods
    command: pods
  shift-1:
    shortCut: Shift-1
    description: Production deployments
    command: "dp @production"
  f5:
    shortCut: F5
    description: Nodes
    command: nodes
```

How hotkeys work:
- **What runs:** a hotkey types its `command` as if after `:`, so aliases and reviews apply and nothing runs a shell. It fires only in the resource table.
- **Allowed keys:** `Shift-0`…`Shift-9`, `Alt-<letter or digit>`, `F1`…`F12`, and `Ctrl-<letter>` except `a c d e f h i j k m n p q r s u w z`.
- **Clashes:** a hotkey cannot take a key that `keymap.yaml` gave an action, and `kborg config check` reports the clash.
- **Ignored fields:** `keepHistory` and `override` are accepted and ignored.
- **Limits:** 64 hotkeys, 64 KiB.

Ctrl-A (`:aliases`) lists the loaded hotkeys and aliases.

## aliases.yaml: your own command names

<!-- FILE: aliases.yaml -->
```yaml
aliases:
  pp: v1/pods
  apis: "pp 'app in (api,web)'"
  prodpods: "pod default @production"
```

An alias expands into another `:` command and may use another alias. Cycles, and replacing built-in navigation (`ctx`, `ns`, `q` and similar), are rejected. Limits: 256 aliases, 64 KiB.

## Command grammar, for writing hotkeys and aliases

- `RESOURCE [NAMESPACE|all] [/REGEX] [LABEL_SELECTOR] [@CONTEXT]`, for example `dp payments /api @prod`, or `pod 'app in (api,web)'`.
- Any `:` command works too: `pulses`, `xray deploy`, `logs app=web`, `helm`, `secrets`, `metrics`, `ext NAME` (a registered plugin).
- `kborg --help` lists the TUI commands under "TUI navigation".
