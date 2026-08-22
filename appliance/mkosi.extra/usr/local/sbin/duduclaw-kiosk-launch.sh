#!/usr/bin/env bash
# ExecStart= for duduclaw-kiosk.service — only ever reached after
# ExecCondition= (duduclaw-kiosk-detect-display.sh) has already confirmed a
# display is attached.
#
# cage(1) SYNOPSIS (verified against the upstream man page source,
# cage-kiosk/cage, cage.1.scd): "cage [options...] [--] [application...]"
# — everything after -- is the child process's own argv, launched with
# WAYLAND_DISPLAY (and friends) already exported by cage itself, standard
# behavior for any Wayland compositor spawning its one client. `-d`
# ("Don't draw client side decorations when possible") is belt-and-braces
# alongside Chromium's own --kiosk (which already requests a borderless
# window on its own) — not load-bearing by itself.
#
# Every Chromium flag below has its exact string confirmed against
# chromium/chromium source this round (chrome/common/chrome_switches.cc,
# base/base_switches.h, ui/ozone/public/ozone_switches.cc) — not guessed
# or copied from a third-party "chromium flags" list:
#   --kiosk                     kKioskMode: "Enable kiosk mode."
#   --ozone-platform=wayland    kOzonePlatform: run Chromium's native
#                                Wayland backend under cage, instead of
#                                falling back through XWayland (which
#                                cage's own xwayland dependency would also
#                                support) — one fewer moving part for the
#                                only app this compositor ever runs.
#   --no-first-run              kNoFirstRun: skip first-run UI/promo
#                                surfaces.
#   --noerrdialogs               kNoErrorDialogs (base/base_switches.h):
#                                "Suppresses all error dialogs when
#                                present."
#   --hide-crash-restore-bubble  kHideCrashRestoreBubble: suppresses the
#                                "restore pages?" bubble after an unclean
#                                shutdown — expected after every power cut
#                                on an appliance, not a noteworthy event.
#   --disable-component-update  kDisableComponentUpdate: this is Debian's
#                                chromium package (no Google Update
#                                machinery to begin with), but the
#                                component-updater subsystem still exists
#                                independently of that and would otherwise
#                                phone home on a schedule.
#   --user-data-dir=...         kUserDataDir: explicit profile path on the
#                                only writable partition, rather than
#                                relying on implicit XDG resolution off
#                                $HOME (which is /data/duduclaw-kiosk, set
#                                via the user's passwd entry — see
#                                postinst.d/20-users-and-units.sh).
#
# Translate-bar suppression is deliberately NOT a command-line flag here:
# there is no verified single switch that fully disables it (components/
# translate/core/common/translate_switches.cc only has URL/security-origin
# overrides, no on/off toggle). It's set via Chromium's documented
# enterprise policy mechanism instead — see
# /etc/chromium/policies/managed/duduclaw-kiosk.json in this same image
# (a plain JSON file with no room for its own comment; the policy-path and
# policy-name verification for it lives in README.md and this comment
# block instead): policy::kPolicyPath resolves to "/etc/chromium/policies"
# for non-Google-Chrome branding (components/policy/core/common/
# policy_paths.cc — Debian's chromium package uses this branding, not
# GOOGLE_CHROME_BRANDING), "managed" is the mandatory-policy subdirectory
# (kMandatoryConfigDir, config_dir_policy_loader.cc), and "TranslateEnabled"
# is policy::key::kTranslateEnabled — Chromium's own source even documents
# the exact JSON shape in a network-traffic-annotation comment
# (translate_url_fetcher.cc: `chrome_policy { TranslateEnabled {
# TranslateEnabled: false } }`).
#
# The URL is a build-time constant, not read from config.toml at runtime —
# same "no live templating" precedent as nftables.conf's DASHBOARD_PORT.
# 18789 is the gateway's documented default port
# (crates/duduclaw-core/src/config.rs); if that default ever changes, this
# file, nftables.conf, and duduclaw-firstboot-provision.sh's written
# config.toml all need the same update together.
set -euo pipefail
shopt -s nullglob

log() { echo "duduclaw-kiosk-launch: $*" >&2; }

