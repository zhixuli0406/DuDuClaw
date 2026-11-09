# Native Linux protected mode: acceptance tooling

| Tool | Status |
|---|---|
| `connectors-acceptance.sh` (+ `connectors_rpc.py`, `fixtures/`) | **Current.** v1.81 Personal-binding / connector acceptance, matrix items 1-15 (see below). |
| `package-h-acceptance.sh` (+ `fixtures/public_origin.py`) | **Current.** v1.81 Package H acceptance (H15, matrix additions 16-27) and the G2 lifecycle chain: installs the service, records host facts, runs the prebuilt `iso_package_h_chain` / `iso_personal_chain` test binaries (`crates/duduclaw-gateway/tests/`), prints a PASS/FAIL/SKIP table. Runbook: `docs/guides/linux-protected-acceptance.md`. |
| `acceptance.sh` + `rpc_probe.py` | **Superseded for v1.81.** `acceptance.sh` asserts the removed `container_root_key` backend (`security.credential_protection` ⇒ `backend=container_root_key`), so it cannot pass on a v1.81 build. It is kept unchanged for the historical record (see "Legacy" at the end). `rpc_probe.py` is still used: `connectors_rpc.py` imports its WebSocket client. |

## Connector acceptance (`connectors-acceptance.sh`)

Source of truth for the matrix: `wiki/duduclaw-kb/architecture/native-linux-isolation-v181-work-breakdown.md` (task G3, "Infrastructure and acceptance"). The script executes items 1-15 as numbered checks (`01a`, `01b`, ... sub-checks), prints `PASS` / `FAIL` / `SKIP` per check, a final summary, and exits non-zero on any `FAIL`. A `SKIP` always names what is missing; it is never counted as a pass.

> **Destructive.** It installs the protected service (`duduclaw service install --protected-linux --enable-now`), creates the accounts `duduclaw*`, writes under `/var/lib/duduclaw*`, `/opt/duduclaw*`, `/usr/lib/duduclaw`, `/etc/duduclaw*` and removes all of it again. Run it only on a disposable VM. It refuses unless `DUDUCLAW_ISO_DISPOSABLE=1`, and refuses (even with `--reset`) when a DuDuClaw installation, home or service account already exists that this run did not create (no `/root/duduclaw-connectors-acceptance/harness-owned` marker). It also refuses when `DUDUCLAW_HOME` is set in the environment. The operator CLI runs with `HOME` and `DUDUCLAW_HOME` pointing into a private temp directory (`make-home.sh` convention), never the operator's own `~/.duduclaw`.

### Prerequisites per distro

Common: run as root on the host (not in a container: the installer refuses containers); systemd as PID 1; unified cgroup v2 (`stat -f -c %T /sys/fs/cgroup` = `cgroup2fs`); initial user namespace; explicit `SYS_UID_MAX`/`SYS_GID_MAX` in `/etc/login.defs` (stock on all three distros); outbound network is **not** required for the local checks; a candidate `duduclaw` binary verified by you beforehand (the script passes `--i-verified-this-binary` and `--allow-missing-runtime`, so no AI CLI has to be installed); about 2 GB free under `/var` and `/opt`; the scripts need `python3` ≥ 3.10, `setpriv` (util-linux), `getent`/`useradd`/`userdel`/`groupdel` (passwd), `journalctl`, `sha256sum`, GNU `date` and `openssl` (optional: without it the HTTPS canary origin is disabled).

| Distro | Packages to install first | Notes |
|---|---|---|
| Ubuntu 22.04 | `apt-get install -y python3 util-linux passwd openssl coreutils` (python3 = 3.10, the minimum) | Reference host (192.168.0.45). Run all 15 items. |
| Ubuntu 24.04 | same (python3 = 3.12) | † items 2, 4, 6, 10, 12 must pass here too. |
| Debian 12 | `apt-get install -y python3 util-linux passwd openssl coreutils procps sudo` (python3 = 3.11) | † items 2, 4, 6, 10, 12 must pass here too. Debian minimal images lack `sudo`/`procps`; log in as root instead. |

The rest of the v1.81 acceptance (outside this script) is unchanged: `DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd -- --ignored iso_root_ --test-threads=1` and `... cargo test -p duduclaw-gateway --test iso_personal_chain -- --ignored iso_root_e2e_ --test-threads=1 --nocapture`.

### How to run

```sh
sudo env DUDUCLAW_ISO_DISPOSABLE=1 \
  bash scripts/live-test/linux-protected/connectors-acceptance.sh \
  --candidate /abs/path/to/dir-with-duduclaw --out /abs/path/evidence-ubuntu2204
```

