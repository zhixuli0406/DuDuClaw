#!/usr/bin/env python3
"""Generate the DMG window background (src-tauri/dmg/background.png).

Rendered with Pillow rather than an SVG rasterizer: qlmanage mats transparent
SVGs onto white, which left a white band in the DMG window. This draws an
opaque 2x (1320x800) canvas that lines up with the Tauri DMG layout
(window 660x400, app icon @ (180,170), Applications @ (480,170)).

    pip install Pillow && python3 scripts/desktop/gen-dmg-background.py
"""
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

# macOS sizes the DMG background by the image's *point* size = pixels / (dpi/72).
# We ship a 2x (1320x800) image tagged 144 DPI, so it occupies 660x400 POINTS
# and exactly fills a 660x400-point window — crisp on Retina, correct on non-
# Retina. (A 72-DPI 1320px image would be read as 1320 pt and overflow; a 660px
# image would under-cover a Retina window.) Keep WINDOW_PT == bundle windowSize.
WINDOW_PT = (660, 400)      # must match bundle.macOS.dmg windowSize
DPI = 144                   # 2x
W, H = WINDOW_PT[0] * DPI // 72, WINDOW_PT[1] * DPI // 72
# C1 brand palette (dark surface): navy page, cream ink, amber accent.
BG = (11, 30, 51)           # #0B1E33 — dark-theme background
AMBER = (204, 139, 45)      # #CC8B2D — DuDuClaw accent (decorative)
CREAM = (245, 244, 240)     # #F5F4F0 — text / mark ink on dark
SUB = (183, 189, 196)       # #B7BDC4 — muted text on dark
FOOT = (183, 189, 196)      # #B7BDC4

ROOT = Path(__file__).resolve().parents[2]
# Transparent color mark (navy ink + amber tail) from the C1 brand kit.
MARK_SRC = ROOT / "docs/assets/brand/duduclaw-icon-1024.png"
MARK_PX = 76                # rendered mark height in pixels (2x canvas)

# zh-TW capable fonts that ship with macOS, in preference order. PingFang isn't
# present on every macOS version; Heiti TC (STHeiti Medium.ttc idx 0) is the
# reliable Traditional-Chinese fallback (otherwise CJK renders as tofu boxes).
FONT_CANDIDATES = [
    ("/System/Library/Fonts/PingFang.ttc", 0),
    ("/System/Library/Fonts/STHeiti Medium.ttc", 0),   # Heiti TC Medium
    ("/System/Library/Fonts/Hiragino Sans GB.ttc", 2),
]


def font(size):
    import os

    for path, idx in FONT_CANDIDATES:
        if os.path.exists(path):
            try:
                return ImageFont.truetype(path, size, index=idx)
            except OSError:
                continue
    return ImageFont.load_default()


def wordmark_font(size):
    """Brand wordmark face: Avenir Next Demi Bold (weight 600), shipped with
    macOS; falls back to the zh-TW stack above."""
    try:
        return ImageFont.truetype("/System/Library/Fonts/Avenir Next.ttc", size, index=2)
    except OSError:
        return font(size)


def reverse_mark(height):
    """The color mark recolored for a dark background: ink layer -> cream,
    accent layer kept amber (brand rule: never a navy mark on dark)."""
    mark = Image.open(MARK_SRC).convert("RGBA")
    mark = mark.crop(mark.getbbox())
    px = mark.load()
    for y in range(mark.height):
        for x in range(mark.width):
            r, g, b, a = px[x, y]
            if a and r < 110:   # navy ink (amber has R ~203)
                px[x, y] = CREAM + (a,)
    w = round(mark.width * height / mark.height)
    return mark.resize((w, height), Image.LANCZOS)


img = Image.new("RGBA", (W, H), BG + (255,))
d = ImageDraw.Draw(img)
title, sub, foot = wordmark_font(58), font(28), font(22)

# Lockup: mark + live wordmark, centered as one group.
mark = reverse_mark(MARK_PX)
gap = 22
tw = d.textlength("DuDuClaw", font=title)
x0 = (W - (mark.width + gap + tw)) / 2
img.alpha_composite(mark, (round(x0), 78))
d.text((x0 + mark.width + gap, 86), "DuDuClaw", font=title, fill=CREAM)


def centered(y, text, fnt, fill):
    w = d.textlength(text, font=fnt)
    d.text(((W - w) / 2, y), text, font=fnt, fill=fill)


centered(190, "把 DuDuClaw 拖曳到 Applications 完成安裝", sub, SUB)

# drag arrow in the gap between the two icons (app@x360 → Applications@x960, y=340)
d.line([524, 340, 788, 340], fill=AMBER, width=6)
d.polygon([(788, 324), (820, 340), (788, 356)], fill=AMBER)

centered(724, "您的 AI 員工 · 跑在自己的機器上,資料不出機", foot, FOOT)

out = img.convert("RGB")
out.save(ROOT / "src-tauri/dmg/background.png", dpi=(DPI, DPI))
print(f"wrote src-tauri/dmg/background.png {out.size}px @ {DPI}dpi "
      f"= {WINDOW_PT[0]}x{WINDOW_PT[1]}pt")