# ── Operator knobs (optional) ────────────────────────────────────────────
# /etc/duduclaw/kiosk.env — root-owned, absent by default, same
# "escape hatch you can drop from a serial/debug session, no image rebuild"
# convention as /etc/duduclaw/kiosk-app below. Sourced with `set -a` so
# every assignment becomes an exported env var for the whole session tree.
# It is a shell fragment, not a key=value parser: it is only ever writable
# by root on this image, and this script runs as the unprivileged
# duduclaw-kiosk user, so sourcing it grants no privilege the writer didn't
# already have. Recognized knobs (all optional, defaults below):
#   DUDUCLAW_KIOSK_DBUS=0                  skip the dbus-run-session wrapper
#   DUDUCLAW_KIOSK_COMP_MAX_FAILURES=<n>   consecutive early comp failures
#                                          before auto-selection stops
#                                          trying comp (default 3)
#   DUDUCLAW_KIOSK_COMP_SOCKET_WAIT_SECS   how long to wait for comp's
#                                          Wayland socket (default 10)
#   DUDUCLAW_KIOSK_COMP_SHELL_PROBE_SECS   how long comp+shell must both
#                                          survive to count as a healthy
#                                          session (default 20)
#   DUDUCLAW_COMP_BACKEND=<name>           passed straight through to
#                                          duduclaw-comp (default "udev")
KIOSK_ENV_FILE=/etc/duduclaw/kiosk.env
if [[ -r "$KIOSK_ENV_FILE" ]]; then
    log "sourcing operator env file $KIOSK_ENV_FILE"
    set -a
    # shellcheck disable=SC1090,SC1091
    . "$KIOSK_ENV_FILE"
    set +a
fi

# ── D-Bus session bus ────────────────────────────────────────────────────
# This unit is a plain systemd SYSTEM service (User=duduclaw-kiosk), NOT a
# PAM/logind login session — so nothing creates /run/user/<uid>, nothing
# starts a `systemd --user` instance, and DBUS_SESSION_BUS_ADDRESS is unset.
# That was fine while the session only ever ran cage+Chromium (a missing
# session bus degrades non-fatally — verified in
# research/native-os-2026-08/flatpak-portal-scope-2026-08.md §3.2), but
# xdg-desktop-portal is a D-Bus SESSION-activated service: with no session
# bus at all, every portal call is `ServiceUnknown` and no Flatpak app can
# ever reach a file chooser / notification / settings portal (that research
# note's §5.3 lists this as an unverified debt — this is the fix).
#
# dbus-run-session starts a private session bus, exports
# DBUS_SESSION_BUS_ADDRESS, execs the command, and tears the bus down on
# exit. The whole session (compositor + shell + anything they spawn) has to
# share ONE bus, so the wrapper goes around this entire script via re-exec
# rather than around any individual branch below. Fail-open: if the binary
# isn't installed the session still starts exactly as before, just without
# portal support, and says so in the journal.
if [[ -z "${DUDUCLAW_KIOSK_DBUS_ACTIVE:-}" && "${DUDUCLAW_KIOSK_DBUS:-1}" != "0" ]]; then
    if command -v dbus-run-session >/dev/null 2>&1; then
        export DUDUCLAW_KIOSK_DBUS_ACTIVE=1
        log "re-exec under dbus-run-session (session bus for xdg-desktop-portal)"
        exec dbus-run-session -- "$0" "$@"
    fi
    log "WARNING: dbus-run-session not found — no session bus; Flatpak apps will have no portal support"
    log "         (if this image is expected to run Flatpak apps, add the dbus-bin package to mkosi.conf)"
fi

# A factual statement about this session, not a forcing knob: every branch
# below is a Wayland compositor. Well-behaved toolkits read this; Chromium
# deliberately does NOT rely on it (its ozone default stays x11 unless
# given an explicit flag — measured in the flatpak research note §3.2③,
# where WAYLAND_DISPLAY was already exported and it still picked x11), so
# the Chromium branch keeps passing --ozone-platform=wayland by hand.
export XDG_SESSION_TYPE=wayland

SHELL_BIN=/usr/local/bin/duduclaw-shell
COMP_BIN=/usr/local/bin/duduclaw-comp
KIOSK_HOME="${HOME:-/data/duduclaw-kiosk}"
RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/duduclaw-kiosk}"

