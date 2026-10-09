# Theme authoring

Run `k8borg theme new my-theme --base catppuccin-mocha` to get a template and its path. The file is installed in the user theme directory; editing it changes the next load. Validate before selecting. To install a shared file, use `theme validate FILE`, `theme preview FILE`, then `theme install FILE` and `theme use ID`.

```json
{"schema_version":1,"id":"my-theme","name":"My Theme","base":"catppuccin-mocha","colors":{"accent":"#2DD4BF","focus":"#2DD4BF"}}
```

Allowed roles: `background`, `text`, `muted`, `accent`, `success`, `warning`, `danger`, `focus`, `selection_background`, `selection_text`, `status_background`, `status_text`. Values are `#RRGGBB`. Missing roles inherit from an embedded fixed-palette base only. Change paired colors together if needed. `export` writes all resolved colors; built-in exports get a `-custom` ID.

Files are limited to 64 KiB. Unknown/duplicate fields, reserved IDs, invalid colors, control characters and low-contrast critical pairs are rejected. No includes, scripts, URLs or executable configuration. Invalid directory entries are skipped; explicit validation explains errors. Custom installation never replaces another file. Remove a custom theme with `theme remove ID`; remove/reset never changes cluster settings.

Config roots: Windows `%APPDATA%\K8Borg`, macOS `~/Library/Application Support/K8Borg`, Linux `$XDG_CONFIG_HOME/k8borg` or `~/.config/k8borg`. Themes are under `themes/`, preference under `appearance.json`. An absolute `K8BORG_CONFIG_DIR` override supports test isolation.

RGB palettes enforce 4.5:1 for foreground text pairs and 3:1 for focus. Built-ins may replace low-contrast semantic colors with black/white; source base colors remain documented in code. Terminal mode uses host colors, whose contrast cannot be guaranteed. Current renderer uses RGB for color mode: terminals without truecolor should use `--color=never` or Terminal mode; 256-color quantization is pending. Theme preference replacement is fallback-safe but not an atomic replace on Windows.

First interactive use opens the picker with Catppuccin Mocha selected. Enter chooses and Esc keeps that default. Missing/invalid selection falls back to Mocha. Appearance settings also store animation and onboarding state; startup off/on preserves theme selection.

