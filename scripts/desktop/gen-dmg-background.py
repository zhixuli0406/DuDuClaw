#!/usr/bin/env python3
"""Generate the DMG window background (src-tauri/dmg/background.png).

Rendered with Pillow rather than an SVG rasterizer: qlmanage mats transparent
SVGs onto white, which left a white band in the DMG window. This draws an
opaque 2x (1320x800) canvas that lines up with the Tauri DMG layout
(window 660x400, app icon @ (180,170), Applications @ (480,170)).

    pip install Pillow && python3 scripts/desktop/gen-dmg-background.py
"""
from PIL import Image, ImageDraw, ImageFont, ImageFilter

# macOS sizes the DMG background by the image's *point* size = pixels / (dpi/72).
# We ship a 2x (1320x800) image tagged 144 DPI, so it occupies 660x400 POINTS
# and exactly fills a 660x400-point window — crisp on Retina, correct on non-
# Retina. (A 72-DPI 1320px image would be read as 1320 pt and overflow; a 660px
# image would under-cover a Retina window.) Keep WINDOW_PT == bundle windowSize.
WINDOW_PT = (660, 400)      # must match bundle.macOS.dmg windowSize
DPI = 144                   # 2x
W, H = WINDOW_PT[0] * DPI // 72, WINDOW_PT[1] * DPI // 72
STONE = (28, 25, 23)        # #1c1917 — app surface-dark
AMBER = (245, 158, 11)      # #f59e0b
WHITE = (250, 250, 249)     # #fafaf9
SUB = (168, 162, 158)       # stone-400
FOOT = (120, 113, 108)      # stone-500

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


img = Image.new("RGBA", (W, H), STONE + (255,))

# subtle amber glow along the top edge
glow = Image.new("RGBA", (W, H), (0, 0, 0, 0))
ImageDraw.Draw(glow).ellipse([W // 2 - 560, -440, W // 2 + 560, 300], fill=AMBER + (44,))
img = Image.alpha_composite(img, glow.filter(ImageFilter.GaussianBlur(180)))

d = ImageDraw.Draw(img)
title, sub, foot = font(58), font(28), font(22)


def centered(y, text, fnt, fill):
    w = d.textlength(text, font=fnt)
    d.text(((W - w) / 2, y), text, font=fnt, fill=fill)


centered(86, "DuDuClaw", title, WHITE)
centered(190, "把 DuDuClaw 拖曳到 Applications 完成安裝", sub, SUB)

# drag arrow in the gap between the two icons (app@x360 → Applications@x960, y=340)
d.line([524, 340, 788, 340], fill=AMBER, width=6)
d.polygon([(788, 324), (820, 340), (788, 356)], fill=AMBER)

centered(724, "您的 AI 員工 · 跑在自己的機器上,資料不出機", foot, FOOT)

out = img.convert("RGB")
out.save("src-tauri/dmg/background.png", dpi=(DPI, DPI))
print(f"wrote src-tauri/dmg/background.png {out.size}px @ {DPI}dpi "
      f"= {WINDOW_PT[0]}x{WINDOW_PT[1]}pt")
