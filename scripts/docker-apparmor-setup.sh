#!/usr/bin/env bash
# Enable bubblewrap user namespaces on hosts with Ubuntu's userns restriction.
set -euo pipefail

if [[ "$(uname -s)" != Linux ]]; then
  echo 'Nothing needed: this host is not Linux.'
  exit 0
fi
restriction=/proc/sys/kernel/apparmor_restrict_unprivileged_userns
if [[ ! -r "$restriction" ]] || [[ "$(cat "$restriction")" != 1 ]]; then
  echo 'Nothing needed: restricted unprivileged user namespaces are not enabled.'
  exit 0
fi
if [[ $EUID -ne 0 ]]; then
  echo 'Run this script as root: sudo scripts/docker-apparmor-setup.sh' >&2
  exit 1
fi
if ! command -v apparmor_parser >/dev/null 2>&1; then
  echo 'apparmor_parser is required; install the host AppArmor tools first.' >&2
  exit 1
fi
if [[ ! -f /etc/apparmor.d/abi/4.0 ]]; then
  echo 'Missing /etc/apparmor.d/abi/4.0; this host cannot load the required AppArmor ABI 4.0 profile.' >&2
  exit 1
fi
repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
install -m 0644 "$repo_root/container/apparmor/duduclaw-userns" /etc/apparmor.d/duduclaw-userns
apparmor_parser -r /etc/apparmor.d/duduclaw-userns
if ! awk '$1 == "duduclaw-userns" { found = 1 } END { exit !found }' /sys/kernel/security/apparmor/profiles; then
  echo 'AppArmor did not report duduclaw-userns as loaded.' >&2
  exit 1
fi
echo 'Add this line to .env, then recreate the container with docker compose up -d:'
echo 'DUDUCLAW_APPARMOR_PROFILE=duduclaw-userns'
