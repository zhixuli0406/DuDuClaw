#!/usr/bin/env bash
# Boot the built DuDuClaw OS image as a LONG-LIVED, interactive test VM you can
# reach from your host browser — unlike smoke-qemu.sh, which boots once to
# multi-user and exits. State persists in a working copy of the disk, so the
# admin account / config / agents you set up survive a reboot.
#
# Two UI/UX test surfaces:
#   1. THE DASHBOARD / CONVERSATIONAL CONSOLE (primary) — forwarded to
#      http://localhost:${DASH_PORT} (default 18789). Open it in your Mac
#      browser: the web UI renders NATIVELY on your host, so CSS / animation /
#      layout smoothness is your Mac's real fidelity — this is the right
#      surface to judge design + fluidity. The VM only serves data + backend.
#      Works in the default headless mode.
#   2. THE ON-BOX KIOSK (secondary) — the full-screen Chromium the appliance
#      shows on an attached display. Needs a graphical window: run with
#      VM_DISPLAY=gui to attach a virtio-gpu + a QEMU window. (Experimental:
#      whether the detection-gated kiosk auto-starts under QEMU's virtual GPU
#      depends on the guest seeing a "connected" DRM output; if it doesn't,
#      you still get the login console in the window.)
#
# Usage:
#   appliance/run-vm.sh                 # headless; dashboard → localhost (browser test)
#   VM_DISPLAY=gui appliance/run-vm.sh  # + graphical window (see the on-box kiosk)
#   DASH_PORT=8080 appliance/run-vm.sh  # forward the dashboard to a different host port
#   VM_ACCEL=tcg appliance/run-vm.sh    # force software emulation (slower, most compatible)
#   VM_MEM=6144 VM_SMP=6 appliance/run-vm.sh   # give it more RAM/CPUs for a snappier backend
#   VM_FRESH=1 appliance/run-vm.sh      # discard the persistent working copy and start clean
#
# Stop it: Ctrl-A then X (headless serial) / close the window (gui), or
# `pkill -f duduclaw-os-vm`.
set -euo pipefail

APPLIANCE_ARCH="${APPLIANCE_ARCH:-arm64}"
DASH_PORT="${DASH_PORT:-18789}"
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC="${VM_IMAGE:-$DIR/mkosi.output/duduclaw-os.raw}"
WORK_DIR="$DIR/.vm"
DISK="$WORK_DIR/duduclaw-os-vm.raw"
MEM="${VM_MEM:-4096}"
SMP="${VM_SMP:-4}"

[[ -f "$SRC" ]] || { echo "[run-vm] image not found: $SRC — build it first (appliance/build.sh)" >&2; exit 1; }

mkdir -p "$WORK_DIR"
# Persistent working copy: copy once (APFS keeps it sparse), reuse thereafter so
# host reboots / VM reboots don't lose the appliance's /data state.
if [[ -n "${VM_FRESH:-}" || ! -f "$DISK" ]]; then
    echo "[run-vm] preparing a fresh persistent working image (this copies the built image once)..."
    rm -f "$DISK"
    cp -c "$SRC" "$DISK" 2>/dev/null || cp "$SRC" "$DISK"   # -c = APFS clone (instant, still CoW-sparse)
fi

# --- firmware + machine, per architecture ---------------------------------
QEMU_SHARE="$(cd "$(dirname "$(command -v qemu-system-aarch64 2>/dev/null || echo /usr/bin/false)")/../share/qemu" 2>/dev/null && pwd || true)"
case "$APPLIANCE_ARCH" in
  arm64)
    QEMU_BIN=qemu-system-aarch64
    MACHINE=virt
    # HVF only helps an arm64 guest on an Apple Silicon host — detect via
    # sysctl (survives a Rosetta/x86_64 process context, unlike `uname -m`).
    if [[ -z "${VM_ACCEL:-}" ]]; then
        if [[ "$(uname -s)" == "Darwin" && "$(sysctl -n hw.optional.arm64 2>/dev/null)" == "1" ]]; then
            VM_ACCEL=hvf
        else
            VM_ACCEL=tcg
        fi
    fi
    [[ "$VM_ACCEL" == "hvf" ]] && CPU=host || CPU=max
    CODE_CANDS=("${OVMF_CODE:-}" "${QEMU_SHARE:+$QEMU_SHARE/edk2-aarch64-code.fd}" /opt/homebrew/share/qemu/edk2-aarch64-code.fd /usr/local/share/qemu/edk2-aarch64-code.fd /usr/share/AAVMF/AAVMF_CODE.fd)
    VARS_CANDS=("${OVMF_VARS:-}" "${QEMU_SHARE:+$QEMU_SHARE/edk2-arm-vars.fd}" /opt/homebrew/share/qemu/edk2-arm-vars.fd /usr/local/share/qemu/edk2-arm-vars.fd /usr/share/AAVMF/AAVMF_VARS.fd)
    ;;
  x86-64)
    QEMU_BIN=qemu-system-x86_64; MACHINE=q35; CPU=max; VM_ACCEL="${VM_ACCEL:-tcg}"
    CODE_CANDS=("${OVMF_CODE:-}" "${QEMU_SHARE:+$QEMU_SHARE/edk2-x86_64-code.fd}" /opt/homebrew/share/qemu/edk2-x86_64-code.fd /usr/share/OVMF/OVMF_CODE.fd)
    VARS_CANDS=("${OVMF_VARS:-}" "${QEMU_SHARE:+$QEMU_SHARE/edk2-i386-vars.fd}" /opt/homebrew/share/qemu/edk2-i386-vars.fd /usr/share/OVMF/OVMF_VARS.fd)
    ;;
  *) echo "[run-vm] unsupported APPLIANCE_ARCH=$APPLIANCE_ARCH" >&2; exit 1 ;;
