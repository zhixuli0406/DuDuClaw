#!/bin/sh
set -eu
PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH
[ "$(id -u)" = 0 ] || { echo 'Run with sudo as root' >&2; exit 1; }
LC_ALL=C
export LC_ALL
umask 077
trust_fail() {
    if [ "${trust_context:-source}" = updater ]; then
        echo "Unsafe protected updater: $*" >&2
        echo 'Repair the protected update installation as root before enabling automatic updates.' >&2
        exit 1
    fi
    echo "Unsafe installer source: $*" >&2
    echo 'As root, copy the reviewed directory to /root/duduclaw-auto-upgrade with cp -r --no-preserve=ownership (or install), chown -R root:root that copy, and chmod -R go-w that copy; then verify it against the pinned commit before running it.' >&2
    exit 1
}
trusted() {
    [ ! -L "$1" ] || trust_fail "symlink: $1"
    if [ "$2" = directory ]; then
        [ -d "$1" ] || trust_fail "not a directory: $1"
    else
        [ -f "$1" ] || trust_fail "not a regular file: $1"
    fi
    metadata=$(stat -c '%u:%g:%a' -- "$1") || trust_fail "cannot stat: $1"
    case "$metadata" in
        0:0:*) mode=${metadata#0:0:} ;;
        *) trust_fail "must be owned by root:root: $1" ;;
    esac
    case "$mode" in ''|*[!0-7]*) trust_fail "invalid mode: $1" ;; esac
    [ "$((0$mode & 022))" -eq 0 ] || trust_fail "group/other writable: $1"
}
check_updater() {
    # Walk the lexical path so no writable/symlink ancestor can replace the
    # frozen launcher. Merely finding an old protected gateway is insufficient.
    trust_context=updater
    launcher=/usr/libexec/duduclaw-updated-launch
    trusted "$launcher" file
    [ -x "$launcher" ] || trust_fail "launcher is not executable: $launcher"
    [ "$(stat -c '%h' -- "$launcher")" = 1 ] || trust_fail 'launcher must have exactly one link'
    ancestor=$(dirname -- "$launcher")
    while :; do
        trusted "$ancestor" directory
        [ "$ancestor" != / ] || break
        ancestor=$(dirname -- "$ancestor")
    done
    trust_context=source
    check_updater_active
}
check_updater_active() {
    state=$(systemctl show -p ActiveState --value duduclaw-updated.service) || {
        echo 'Cannot read protected updater service state; automatic updates were not enabled.' >&2
        exit 1
    }
    [ "$state" = active ] || {
        echo 'Protected updater service must be active before enabling automatic updates.' >&2
        exit 1
    }
}
check_source() {
    source_path=$1
    # A trailing slash must not hide a symlink from test -L.
    while [ "$source_path" != / ] && [ "${source_path%/}" != "$source_path" ]; do
        source_path=${source_path%/}
    done
    [ ! -L "$source_path" ] || trust_fail "symlink directory: $source_path"
    source_dir=$(CDPATH='' cd -- "$source_path" && pwd -P) || trust_fail "cannot resolve: $source_path"
    ancestor=$source_dir
    while :; do
        trusted "$ancestor" directory
        [ "$ancestor" != / ] || break
        ancestor=$(dirname -- "$ancestor")
    done
    for name in duduclaw-auto-upgrade duduclaw-auto-upgrade.service duduclaw-auto-upgrade.timer install.sh; do
        trusted "$source_dir/$name" file
    done
}
# Read-only diagnostic; installation always repeats these same checks.
if [ "$#" -eq 2 ] && [ "$1" = --check-source ]; then
    check_source "$2"
    exit 0
fi
case "$#:$*" in
    1:--uninstall)
        # Disable new timer runs, but never interrupt an in-flight replacement.
        systemctl disable --now duduclaw-auto-upgrade.timer
        state=$(systemctl show -p ActiveState --value duduclaw-auto-upgrade.service)
        case "$state" in
            inactive|failed) ;;
            *) echo 'Upgrade service is busy or its state is unknown; timer disabled. Wait for completion and retry --uninstall.' >&2
               exit 1 ;;
        esac
        rm -f /usr/local/sbin/duduclaw-auto-upgrade \
            /etc/systemd/system/duduclaw-auto-upgrade.service \
            /etc/systemd/system/duduclaw-auto-upgrade.timer
        systemctl daemon-reload
        exit 0
        ;;
    0:) ;;
    *) echo 'usage: install.sh [--uninstall | --check-source DIRECTORY]' >&2; exit 1 ;;
esac
[ -x /opt/duduclaw/bin/duduclaw ] || {
    echo 'Protected binary /opt/duduclaw/bin/duduclaw is absent or not executable' >&2
    exit 1
}
# Reject the lexical directory link before canonicalizing and checking ancestors.
[ ! -L "$0" ] || trust_fail "symlink installer: $0"
check_source "$(dirname -- "$0")"
check_updater
# Trusted ancestors prevent unprivileged replacement during snapshot creation.
# Install only the private snapshot, never reopen source files after validation.
trusted /run directory
stage=$(mktemp -d /run/duduclaw-auto-upgrade.XXXXXXXXXX) || trust_fail 'cannot create private snapshot in /run'
trap 'rm -rf -- "$stage"' 0
trap 'exit 1' HUP INT TERM
chmod 0700 "$stage"
trusted "$stage" directory
for name in duduclaw-auto-upgrade duduclaw-auto-upgrade.service duduclaw-auto-upgrade.timer install.sh; do
    cp -- "$source_dir/$name" "$stage/$name"
    trusted "$stage/$name" file
    size=$(stat -c '%s' -- "$stage/$name")
    case "$size" in ''|*[!0-9]*) trust_fail "invalid snapshot size: $name" ;; esac
    [ "$size" -gt 0 ] && [ "$size" -le 1048576 ] || trust_fail "snapshot size must be 1..1048576 bytes: $name"
done
install -o root -g root -m 0755 "$stage/duduclaw-auto-upgrade" /usr/local/sbin/duduclaw-auto-upgrade
for unit in duduclaw-auto-upgrade.service duduclaw-auto-upgrade.timer; do
    install -o root -g root -m 0644 "$stage/$unit" "/etc/systemd/system/$unit"
done
systemctl daemon-reload
check_updater_active
systemctl enable --now duduclaw-auto-upgrade.timer
