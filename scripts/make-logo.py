#!/usr/bin/env python3
"""Generate the KBorg brand assets from one geometry.

The mark reads "K<B": an orange K whose arms form a "<" chevron (also a
terminal prompt) pointing into a green B. Output:

  assets/kborg-mark.svg        scalable mark on a transparent background
  assets/kborg-icon.png        1024px app icon on a dark rounded tile
  assets/kborg-logo.png        wordmark lockup for light backgrounds
  assets/kborg-logo-dark.png   wordmark lockup for dark backgrounds

Requires Pillow. The wordmark uses macOS's Avenir Next Heavy; pass --font to
use another bold face.
"""
import argparse
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ORANGE = ("#FFA41B", "#FF5A1F")  # top -> bottom
GREEN = ("#3BE08A", "#0FA968")
TILE = "#0B1220"
INK_LIGHT_BG = "#0F172A"
INK_DARK_BG = "#F8FAFC"

# Mark geometry on a 1000 x 1000 canvas.
STROKE = 100
K_STEM = (90, 210, 190, 790)
# The chevron floats just off the stem so it reads as both K and "<".
VERTEX = (298, 500)
TIPS = ((480, 240), (480, 760))
B_STEM = (580, 210, 680, 790)
B_UPPER = (580, 210, 890, 522, 156)
B_LOWER = (580, 478, 930, 790, 156)
B_UPPER_HOLE = (680, 310, 790, 422, 52)
B_LOWER_HOLE = (680, 578, 830, 690, 54)


def gradient(size, colors):
    top, bottom = (Image.new("RGB", (1, 1), c).getpixel((0, 0)) for c in colors)
    column = Image.new("RGB", (1, size[1]))
    for y in range(size[1]):
        t = y / max(1, size[1] - 1)
        column.putpixel((0, y), tuple(round(a + (b - a) * t) for a, b in zip(top, bottom)))
    return column.resize(size)


