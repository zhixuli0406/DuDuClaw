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
destination=/etc/apparmor.d/duduclaw-userns
profiles=/sys/kernel/security/apparmor/profiles
if [[ ! -r "$profiles" ]]; then
  echo 'Cannot inspect loaded AppArmor profiles; refusing to change state.' >&2
  exit 1
fi
if ! command -v flock >/dev/null 2>&1; then
  echo 'flock is required to serialize AppArmor profile installation.' >&2
  exit 1
fi
# The lock lives in a root-owned directory and serializes this installer's
# compare/rename/load/rollback sequence with other invocations.
exec 9>/etc/apparmor.d/.duduclaw-userns.lock
flock -x 9

# Keep staging on the destination filesystem for atomic rename. Root creates
# this private directory; never parse or load a user-writable staging file.
umask 077
transaction="$(mktemp -d /etc/apparmor.d/.duduclaw-userns.XXXXXX)"
had_file=0
file_changed=0
load_attempted=0
committed=0
is_loaded() {
  awk '$1 == "duduclaw-userns" { found = 1 } END { exit !found }' "$profiles"
}
is_expected_label() {
  awk '$0 == "duduclaw-userns (unconfined)" { found = 1 } END { exit !found }' "$profiles"
}
cleanup() {
  local result=$?
  trap - EXIT HUP INT TERM
  if [[ $committed -eq 0 ]]; then
    # The previous file, if present, was byte-identical and was never replaced.
    if [[ $load_attempted -eq 1 ]] && is_loaded; then
      if ! apparmor_parser -R "$transaction/candidate" || is_loaded; then
        echo 'ERROR: could not remove the newly loaded AppArmor profile.' >&2
        result=1
      fi
    fi
    if [[ $file_changed -eq 1 ]] && ! rm -f -- "$destination"; then
      echo 'ERROR: could not restore the previous absent profile file.' >&2
      result=1
    fi
  fi
  rm -f -- "$transaction/candidate" "$transaction/install"
  rmdir -- "$transaction"
  exit "$result"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
install -o root -g root -m 0600 "$repo_root/container/apparmor/duduclaw-userns" "$transaction/candidate"
apparmor_parser -Q "$transaction/candidate"
if [[ -e "$destination" || -L "$destination" ]]; then
  if [[ -L "$destination" || ! -f "$destination" ]] || ! cmp -s "$transaction/candidate" "$destination"; then
    echo "Refusing to overwrite a differing or non-regular $destination." >&2
    exit 1
  fi
  had_file=1
  if [[ "$(stat -c '%u' "$destination")" != 0 ]] || (( (8#$(stat -c '%a' "$destination") & 8#022) != 0 )); then
    echo 'Existing profile must be root-owned and not group/world-writable.' >&2
    exit 1
  fi
fi
if is_loaded; then
  if [[ $had_file -ne 1 ]] || ! is_expected_label; then
    echo 'Loaded duduclaw-userns has no matching disk profile or label.' >&2
    exit 1
  fi
  # Kernel policy bytes cannot be read back. Even a matching disk file and
  # label do not prove its contents, so never replace an already loaded policy.
  committed=1
  echo 'Nothing needed: matching profile file and expected AppArmor label already exist.'
  echo 'DUDUCLAW_APPARMOR_PROFILE=duduclaw-userns'
  exit 0
fi
if [[ $had_file -eq 0 ]]; then
  install -o root -g root -m 0644 "$transaction/candidate" "$transaction/install"
  # Record intent before rename so a signal immediately after mv still rolls back.
  file_changed=1
  mv -- "$transaction/install" "$destination"
fi
load_attempted=1
apparmor_parser -r "$destination"
if ! is_expected_label; then
  echo 'AppArmor did not report duduclaw-userns as loaded.' >&2
  exit 1
fi
committed=1
echo 'Add this line to .env, then recreate the container with docker compose up -d:'
echo 'DUDUCLAW_APPARMOR_PROFILE=duduclaw-userns'