COMP_FAIL_FILE="$KIOSK_HOME/.kiosk-comp-failures"
COMP_MAX_FAILURES="${DUDUCLAW_KIOSK_COMP_MAX_FAILURES:-3}"
COMP_SOCKET_WAIT_SECS="${DUDUCLAW_KIOSK_COMP_SOCKET_WAIT_SECS:-10}"
COMP_SHELL_PROBE_SECS="${DUDUCLAW_KIOSK_COMP_SHELL_PROBE_SECS:-20}"

# ── Persistent compositor-failure breadcrumb ─────────────────────────────
# Lives in $HOME (/data/duduclaw-kiosk — the only writable partition;
# $XDG_RUNTIME_DIR is deliberately NOT used because RuntimeDirectory= wipes
# it on every unit stop, which would make the count meaningless across
# restarts). Every helper is fail-open: an unreadable/unwritable file must
# never stop the kiosk from starting, so a missing count reads as 0 and a
# failed write is logged-and-ignored.
comp_failure_count() {
    local n
    n="$(cat "$COMP_FAIL_FILE" 2>/dev/null || echo 0)"
    [[ "$n" =~ ^[0-9]+$ ]] || n=0
    echo "$n"
}

comp_failure_bump() {
    local n
    n=$(( $(comp_failure_count) + 1 ))
    if printf '%s\n' "$n" > "$COMP_FAIL_FILE" 2>/dev/null; then
        log "duduclaw-comp consecutive early-failure count is now $n (limit $COMP_MAX_FAILURES)"
        log "  clear it with: rm $COMP_FAIL_FILE"
    else
        log "duduclaw-comp failed, but the failure counter at $COMP_FAIL_FILE is not writable — not persisting"
    fi
}

comp_failure_reset() {
    [[ -e "$COMP_FAIL_FILE" ]] || return 0
    rm -f "$COMP_FAIL_FILE" 2>/dev/null || true
}

# First Wayland socket in $XDG_RUNTIME_DIR, or empty. `-S` (is a socket) is
# what filters out smithay's sibling `wayland-N.lock` regular file without
# having to pattern-match the name. smithay's ListeningSocketSource::
# new_auto() always skips "wayland-0" and RuntimeDirectory= hands us a
# freshly created empty dir, so in practice this resolves to "wayland-1" —
# but nothing here depends on that, precisely so a future comp change to
# the socket name can't silently break the handoff.
comp_socket_name() {
    local f
    for f in "$RUNTIME_DIR"/wayland-*; do
        [[ -S "$f" ]] || continue
        printf '%s' "${f##*/}"
        return 0
    done
    return 0
}

# ── Kiosk app selection ──────────────────────────────────────────────────
# Shell-S2 wave (2026-08-20) introduced the gpui shell under cage; the A4
# wave (2026-08-22) adds duduclaw-comp — DuDuClaw OS's own compositor —
# as the preferred session compositor, with cage kept as the automatic
# fallback rather than deleted (design doc DESIGN-native-gui-gpui-2026-08.md
# §13.5, D11: smithay self-built compositor). Three session shapes exist:
#
#   comp      duduclaw-comp owns the hardware (DRM/KMS via seatd) and
#             duduclaw-shell is its only client.  <- preferred
#   cage      cage owns the hardware and duduclaw-shell is its only client
#             (the Shell-S2 shape, still fully supported).
#   chromium  cage owns the hardware and Chromium kiosks the dashboard
#             (the pre-shell shape, byte-identical to before).
#
# Selection order, first match wins:
#   1. /etc/duduclaw/kiosk-app, matched as a whole word:
#        "chromium" -> chromium   (checked first: it was the ONLY token this
#                                  file ever recognized before this wave, so
#                                  its precedence must not change)
#        "comp"     -> comp
#        "cage" or "shell" -> cage ("shell" is the back-compat spelling: an
#                                  operator who wrote it before this wave
#                                  meant shell-under-cage, and must keep
#                                  getting exactly that)
#   2. Auto: comp+shell present and not failure-blacklisted -> comp;
#      else shell present -> cage; else chromium.
KIOSK_APP=""
if [[ -r /etc/duduclaw/kiosk-app ]]; then
    if grep -qw chromium /etc/duduclaw/kiosk-app; then
        KIOSK_APP=chromium
    elif grep -qw comp /etc/duduclaw/kiosk-app; then
        KIOSK_APP=comp
    elif grep -qwE 'cage|shell' /etc/duduclaw/kiosk-app; then
        KIOSK_APP=cage
    fi
    # NOT `[[ ... ]] && log ...`: an && compound that evaluates false is a
    # failing command at top level, which `set -e` turns into an immediate
    # exit — i.e. a kiosk-app file containing none of the three tokens would
    # kill the launcher outright instead of falling through to auto-select.
    if [[ -n "$KIOSK_APP" ]]; then
        log "operator override /etc/duduclaw/kiosk-app selects: $KIOSK_APP"
    fi
