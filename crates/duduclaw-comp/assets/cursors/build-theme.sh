#!/usr/bin/env bash
# CUR-2 — build the DuDuClaw brand XCursor theme from the SVG masters.
#
#   ./build-theme.sh            # regenerate DuDuClaw/ from svg/*.svg
#   ./build-theme.sh --check    # rebuild into a temp dir and diff against
#                               # the committed DuDuClaw/ (CI / review use)
#
# ── What this produces ────────────────────────────────────────────────────
#
# A perfectly ordinary XCursor theme directory:
#
#   DuDuClaw/
#     index.theme            [Icon Theme] Inherits=Adwaita
#     cursors/default        binary XCursor file, 4 nominal sizes inside
#     cursors/grab
#     cursors/grabbing
#     cursors/left_ptr  -> default      (freedesktop/X11 alias names, so a
#     cursors/arrow     -> default       toolkit that loads the theme itself
#     cursors/openhand  -> grab          instead of going through the
#     cursors/closedhand-> grabbing      compositor's cursor-shape-v1 sees
#                                        the brand art too)
#
# That is deliberately the SAME shape as any system theme: `duduclaw-comp`
# needs ZERO new loading code for it (crates/duduclaw-comp/src/cursor/
# source.rs's module doc — "Brand only changes the theme *name* handed to
# xcursor::CursorTheme::load").
#
# ── Why only three shapes ─────────────────────────────────────────────────
#
# `index.theme` declares `Inherits=Adwaita`, and both libXcursor and the
# `xcursor` Rust crate walk that chain, so every shape this theme does NOT
# carry (text, resize arrows, wait, …) resolves to the system theme
# automatically. Nothing ever goes missing by drawing three cursors instead
# of forty.
#
# `pointer` (the "this is a link" hand) is deliberately NOT one of them:
# that shape's meaning is owned by two decades of hyperlink convention and
# Microsoft's own Win32 UX guidance warns against repurposing it. A brand
# paw there would read as "clickable", not as "DuDuClaw".
#
# ── Why PNG, not SVG ──────────────────────────────────────────────────────
#
# `xcursor` 0.3 (what duduclaw-comp uses) can *locate* `cursors_scalable/`
# entries but explicitly leaves rendering them to the caller — see
# src/cursor/theme.rs's "honest limitations". So the SVG masters have to be
# rasterised ahead of time. XCursor's own file format is raster-only anyway.
#
# ── Sizes and the hotspot arithmetic ──────────────────────────────────────
#
# 24 / 32 / 48 / 64 (research note icon-and-cursor-system-2026-08.md §1.4.2).
# 64 is the ceiling on purpose: it is the width every mainstream DRM driver
# guarantees for a hardware cursor plane, and a cursor larger than the plane
# forces software compositing.
#
# Each master is authored in a 24-unit viewBox, so one unit is size/24 px and
# a hotspot expressed as a fraction of 24 stays a WHOLE pixel at every output
# size — no per-size rounding, no tip drift:
#
#   default          tip of the claw, unit (3,3)   -> size/8   = 3 / 4 / 6 / 8
#   grab / grabbing  centre of the paw, unit (12,12) -> size/2  = 12/16/24/32
#
# `grab`/`grabbing` share a hotspot on purpose: pressing the button must not
# make the pointer jump. Centre (rather than a tip) matches the X11
# `openhand`/`closedhand` convention — while dragging, the pointer IS the
# thing being carried.
#
# ── Colours ───────────────────────────────────────────────────────────────
#
#   fill    #E85055  brand crimson (appliance/branding/duduclaw-cat.svg's
#                    torso gradient mid-tone — the mascot's own colour)
#   outline #1C1917  stone-900 (CLAUDE.md palette), 1.2 units => 1.2 px at
#                    size 24 and proportionally more above it
#
# Dark-outline-around-a-coloured-fill is the research note's prescription and
# it is what makes the cursor survive an arbitrary background: on white the
# crimson fill carries (3.7:1), on black the crimson fill still carries
# (5.7:1), and on a red-ish background the stone-900 outline carries (4.4:1).
# No single-colour cursor has that property.
#
# ── Why the OUTPUT is committed to git as well as the masters ─────────────
#
# Because the consumer is an OS image build (appliance/build.sh) that runs
# inside an mkosi container which has neither librsvg nor xcursorgen, on a
# macOS host that has neither either. Committing ~100 KB of generated binary
# is the honest trade against making the image build depend on two more
# toolchains; `--check` is what keeps the two halves from drifting.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SVG_DIR="$HERE/svg"
THEME_NAME="DuDuClaw"

