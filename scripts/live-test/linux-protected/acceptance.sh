#!/bin/sh
# Destructive disposable-host acceptance only. Never enable shell tracing.
set -eu
umask 077
PATH=/usr/sbin:/usr/bin:/sbin:/bin
LC_ALL=C
export PATH LC_ALL

usage() { echo 'Usage: sudo sh acceptance.sh <candidate-dir> [--reset] [--keep]' >&2; exit 2; }
[ "$#" -ge 1 ] || usage
DIR=$(cd "$1" && pwd -P)
shift
KEEP=0 RESET=0
for option do
    case "$option" in --keep) KEEP=1 ;; --reset) RESET=1 ;; *) usage ;; esac
done
[ "$(id -u)" = 0 ] || { echo 'FAIL preconditions: root required'; exit 1; }
[ ! -L "$DIR/acceptance.log" ] || { echo 'FAIL preconditions: log is a symlink'; exit 1; }
: > "$DIR/acceptance.log"
LOG=$DIR/acceptance.log
FAILED=0 PASSED=0 ARMED=0 FINISHED=0 SOCKET_PID= TMP=
UNIT=duduclaw-gateway.service
STAGE=/root/duduclaw-stage
BIN=/opt/duduclaw/bin/duduclaw
DATA=/var/lib/duduclaw/.duduclaw
FLOOR=/var/lib/duduclaw-protected/version-floor
PROBE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)/rpc_probe.py