def k_mask(scale):
    mask = Image.new("L", (1000 * scale, 1000 * scale), 0)
    draw = ImageDraw.Draw(mask)
    s = lambda v: v * scale  # noqa: E731
    draw.rounded_rectangle([s(v) for v in K_STEM], radius=s(STROKE // 2), fill=255)
    for tip in TIPS:
        draw.line([s(VERTEX[0]), s(VERTEX[1]), s(tip[0]), s(tip[1])], fill=255, width=s(STROKE))
        for x, y in (VERTEX, tip):
            r = s(STROKE // 2)
            draw.ellipse([s(x) - r, s(y) - r, s(x) + r, s(y) + r], fill=255)
    return mask


def b_mask(scale):
    mask = Image.new("L", (1000 * scale, 1000 * scale), 0)
    draw = ImageDraw.Draw(mask)
    s = lambda v: v * scale  # noqa: E731
    for x0, y0, x1, y1, r in (B_UPPER, B_LOWER):
        draw.rounded_rectangle([s(x0), s(y0), s(x1), s(y1)], radius=s(r), fill=255)
    draw.rectangle([s(v) for v in B_STEM], fill=255)
    for x0, y0, x1, y1, r in (B_UPPER_HOLE, B_LOWER_HOLE):
        draw.rounded_rectangle([s(x0), s(y0), s(x1), s(y1)], radius=s(r), fill=0)
    return mask


def mark(size, tile=False):
    scale = 4
    canvas = Image.new("RGBA", (1000 * scale, 1000 * scale), (0, 0, 0, 0))
    if tile:
        ImageDraw.Draw(canvas).rounded_rectangle(
            [0, 0, 1000 * scale - 1, 1000 * scale - 1], radius=224 * scale, fill=TILE
        )
    for mask, colors in ((k_mask(scale), ORANGE), (b_mask(scale), GREEN)):
        canvas.paste(gradient(canvas.size, colors), (0, 0), mask)
    return canvas.resize((size, size), Image.LANCZOS)


def svg():
    def rr(x0, y0, x1, y1, r):
        return f'M{x0 + r},{y0} H{x1 - r} A{r},{r} 0 0 1 {x1},{y0 + r} V{y1 - r} A{r},{r} 0 0 1 {x1 - r},{y1} H{x0 + r} A{r},{r} 0 0 1 {x0},{y1 - r} V{y0 + r} A{r},{r} 0 0 1 {x0 + r},{y0} Z'

    x0, y0, x1, y1 = K_STEM
    arms = "".join(
        f'<line x1="{VERTEX[0]}" y1="{VERTEX[1]}" x2="{x}" y2="{y}" stroke="url(#k)" stroke-width="{STROKE}" stroke-linecap="round"/>'
        for x, y in TIPS
    )
    b_outer = rr(*B_UPPER) + rr(*B_LOWER) + "M{0},{1} H{2} V{3} H{0} Z".format(*B_STEM)
    b_holes = rr(*B_UPPER_HOLE) + rr(*B_LOWER_HOLE)
    return f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1000 1000" role="img" aria-label="KBorg">
  <defs>
    <linearGradient id="k" x1="0" y1="0" x2="0" y2="1000" gradientUnits="userSpaceOnUse"><stop offset="0" stop-color="{ORANGE[0]}"/><stop offset="1" stop-color="{ORANGE[1]}"/></linearGradient>
    <linearGradient id="b" x1="0" y1="0" x2="0" y2="1000" gradientUnits="userSpaceOnUse"><stop offset="0" stop-color="{GREEN[0]}"/><stop offset="1" stop-color="{GREEN[1]}"/></linearGradient>
    <mask id="holes"><rect width="1000" height="1000" fill="white"/><path d="{b_holes}" fill="black"/></mask>
  </defs>
  <rect x="{x0}" y="{y0}" width="{x1 - x0}" height="{y1 - y0}" rx="{STROKE // 2}" fill="url(#k)"/>
  {arms}
  <path d="{b_outer}" fill="url(#b)" mask="url(#holes)"/>
</svg>
"""


def lockup(ink, font_path, height=480):
    icon = mark(height)
    font = ImageFont.truetype(font_path, int(height * 0.62), index=8 if font_path.endswith(".ttc") else 0)
    parts = (("K", ORANGE), ("B", GREEN), ("org", ink))
    probe = ImageDraw.Draw(Image.new("RGBA", (1, 1)))
    gap = int(height * 0.14)
    widths = [probe.textlength(text, font=font) for text, _ in parts]
    width = height + gap + int(sum(widths)) + int(height * 0.08)
    image = Image.new("RGBA", (width, height), (0, 0, 0, 0))
    image.paste(icon, (0, 0), icon)
    ascent, descent = font.getmetrics()
    baseline = (height + ascent - descent) // 2 + int(height * 0.02)
    x = height + gap
    for (text, color), advance in zip(parts, widths):
        layer = Image.new("L", image.size, 0)
        ImageDraw.Draw(layer).text((x, baseline), text, font=font, fill=255, anchor="ls")
        fill = gradient(image.size, color) if isinstance(color, tuple) else Image.new("RGB", image.size, color)
        image.paste(fill, (0, 0), layer)
        x += advance
    return image


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--assets", type=Path, default=Path(__file__).resolve().parent.parent / "assets")
    parser.add_argument("--font", default="/System/Library/Fonts/Avenir Next.ttc")
    args = parser.parse_args()
    args.assets.mkdir(parents=True, exist_ok=True)
    (args.assets / "kborg-mark.svg").write_text(svg())
    mark(1024, tile=True).save(args.assets / "kborg-icon.png")
    lockup(INK_LIGHT_BG, args.font).save(args.assets / "kborg-logo.png")
    lockup(INK_DARK_BG, args.font).save(args.assets / "kborg-logo-dark.png")
    print("wrote", ", ".join(p.name for p in sorted(args.assets.glob("kborg-*"))))


if __name__ == "__main__":
    main()