SIZES=(24 32 48 64)

# shape:hotspot-divisor — the divisor turns an output size into the hotspot
# in pixels (see the arithmetic block above). Both are exact for all four
# sizes; the script asserts that rather than trusting it.
SHAPES=(
    "default:8"
    "grab:2"
    "grabbing:2"
)

# Alias names to symlink onto each shape. Deliberately conservative: only
# names whose meaning is unambiguously the same shape. `fleur` is NOT here
# (in X11 that is the four-way MOVE cursor, not a grab), and neither are
# `hand1`/`hand2` (toolkits disagree about whether those mean grab or the
# hyperlink pointer, and `pointer` is out of scope by design).
declare -A ALIASES=(
    [default]="left_ptr arrow"
    [grab]="openhand"
    [grabbing]="closedhand"
)

CHECK_ONLY=0
if [[ "${1:-}" == "--check" ]]; then
    CHECK_ONLY=1
elif [[ $# -gt 0 ]]; then
    echo "usage: $0 [--check]" >&2
    exit 2
fi

# ── Tool preflight — fail loudly, never emit half a theme ─────────────────
missing=()
command -v rsvg-convert >/dev/null 2>&1 || missing+=("rsvg-convert (Debian/Ubuntu: librsvg2-bin, macOS: brew install librsvg)")
command -v xcursorgen  >/dev/null 2>&1 || missing+=("xcursorgen (Debian/Ubuntu: x11-apps, macOS: not packaged — use the container in ../../BUILD.md)")
if (( ${#missing[@]} )); then
    echo "build-theme.sh: required tool(s) not found:" >&2
    for m in "${missing[@]}"; do echo "  - $m" >&2; done
    echo "Refusing to run: a partial theme directory is worse than none — a" >&2
    echo "cursor that exists but has no image is a blank pointer, while a" >&2
    echo "theme that is absent falls back cleanly to the system cursors." >&2
    exit 1
fi

for entry in "${SHAPES[@]}"; do
    shape="${entry%%:*}"
    [[ -f "$SVG_DIR/$shape.svg" ]] || { echo "build-theme.sh: missing master $SVG_DIR/$shape.svg" >&2; exit 1; }
done

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
OUT="$WORK/$THEME_NAME"
mkdir -p "$OUT/cursors"

for entry in "${SHAPES[@]}"; do
    shape="${entry%%:*}"
    divisor="${entry##*:}"
    cfg="$WORK/$shape.cursor"
    : > "$cfg"

    for size in "${SIZES[@]}"; do
        if (( size % divisor != 0 )); then
            # The whole point of choosing 3/24 and 12/24 as hotspots is that
            # this never happens. If someone adds a size or moves a hotspot,
            # they find out here instead of shipping a cursor whose tip is
            # half a pixel off at one size only.
            echo "build-theme.sh: hotspot for '$shape' is not a whole pixel at size $size (size/$divisor)" >&2
            exit 1
        fi
        hot=$(( size / divisor ))
        png="$WORK/$shape-$size.png"
        rsvg-convert -w "$size" -h "$size" -o "$png" "$SVG_DIR/$shape.svg"
        printf '%s %s %s %s\n' "$size" "$hot" "$hot" "$png" >> "$cfg"
    done

    xcursorgen "$cfg" "$OUT/cursors/$shape"
    echo "build-theme.sh: built cursors/$shape (${SIZES[*]}, hotspot size/$divisor)"
done

for shape in "${!ALIASES[@]}"; do
    for alias in ${ALIASES[$shape]}; do
        ln -sf "$shape" "$OUT/cursors/$alias"
    done
done

cat > "$OUT/index.theme" <<EOF
[Icon Theme]
Name=$THEME_NAME
Comment=DuDuClaw OS brand paw cursors
Inherits=Adwaita
EOF

if (( CHECK_ONLY )); then
    if diff -r "$HERE/$THEME_NAME" "$OUT" >/dev/null 2>&1; then
        echo "build-theme.sh: --check OK — committed $THEME_NAME/ matches the SVG masters"
        exit 0
    fi
    echo "build-theme.sh: --check FAILED — committed $THEME_NAME/ differs from a fresh build:" >&2
    diff -r "$HERE/$THEME_NAME" "$OUT" >&2 || true
    exit 1
fi

rm -rf "$HERE/$THEME_NAME"
cp -R "$OUT" "$HERE/$THEME_NAME"
echo "build-theme.sh: wrote $HERE/$THEME_NAME"