record() {
    printf '%s %s: %s\n' "$1" "$2" "$3" | tee -a "$LOG"
    case "$1" in PASS) PASSED=$((PASSED + 1)) ;; FAIL) FAILED=$((FAILED + 1)) ;; esac
}
check() {
    check_name=$1
    check_detail=$2
    shift 2
    if "$@" >/dev/null 2>&1; then record PASS "$check_name" "$check_detail"
    else record FAIL "$check_name" "$check_detail"; fi
}
fatal() { record FAIL "$1" "$2"; exit 1; }
masked_error() {
    python3 - "$1" <<'REASON'
import pathlib, re, sys
lines = [line.strip() for line in pathlib.Path(sys.argv[1]).read_text(errors='replace').splitlines() if line.strip()]
errors = [line for line in lines if re.search(r'GLIBC_|error|failed|refused|錯誤|失敗|拒絕', line, re.I)]
glibc_errors = [line for line in lines if 'GLIBC_' in line]
reason = (glibc_errors or errors or lines or ['command failed without output'])[-1]
reason = re.sub(r'(https?://[^\s?]+)\?[^\s]*', r'\1?[REDACTED]', reason)
reason = re.sub(r'(?i)(bearer\s+)\S+', r'\1[REDACTED]', reason)
reason = re.sub(r'(?i)((?:token|secret|password|api[_-]?key|authorization)[\x22\x27]?\s*[=:]\s*[\x22\x27]?)\S+', r'\1[REDACTED]', reason)
reason = re.sub(r'[A-Za-z0-9_./+%=-]{32,}', '[REDACTED]', reason)
reason = re.sub(r'[\x00-\x1f\x7f-\x9f]', '', reason)
print(reason[:600])
REASON
}
cursor() { journalctl -n 0 --show-cursor --no-pager | sed -n 's/^-- cursor: //p'; }
stop_socket() {
    if [ -n "$SOCKET_PID" ]; then
        kill "$SOCKET_PID" 2>/dev/null || :
        wait "$SOCKET_PID" 2>/dev/null || :
        SOCKET_PID=
    fi
}
cleanup() {
    # Called for --reset as well as the EXIT trap. No downloaded binary runs as root.
    cleanup_before=$FAILED
    cleanup_fixture=
    cleanup_docker=0
    cleanup_marker=0
    if [ -f "$STAGE/fixture-path" ]; then cleanup_fixture=$(sed -n '2p' "$STAGE/fixture-path"); fi
    if [ -f "$STAGE/docker-created" ]; then cleanup_docker=1; fi
    if [ -f "$STAGE/containerenv-created" ]; then cleanup_marker=1; fi
    stop_socket
    if [ -f "$STAGE/containerenv-created" ]; then
        check cleanup-marker 'remove test container marker' rm -f /run/.containerenv
    fi
    if getent passwd duduclaw >/dev/null 2>&1; then
        loginctl disable-linger duduclaw >/dev/null 2>&1 || :
        if getent group docker >/dev/null 2>&1; then
            gpasswd -d duduclaw docker >/dev/null 2>&1 || :
        fi
    fi
    [ ! -f "$BIN" ] || chmod 0755 "$BIN" 2>/dev/null || :
    if [ -x "$STAGE/duduclaw" ]; then
        check cleanup-uninstall 'candidate service uninstall --protected-linux' \
            "$STAGE/duduclaw" service uninstall --protected-linux
    fi
    systemctl disable --now "$UNIT" >/dev/null 2>&1 || :
    # Also remove a failed-start unit; uninstall deliberately retains data/account.
    check cleanup-unit 'remove system unit' rm -f "/etc/systemd/system/$UNIT"
    check cleanup-data 'remove fixed test installation paths' rm -r -f \
        /opt/duduclaw /usr/local/libexec/duduclaw /var/lib/duduclaw /var/lib/duduclaw-protected
    if getent passwd duduclaw >/dev/null 2>&1; then
        check cleanup-user 'remove service user' userdel duduclaw
    fi
    if getent group duduclaw >/dev/null 2>&1; then
        check cleanup-group 'remove service group' groupdel duduclaw
    fi
    if [ -f "$STAGE/fixture-path" ]; then
        # The root-only manifest records only a fixture created by this harness.
        # Validate against passwd, never source a shell manifest or use HOME.
        check cleanup-fixture 'remove only harness-created migration fixture' python3 - "$STAGE/fixture-path" <<'PY'
import os, pathlib, pwd, shutil, sys
lines = pathlib.Path(sys.argv[1]).read_text().splitlines()
uid = int(lines[0])
path = pathlib.Path(lines[1])
assert uid != 0 and path == pathlib.Path(pwd.getpwuid(uid).pw_dir) / '.duduclaw'
assert not path.is_symlink()
if path.exists():
    assert path.stat().st_uid == uid
    shutil.rmtree(path)
PY
    fi
    if [ -f "$STAGE/docker-created" ] && getent group docker >/dev/null 2>&1; then
        check cleanup-docker 'remove harness-created docker group' groupdel docker
    fi
    # Retain root-only ownership evidence if any cleanup operation failed.
    if [ "$FAILED" = "$cleanup_before" ]; then
        check cleanup-stage 'remove root staging tree' rm -r -f "$STAGE"
    else
        record FAIL cleanup-stage 'retained staging/ownership manifest for a later --reset'
    fi
    check cleanup-reload 'reload systemd units' systemctl daemon-reload
    systemctl reset-failed "$UNIT" >/dev/null 2>&1 || :
    leftovers=0
    for path in /opt/duduclaw /usr/local/libexec/duduclaw /var/lib/duduclaw \
        /var/lib/duduclaw-protected "$STAGE" "/etc/systemd/system/$UNIT" /var/lib/systemd/linger/duduclaw; do
        if [ -e "$path" ] || [ -L "$path" ]; then leftovers=1; fi
    done
    if getent passwd duduclaw >/dev/null 2>&1 || getent group duduclaw >/dev/null 2>&1; then leftovers=1; fi
    if systemctl cat "$UNIT" >/dev/null 2>&1; then leftovers=1; fi
    if [ -n "$cleanup_fixture" ] && { [ -e "$cleanup_fixture" ] || [ -L "$cleanup_fixture" ]; }; then leftovers=1; fi
    if [ "$cleanup_docker" = 1 ] && getent group docker >/dev/null 2>&1; then leftovers=1; fi
    if [ "$cleanup_marker" = 1 ] && { [ -e /run/.containerenv ] || [ -L /run/.containerenv ]; }; then leftovers=1; fi
    if [ "$leftovers" = 0 ]; then record PASS cleanup-final 'installation, unit and account absent'
    else record FAIL cleanup-final 'test installation residue remains'; fi
}
finish() {
    finish_code=$?
    trap - 0 HUP INT TERM
    set +e
    if [ "$finish_code" -ne 0 ] && [ "$FINISHED" = 0 ]; then
        record FAIL interrupted 'script stopped before all steps completed'
    fi
    if [ "$ARMED" = 1 ]; then
        if [ "$KEEP" = 0 ]; then cleanup
        else stop_socket; record PASS keep 'installation retained; rerun with --reset to clean'; fi
    fi
    [ -z "$TMP" ] || rm -r -f "$TMP"
    printf '\n%-12s %s\n%-12s %s\n%-12s %s\n' 'RESULT' 'COUNT' 'PASS' "$PASSED" 'FAIL' "$FAILED" | tee -a "$LOG"
    if [ "$FAILED" -ne 0 ] || [ "$finish_code" -ne 0 ]; then exit 1; fi
    exit 0
}
trap finish 0
trap 'exit 1' HUP INT TERM

