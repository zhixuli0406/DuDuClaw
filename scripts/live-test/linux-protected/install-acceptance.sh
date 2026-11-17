#!/bin/sh
# v1.83 fresh isolated install acceptance (plan §4.3 R3, §7 V1 item 3).
# Destructive: run only as root on a disposable VM built from test-key
# artifacts. Never enable shell tracing (license and keys stay out of logs).
#
#   sudo sh install-acceptance.sh install  [--dns-upstream IP:53] [--enable-now]
#   sudo sh install-acceptance.sh rollback
#   sudo sh install-acceptance.sh root-tests <workspace> <iso_root_install_ test name>
#
# install    runs duduclaw-bootstrap install from /root/duduclaw-migrate and
#            checks the committed journal, the readiness report, the frozen
#            launcher receipt and the nightly timer.
# rollback   runs duduclaw-bootstrap rollback and checks that no isolation
#            fact and no journal remain.
# root-tests runs the iso_root_install_ tests (interrupt each step and resume,
#            rollback before the first fact and after S6, refusals). They need
#            ISO_BOOTSTRAP_GENERATION (a staged generation) in the environment.
# Each check prints PASS/FAIL; the script exits non-zero on any FAIL.
set -eu
umask 077
PATH=/usr/sbin:/usr/bin:/sbin:/bin
LC_ALL=C
export PATH LC_ALL

INBOX=/root/duduclaw-migrate
BOOTSTRAP=$INBOX/duduclaw-bootstrap
JOURNAL=/var/lib/duduclaw-protected/isolation-bootstrap.json
RECEIPT=/etc/duduclaw-updated/launcher-receipt.json
# The complete production list, one path per line. It must equal
# duduclaw_core::isolation_install::FACTS exactly (set equality is a CI unit
# test: isolation_install::tests::isolation_install_acceptance_script_lists_every_fact),
# so a leftover outside a hand-picked subset cannot pass the rollback check.
FACTS='
/run/duduclaw-spawnd/control.sock
/etc/systemd/system/duduclaw-spawnd.socket
/etc/systemd/system/duduclaw-spawnd.service
/etc/systemd/system/duduclaw-employees.slice
/etc/systemd/system/duduclaw-egress.socket
/etc/systemd/system/duduclaw-egress.service
/etc/systemd/system/duduclaw-dns-broker.socket
/etc/systemd/system/duduclaw-dns-broker.service
/etc/systemd/system/duduclaw-parse.socket
/etc/systemd/system/duduclaw-parse@.service
/etc/systemd/system/duduclaw-updated.service
/etc/systemd/system/duduclaw-import@.service
/etc/systemd/system/duduclaw-migrate-expire@.service
/etc/systemd/system/duduclaw-migrate-expire@.timer
/usr/lib/duduclaw
/usr/lib/duduclaw/parse-root
/usr/lib/duduclaw/parse-root-tree
/usr/lib/duduclaw/current
/usr/libexec/duduclaw
/var/lib/duduclaw-spawnd
/var/lib/duduclaw-spawnd/etc
/var/log/duduclaw-spawnd
/etc/duduclaw-updated
/var/lib/duduclaw-updated
/opt/duduclaw-isolated
/opt/duduclaw-isolated/generations
/opt/duduclaw-isolated/slots
/opt/duduclaw-isolated/slots/a
/opt/duduclaw-isolated/slots/b
/var/lib/duduclaw-migrate
/opt/duduclaw-isolated/current
/var/lib/duduclaw-updated/state.json
/etc/duduclaw/migration-import.json
/usr/lib/duduclaw/duduclaw-spawnd
/usr/lib/duduclaw/duduclaw-run
/opt/duduclaw/bin/duduclaw-egress
/usr/libexec/duduclaw/duduclaw-parse-worker
/usr/lib/duduclaw/current/duduclaw-import
/usr/lib/duduclaw/current/duduclaw-migrate
/usr/lib/duduclaw/current/duduclaw-mcp-bridge
/usr/lib/duduclaw/current/duduclaw
/usr/libexec/duduclaw-updated-launch
'
PASSED=0 FAILED=0

