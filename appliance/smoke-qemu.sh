#!/usr/bin/env bash
# QEMU/OVMF UEFI boot smoke test for the built appliance image.
#
# Boots the mkosi.output/*.raw disk image with software-emulated UEFI
# firmware (OVMF/edk2) under QEMU. Architecture is selected via
# APPLIANCE_ARCH (default x86-64, matching build.sh's default and the
# shipping target):
#   - x86-64: qemu-system-x86_64, `q35` machine, TCG software CPU
#     emulation. TCG (rather than HVF/KVM hardware acceleration) is
#     expected and fine for a smoke test of the shipping target — it's
#     what makes this runnable the same way on any build host, including
#     cross-architecture (an x86-64 appliance image on an Apple Silicon
#     host can only ever use TCG here; HVF only accelerates
#     same-architecture guests).
#   - arm64: qemu-system-aarch64, `virt` machine. On an Apple Silicon host
#     this uses `-accel hvf` (native hardware acceleration — this is the
#     APPLIANCE_ARCH=arm64 use case: a fast local smoke test without
#     cross-arch emulation, see appliance/README.md); everywhere else it
#     falls back to TCG, same tradeoff as the x86-64 path.
#
# Runs on the HOST directly, not inside Docker — only the mkosi *build*
# (build.sh) needs the Linux container; QEMU itself needs actual
# virtualization/emulation access that a nested container adds nothing for.
#
# What "smoke" means here: boots far enough to reach multi-user.target,
# and duduclaw-gateway.service was at least attempted (started or failed
# — either proves the unit is wired up; a real pass/fail on the *gateway
# actually working* needs the real binary + real credentials, out of
# scope for this scaffold). NOT a real-hardware validation — that's a
# separate, later step once a certified physical machine is available.
#
# Usage:
#   appliance/smoke-qemu.sh [path/to/duduclaw-os.raw]
#   (defaults to mkosi.output/duduclaw-os.raw, mkosi's own Output=/
#   OutputDirectory= naming from mkosi.conf)
#   APPLIANCE_ARCH=arm64 appliance/smoke-qemu.sh   # boot an arm64-built image instead
set -euo pipefail

APPLIANCE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IMAGE="${1:-$APPLIANCE_DIR/mkosi.output/duduclaw-os.raw}"
# 300s default: TCG (software emulation, the arm64-on-non-arm64 and every
# x86-64 case) takes ~2min of userspace to reach multi-user for this image.
# HVF-accelerated runs finish far sooner; the poll loop exits as soon as the
# markers appear, so a generous ceiling costs nothing on fast runs.
BOOT_TIMEOUT="${BOOT_TIMEOUT:-300}"
APPLIANCE_ARCH="${APPLIANCE_ARCH:-x86-64}"

if [[ ! -f "$IMAGE" ]]; then
    echo "[smoke-qemu] image not found: $IMAGE" >&2
    echo "[smoke-qemu] run appliance/build.sh first, or pass an explicit path" >&2
    exit 1
fi

case "$APPLIANCE_ARCH" in
    x86-64)
        QEMU_BIN=qemu-system-x86_64
        QEMU_MACHINE=q35
        QEMU_CPU=max
        QEMU_ACCEL=tcg
        ;;
    arm64)
        QEMU_BIN=qemu-system-aarch64
        QEMU_MACHINE=virt
        # HVF only accelerates same-architecture guests (Apple Silicon host
        # running an arm64 guest). Detect the host via sysctl, NOT
        # `uname -m`: under a Rosetta/x86_64 process context (which is how
        # this script can end up running) `uname -m` reports "x86_64" on an
        # Apple Silicon machine and wrongly forces the slow TCG path —
        # `sysctl -n hw.optional.arm64` returns 1 regardless of the calling
        # process's own architecture. QEMU_ACCEL/QEMU_CPU env overrides win.
        if [[ -n "${QEMU_ACCEL:-}" ]]; then
            QEMU_CPU="${QEMU_CPU:-host}"
        elif [[ "$(uname -s)" == "Darwin" && "$(sysctl -n hw.optional.arm64 2>/dev/null)" == "1" ]]; then
            QEMU_CPU="${QEMU_CPU:-host}"
            QEMU_ACCEL=hvf
        else
            QEMU_CPU="${QEMU_CPU:-max}"
            QEMU_ACCEL=tcg
        fi
        ;;
    *)
        echo "[smoke-qemu] unsupported APPLIANCE_ARCH=$APPLIANCE_ARCH (expected x86-64 or arm64)" >&2
        exit 1
        ;;
esac
echo "[smoke-qemu] target arch: $APPLIANCE_ARCH ($QEMU_BIN, machine=$QEMU_MACHINE, accel=$QEMU_ACCEL)"