for tool in systemctl journalctl loginctl setpriv python3 getent useradd userdel groupadd groupdel \
    gpasswd install stat awk timeout sha256sum cmp tee mktemp grep sed head touch chmod cat; do
    command -v "$tool" >/dev/null 2>&1 || fatal preconditions "missing $tool"
done
[ "$(cat /proc/1/comm)" = systemd ] || fatal preconditions 'PID 1 must be systemd'
[ "$(stat -f -c %T /sys/fs/cgroup)" = cgroup2fs ] || fatal preconditions 'unified cgroup v2 required'
[ -x "$DIR/duduclaw" ] && [ -f "$PROBE" ] || fatal preconditions 'candidate binary or rpc_probe.py missing'
python3 -c 'import sys; assert sys.version_info >= (3,10)' || fatal preconditions 'Python 3.10+ required'
[ -n "${SUDO_USER:-}" ] && [ "${SUDO_UID:-0}" != 0 ] && [ "${SUDO_GID:-0}" != 0 ] \
    || fatal preconditions 'invoke sudo from a non-root user'
[ "$(id -u "$SUDO_USER")" = "$SUDO_UID" ] && [ "$(id -g "$SUDO_USER")" = "$SUDO_GID" ] \
    || fatal preconditions 'sudo identity does not match passwd'
TMP=$(mktemp -d /tmp/duduclaw-acceptance.XXXXXX)
DUDUCLAW_HOME=$TMP/operator-home
export DUDUCLAW_HOME
INVOKING_HOME=$(getent passwd "$SUDO_USER" | awk -F: '{print $6}')
FIXTURE=$INVOKING_HOME/.duduclaw
# Preserve container evidence: --reset is not permission to erase a host marker.
[ ! -e /run/.containerenv ] && [ ! -L /run/.containerenv ] \
    || [ -f "$STAGE/containerenv-created" ] || fatal preconditions 'pre-existing container marker'
if [ -e "$STAGE" ] || [ -L "$STAGE" ]; then
    [ ! -L "$STAGE" ] && [ "$(stat -c %u:%g:%a "$STAGE")" = 0:0:700 ] \
        || fatal preconditions 'unsafe root staging directory'
    if [ -e "$STAGE/duduclaw" ]; then
        [ ! -L "$STAGE/duduclaw" ] && [ "$(stat -c %u:%g:%a "$STAGE/duduclaw")" = 0:0:755 ] \
            || fatal preconditions 'unsafe staged candidate'
    fi
fi
if [ "$RESET" = 1 ]; then cleanup; fi
for path in /opt/duduclaw /var/lib/duduclaw /var/lib/duduclaw-protected \
    /usr/local/libexec/duduclaw "$STAGE"; do
    [ ! -e "$path" ] && [ ! -L "$path" ] || fatal preconditions "already exists: $path (use --reset)"
done
if getent passwd duduclaw >/dev/null 2>&1 || getent group duduclaw >/dev/null 2>&1 \
    || systemctl cat "$UNIT" >/dev/null 2>&1; then
    fatal preconditions 'service account, group or unit already exists (use --reset)'
