#!/usr/bin/env bash
# Native Linux isolation v1.81 Package H acceptance (H15, matrix additions 16-27) + G2 chain.
#
#   sudo env DUDUCLAW_ISO_DISPOSABLE=1 [DUDUCLAW_ISO_TOOL_DRIVER=/abs/driver] \
#     scripts/live-test/linux-protected/package-h-acceptance.sh \
#       --candidate /abs/dir/with/duduclaw --tests-dir /abs/dir/with/test-binaries --out /abs/evidence-dir \
#       [--fixture-ip PUBLIC_ADDR] [--only g2,h15] [--allow-spawnd-reset] [--reset] [--keep]
#
# DESTRUCTIVE. Root on a DISPOSABLE Linux host (Ubuntu 22.04 / 24.04, Debian 12): installs the protected
# service, claims the first-run admin (TOTP), records host facts (kernel, Landlock ABI, distro, systemd,
# spawnd feature bits), optionally starts the public fixture origin + fixture skill hub, runs the prebuilt
# `iso_package_h_chain` (H15) and `iso_personal_chain` (G2) test binaries against the real service and
# spawnd, prints a PASS/FAIL/SKIP table and removes everything again.
# Exit status 1 on any FAIL (or an interrupted run). A SKIP is never a pass: it names what is missing.
# See docs/guides/linux-protected-acceptance.md and README.md in this directory.
set -euo pipefail
umask 077
PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
LC_ALL=C
export PATH LC_ALL

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
RPC_PY=$SCRIPT_DIR/connectors_rpc.py
FIXTURES=$SCRIPT_DIR/fixtures
STAGE=/root/duduclaw-package-h-acceptance
UNIT=duduclaw-gateway.service
DATA=/var/lib/duduclaw/.duduclaw
SERVICE_UNITS=(duduclaw-gateway.service duduclaw-spawnd.service duduclaw-spawnd.socket
  duduclaw-egress.service duduclaw-egress.socket duduclaw-dns-broker.service
  duduclaw-dns-broker.socket duduclaw-parse.socket duduclaw-updated.service)
FIXED_PATHS=(/var/lib/duduclaw /var/lib/duduclaw-protected /var/lib/duduclaw-spawnd
  /var/log/duduclaw-spawnd /var/lib/duduclaw-updated /var/lib/duduclaw-migrate
  /etc/duduclaw /etc/duduclaw-updated /opt/duduclaw /opt/duduclaw-isolated
  /usr/lib/duduclaw /usr/libexec/duduclaw /usr/local/libexec/duduclaw)
SERVICE_ACCOUNTS=(duduclaw duduclaw-runsup duduclaw-egress duduclaw-dl duduclaw-dns)
CA_FILE=/usr/local/share/ca-certificates/duduclaw-h15-acceptance.crt
ORIGIN_NAME=h15.fixture-duduclaw.net
REBIND_NAME=rebind.fixture-duduclaw.net
HUB_NAME=clawhub.ai

CANDIDATE='' TESTS='' OUT='' ONLY='g2,h15' FIXTURE_IP='' KEEP=0 RESET=0 ALLOW_RESET=0
PASSED=0 FAILED=0 SKIPPED=0 ARMED=0 FINISHED=0
TMP='' STATE='' BIN='' ORIGIN_PID='' SVC_UID='' SVC_GID='' OPHOME='' OPDUDU='' HOSTS_SAVED=0

usage() {
  sed -n '2,15p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
  cat >&2 <<'EOF'

Options:
  --candidate DIR       directory containing the candidate `duduclaw` binary (required)
  --tests-dir DIR       directory with the prebuilt test binaries named `iso_package_h_chain` and
                        `iso_personal_chain` (cargo test -p duduclaw-gateway --test <name> --no-run; copy and
                        rename the printed executables). Missing binaries are SKIP rows.
  --out DIR             evidence directory, created 0700 (required, must not exist or be empty)
  --fixture-ip ADDR     a GLOBAL (non-private) address of this host for the fixture origin + hub on :443
                        (/etc/hosts + a throw-away CA are installed for the run and removed afterwards).
                        Without it the web (19-20) and hub (23-24) checks SKIP.
  --only LIST           g2 and/or h15 (default g2,h15)
  --allow-spawnd-reset  let G2 move /var/lib/duduclaw-spawnd aside for the adoption step (G2-06)
  --reset               remove a previous installation made by this harness, then continue
  --keep                leave the installation in place (clean up later with --reset)
EOF
  exit 2
}