Options: `--only 2,4,6,10,12` (run selected items; fixtures are still created), `--reset` (remove an earlier run of this harness, e.g. after SIGKILL or power loss), `--keep` (leave the install for investigation; clean up later with `--reset`), `--pause-after-setup` (stop after the fixture exists so you can connect provider accounts in the dashboard; prints the fixture logins, then waits for Enter). `--out` must not exist or be empty. Cleanup runs from an `EXIT`/signal trap and is idempotent.

Because `sudo` strips the environment, pass the variables below with `sudo env VAR=... script` (never put real tokens on a shared shell history).

Fixture created by the run: first-run admin `admin@iso.test` (random password, TOTP enrolled so the dashboard step-up works), users `alice` (owner), `bob`, plus scratch users `carol` (item 4, removed during the test) and `dave` (items 6 and 13), Shared employee `team`, Personal employees `alice-personal` = Personal(alice) (and `carol-personal`, `dave-personal` when their items run), sqlite data sources `fixturedb` (`allowed_tables=["t"]`) and `fixtureopen` (`["*"]`) at `/var/lib/duduclaw/.duduclaw/fixtures/fixture.db`, and Personal database routes for alice. Employees go through the real flow: staff `me.isolation.request_personal_employee` → admin `isolation.employee_propose` (step-up) → `migration.confirmations.approve` → root allocation.

### Environment variables

