#!/usr/bin/env bash
# Regenerate the static weight instances in this directory from the variable
# originals one level up. Why statics exist at all: the shell/native-gui text
# system registers a variable font under its fvar DEFAULT instance, and
# NotoSansTC-Variable's default is wght=100 (Thin) while InterVariable's is
# wght=400 — so all CJK rendered Thin against Regular Latin (user bug report
# 2026-08-22). See duduclaw-native-gui/src/theme.rs::BUNDLED_FONTS.
#
# Needs fonttools (pip install fonttools). Inter pins opsz=14 ("Text", the
# UI optical size) on top of the weight.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
command -v fonttools >/dev/null || { echo "fonttools not found — pip install fonttools" >&2; exit 1; }
for W in 400 500 600 700; do
    fonttools varLib.instancer --update-name-table -q -o "NotoSansTC-$W.ttf" ../NotoSansTC-Variable.ttf "wght=$W"
    fonttools varLib.instancer --update-name-table -q -o "Inter-$W.ttf" ../InterVariable.ttf "wght=$W" "opsz=14"
done
echo "regenerated 8 static faces"