while [ "$#" -gt 0 ]; do
  case $1 in
    --candidate) [ "$#" -ge 2 ] || usage; CANDIDATE=$2; shift 2 ;;
    --tests-dir) [ "$#" -ge 2 ] || usage; TESTS=$2; shift 2 ;;
    --out) [ "$#" -ge 2 ] || usage; OUT=$2; shift 2 ;;
    --fixture-ip) [ "$#" -ge 2 ] || usage; FIXTURE_IP=$2; shift 2 ;;
    --only) [ "$#" -ge 2 ] || usage; ONLY=$2; shift 2 ;;
    --allow-spawnd-reset) ALLOW_RESET=1; shift ;;
    --reset) RESET=1; shift ;;
    --keep) KEEP=1; shift ;;
    -h|--help) usage ;;
    *) echo "unknown option: $1" >&2; usage ;;
  esac
done
[ -n "$CANDIDATE" ] && [ -n "$OUT" ] || usage

# ── results ──────────────────────────────────────────────────────────────────
log() { printf '%s\n' "$*" | tee -a "$OUT/run.log" >&2; }
record() { # STATUS ID DETAIL
  local status=$1 id=$2 detail=$3
  if [ ! -d "$OUT/evidence" ]; then printf '%-4s %-22s %s\n' "$status" "$id" "$detail"; return 0; fi
  printf '%-4s %-22s %s\n' "$status" "$id" "$detail" | tee -a "$OUT/run.log"
  printf '%s\t%s\t%s\n' "$status" "$id" "$detail" >> "$OUT/results.tsv"
  case $status in
    PASS) PASSED=$((PASSED + 1)) ;;
    FAIL) FAILED=$((FAILED + 1)) ;;
    SKIP) SKIPPED=$((SKIPPED + 1)) ;;
    *) ;;
  esac
}
pass() { record PASS "$1" "$2"; }
fail() { record FAIL "$1" "$2"; }
skip() { record SKIP "$1" "$2"; }
fact() { printf '%s\t%s\n' "$1" "$2" >> "$OUT/facts.tsv"; record INFO "fact:$1" "$2"; }
fatal() { record FAIL setup "$1"; exit 1; }
want() { case ",$ONLY," in *",$1,"*) return 0 ;; *) return 1 ;; esac; }

PY() { python3 -I "$RPC_PY" "$@"; }
RPC() { python3 -I "$RPC_PY" --data-home "$DATA" "$@"; }
jf() { PY jget "$1"; }
rpc_ok() { [ "$(printf '%s' "$1" | jf ok 2>/dev/null || true)" = true ]; }
err_of() { printf '%s' "$1" | jf error 2>/dev/null || echo '(no error text)'; }
iso_env() { env -i PATH="$PATH" HOME="$OPHOME" DUDUCLAW_HOME="$OPDUDU" LANG=C LC_ALL=C "$@"; }
as_service() { setpriv --reuid "$SVC_UID" --regid "$SVC_GID" --clear-groups "$@"; }

# ── lifecycle ────────────────────────────────────────────────────────────────
looks_installed() {
  local p
  for p in "${FIXED_PATHS[@]}" "/etc/systemd/system/$UNIT"; do
    if [ -e "$p" ] || [ -L "$p" ]; then return 0; fi
  done
  getent passwd duduclaw >/dev/null 2>&1 && return 0
  systemctl cat "$UNIT" >/dev/null 2>&1 && return 0
  return 1
}

