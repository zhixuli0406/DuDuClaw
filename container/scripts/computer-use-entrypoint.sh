#!/bin/bash
# Entrypoint for the DuDuClaw Computer Use (L5) container.
# Starts the Xvfb display, openbox, and Chromium in kiosk mode. The live-view
# VNC server is not started here: the gateway starts it on demand with
# `duduclaw-vnc` (unix socket in /tmp/duduclaw-root only, no TCP port).
#
# Contract with the gateway (computer_use_orchestrator.rs / computer_use.rs):
#   - DISPLAY=:99, size from DISPLAY_SIZE (<w>x<h>, default 1280x800).
#   - Root filesystem is read-only; only /tmp (tmpfs) is writable, so HOME,
#     the Chromium profile and the X socket all live under /tmp.
#   - `xdotool getactivewindow` must succeed once the browser window is up
#     (readiness poll + HEALTHCHECK) -> an EWMH window manager (openbox) runs.
#   - `duduclaw-eval-dom` talks CDP to 127.0.0.1:${CDP_PORT}. Chromium binds the
#     DevTools port to loopback only (--remote-debugging-address=127.0.0.1; no
#     --remote-allow-origins, so web pages cannot open the DevTools WebSocket).
#   - Screenshot mapping: --kiosk + --window-position=0,0 + full-screen
#     --window-size + --force-device-scale-factor=1 make the page viewport start
#     at screen pixel 0,0 with no browser chrome above it, so a
#     getBoundingClientRect() CSS pixel equals a scrot pixel (times
#     devicePixelRatio, which only changes under page zoom). --test-type hides
#     the "unsupported command-line flag" infobar that --no-sandbox would
#     otherwise insert above the viewport. duduclaw-eval-dom re-checks this
#     geometry on every call and fails closed if it does not hold.
#   - The browser is restarted if it exits (e.g. the agent closed the window),
#     so masking does not stay in its fail-closed state for the whole session.
#   - Egress: domain-filter.sh runs FIRST, as root. With `--network=none` it
#     only sets default-deny policies; with ALLOWED_IPS (tool-driven sessions
#     with a navigation allowlist) it allows TCP 443 to the gateway-pinned
#     addresses only. Then the entrypoint re-executes itself through
#     `setpriv --bounding-set -net_admin`, so the display, the window manager,
#     the browser and everything else that keeps running can never regain
#     CAP_NET_ADMIN (the browser additionally runs as the unprivileged
#     `sandbox` user). Names resolve through /etc/hosts only: the gateway's
#     `--add-host` entries; Chromium's built-in DNS client and DNS-over-HTTPS
#     are off (flags below plus /etc/chromium/policies/managed/duduclaw.json).
#   - `duduclaw-navigate` (URL on stdin, `docker exec -i`) opens a page
#     through the same loopback DevTools port (one JSON line out, see that
#     script).
#   - /tmp/duduclaw-root (root, 0700) holds root-side temp files: the
#     gateway's screenshot (`scrot -o /tmp/duduclaw-root/screen.png`) and the
#     browser log. Meant to run with `--security-opt no-new-privileges`
#     (setpriv only ever drops privileges, which that does not affect).

set -euo pipefail

if [ "${1:-}" != "--duduclaw-filtered" ]; then
    # Stage 1 (root, full capability set): install the egress filter (exits
    # non-zero when egress could not be filtered), then drop CAP_NET_ADMIN
    # from the bounding set for good and continue as stage 2.
    source /usr/local/bin/domain-filter.sh
    exec setpriv --bounding-set -net_admin -- "$0" --duduclaw-filtered
