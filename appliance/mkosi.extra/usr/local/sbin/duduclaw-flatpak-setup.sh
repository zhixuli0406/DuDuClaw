#!/usr/bin/env bash
# ExecStart= for duduclaw-flatpak-setup.service — brings the /data-resident
# Flatpak installation into a usable state.
#
# Two jobs, deliberately split by whether they need the network:
#
#   1. Create /data/flatpak. Offline, idempotent, runs every boot. The
#      installation is DECLARED statically at
#      /etc/flatpak/installations.d/10-duduclaw-data.conf (image content —
#      it has to exist before the first `flatpak install` or the repository
#      lands on the read-only root), but the directory itself cannot be:
#      /data is an empty partition until first boot.
#
#   2. Add the flathub remote. NEEDS THE NETWORK, and that is exactly why
#      this is a boot-time service and not a mkosi postinst step: mkosi's
#      postinst sandbox has no outbound network (the same constraint that
#      already forces the AI CLIs through appliance/Dockerfile.cli-vendor —
#      see mkosi.conf's own comment). Nor can it be a static file: a remote
#      is stored INSIDE the installation's own OSTree repo config, which
#      lives on /data and therefore does not exist at image build time.
#      So it is done here, with retries, guarded by a stamp file, and it is
#      never fatal: an appliance that boots before its uplink is up simply
#      picks the remote up on a later boot.
#
# Nothing here ever installs an app. Choosing and installing apps is a
# user-facing decision made through DuDuClaw, not something an image build
# or a boot script gets to make on the operator's behalf.
set -euo pipefail

INSTALLATION=data
INSTALL_PATH=/data/flatpak
STAMP="$INSTALL_PATH/.duduclaw-flathub-added"
FLATHUB_URL=https://flathub.org/repo/flathub.flatpakrepo
RETRIES="${DUDUCLAW_FLATPAK_SETUP_RETRIES:-5}"
RETRY_SLEEP="${DUDUCLAW_FLATPAK_SETUP_RETRY_SLEEP:-10}"

log() { echo "duduclaw-flatpak-setup: $*"; }

if ! command -v flatpak >/dev/null 2>&1; then
    log "flatpak is not installed — nothing to do"
    exit 0
fi

# --- 1. installation directory -------------------------------------------
# Refuse to proceed if /data is not actually mounted. The unit already
# declares Requires=/After=data.mount, but a `mkdir -p /data/flatpak` that
# silently landed on the ROOT filesystem instead is precisely the failure
# this whole file exists to prevent — so it is checked, not assumed.
if ! mountpoint -q /data; then
    log "/data is not mounted — refusing to create $INSTALL_PATH on the root filesystem"
    exit 0
fi

mkdir -p "$INSTALL_PATH"
chmod 0755 "$INSTALL_PATH"

# --- 2. session-wide environment for sandboxed apps -----------------------
# XDG_SESSION_TYPE is a FACT about this appliance (every kiosk session is a
# Wayland compositor — see duduclaw-kiosk-launch.sh), and it is the signal
# well-behaved toolkits check. It is deliberately the ONLY thing set here:
#   - GDK_BACKEND / QT_QPA_PLATFORM are NOT set. Forcing a toolkit backend
#     turns "this app quietly fell back" into "this app cannot start", and
#     that is a per-app policy call, not an image-wide one.
#   - Chromium-family apps are NOT fixed by this. Measured first-hand
#     (research/native-os-2026-08/flatpak-portal-scope-2026-08.md §3.2③):
#     with WAYLAND_DISPLAY already exported, Flathub's Chromium still chose
#     the X11 ozone backend and failed; it only worked with an explicit
#     `--ozone-platform=wayland` on the command line. That is ARGV, and no
#     env var expresses it — which is why the durable fix belongs to
#     whatever launches the app (DuDuClaw's own launcher, which can hold a
#     per-app-id argv policy), NOT to an edited .desktop file inside the
#     app (destroyed by the next `flatpak update`).
#
# WHY THIS FILE IS WRITTEN BY HAND INSTEAD OF VIA `flatpak override`:
# because `flatpak override` cannot target a named installation. Measured
# on flatpak 1.16.6 (Debian trixie, 2026-08-22), with the installation's
# repo already initialized, in BOTH documented option positions
# (`flatpak override --installation=data --env=...` and
# `flatpak --installation=data override --env=...`): it exits 0, prints no
# warning, `--show` reads the value straight back — and the bytes land in
# /var/lib/flatpak/overrides/global, i.e. the DEFAULT system installation
# on the read-only root, which no app installed under /data ever reads.
# Silently writing to the wrong installation is worse than not writing at
# all, so the CLI is bypassed. The file format is flatpak's own (this is
# byte-for-byte what the command produced in that same test).
#
# Create-if-absent, never rewrite: once this file exists it may carry
# operator or product customization, and a boot script has no business
# clobbering it.
OVERRIDE_FILE="$INSTALL_PATH/overrides/global"
if [[ ! -e "$OVERRIDE_FILE" ]]; then
    mkdir -p "$INSTALL_PATH/overrides"
    cat > "$OVERRIDE_FILE" <<'EOF'
[Environment]
XDG_SESSION_TYPE=wayland
EOF
    log "wrote default sandbox environment to $OVERRIDE_FILE"
fi

# --- 3. flathub remote ----------------------------------------------------
if [[ -e "$STAMP" ]]; then
    log "flathub remote already configured ($STAMP) — done"
    exit 0
fi

for (( attempt = 1; attempt <= RETRIES; attempt++ )); do
    if flatpak remote-add --installation="$INSTALLATION" --if-not-exists \
        flathub "$FLATHUB_URL"; then
        : > "$STAMP"
        log "flathub remote added to the '$INSTALLATION' installation at $INSTALL_PATH"
        exit 0
    fi
    log "attempt $attempt/$RETRIES to reach $FLATHUB_URL failed"
    if (( attempt < RETRIES )); then
        sleep "$RETRY_SLEEP"
    fi
done

# Non-fatal on purpose: no uplink at boot is a normal state for an
# appliance, and a red unit in `systemctl status` for it would be noise.
# RemainAfterExit=yes is deliberately NOT set on this unit, so the next
# boot retries.
log "giving up for this boot — no flathub remote yet; will retry on the next boot"
log "  install apps with: flatpak --installation=$INSTALLATION install flathub <app-id>"
exit 0