undo_fixtures() {
  if [ -n "$ORIGIN_PID" ]; then kill "$ORIGIN_PID" 2>/dev/null; wait "$ORIGIN_PID" 2>/dev/null; ORIGIN_PID=''; fi
  if [ "$HOSTS_SAVED" -eq 1 ] && [ -f "$STAGE/hosts.orig" ]; then cat "$STAGE/hosts.orig" > /etc/hosts; HOSTS_SAVED=0; fi
  if [ -f "$CA_FILE" ]; then rm -f "$CA_FILE"; update-ca-certificates --fresh >/dev/null 2>&1; fi
  [ -z "$BIN" ] || rm -rf "$BIN"
}

cleanup() { # idempotent; only when the harness marker exists
  local unit account path
  set +e
  undo_fixtures
  if getent passwd duduclaw >/dev/null 2>&1; then loginctl disable-linger duduclaw >/dev/null 2>&1; fi
  if [ -x "$STAGE/duduclaw" ]; then
    iso_env "$STAGE/duduclaw" service uninstall --protected-linux >>"$OUT/cleanup.log" 2>&1 \
      || echo "uninstall command failed or nothing to uninstall" >>"$OUT/cleanup.log"
  fi
  for unit in "${SERVICE_UNITS[@]}"; do systemctl disable --now "$unit" >>"$OUT/cleanup.log" 2>&1; done
  rm -f /etc/systemd/system/duduclaw*.service /etc/systemd/system/duduclaw*.socket \
    /etc/systemd/system/duduclaw*.slice /etc/systemd/system/duduclaw*.timer
  for path in "${FIXED_PATHS[@]}" /var/lib/duduclaw-spawnd.g2-reset-*; do rm -rf "$path"; done
  for account in "${SERVICE_ACCOUNTS[@]}"; do
    if getent passwd "$account" >/dev/null 2>&1; then userdel "$account" >>"$OUT/cleanup.log" 2>&1; fi
  done
  for account in "${SERVICE_ACCOUNTS[@]}"; do
    if getent group "$account" >/dev/null 2>&1; then groupdel "$account" >>"$OUT/cleanup.log" 2>&1; fi
  done
  systemctl daemon-reload >>"$OUT/cleanup.log" 2>&1
  systemctl reset-failed >>"$OUT/cleanup.log" 2>&1
  set -e
  local leftovers=0
  for path in "${FIXED_PATHS[@]}" "/etc/systemd/system/$UNIT" "$CA_FILE"; do
    if [ -e "$path" ] || [ -L "$path" ]; then leftovers=1; echo "leftover: $path" >>"$OUT/cleanup.log"; fi
  done
  if getent passwd duduclaw >/dev/null 2>&1; then leftovers=1; echo "leftover: account duduclaw" >>"$OUT/cleanup.log"; fi
  if grep -q "$ORIGIN_NAME\|$HUB_NAME" /etc/hosts 2>/dev/null; then leftovers=1; echo "leftover: /etc/hosts entry" >>"$OUT/cleanup.log"; fi
  if [ "$leftovers" -eq 0 ]; then rm -rf "$STAGE"; record PASS cleanup 'installation, units, accounts, fixture CA and hosts entries removed'
  else record FAIL cleanup 'residue remains (see cleanup.log); staging kept for a later --reset'; fi
}

print_table() {
  [ -f "$OUT/results.tsv" ] || return 0
  {
    printf '\n%-6s %-34s %s\n' STATUS CHECK DETAIL
    printf '%-6s %-34s %s\n' ------ ---------------------------------- ------
    awk -F'\t' '$1 != "INFO" { printf "%-6s %-34s %s\n", $1, $2, substr($3, 1, 110) }' "$OUT/results.tsv"
    printf '\n%-8s %s\n%-8s %s\n%-8s %s\n%-8s %s\n' RESULT COUNT PASS "$PASSED" FAIL "$FAILED" SKIP "$SKIPPED"
  } | tee -a "$OUT/run.log" > "$OUT/summary.txt"
  cat "$OUT/summary.txt"
}