fi
record PASS host "$(uname -a)"
record PASS systemd "$(systemctl --version | head -n 1)"
record PASS preconditions 'root, systemd, cgroup2, tools and clean paths checked'
install -d -o root -g root -m 0700 "$STAGE"
ARMED=1
check stage 'stage candidate root:root 0755' install -o root -g root -m 0755 "$DIR/duduclaw" "$STAGE/duduclaw"
[ -x "$STAGE/duduclaw" ] || fatal stage 'candidate staging failed'

if [ ! -e "$FIXTURE" ] && [ ! -L "$FIXTURE" ]; then
    printf '%s\n%s\n' "$SUDO_UID" "$FIXTURE" > "$STAGE/fixture-path"
    # Root opens the executable before dropping privileges: /root stays 0700.
    exec 4< "$STAGE/duduclaw"
    if setpriv --reuid "$SUDO_UID" --regid "$SUDO_GID" --clear-groups \
        env -i PATH="$PATH" HOME="$INVOKING_HOME" USER="$SUDO_USER" LOGNAME="$SUDO_USER" \
        /proc/self/fd/4 onboard --yes >/dev/null 2>&1; then
        record PASS fixture 'created via candidate onboard --yes as sudo user'
    else fatal fixture 'non-interactive onboarding failed'; fi
    exec 4<&-
    # Onboarding has no admin-token generator. Configure a random test-only
    # legacy token and disable automatic work in this disposable fixture.
    if setpriv --reuid "$SUDO_UID" --regid "$SUDO_GID" --clear-groups \
        python3 - "$FIXTURE" <<'PY'
import pathlib, secrets, sys
home = pathlib.Path(sys.argv[1])
config = home / 'config.toml'
raw = config.read_text()
assert raw.count('[gateway]') == 1
raw = raw.replace('[gateway]', '[gateway]\nauth_token = "' + secrets.token_hex(32) + '"', 1)
config.write_text(raw)
config.chmod(0o600)
(home / 'acceptance-fixture.txt').write_text('native protected migration fixture\n')
PY
    then record PASS fixture-token 'random token configured without disclosure'
    else fatal fixture-token 'cannot configure isolated token'; fi
else
    record PASS fixture 'existing sudo-user home retained unchanged; requires usable plaintext admin token'
fi

START_CURSOR=$(cursor)
[ -n "$START_CURSOR" ] || fatal startup-journal 'cannot acquire journal cursor before install'
if env SUDO_UID="$SUDO_UID" SUDO_GID="$SUDO_GID" "$STAGE/duduclaw" service install \
    --protected-linux --migrate-from "$FIXTURE" --enable-now >/dev/null 2>&1; then
    record PASS install 'protected service installed with migration and enable-now'
else fatal install 'protected installation failed (inspect journal on host)'; fi
check account-shell 'dedicated account has nologin/false shell' sh -c \
    'getent passwd duduclaw | awk -F: '\''$7 ~ /^\/(usr\/)?s?bin\/(nologin|false)$/ {ok=1} END {exit !ok}'\'''
check account-groups 'only primary group in account database' sh -c \
    '[ "$(id -G duduclaw)" = "$(id -g duduclaw)" ]'
for file in "$BIN" /usr/local/libexec/duduclaw/protected-launcher; do
    check install-permissions "$file root:root 0755" test "$(stat -c %u:%g:%a "$file")" = 0:0:755
done
check key-directory 'root:root 0700' test "$(stat -c %u:%g:%a /var/lib/duduclaw-protected)" = 0:0:700
check migration 'copied tree present, owned by service uid, stable fixture bytes preserved' python3 - "$FIXTURE" "$DATA" <<'PY'
import hashlib, os, pathlib, pwd, sys
source, target = map(pathlib.Path, sys.argv[1:])
uid = pwd.getpwnam('duduclaw').pw_uid
count = 0
compared = 0
for base, dirs, files in os.walk(source):
    for name in dirs + files:
        old = pathlib.Path(base) / name
        new = target / old.relative_to(source)
        assert not old.is_symlink() and new.stat().st_uid == uid
        if old.is_file():
            count += 1
            # The gateway appends migrated logs and updates runtime databases.
            # Compare only the deliberate marker or the static configuration.
            if old.relative_to(source).as_posix() in ('acceptance-fixture.txt', 'config.toml'):
                assert hashlib.sha256(old.read_bytes()).digest() == hashlib.sha256(new.read_bytes()).digest()
                compared += 1
