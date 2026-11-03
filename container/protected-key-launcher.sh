#!/bin/sh
# This root stage must never read or execute anything from a user-writable
# path. Only fixed image-owned tools and the root-owned key directory are
# accessed before setpriv drops every privilege. In particular, do not source
# configuration, invoke user-installed tools, or run onboarding as root here.
set -eu
umask 077
PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
export PATH
unset LD_PRELOAD LD_LIBRARY_PATH LD_AUDIT GCONV_PATH DOCKER_HOST DUDUCLAW_ROOT_KEY_FD
# First-run preparation must never inherit a key descriptor.
exec 3<&-

fail() { echo "Protected-key startup refused: $*" >&2; exit 1; }

[ "$(id -u)" = 0 ] || fail "the launcher must run as root"
[ "$#" -gt 0 ] || fail "no command was supplied"
DROP_UID=$(id -u duduclaw) || fail "cannot resolve duduclaw uid"
DROP_GID=$(id -g duduclaw) || fail "cannot resolve duduclaw gid"
case "$DROP_UID" in ''|0) fail "duduclaw uid must be nonzero" ;; esac
case "$DROP_GID" in ''|0) fail "duduclaw gid must be nonzero" ;; esac
KEY_DIR=/var/lib/duduclaw-protected
KEY=$KEY_DIR/root.key
[ ! -L "$KEY_DIR" ] && [ -d "$KEY_DIR" ] || fail "key directory must be a real directory, not a symlink"
[ "$(stat -c %u "$KEY_DIR")" = 0 ] || fail "key directory must be owned by root"
chmod 0700 "$KEY_DIR"
[ ! -L "$KEY" ] || fail "key must not be a symlink"
if [ ! -e "$KEY" ]; then
    temp=$(mktemp "$KEY_DIR/.root.key.XXXXXX")
    trap 'rm -f "$temp"' EXIT HUP INT TERM
    dd if=/dev/urandom of="$temp" bs=32 count=1 status=none
    [ "$(stat -c %s "$temp")" = 32 ] || fail "key generation did not produce 32 bytes"
    chmod 0400 "$temp"
    # Do not overwrite a key created by another root launcher using this volume.
    mv -T -n "$temp" "$KEY"
    rm -f "$temp"
    trap - EXIT HUP INT TERM
fi
[ ! -L "$KEY" ] && [ -f "$KEY" ] || fail "key must be a regular file, not a symlink"
[ "$(stat -c %s "$KEY")" = 32 ] || fail "key must contain exactly 32 bytes"
[ "$(stat -c %u:%a:%h "$KEY")" = 0:400:1 ] || fail "key must be root-owned, mode 0400, with one link"

# The enterprise image has no /entrypoint.sh. For the server image, run the
# existing first-run setup unprivileged, with the launch mode cleared to avoid
# recursion or a misleading warning during this deliberate preparation step.
if [ -f /entrypoint.sh ]; then
    HOME=/home/duduclaw USER=duduclaw LOGNAME=duduclaw DUDUCLAW_PROTECTED_KEY_MODE= \
        /usr/bin/setpriv --reuid="$DROP_UID" --regid="$DROP_GID" --clear-groups \
        --inh-caps=-all --bounding-set=-all --no-new-privs /entrypoint.sh true
fi
# Remove unprivileged survivors before hand-off so they cannot ptrace the gateway's key fd.
/usr/bin/setpriv --reuid="$DROP_UID" --regid="$DROP_GID" --clear-groups \
    --inh-caps=-all --bounding-set=-all --no-new-privs \
    /bin/sh -c 'kill -9 -1 2>/dev/null; exit 0'
for status in /proc/[0-9]*/status; do
    real_uid=$(awk '$1 == "Uid:" { print $2; exit }' "$status" 2>/dev/null) || continue
    [ -z "$real_uid" ] || [ "$real_uid" = 0 ] || fail "an unprivileged process remains before hand-off"
done
exec 3<"$KEY"
HOME=/home/duduclaw USER=duduclaw LOGNAME=duduclaw DUDUCLAW_ROOT_KEY_FD=3 \
    exec /usr/bin/setpriv --reuid="$DROP_UID" --regid="$DROP_GID" --clear-groups \
    --inh-caps=-all --bounding-set=-all --no-new-privs "$@"