if ! command -v "$QEMU_BIN" >/dev/null 2>&1; then
    echo "[smoke-qemu] $QEMU_BIN not found on PATH. Install it first, e.g.:" >&2
    echo "[smoke-qemu]   brew install qemu                (macOS, both architectures)" >&2
    echo "[smoke-qemu]   apt-get install qemu-system-x86   (Debian/Ubuntu, x86-64 target)" >&2
    echo "[smoke-qemu]   apt-get install qemu-system-arm   (Debian/Ubuntu, arm64 target)" >&2
    exit 1
fi

# --- locate OVMF/edk2 UEFI firmware ------------------------------------
# Exact filenames genuinely vary across distros/Homebrew versions. The
# macOS/Homebrew candidates below were confirmed by actually listing
# `$(brew --prefix qemu)/share/qemu/` after `brew install qemu` (11.1.0,
# arm64_tahoe bottle, 2026-08) rather than guessed — that directory ships
# edk2-x86_64-code.fd + edk2-i386-vars.fd for the x86-64 path (unchanged
# from before this file supported arm64) and edk2-aarch64-code.fd +
# edk2-arm-vars.fd for the arm64 path; there is no edk2-aarch64-vars.fd —
# aarch64 code pairs with the arm-vars template, mirroring how x86_64 code
# already paired with the i386-vars template. The Linux/apt paths are
# best-effort (Debian/Ubuntu's qemu-efi-aarch64 package installs AAVMF
# under /usr/share/AAVMF/ — this specific path was NOT independently
# confirmed this round, same "not live-tested" status as the pre-existing
# x86-64 /usr/share/OVMF/* candidates — see README.md "known open points"),
# so this tries several candidates instead of asserting one.
if [[ "$APPLIANCE_ARCH" == "x86-64" ]]; then
    OVMF_CODE_CANDIDATES=(
        "${OVMF_CODE:-}"
        "/usr/share/OVMF/OVMF_CODE_4M.fd"
        "/usr/share/OVMF/OVMF_CODE.fd"
        "/usr/share/edk2/ovmf/OVMF_CODE.fd"
        "$(command -v brew >/dev/null 2>&1 && echo "$(brew --prefix qemu 2>/dev/null)/share/qemu/edk2-x86_64-code.fd" || true)"
    )
    OVMF_VARS_CANDIDATES=(
        "${OVMF_VARS:-}"
        "/usr/share/OVMF/OVMF_VARS_4M.fd"
        "/usr/share/OVMF/OVMF_VARS.fd"
        "/usr/share/edk2/ovmf/OVMF_VARS.fd"
        "$(command -v brew >/dev/null 2>&1 && echo "$(brew --prefix qemu 2>/dev/null)/share/qemu/edk2-i386-vars.fd" || true)"
    )
else
    # Derive the firmware dir from the ACTUAL qemu binary being run
    # (<bindir>/../share/qemu), which is authoritative and survives a
    # dual-Homebrew host where `brew --prefix qemu` points at the wrong
    # (Intel /usr/local) tree while the working qemu is under /opt/homebrew.
    QEMU_SHARE="$(cd "$(dirname "$(command -v "$QEMU_BIN")")/../share/qemu" 2>/dev/null && pwd || true)"
    OVMF_CODE_CANDIDATES=(
        "${OVMF_CODE:-}"
        "${QEMU_SHARE:+$QEMU_SHARE/edk2-aarch64-code.fd}"
        "/opt/homebrew/share/qemu/edk2-aarch64-code.fd"
        "/usr/local/share/qemu/edk2-aarch64-code.fd"
        "/usr/share/AAVMF/AAVMF_CODE.fd"
        "/usr/share/qemu-efi-aarch64/QEMU_EFI.fd"
    )
    OVMF_VARS_CANDIDATES=(
        "${OVMF_VARS:-}"
        "${QEMU_SHARE:+$QEMU_SHARE/edk2-arm-vars.fd}"
        "/opt/homebrew/share/qemu/edk2-arm-vars.fd"
        "/usr/local/share/qemu/edk2-arm-vars.fd"
        "/usr/share/AAVMF/AAVMF_VARS.fd"
    )
fi

OVMF_CODE_PATH=""
for c in "${OVMF_CODE_CANDIDATES[@]}"; do
    [[ -n "$c" && -f "$c" ]] && { OVMF_CODE_PATH="$c"; break; }
done
OVMF_VARS_TEMPLATE=""
for c in "${OVMF_VARS_CANDIDATES[@]}"; do
    [[ -n "$c" && -f "$c" ]] && { OVMF_VARS_TEMPLATE="$c"; break; }
