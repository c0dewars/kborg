# KBorg logo

The mark reads **K&lt;B**: an orange K whose arms form a `<` chevron, which doubles as a terminal prompt, pointing into a green B. Orange and green come from gradients, top to bottom:

| Role | Top | Bottom |
|---|---|---|
| K / chevron (orange) | `#FFA41B` | `#FF5A1F` |
| B (green) | `#3BE08A` | `#0FA968` |
| Icon tile | `#0B1220` | |

| File | Use |
|---|---|
| `kborg-mark.svg` | Scalable mark on a transparent background |
| `kborg-icon.png` | 1024 px app icon on a dark rounded tile |
| `kborg-logo.png` | Wordmark lockup for light backgrounds (packaged with releases) |
| `kborg-logo-dark.png` | Wordmark lockup for dark backgrounds |

All four are generated from one geometry by [`scripts/make-logo.py`](../scripts/make-logo.py) (Pillow). The wordmark uses macOS's Avenir Next Heavy, rasterized into the PNGs; pass `--font` to use another bold face.

The earlier teal connected-squares emblem (2026-10-10) is retained as `k8borg-logo-concept.png` for history.