usage() { echo 'usage: sudo sh install-acceptance.sh install [--dns-upstream IP:53] [--enable-now] | rollback | root-tests <workspace> <test>' >&2; exit 2; }
record() {
    printf '%s %s\n' "$1" "$2"
    case "$1" in PASS) PASSED=$((PASSED + 1)) ;; FAIL) FAILED=$((FAILED + 1)) ;; esac
}
check() {
    check_name=$1
    shift
    if "$@" >/dev/null 2>&1; then record PASS "$check_name"; else record FAIL "$check_name"; fi
}
finish() {
    printf 'summary: %s passed, %s failed\n' "$PASSED" "$FAILED"
    [ "$FAILED" -eq 0 ]
}
no_facts() {
    for fact in $FACTS; do
        if [ -e "$fact" ] || [ -L "$fact" ]; then return 1; fi
    done
    return 0
}
journal_committed() { grep -q '"committed":true' "$JOURNAL"; }

[ "$#" -ge 1 ] || usage
[ "$(id -u)" = 0 ] || { echo 'FAIL preconditions: root required'; exit 1; }
[ -d "$INBOX" ] && [ ! -L "$INBOX" ] || { echo 'FAIL preconditions: /root/duduclaw-migrate missing'; exit 1; }
[ "$(stat -c %u:%a "$INBOX")" = 0:700 ] || { echo 'FAIL preconditions: inbox must be root 0700'; exit 1; }
mode=$1
shift
case "$mode" in
    install)
        pin=$("$BOOTSTRAP" --print-root-pin) || { echo 'FAIL preconditions: verifier has no root pin'; exit 1; }
        case "$pin" in TEST\ *) record PASS 'verifier reports a TEST root pin' ;;
            *) echo 'FAIL preconditions: acceptance uses TEST root keys only'; exit 1 ;; esac
        if "$BOOTSTRAP" install "$@"; then record PASS 'duduclaw-bootstrap install'; else record FAIL 'duduclaw-bootstrap install'; fi
        check 'journal committed' journal_committed
        check 'readiness --protected-linux' /opt/duduclaw/bin/duduclaw service readiness --protected-linux
        check 'frozen launcher receipt root 0600' test "$(stat -c %u:%g:%a "$RECEIPT")" = 0:0:600
        check 'nightly update timer active' systemctl is-active --quiet duduclaw-auto-upgrade.timer
        check 'gateway unit enabled' systemctl is-enabled --quiet duduclaw-gateway.service
        check 'migrate-from refused' sh -c '! /opt/duduclaw/bin/duduclaw service install --protected-linux --migrate-from /nonexistent'
        ;;
    rollback)
        [ "$#" -eq 0 ] || usage
        if "$BOOTSTRAP" rollback; then record PASS 'duduclaw-bootstrap rollback'; else record FAIL 'duduclaw-bootstrap rollback'; fi
        check 'no isolation fact remains' no_facts
        check 'journal removed' test ! -e "$JOURNAL"
        check 'gateway unit removed' test ! -e /etc/systemd/system/duduclaw-gateway.service
        ;;
    root-tests)
        # One test per fresh VM snapshot: each starts from a host with only the
        # staged generation (the end-to-end test commits; the others roll back).
        [ "$#" -eq 2 ] || usage
        [ -n "${ISO_BOOTSTRAP_GENERATION:-}" ] || { echo 'FAIL preconditions: set ISO_BOOTSTRAP_GENERATION'; exit 1; }
        case "$2" in iso_root_install_*) ;; *) usage ;; esac
        cd "$1"
        if DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-cli --lib -- --ignored --exact \
            "service::protected_linux::linux::bootstrap::tests::$2" --test-threads=1; then
            record PASS "$2"
        else
            record FAIL "$2"
        fi
        ;;
    *) usage ;;
esac
finish