done

if [[ -z "$OVMF_CODE_PATH" || -z "$OVMF_VARS_TEMPLATE" ]]; then
    echo "[smoke-qemu] could not locate OVMF/edk2 firmware for $APPLIANCE_ARCH. Set OVMF_CODE and OVMF_VARS explicitly:" >&2
    echo "[smoke-qemu]   OVMF_CODE=/path/CODE.fd OVMF_VARS=/path/VARS.fd appliance/smoke-qemu.sh" >&2
    exit 1
fi
echo "[smoke-qemu] OVMF_CODE: $OVMF_CODE_PATH"
echo "[smoke-qemu] OVMF_VARS: $OVMF_VARS_TEMPLATE"

WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT
OVMF_VARS_COPY="$WORKDIR/OVMF_VARS.fd"
cp "$OVMF_VARS_TEMPLATE" "$OVMF_VARS_COPY"
SERIAL_LOG="$WORKDIR/serial.log"

echo "[smoke-qemu] booting $IMAGE (timeout ${BOOT_TIMEOUT}s)..."
"$QEMU_BIN" \
    -name duduclaw-os-smoke \
    -machine "$QEMU_MACHINE" \
    -accel "$QEMU_ACCEL" \
    -cpu "$QEMU_CPU" \
    -m 2048 \
    -smp 2 \
    -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE_PATH" \
    -drive if=pflash,format=raw,file="$OVMF_VARS_COPY" \
    -drive file="$IMAGE",format=raw,if=virtio \
    -netdev user,id=net0 -device virtio-net-pci,netdev=net0 \
    -nographic \
    -serial file:"$SERIAL_LOG" \
    -display none \
    -no-reboot \
    &
QEMU_PID=$!

# --- poll the serial log for our two pass markers ---------------------
DEADLINE=$(( $(date +%s) + BOOT_TIMEOUT ))
REACHED_MULTI_USER=0
GATEWAY_ATTEMPTED=0
while [[ "$(date +%s)" -lt "$DEADLINE" ]]; do
    if ! kill -0 "$QEMU_PID" 2>/dev/null; then
        echo "[smoke-qemu] qemu exited early — see $SERIAL_LOG" >&2
        break
    fi
    if [[ -f "$SERIAL_LOG" ]]; then
        # Detection must work with the shipping cmdline (systemd.show_status=
        # auto), which does NOT print a "Reached target multi-user" or
        # "Started ..." line for every unit — so match signals that always
        # appear regardless of log verbosity:
        #   - multi-user reached  ⇒ the getty login prompt "<host> login:"
        #     (getty is pulled in by multi-user.target), OR the explicit
        #     "Reached target ... multi-user" line when boot IS verbose.
        #   - gateway attempted    ⇒ the kernel audit record for the unit
        #     (audit always goes to the console via kmsg, independent of
        #     systemd's own status output), OR the verbose "Started" text.
        grep -Eq "Reached target .*[Mm]ulti-[Uu]ser|[[:alnum:]_-]+ login:" "$SERIAL_LOG" 2>/dev/null && REACHED_MULTI_USER=1
        grep -Eq "unit=duduclaw-gateway .*res=(success|failed)|duduclaw-gateway\.service.*(Deactivated|Failed|Main process exited)|(Started|Starting) [Dd]udu[Cc]law[- ]gateway" "$SERIAL_LOG" 2>/dev/null && GATEWAY_ATTEMPTED=1
    fi
    if [[ "$REACHED_MULTI_USER" -eq 1 && "$GATEWAY_ATTEMPTED" -eq 1 ]]; then
        break
    fi
    sleep 2
done

kill "$QEMU_PID" 2>/dev/null || true
wait "$QEMU_PID" 2>/dev/null || true

echo "[smoke-qemu] --- serial log tail ---"
tail -n 60 "$SERIAL_LOG" 2>/dev/null || echo "(no serial log captured)"
echo "[smoke-qemu] -----------------------"

if [[ "$REACHED_MULTI_USER" -eq 1 ]]; then
    echo "[smoke-qemu] PASS: reached multi-user.target"
else
    echo "[smoke-qemu] FAIL: never reached multi-user.target within ${BOOT_TIMEOUT}s" >&2
fi
if [[ "$GATEWAY_ATTEMPTED" -eq 1 ]]; then
    echo "[smoke-qemu] PASS: duduclaw-gateway.service was attempted"
else
    echo "[smoke-qemu] FAIL: no sign duduclaw-gateway.service was ever started" >&2
fi

[[ "$REACHED_MULTI_USER" -eq 1 && "$GATEWAY_ATTEMPTED" -eq 1 ]]
