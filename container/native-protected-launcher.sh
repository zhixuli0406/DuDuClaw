#!/bin/sh
# Only fixed system tools and root-owned paths are used before the drop.
# Never source gateway configuration or run user-installed code as root.
set -eu
umask 077
PATH=/usr/sbin:/usr/bin:/sbin:/bin
LC_ALL=C
export PATH LC_ALL
unset LD_PRELOAD LD_LIBRARY_PATH LD_AUDIT GCONV_PATH ENV BASH_ENV CDPATH DUDUCLAW_ROOT_KEY_FD
exec 3<&-

refused=0
temp=
fail() { refused=1; echo "Protected-key startup refused: $*" >&2; exit 1; }
cleanup() {
    result=$?
    [ -z "$temp" ] || rm -f "$temp"
    if [ "$result" -ne 0 ] && [ "$refused" = 0 ]; then
        echo "Protected-key startup refused: launcher operation failed" >&2
    fi
}
trap cleanup 0
trap 'fail "launcher interrupted"' HUP INT TERM
# Variable names only, never evaluate environment values as shell code.
loader_names=$(env | sed -n 's/^\(LD_[A-Za-z0-9_]*\)=.*/\1/p') || fail "cannot inspect loader environment"
for loader_name in $loader_names; do unset "$loader_name"; done

[ "$(id -u)" = 0 ] || fail "the launcher must run as root"
# Namespace identity must be checked before creating or opening root.key.
for id_map in /proc/self/uid_map /proc/self/gid_map; do
    awk 'NR == 1 && NF == 3 && "x" $1 == "x0" && "x" $2 == "x0" &&
        "x" $3 == "x4294967295" { valid = 1 }
        END { exit !(NR == 1 && valid) }' "$id_map" || fail "user_namespace"
done
for marker in /run/.containerenv /run/systemd/container /.dockerenv; do
    [ ! -e "$marker" ] && [ ! -L "$marker" ] || fail "container_environment"
done
pid_one_environment=$(tr '\000' '\n' < /proc/1/environ) || fail "cannot inspect PID 1 environment"
printf '%s\n' "$pid_one_environment" | awk '/^container=/ { found = 1 } END { exit found }' \
    || fail "container_environment"
unset pid_one_environment
[ "${DOCKER_HOST+x}" != x ] || fail "DOCKER_HOST must be unset (even an empty value is refused)"
[ -n "${INVOCATION_ID:-}" ] && [ "${PPID:-}" = 1 ] || fail "the launcher must be started directly by systemd"
[ "$#" -gt 0 ] || fail "no command was supplied"

