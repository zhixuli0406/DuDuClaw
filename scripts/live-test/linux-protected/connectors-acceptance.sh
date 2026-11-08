#!/usr/bin/env bash
# Native Linux isolation v1.81 connector / Personal-binding acceptance (matrix items 1-15).
#
#   sudo DUDUCLAW_ISO_DISPOSABLE=1 \
#     scripts/live-test/linux-protected/connectors-acceptance.sh \
#       --candidate /abs/dir/with/duduclaw --out /abs/evidence-dir [--only 2,4] [--reset] [--keep]
#
# DESTRUCTIVE. Runs as root on a DISPOSABLE Linux host (Ubuntu 22.04 / 24.04, Debian 12): it
# installs the protected service (`duduclaw service install --protected-linux`), creates
# fixture users alice (owner) / bob (+ scratch users carol, dave), a Personal employee
# alice-personal = Personal(alice) and a Shared employee `team`, runs the numbered checks and
# removes everything again. Supersedes acceptance.sh (container_root_key backend, removed).
# See README.md in this directory for prerequisites, environment variables and how to read results.
#
# Result lines: PASS / FAIL / SKIP <id> <detail>. A SKIP is never a pass: it names what is
# missing (provider account, tool driver, an unrecognised RPC shape). Exit status 1 on any FAIL.
set -euo pipefail
umask 077
PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
LC_ALL=C
export PATH LC_ALL

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
RPC_PY=$SCRIPT_DIR/connectors_rpc.py
FIXTURES=$SCRIPT_DIR/fixtures
STAGE=/root/duduclaw-connectors-acceptance
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
ALL_ITEMS=(01 02 03 05 11 12 14 15 04 06 13 07 08 09 10) # destructive/restarting items last
CANARY_PORT=${DUDUCLAW_ISO_CANARY_PORT:-18943}

CANDIDATE='' OUT='' ONLY='' KEEP=0 RESET=0 PAUSE=0
PASSED=0 FAILED=0 SKIPPED=0 ARMED=0 FINISHED=0
TMP='' STATE='' CANARY_PID='' SVC_UID='' SVC_GID='' OPHOME='' OPDUDU='' ADMIN_PW=''
declare -A USER_ID=() USER_PW=()
DB_AGENTS=()

usage() {
  sed -n '2,15p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
  cat >&2 <<'EOF'

Options:
  --candidate DIR   directory containing the candidate `duduclaw` binary (required)
  --out DIR         evidence/log directory, created 0700 (required, must not exist or be empty)
  --only LIST       comma separated item numbers to run (default: all 1-15)
  --reset           remove a previous installation made by this harness, then continue
  --keep            leave the installation and fixture in place (clean up later with --reset)
  --pause-after-setup  stop after fixture setup so provider accounts can be connected by hand
EOF
  exit 2
}

# ── option parsing ───────────────────────────────────────────────────────────
while [ "$#" -gt 0 ]; do
  case $1 in
    --candidate) [ "$#" -ge 2 ] || usage; CANDIDATE=$2; shift 2 ;;
    --out) [ "$#" -ge 2 ] || usage; OUT=$2; shift 2 ;;
    --only) [ "$#" -ge 2 ] || usage; ONLY=$2; shift 2 ;;
    --reset) RESET=1; shift ;;
    --keep) KEEP=1; shift ;;
    --pause-after-setup) PAUSE=1; shift ;;
    -h|--help) usage ;;
    *) echo "unknown option: $1" >&2; usage ;;
  esac
done
[ -n "$CANDIDATE" ] && [ -n "$OUT" ] || usage

