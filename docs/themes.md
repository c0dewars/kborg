# Theme authoring

Built-in `flexoki-dark` pairs warm charcoal and cream with a teal focus color. Built-in `rose-pine` pairs deep violet and ivory with iris and foam accents. Both use quiet selected-row and status surfaces. Choose one with `kborg theme use flexoki-dark` or `kborg theme use rose-pine`; preview them in the interactive theme picker. Flexoki Dark is the default for new installations; existing saved selections are preserved.

These are unofficial adaptations of [Flexoki by Steph Ango](https://stephango.com/flexoki) and [Rosé Pine](https://rosepinetheme.com/palette/). Pinned palette snapshots and MIT notices are in `assets/upstream/flexoki/` and `assets/upstream/rose-pine/`, with hashes in `assets/upstream/manifest.json`. Flexoki's red-300 and Rosé Pine's subtle foreground replace their fainter source counterparts for readable functional text; the other mapped colors retain their source values.

Run `kborg theme new my-theme --base catppuccin-mocha` to get a template and its path. The file is installed in the user theme directory; editing it changes the next load. Validate before selecting. To install a shared file, use `theme validate FILE`, `theme preview FILE`, then `theme install FILE` and `theme use ID`.

```json
{"schema_version":1,"id":"my-theme","name":"My Theme","base":"catppuccin-mocha","colors":{"accent":"#2DD4BF","focus":"#2DD4BF"}}
```

Allowed roles: `background`, `text`, `muted`, `accent`, `success`, `warning`, `danger`, `focus`, `selection_background`, `selection_text`, `status_background`, `status_text`. Values are `#RRGGBB`. Missing roles inherit from an embedded fixed-palette base only. Change paired colors together if needed. `export` writes all resolved colors; built-in exports get a `-custom` ID.

Files are limited to 64 KiB. Unknown/duplicate fields, reserved IDs, invalid colors, control characters and low-contrast critical pairs are rejected. No includes, scripts, URLs or executable configuration. Invalid directory entries are skipped; explicit validation explains errors. Custom installation never replaces another file. Remove a custom theme with `theme remove ID`; remove/reset never changes cluster settings.

Config roots: Windows `%APPDATA%\KBorg`, macOS `~/Library/Application Support/KBorg`, Linux `$XDG_CONFIG_HOME/kborg` or `~/.config/kborg`. Themes are under `themes/`, preference under `appearance.json`. An absolute `KBORG_CONFIG_DIR` override supports test isolation.

RGB palettes enforce 4.5:1 for foreground text pairs and 3:1 for focus. Built-ins may replace low-contrast semantic colors with black/white; source base colors remain documented in code. Terminal mode uses host colors, whose contrast cannot be guaranteed. The renderer detects `TERM`/`COLORTERM` once per process and uses truecolor, the nearest fixed xterm 256-color entry, or basic ANSI colors. Quantization and customized host ANSI palettes can change contrast; `--color=never` is available when needed. Appearance preferences use synced sibling files and same-directory replacement without deleting the previous file first. Filesystem/OS crash durability and local ACL limitations are documented in STORAGE.md.

First interactive use opens the picker with Flexoki Dark selected. Enter chooses and Esc keeps that default. Missing/invalid selection falls back to Flexoki Dark. Appearance settings also store animation and onboarding state; startup off/on preserves theme selection.


Use theme clone-current ID to copy current resolved colors. Use theme replace FILE to replace a custom theme after validation while retaining an original .json.bak; a second replacement refuses until that backup is deliberately archived.

The UI uses thin rounded frames in UTF-8 locales, with ASCII frames otherwise. Resource data retains conservative single-cell ASCII rendering. A quiet table header, muted shortcut descriptions, compact form guidance and status-only semantic colors keep the selected object prominent. Saved PNG captures join box strokes across cell boundaries so their lines stay continuous. The visual direction follows the compact hierarchy of [k10s](https://github.com/mpiorowski/k10s) and the restrained palettes in [Omarchy themes](https://omarchy.org/themes/); navigation remains command driven without a persistent sidebar.