assert count > 0 and compared > 0
PY
# Read only the legacy plaintext token; no shell evaluation, no secret in argv.
# Existing encrypted-only homes require a dedicated fresh sudo-user fixture.
if python3 - "$DATA/config.toml" "$TMP/token" <<'PY'
import os, pathlib, re, sys
raw = pathlib.Path(sys.argv[1]).read_text()
section = re.search(r'(?ms)^\[gateway\]\s*\n(.*?)(?=^\[|\Z)', raw)
assert section
assert not re.search(r'^\s*auth_token_enc\s*=', section[1], re.M)
m = re.search(r'^\s*auth_token\s*=\s*"([A-Za-z0-9_-]+)"\s*(?:#.*)?$', section[1], re.M)
assert m and m[1]
fd = os.open(sys.argv[2], os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
with os.fdopen(fd, 'w') as out: out.write(m[1])
PY
then record PASS token 'read service config [gateway] auth_token into root-only temporary file'
else fatal token 'no supported plaintext token; use a fresh dedicated sudo user'; fi

rpc() {
    rpc_params='{}'
    [ "$#" -lt 2 ] || rpc_params=$2
    env DUDUCLAW_HOME="$DATA" DUDUCLAW_PORT= python3 "$PROBE" --token-file "$TMP/token" \
        --method "$1" --params "$rpc_params"
}
healthy() {
    rpc security.credential_protection > "$TMP/status" 2>/dev/null || return 1
    python3 - "$TMP/status" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
assert s['level'] == s['platform_level'] == 'process_isolated'
assert s['backend'] == 'container_root_key' and s['state'] == 'ready'
assert s['person_identity_allowed'] is True and s['refusal_reason'] is None
PY
}
wait_gateway() {
    # One monotonic budget includes socket timeouts and sleep intervals.
    env DUDUCLAW_HOME="$DATA" DUDUCLAW_PORT= python3 - "$PROBE" <<'PYCODE'
import importlib.util, socket, sys, time, urllib.parse
spec = importlib.util.spec_from_file_location('probe', sys.argv[1])
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
u = urllib.parse.urlsplit(m.default_url())
deadline = time.monotonic() + 60
while time.monotonic() < deadline:
    try:
        with socket.create_connection((u.hostname, u.port), timeout=min(0.5, max(0.01, deadline-time.monotonic()))):
            sys.exit(0)
    except OSError:
        time.sleep(min(1, max(0, deadline-time.monotonic())))
sys.exit(1)
PYCODE
}
ensure_ready() {
    wait_gateway || return 1
    rpc security.credential_protection > "$TMP/status" 2>/dev/null || return 1
    if python3 -c 'import json,sys; sys.exit(json.load(open(sys.argv[1]))["state"] != "not_enabled")' "$TMP/status"; then
        rpc security.credential_protection_enable > /dev/null 2>&1 || return 1
    fi
    healthy
}
journal_clean() {
    journalctl -u "$UNIT" --after-cursor "$1" --no-pager -o cat > "$TMP/journal" 2>/dev/null || return 1
    ! grep -Ei 'refused|revoked' "$TMP/journal" >/dev/null
}
check live-state 'ready, process_isolated, container_root_key, personal identities allowed' ensure_ready
check active 'unit active' systemctl is-active --quiet "$UNIT"
check root-key 'root:root 0400, 32 bytes, one hardlink' test \
    "$(stat -c %u:%g:%a:%s:%h /var/lib/duduclaw-protected/root.key)" = 0:0:400:32:1
journalctl -u "$UNIT" --after-cursor "$START_CURSOR" --no-pager -o cat > "$TMP/journal"
if grep -Ei 'refused|revoked' "$TMP/journal" >/dev/null; then record FAIL startup-journal 'refused/revoked found'
else record PASS startup-journal 'no refused/revoked lines'; fi
PID=$(systemctl show -p MainPID --value "$UNIT")
case "$PID" in ''|0|*[!0-9]*) fatal process-pid 'no valid gateway MainPID' ;; esac
systemctl is-active --quiet "$UNIT" && [ -r "/proc/$PID/environ" ] \
    || fatal process-pid 'gateway inactive or root cannot inspect environment'
