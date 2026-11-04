#!/usr/bin/env bash
# Run with the operator's own gcloud identity; never change global gcloud config.
set -euo pipefail
umask 077
project_id=''
endpoint_url=''
agent=''
app_name=''
out=''
dry_run=false
created_this_run=false
usage() {
  cat <<'HELP'
Usage: bootstrap.sh [--project-id ID]
  [--endpoint-url HTTPS_URL] [--agent ID] [--app-name NAME] [--out DIR] [--dry-run]
HTTP requires --endpoint-url. Requires bash, python3, and your own gcloud login.
--dry-run prints commands without calling gcloud or writing files.
HELP
}
fail() { printf '錯誤 / Error: %s\n' "$*" >&2; exit 1; }
while (($#)); do
  case "$1" in
    --dry-run) dry_run=true; shift ;;
    --help|-h) usage; exit 0 ;;
    --project-id|--endpoint-url|--agent|--app-name|--out)
      (($# >= 2)) || fail "$1 缺少值 / missing value"
      [[ -n "$2" && "$2" != --* ]] || fail "$1 缺少值 / missing value"
      case "$1" in
        --project-id) project_id=$2 ;;
        --endpoint-url) endpoint_url=$2 ;; --agent) agent=$2 ;;
        --app-name) app_name=$2 ;; --out) out=$2 ;;
      esac
      shift 2 ;;
    *) fail "未知參數 / unknown argument: $1" ;;
  esac
done
if [[ -z "$project_id" ]]; then
  project_id="duduclaw-chat-$(od -An -N3 -tx1 /dev/urandom | tr -d ' \n')"
fi
[[ "$project_id" =~ ^[a-z][a-z0-9-]{4,28}[a-z0-9]$ ]] || fail '專案 ID 格式錯誤 / invalid project ID (6–30 lowercase letters, digits, hyphens)'
[[ -z "$agent" || "$agent" =~ ^[a-zA-Z0-9][a-zA-Z0-9_-]{0,63}$ ]] || fail '員工 ID 格式錯誤 / invalid agent ID'
command -v python3 >/dev/null || fail '請安裝 Python 3 / install Python 3'
[[ -n "$endpoint_url" ]] || fail 'HTTP 需要 --endpoint-url / HTTP requires --endpoint-url'
if [[ -n "$endpoint_url" ]]; then
  python3 -I - "$endpoint_url" <<'PY' || fail '請提供有效的公開 HTTPS URL（不可含帳密或片段）/ expected a valid public HTTPS URL without credentials or fragment'
import sys, urllib.parse
value = sys.argv[1]
try:
    u = urllib.parse.urlsplit(value)
    valid = (u.scheme == 'https' and u.hostname and not u.username and not u.password
             and not u.fragment and not any(c.isspace() or ord(c) < 32 for c in value)
             and '\\' not in value and (u.port is None or 1 <= u.port <= 65535))
except ValueError:
    valid = False
sys.exit(0 if valid else 1)
PY
fi
[[ -n "$app_name" ]] || app_name="DuDuClaw${agent:+ - $agent}"
[[ -n "$out" ]] || out="./googlechat-setup-$project_id"
python3 -I - "$app_name" "$out" <<'PYVALIDATE' || fail '名稱或路徑含控制字元，或應用程式名稱超過 25 字；長員工 ID 請另指定 --app-name / no control characters; app name must be at most 25 characters'
import sys
name, path = sys.argv[1:]
def controls(value):
    return any(ord(c) < 32 or 127 <= ord(c) <= 159 for c in value)
sys.exit(1 if controls(name) or controls(path) or len(name) > 25 else 0)
PYVALIDATE
sa="duduclaw-chat@$project_id.iam.gserviceaccount.com"
print_command() { printf '%q ' "$@"; printf '\n'; }
# Capture only command errors; never print key material or access tokens.
gcloud_run() {
  if "$dry_run"; then print_command gcloud "$@"; return; fi
  local output
  if ! output=$(gcloud "$@" 2>&1); then
    printf '%s\n' "$output" >&2
    case "$output" in
      *[Bb]illing*|*BILLING*)
        printf '請由帳務管理員確認帳務設定；腳本不會綁定帳務。/ Ask your billing administrator; this script does not attach billing.\nhttps://console.cloud.google.com/billing/linkedaccount?project=%s\n' "$project_id" >&2 ;;
    esac
    case "$output" in
      *disableServiceAccountKeyCreation*|*FAILED_PRECONDITION*)
        printf '若為金鑰政策阻擋，請 Org Policy Admin 審核以下專案例外；不會自動執行。/ If key creation is blocked, an Org Policy Admin must review these project overrides; not executed automatically.\n' >&2
        print_command gcloud resource-manager org-policies disable-enforce iam.disableServiceAccountKeyCreation "--project=$project_id" >&2
        print_command gcloud resource-manager org-policies disable-enforce iam.managed.disableServiceAccountKeyCreation "--project=$project_id" >&2 ;;
    esac
    return 1
  fi
  printf '%s\n' "$output"
}
if "$dry_run"; then
  printf '預覽 / Dry run: existing resources are checked and reused only after validation.\n'
  print_command gcloud auth list --filter=status:ACTIVE '--format=value(account)'
  print_command gcloud projects describe "$project_id" '--format=value(projectNumber)'
  print_command gcloud projects get-iam-policy "$project_id" --format=json
  gcloud_run projects create "$project_id" --name="DuDuClaw Chat" --quiet
  project_number=123456789012
  account='you@example.com'
