# Linux 原生保護模式驗收工具

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