| Variable | Needed for | Effect when unset |
|---|---|---|
| `DUDUCLAW_ISO_DISPOSABLE=1` | everything | script refuses |
| `DUDUCLAW_ISO_TOOL_DRIVER` | every check that makes a live employee tool call (see "Tool driver contract") | those checks `SKIP` |
| `DUDUCLAW_ISO_CANARY_PORT` | local HTTPS canary origin port (default `18943`) | default used |
| `DUDUCLAW_ISO_GOOGLE_ALICE=1` | 1d, 7, 8, 15b: alice's Personal Google account connected (do it with `--pause-after-setup` through `me.google.connect_*`) from a Google Workspace test tenant | `SKIP` |
| `DUDUCLAW_ISO_GOOGLE_HOME=1` | 3f: Home OAuth (and, for E4 coverage, delegate / service-account) connected | `SKIP` |
| `DUDUCLAW_ISO_NOTION=1` | 9, 15c: Notion workspace credential connected | `SKIP` |
| `DUDUCLAW_ISO_GITHUB=1` | 15d: GitHub org/repo credential connected | `SKIP` |
| `DUDUCLAW_ISO_ODOO_URL` (+ `DUDUCLAW_ISO_ODOO_DB`, `_USER`, `_KEY` for the driver) | 14e-g, 15e: disposable Odoo; set `DUDUCLAW_ISO_ODOO_URL=https://127.0.0.1:<canary port>` to also assert the canary origin saw no request | `SKIP` |
| `DUDUCLAW_ISO_PG_DSN_RO`, `DUDUCLAW_ISO_PG_TABLE` | 15f: Postgres read-only user and one allowed table (DSN is written to the disposable service's `config.toml`) | `SKIP` |
| `DUDUCLAW_ISO_MYSQL_DSN_RO`, `DUDUCLAW_ISO_MYSQL_TABLE` | 15g: MySQL read-only user | `SKIP` |
| `DUDUCLAW_ISO_DISCORD_TOKEN` / `_LINE_TOKEN` / `_TELEGRAM_TOKEN` | 15h / 15i / 15j: bot per channel (the driver configures and uses it) | `SKIP` |
| `DUDUCLAW_ISO_CHANNEL` (`discord`\|`line`\|`telegram`), `DUDUCLAW_ISO_CHANNEL_UNBOUND_TARGET` | 14h-i: a recipient that is deliberately not bound to alice | `SKIP` |
| `DUDUCLAW_ISO_SEARCH_API_KEY` | 15k: search provider | `SKIP` |

For the `=1` provider variables the operator asserts that the account is connected; the harness cannot run an OAuth consent screen. The harness reads no secret itself except the two database DSNs.

### Matrix coverage: what is local and what needs accounts

| # | Item | Fully local (no provider account) | Needs tool driver | Needs provider accounts |
|---|---|---|---|---|
| 1 † | owner `gmail_search` ok | 1a-b (Personal/Shared binding in the lifecycle list) | 1c (owner `db_select` on the sqlite fixture, the local stand-in) | 1d (Google) |
| 2 † | bob binding refused / forced ⇒ `personal_binding_conflict` | **2a-c, 2e** (RPC refusal, request refusal, trigger-bypass tamper ⇒ readiness reason, restore) | 2d | - |
| 3 | team Personal tool only via explicit home fallback | 3b-c (Personal route refused for a Shared employee; SharedHome route provisioned) | 3a, 3d-e | 3f (Google Home) |
| 4 † | member removed mid-run ⇒ refused ≤ 1 s | **4a** (removal ⇒ lifecycle unusable; latency printed, proxy via `isolation.employee_list`) | **4b** (real next tool call; latency printed) | - |
| 5 | tenant uuid replaced ⇒ all refused | 5a, 5d (tamper `auth_meta.isolation_tenant_id`, readiness refuses, restore) | 5b-c | - |
| 6 † | delete/recreate same label | **6a-d** (tombstone ⇒ quarantine, new employee id, old id listed as tombstoned, not addressable) | 6e (old capability); old receipts: see 13b / G2 | - |
| 7 | revoke between selection and send | - | - | 7 (Google + driver `race`) |
| 8 | revoke during Google refresh | - | - | 8 (Google + driver `race`) |
| 9 | revoke between Notion pages | - | - | 9 (Notion + driver `race`) |
| 10 † | gateway + spawnd restart ⇒ old capability refused | **10a-c** (both services restarted, pre-restart capability publications under `/run/duduclaw-spawnd` gone or re-issued, employees survive) | 10d-e | - |
| 11 | same-name relink ⇒ new id | **11a-c** | - | - |
| 12 † | missing factory/provider ⇒ `connector_setup_required` | **12a-b, 12d** (unconfigured `odoo` route, unknown database source) | 12c | - |
| 13 | background after password reset | 13a (pre-reset session refused) | 13b (record-backed run) | - |
| 14 | Odoo forbidden / DB non-SELECT / unbound channel recipient refused before credential | - | 14a-d (DELETE / DROP / stacked statement refused, table untouched; sqlite, no provider) | 14e-g (Odoo), 14h-i (channel) |
| 15 | live smoke per provider family | - | 15a (sqlite) | 15b-k (google, notion, github, odoo, postgres, mysql, discord, line, telegram, search) |

† = must also pass on Ubuntu 24.04 and Debian 12. Items 2, 4, 6, 10, 12 never need a provider account; their **bold** checks need nothing but the host. The checks that make a real tool call still need the driver, which is not a provider account but is not shipped (below).

### Tool driver contract (`DUDUCLAW_ISO_TOOL_DRIVER`)

A protected employee's MCP calls are authorised per live run by the gateway and spawnd; no dashboard RPC invokes an employee tool. The harness therefore delegates "make this employee call this tool" to an operator-supplied executable (for example one built on the G2 `iso_personal_chain` test support). It is invoked as below; it must print **exactly one JSON line** on stdout (exit status is ignored; stderr goes to `driver.err`):

| Invocation | Meaning | Reply |
|---|---|---|
| `driver call <label> <tool> <json-args>` | a fresh run of employee `<label>` as its legitimate principal calls `<tool>` | `{"outcome":"ok\|refused\|uncertain\|error","code":"...","route_kind":"personal\|shared_home\|...","credential_used":false}` (`code`, `route_kind`, `credential_used` optional) |
| `driver capture <label> <user> [--background]` | start a run for `<label>` as `<user>` (or a record-backed background run) and keep its capability | `{"handle":"<opaque>"}` |
| `driver replay <handle> <tool> <json-args>` | call with the previously captured capability | as `call` |
| `driver race <scenario> <label> <json>` | `revoke_before_send`, `revoke_during_refresh`, `revoke_between_pages`: perform the race, revoking through the admin RPC | `{"outcome":"uncertain\|refused\|ok\|error","requests_sent":N,"next_call":"refused","next_page":"refused"}` |
| `driver smoke <family> <label>` | one read-only smoke call for `google\|notion\|github\|odoo\|discord\|line\|telegram\|search` (the driver configures the provider from its env) | `{"outcome":"ok\|refused\|error"}` |
| `driver seed <label>` (H15, optional) | write the H15 work files `h15/note.txt`, `h15/table.csv`, `h15/book.xlsx` and the symlink `h15/link.txt → /etc/passwd` into the employee's `/work` (for example through the employee's own runtime). Without it, H15 seeds them with one `execute_program` call | `{"outcome":"ok","seeded":true}` |
| `driver cancel <label> <tool> <json-args> <after-ms>` (H15) | start the call, send MCP `notifications/cancelled` for its request id after `<after-ms>`, then make one more `execute_program` call (`print(1)`) in the same run | `{"outcome":"cancelled","run_alive_after":true,"next_call":"ok"}` |

