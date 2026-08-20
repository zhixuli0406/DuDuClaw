#!/usr/bin/env bash
# mkosi PostInstallationScripts= entry. Runs on the HOST with the image at
# $BUILDROOT; the mutations below are wrapped in a single `mkosi-chroot` so
# they act on the image. (mkosi auto-roots useradd/groupadd to $BUILDROOT
# but NOT systemctl — a bare `systemctl enable` here targets the build host,
# which is why enabling systemd-timesyncd.service failed with "Unit does not
# exist" before this wrapping. `systemctl enable` needs no running PID 1
# inside the chroot; it only manipulates unit-file symlinks.)
#
# (1) Creates the `duduclaw` system user the gateway service runs as.
#     Static UID 1000 to match container/Dockerfile.server's convention
#     (`useradd -r -g duduclaw -u 1000 duduclaw`) — kept identical so /data
#     ownership is consistent for anyone who's used the container image
#     before, and so a future container<->appliance data migration doesn't
#     need a uid remap. -r alone (no fixed uid) would risk a different
#     dynamically-assigned uid on the next mkosi run, and DynamicUser=yes
#     in the unit was rejected for the same reason: it would mint a new
#     random uid on every gateway restart, breaking persisted /data
#     ownership across reboots.
#
# (2) Creates the `duduclaw-kiosk` system user the detection-gated kiosk
#     display session runs as — see the comment right above that
#     useradd call below for the full "why these exact groups" reasoning.
#
# (3) Explicitly enables every unit this image depends on, rather than
#     trusting each package's own default preset. Checked and NOT assumed:
#     Debian's nftables package does not enable nftables.service by default
#     (deliberately, to avoid a package install silently locking out the
#     admin), and Debian overrides systemd upstream's own default-enable
#     preset for systemd-networkd/systemd-resolved with "disable" (Debian
#     defaults to ifupdown/NetworkManager, neither of which this image
#     installs). Rather than lean on assumptions about avahi-daemon/
#     docker.io/systemd-timesyncd's presumed default-enabled state too,
#     every unit this image actually needs is listed here explicitly —
#     one source of truth, no per-package guessing.
#
set -euo pipefail

mkosi-chroot bash -s <<'CHROOT_SETUP'
set -euo pipefail

echo "[postinst] creating duduclaw system user"
groupadd -r duduclaw
useradd -r -g duduclaw -u 1000 -d /data/duduclaw -s /usr/sbin/nologin duduclaw

# (2) Creates the `duduclaw-kiosk` system user the detection-gated kiosk
#     display session runs as (see mkosi.extra/etc/systemd/system/
#     duduclaw-kiosk.service). Same "home dir on /data, not created here"
#     reasoning as duduclaw above — root is read-only, and /data isn't
#     populated until duduclaw-firstboot-provision.sh runs on real
#     hardware, which is why that script (not this one) does the
#     mkdir+chown. No static UID unlike duduclaw: nothing needs a stable
#     uid for this user across image rebuilds, its entire $HOME is
#     disposable browser cache/profile state.
#
#     -G video,render: BOTH verified as actually required on Debian
#     trixie specifically, not guessed —
#       - video:  Debian's own seatd packaging (debian/seatd.service,
#                 debian/seatd.default) runs `seatd -g video` ("Allow
#                 access to video group members"), not upstream seatd's
#                 contrib unit, which uses a "seat" group instead. video
#                 group membership is what lets this user connect to
#                 seatd's socket at all — see duduclaw-kiosk.service's
#                 own comment for the full device-access chain.
#       - render: Debian's systemd package is built with
#                 -Dgroup-render-mode=0660 (debian/rules, systemd source
#                 package, trixie 257.13-1~deb13u1) — NOT upstream
#                 systemd's own default of 0666 (world-accessible). Without
#                 this group, Chromium's GPU process cannot open
#                 /dev/dri/renderD* directly (that device is opened by the
#                 client itself, unlike the DRM "card" device which seatd
#                 brokers over its socket).
#     Both groups are standard Debian base-system groups (created by
#     base-passwd / udev respectively), not something this image invents.
echo "[postinst] creating duduclaw-kiosk system user"
groupadd -r duduclaw-kiosk
useradd -r -g duduclaw-kiosk -G video,render -d /data/duduclaw-kiosk \
    -s /usr/sbin/nologin -c "DuDuClaw kiosk display session" duduclaw-kiosk

echo "[postinst] enabling core units"
systemctl enable \
    systemd-networkd.service \
    systemd-resolved.service \
    systemd-timesyncd.service \
    nftables.service \
    avahi-daemon.service \
    docker.service \
    seatd.service \
    duduclaw-firstboot-repart.service \
    duduclaw-firstboot-provision.service \
    duduclaw-sysd.service \
    duduclaw-gateway.service \
    duduclaw-usb-install.service \
    duduclaw-kiosk.service

# Mask systemd-networkd-wait-online.service outright (2026-08-19). Removing it
# from the enable list above is not enough: the Debian networkd package ships
# a preset that re-pulls wait-online into network-online.target, and under
# QEMU's virtio-net it blocks ~120s before *failing*, stalling the whole boot
# (measured: login prompt slips from ~13s to ~135s). The gateway binds 0.0.0.0
# and its channels/relay reconnect on their own with backoff, so nothing here
# needs to wait for "fully online" — masking it is the verified fast-boot path.
echo "[postinst] masking systemd-networkd-wait-online.service (fast boot)"
systemctl mask systemd-networkd-wait-online.service
CHROOT_SETUP