check process-status 'non-root uid, NoNewPrivs 1, CapEff 0' python3 - "$PID" <<'PY'
import sys
s = dict(line.split(':', 1) for line in open('/proc/' + sys.argv[1] + '/status') if ':' in line)
assert all(int(x) != 0 for x in s['Uid'].split())
assert s['NoNewPrivs'].strip() == '1' and int(s['CapEff'].strip(), 16) == 0
PY
check cgroup 'exact system unit cgroup' test "$(cat "/proc/$PID/cgroup")" = '0::/system.slice/duduclaw-gateway.service'
DUID=$(id -u duduclaw) DGID=$(id -g duduclaw)
if ! setpriv --reuid "$DUID" --regid "$DGID" --clear-groups python3 -c \
    'import os,sys; assert os.getuid() == int(sys.argv[1]); open("/proc/self/status").read()' "$DUID" >/dev/null 2>&1; then
    fatal denial-control 'setpriv control failed; denial tests cannot establish isolation'
fi
cat "/proc/$PID/environ" > /dev/null && cat /var/lib/duduclaw-protected/root.key > /dev/null \
    || fatal denial-control 'root cannot read targets; denial test would be ambiguous'
record PASS denial-control 'root reads targets and dropped-uid control works'
if setpriv --reuid "$DUID" --regid "$DGID" --clear-groups cat "/proc/$PID/environ" >/dev/null 2>&1; then
    record FAIL nondumpable 'same uid could read gateway environment'
else record PASS nondumpable 'same uid cannot read gateway environment'; fi
if setpriv --reuid "$DUID" --regid "$DGID" --clear-groups cat /var/lib/duduclaw-protected/root.key >/dev/null 2>&1; then
    record FAIL key-denial 'service uid could read root key'
else record PASS key-denial 'service uid cannot read root key'; fi
check denial-pid 'gateway PID still live after denial checks' test \
    "$(systemctl show -p MainPID --value "$UNIT")" = "$PID"

negative() {
    neg_name=$1 neg_reason=$2 neg_cursor=$(cursor)
    if [ -z "$neg_cursor" ]; then record FAIL "$neg_name" 'journal cursor unavailable'; return; fi
    systemctl reset-failed "$UNIT" >/dev/null 2>&1 || :
    systemctl restart "$UNIT" >/dev/null 2>&1 || :
    neg_count=0 neg_ok=0
    while [ "$neg_count" -lt 15 ]; do
        journalctl -u "$UNIT" --after-cursor "$neg_cursor" --no-pager -o cat > "$TMP/negative" 2>/dev/null || :
        # Launcher branch must actually refuse start, not merely emit a warning.
        if grep -F "Protected-key startup refused: $neg_reason" "$TMP/negative" >/dev/null \
            && ! systemctl is-active --quiet "$UNIT"; then neg_ok=1; break; fi
        if rpc security.credential_protection > "$TMP/status" 2>/dev/null && \
            python3 - "$TMP/status" "$neg_reason" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
assert s['state'] == 'unavailable' and s['person_identity_allowed'] is False
assert s['level'] == 'file' and s['refusal_reason'] == sys.argv[2]
PY
        then neg_ok=1; break; fi
        sleep 1
        neg_count=$((neg_count + 1))
    done
    if [ "$neg_ok" = 1 ]; then record PASS "$neg_name" "refused with expected reason: $neg_reason"
    else record FAIL "$neg_name" "expected refusal absent: $neg_reason"; fi
}
recover() {
    systemctl reset-failed "$UNIT" >/dev/null 2>&1 || :
    if systemctl restart "$UNIT" >/dev/null 2>&1 && ensure_ready >/dev/null 2>&1; then
        record PASS "$1-recovery" 'healthy after revert'
    else record FAIL "$1-recovery" 'gateway did not recover'; fi
}
if ! getent group docker >/dev/null 2>&1; then
    : > "$STAGE/docker-created"
    groupadd docker
