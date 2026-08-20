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

# ── Kiosk app selection (Shell-S2 wave, 2026-08-20) ──────────────────────
# When the image was built with DUDUCLAW_SHELL_BIN_PATH (build.sh step 1c),
# /usr/local/bin/duduclaw-shell exists and the native gpui shell IS the
# kiosk session — that's the DuDuClaw OS product direction (design doc
# DESIGN-native-gui-gpui-2026-08.md §13.5; the Chromium dashboard kiosk
# below remains as the automatic fallback for images built without the
# shell, and as the explicit escape hatch during the shell's rollout).
# Selection order, first match wins:
#   1. /etc/duduclaw/kiosk-app containing the word "chromium" → force the
#      Chromium fallback even when the shell binary exists (operator
#      escape hatch — drop the file from a serial/debug session, restart
#      duduclaw-kiosk.service; no image rebuild needed).
#   2. /usr/local/bin/duduclaw-shell present → run it under cage.
#   3. Chromium dashboard kiosk (byte-identical to the pre-shell behavior).
# Env notes for the shell branch, all verified live in the Shell-S2 VM
# rounds (crates/duduclaw-shell/BUILD-LINUX.md stage B-③):
#   - DUDUCLAW_HOME=$HOME → the shell keeps its OOBE state in
#     $DUDUCLAW_HOME/shell/ on the only writable partition (/data), and
#     its first-run account step claims the local gateway on
#     127.0.0.1:18789 (same build-time port constant as the URL below).
#   - No LIBGL_ALWAYS_SOFTWARE here: that env on CAGE itself makes Mesa
#     refuse ("Not allowed to force software rendering when API explicitly
#     selects a hardware device") and cage segfaults — found the hard way;
#     the shell needs no GL env at all (its gpui renderer speaks Vulkan,
#     lavapipe/hardware selected by the loader on its own).
KIOSK_APP=""
if [[ -r /etc/duduclaw/kiosk-app ]] && grep -qw chromium /etc/duduclaw/kiosk-app; then
    KIOSK_APP=chromium
elif [[ -x /usr/local/bin/duduclaw-shell ]]; then
    KIOSK_APP=shell
fi

if [[ "$KIOSK_APP" == "shell" ]]; then
    exec cage -d -- env DUDUCLAW_HOME="${HOME:-/data/duduclaw-kiosk}" \
        /usr/local/bin/duduclaw-shell
fi

exec cage -d -- chromium \
    --kiosk \
    --ozone-platform=wayland \
    --no-first-run \
    --noerrdialogs \
    --hide-crash-restore-bubble \
    --disable-component-update \
    --user-data-dir=/data/duduclaw-kiosk/chromium-profile \
    http://127.0.0.1:18789