else
  command -v gcloud >/dev/null || fail '請先安裝 Google Cloud CLI / install Google Cloud CLI: https://cloud.google.com/sdk/docs/install'
  account=$(gcloud auth list --filter=status:ACTIVE '--format=value(account)') || fail '無法讀取登入狀態 / cannot read gcloud login'
  [[ -n "$account" && "$account" != *$'\n'* ]] || fail '請先執行 gcloud auth login / run gcloud auth login first'
  [[ "$account" != *.gserviceaccount.com ]] || fail '請使用您自己的使用者帳號登入 / sign in with your own user account'
  if project_number=$(gcloud projects describe "$project_id" '--format=value(projectNumber)' 2>/dev/null); then
    # Existing project reuse requires an explicit owner binding for this user.
    policy=$(gcloud_run projects get-iam-policy "$project_id" --format=json) || fail '無法確認專案擁有者 / cannot verify project owner'
    printf '%s' "$policy" | python3 -I -c 'import json,sys; p=json.load(sys.stdin); sys.exit(0 if any(b.get("role")=="roles/owner" and "user:"+sys.argv[1] in b.get("members",[]) for b in p.get("bindings",[])) else 1)' "$account" || fail '僅能沿用您直接擁有的專案；請指定新 ID 或請管理員處理。/ Existing project requires your direct Owner role; choose a new ID or contact an administrator.'
  else
    gcloud_run projects create "$project_id" --name="DuDuClaw Chat" --quiet || fail '專案建立失敗：ID 可能已被使用，或您沒有建立專案的權限。/ Cannot create project: ID may be taken or project creation permission is missing.'
    project_number=$(gcloud_run projects describe "$project_id" '--format=value(projectNumber)')
  fi
  [[ "$project_number" =~ ^[0-9]{6,20}$ ]] || fail '無法取得有效專案編號 / invalid project number'
fi
gcloud_run services enable chat.googleapis.com "--project=$project_id" --quiet
if "$dry_run" || ! gcloud iam service-accounts describe "$sa" "--project=$project_id" >/dev/null 2>&1; then
  gcloud_run iam service-accounts create duduclaw-chat "--display-name=$app_name" "--project=$project_id" --quiet
fi
if "$dry_run"; then
  key_file="$out/service-account.json"
  gcloud_run iam service-accounts keys create "$key_file" "--iam-account=$sa" "--project=$project_id" --quiet
else
  # A private output directory also protects partial key writes and result files.
  out=$(python3 -I - "$out" <<'PY'
import os, stat, sys
p = os.path.abspath(sys.argv[1])
if os.path.lexists(p):
    s = os.lstat(p)
    if not stat.S_ISDIR(s.st_mode) or s.st_uid != os.getuid() or stat.S_IMODE(s.st_mode) & 0o077:
        sys.exit('Output must be an owned private directory (0700), not a symlink.')
else:
    os.makedirs(p, mode=0o700)
print(p)
PY
  ) || fail '輸出目錄不安全 / unsafe output directory'
  key_file="$out/service-account.json"
  if [[ -e "$key_file" || -L "$key_file" ]]; then
    python3 -I - "$key_file" "$project_id" "$sa" <<'PY' || fail '既有金鑰不符或權限不安全；請使用新的 --out / existing key is unsafe or mismatched; use a new --out'