# ── logging / results ────────────────────────────────────────────────────────
log() { printf '%s\n' "$*" | tee -a "$OUT/run.log" >&2; }
record() { # STATUS ID DETAIL
  local status=$1 id=$2 detail=$3
  if [ ! -d "$OUT/evidence" ]; then printf '%-4s %-5s %s\n' "$status" "$id" "$detail"; return 0; fi
  printf '%-4s %-5s %s\n' "$status" "$id" "$detail" | tee -a "$OUT/run.log"
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
fatal() { record FAIL setup "$1"; exit 1; }
ev() { cat > "$OUT/evidence/$1.json"; } # evidence from stdin

want() { # item number selected?
  [ -z "$ONLY" ] && return 0
  local n=$((10#$1)) x
  # shellcheck disable=SC2086 # intentional word splitting of the comma list
  for x in ${ONLY//,/ }; do
    if [ "$((10#$x))" -eq "$n" ]; then return 0; fi
  done
  return 1
}

now_ms() { date +%s%N | cut -c1-13; }

# ── RPC helpers (connectors_rpc.py) ──────────────────────────────────────────
PY() { python3 "$RPC_PY" "$@"; }
RPC() { python3 "$RPC_PY" --data-home "$DATA" "$@"; }
rpc_as() { local who=$1; shift; RPC --state "$STATE/$who.json" "$@"; }
adm() { rpc_as admin call "$@"; } # adm METHOD [PARAMS] [--stepup]
jf() { PY jget "$1"; }            # stdin JSON -> value at path (exit 1 if missing)
rpc_ok() { [ "$(printf '%s' "$1" | jf ok 2>/dev/null || true)" = true ]; }
err_of() { printf '%s' "$1" | jf error 2>/dev/null || echo '(no error text)'; }
json_str() { python3 -c 'import json,sys;print(json.dumps(sys.argv[1]))' "$1"; }

as_service() { setpriv --reuid "$SVC_UID" --regid "$SVC_GID" --clear-groups "$@"; }
iso_env() { env -i PATH="$PATH" HOME="$OPHOME" DUDUCLAW_HOME="$OPDUDU" LANG=C LC_ALL=C "$@"; }

# ── optional tool driver (see README "Tool driver contract") ─────────────────
have_driver() { [ -n "${DUDUCLAW_ISO_TOOL_DRIVER:-}" ] && [ -x "$DUDUCLAW_ISO_TOOL_DRIVER" ]; }
driver() {
  env DUDUCLAW_ISO_DATA="$DATA" DUDUCLAW_ISO_OUT="$OUT" DUDUCLAW_ISO_RPC="$RPC_PY" \
    DUDUCLAW_ISO_STATE_DIR="$STATE" DUDUCLAW_ISO_CANARY_URL="https://127.0.0.1:$CANARY_PORT" \
    "$DUDUCLAW_ISO_TOOL_DRIVER" "$@"
}
DRV_RAW='' DRV_OUTCOME='' DRV_CODE=''
drv() { # SUBCOMMAND ARGS... -> DRV_RAW / DRV_OUTCOME / DRV_CODE (driver prints one JSON line)
  DRV_RAW=$(driver "$@" 2>>"$OUT/driver.err" || true)
  DRV_OUTCOME=$(printf '%s' "$DRV_RAW" | jf outcome 2>/dev/null || echo error)
  DRV_CODE=$(printf '%s' "$DRV_RAW" | jf code 2>/dev/null || true)
}
drv_field() { printf '%s' "$DRV_RAW" | jf "$1" 2>/dev/null || true; }
need_driver() { # ID DESC -> returns 1 (and records SKIP) without a driver
  have_driver && return 0
  skip "$1" "$2: needs DUDUCLAW_ISO_TOOL_DRIVER (live employee tool calls)"
  return 1
}
drv_expect() { # ID DESC EXPECTED(ok|refused) LABEL TOOL ARGS
  local id=$1 desc=$2 expected=$3
  shift 3
  drv call "$@"
  printf '%s\n' "$DRV_RAW" > "$OUT/evidence/$id-driver.json"
  if [ "$DRV_OUTCOME" = "$expected" ]; then pass "$id" "$desc (outcome=$DRV_OUTCOME${DRV_CODE:+ code=$DRV_CODE})"
  else fail "$id" "$desc: expected $expected, driver reported ${DRV_OUTCOME}${DRV_CODE:+ code=$DRV_CODE}"; fi
}
# Provider gating: ENV must be 1 (operator asserts the account is connected) and a driver exists.
need_provider() { # ID DESC ENVNAME
  local id=$1 desc=$2 name=$3
  if [ "${!name:-}" = "" ] || [ "${!name:-}" = 0 ]; then skip "$id" "$desc: provider account not configured ($name unset)"; return 1; fi
  need_driver "$id" "$desc"
}

# ── lifecycle: preflight, install, cleanup ───────────────────────────────────
looks_installed() {
  local p
  for p in "${FIXED_PATHS[@]}" "/etc/systemd/system/$UNIT"; do
    if [ -e "$p" ] || [ -L "$p" ]; then return 0; fi
  done
  getent passwd duduclaw >/dev/null 2>&1 && return 0
  systemctl cat "$UNIT" >/dev/null 2>&1 && return 0
  return 1
}

cleanup() { # idempotent; only ever called when the harness marker exists
  local unit account path
  set +e
  if [ -n "$CANARY_PID" ]; then kill "$CANARY_PID" 2>/dev/null; wait "$CANARY_PID" 2>/dev/null; CANARY_PID=''; fi
  if getent passwd duduclaw >/dev/null 2>&1; then loginctl disable-linger duduclaw >/dev/null 2>&1; fi
  if [ -x "$STAGE/duduclaw" ]; then
    iso_env "$STAGE/duduclaw" service uninstall --protected-linux >>"$OUT/cleanup.log" 2>&1 \
      || echo "uninstall command failed or nothing to uninstall" >>"$OUT/cleanup.log"
  fi
  for unit in "${SERVICE_UNITS[@]}"; do systemctl disable --now "$unit" >>"$OUT/cleanup.log" 2>&1; done
  rm -f /etc/systemd/system/duduclaw*.service /etc/systemd/system/duduclaw*.socket \
    /etc/systemd/system/duduclaw*.slice /etc/systemd/system/duduclaw*.timer
  for path in "${FIXED_PATHS[@]}"; do rm -rf "$path"; done
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
  for path in "${FIXED_PATHS[@]}" "/etc/systemd/system/$UNIT"; do
    if [ -e "$path" ] || [ -L "$path" ]; then leftovers=1; echo "leftover: $path" >>"$OUT/cleanup.log"; fi
  done
  if getent passwd duduclaw >/dev/null 2>&1; then leftovers=1; echo "leftover: account duduclaw" >>"$OUT/cleanup.log"; fi
  if [ "$leftovers" -eq 0 ]; then
    rm -rf "$STAGE"
    record PASS cleanup 'installation, units and service accounts removed'
  else
    record FAIL cleanup "residue remains (see cleanup.log); staging kept for a later --reset"
  fi
}

finish() {
  local code=$?
  trap - EXIT HUP INT TERM
  set +e
  if [ "$code" -ne 0 ] && [ "$FINISHED" -eq 0 ] && [ "$ARMED" -eq 1 ]; then
    record FAIL interrupted 'script stopped before all steps completed'
  fi
  if [ "$ARMED" -eq 1 ] && [ -d "$OUT" ]; then
    journalctl -u 'duduclaw*' --no-pager -o short-iso >"$OUT/journal.log" 2>&1
    if [ "$KEEP" -eq 1 ]; then
      [ -z "$CANARY_PID" ] || kill "$CANARY_PID" 2>/dev/null
      log 'installation retained (--keep); remove it with --reset'
    else
      cleanup
    fi
  fi
  [ -z "$TMP" ] || rm -rf "$TMP"
  if [ -d "$OUT" ]; then
    {
      printf '\n%-8s %s\n' RESULT COUNT
      printf '%-8s %s\n%-8s %s\n%-8s %s\n' PASS "$PASSED" FAIL "$FAILED" SKIP "$SKIPPED"
    } | tee -a "$OUT/run.log" "$OUT/summary.txt"
    if [ -f "$OUT/results.tsv" ]; then sort -t$'\t' -k2,2 -s "$OUT/results.tsv" > "$OUT/results.sorted.tsv"; fi
  fi
  if [ "$FAILED" -ne 0 ] || [ "$code" -ne 0 ]; then exit 1; fi
  exit 0
}
trap finish EXIT
trap 'exit 1' HUP INT TERM

preflight() {
  [ "$(uname -s)" = Linux ] || { echo 'refusing: Linux only' >&2; exit 1; }
  [ "$(id -u)" -eq 0 ] || { echo 'refusing: must run as root' >&2; exit 1; }
  if [ "${DUDUCLAW_ISO_DISPOSABLE:-}" != 1 ]; then
    echo 'refusing: this installs and removes system services, accounts and /var/lib/duduclaw.' >&2
    echo 'Run it only on a disposable host and set DUDUCLAW_ISO_DISPOSABLE=1.' >&2
    exit 1
  fi
  if [ -n "${DUDUCLAW_HOME:-}" ]; then
    echo 'refusing: DUDUCLAW_HOME is set in the environment; the harness sets its own isolated home.' >&2
    exit 1
  fi
  [ -d "$CANDIDATE" ] && [ -x "$CANDIDATE/duduclaw" ] || { echo "no executable $CANDIDATE/duduclaw" >&2; exit 1; }
  CANDIDATE=$(cd "$CANDIDATE" && pwd -P)
  case $OUT in /*) ;; *) OUT=$PWD/$OUT ;; esac
  if [ -e "$OUT" ] && [ -n "$(ls -A "$OUT" 2>/dev/null)" ]; then echo "refusing: --out $OUT is not empty" >&2; exit 1; fi
  mkdir -p "$OUT/evidence"
  chmod 700 "$OUT"
  local tool
  for tool in systemctl journalctl loginctl setpriv python3 getent useradd userdel groupdel sha256sum \
      stat awk sed grep cut date sort mktemp; do
    command -v "$tool" >/dev/null 2>&1 || { echo "missing tool: $tool" >&2; exit 1; }
  done
  python3 -c 'import sys; sys.exit(sys.version_info < (3, 10))' || { echo 'Python 3.10+ required' >&2; exit 1; }
  [ "$(cat /proc/1/comm)" = systemd ] || { echo 'PID 1 must be systemd' >&2; exit 1; }
  [ "$(stat -f -c %T /sys/fs/cgroup)" = cgroup2fs ] || { echo 'unified cgroup v2 required' >&2; exit 1; }
  [ -f "$RPC_PY" ] && [ -f "$SCRIPT_DIR/rpc_probe.py" ] || { echo 'connectors_rpc.py / rpc_probe.py missing' >&2; exit 1; }
  # A DuDuClaw home that is not ours is a real install: never touch it, not even with --reset.
  if looks_installed && [ ! -f "$STAGE/harness-owned" ]; then
    echo 'refusing: a DuDuClaw installation, account or home already exists and was not created by' >&2
    echo 'this harness (no marker). Use a fresh disposable host.' >&2
    exit 1
  fi
  : > "$OUT/results.tsv"
  TMP=$(mktemp -d /tmp/duduclaw-connectors-acc.XXXXXX)
  STATE=$TMP/state; OPHOME=$TMP/os-home; OPDUDU=$TMP/operator-home
  install -d -m 0700 "$STATE" "$OPHOME" "$OPDUDU"
  if looks_installed; then
    if [ "$RESET" -ne 1 ]; then
      echo 'a previous harness installation exists; rerun with --reset to remove it first' >&2
      exit 1
    fi
    ARMED=1
    log 'removing previous harness installation (--reset)'
    cleanup
  fi
  ARMED=1
  # shellcheck disable=SC1091
  . /etc/os-release
  record INFO host "$(uname -srm); ${PRETTY_NAME:-unknown distro}; $(systemctl --version | head -n 1)"
  case "${ID:-}-${VERSION_ID:-}" in
    ubuntu-22.04|ubuntu-24.04|debian-12) ;;
    *) log "note: ${ID:-?}-${VERSION_ID:-?} is outside the acceptance matrix (Ubuntu 22.04/24.04, Debian 12)" ;;
  esac
  record INFO candidate "$(iso_env "$CANDIDATE/duduclaw" --version 2>&1 | head -n 1)"
}

install_service() {
  install -d -o root -g root -m 0700 "$STAGE"
  printf '%s\n' "$$" > "$STAGE/harness-owned"
  install -o root -g root -m 0755 "$CANDIDATE/duduclaw" "$STAGE/duduclaw"
  # The service has fixed system paths and its own account; HOME/DUDUCLAW_HOME above only isolate the
  # operator CLI, so it can never read or write an operator's real ~/.duduclaw.
  if ! iso_env "$STAGE/duduclaw" service install --protected-linux --i-verified-this-binary \
      --enable-now --allow-missing-runtime >"$OUT/install.log" 2>&1; then
    fatal 'protected service install failed (see install.log)'
  fi
  pass install 'protected service installed with --enable-now'
  getent passwd duduclaw >/dev/null || fatal 'service account duduclaw missing after install'
  SVC_UID=$(id -u duduclaw); SVC_GID=$(id -g duduclaw)
  [ -d "$DATA" ] || fatal "service home $DATA missing after install"
  local r
  r=$(RPC wait --timeout 120)
  rpc_ok "$r" || fatal "gateway not reachable: $(err_of "$r")"
  systemctl is-active --quiet "$UNIT" || fatal 'gateway unit not active'
  systemctl is-active --quiet duduclaw-spawnd.service || systemctl is-active --quiet duduclaw-spawnd.socket \
    || fatal 'duduclaw-spawnd neither active nor socket-activated'
  pass services 'gateway active; spawnd active or socket-activated'
}

genpass() { python3 -c 'import secrets;print("Aa1-"+secrets.token_hex(10))'; }

start_canary() {
  command -v openssl >/dev/null 2>&1 || { log 'openssl missing: HTTPS canary origin disabled'; return 0; }
  local dir=$TMP/canary nobody_uid nobody_gid
  nobody_uid=$(id -u nobody); nobody_gid=$(id -g nobody)
  install -d -m 0700 -o "$nobody_uid" -g "$nobody_gid" "$dir"
  openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj /CN=localhost \
    -keyout "$dir/key.pem" -out "$dir/cert.pem" >/dev/null 2>&1 || { log 'openssl failed: canary disabled'; return 0; }
  install -m 0600 -o "$nobody_uid" -g "$nobody_gid" "$FIXTURES/https_origin.py" "$dir/https_origin.py"
  chown "$nobody_uid:$nobody_gid" "$dir/key.pem" "$dir/cert.pem"
  setpriv --reuid "$nobody_uid" --regid "$nobody_gid" --clear-groups \
    python3 "$dir/https_origin.py" --port "$CANARY_PORT" --cert "$dir/cert.pem" --key "$dir/key.pem" \
    --log "$dir/hits.jsonl" >"$dir/origin.log" 2>&1 &
  CANARY_PID=$!
  sleep 1
  if kill -0 "$CANARY_PID" 2>/dev/null; then record INFO canary "https://127.0.0.1:$CANARY_PORT (request log $dir/hits.jsonl, copied to evidence at exit)"
  else CANARY_PID=''; log 'canary origin did not start'; fi
}
canary_hits() { if [ -f "$TMP/canary/hits.jsonl" ]; then wc -l < "$TMP/canary/hits.jsonl"; else echo -1; fi; }

# ── fixture: users, employees, local database connector ─────────────────────
create_user() { # name
  local who=$1 pw email r
  pw=$(genpass); email="$who@iso.test"
  r=$(adm users.create "{\"email\":\"$email\",\"display_name\":\"$who\",\"password\":\"$pw\",\"role\":\"employee\"}")
  rpc_ok "$r" || fatal "users.create $who failed: $(err_of "$r")"
  r=$(RPC --state "$STATE/$who.json" login --email "$email" --password "$pw")
  rpc_ok "$r" || fatal "login $who failed: $(err_of "$r")"
  USER_ID[$who]=$(printf '%s' "$r" | jf payload.user_id) || fatal "no user id for $who"
  USER_PW[$who]=$pw
}

employee_json() { adm isolation.employee_list | PY employee "$1"; }
employee_id() { employee_json "$1" | jf employee_id; }

request_id_of() { # JSON-with-payload -> request id
  local json=$1 p v
  for p in payload.request_id payload.id payload.request.request_id payload.request.id; do
    if v=$(printf '%s' "$json" | jf "$p" 2>/dev/null) && [ -n "$v" ]; then printf '%s' "$v"; return 0; fi
  done
  return 1
}

approve_label() { # label -> approve + apply the open Employee proposal
  local label=$1 listing binding r
  listing=$(adm migration.confirmations.list)
  binding=$(printf '%s' "$listing" | PY pick-proposal "$label") || { log "no open proposal for $label"; return 1; }
  r=$(adm migration.confirmations.approve "{\"bindings\":[$binding],\"csrf_token\":\"@csrf\"}" --stepup)
  printf '%s' "$r" | ev "approve-$label"
  rpc_ok "$r" || { log "approve $label refused: $(err_of "$r")"; return 1; }
}

wait_employee() { # label [seconds]
  local label=$1 i max=${2:-60}
  for ((i = 0; i < max; i++)); do
    if employee_json "$label" >/dev/null 2>&1; then return 0; fi
    sleep 1
  done
  return 1
}

ensure_personal() { # owner label : staff request -> admin proposal -> approval card -> applied
  local owner=$1 label=$2 resp rid node
  if employee_json "$label" >/dev/null 2>&1; then return 0; fi
  resp=$(rpc_as "$owner" call me.isolation.request_personal_employee "{\"label\":\"$label\",\"display_name\":\"$label\"}")
  printf '%s' "$resp" | ev "request-$label"
  rpc_ok "$resp" || { log "request $label refused: $(err_of "$resp")"; return 1; }
  if ! rid=$(request_id_of "$resp"); then
    resp=$(rpc_as "$owner" call me.isolation.list_personal_employee_requests)
    node=$(printf '%s' "$resp" | PY find label "$label") || { log "request $label not listed"; return 1; }
    rid=$(request_id_of "{\"payload\":$node}") || { log "no request id for $label"; return 1; }
  fi
  resp=$(adm isolation.employee_propose "{\"label\":\"$label\",\"binding_kind\":\"personal\",\"request_id\":\"$rid\",\"csrf_token\":\"@csrf\"}" --stepup)
  printf '%s' "$resp" | ev "propose-$label"
  rpc_ok "$resp" || { log "propose $label refused: $(err_of "$resp")"; return 1; }
  approve_label "$label" || return 1
  wait_employee "$label" || { log "employee $label not confirmed in time"; return 1; }
}

ensure_shared() { # label
  local label=$1 resp
  if employee_json "$label" >/dev/null 2>&1; then return 0; fi
  resp=$(adm isolation.employee_propose "{\"label\":\"$label\",\"display_name\":\"$label\",\"binding_kind\":\"shared\",\"csrf_token\":\"@csrf\"}" --stepup)
  printf '%s' "$resp" | ev "propose-$label"
  rpc_ok "$resp" || { log "propose $label refused: $(err_of "$resp")"; return 1; }
  approve_label "$label" || return 1
  wait_employee "$label" || return 1
}

grant_db() { # agent... : (re)grant both fixture sources to this agent as well
  local a list r name
  for a in "$@"; do DB_AGENTS+=("$a"); done
  list=$(printf '"%s",' "${DB_AGENTS[@]}"); list="[${list%,}]"
  for name in fixturedb fixtureopen; do
    r=$(adm db_sources.grants.set "{\"name\":\"$name\",\"agents\":$list}")
    rpc_ok "$r" || { log "grant $name failed: $(err_of "$r")"; return 1; }
  done
}

setup_db_fixture() {
  local r
  install -d -m 0700 -o "$SVC_UID" -g "$SVC_GID" "$DATA/fixtures"
  # The service account cannot read this repository checkout (often under /root): hand it a copy.
  install -m 0600 -o "$SVC_UID" -g "$SVC_GID" "$FIXTURES/tamper_users_db.py" "$DATA/fixtures/tamper_users_db.py"
  as_service python3 - "$DATA/fixtures/fixture.db" <<'PY'
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
db.execute("CREATE TABLE IF NOT EXISTS t (id INTEGER PRIMARY KEY, name TEXT)")
db.execute("DELETE FROM t")
db.executemany("INSERT INTO t(id, name) VALUES(?, ?)", [(1, "alpha"), (2, "beta"), (3, "gamma")])
db.commit()
PY
  r=$(adm db_sources.upsert "{\"name\":\"fixturedb\",\"driver\":\"sqlite\",\"url\":\"$DATA/fixtures/fixture.db\",\"allowed_tables\":[\"t\"],\"skip_test\":true}")
  rpc_ok "$r" || fatal "db_sources.upsert fixturedb: $(err_of "$r")"
  r=$(adm db_sources.upsert "{\"name\":\"fixtureopen\",\"driver\":\"sqlite\",\"url\":\"$DATA/fixtures/fixture.db\",\"allowed_tables\":[\"*\"],\"skip_test\":true}")
  rpc_ok "$r" || fatal "db_sources.upsert fixtureopen: $(err_of "$r")"
}

provision() { # provider kind binding owner|- label employee|- [detail-json-fragment]
  local provider=$1 kind=$2 binding=$3 owner=$4 label=$5 employee=$6 detail=${7:-} params
  params="{\"provider\":\"$provider\",\"kind\":\"$kind\",\"binding\":\"$binding\",\"label\":\"$label\",\"csrf_token\":\"@csrf\""
  [ "$owner" = - ] || params+=",\"owner_user_id\":\"$owner\""
  [ "$employee" = - ] || params+=",\"details\":{\"employee\":\"$employee\"${detail:+,$detail}}"
  params+='}'
  adm isolation.connector_provision "$params" --stepup
}
revoke_route() { adm isolation.connector_revoke "{\"account\":\"$1\",\"csrf_token\":\"@csrf\"}" --stepup; }

setup_fixture() {
  local r
  ADMIN_PW=$(genpass)
  r=$(RPC --state "$STATE/admin.json" claim --email admin@iso.test --password "$ADMIN_PW")
  rpc_ok "$r" || fatal "first-run admin claim failed: $(err_of "$r")"
  r=$(RPC --state "$STATE/admin.json" login)
  rpc_ok "$r" || fatal "admin login failed: $(err_of "$r")"
  r=$(RPC --state "$STATE/admin.json" enroll-mfa)
  rpc_ok "$r" || fatal "admin MFA enrolment failed: $(err_of "$r")"
  pass admin 'first-run admin claimed, logged in, TOTP enrolled (step-up capable)'
  local who
  for who in alice bob carol dave; do create_user "$who"; done
  pass users "users alice(owner) bob carol dave created and logged in once (Active, last_login set)"
  setup_db_fixture
  ensure_shared team || fatal 'Shared employee `team` could not be created'
  ensure_personal alice alice-personal || fatal 'Personal employee alice-personal could not be created'
  grant_db alice-personal team || fatal 'could not grant the fixture database sources'
  r=$(provision database personal personal "${USER_ID[alice]}" alice-fixture-db alice-personal '"source":"fixturedb"')
  printf '%s' "$r" | ev setup-alice-db-route
  rpc_ok "$r" || fatal "alice database route: $(err_of "$r")"
  r=$(provision database personal personal "${USER_ID[alice]}" alice-fixture-open alice-personal '"source":"fixtureopen"')
  rpc_ok "$r" || fatal "alice open database route: $(err_of "$r")"
  adm isolation.employee_list | ev setup-employees
  pass fixture 'alice-personal = Personal(alice), team = Shared, sqlite database sources + alice routes provisioned'
  start_canary
  if [ "$PAUSE" -eq 1 ]; then
    {
      echo "Fixture ready. Gateway port: see [gateway] port in $DATA/config.toml (default 18889), loopback only."
      echo "Admin admin@iso.test password: $ADMIN_PW (TOTP secret in $STATE/admin.json)"
      echo "alice@iso.test password: ${USER_PW[alice]}"
      echo 'Connect provider accounts now (dashboard), then press Enter to run the checks.'
    } >&2
    read -r _ || fatal '--pause-after-setup needs an interactive stdin'
  fi
}

# ── readiness / status helpers ───────────────────────────────────────────────
status_json() { adm system.status; }
wait_text() { # TEXT SECONDS COMMAND... : poll until the output contains TEXT; prints elapsed ms
  local text=$1 secs=$2 start now r
  shift 2
  start=$(now_ms)
  while :; do
    r=$("$@" || true)
    if printf '%s' "$r" | PY contains "$text" 2>/dev/null; then echo $(($(now_ms) - start)); return 0; fi
    now=$(now_ms)
    [ $((now - start)) -lt $((secs * 1000)) ] || return 1
    sleep 0.2
  done
}
wait_absent() { # TEXT SECONDS COMMAND... : poll until the output no longer contains TEXT
  local text=$1 secs=$2 start now r rc
  shift 2
  start=$(now_ms)
  while :; do
    r=$("$@" || true)
    rc=0
    printf '%s' "$r" | PY contains "$text" 2>/dev/null || rc=$?
    # 1 = valid JSON without the text; 2 = unreadable response (gateway down): keep polling
    if [ "$rc" -eq 1 ]; then return 0; fi
    now=$(now_ms)
    [ $((now - start)) -lt $((secs * 1000)) ] || return 1
    sleep 0.5
  done
}
readiness_text() { status_json | PY jget payload.isolation_readiness 2>/dev/null || echo none; }

# ═══ items ═══════════════════════════════════════════════════════════════════
item_01() { # owner gmail_search ok†
  local list node
  list=$(adm isolation.employee_list); printf '%s' "$list" | ev 01-employee-list
  if node=$(printf '%s' "$list" | PY employee alice-personal) \
      && printf '%s' "$node" | grep -q "${USER_ID[alice]}" && printf '%s' "$node" | grep -qi personal; then
    pass 01a 'alice-personal is a confirmed Personal employee owned by alice'
  else fail 01a 'alice-personal is not reported as Personal(alice) in isolation.employee_list'; fi
  if node=$(printf '%s' "$list" | PY employee team) && printf '%s' "$node" | grep -qi shared; then
    pass 01b 'team is a confirmed Shared employee'
  else fail 01b 'team is not reported as Shared'; fi
  if need_driver 01c 'owner reads her fixture database through alice-personal (local substitute for gmail_search †)'; then
    drv_expect 01c 'owner db_select via Personal route' ok alice-personal db_select '{"source":"fixturedb","table":"t"}'
  fi
  if need_provider 01d 'owner gmail_search through Personal Google' DUDUCLAW_ISO_GOOGLE_ALICE; then
    drv_expect 01d 'owner gmail_search' ok alice-personal gmail_search '{"query":"newer_than:30d","max_results":1}'
  fi
}

item_02() { # bob binding refused / forced => personal_binding_conflict†
  local r ms
  r=$(adm users.bind_agent "{\"user_id\":\"${USER_ID[bob]}\",\"agent_name\":\"alice-personal\",\"access_level\":\"viewer\"}")
  printf '%s' "$r" | ev 02-bind-bob
  if rpc_ok "$r"; then
    fail 02a 'bob was bound to the Personal employee alice-personal'
    adm users.unbind_agent "{\"user_id\":\"${USER_ID[bob]}\",\"agent_name\":\"alice-personal\"}" >/dev/null || true
  else pass 02a "binding bob to alice-personal refused ($(err_of "$r" | cut -c1-80))"; fi
  r=$(rpc_as bob call me.isolation.request_personal_employee '{"label":"alice-personal","display_name":"x"}')
  if rpc_ok "$r"; then fail 02b 'bob could request the label of an existing Personal employee'
  else pass 02b 'bob cannot request an already used Personal label'; fi
  local tamper_out
  tamper_out=$(as_service python3 "$DATA/fixtures/tamper_users_db.py" force-bind --db "$DATA/users.db" \
    --save "$DATA/fixtures/tamper-bind.json" --user "${USER_ID[bob]}" --agent alice-personal 2>&1 || true)
  printf '%s\n' "$tamper_out" > "$OUT/evidence/02-tamper-force-bind.json"
  if [ "$(printf '%s' "$tamper_out" | jf ok 2>/dev/null || true)" != true ]; then
    skip 02c "could not force the second binding at database level: $(printf '%s' "$tamper_out" | jf detail 2>/dev/null || echo "$tamper_out")"
    return 0
  fi
  if ms=$(wait_text personal_binding_conflict 20 status_json); then
    pass 02c "forced second binding => readiness reports personal_binding_conflict after ${ms} ms"
  else fail 02c 'forced second binding did not surface personal_binding_conflict in system.status within 20 s'; fi
  status_json | ev 02-status-forced
  if need_driver 02d 'tool call while the binding is forced'; then
    drv_expect 02d 'owner call refused while a second user is bound' refused alice-personal db_select '{"source":"fixturedb","table":"t"}'
  fi
  as_service python3 "$DATA/fixtures/tamper_users_db.py" restore --db "$DATA/users.db" --save "$DATA/fixtures/tamper-bind.json" \
    > "$OUT/evidence/02-tamper-restore.json" 2>&1 || true
  if wait_absent personal_binding_conflict 20 status_json; then
    pass 02e 'conflict reason cleared after the extra binding was removed'
  else fail 02e 'personal_binding_conflict persists after restoring the database'; fi
}

item_03() { # team Personal tool only via explicit home fallback
  local r acct
  if have_driver; then
    drv_expect 03a 'team has no route yet: db_select refused' refused team db_select '{"source":"fixturedb","table":"t"}'
  else skip 03a 'team refused without a route: needs DUDUCLAW_ISO_TOOL_DRIVER'; fi
  r=$(provision database personal personal "${USER_ID[alice]}" alice-route-for-team team '"source":"fixturedb"')
  printf '%s' "$r" | ev 03-personal-route-for-team
  if rpc_ok "$r"; then fail 03b "alice's Personal route was pinned to the Shared employee team"; revoke_route "$(printf '%s' "$r" | jf payload.account)" >/dev/null || true
  else pass 03b "a Personal route cannot be given to the Shared employee team ($(err_of "$r" | cut -c1-60))"; fi
  r=$(provision database shared_home shared - team-home-db team '"source":"fixturedb"')
  printf '%s' "$r" | ev 03-home-route
  if rpc_ok "$r"; then acct=$(printf '%s' "$r" | jf payload.account); pass 03c "explicit SharedHome fallback route provisioned for team ($acct)"
  else fail 03c "SharedHome route for team refused: $(err_of "$r")"; return 0; fi
  if have_driver; then
    drv_expect 03d 'team db_select ok via the explicit home route' ok team db_select '{"source":"fixturedb","table":"t"}'
    local kind
    kind=$(drv_field route_kind)
    if [ -z "$kind" ]; then skip 03e 'driver did not report route_kind'
    elif [ "$kind" = shared_home ]; then pass 03e 'driver reports route_kind=shared_home'
    else fail 03e "driver reports route_kind=$kind"; fi
  else skip 03d 'team ok via home route: needs DUDUCLAW_ISO_TOOL_DRIVER'; fi
  if need_provider 03f 'team Google tool only through Home OAuth' DUDUCLAW_ISO_GOOGLE_HOME; then
    drv_expect 03f 'team gmail_search via Home OAuth only' ok team gmail_search '{"query":"newer_than:30d","max_results":1}'
    [ "$(drv_field route_kind)" != personal ] || fail 03g 'team used a Personal Google route'
  fi
}

item_04() { # member removed mid-run => next call refused <= 1 s†
  ensure_personal carol carol-personal || { fail 04 'could not create carol-personal'; return 0; }
  grant_db carol-personal || true
  provision database personal personal "${USER_ID[carol]}" carol-fixture-db carol-personal '"source":"fixturedb"' >/dev/null || true
  local handle='' t0 t1 r node state=''
  if have_driver; then
    drv capture carol-personal carol
    handle=$(drv_field handle)
    [ -n "$handle" ] || { skip 04b "driver gave no capability handle: $DRV_RAW"; }
  fi
  node=$(employee_json carol-personal) || { fail 04 'carol-personal vanished before the test'; return 0; }
  printf '%s' "$node" | ev 04-before
  r=$(adm users.remove "{\"user_id\":\"${USER_ID[carol]}\"}")
  t0=$(now_ms)
  rpc_ok "$r" || { fail 04a "users.remove refused: $(err_of "$r")"; return 0; }
  local deadline=$((t0 + 5000)) seen=''
  while [ "$(now_ms)" -lt "$deadline" ]; do
    node=$(employee_json carol-personal || true)
    state=$(employee_usable "$node")
    if [ "$state" = unusable ]; then t1=$(now_ms); seen=$((t1 - t0)); break; fi
    [ "$state" != unknown ] || break
  done
  if [ "$state" = unknown ]; then
    skip 04a 'lifecycle state not recognisable in isolation.employee_list (no usable/status field); see evidence 04-before.json'
  elif [ -n "$seen" ]; then
    if [ "$seen" -le 1000 ]; then pass 04a "lifecycle reports the employee unusable ${seen} ms after users.remove (proxy via isolation.employee_list)"
    else fail 04a "lifecycle needed ${seen} ms (> 1000 ms) after users.remove"; fi
  else fail 04a 'employee still reported usable 5 s after the owner was removed'; fi
  if [ -n "$handle" ]; then
    t0=$(now_ms); local tries=0 got=''
    while [ "$tries" -lt 40 ]; do
      drv replay "$handle" db_select '{"source":"fixturedb","table":"t"}'
      if [ "$DRV_OUTCOME" = refused ]; then got=$(($(now_ms) - t0)); break; fi
      tries=$((tries + 1))
    done
    printf '%s\n' "$DRV_RAW" > "$OUT/evidence/04b-driver.json"
    if [ -z "$got" ]; then fail 04b 'next tool call was not refused after the member was removed'
    elif [ "$got" -le 1000 ]; then pass 04b "next call refused ${got} ms after removal (driver round trip included)"
    else fail 04b "next call refused only after ${got} ms (> 1000 ms)"; fi
  elif ! have_driver; then skip 04b 'timed next-call refusal: needs DUDUCLAW_ISO_TOOL_DRIVER'; fi
}
# usable | unusable | unknown, from one employee_list node
employee_usable() {
  local node=$1 v key
  [ -n "$node" ] || { echo unusable; return; }
  for key in usable ready; do
    if v=$(printf '%s' "$node" | jf "$key" 2>/dev/null); then
      if [ "$v" = true ]; then echo usable; else echo unusable; fi
      return
    fi
  done
  for key in lifecycle status state; do
    if v=$(printf '%s' "$node" | jf "$key" 2>/dev/null); then
      case $(printf '%s' "$v" | tr 'A-Z' 'a-z') in
        *denied*|*stale*|*tombstone*|*revoked*|*unavailable*|*disabled*|*suspend*) echo unusable ;;
        *ready*|*active*|*applied*|*confirmed*|*ok*) echo usable ;;
        *) echo unknown ;;
      esac
      return
    fi
  done
  echo unknown
}

item_05() { # tenant uuid replaced => all refused
  local before out
  before=$(readiness_text); printf '%s' "$before" | ev 05-readiness-before
  out=$(as_service python3 "$DATA/fixtures/tamper_users_db.py" replace-tenant --db "$DATA/users.db" --save "$DATA/fixtures/tamper-tenant.json" 2>&1 || true)
  printf '%s\n' "$out" > "$OUT/evidence/05-tamper.json"
  if [ "$(printf '%s' "$out" | jf ok 2>/dev/null || true)" != true ]; then
    skip 05a "could not replace the installation tenant: $(printf '%s' "$out" | jf detail 2>/dev/null || echo "$out")"; return 0
  fi
  local waited=0 changed=''
  while [ "$waited" -lt 20 ]; do
    out=$(readiness_text)
    if [ "$out" != "$before" ] && printf '%s' "$out" | grep -Eqi 'revoked|binding_evidence_missing|service_unavailable|"ready": *false'; then changed=1; break; fi
    sleep 1; waited=$((waited + 1))
  done
  printf '%s' "$out" | ev 05-readiness-after
  if [ -n "$changed" ]; then pass 05a "readiness changed to a refusing state ${waited}s after the tenant uuid was replaced"
  else fail 05a 'readiness did not refuse after the installation tenant was replaced'; fi
  if need_driver 05b 'tool calls after tenant replacement'; then
    drv_expect 05b 'Personal employee call refused' refused alice-personal db_select '{"source":"fixturedb","table":"t"}'
    drv_expect 05c 'Shared employee call refused' refused team db_select '{"source":"fixturedb","table":"t"}'
  fi
  as_service python3 "$DATA/fixtures/tamper_users_db.py" restore --db "$DATA/users.db" --save "$DATA/fixtures/tamper-tenant.json" \
    > "$OUT/evidence/05-restore.json" 2>&1 || true
  if [ "$(jf ok < "$OUT/evidence/05-restore.json" 2>/dev/null || true)" = true ]; then pass 05d 'original tenant restored'
  else fail 05d 'could not restore the original tenant (later items are unreliable)'; fi
}

item_06() { # delete/recreate same label => old capability + receipts refused†
  local old new handle='' q
  ensure_personal dave dave-personal || { fail 06 'could not create dave-personal'; return 0; }
  old=$(employee_id dave-personal) || { fail 06 'no employee id for dave-personal'; return 0; }
  if have_driver; then drv capture dave-personal dave; handle=$(drv_field handle); fi
  local r
  r=$(adm isolation.employee_tombstone "{\"label\":\"dave-personal\",\"employee_id\":\"$old\",\"csrf_token\":\"@csrf\"}" --stepup)
  printf '%s' "$r" | ev 06-tombstone
  if ! rpc_ok "$r"; then fail 06a "tombstone refused: $(err_of "$r")"; return 0; fi
  q=$(find "$DATA/isolation-quarantine" -maxdepth 1 -name '*-dave-personal' 2>/dev/null | head -n 1 || true)
  if [ -n "$q" ] && [ ! -e "$DATA/agents/dave-personal" ]; then pass 06a "old agent directory quarantined at ${q#"$DATA"/}"
  else fail 06a 'old agent directory not moved to isolation-quarantine'; fi
  if ensure_personal dave dave-personal; then
    new=$(employee_id dave-personal || true)
    if [ -n "$new" ] && [ "$new" != "$old" ]; then pass 06b 'same label recreated with a new employee id'
    else fail 06b "recreated label kept the old employee id (${new:-none})"; fi
    if adm isolation.employee_list | PY employee dave-personal --all | grep -qx "$old"; then
      pass 06c 'old employee id still listed (tombstoned), never reused'
    else fail 06c 'old employee id disappeared from the lifecycle list'; fi
  else fail 06b 'same label could not be recreated after tombstone (spawnd may still hold the old allocation: README, A2 open item 1)'; fi
  r=$(adm isolation.employee_tombstone "{\"label\":\"dave-personal\",\"employee_id\":\"$old\",\"csrf_token\":\"@csrf\"}" --stepup)
  if rpc_ok "$r"; then fail 06d 'the old employee id could be tombstoned again / still acts as live'; else pass 06d 'old employee id no longer addressable'; fi
  if [ -n "$handle" ]; then
    drv replay "$handle" db_select '{"source":"fixturedb","table":"t"}'
    if [ "$DRV_OUTCOME" = refused ]; then pass 06e 'old capability refused after delete/recreate'
    else fail 06e "old capability not refused (${DRV_OUTCOME})"; fi
  else skip 06e 'old capability refusal: needs DUDUCLAW_ISO_TOOL_DRIVER'; fi
  skip 06f 'old background receipts refused: covered by 13b (driver) and the G2 chain test'
}

# Provider races (7, 8, 9): scenario implemented by the operator's driver.
race_item() { # ID SCENARIO ENV DESC EXPECT-JSON-PATH EXPECT-VALUE
  local id=$1 scenario=$2 env=$3 desc=$4 path=$5 want_value=$6
  need_provider "$id" "$desc" "$env" || return 0
  drv race "$scenario" alice-personal '{}'
  printf '%s\n' "$DRV_RAW" > "$OUT/evidence/$id-driver.json"
  local sent
  sent=$(drv_field requests_sent)
  if [ "$DRV_OUTCOME" = uncertain ] || [ "$scenario" = revoke_between_pages ]; then
    if [ "$(drv_field "$path")" = "$want_value" ]; then pass "$id" "$desc (outcome=$DRV_OUTCOME, $path=$want_value${sent:+, requests_sent=$sent})"
    else fail "$id" "$desc: expected $path=$want_value, driver said: $DRV_RAW"; fi
  else fail "$id" "$desc: expected an uncertain outcome, driver said: $DRV_RAW"; fi
}
item_07() { race_item 07 revoke_before_send DUDUCLAW_ISO_GOOGLE_ALICE 'revoke between selection and send: no request, uncertain' requests_sent 0; }
item_08() { race_item 08 revoke_during_refresh DUDUCLAW_ISO_GOOGLE_ALICE 'revoke during Google refresh: uncertain, next page refused' next_call refused; }
item_09() { race_item 09 revoke_between_pages DUDUCLAW_ISO_NOTION 'revoke between Notion pages: next page refused' next_page refused; }

item_10() { # gateway + spawnd restart => old capability refused†
  local handle='' gw0 sp0 gw1 sp1 f changed=0 total=0 sum
  gw0=$(systemctl show -p MainPID --value "$UNIT")
  sp0=$(systemctl show -p MainPID --value duduclaw-spawnd.service)
  : > "$TMP/pub-before"
  for f in /run/duduclaw-spawnd/mcp-*.json; do
    [ -e "$f" ] || continue
    sum=$(sha256sum "$f" | cut -d' ' -f1); printf '%s %s\n' "$f" "$sum" >> "$TMP/pub-before"
  done
  if have_driver; then drv capture alice-personal alice; handle=$(drv_field handle); fi
  systemctl stop "$UNIT" || true
  systemctl restart duduclaw-spawnd.service || systemctl restart duduclaw-spawnd.socket || true
  systemctl start "$UNIT" || true
  local r
  r=$(RPC wait --timeout 120)
  rpc_ok "$r" || { fail 10 "gateway did not return after restart: $(err_of "$r")"; return 0; }
  gw1=$(systemctl show -p MainPID --value "$UNIT")
  sp1=$(systemctl show -p MainPID --value duduclaw-spawnd.service)
  if [ "$gw0" != "$gw1" ] && { [ "$sp0" != "$sp1" ] || [ "$sp0" = 0 ]; }; then pass 10a "gateway restarted ($gw0 -> $gw1); spawnd restarted ($sp0 -> $sp1)"
  else fail 10a "service PIDs unchanged (gateway $gw0 -> $gw1, spawnd $sp0 -> $sp1)"; fi
  while read -r f sum; do
    [ -n "$f" ] || continue
    total=$((total + 1))
    if [ -e "$f" ] && [ "$(sha256sum "$f" | cut -d' ' -f1)" = "$sum" ]; then changed=$((changed + 1)); fi
  done < "$TMP/pub-before"
  if [ "$total" -eq 0 ]; then skip 10b 'no capability publication existed before the restart (no live run); the driver check 10d covers it'
  elif [ "$changed" -eq 0 ]; then pass 10b "all $total pre-restart capability publications were removed or re-issued"
  else fail 10b "$changed of $total pre-restart capability publications are byte-identical after restart"; fi
  if wait_employee alice-personal 60 && wait_employee team 60; then pass 10c 'confirmed employees survive the restart'
  else fail 10c 'confirmed employees not readable after the restart'; fi
  if [ -n "$handle" ]; then
    drv replay "$handle" db_select '{"source":"fixturedb","table":"t"}'
    printf '%s\n' "$DRV_RAW" > "$OUT/evidence/10d-driver.json"
    if [ "$DRV_OUTCOME" = refused ]; then pass 10d 'pre-restart capability refused'
    else fail 10d "pre-restart capability not refused (${DRV_OUTCOME})"; fi
    drv_expect 10e 'a fresh run after restart works' ok alice-personal db_select '{"source":"fixturedb","table":"t"}'
  else skip 10d 'old capability replay: needs DUDUCLAW_ISO_TOOL_DRIVER'; fi
}

item_11() { # same-name relink => new id
  local a b r list
  r=$(provision search personal personal "${USER_ID[alice]}" relink-me -)
  printf '%s' "$r" | ev 11-first
  rpc_ok "$r" || { fail 11a "first provision refused: $(err_of "$r")"; return 0; }
  a=$(printf '%s' "$r" | jf payload.account)
  r=$(revoke_route "$a")
  rpc_ok "$r" || { fail 11a "revoke refused: $(err_of "$r")"; return 0; }
  r=$(provision search personal personal "${USER_ID[alice]}" relink-me -)
  printf '%s' "$r" | ev 11-second
  rpc_ok "$r" || { fail 11a "re-provision with the same label refused: $(err_of "$r")"; return 0; }
  b=$(printf '%s' "$r" | jf payload.account)
  if [ "$a" != "$b" ]; then pass 11a "same label, new opaque account id ($a -> $b)"; else fail 11a 'revoked account id was reused'; fi
  list=$(adm isolation.connector_list); printf '%s' "$list" | ev 11-list
  if [ "$(printf '%s' "$list" | jf "payload.routes[account=$a].active" 2>/dev/null || true)" = false ] \
      && [ "$(printf '%s' "$list" | jf "payload.routes[account=$b].active" 2>/dev/null || true)" = true ]; then
    pass 11b 'old route stays inactive, new route active'
  else fail 11b 'route states after relink are not (old inactive, new active)'; fi
  local ga gb
  ga=$(printf '%s' "$list" | jf "payload.routes[account=$a].generation" 2>/dev/null || echo 0)
  gb=$(printf '%s' "$list" | jf "payload.routes[account=$b].generation" 2>/dev/null || echo 0)
  if [ "$gb" -gt "$ga" ] 2>/dev/null; then pass 11c "generation advanced ($ga -> $gb)"; else fail 11c "generation did not advance ($ga -> $gb)"; fi
  revoke_route "$b" >/dev/null || true
}

item_12() { # missing factory/provider => connector_setup_required†
  local r acct ms where=''
  r=$(provision odoo personal personal "${USER_ID[alice]}" alice-odoo-unconfigured -)
  printf '%s' "$r" | ev 12-provision-odoo
  if ! rpc_ok "$r"; then skip 12a "could not provision an unconfigured odoo route: $(err_of "$r")"; return 0; fi
  acct=$(printf '%s' "$r" | jf payload.account)
  local src
  for src in status_json "adm isolation.employee_list" "adm isolation.connector_list"; do
    # shellcheck disable=SC2086 # $src is a command plus fixed arguments
    if ms=$(wait_text connector_setup_required 20 $src); then where=$src; break; fi
  done
  if [ -n "$where" ]; then
    pass 12a "route without a configured provider => connector_setup_required reported by '${where}' after ${ms} ms"
    # shellcheck disable=SC2086
    if $where | PY contains odoo; then pass 12b 'reason names the provider (odoo)'
    else fail 12b 'connector_setup_required does not name the provider'; fi
  else fail 12a 'no connector_setup_required reason within 20 s (system.status / employee_list / connector_list)'; fi
  status_json | ev 12-status
  if need_driver 12c 'tool call on the unconfigured provider'; then
    drv_expect 12c 'odoo call refused before any credential is read' refused alice-personal odoo_search '{"model":"res.partner"}'
  fi
  r=$(provision database personal personal "${USER_ID[alice]}" alice-bad-source alice-personal '"source":"nosuchsource"')
  if rpc_ok "$r"; then fail 12d 'a database route for an unknown source was accepted'; revoke_route "$(printf '%s' "$r" | jf payload.account)" >/dev/null || true
  else pass 12d 'database route for an unknown source refused at provision time'; fi
  revoke_route "$acct" >/dev/null || true
}

item_13() { # background after password reset => refused
  ensure_personal dave dave-personal || { fail 13 'dave-personal unavailable'; return 0; }
  grant_db dave-personal || true
  local handle='' r newpw precheck
  if have_driver; then drv capture dave-personal dave --background; handle=$(drv_field handle); fi
  r=$(RPC --state "$STATE/dave.json" connect-only)
  precheck=$(printf '%s' "$r" | jf ok 2>/dev/null || echo false)
  newpw=$(genpass)
  r=$(adm users.update "{\"user_id\":\"${USER_ID[dave]}\",\"password\":\"$newpw\"}")
  printf '%s' "$r" | ev 13-reset
  if ! rpc_ok "$r"; then fail 13a "password reset refused: $(err_of "$r")"; return 0; fi
  r=$(RPC --state "$STATE/dave.json" connect-only)
  if rpc_ok "$r"; then fail 13a 'dave still has an accepted session after the password reset'
  elif [ "$precheck" != true ]; then skip 13a 'dave session was already unusable before the reset (cannot attribute the refusal)'
  else pass 13a 'pre-reset session of dave refused after the password reset'; fi
  if [ -n "$handle" ]; then
    drv replay "$handle" db_select '{"source":"fixturedb","table":"t"}'
    printf '%s\n' "$DRV_RAW" > "$OUT/evidence/13b-driver.json"
    if [ "$DRV_OUTCOME" = refused ]; then pass 13b 'background run authorised before the reset is refused afterwards'
    else fail 13b "background run still accepted after the password reset (${DRV_OUTCOME})"; fi
  else skip 13b 'background refusal after password reset: needs DUDUCLAW_ISO_TOOL_DRIVER'; fi
}

item_14() { # Odoo forbidden / DB non-SELECT / unbound channel recipient refused before credential
  local rows_before rows_after hits0 hits1
  count_rows() { python3 -c 'import sqlite3,sys;print(sqlite3.connect("file:"+sys.argv[1]+"?mode=ro",uri=True).execute("select count(*) from t").fetchone()[0])' "$DATA/fixtures/fixture.db" 2>/dev/null || echo -1; }
  rows_before=$(count_rows)
  if need_driver 14a 'non-SELECT statements on a database source'; then
    drv_expect 14a 'DELETE refused' refused alice-personal db_query '{"source":"fixtureopen","sql":"DELETE FROM t"}'
    drv_expect 14b 'DROP TABLE refused' refused alice-personal db_query '{"source":"fixtureopen","sql":"DROP TABLE t"}'
    drv_expect 14c 'stacked statement refused' refused alice-personal db_query '{"source":"fixtureopen","sql":"SELECT 1; DELETE FROM t"}'
    rows_after=$(count_rows)
    if [ "$rows_before" = "$rows_after" ] && [ "$rows_after" -ge 0 ]; then pass 14d "fixture table untouched ($rows_after rows)"
    else fail 14d "fixture table changed ($rows_before -> $rows_after rows)"; fi
  fi
  hits0=$(canary_hits)
  if need_provider 14e 'forbidden Odoo model/method' DUDUCLAW_ISO_ODOO_URL; then
    drv_expect 14e 'Odoo forbidden model refused' refused alice-personal odoo_search '{"model":"ir.config_parameter"}'
    if [ "$(drv_field credential_used)" != true ]; then pass 14f 'driver: no Odoo credential used for the refused call'
    else fail 14f 'driver: Odoo credential was used for a refused call'; fi
    if [ "$DUDUCLAW_ISO_ODOO_URL" = "https://127.0.0.1:$CANARY_PORT" ]; then
      hits1=$(canary_hits)
      if [ "$hits0" = "$hits1" ]; then pass 14g 'canary origin saw no request'
      else fail 14g "canary origin saw $((hits1 - hits0)) request(s)"; fi
    fi
  fi
  if need_provider 14h 'channel send to an unbound recipient' DUDUCLAW_ISO_CHANNEL; then
    drv_expect 14h "unbound ${DUDUCLAW_ISO_CHANNEL} recipient refused" refused alice-personal channel_send \
      "{\"channel\":\"${DUDUCLAW_ISO_CHANNEL}\",\"to\":\"${DUDUCLAW_ISO_CHANNEL_UNBOUND_TARGET:-unbound-target}\",\"text\":\"acceptance\"}"
    if [ "$(drv_field credential_used)" != true ]; then pass 14i 'driver: no channel token used for the refused send'
    else fail 14i 'driver: channel token used for a refused send'; fi
  fi
}

smoke() { # ID FAMILY ENV
  local id=$1 family=$2 env=$3
  need_provider "$id" "live smoke: $family" "$env" || return 0
  drv smoke "$family" alice-personal
  printf '%s\n' "$DRV_RAW" > "$OUT/evidence/$id-driver.json"
  if [ "$DRV_OUTCOME" = ok ]; then pass "$id" "live smoke $family ok"
  else fail "$id" "live smoke $family: ${DRV_OUTCOME}${DRV_CODE:+ code=$DRV_CODE}"; fi
}
smoke_sql() { # ID FAMILY DSN-ENV TABLE-ENV DRIVER-NAME
  local id=$1 family=$2 dsn=$3 table=$4 name=$5 params r
  if [ -z "${!dsn:-}" ] || [ -z "${!table:-}" ]; then skip "$id" "live smoke: $family read-only user ($dsn / $table unset)"; return 0; fi
  need_driver "$id" "live smoke: $family" || return 0
  params=$(DSN="${!dsn}" TBL="${!table}" NAME="${family}ro" DRV="$name" python3 -c \
    'import json,os;print(json.dumps({"name":os.environ["NAME"],"driver":os.environ["DRV"],"url":os.environ["DSN"],"allowed_tables":[os.environ["TBL"]]}))')
  r=$(adm db_sources.upsert "$params")
  rpc_ok "$r" || { fail "$id" "db_sources.upsert for $family refused: $(err_of "$r")"; return 0; }
  grant_db_source "${family}ro" alice-personal || true
  r=$(provision database personal personal "${USER_ID[alice]}" "alice-${family}" alice-personal "\"source\":\"${family}ro\"")
  rpc_ok "$r" || { fail "$id" "route for $family refused: $(err_of "$r")"; return 0; }
  drv_expect "$id" "live smoke $family: db_select through the Personal route" ok alice-personal db_select \
    "{\"source\":\"${family}ro\",\"table\":\"${!table}\"}"
}
grant_db_source() { adm db_sources.grants.set "{\"name\":\"$1\",\"agents\":[\"$2\"]}" >/dev/null; }
item_15() { # live smoke per provider family
  if need_driver 15a 'sqlite fixture smoke'; then drv_expect 15a 'database (local sqlite) smoke' ok alice-personal db_select '{"source":"fixturedb","table":"t"}'; fi
  smoke 15b google DUDUCLAW_ISO_GOOGLE_ALICE
  smoke 15c notion DUDUCLAW_ISO_NOTION
  smoke 15d github DUDUCLAW_ISO_GITHUB
  smoke 15e odoo DUDUCLAW_ISO_ODOO_URL
  smoke_sql 15f postgres DUDUCLAW_ISO_PG_DSN_RO DUDUCLAW_ISO_PG_TABLE postgres
  smoke_sql 15g mysql DUDUCLAW_ISO_MYSQL_DSN_RO DUDUCLAW_ISO_MYSQL_TABLE mysql
  smoke 15h discord DUDUCLAW_ISO_DISCORD_TOKEN
  smoke 15i line DUDUCLAW_ISO_LINE_TOKEN
  smoke 15j telegram DUDUCLAW_ISO_TELEGRAM_TOKEN
  smoke 15k search DUDUCLAW_ISO_SEARCH_API_KEY
}

# ── main ─────────────────────────────────────────────────────────────────────
preflight
install_service
setup_fixture
for n in "${ALL_ITEMS[@]}"; do
  want "$n" || continue
  log "── item $((10#$n)) ──"
  "item_$n" || record FAIL "$n" "item aborted unexpectedly (see run.log)"
done
[ ! -f "$TMP/canary/hits.jsonl" ] || cp "$TMP/canary/hits.jsonl" "$OUT/evidence/canary-hits.jsonl"
FINISHED=1