esac

pick() { for c in "$@"; do [[ -n "$c" && -f "$c" ]] && { echo "$c"; return; }; done; }
CODE="$(pick "${CODE_CANDS[@]}")"; VARS_TMPL="$(pick "${VARS_CANDS[@]}")"
[[ -n "$CODE" && -n "$VARS_TMPL" ]] || { echo "[run-vm] UEFI firmware not found; set OVMF_CODE / OVMF_VARS" >&2; exit 1; }

# Writable varstore, reset from the pristine template on EVERY launch.
# Deliberately NOT persisted across runs (2026-08-19): a varstore that has
# been booted once can end up with a BootOrder that drops QEMU's edk2 into
# the UEFI Interactive Shell instead of booting the disk, while a fresh
# varstore reliably auto-boots via the ESP's removable fallback
# (/EFI/BOOT/BOOTAA64.EFI). We keep no NVRAM state on purpose — everything
# needed to boot lives on the ESP — so wiping it each time costs nothing and
# removes the "second boot lands in the EFI shell" flakiness.
VARS="$WORK_DIR/vars.fd"
cp "$VARS_TMPL" "$VARS"

# Display: headless (serial only, dashboard via the browser) vs a graphical
# window with a virtio GPU so the on-box kiosk / login console is visible.
DISPLAY_ARGS=(-nographic)
if [[ "${VM_DISPLAY:-}" == "gui" ]]; then
    # cocoa on macOS, gtk elsewhere; ramfb+virtio-gpu gives the guest a DRM
    # output the kiosk-detect script can find. Serial still comes to the tty.
    #
    # USB tablet + keyboard (2026-08-19): without an ABSOLUTE pointer device
    # the guest gets a relative PS/2 mouse, whose motion doesn't map to the
    # host cursor position — inside a Wayland kiosk the cursor is unusable
    # (can't click the dashboard). qemu-xhci + usb-tablet gives absolute
    # coordinates (guest cursor tracks the host 1:1, no capture needed) and
    # usb-kbd lets you type into the first-run setup. The arm64 virt machine
    # ships no USB by default, hence the explicit xhci controller.
    HOST_DISPLAY=gtk
    [[ "$(uname -s)" == "Darwin" ]] && HOST_DISPLAY=cocoa
    DISPLAY_ARGS=(
        -device virtio-gpu-pci
        -device qemu-xhci,id=usb -device usb-tablet -device usb-kbd
        -display "$HOST_DISPLAY" -serial mon:stdio
    )
fi

echo "[run-vm] $APPLIANCE_ARCH  accel=$VM_ACCEL  mem=${MEM}M  cpus=$SMP  display=${VM_DISPLAY:-headless}"
echo "[run-vm] dashboard → http://localhost:${DASH_PORT}   (open in a browser; first-run setup happens there)"
[[ "${VM_DISPLAY:-}" == "gui" ]] && echo "[run-vm] a graphical window will open for the on-box kiosk / console"
echo "[run-vm] first boot takes a minute or two; the dashboard port answers once it's up"

exec "$QEMU_BIN" \
  -name duduclaw-os-vm \
  -machine "$MACHINE",accel="$VM_ACCEL" -cpu "$CPU" -smp "$SMP" -m "$MEM" \
  -drive if=pflash,format=raw,readonly=on,file="$CODE" \
  -drive if=pflash,format=raw,file="$VARS" \
  -drive if=virtio,format=raw,file="$DISK" \
  -netdev user,id=net0,hostfwd=tcp::"${DASH_PORT}"-:18789 \
  -device virtio-net-pci,netdev=net0 \
  "${DISPLAY_ARGS[@]}"
