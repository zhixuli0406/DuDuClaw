#!/usr/bin/env bash
# Re-evaluate the shipped partition definitions (appliance/mkosi.repart/,
# copied into the image at /usr/lib/repart.d/ via mkosi.conf's ExtraTrees=)
# against the real root disk.
#
# On the golden image itself this is a no-op: every partition already
# matches its definition. On real hardware, after the golden image has
# been written onto a disk larger than it was built for (self-install USB
# -> internal NVMe, or a channel partner's pre-burn), the /data definition
# (mkosi.repart/30-data.conf: highest Weight=, no SizeMaxBytes=) is the one
# that actually grows — it consumes whatever space is left after ESP +
# root-a + root-b's fixed sizes.
#
# CLI syntax verified against systemd-repart(8) (2026-08,
# man.archlinux.org): `systemd-repart [OPTIONS...] [BLOCKDEVICE]`,
# `--definitions=<path>` overrides the default /usr/lib/repart.d search
# path list.
#
# NOTE on root-a's CopyFiles=/:/ (mkosi.repart/20-root-a.conf): re-running
# systemd-repart here happens WHILE the system is booted from that very
# root-a partition. Per repart's documented matching behavior, an existing
# partition that already matches a definition (by type/label/size) is
# resized-if-needed, not recreated — CopyFiles=/Format= only apply when a
# definition results in a genuinely NEW partition. So this should be a
# pure no-op for root-a/root-b (fixed Min==Max, already present) and only
# ever touch /data. That reasoning wasn't verified against an actual repart
# run this round, though — confirming root-a survives untouched is the
# first thing to check on the QEMU smoke pass, before ever running this
# against real hardware.
set -euo pipefail

STAMP=/var/lib/duduclaw/.repart-grown
mkdir -p "$(dirname "$STAMP")"

ROOT_SOURCE="$(findmnt -n -o SOURCE / || true)"
if [[ -z "$ROOT_SOURCE" ]]; then
    echo "duduclaw-firstboot-repart: could not resolve the root filesystem source, skipping" >&2
    touch "$STAMP"
    exit 0
fi

ROOT_DISK="$(lsblk -no PKNAME "$ROOT_SOURCE" 2>/dev/null | head -n1)"
if [[ -z "$ROOT_DISK" ]]; then
    echo "duduclaw-firstboot-repart: could not resolve the parent disk of $ROOT_SOURCE, skipping" >&2
    touch "$STAMP"
    exit 0
fi

echo "duduclaw-firstboot-repart: re-applying /usr/lib/repart.d against /dev/${ROOT_DISK}"
# Best-effort: growing /data on real hardware is a nice-to-have, NOT a
# reason to fail the boot. --dry-run=no makes repart actually resize (its
# default is a no-op dry run). Re-applying the FULL definition set on the
# live boot disk is imperfect (the ESP/root defs carry CopyFiles= that don't
# apply to already-present partitions, but repart still evaluates them) —
# tracked as a known issue; the correct fix is a /data-only grow definition.
# On the golden/QEMU exact-size image there is nothing to grow anyway, so a
# failure here is expected and must not drop the machine to emergency.
if ! systemd-repart --dry-run=no --definitions=/usr/lib/repart.d "/dev/${ROOT_DISK}"; then
    echo "duduclaw-firstboot-repart: systemd-repart did not complete; /data left at its" \
         "current size (image is fully usable). See appliance/README.md known issues." >&2
fi

# Stamp regardless so this doesn't re-run (and re-log the same failure) every
# boot — ConditionPathExists=! on the stamp gates the unit.
touch "$STAMP"