fi
# Stage 2: refuse to run if NET_ADMIN (bit 12) is still in the bounding set.
CAP_BND=$(awk '/^CapBnd:/ { print $2 }' /proc/self/status)
if (( (16#${CAP_BND:-ffffffffffffffff} >> 12) & 1 )); then
    echo "[computer-use] ERROR: CAP_NET_ADMIN still in the bounding set" >&2
    exit 1
fi

# Root-only scratch directory for every root-side temp file (the gateway's
# `docker exec` screenshot `scrot -o /tmp/duduclaw-root/screen.png` + `cat`,
# the browser log). Created here, before anything runs as `sandbox`, so the
# browser user can neither pre-create nor race a path in the world-writable
# /tmp. Fails if it already exists (it never should on a fresh tmpfs).
ROOT_TMP=/tmp/duduclaw-root
if [ -e "$ROOT_TMP" ] || [ -L "$ROOT_TMP" ]; then
    echo "[computer-use] ERROR: $ROOT_TMP already exists" >&2
    exit 1
fi
mkdir -m 0700 "$ROOT_TMP"

DISPLAY_SIZE="${DISPLAY_SIZE:-1280x800}"
DISPLAY_DEPTH="${DISPLAY_DEPTH:-24}"
CDP_PORT="${DUDUCLAW_CDP_PORT:-9222}"
START_URL="${START_URL:-about:blank}"
SCREEN_W="${DISPLAY_SIZE%x*}"
SCREEN_H="${DISPLAY_SIZE#*x}"

if ! [[ "$SCREEN_W" =~ ^[0-9]+$ && "$SCREEN_H" =~ ^[0-9]+$ ]]; then
    echo "[computer-use] ERROR: bad DISPLAY_SIZE '$DISPLAY_SIZE'" >&2
    exit 1
fi

echo "[computer-use] Starting virtual display: ${DISPLAY_SIZE}x${DISPLAY_DEPTH}"

# Start Xvfb. -nolisten tcp: X is reachable through the unix socket only.
Xvfb :99 -screen 0 "${DISPLAY_SIZE}x${DISPLAY_DEPTH}" -ac -nolisten tcp +extension GLX +render -noreset &
XVFB_PID=$!

for _ in $(seq 1 50); do
    [ -S /tmp/.X11-unix/X99 ] && break
    kill -0 "$XVFB_PID" 2>/dev/null || break
    sleep 0.1
done
if ! kill -0 "$XVFB_PID" 2>/dev/null || [ ! -S /tmp/.X11-unix/X99 ]; then
    echo "[computer-use] ERROR: Xvfb failed to start" >&2
    exit 1
fi
echo "[computer-use] Xvfb started (PID: $XVFB_PID)"

# Everything below runs as the unprivileged `sandbox` user with HOME in /tmp.
SANDBOX_HOME=/tmp/sandbox-home
mkdir -p "$SANDBOX_HOME"
chown sandbox:sandbox "$SANDBOX_HOME"
as_sandbox() {
    setpriv --reuid=sandbox --regid=sandbox --init-groups \
        env HOME="$SANDBOX_HOME" XDG_CONFIG_HOME="$SANDBOX_HOME/.config" \
            XDG_CACHE_HOME="$SANDBOX_HOME/.cache" DISPLAY=:99 "$@"
}

as_sandbox openbox --sm-disable >/dev/null 2>&1 &

run_browser() {
    while kill -0 "$XVFB_PID" 2>/dev/null; do
        as_sandbox chromium \
            --kiosk \
            --window-position=0,0 \
            --window-size="${SCREEN_W},${SCREEN_H}" \
            --force-device-scale-factor=1 \
            --user-data-dir="$SANDBOX_HOME/chromium-profile" \
            --remote-debugging-address=127.0.0.1 \
            --remote-debugging-port="$CDP_PORT" \
            --no-sandbox \
            --test-type \
            --disable-gpu \
            --disable-dev-shm-usage \
            --disable-extensions \
            --disable-background-networking \
            --disable-component-update \
            --disable-sync \
            --disable-translate \
            --disable-features=Translate,MediaRouter,OptimizationHints,SpareRendererForSitePerProcess,AudioServiceOutOfProcess,AsyncDns,DnsOverHttps \
            --in-process-gpu \
            --disable-crash-reporter \
            --disable-breakpad \
            --no-first-run \
            --no-default-browser-check \
            --password-store=basic \
            --renderer-process-limit=2 \
            "$START_URL" >>"$ROOT_TMP/chromium.log" 2>&1 || true
        echo "[computer-use] Chromium exited; restarting in 1s" >&2
        sleep 1
    done
}
run_browser &

echo "[computer-use] Container ready. Waiting for commands..."

# Keep running until killed
wait "$XVFB_PID"
