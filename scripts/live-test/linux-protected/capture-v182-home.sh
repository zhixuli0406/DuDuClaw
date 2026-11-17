#!/usr/bin/env bash
# Reviewer-only smoke fixture; no compilation or downloads are performed here.
# Usage: DUDUCLAW_V182_BIN=/verified/v182/duduclaw CAPTURE_BIN=/built/capture \
#   bash capture-v182-home.sh > /outside/home/report.json
set -euo pipefail
[ "$#" -eq 0 ] || { echo '此腳本不接受既有 home；只建立拋棄式 fixture。' >&2; exit 2; }
[ "$(uname -s)" = Linux ] || { echo '請在拋棄式 Linux VM 執行。' >&2; exit 2; }
: "${DUDUCLAW_V182_BIN:?請設定已驗證官方簽章的 v1.82.x 執行檔}"
: "${CAPTURE_BIN:?請設定已建置的 duduclaw-capture-legacy-schemas}"
case "$DUDUCLAW_V182_BIN" in /*) ;; *) echo '執行檔必須為絕對路徑。' >&2; exit 2 ;; esac
case "$("$DUDUCLAW_V182_BIN" --version)" in 'duduclaw 1.82.0') ;; *) echo 'fixture 建立版本必須為 v1.82.0。' >&2; exit 2 ;; esac
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd -P)"
FIXTURE="$(mktemp -d /tmp/duduclaw-r5-capture.XXXXXXXX)"
PORT="${CAPTURE_PORT:-18978}"
GATEWAY_PID=''
stop_gateway() {
  if [ -n "$GATEWAY_PID" ]; then
    kill -TERM "$GATEWAY_PID" 2>/dev/null || true
    for _ in $(seq 1 30); do
      kill -0 "$GATEWAY_PID" 2>/dev/null || break
      sleep 1
    done
    if kill -0 "$GATEWAY_PID" 2>/dev/null; then
      kill -KILL "$GATEWAY_PID" 2>/dev/null || true
      echo 'Gateway 未正常停止；capture 將拒絕尚未 checkpoint 的 WAL。' >&2
    fi
    wait "$GATEWAY_PID" || true
    GATEWAY_PID=''
  fi
}
trap stop_gateway EXIT
# Keep diagnostics in this disposable fixture; stdout is report JSON only.
bash "$SCRIPT_DIR/../make-home.sh" "$FIXTURE/home" --port "$PORT" > "$FIXTURE/make-home.log"
HOME="$FIXTURE/home/os-home" DUDUCLAW_HOME="$FIXTURE/home" \
  "$DUDUCLAW_V182_BIN" run --yes > "$FIXTURE/gateway.log" 2>&1 &
GATEWAY_PID=$!
READY=false
for _ in $(seq 1 60); do
  kill -0 "$GATEWAY_PID" 2>/dev/null || break
  if curl --noproxy '*' --silent --fail --max-time 1 "http://127.0.0.1:$PORT/health" > /dev/null; then
    if [ -f "$FIXTURE/home/sessions.db" ] && kill -0 "$GATEWAY_PID" 2>/dev/null; then READY=true; break; fi
  fi
  sleep 1
done
if [ "$READY" != true ]; then echo 'Gateway 未就緒；fixture 保留在 /tmp，請在 VM 檢查。' >&2; exit 3; fi
stop_gateway
# A clean shutdown must checkpoint SQLite. The capture refuses leftover WAL;
# this script never edits/checkpoints a customer's home or guesses a schema.
echo "拋棄式 fixture 保留：$FIXTURE；只回傳 stdout 報告。" >&2
bash "$SCRIPT_DIR/capture-legacy-schemas.sh" "$FIXTURE/home" v1.82.0
