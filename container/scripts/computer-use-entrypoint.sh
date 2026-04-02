#!/bin/bash
# Entrypoint for DuDuClaw Computer Use (L5) container.
# Starts Xvfb virtual display, optional VNC server, and Chromium.

set -euo pipefail

DISPLAY_SIZE="${DISPLAY_SIZE:-1280x800}"
DISPLAY_DEPTH="${DISPLAY_DEPTH:-24}"
VNC_ENABLED="${VNC_ENABLED:-false}"
VNC_PASSWORD="${VNC_PASSWORD:-duduclaw}"

echo "[computer-use] Starting virtual display: ${DISPLAY_SIZE}x${DISPLAY_DEPTH}"

# Start Xvfb
Xvfb :99 -screen 0 "${DISPLAY_SIZE}x${DISPLAY_DEPTH}" -ac +extension GLX +render -noreset &
XVFB_PID=$!
sleep 1

# Verify Xvfb is running
if ! kill -0 $XVFB_PID 2>/dev/null; then
    echo "[computer-use] ERROR: Xvfb failed to start"
    exit 1
fi
echo "[computer-use] Xvfb started (PID: $XVFB_PID)"

# Optional: Start VNC server
if [ "$VNC_ENABLED" = "true" ]; then
    echo "[computer-use] Starting VNC server on :5900"
    x11vnc -display :99 -forever -passwd "$VNC_PASSWORD" -rfbport 5900 -bg -q
    echo "[computer-use] VNC server started"
fi

# Apply domain filtering
source /usr/local/bin/domain-filter.sh

# Start Chromium in background (maximized)
chromium-browser \
    --no-sandbox \
    --disable-gpu \
    --disable-dev-shm-usage \
    --window-size="${DISPLAY_SIZE/x/,}" \
    --start-maximized \
    --disable-extensions \
    --disable-background-networking \
    --no-first-run \
    "about:blank" &

echo "[computer-use] Container ready. Waiting for commands..."

# Keep running until killed
wait $XVFB_PID
