#!/usr/bin/env bash
# Read-only operator wrapper. stdout is the only artifact to return.
# Usage: bash capture-legacy-schemas.sh <home> <creating-version>
# CAPTURE_BIN must name the reviewer's prebuilt capture binary.
PATH=/usr/bin:/bin
export PATH
set -euo pipefail
[ "$#" -eq 2 ] || { echo '用法：capture-legacy-schemas.sh <home> <建立版本>' >&2; exit 2; }
[ "$(uname -s)" = Linux ] || { echo '請在拋棄式 Linux VM 執行。' >&2; exit 2; }
: "${CAPTURE_BIN:?請設定 CAPTURE_BIN 為已建置的 duduclaw-capture-legacy-schemas}"
case "$CAPTURE_BIN" in /*) ;; *) echo 'CAPTURE_BIN 必須為絕對路徑。' >&2; exit 2 ;; esac
[ -f "$CAPTURE_BIN" ] && [ -x "$CAPTURE_BIN" ] && [ ! -L "$CAPTURE_BIN" ] || {
  echo 'CAPTURE_BIN 必須為非符號連結的一般可執行檔。' >&2; exit 2;
}
case "$2" in v1.80.0|v1.80.1|v1.81.0|v1.82.0) ;; *) echo '不支援此建立版本。' >&2; exit 2 ;; esac
exec "$CAPTURE_BIN" "$1" --created-with "$2"