Environment given to the driver: `DUDUCLAW_ISO_DATA` (service home), `DUDUCLAW_ISO_OUT`, `DUDUCLAW_ISO_RPC` (path of `connectors_rpc.py`), `DUDUCLAW_ISO_STATE_DIR` (private state with the admin and user sessions, usable with `connectors_rpc.py --state <dir>/admin.json call ... --stepup`), `DUDUCLAW_ISO_CANARY_URL`. Expected results the harness enforces: item 7 `uncertain` with `requests_sent=0`; item 8 `uncertain` with `next_call=refused`; item 9 `next_page=refused`; refusals in 14 must not report `credential_used=true`.

### How to read the results

* Stdout and `OUT/run.log` carry one line per check: `PASS|FAIL|SKIP <id> <detail>`; the end prints counts. Exit status `1` means at least one `FAIL` (or the run was interrupted); `0` means no FAIL, **which can still include SKIPs: read them**. A release acceptance needs the † items PASS on all three distros and every other item either PASS or an accepted, documented SKIP.
* `OUT/results.tsv` (execution order) and `results.sorted.tsv` (by id); `summary.txt`; `evidence/` (RPC responses without tokens, readiness snapshots, driver replies, `canary-hits.jsonl`); `install.log`, `cleanup.log`, `journal.log` (all `duduclaw*` units), `driver.err`.
* Timing checks (`04a`, `04b`, `02c`, `12a`) print the observed milliseconds in the detail. `04a` measures from the end of `users.remove` to the first `isolation.employee_list` read that reports the employee unusable (each probe costs one WebSocket round trip, ~50-150 ms, included in the number); `04b` includes the driver round trip. Both fail above 1000 ms.
* Final line `PASS cleanup` means the installation, units and service accounts are gone; `FAIL cleanup` keeps `/root/duduclaw-connectors-acceptance` for a later `--reset`.

### Known limits (read before trusting a result)

* Written against the v1.81 RPC surface and the F3 readiness reason names (`personal_binding_conflict`, `connector_setup_required`, `revoked`, `binding_evidence_missing`, `service_unavailable`). On a build without F3, 2c and 12a FAIL by design. **The script was written without a shell or a Linux host available: it has been read through but neither executed nor run through `bash -n` / `shellcheck` / `python3 -m py_compile`. Do those three first, then expect a first-run shakedown.**
* Response shapes the harness assumes and searches for (and reports as FAIL/SKIP with evidence if they differ): `isolation.employee_list` nodes carry `label`, `employee_id` and a status/usable field; the staff request response carries a request id; `migration.confirmations.list` items carry `category`, `status`, `binding`; `system.status` exposes `isolation_readiness`.
* Admin MFA: the harness enrols TOTP for the first-run admin through `mfa.enroll_begin`/`confirm`. If the build enforces an independent enrolment proof (`proof_required`), automation stops with a clear `FAIL setup`; enrol by hand (`--pause-after-setup` is too late for that) or run on a build/policy where the admin can enrol directly. Login of alice/bob/carol/dave must return a session without MFA for the same reason.
* 2c and 5 tamper with `users.db` as the service account (they drop and re-create `isolation_personal_binding_insert` / overwrite `auth_meta.isolation_tenant_id`) and restore it afterwards. This simulates a database-level attacker or an A3 trigger bypass; if the product opens `users.db` differently the check reports `SKIP` with the database error.
* 6b may legitimately FAIL while spawnd keeps the UID/storage of a tombstoned Personal employee and refuses a new live allocation with the same label (A2 open item 1).
* The sqlite stand-in proves binding/lifecycle/refusal behaviour, not a Google/Notion/GitHub/Odoo request path; those only count when their provider checks PASS.

## Legacy: `acceptance.sh` (superseded, kept for the record)

> Targets the removed `container_root_key` credential backend. Do not use it to accept v1.81. The text below is the original description.

在可丟棄的 Ubuntu 22.04 實體機／VM 上，驗收候選版的 systemd 安裝、資料搬移、憑證 RPC、程序隔離、拒絕條件、升級與 dashboard 更新限制。這套工具會修改系統、建立／移除服務帳號和資料，請使用專用主機及專用 sudo 使用者。

