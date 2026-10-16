#!/usr/bin/env python3
"""Build the desktop app-icon master (src-tauri/icons/source/duduclaw-1024.png).

Input is the C1 brand avatar (docs/assets/brand/duduclaw-avatar-1024.png): the
cat mark on an opaque cream square. App icons need a background, so the master
keeps that square but cuts it to a rounded rectangle with a supersampled alpha
mask (clean transparent corners, no white fringe; see desktop-build.md §9).
`scripts/desktop/gen-icons.sh` / `cargo tauri icon` then derive every platform
size from this one file.

    pip install Pillow && python3 scripts/desktop/make-icon-master.py
"""
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parents[2]
SRC = ROOT / "docs/assets/brand/duduclaw-avatar-1024.png"
OUT = ROOT / "src-tauri/icons/source/duduclaw-1024.png"

SIZE = 1024
RADIUS = 225          # ~22% of the edge, same corner as the previous master
SUPERSAMPLE = 4       # draw the mask at 4x, downsample for an antialiased edge


def rounded_mask(size: int, radius: int, ss: int) -> Image.Image:
    big = Image.new("L", (size * ss, size * ss), 0)
    ImageDraw.Draw(big).rounded_rectangle(
        [0, 0, size * ss - 1, size * ss - 1], radius=radius * ss, fill=255
    )
    return big.resize((size, size), Image.LANCZOS)


def main() -> None:
    src = Image.open(SRC).convert("RGB")
    if src.size != (SIZE, SIZE):
        src = src.resize((SIZE, SIZE), Image.LANCZOS)
    out = src.convert("RGBA")
    out.putalpha(rounded_mask(SIZE, RADIUS, SUPERSAMPLE))
    OUT.parent.mkdir(parents=True, exist_ok=True)
    out.save(OUT, optimize=True)
    print(f"wrote {OUT.relative_to(ROOT)} {out.size}px")


if __name__ == "__main__":
    main()