fi
check docker-change 'add privileged supplementary group' gpasswd -a duduclaw docker
negative docker 'duduclaw belongs to a privileged group'
check docker-revert 'remove privileged supplementary group' gpasswd -d duduclaw docker
if [ -f "$STAGE/docker-created" ]; then check docker-remove 'remove harness-created group' groupdel docker; fi
recover docker
check linger-change 'enable linger' loginctl enable-linger duduclaw
negative linger 'duduclaw linger must be disabled'
check linger-revert 'disable linger' loginctl disable-linger duduclaw
recover linger
check writable-change 'make candidate group writable' chmod g+w "$BIN"
negative writable 'protected path must not be group/other writable'
check writable-revert 'restore 0755' chmod 0755 "$BIN"
recover writable
: > "$STAGE/containerenv-created"
check container-change 'create container marker' touch /run/.containerenv
negative container container_environment
check container-revert 'remove test marker' rm -f /run/.containerenv
recover container

SOCKET_CURSOR=$(cursor)
setpriv --reuid nobody --regid "$(id -g nobody)" --clear-groups python3 -c '
import socket
s = socket.socket(socket.AF_UNIX); s.bind("\0docker-test"); s.listen(64)
print("ready", flush=True)
while True:
    c, _ = s.accept(); c.close()
' > "$TMP/socket-ready" 2>/dev/null &
SOCKET_PID=$!
sleep 1
if kill -0 "$SOCKET_PID" 2>/dev/null && grep -qx ready "$TMP/socket-ready"; then
    # Poll throughout the 70-second window, including the minute reattestation.
    socket_count=0 socket_ok=1
    while [ "$socket_count" -lt 70 ]; do
        sleep 5
        if ! healthy >/dev/null 2>&1 || ! kill -0 "$SOCKET_PID" 2>/dev/null; then socket_ok=0; fi
        socket_count=$((socket_count + 5))
    done
    if [ "$socket_ok" = 1 ] && journal_clean "$SOCKET_CURSOR"; then
        record PASS abstract-socket 'nobody-owned docker-test listener did not revoke protection for 70 s'
    else record FAIL abstract-socket 'listener died or protection revoked/unhealthy'; fi
else record FAIL abstract-socket 'unprivileged abstract listener did not start'; fi
stop_socket

VERSION=$("$BIN" --version | awk '{print $NF}')
BEFORE=$(stat -c %d:%i "$BIN")
if "$BIN" service upgrade --protected-linux --version "$VERSION" > "$TMP/replay" 2>&1; then
    record FAIL upgrade-replay 'equal version unexpectedly accepted'
elif grep -F 'refuses equal or older upgrade versions' "$TMP/replay" >/dev/null \
    && [ "$BEFORE" = "$(stat -c %d:%i "$BIN")" ]; then
    record PASS upgrade-replay 'equal version refused without replacing binary'
