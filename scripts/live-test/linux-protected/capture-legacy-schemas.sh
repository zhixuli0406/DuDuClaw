#!/usr/bin/env bash
# Read-only operator wrapper. stdout is the only artifact to return.
# Usage: bash capture-legacy-schemas.sh <home> <creating-version>
# CAPTURE_BIN must name the reviewer's prebuilt capture binary.
set -euo pipefail
[ "$#" -eq 2 ] || { echo '用法：capture-legacy-schemas.sh <home> <建立版本>' >&2; exit 2; }
[ "$(uname -s)" = Linux ] || { echo '請在拋棄式 Linux VM 執行。' >&2; exit 2; }
: "${CAPTURE_BIN:?請設定 CAPTURE_BIN 為已建置的 duduclaw-capture-legacy-schemas}"
case "$2" in v1.80.0|v1.80.1|v1.81.0|v1.82.0) ;; *) echo '不支援此建立版本。' >&2; exit 2 ;; esac
exec "$CAPTURE_BIN" "$1" --created-with "$2"