fi

if [[ -z "$KIOSK_APP" ]]; then
    if [[ -x "$COMP_BIN" && -x "$SHELL_BIN" ]]; then
        if (( $(comp_failure_count) >= COMP_MAX_FAILURES )); then
            log "duduclaw-comp is failure-blacklisted ($(comp_failure_count) consecutive early failures >= $COMP_MAX_FAILURES) — using cage"
            log "  re-enable with: rm $COMP_FAIL_FILE && systemctl restart duduclaw-kiosk.service"
            KIOSK_APP=cage
        else
            KIOSK_APP=comp
        fi
    elif [[ -x "$SHELL_BIN" ]]; then
        KIOSK_APP=cage
    else
        KIOSK_APP=chromium
    fi
fi

# Demote impossible selections rather than dying on them: an operator who
# writes "comp" into an image built without the binary should get a working
# screen and a journal line, not a restart loop.
if [[ "$KIOSK_APP" == "comp" && ! -x "$COMP_BIN" ]]; then
    log "WARNING: $COMP_BIN not present/executable — falling back to cage"
    KIOSK_APP=cage
fi
if [[ "$KIOSK_APP" == "cage" && ! -x "$SHELL_BIN" ]]; then
    log "WARNING: $SHELL_BIN not present/executable — falling back to the Chromium dashboard kiosk"
    KIOSK_APP=chromium
fi

