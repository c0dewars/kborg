# Themes and looks

Three layers decide how KBorg looks, from bottom to top:

1. **The theme:** 22 built-in palettes (`okabe-ito-dark` and `okabe-ito-light` are color-blind safe, `k8borg-high-contrast` meets 7:1), or the person's own `themes/ID.json`.
2. **`ui.yaml`:** layout and style, plus per-role color overrides on top of the theme.
3. **Per run or per cluster:** `--theme ID` for one run, or `theme:` in `contexts/NAME.yaml` while that context is selected.

## Choosing a theme

| Where | How |
|---|---|
| In the TUI | `:themes` previews themes live: the arrows preview, Enter saves, `N` creates a custom theme from the previewed colors, Esc restores the previous one |
| CLI | `kborg theme list` (the current one is marked `[selected]`), `kborg theme use ID`, `kborg theme reset` |
| Pinned | `theme: ID` in `ui.yaml` wins over the picker's choice at every start |

The built-in IDs:

| Family | IDs |
|---|---|
| KBorg | `k8borg-dark` `k8borg-light` `k8borg-high-contrast` |
| Catppuccin | `catppuccin-latte` `catppuccin-frappe` `catppuccin-macchiato` `catppuccin-mocha` |
| Others | `dracula` `nord` `gruvbox-dark` `gruvbox-light` `flexoki-dark` `rose-pine` `solarized-dark` `solarized-light` |
| Tokyo Night | `tokyo-night-night` `tokyo-night-storm` `tokyo-night-moon` `tokyo-night-day` |
| Host palette | `terminal` (the terminal's own colors) |

`kborg theme list` is the source of truth for the installed version.

## Creating a theme

1. **Start from a template:** `kborg theme new my-theme --base catppuccin-mocha` writes the template and prints its path, `themes/my-theme.json` in the config folder. `kborg theme clone-current my-theme` instead copies every color of the theme in use.
2. **Edit the colors.** A theme names a built-in `base` and overrides some of its 12 color roles. Roles left out come from the base.
3. **Check it:** run `kborg theme validate FILE`, and `kborg theme preview FILE` to see it in the terminal.
4. **Select it:** run `kborg theme use my-theme`. A running KBorg picks up later edits to the active theme file within a second.

<!-- FILE: themes/my-theme.json -->
```json
{
  "schema_version": 1,
  "id": "my-theme",
  "name": "My Theme",
  "base": "catppuccin-mocha",
  "colors": {
    "accent": "#2DD4BF",
    "focus": "#2DD4BF"
  }
}
```

The roles, all written `#RRGGBB`:

| Role | Used for |
|---|---|
| `background`, `text` | Main surface and text |
| `muted` | Secondary text, key-hint labels, axes |
| `accent` | Keys, titles, CPU series |
| `focus` | Focused frame and cursor |
| `success`, `warning`, `danger` | Healthy, attention and failing states. `danger` is also used for destructive keys and the PROD badge |
| `selection_background`, `selection_text` | Selected row |
| `status_background`, `status_text` | Status bar |

Rules KBorg enforces, so tell the person before they hit them:
- **Contrast:** text pairs need 4.5:1 and focus needs 3:1, and a low-contrast critical pair is rejected. Change paired colors together, for example `selection_background` with `selection_text`.
- **IDs:** a custom theme cannot reuse a built-in ID.
- **The file:** strict JSON with no unknown or duplicate fields, at most 64 KiB, and no includes, URLs or scripts.

Sharing and managing themes:
- **Install a shared file:** `kborg theme validate FILE`, `kborg theme preview FILE`, `kborg theme install FILE`, then `kborg theme use ID`. Installing never replaces another theme.
- **Update an installed theme:** `kborg theme replace FILE`. It keeps the original as `.json.bak` and refuses a second replace until that backup is archived.
- **Export:** `kborg theme export ID --output FILE` writes every resolved color, and is a good way to start from a built-in theme. A built-in theme's export gets a `-custom` ID.
- **Remove:** `kborg theme remove ID`.
- **From a k9s skin:** `kborg theme import-k9s SKIN.yaml` saves the skin as a new theme, `k9s-` plus the file name (or `--id ID`), and never replaces one. It prints each role with the k9s color it came from. Colors the skin leaves to the terminal (`default`) come from KBorg Dark, and a color too faint to read keeps its hue but is lightened or darkened to 4.5:1; the output says so. Then `kborg theme use ID`, or pick it in `:themes`.

Turning a terminal or editor palette (iTerm, VS Code, a colorscheme) into a KBorg theme:
1. Map its background, foreground, comment or gray, blue or cyan, green, yellow and red to `background`, `text`, `muted`, `accent` and `focus`, `success`, `warning` and `danger`.
2. Pick the built-in base that looks closest, so the roles you leave out still fit.
3. Run `kborg theme validate FILE`. It names any pair whose contrast is too low.

## ui.yaml: layout and style

`kborg ui init` writes a commented file with every key and never replaces an existing one. `kborg ui validate` checks it, and `:ui reload` re-reads it on demand.

<!-- FILE: ui.yaml -->
```yaml
version: 1
theme: rose-pine           # pin a theme at every start
borders: rounded           # auto, rounded, square, ascii, none
panel_title: bracketed     # marker, plain, bracketed
header:
  style: block             # block (k9s-style) or bar
  logo: false
hints:
  show: true
table:
  zebra: true
  density: normal          # normal or relaxed
  column_separator: "|"
  age: long                # 14d or 14d3h
colors:
  accent: "#88C0D0"        # any theme role, on top of the theme
dashboard:
  columns: 0
charts:
  style: braille           # auto, braille, blocks, ascii
timestamps: local          # log timestamps: utc or local
```

What else it controls:
- **Header:** `header.usage` (cluster CPU and memory), `header.namespaces` (recent namespaces), `header.show`, `header.identity`, `header.summary`, and `header.compact` (the most table rows).
- **Hints:** `hints.key_role` and `hints.text_role`.
- **Table:** `table.selection_marker`, `table.mark_marker`, `table.sort_ascending` and `table.sort_descending`.
- **Charts:** `charts.show_axes`, `charts.cpu_role` and `charts.memory_role`.
- **Text:** `text` (`auto`, `unicode` or `ascii`, for terminals without good Unicode).
- **Startup and reload:** `splash` (the startup animation) and `reload` (live reload).

Other rules:
- **File:** at most 16 KiB.
- **Terminal size:** the `block` header needs a 100x24 terminal; smaller terminals use `bar`.

## Startup animation and terminal colors

- **Animation:** `kborg startup off` (or `on`, or `preview`) sets the preference. `--no-splash` skips it for one run, and `splash: false` in `ui.yaml` skips it always.
- **Colors:** KBorg uses truecolor when the terminal reports it (`COLORTERM=truecolor`) and otherwise the nearest 256 or basic colors, so a theme can look slightly different in an old terminal. `--color=never` turns colors off, and `theme: terminal` uses the terminal's own palette.
- **Copy mode:** Ctrl-Y, or `:mouse`, releases the mouse so the terminal can select text.
- **Accessibility:** `accessible: true` in `ui.yaml` (or `KBORG_ACCESSIBLE=1`) is for screen readers and reduced motion: no animation, box drawing, logo, zebra rows, braille charts or sparklines, and the terminal cursor sits on the selected row so VoiceOver or Orca read it. For color vision deficiencies suggest `kborg theme use okabe-ito-dark` (or `okabe-ito-light`); for low vision `k8borg-high-contrast`. Status is always written as text too, never by color alone. For linear text, suggest the CLI (`kborg list pods`, `kborg describe pod NAME`).