需要 systemd 為 PID 1、cgroup v2、初始 user namespace、Python 3.10+、util-linux `setpriv`、GNU coreutils、passwd/group 管理工具及 sudo 權限。升級與 dashboard 更新檢查需要連到官方 GitHub release。候選 binary 必須事先由操作者驗證；腳本將它放到 root 管理的 staging 路徑，這不等同驗證發行簽章。不得在容器或 AI 員工執行環境執行。

將候選 binary 與工具資料夾放在同一個目錄：

```text
candidate/
  duduclaw
  scripts/live-test/linux-protected/
    acceptance.sh
    rpc_probe.py
    README.md
```

由一般 sudo 使用者執行：

```sh
sudo sh candidate/scripts/live-test/linux-protected/acceptance.sh /absolute/path/candidate
# 保留安裝供調查；稍後用 --reset 先清理再跑一輪
sudo sh candidate/scripts/live-test/linux-protected/acceptance.sh /absolute/path/candidate --keep
sudo sh candidate/scripts/live-test/linux-protected/acceptance.sh /absolute/path/candidate --reset
```

結果同時寫入 stdout 與候選目錄的 `acceptance.log`，最後列出 PASS／FAIL 數量。有任何失敗就回傳非零。一般退出或可攔截的中斷都會清理，`--keep` 則保留安裝；斷電或 SIGKILL 後可用 `--reset` 清理。`--reset` 會刪除既有固定安裝路徑與服務帳號。fixture／docker group 的建立紀錄存在 root-only staging 內；清理失敗時保留紀錄，避免下次誤刪來源資料。預先存在的容器 marker 不會被當成測試資料移除。

只有當 sudo 使用者的 `~/.duduclaw` 不存在時，才以候選 binary 的 `onboard --yes` 建立 fixture，加入隨機測試 token。既有目錄保持不變；既有來源若只有加密 token、啟用 MFA 或正在執行，請改用全新的專用 sudo 使用者。不要拿個人使用中的資料目錄做 fixture。operator CLI 的暫存家目錄與 probe token 都在 root-only 暫存目錄，結束時刪除。

管理 token 在 DuDuClaw home 的 `config.toml`，key 為 `[gateway] auth_token_enc`（加密）或舊式 `auth_token`（明文）；環境變數 `DUDUCLAW_AUTH_TOKEN` 優先。首次啟動沒有自動生成的 token 檔，`onboard --yes` 也不會產生 token。測試工具使用自行產生的 `auth_token`，以 root 讀入 0600 暫存檔；probe 不解密 `auth_token_enc`，也不輸出 token。獨立使用 probe 時，需先把有效明文 token 安全寫到 0600 檔案：

```sh
python3 rpc_probe.py --url ws://127.0.0.1:18889/ws \
  --token-file /root/probe-token --method security.credential_protection --params '{}'
```

省略 `--url` 時依 `DUDUCLAW_PORT`、`DUDUCLAW_HOME/config.toml` 的 `[gateway] port`、18889 依序決定；未設定 home 時讀 `~/.duduclaw`。Python 3.10 無內建 TOML parser，fallback 支援一般 `[gateway]`／`port` 寫法；特殊 TOML 請明確傳 `--url`。probe 僅支援 `ws://`，應走 loopback 或 SSH tunnel。

健康判定：`level`、`platform_level` 都是 `process_isolated`，`backend=container_root_key`、`state=ready`、`person_identity_allowed=true`、`refusal_reason=null`。負向測試限定本次 journal cursor 後的預期 launcher 拒絕，或 RPC 的 `unavailable`／指定原因；每次還原後重新驗證健康。抽象 socket 測試保持 nobody listener 70 秒，跨過每分鐘重驗。

升級項目刻意保留嚴格判定：題目基準的官方 v1.79.2 比候選版舊，實作會在下載前拒絕，因此無法驗收「成功降級至 v1.79.2」。腳本仍呼叫 latest 升級並記 FAIL，繼續測試。只有未來較新的官方版本成功通過簽章驗證／安裝回報、inode 更換、unit 重啟與版本下限檢查才記 PASS。inode 更換是執行後證據；atomic rename/fsync 的中途行為需另做故障注入才能活驗。接著明確執行**僅供測試的回復**：root 移除 floor／pending，再以 staging 候選版無搬移重裝，供 dashboard RPC 驗收與清理使用。

本工具目前僅做過 shell 語法與 Python 編譯檢查，尚未在 Linux root 環境執行。