# ── comp session ─────────────────────────────────────────────────────────
# Returns 1 when the session failed EARLY (caller falls back to cage);
# never returns on the healthy path — it `exit`s with the shell's own
# status so systemd's Restart=on-failure sees the real outcome.
#
# Env notes, all previously found the hard way:
#   - NO LIBGL_ALWAYS_SOFTWARE here. That env on the process that OWNS the
#     hardware makes Mesa refuse ("Not allowed to force software rendering
#     when API explicitly selects a hardware device") — it made cage
#     segfault when it was set on cage instead of on cage's child
#     (crates/duduclaw-comp/BUILD.md "Launch details worth keeping"). In
#     this branch duduclaw-comp IS that process, so the same rule now
#     points at comp. Set it from /etc/duduclaw/kiosk.env only if you are
#     deliberately debugging a software-rendering path.
#   - $XDG_RUNTIME_DIR must already exist at mode 0700 before a compositor
#     starts (cage segfaults, not errors, without it — observed twice).
#     duduclaw-kiosk.service's RuntimeDirectory=/RuntimeDirectoryMode=0700
#     guarantees that; this script never creates it.
#   - WAYLAND_DISPLAY is explicitly UNSET for comp (`env -u`) so it takes
#     its "own the hardware" path rather than nesting itself as a client of
#     something else; DUDUCLAW_COMP_BACKEND is passed explicitly on top of
#     that, so the target backend is never implicit (same philosophy as
#     build.sh always passing --architecture even when it matches the
#     file default). A comp build that doesn't read that variable simply
#     ignores it — the unset WAYLAND_DISPLAY still selects the same path.
run_comp_session() {
    local comp_pid shell_pid sock rc i

    log "starting duduclaw-comp (backend=${DUDUCLAW_COMP_BACKEND:-udev}) as the session compositor"
    env -u WAYLAND_DISPLAY \
        DUDUCLAW_COMP_BACKEND="${DUDUCLAW_COMP_BACKEND:-udev}" \
        "$COMP_BIN" &
    comp_pid=$!

    sock=""
    for (( i = 0; i < COMP_SOCKET_WAIT_SECS * 10; i++ )); do
        kill -0 "$comp_pid" 2>/dev/null || break
        sock="$(comp_socket_name)"
        if [[ -n "$sock" ]]; then
            break
        fi
        sleep 0.1
    done

    if [[ -z "$sock" ]] || ! kill -0 "$comp_pid" 2>/dev/null; then
        log "duduclaw-comp produced no Wayland socket within ${COMP_SOCKET_WAIT_SECS}s (or exited first)"
        kill "$comp_pid" 2>/dev/null || true
        wait "$comp_pid" 2>/dev/null || true
        return 1
    fi
    log "duduclaw-comp is listening on $sock (pid $comp_pid)"

    # Hand the display coordinates to the D-Bus activation environment so a
    # LATER session-activated service (xdg-desktop-portal, portal-gtk) is
    # spawned knowing where the display is. The bus itself was started by
    # dbus-run-session above, before any compositor existed, so it captured
    # an environment with no WAYLAND_DISPLAY in it — without this call every
    # portal dialog would be spawned blind. Non-fatal by construction.
    export WAYLAND_DISPLAY="$sock"
    if [[ -n "${DBUS_SESSION_BUS_ADDRESS:-}" ]] \
        && command -v dbus-update-activation-environment >/dev/null 2>&1; then
        dbus-update-activation-environment WAYLAND_DISPLAY XDG_RUNTIME_DIR XDG_SESSION_TYPE \
            || log "note: dbus-update-activation-environment failed (portal dialogs may not find the display)"
    fi

    log "starting duduclaw-shell as duduclaw-comp's client"
    env DUDUCLAW_HOME="$KIOSK_HOME" "$SHELL_BIN" &
    shell_pid=$!

    # Health gate. The Wayland socket alone is a WEAK liveness signal —
    # smithay creates its listening socket before the backend is brought up,
    # so "socket exists" does not prove comp got the DRM device. Requiring
    # BOTH processes to still be alive after the probe window is the signal
    # that actually means "there is a compositor holding real hardware with
    # a client attached to it": if comp's backend died, comp exits and the
    # shell follows it out on the Wayland disconnect.
    for (( i = 0; i < COMP_SHELL_PROBE_SECS * 10; i++ )); do
        kill -0 "$comp_pid" 2>/dev/null || break
        kill -0 "$shell_pid" 2>/dev/null || break
        sleep 0.1
    done

    if ! kill -0 "$comp_pid" 2>/dev/null || ! kill -0 "$shell_pid" 2>/dev/null; then
        log "duduclaw-comp/duduclaw-shell did not survive ${COMP_SHELL_PROBE_SECS}s — treating as a compositor failure"
        kill "$shell_pid" 2>/dev/null || true
        kill "$comp_pid" 2>/dev/null || true
        wait "$shell_pid" 2>/dev/null || true
        wait "$comp_pid" 2>/dev/null || true
        return 1
    fi

    comp_failure_reset
    log "kiosk session healthy: duduclaw-comp (pid $comp_pid) + duduclaw-shell (pid $shell_pid)"

    rc=0
    wait "$shell_pid" || rc=$?
    log "duduclaw-shell exited (status $rc) — stopping duduclaw-comp"
    kill "$comp_pid" 2>/dev/null || true
    wait "$comp_pid" 2>/dev/null || true
    exit "$rc"
}

if [[ "$KIOSK_APP" == "comp" ]]; then
    if run_comp_session; then
        # run_comp_session never returns 0 — it exits on the healthy path.
        # Reaching here would mean the contract changed; fail loudly rather
        # than silently leaving the screen blank.
        log "BUG: run_comp_session returned success; falling back to cage"
    fi
    comp_failure_bump
    log "falling back to the cage session (giving the DRM device a moment to be released)"
    sleep 1
    KIOSK_APP=cage
fi

if [[ "$KIOSK_APP" == "cage" ]]; then
    log "starting cage + duduclaw-shell"
    exec cage -d -- env DUDUCLAW_HOME="$KIOSK_HOME" "$SHELL_BIN"
fi

log "starting cage + Chromium dashboard kiosk"
exec cage -d -- chromium \
    --kiosk \
    --ozone-platform=wayland \
    --no-first-run \
    --noerrdialogs \
    --hide-crash-restore-bubble \
    --disable-component-update \
    --user-data-dir=/data/duduclaw-kiosk/chromium-profile \
    http://127.0.0.1:18789
