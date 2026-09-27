"""Render the Discord status icons in assets/discord/ (PNG; Discord can't show SVG).

    python scripts/gen_icons.py

Drawn at 4x and downsampled for smooth edges. Colours match src/discord.rs and
the dashboard palette.
"""

import math
from pathlib import Path

from PIL import Image, ImageDraw

OUT = Path(__file__).resolve().parent.parent / "assets" / "discord"
SIZE, SCALE = 128, 4
S = SIZE * SCALE
WHITE = (255, 255, 255, 255)

COLORS = {
    "ok": (48, 164, 108),
    "down": (229, 72, 77),
    "behind": (245, 165, 36),
    "indexer-behind": (245, 165, 36),
    "syncing": (62, 139, 255),
    "unreachable": (139, 141, 152),
    "received": (255, 90, 31),
}


def p(x, y):
    """Glyph coordinates in a 0..24 box (Lucide-style), centred in the badge."""
    k = S * 0.5 / 24
    off = S * 0.25
    return (off + x * k, off + y * k)


def stroke(d, points, w=2.6):
    pts = [p(*pt) for pt in points]
    width = int(w * S * 0.5 / 24)
    d.line(pts, fill=WHITE, width=width, joint="curve")
    r = width / 2
    for x, y in (pts[0], pts[-1]):
        d.ellipse((x - r, y - r, x + r, y + r), fill=WHITE)


def arc(d, cx, cy, radius, start, end, w=2.6, steps=48):
    pts = [
        (cx + radius * math.cos(math.radians(a)), cy + radius * math.sin(math.radians(a)))
        for a in (start + (end - start) * i / steps for i in range(steps + 1))
    ]
    stroke(d, pts, w)


def dot(d, x, y, r=1.6):
    cx, cy = p(x, y)
    rr = r * S * 0.5 / 24
    d.ellipse((cx - rr, cy - rr, cx + rr, cy + rr), fill=WHITE)


GLYPHS = {
    "ok": lambda d: stroke(d, [(5, 12.5), (10, 17.5), (19, 7)]),
    "down": lambda d: (stroke(d, [(6.5, 6.5), (17.5, 17.5)]), stroke(d, [(17.5, 6.5), (6.5, 17.5)])),
    "behind": lambda d: (stroke(d, [(12, 4.5), (12, 18)]), stroke(d, [(6.5, 12.5), (12, 18), (17.5, 12.5)])),
    # Stacked layers (the index) with a gap.
    "indexer-behind": lambda d: (
        stroke(d, [(12, 4), (20, 8), (12, 12), (4, 8), (12, 4)]),
        stroke(d, [(4, 12.5), (12, 16.5), (20, 12.5)]),
        stroke(d, [(4, 17), (12, 21), (20, 17)]),
    ),
    "syncing": lambda d: (
        arc(d, 12, 12, 7.5, 200, 340),
        stroke(d, [(19.2, 5.5), (19.2, 9.6), (15.2, 9.6)]),
        arc(d, 12, 12, 7.5, 20, 160),
        stroke(d, [(4.8, 18.5), (4.8, 14.4), (8.8, 14.4)]),
    ),
    "unreachable": lambda d: (stroke(d, [(12, 6), (12, 13.5)]), dot(d, 12, 18)),
    # Arrow down into a tray.
    "received": lambda d: (
        stroke(d, [(12, 3.5), (12, 13.5)]),
        stroke(d, [(7.5, 9.5), (12, 14), (16.5, 9.5)]),
        stroke(d, [(4, 15), (4, 19.5), (20, 19.5), (20, 15)]),
    ),
}


def badge(color):
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.ellipse((0, 0, S - 1, S - 1), fill=color + (255,))
    return img, d


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    for name, color in COLORS.items():
        img, d = badge(color)
        glyph = GLYPHS[name]
        # Lambdas scale glyph points from the 24-box via p(); arcs work in that box too.
        glyph(d)
        img.resize((SIZE, SIZE), Image.LANCZOS).save(OUT / f"{name}.png")

    # Bot avatar: dark tile with an Ergo-orange node/pulse mark.
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle((0, 0, S - 1, S - 1), radius=S // 4, fill=(17, 19, 26, 255))
    orange = (255, 90, 31, 255)
    pts = [p(*xy) for xy in [(2, 13), (7, 13), (9.5, 7), (13.5, 18), (16, 11), (22, 11)]]
    width = int(2.4 * S * 0.5 / 24)
    d.line(pts, fill=orange, width=width, joint="curve")
    for x, y in (pts[0], pts[-1]):
        r = width / 2
        d.ellipse((x - r, y - r, x + r, y + r), fill=orange)
    img.resize((SIZE, SIZE), Image.LANCZOS).save(OUT / "avatar.png")
    print(f"wrote {len(COLORS) + 1} icons to {OUT}")


if __name__ == "__main__":
    main()