else record FAIL upgrade-replay 'failure did not match replay policy'; fi
OLD_PID=$(systemctl show -p MainPID --value "$UNIT")
# Network/signature failure remains FAIL; never weaken the version floor to
# force an official downgrade. No raw command output is copied into the log.
if timeout 600 "$BIN" service upgrade --protected-linux > "$TMP/upgrade" 2>&1; then
    if grep -F '已驗證並安裝官方版本' "$TMP/upgrade" >/dev/null \
        && [ "$BEFORE" != "$(stat -c %d:%i "$BIN")" ] \
        && [ "$(stat -c %u:%g:%a "$FLOOR")" = 0:0:600 ]; then
        UPGRADE_RUNNABLE=1
        if ! "$BIN" --version > "$TMP/upgrade-version" 2>&1; then UPGRADE_RUNNABLE=0; fi
        if [ "$(systemctl show -p MainPID --value "$UNIT")" = "$OLD_PID" ]; then
            UPGRADE_RUNNABLE=0
            printf '%s\n' 'error: unit PID unchanged after upgrade' >> "$TMP/upgrade-version"
        fi
        sleep 15
        if ! systemctl is-active --quiet "$UNIT"; then
            UPGRADE_RUNNABLE=0
            journalctl -u "$UNIT" -n 20 --no-pager >> "$TMP/upgrade-version" 2>&1 || :
            printf '%s\n' 'unit inactive 15 s after upgrade' >> "$TMP/upgrade-version"
        fi
        if [ "$UPGRADE_RUNNABLE" = 1 ]; then
            record PASS upgrade-official 'verified install; binary --version exits 0; unit active after 15 s'
        else record FAIL upgrade-official "$(masked_error "$TMP/upgrade-version")"; fi
        if python3 - "$BIN" "$FLOOR" > "$TMP/upgrade-floor" 2>&1 <<'PY'
import pathlib, subprocess, sys
floor = pathlib.Path(sys.argv[2]).read_text()
result = subprocess.run([sys.argv[1], '--version'], capture_output=True, text=True)
parts = result.stdout.split()
version = parts[-1] if result.returncode == 0 and parts else None
if version is None or floor != version:
    print(f'floor mismatch: actual content={floor!r}; parsed version={version!r}')
    sys.exit(1)
PY
        then record PASS upgrade-floor 'floor equals installed release version'
        else record FAIL upgrade-floor "$(masked_error "$TMP/upgrade-floor")"; fi
    else record FAIL upgrade-official 'signature/install receipt, inode, floor or restart evidence missing'; fi
else
    UPGRADE_REASON=$(masked_error "$TMP/upgrade")
    record FAIL upgrade-official "$UPGRADE_REASON"
fi
# The verified installer emits numeric sizes before/after extraction when available.
for SIZE_KIND in archive binary; do
    SIZE_VALUE=$(sed -n "s/^Release $SIZE_KIND bytes: \([0-9][0-9]*\)$/\1/p" "$TMP/upgrade" | tail -n 1)
    if [ -n "$SIZE_VALUE" ]; then record INFO "upgrade-$SIZE_KIND-bytes" "$SIZE_VALUE"; fi
done
record PASS upgrade-context 'Task baseline latest v1.79.2 predates native mode: if installed, personal accounts may be unavailable; not a healthy-native assertion'
# TEST ONLY: deliberately discard rollback protection and reinstall the staged
# candidate, without migration. Also clear pending intent after a failed swap.
check rollback-floor 'TEST ONLY rollback: remove version floor and pending intent' rm -f "$FLOOR" /var/lib/duduclaw-protected/version-pending
check rollback-install 'TEST ONLY reinstall candidate without migration' "$STAGE/duduclaw" service install --protected-linux --enable-now
check rollback-state 'candidate protected state recovered' ensure_ready

if rpc system.check_update > "$TMP/update" 2>/dev/null; then
    check updater-check 'manual install_method, none update_channel, auto_update false' python3 - "$TMP/update" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
assert s['install_method'] == 'manual' and s['update_channel'] == 'none'
assert s['auto_update'] is False
PY
else record FAIL updater-check 'dashboard check RPC failed (network required)'; fi
if rpc system.apply_update > /dev/null 2> "$TMP/apply-error"; then
    record FAIL updater-install 'dashboard install unexpectedly accepted'
elif grep -F 'sudo /opt/duduclaw/bin/duduclaw service upgrade --protected-linux' "$TMP/apply-error" >/dev/null; then
    record PASS updater-install 'dashboard replacement refused with root-managed service instruction'
else record FAIL updater-install 'RPC failed without expected protected-service refusal'; fi
FINISHED=1
# EXIT trap performs cleanup, prints the summary, and sets aggregate exit status.