finish() {
  local code=$?
  trap - EXIT HUP INT TERM
  set +e
  if [ "$code" -ne 0 ] && [ "$FINISHED" -eq 0 ] && [ "$ARMED" -eq 1 ]; then record FAIL interrupted 'script stopped before all steps completed'; fi
  if [ "$ARMED" -eq 1 ] && [ -d "$OUT" ]; then
    journalctl -u 'duduclaw*' --no-pager -o short-iso >"$OUT/journal.log" 2>&1
    if [ "$KEEP" -eq 1 ]; then undo_fixtures; log 'installation retained (--keep); remove it with --reset'; else cleanup; fi
  fi
  [ -z "$TMP" ] || rm -rf "$TMP"
  [ -d "$OUT" ] && print_table
  if [ "$FAILED" -ne 0 ] || [ "$code" -ne 0 ]; then exit 1; fi
  exit 0
}
trap finish EXIT
trap 'exit 1' HUP INT TERM

preflight() {
  [ "$(uname -s)" = Linux ] || { echo 'refusing: Linux only' >&2; exit 1; }
  [ "$(id -u)" -eq 0 ] || { echo 'refusing: must run as root' >&2; exit 1; }
  if [ "${DUDUCLAW_ISO_DISPOSABLE:-}" != 1 ]; then
    echo 'refusing: installs and removes system services, accounts, /etc/hosts entries and a CA. Disposable hosts only;' >&2
    echo 'set DUDUCLAW_ISO_DISPOSABLE=1.' >&2
    exit 1
  fi
  if [ -n "${DUDUCLAW_HOME:-}" ]; then echo 'refusing: DUDUCLAW_HOME is set; the harness uses its own isolated home.' >&2; exit 1; fi
  [ -d "$CANDIDATE" ] && [ -x "$CANDIDATE/duduclaw" ] || { echo "no executable $CANDIDATE/duduclaw" >&2; exit 1; }
  CANDIDATE=$(cd "$CANDIDATE" && pwd -P)
  if [ -n "$TESTS" ]; then TESTS=$(cd "$TESTS" && pwd -P); fi
  case $OUT in /*) ;; *) OUT=$PWD/$OUT ;; esac
  if [ -e "$OUT" ] && [ -n "$(ls -A "$OUT" 2>/dev/null)" ]; then echo "refusing: --out $OUT is not empty" >&2; exit 1; fi
  mkdir -p "$OUT/evidence"; chmod 700 "$OUT"
  local tool
  for tool in systemctl journalctl loginctl setpriv python3 getent userdel groupdel stat awk sed grep date mktemp timeout; do
    command -v "$tool" >/dev/null 2>&1 || { echo "missing tool: $tool" >&2; exit 1; }
  done
  python3 -c 'import sys; sys.exit(sys.version_info < (3, 10))' || { echo 'Python 3.10+ required' >&2; exit 1; }
  [ "$(cat /proc/1/comm)" = systemd ] || { echo 'PID 1 must be systemd' >&2; exit 1; }
  [ "$(stat -f -c %T /sys/fs/cgroup)" = cgroup2fs ] || { echo 'unified cgroup v2 required' >&2; exit 1; }
  if looks_installed && [ ! -f "$STAGE/harness-owned" ]; then
    echo 'refusing: a DuDuClaw installation, account or home exists that this harness did not create.' >&2
    echo 'Use a fresh disposable host (connectors-acceptance.sh uses a different marker).' >&2
    exit 1
  fi
  : > "$OUT/results.tsv"; : > "$OUT/facts.tsv"
  TMP=$(mktemp -d /tmp/duduclaw-package-h-acc.XXXXXX)
  # The G2/H15 token probe re-executes the test binary as the service account and keeps 0600 token
  # files in a service-owned subdirectory of STATE: TMP and STATE must be traversable (never readable).
  chmod 0711 "$TMP"
  STATE=$TMP/state; OPHOME=$TMP/os-home; OPDUDU=$TMP/operator-home
  install -d -m 0711 "$STATE"; install -d -m 0700 "$OPHOME" "$OPDUDU"
  if looks_installed; then
    [ "$RESET" -eq 1 ] || { echo 'a previous harness installation exists; rerun with --reset' >&2; exit 1; }
    ARMED=1; log 'removing previous harness installation (--reset)'; cleanup
  fi
  ARMED=1
}

host_facts() {
  # shellcheck disable=SC1091
  . /etc/os-release
  fact distro "${PRETTY_NAME:-unknown} (${ID:-?}-${VERSION_ID:-?})"
  fact kernel "$(uname -srm)"
  fact systemd "$(systemctl --version | head -n 1)"
  fact landlock_abi "$(python3 -I -c 'import ctypes;l=ctypes.CDLL(None,use_errno=True);print(l.syscall(444,None,0,1))' 2>/dev/null || echo unknown)"
  fact lsm "$(cat /sys/kernel/security/lsm 2>/dev/null || echo unknown)"
  fact cgroup "$(stat -f -c %T /sys/fs/cgroup)"
  case "${ID:-}-${VERSION_ID:-}" in
    ubuntu-22.04|ubuntu-24.04|debian-12) pass matrix "host is in the acceptance matrix (${ID}-${VERSION_ID})" ;;
    *) skip matrix "${ID:-?}-${VERSION_ID:-?} is outside the matrix (Ubuntu 22.04 / 24.04, Debian 12): results are informational" ;;
  esac
}

install_service() {
  install -d -o root -g root -m 0700 "$STAGE"
  printf '%s\n' "$$" > "$STAGE/harness-owned"
  install -o root -g root -m 0755 "$CANDIDATE/duduclaw" "$STAGE/duduclaw"
  record INFO candidate "$(iso_env "$STAGE/duduclaw" --version 2>&1 | head -n 1)"
  if ! iso_env "$STAGE/duduclaw" service install --protected-linux --i-verified-this-binary \
      --enable-now --allow-missing-runtime >"$OUT/install.log" 2>&1; then
    fatal 'protected service install failed (see install.log)'
  fi
  getent passwd duduclaw >/dev/null || fatal 'service account duduclaw missing after install'
  SVC_UID=$(id -u duduclaw); SVC_GID=$(id -g duduclaw)
  local r
  r=$(RPC wait --timeout 120)
  rpc_ok "$r" || fatal "gateway not reachable: $(err_of "$r")"
  systemctl is-active --quiet duduclaw-spawnd.service || systemctl is-active --quiet duduclaw-spawnd.socket \
    || fatal 'duduclaw-spawnd neither active nor socket-activated'
  pass install 'protected service installed; gateway reachable; spawnd active or socket-activated'
  # spawnd evidence (root may read it): feature bits and the recording probe.
  local features
  features=$(python3 -I -c 'import json;r=json.load(open("/run/duduclaw-spawnd/readiness.json"));f=int(r.get("features",0));print("bits=%d work_broker=%s helper_calls=%s helper_recording=%s landlock=%s recording_probe=%s"%(f,bool(f&2),bool(f&1),bool(f&4),r.get("landlock"),"present" if r.get("recording_probe") else "absent"))' 2>/dev/null || echo unreadable)
  fact spawnd_features "$features"
}

setup_admin() {
  local pw r
  pw=$(python3 -c 'import secrets;print("Aa1-"+secrets.token_hex(10))')
  r=$(RPC --state "$STATE/admin.json" claim --email admin@iso.test --password "$pw")
  rpc_ok "$r" || fatal "first-run admin claim failed: $(err_of "$r")"
  r=$(RPC --state "$STATE/admin.json" login); rpc_ok "$r" || fatal "admin login failed: $(err_of "$r")"
  r=$(RPC --state "$STATE/admin.json" enroll-mfa); rpc_ok "$r" || fatal "admin TOTP enrolment failed: $(err_of "$r")"
  pass admin 'first-run admin claimed, logged in, TOTP enrolled (dashboard step-up capable)'
}

is_global_ip() { python3 -I -c 'import ipaddress,sys;sys.exit(0 if ipaddress.ip_address(sys.argv[1]).is_global else 1)' "$1" 2>/dev/null; }

start_fixtures() {
  if [ -z "$FIXTURE_IP" ]; then skip fixtures 'no --fixture-ip: web (19-20) and hub (23-24) checks will SKIP'; return 0; fi
  if ! is_global_ip "$FIXTURE_IP"; then
    skip fixtures "--fixture-ip $FIXTURE_IP is not a global address: PublicOrigins fetches only public addresses, so the fixture would be refused by design"
    return 0
  fi
  command -v openssl >/dev/null 2>&1 && command -v update-ca-certificates >/dev/null 2>&1 \
    || { skip fixtures 'openssl / update-ca-certificates missing'; return 0; }
  local dir=$TMP/origin nobody_uid nobody_gid
  nobody_uid=$(id -u nobody); nobody_gid=$(id -g nobody)
  install -d -m 0711 "$dir"
  openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj '/CN=DuDuClaw H15 throw-away CA' \
    -addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign \
    -keyout "$dir/ca.key" -out "$dir/ca.pem" >/dev/null 2>&1 || { skip fixtures 'CA generation failed'; return 0; }
  openssl req -newkey rsa:2048 -nodes -subj "/CN=$ORIGIN_NAME" -keyout "$dir/key.pem" -out "$dir/leaf.csr" >/dev/null 2>&1
  printf 'subjectAltName=DNS:%s,DNS:%s,DNS:%s\nextendedKeyUsage=serverAuth\n' "$ORIGIN_NAME" "$REBIND_NAME" "$HUB_NAME" > "$dir/ext.cnf"
  openssl x509 -req -in "$dir/leaf.csr" -CA "$dir/ca.pem" -CAkey "$dir/ca.key" -CAcreateserial -days 1 \
    -extfile "$dir/ext.cnf" -out "$dir/cert.pem" >/dev/null 2>&1 || { skip fixtures 'leaf certificate failed'; return 0; }
  install -m 0644 "$dir/ca.pem" "$CA_FILE"; update-ca-certificates >/dev/null 2>&1
  cp /etc/hosts "$STAGE/hosts.orig"; HOSTS_SAVED=1
  printf '%s %s %s\n127.0.0.1 %s\n' "$FIXTURE_IP" "$ORIGIN_NAME" "$HUB_NAME" "$REBIND_NAME" >> /etc/hosts
  install -m 0644 "$FIXTURES/public_origin.py" "$dir/public_origin.py"
  chown "$nobody_uid:$nobody_gid" "$dir/key.pem" "$dir/cert.pem"; chmod 0400 "$dir/key.pem"
  setpriv --reuid "$nobody_uid" --regid "$nobody_gid" --clear-groups --inh-caps +net_bind_service \
    --ambient-caps +net_bind_service python3 -I "$dir/public_origin.py" --bind "$FIXTURE_IP" --port 443 \
    --cert "$dir/cert.pem" --key "$dir/key.pem" --log "$dir/hits.jsonl" >"$dir/origin.log" 2>&1 &
  ORIGIN_PID=$!
  sleep 1
  kill -0 "$ORIGIN_PID" 2>/dev/null || { ORIGIN_PID=''; skip fixtures 'fixture origin did not start (see evidence/origin.log)'; return 0; }
  # clawhub must be an enabled hub for the protected install path.
  if ! grep -q '^\[skill_hubs\]' "$DATA/config.toml"; then
    printf '\n[skill_hubs]\nenabled = ["clawhub"]\n' | as_service tee -a "$DATA/config.toml" >/dev/null
  fi
  export DUDUCLAW_ISO_PUBLIC_ORIGIN="https://$ORIGIN_NAME" DUDUCLAW_ISO_REBIND_URL="https://$REBIND_NAME/h15/ok" DUDUCLAW_ISO_HUB_FIXTURE=1
  pass fixtures "fixture origin https://$ORIGIN_NAME + hub $HUB_NAME on $FIXTURE_IP:443 (CA trusted for this run; the product's DNS/TLS stack must honour /etc/hosts and the system store)"
}

# ── chain test binaries ──────────────────────────────────────────────────────
run_chain() { # NAME FILTER LABEL
  local name=$1 filter=$2 label=$3 src rc=0 line status id detail
  src=${TESTS:+$TESTS/$name}
  if [ -z "$src" ] || [ ! -x "$src" ]; then skip "$label" "test binary $name not supplied (--tests-dir; cargo test -p duduclaw-gateway --test $name --no-run)"; return 0; fi
  [ -n "$BIN" ] || { BIN=$(mktemp -d /var/tmp/duduclaw-h15-bin.XXXXXX); chmod 0755 "$BIN"; }
  install -o root -g root -m 0755 "$src" "$BIN/$name"
  log "── $name ($filter) ──"
  timeout 3600 env -i PATH="$PATH" LC_ALL=C HOME=/root DUDUCLAW_ISO_DISPOSABLE=1 DUDUCLAW_ISO_DATA="$DATA" \
    DUDUCLAW_ISO_STATE_DIR="$STATE" DUDUCLAW_ISO_RPC="$RPC_PY" DUDUCLAW_ISO_OUT="$OUT/evidence" \
    ${DUDUCLAW_ISO_TOOL_DRIVER:+DUDUCLAW_ISO_TOOL_DRIVER="$DUDUCLAW_ISO_TOOL_DRIVER"} \
    ${DUDUCLAW_ISO_PUBLIC_ORIGIN:+DUDUCLAW_ISO_PUBLIC_ORIGIN="$DUDUCLAW_ISO_PUBLIC_ORIGIN"} \
    ${DUDUCLAW_ISO_REBIND_URL:+DUDUCLAW_ISO_REBIND_URL="$DUDUCLAW_ISO_REBIND_URL"} \
    ${DUDUCLAW_ISO_HUB_FIXTURE:+DUDUCLAW_ISO_HUB_FIXTURE="$DUDUCLAW_ISO_HUB_FIXTURE"} \
    $( [ "$ALLOW_RESET" -eq 1 ] && echo DUDUCLAW_ISO_G2_ALLOW_SPAWND_RESET=1 ) \
    "$BIN/$name" --ignored --test-threads=1 --nocapture "$filter" >"$OUT/$name.log" 2>&1 || rc=$?
  local seen=0
  while IFS= read -r line; do
    case $line in
      PASS\ *|FAIL\ *|SKIP\ *)
        status=${line%% *}; line=${line#* }; id=${line%% *}; detail=${line#* }
        record "$status" "$label:$id" "$detail"; seen=1 ;;
      FACT\ *) printf 'chain\t%s\n' "${line#FACT }" >> "$OUT/facts.tsv" ;;
      *) ;;
    esac
  done < "$OUT/$name.log"
  if grep -q '^test result: ok' "$OUT/$name.log" && [ "$rc" -eq 0 ]; then pass "$label" "$name: every selected test passed (rc 0)"
  else fail "$label" "$name exited rc=$rc (see $name.log)"; fi
  [ "$seen" -eq 1 ] || fail "$label:none" "$name printed no PASS/FAIL/SKIP line (filter $filter matched nothing?)"
}

# ── main ─────────────────────────────────────────────────────────────────────
preflight
host_facts
install_service
setup_admin
if [ -n "${DUDUCLAW_ISO_TOOL_DRIVER:-}" ] && [ -x "${DUDUCLAW_ISO_TOOL_DRIVER}" ]; then pass driver "tool driver $DUDUCLAW_ISO_TOOL_DRIVER"
else skip driver 'no DUDUCLAW_ISO_TOOL_DRIVER: every live employee tool call SKIPs (see README "Tool driver contract")'; fi
start_fixtures
if want h15; then run_chain iso_package_h_chain iso_root_h15_ H15; fi
if want g2; then run_chain iso_personal_chain iso_root_e2e_ G2; fi
[ ! -f "$TMP/origin/hits.jsonl" ] || cp "$TMP/origin/hits.jsonl" "$OUT/evidence/origin-hits.jsonl"
[ ! -f "$TMP/origin/origin.log" ] || cp "$TMP/origin/origin.log" "$OUT/evidence/origin.log"
FINISHED=1