# Reject links at every component, including the final file. The installed
# paths deliberately avoid distribution-specific /bin -> /usr/bin links.
check_root_path() {
    checked_path=$1
    case "$checked_path" in /*) ;; *) fail "executable path must be absolute" ;; esac
    [ ! -L "$checked_path" ] && [ -f "$checked_path" ] || fail "executable must be a regular file, not a symlink"
    while :; do
        [ ! -L "$checked_path" ] || fail "protected path must not contain symlinks"
        owner=$(stat -c %u "$checked_path") || fail "cannot inspect protected path owner"
        mode=$(stat -c %a "$checked_path") || fail "cannot inspect protected path mode"
        [ "$owner" = 0 ] || fail "protected path must be root-owned"
        [ "$((0$mode & 0022))" -eq 0 ] || fail "protected path must not be group/other writable"
        [ "$checked_path" != / ] || break
        checked_path=$(dirname -- "$checked_path") || fail "cannot inspect protected path ancestor"
        [ -d "$checked_path" ] || fail "protected path ancestor is not a directory"
    done
}
check_root_path "$1"
check_root_path /usr/local/libexec/duduclaw/protected-launcher

DROP_UID=$(id -u duduclaw) || fail "cannot resolve duduclaw uid"
DROP_GID=$(id -g duduclaw) || fail "cannot resolve duduclaw gid"
case "$DROP_UID" in ''|0|*[!0-9]*) fail "duduclaw uid must be nonzero" ;; esac
case "$DROP_GID" in ''|0|*[!0-9]*) fail "duduclaw gid must be nonzero" ;; esac
account=$(getent passwd duduclaw) || fail "cannot resolve duduclaw account"
printf '%s\n' "$account" | awk -F: '
    NR == 1 && NF == 7 && $1 == "duduclaw" &&
    ($7 == "/usr/sbin/nologin" || $7 == "/sbin/nologin" ||
     $7 == "/usr/bin/false" || $7 == "/bin/false") { valid = 1 }
    END { exit !(NR == 1 && valid) }
' || fail "duduclaw must have a nologin/false shell"
groups=$(id -Gn duduclaw) || fail "cannot inspect duduclaw groups"
for group in $groups; do
    case "$group" in sudo|wheel|admin|adm|docker|lxd|incus|libvirt)
        fail "duduclaw belongs to a privileged group" ;; esac
done
[ ! -e /var/lib/systemd/linger/duduclaw ] && [ ! -L /var/lib/systemd/linger/duduclaw ] || fail "duduclaw linger must be disabled"
if command -v sudo >/dev/null 2>&1; then
    # sudoers display.c emits this exact C-locale line when the list is empty.
    # Reject extra diagnostics, allowed-command lists, and unknown formats.
    sudo -n -l -U duduclaw 2>&1 | awk '
        NR == 1 && /^User duduclaw is not allowed to run sudo on [A-Za-z0-9_.-]+\.$/ { denied = 1 }
        END { exit !(NR == 1 && denied) }
    ' || fail "sudo did not confirm that duduclaw has no allowed commands"
fi

KEY_DIR=/var/lib/duduclaw-protected
KEY=$KEY_DIR/root.key
[ ! -L "$KEY_DIR" ] && [ -d "$KEY_DIR" ] || fail "key directory must be a real directory, not a symlink"
[ "$(stat -c %u:%g "$KEY_DIR")" = 0:0 ] || fail "key directory must be owned by root:root"
chmod 0700 "$KEY_DIR" || fail "cannot secure key directory"
[ ! -L "$KEY" ] || fail "key must not be a symlink"
if [ ! -e "$KEY" ]; then
    temp=$(mktemp "$KEY_DIR/.root.key.XXXXXX") || fail "cannot create key temporary file"
    dd if=/dev/urandom of="$temp" bs=32 count=1 status=none || fail "key generation failed"
    [ "$(stat -c %s "$temp")" = 32 ] || fail "key generation did not produce 32 bytes"
    chown 0:0 "$temp" && chmod 0400 "$temp" || fail "cannot secure key"
    mv -T -n "$temp" "$KEY" || fail "cannot install key"
    rm -f "$temp" || fail "cannot remove key temporary file"
    temp=
fi
[ ! -L "$KEY" ] && [ -f "$KEY" ] || fail "key must be a regular file, not a symlink"
[ "$(stat -c %s "$KEY")" = 32 ] || fail "key must contain exactly 32 bytes"
[ "$(stat -c %u:%g:%a:%h "$KEY")" = 0:0:400:1 ] || fail "key must be root:root, mode 0400, with one link"

# Sweep the dedicated uid system-wide before exposing the descriptor. Root
# must not run any independent job as this uid during service startup.
/usr/bin/setpriv --reuid="$DROP_UID" --regid="$DROP_GID" --clear-groups \
    --inh-caps=-all --bounding-set=-all --no-new-privs \
    /bin/sh -c 'kill -9 -1 2>/dev/null; exit 0' || fail "cannot clear old duduclaw processes"
for status in /proc/[0-9]*/status; do
    if ! process_identity=$(awk '
        $1 == "State:" { zombie = ($2 == "Z"); state_seen = 1 }
        $1 == "Uid:" { uid = $2 }
        END {
            if (!state_seen || uid == "") exit 1
            if (!zombie) print uid
        }
    ' "$status" 2>/dev/null); then
        [ ! -d "${status%/status}" ] || fail "cannot inspect a live process before hand-off"
        continue
    fi
    [ "$process_identity" != "$DROP_UID" ] || fail "a duduclaw process remains before hand-off"
done
exec 3<"$KEY"
# Clear inherited interpreter/runtime settings as well as loader variables.
exec /usr/bin/env -i PATH="$PATH" LC_ALL=C HOME=/var/lib/duduclaw \
    USER=duduclaw LOGNAME=duduclaw DUDUCLAW_ROOT_KEY_FD=3 \
    DUDUCLAW_PROTECTED_KEY_MODE=native-systemd \
    /usr/bin/setpriv --reuid="$DROP_UID" --regid="$DROP_GID" --clear-groups \
    --inh-caps=-all --bounding-set=-all --no-new-privs "$@"