import json, os, stat, sys
p, project, email = sys.argv[1:]
fd = os.open(p, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
with os.fdopen(fd) as f:
    s = os.fstat(f.fileno())
    if not (stat.S_ISREG(s.st_mode) and s.st_uid == os.getuid() and stat.S_IMODE(s.st_mode) == 0o600 and s.st_nlink == 1):
        sys.exit('Key must be an owned regular file with one link and mode 0600; run chmod 600.')
    key = json.load(f)
if not (key.get('type') == 'service_account' and key.get('project_id') == project and key.get('client_email') == email and key.get('private_key')):
    sys.exit('Existing service account key does not match this project and account.')
PY
  else
    staging=$(mktemp -d "$out/.bootstrap.XXXXXX")
    # Keep rollback armed through local result installation and summary output.
    created_key_file="$staging/key.json"
    installed_key=false
    cleanup_created_key() {
      local status=$?
      trap - EXIT HUP INT TERM
      if ((status != 0)); then
        local key_id
        key_id=$(python3 -I - "$created_key_file" <<'PYKEY'
import json, re, sys
try:
    key_id = json.load(open(sys.argv[1])).get('private_key_id', '')
    if not re.fullmatch(r'[a-fA-F0-9]{40}', key_id): sys.exit(1)
    print(key_id)
except (OSError, ValueError):
    sys.exit(1)
PYKEY
        ) || key_id=''
        if [[ -n "$key_id" ]]; then
          if ! gcloud iam service-accounts keys delete "$key_id" "--iam-account=$sa" "--project=$project_id" --quiet >/dev/null; then
            printf '金鑰撤銷失敗，請手動執行 / Key revocation failed; run: ' >&2
            printf '%q ' gcloud iam service-accounts keys delete "$key_id" "--iam-account=$sa" "--project=$project_id" --quiet >&2
            printf '\n' >&2
          else
            printf '已撤銷本次新建金鑰 / Revoked newly created key: %s (%s, project %s)\n' "$key_id" "$sa" "$project_id" >&2
          fi
        else
          printf '無法讀取新金鑰 ID，請管理員檢查服務帳戶金鑰 / Cannot read created key ID; review service account keys.\n' >&2
        fi
        if "$installed_key"; then rm -f -- "$key_file"; fi
      fi
      rm -f -- "$staging/key.json"
      rmdir -- "$staging"
      exit "$status"
    }
    trap cleanup_created_key EXIT
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM
    gcloud_run iam service-accounts keys create "$staging/key.json" "--iam-account=$sa" "--project=$project_id" --quiet
    chmod 0600 "$staging/key.json"
    # Hard-link installation refuses an existing destination, including symlinks.
    ln "$staging/key.json" "$key_file"
    installed_key=true
    # Leave exactly one link before the CLI validates the installed key.
    created_key_file="$key_file"
    rm -f -- "$staging/key.json"
    created_this_run=true
  fi
fi
if ! "$dry_run"; then
  python3 -I - "$out/bootstrap-result.json" "$project_number" "$key_file" "$agent" "$project_id" "$sa" "$created_this_run" <<'PY'
import json, os, sys, tempfile
path, number, key, agent, project, email, created = sys.argv[1:]
with open(key) as f:
    key_id = json.load(f)['private_key_id']
result = dict(project_number=number, key_file=key, agent=agent, project_id=project, service_account_email=email, key_id=key_id, created_this_run=created == 'true')
fd, temp = tempfile.mkstemp(prefix='.result-', dir=os.path.dirname(path))
try:
    with os.fdopen(fd, 'w') as f:
        json.dump(result, f)
        f.write('\n')
    os.replace(temp, path)
finally:
    if os.path.exists(temp): os.unlink(temp)
PY
fi
printf '\nGoogle Chat 設定摘要 / Setup summary\nProject ID: %s\nProject number: %s\nKey file (0600): %q\n' "$project_id" "$project_number" "$key_file"
printf 'Chat Configuration: https://console.cloud.google.com/apis/api/chat.googleapis.com/hangouts-chat?project=%s\n' "$project_id"
printf '建立時請取消勾選「以 Google Workspace 外掛程式建構這個 Chat 應用程式」（Build this Chat app as a Google Workspace add-on）。/ Uncheck this option when creating the app.\n'
printf 'App name: %s\nAvatar URL: （請填入您的公開 HTTPS 頭像 / enter your public HTTPS avatar URL）\nDescription: DuDuClaw AI assistant\nInteractive features: On\n1:1 messages + spaces: On\nVisibility: %s\n' "$app_name" "$account"
printf 'HTTP endpoint URL: %s\nAuthentication Audience: Project number (%s)\n' "$endpoint_url" "$project_number"
printf '\n在 DuDuClaw 主機匯入（Cloud Shell 使用者請先安全下載金鑰）/ Import on the DuDuClaw host (download the key securely from Cloud Shell first):\n'
args=(duduclaw channels googlechat import --project-number "$project_number" --key-file "$key_file")
[[ -z "$agent" ]] || args+=(--agent "$agent")
print_command "${args[@]}"
printf '設定頁需手動儲存；建立資源不代表已連線。/ Save the Chat Configuration page manually; resource creation does not verify a working connection.\n'
printf '腳本成功後會保留金鑰；CLI 匯入失敗時僅提供重試或手動撤銷指令，不會自動撤銷或刪檔。/ After script success the key is kept; CLI import failures only provide retry or manual revocation commands.\n'
