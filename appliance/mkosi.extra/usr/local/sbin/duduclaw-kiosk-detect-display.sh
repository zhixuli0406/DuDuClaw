#!/usr/bin/env bash
# ExecCondition= for duduclaw-kiosk.service — boot-time-only display
# detection. See that unit file's own header comment for the full design
# rationale; this script is just the "is a monitor actually plugged in"
# predicate it runs.
#
# Exit 0 (condition met — ExecStart proceeds) if ANY DRM connector reports
# "connected". Exit 1 otherwise, including when /sys/class/drm doesn't
# exist at all (no GPU/KMS driver loaded). ExecCondition= semantics
# (systemd.service(5), verified against the upstream manual): an exit code
# in 1-254 skips the remaining ExecStart commands and moves the unit
# active->inactive WITHOUT marking it failed — only a crash or exit 255
# from this script itself would count as a real failure. That is what
# makes "no monitor attached" a clean, silent no-op instead of a boot
# error, and it's what keeps the headless path (the overwhelming majority
# of real deployments — see README.md) completely unaffected: this script
# doesn't touch anything else when it exits 1.
#
# The three possible values of /sys/class/drm/<connector>/status are
# verified straight from kernel source, not taken from secondary docs or
# guessed: drivers/gpu/drm/drm_connector.c, drm_get_connector_status_name()
# returns exactly one of "connected", "disconnected", or "unknown" (the
# catch-all/default branch) — never any other string. "unknown" (detection
# genuinely isn't possible for that connector, e.g. some legacy analog
# outputs that can't be probed) is treated the same as "no display" here —
# this is a "should the kiosk browser even start" boot gate, not a
# best-effort guess at operator intent.
#
# Boot-time detection only, on purpose: a display plugged in after this
# unit has already run (started or skipped) is NOT picked up without an
# explicit restart of duduclaw-kiosk.service — hot-plug re-detection is
# out of scope for this round (see README.md "Explicitly out of scope").
set -euo pipefail

shopt -s nullglob
status_files=(/sys/class/drm/*/status)

for status_file in "${status_files[@]}"; do
    status="$(cat "$status_file" 2>/dev/null || true)"
    if [[ "$status" == "connected" ]]; then
        echo "duduclaw-kiosk-detect-display: ${status_file} = connected — starting kiosk"
        exit 0
    fi
done

echo "duduclaw-kiosk-detect-display: no connected DRM display found — headless boot, skipping kiosk"
exit 1
