#!/usr/bin/env bash
# Self-install USB -> internal NVMe. Every boot; safe no-op unless ALL of
# these hold:
#   1. this boot's root disk is on a removable (USB) bus, AND
#   2. exactly one internal (non-removable) NVMe disk is present, AND
#   3. EITHER that NVMe already carries a DuDuClaw OS partition label
#      (unattended upgrade re-flash — the disk is already ours, so
#      re-flashing it needs no extra confirmation) OR the kernel cmdline
#      carries `duduclaw.install=yes` (explicit unattended blank-disk
#      install).
#
# No interactive "are you sure" UI exists yet (that needs either a
# local-display/kiosk mode or a dashboard-served confirmation page —
# neither exists in this image-engineering-only round). A blank target
# disk WITHOUT the cmdline flag is therefore a hard no-op, not a prompt:
# this script has no display to prompt on, and writing to a disk it can't
# confirm intent for is exactly the failure this double-confirmation
# requirement exists to prevent. Fail closed, log, and move on.
set -euo pipefail

log() { echo "duduclaw-usb-install: $*"; }

# --- 1. am I booted from removable media? --------------------------------
ROOT_SOURCE="$(findmnt -n -o SOURCE / || true)"
if [[ -z "$ROOT_SOURCE" ]]; then
    log "could not resolve the root filesystem source, skipping"
    exit 0
fi
BOOT_DISK="$(lsblk -no PKNAME "$ROOT_SOURCE" 2>/dev/null | head -n1)"
if [[ -z "$BOOT_DISK" ]]; then
    log "could not resolve the boot disk, skipping"
    exit 0
fi
BOOT_REMOVABLE="$(cat "/sys/block/${BOOT_DISK}/removable" 2>/dev/null || echo 0)"
if [[ "$BOOT_REMOVABLE" != "1" ]]; then
    log "boot disk /dev/${BOOT_DISK} is not removable — ordinary boot, nothing to do"
    exit 0
fi

# --- 2. is there exactly one internal NVMe present? -----------------------
mapfile -t NVME_CANDIDATES < <(
    for dev in /sys/block/nvme*n1; do
        [[ -e "$dev" ]] || continue
        name="$(basename "$dev")"
        removable="$(cat "$dev/removable" 2>/dev/null || echo 0)"
        [[ "$removable" == "0" ]] && echo "$name"
    done
)
if [[ "${#NVME_CANDIDATES[@]}" -eq 0 ]]; then
    log "no internal NVMe disk found, nothing to install onto"
    exit 0
fi
if [[ "${#NVME_CANDIDATES[@]}" -gt 1 ]]; then
    log "WARNING: ${#NVME_CANDIDATES[@]} internal NVMe disks found (${NVME_CANDIDATES[*]}) — ambiguous target, refusing to guess. Certified target hardware ships with exactly one."
    exit 0
fi
TARGET_DEV="${NVME_CANDIDATES[0]}"

# --- 3. decide: upgrade (existing DuDuClaw labels) vs. explicit install flag
HAS_DUDUCLAW_LABEL=0
if lsblk -no PARTLABEL "/dev/${TARGET_DEV}" 2>/dev/null | grep -q '^duduclaw-'; then
    HAS_DUDUCLAW_LABEL=1
fi

INSTALL_FLAG=0
if grep -qw 'duduclaw.install=yes' /proc/cmdline 2>/dev/null; then
    INSTALL_FLAG=1
fi

if [[ "$HAS_DUDUCLAW_LABEL" -eq 1 ]]; then
    log "/dev/${TARGET_DEV} already carries DuDuClaw OS partition labels — proceeding as an unattended re-flash/upgrade"
elif [[ "$INSTALL_FLAG" -eq 1 ]]; then
    log "/dev/${TARGET_DEV} is blank of DuDuClaw labels, but duduclaw.install=yes was passed on the kernel cmdline — proceeding as an explicit unattended install"
else
    log "/dev/${TARGET_DEV} has no DuDuClaw OS labels and duduclaw.install=yes was not set on the kernel cmdline."
    log "Refusing to write to it without explicit confirmation. To install unattended, add duduclaw.install=yes to the boot entry's kernel cmdline and reboot from this USB stick."
    exit 0
fi

# --- 4. write: dd this boot disk's whole-disk image onto the NVMe target --
# The USB stick IS the golden mkosi disk image (written on with a tool like
# Etcher) — dd'ing the whole source block device reproduces the identical
# ESP + root-a + root-b + (small) data partition layout onto the NVMe. The
# next boot from NVMe runs duduclaw-firstboot-repart.service, which grows
# /data to fill whatever space the NVMe has beyond that.
log "writing /dev/${BOOT_DISK} -> /dev/${TARGET_DEV} ..."
dd if="/dev/${BOOT_DISK}" of="/dev/${TARGET_DEV}" bs=4M conv=fsync,noerror status=progress
sync
# partx (util-linux, already in the base image — no extra package needed,
# unlike `parted`'s partprobe) re-reads the partition table on the target.
partx -u "/dev/${TARGET_DEV}" 2>/dev/null || true

log "install complete — powering off. Remove the USB stick, then power the machine back on."
systemctl poweroff
