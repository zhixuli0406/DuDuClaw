# 原生 Linux 受保護服務

v1.80 架構將 gateway、AI 員工、文件解析與特權更新分開。每位經確認的員工有永久的 Linux 身分；每次執行有獨立的程序、檔案系統與網路命名空間。必要隔離元件不可用時，員工執行會被拒絕。

**開發狀態：** 已實作簽章驗證後的未啟用世代暫存、安裝器遷移轉接、首位管理員設定、唯讀安裝檢查與固定路徑的協調移除。完整新安裝、簽章啟用、遷移平台與 gateway 綜合就緒介面仍未完成；本輪未做 root 實機安裝／移除驗收。服務顯示 `active` 或世代已暫存，都不代表隔離就緒。詳見[實作與驗收紀錄](../../architecture/native-linux-isolation.md)。

## 使用者會遇到的變化

- 要求隔離時，員工不再使用 gateway 帳號執行。同一位員工一次執行一項工作，其餘對話與排程會排隊。
- 員工仍讀得到自己的 CLI 登入權杖；隔離執行路徑不提供其他員工的 home 或 gateway 憑證。個人連線需要經確認、只有一位擁有者的 Personal 員工；Shared 員工不能使用個人憑證。
- 對外流量經過獨立代理，封鎖私有網路、主機 loopback 與雲端 metadata。公開目的地沒有網域白名單限制；不支援代理的工具會連線失敗。
- 不可信文件交給拋棄式低權限工作者解析。工作者沒有網路，只透過檔案描述符接收有大小上限的輸入。
- 更新改由獨立 root 服務驗證元件簽章清單。下載完封存檔或收到更新要求，都不代表更新成功。
- 從 v1.79.3 遷移需要新的憑證與管理員確認。舊帳號、綁定、政策、hook、MCP 伺服器及排程不會因匯入而直接生效。

保護前提是 kernel、systemd、gateway、啟動代理、trampoline、對外連線代理及更新服務仍可信任。這不提供 VM 等級的敵對多租戶隔離，也不防範主機 root 入侵；同一員工不同執行之間仍共用持久檔案。一般使用者安裝不具備此隔離保證。

## 目標主機與已觀察的測試

需要原生 Linux、systemd 為 PID 1、單一 cgroup v2 階層與初始 user namespace。容器、重新映射／rootless namespace、cgroup v1 或混合模式會被拒絕。官方 CLI 執行檔需要 glibc 2.31 以上；符合執行檔最低需求不等於支援隔離。

目標發行版是 Ubuntu 22.04、Ubuntu 24.04 與 Debian 12。2026-11-05 的 **套件層級** root 測試紀錄如下：

| 發行版 | 已測 kernel | systemd | Landlock ABI | 結果 |
| --- | --- | --- | --- | --- |
| Ubuntu 22.04 | 5.15.0-1120-gcp | 249.11 | 1 | 套件測試 66/66 通過 |
| Ubuntu 22.04，GCP 預設／HWE | 6.8.0-1070-gcp | 249.11 | 4 | 套件測試 66/66 通過 |
| Ubuntu 24.04 | 6.8.0-1071-gcp | 255.4 | 4 | 套件測試 66/66 通過 |
| Ubuntu 24.04，GCP 映像預設 | 7.0.0-1014-gcp | 255.4 | 8 | 不支援：16 項 trampoline 測試全數拒絕，其餘 50 項通過 |
| Debian 12 | 6.1.0-54-cloud-amd64 | 252.39 | 2 | 套件測試 66/66 通過 |

Kernel 7.0 **目前不接受**：trampoline 會拒絕大於 7 的 Landlock ABI。請確認實際開機使用的 kernel，不能只看發行版或 GCP 映像名稱。驗收請使用已接受的 kernel 組態，不要關閉 Landlock 或自行放寬 ABI 檢查。上表不涵蓋完整安裝、真實廠商 CLI 執行、完整遷移或更新復原。

主機需有可信任的系統工具，包括 `getent`、`id`、`useradd`、util-linux、systemd 工具及 curl。員工 loop 儲存空間需要 ext4 或 XFS、受支援且非 thin provisioning 的底層裝置鏈，並保留員工配額與 TCB 所需空間。未知儲存配置或容量不足會使就緒檢查失敗。使用外部 IAM／NSS 時，必須永久保留員工 UID 範圍。

## 安裝與就緒檢查

由管理員在自己的終端機執行，不能在 AI 工作階段內操作。取得 x86_64 或 aarch64 的完整發行包，放在 root 擁有、上層目錄不可由其他帳號寫入的暫存區，完成驗證後才以 root 執行。Linux 封存檔含分開的元件執行檔；只複製 `duduclaw` 無法安裝這個架構。

完整安裝需要以下身分與資料：

| 項目 | 必要狀態 |
| --- | --- |
| 系統身分 | `duduclaw`、`duduclaw-runsup`、`duduclaw-egress`、`duduclaw-dl`、`duduclaw-dns`；靜態 ID 小於 `SYS_UID_MAX`，避開 DynamicUser 與員工範圍 |
| 員工身分 | 永久登記，預設 70000–99999；只在員工確認後配置 |
| 可信程式 | Gateway、spawnd、trampoline、egress／forwarder／DNS broker、更新服務／凍結啟動器、解析器、匯入器、遷移命令與 MCP stdio 橋接；由 root 擁有 |
| 服務 | Gateway、spawnd socket／service、egress socket／service、選用的 DNS broker、更新服務、parse socket／template、TCB 與員工 slice |
| 解析根目錄 | `/usr/lib/duduclaw/parse-root` 與 `/usr/lib/duduclaw/parse-root-tree`，root:root 0755，初始為空，不能放主機資料 |
| 簽章狀態 | Bootstrap 信任、相容的元件世代、runtime 登錄表、更新槽與狀態、遷移及就緒檢查接線 |

解析器可能留下[安裝參考](../../../crates/duduclaw-parse-worker/README.md)列出的已知空掛載點目錄。遇到其他內容應先調查，不要遞迴清空解析根目錄。Spawnd 負責員工的 SSH、cron、at 與 user manager 封鎖，並驗證主機上的有效狀態；不要手動放寬這些控制來通過檢查。

安裝器先驗證固定路徑的完整元件，再準備帳號、目錄與 unit。Linux 發行包已加入凍結的更新啟動器；離線工具可將已簽署的資料與元件組成 bootstrap inbox。經獨立驗證的 `duduclaw-updated --bootstrap-stage` 會檢查編譯時固定的根指紋、根金鑰門檻簽章、target 簽章與全部元件雜湊，再發布**尚未啟用的世代**；不選擇 `current`、不建立 trust／state／slots、不驗證舊版過渡雙簽，也不啟動服務。尚無可直接照做的完整新安裝步驟。

可執行 `sudo duduclaw service readiness --protected-linux`（`ops` 為相同入口的別名），唯讀檢查執行檔、系統帳號、目錄權限、unit 內容與 systemd 實際屬性，並分項回報。I1 的綜合就緒介面尚未接上，`MissingComposite` 一律拒絕，不能因此放行員工執行。自訂 DNS drop-in 尚未納入驗證，目前會使 unit 檢查失敗。已由新版管理的安裝再次執行安裝時，只跑唯讀檢查，不改寫世代執行檔或服務單元。

## 從 v1.79.3 遷移

不要原地替換舊執行檔，也不要把舊資料庫複製進新 home。舊環境與員工共用 UID，其權威資料必須視為不可信。安裝器已將 v1.79.3 跨入 v1.80（含無法確認原版本下限）的要求交給 G 遷移流程，固定讀取 root 擁有的 `/var/lib/duduclaw-migrate/plan.json`；計畫缺少或無法驗證時，在停止舊服務前拒絕，不退回原地更新。

管理員流程如下：

1. 從官網及登入後的授權 console，取得 bootstrap 驗證程式、遷移包的 SHA-256 與 root metadata 指紋，兩處必須一致。將下載內容存為檔案，放在 root 擁有的 0700 目錄；執行驗證程式前，先用系統 `sha256sum -c` 比對。舊封存檔簽章不足以建立新信任根，不要把下載內容用管線交給 shell。
2. 經驗證的安裝器檢查 bootstrap 簽章、準備簽章平台，並產生不可由其他帳號修改的 root 遷移計畫。不要自行修改計畫或製造成功讀回資料。
3. 遷移程式會在**停止舊服務前**驗證平台，再停用舊啟動來源、隔離舊資料、產生驗證過的加密快照、輪替金鑰，啟動乾淨且只接受匯入的 gateway。低權限匯入器將已知內容轉成有界紀錄；權威資料只成為未生效提案。
4. 隔離驗證通過後，依安裝器提供的設定流程建立新的管理員。舊 session、密碼、MFA 及 CLI 登入狀態不會恢復；審核高風險項目前先註冊 MFA。
5. 開啟「**系統 → 遷移確認**」，檢查完整值、核准或拒絕提案、重試套用失敗項目，完成要求的憑證輪替。員工 ID 與儲存空間只在確認後配置；個人帳號由新的擁有者重新連結。
6. 全部套用經實際讀回、沒有待確認或待輪替項目後，才能完成。`AwaitingAdministrator` 表示仍在等待管理員。Root 專用復原快照須保留到符合保存期限與復原驗證條件。

支援的來源版本：對話紀錄（`sessions.db`）、目標與排程交辦（`tasks.db`）及排程工作（`cron_tasks.db`）可從 v1.79.3 或 v1.80.x 的資料目錄匯入。每個資料庫都必須與一個已知版本完全相符；手動改過、只升級一半或其他版本的資料庫會整份拒絕，匯入紀錄會列出最接近的已知版本及不同的資料表、索引或觸發器（不含內容）。外部 A2A 用戶端擁有的任務一律不匯入，排程工作原本的委託人、建立者與代理人也不會沿用。v1.79.3 的定義是依版本說明重建，並非從 v1.79.3 安裝擷取，仍需在實際的 v1.79.3 資料目錄上驗證。

完整、已驗證的安裝可使用既有 runner 介面：

```sh
sudo /usr/lib/duduclaw/current/duduclaw-migrate run --plan /var/lib/duduclaw-migrate/plan.json
sudo /usr/lib/duduclaw/current/duduclaw-migrate status --plan /var/lib/duduclaw-migrate/plan.json
```

這是介面範例，**不是已完成端到端驗收的部署步驟**。固定計畫及簽章平台須由經驗證的安裝器提供，目前仍無完整可用的安裝產出；缺少 `duduclaw-migration-platform` 時，runner 會在停機前拒絕。首位管理員已接上驗證 root 對端的 Unix socket 與 loopback 設定頁，token 放在網址 fragment，帳號與新密碼以同一交易建立；遠端管理員須先建立該本機連接埠的 SSH 通道。root 擁有的 `/etc/duduclaw/migration-import.json` 持久保存遷移模式，避免重啟後 `/run` 資料消失便退回一般管理員初始化。這些接線不代表完整遷移成功。不要把 `prepare --stop-after-keys` 當作升級捷徑，它會刻意讓安裝停留在離線狀態。

確認頁需要有效的 Admin session、密碼及已註冊驗證器的新驗證碼；授權有效五分鐘，綁定目前 session。高風險項目須逐項核准，批次核准只限低風險項目。核准與套用成功是不同狀態；不支援的類別繼續封鎖，需由支援的流程重新輸入。舊待核准項目不會變成新授權。

## 為服務帳號安裝 AI CLI

服務以 `duduclaw` 帳號執行，`HOME=/var/lib/duduclaw`、`PATH=/usr/sbin:/usr/bin:/sbin:/bin`。AI CLI 必須安裝在這些系統 PATH 目錄之一，且服務帳號必須有讀取與執行權限。`ProtectHome=yes` 會隱藏 `/home`、`/root` 與 `/run/user`；即使 root 找得到 `/home/operator/.local/bin/claude`，服務也無法使用。在系統目錄放一個指向隱藏家目錄的符號連結也無效。

Claude Code 請先把 `<ver>` 換成要安裝的版本，再執行：

```sh
cd / && sudo -H npm install -g --prefix /usr @anthropic-ai/claude-code@<ver>
sudo -u duduclaw -H claude auth login
```

`-H` 與 `--prefix /usr` 都要保留，否則原使用者的 `~/.npmrc` prefix 設定可能讓 npm 又把 CLI 裝回使用者家目錄。若設定使用 Codex、Grok、AGY 或 Gemini，也要安裝到相同的服務可讀系統 PATH，並依各 CLI 的登入方式替服務帳號登入。

`service install --protected-linux` 與 `service upgrade --protected-linux` 結尾會檢查設定中的主要推論 runtime，以及各員工 `agent.toml` 選用的 runtime；有遷移時讀取遷移後的目的地設定。檢查以服務帳號及服務 PATH 確認執行權限，並排除被隱藏的路徑。只安裝在原使用者家目錄的 CLI 會明確標示為服務無法使用。主要 runtime 缺少時會印出警告，並以非零狀態結束。只有刻意稍後才設定 runtime 時，才在上述任一指令加上 `--allow-missing-runtime`；此旗標不會代為安裝或登入。檢查失敗不會撤回已執行的安裝或升級。透過下方獨立更新服務升級時，`service upgrade --protected-linux` 會在送出要求前執行此檢查，檢查失敗就不送出。

遷移只複製 `.duduclaw` 資料，**不會搬移 AI CLI 或其登入憑證**。Claude 已安裝但服務帳號尚未登入時，安裝流程會顯示上面的服務帳號登入指令。請另行登入，不要複製原使用者家目錄的憑證。Gateway 啟動時若找不到設定的推論執行檔，會記錄 ERROR 與修復方式；runtime 狀態與 doctor 也會顯示受保護服務專用的指引。

## 升級與解除安裝

完成遷移及更新服務佈署後，Admin 可在儀表板要求正式版更新，或使用已安裝的 CLI：

```sh
sudo /opt/duduclaw/bin/duduclaw service upgrade --protected-linux --version X.Y.Z
```

只有版本與要求 ID 會跨入更新邊界。更新服務驗證簽章中繼資料、雜湊、平台、到期日、版本下限與序號，切換完整世代後讀回實際執行版本及協定，才提交結果。服務重啟後仍以同一要求追蹤，只有 `committed` 才算成功。服務、信任或就緒資料缺失會拒絕更新，不會呼叫舊安裝器。一般更新不能降版，緊急回退需要另外的簽章授權。

移除入口為 `sudo duduclaw service uninstall --protected-linux`。隔離安裝會先把 unit 備份到 `/var/lib/duduclaw-protected/uninstall-units`，建立持久的 `/etc` mask 並讀回，再停止 timer、socket 與服務；只有確認服務停止、已知 cgroup 為空、沒有服務／員工程序、已知儲存根目錄下沒有掛載後，才移除固定路徑的執行檔。不執行強制或 lazy unmount。金鑰、資料、系統帳號、永久 UID 對照、更新狀態／世代及版本下限均保留，mask 也保留以阻止自動重啟。H 的任意自訂儲存根目錄介面仍未接上，不能視為自訂儲存配置的完整移除驗收。本輪未做 root 實機移除；不要用舊 uninstaller 或刪除保留狀態來繞過拒絕。

## 每晚自動更新

Timer 在主機當地時間 03:30 要求獨立更新服務檢查更新，最多隨機延後 20 分鐘。`Persistent=false` 避免夜間未開機時，隔天開機立刻補跑。固定 helper 呼叫 `/usr/libexec/duduclaw-updated-launch --check-latest`；加 `--dry-run` 只查候選版本，不送出 Apply。

候選查詢以 `duduclaw-dl` 執行。未簽章的發行索引只選擇非草稿的正式版候選，root 更新服務仍需驗證簽章清單。要求端不安裝下載的程式，也不退回舊更新流程。完整更新服務與信任狀態安裝、驗證後才啟用排程；不要把開發中 helper 複製到 v1.79.3。使用經審查、固定版本的 [timer 安裝器](../../../scripts/linux-protected/auto-upgrade/README.md)，不要使用持續變動的 `main`。

## 就緒檢查故障排除

以下指令用於檢查已安裝元件，不代表完整安裝驗證：

```sh
sudo duduclaw service readiness --protected-linux
sudo /usr/lib/duduclaw/duduclaw-spawnd --check
sudo systemctl status duduclaw-gateway.service duduclaw-spawnd.service duduclaw-egress.service duduclaw-updated.service
sudo journalctl -u duduclaw-gateway.service -u duduclaw-spawnd.service -u duduclaw-egress.service -u duduclaw-updated.service -n 100 --no-pager
```

| 現象 | 處理 |
| --- | --- |
| `UnsupportedKernel` | 檢查實際 kernel 與 Landlock ABI；ABI 8／kernel 7.0 目前會拒絕，保留此檢查。 |
| 就緒資料缺失、過期或不相容 | 確認完整世代、協定版本、帳號登錄表與全部服務；只有 `active` 不足以放行。 |
| UID／帳號／SSH／IAM 失敗 | 處理保留範圍衝突、別名、附加群組、登入／cron／at 或 linger；外部 IAM 保留需管理員證明。 |
| 儲存／配額失敗 | 確認底層裝置鏈、硬性配額、可用容量與主機保留空間；不能用磁碟用量輪詢代替硬性配額。 |
| DNS／egress 失敗 | 檢查 root 指定的 resolved Varlink 或 DNS broker 路徑；不要替 egress 服務加入私有 resolver IP 例外。 |
| 解析根目錄失敗 | 檢查兩個根目錄的擁有者、權限與非預期內容；不要複製主機 `/etc` 或資料進去。 |
| 遷移平台或設定連結不可用 | 保留既有環境，取得完整驗證過的整合；不要搬移舊權威或繞過遷移閘。 |
| 等待確認／輪替 | 在遷移確認頁完成支援的確認、重試與新憑證設定；核准本身不代表遷移完成。 |
| 更新已受理但未 committed | 追蹤持久要求狀態與就緒錯誤，不要同時手動替換執行檔。 |

受保護員工環境不提供容器引擎與依賴主機硬體的功能。所有必要元件均需通過檢查才能恢復工作；套件測試通過或本指南都不能當作正式環境驗收。

## v1.80.x 已發行的單一執行檔受保護服務

以下各節說明 v1.80.x 發行版的受保護服務：gateway 是 root 安裝的單一執行檔，`service upgrade --protected-linux` 會在程序內驗證並替換執行檔。這些內容仍是上述發行版，以及隔離架構沿用的憑證保護控制（引擎 socket 政策與自動復原）的參考。以本開發分支建置的版本，`service upgrade --protected-linux` 改為向上方的獨立更新服務送出要求，不會自行替換執行檔。

官方 GitHub 更新會精確核對 minisign 簽章註解 `duduclaw v{版本} {封存檔名}`，包含平台與架構；Pro 更新則使用 `duduclaw-pro` 及該管道自己的固定公鑰。封存檔簽章與涵蓋註解的全域簽章都必須通過。所有封存檔安裝路徑都必須提供驗證器產生的 `VerifiedReleaseIdentity`，綁定封存檔摘要、版本、檔名與簽章註解，僅通過完整性檢查不能安裝。換檔前還會確認執行檔版本。Linux 受保護升級只讀取編譯時的固定格式標記，不以 root 執行下載的程式：標記必須恰有一筆，header 為 `NUL + DUDUCLAW_RELEASE_ID:V1:`，接穩定版 `X.Y.Z`，suffix 為 `NUL + DUDUCLAW_RELEASE_END + NUL`。每個 `DUDUCLAW_RELEASE_ID` 都必須構成完整標記，缺少、畸形、重複或衝突一律拒絕；沒有標記的舊版封存檔也不接受。發版流程會在簽章前解析實際封存檔內的執行檔，確認標記與 tag 相同，Pro 打包也適用。一般更新則核對 `version` 輸出。任何不符都會在換檔及寫入受保護版本意圖／下限前中止。這些檢查適用於已包含修補的更新程式；舊更新程式仍需先由管理員審查並手動更新。

Provider 整合需改用 `verify_release_archive_with_pubkey(bytes, checksum, signature, pinned_key, expected_version, expected_file)`，並把結果傳給 `install_verified_binary(bytes, &identity)`。預期版本與檔名必須來自選定的發行資訊，不能直接從下載的簽章取值；原本只有一個參數的安裝 API 已移除。

## 自動升級（選用）

`scripts/linux-protected/auto-upgrade/` 從 v1.80.0 起收錄於原始碼，v1.79.3 以前的標籤沒有這個目錄。請從固定的完整 commit SHA 取得並審閱，例如 `v1.80.0` 標籤指向的 commit（下方以 `<commit>` 表示，請換成你審閱過的那一筆），不要使用會移動的 `main`（[原始碼位置](https://github.com/zhixuli0406/DuDuClaw/tree/<commit>/scripts/linux-protected/auto-upgrade)）；Linux 發行封存檔也不含 `scripts/`。它只呼叫既有的 `service upgrade --protected-linux --version`，因此可搭配 v1.79.3 受保護安裝。

排程每天當地時間 03:30 執行，最多隨機延遲 20 分鐘，早上開機不補跑。較新的官方版本經既有流程驗簽安裝後，會重啟原本執行中的 gateway，請避開工作時段。未安裝排程則維持手動升級。不論是否安裝排程，儀表板在此模式都無法執行更新，只會顯示新版本與手動 `sudo` 指令。

審閱四個腳本／unit 檔後，將整個目錄複製到 `/root/duduclaw-auto-upgrade`，確認檔案與所有上層目錄均由 root 管理、非 root 不可寫入，再執行：

```sh
# Replace <commit> with the reviewed full commit SHA; obtain files from that SHA.
# Run from the reviewed checkout; the destination must not already exist.
sudo cp -r --no-preserve=ownership scripts/linux-protected/auto-upgrade /root/duduclaw-auto-upgrade
sudo chown -R root:root /root/duduclaw-auto-upgrade
sudo chmod -R go-w /root/duduclaw-auto-upgrade
sudo sh /root/duduclaw-auto-upgrade/install.sh
sudo /usr/local/sbin/duduclaw-auto-upgrade --dry-run
systemctl list-timers duduclaw-auto-upgrade.timer
sudo journalctl -u duduclaw-auto-upgrade.service
```
不要以 root 執行使用者可寫入目錄中的 `install.sh`。檔案可能在審閱後、複製前被替換，請再核對 root 副本；安裝程式會拒絕來源目錄與四個腳本／unit 檔案的符號連結、非一般檔案、非 root:root 擁有或群組／其他人可寫入的項目，也會檢查解析後目錄至 `/` 的每一層祖先；即使有 sticky bit 的 `/tmp` 也不接受。通過後，先複製至 `/run` 下的 root 專用 0700 暫存目錄，再檢查副本為一般檔案且大小為 1 byte 至 1 MiB，最後從副本安裝並清理暫存。這些權限檢查無法證明程式碼出處；仍須核對固定 commit，改成 root 所有不代表已驗證內容。

需要 GNU coreutils（含 `sort -V`）、curl、grep、sed、tr、systemd 與既有升級流程所需工具。腳本以 root 的 `version-floor` 為已安裝版本；下限遺失、格式錯誤或存在 `version-pending` 時停止，需人工處理。只檢查最近 30 筆 release，選最高的純 `vX.Y.Z`，忽略 `desktop-v*` 與帶後綴的 tag。沒有較新版就正常結束，不下載封存檔；API 失敗或沒有合格 tag 則失敗且不升級。

解析支援單行壓縮與多行 JSON，只檢查外層陣列並擷取版本欄位，未完整驗證 JSON 語法，也不解析 `prerelease`：純 `vX.Y.Z` 即使標為 prerelease 仍納入。未登入 API 不回傳草稿。若 release 先發布、Linux 封存檔尚未上傳完成，當晚升級會失敗，隔晚重試；請查看 journal。

API 回應上限為 8 MiB，保留 60 秒逾時；curl 的 `--max-filesize` 在舊版僅能檢查已知 Content-Length，因此回應先寫入私有 `/tmp` 下 0700 目錄中的暫存檔，解析前再檢查實際大小，退出時清理。systemd 的 `PrivateTmp=yes` 提供私有且可寫入的 `/tmp`；主機寫入只開放 `/opt/duduclaw` 與 `/var/lib/duduclaw-protected`，服務啟動逾時設為 1 小時以容納下載重試與安裝／重啟。純 `vX.Y.Z` 即使 GitHub 標為 prerelease，較新且驗簽通過仍會安裝。封存檔的 minisign 簽章是信任錨點，版本選擇本身沒有簽章。

移除排程、腳本與 unit（不移除 gateway 或其資料）；若升級仍在執行，會先停用 timer 再拒絕移除，請等升級完成後重跑：

```sh
sudo sh /root/duduclaw-auto-upgrade/install.sh --uninstall
```

## 保護範圍與限制

**功能限制**：原生受保護模式透過 `InaccessiblePaths` 阻擋容器引擎，並以 `PrivateDevices` 隔離裝置，因此無法使用依賴容器的任務沙箱、電腦操作及依賴硬體的功能。這是刻意的限制：AI 員工不得接觸容器引擎。

啟動腳本確認執行檔、所有上層目錄及自身安裝位置屬於 root，且其他使用者不能寫入。確認帳號與清除同 uid 殘留程序後才開啟金鑰，接著降 uid/gid、清空附加群組與所有 capabilities，開啟 `no_new_privs`。Gateway 接收金鑰前先設為 non-dumpable，再檢查 systemd 父程序、精確的 cgroup v2 記錄 `0::/system.slice/duduclaw-gateway.service`、執行檔權限、程序權限，以及自身無法開啟根金鑰。原生安裝程式、開啟金鑰前的啟動腳本，以及 Rust 驗證都會拒絕非初始 user namespace。若存在 `/run/.containerenv`、`/run/systemd/container`，或 PID 1 環境含 `container=`，也會拒絕。root 安裝程式與啟動腳本必須能讀取 `/proc/1/environ`；一般權限的 gateway 只有在權限不足時可略過該讀取。偵測到容器時仍使用容器規則，不能靠模式變數取得原生保護。

預設 `revoke` 政策下，已知 Docker／Podman／containerd socket 路徑必須不存在或不可存取：寫入權限探測與非阻塞連線都必須被拒。`DOCKER_HOST` 必須未設定。自訂 slice、cgroup v1／混合階層、非預期的 sudo 輸出、無法確認的探測一律拒絕。啟動拒絕原因可在 journal 或憑證保護卡片查看；不要放寬權限繞過檢查。

服務另外套用以下設定：

```ini
InaccessiblePaths=-/run/docker.sock -/var/run/docker.sock -/run/containerd -/run/podman -/var/run/podman -/run/user -/run/docker
PrivateDevices=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
RestrictSUIDSGID=yes
LockPersonality=yes
SystemCallArchitectures=native
```

依 [systemd.exec](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html) 規則，每個 `-` 前綴會忽略不存在的路徑。這些設定隱藏常見引擎端點、隔離硬體裝置、禁止修改核心／模組／cgroup 與建立 set-ID 檔案，並限制執行模式及 syscall 架構。未設定 `RestrictNamespaces`，因為 gateway 的 Codex／bwrap 工作沙箱需要 user、mount、PID 與 network namespace。

Gateway 在啟動時及每 60 秒重新檢查常見引擎 socket 的可達性與附加群組。預設 `revoke` 政策下，定期檢查失敗時會原位清零持有的金鑰、將保護狀態改為拒絕並記錄警告；個人連線暫停，直到重新啟動並通過完整金鑰交接檢查。撤銷後監測仍會繼續；自動恢復方式見下節。也會檢查 `/proc/net/unix` 的監聽中 abstract socket：名稱以 `/` 分段後，任一段完全等於 `docker`、`containerd`、`podman`、`buildkit`、`lxd`，或以這些名稱接 `.`、`-`、`_` 開頭，就會列為候選；不以任意子字串比對。 名稱命中本身不會造成拒絕：Gateway 會以短逾時的非阻塞連線取得 `SO_PEERCRED`，只有連得上且 peer uid 為 0 才視為風險。無法連線的候選會略過。資料表改以原始 bytes 讀取，非 UTF-8 名稱不會阻斷其他 socket 的檢查。每一行都必須完整解析；欄位或 flags 無效、名稱有歧義或換行殘片時，會拒絕接收金鑰，已持有的金鑰則撤銷。任何狀態下讀取資料表失敗都會拒絕通過；撤銷後也會重置連續通過次數。接受安全結果前，還必須透過 `NETLINK_SOCK_DIAG` 檢查附有長度的 `UNIX_DIAG_NAME`，包含非引擎名稱；只要含換行就拒絕，因此換行後即使偽造成合法資料列也不能略過。診斷 dump 無法取得、格式錯誤、中斷、超過 8 MiB 或逾時 250 ms，也會拒絕通過；核心須支援 UNIX socket diagnostics。兩項檢查都是定期觀察，並非所有 socket 的原子快照。

完全符合 `@docker_cli_` 加上 32 個小寫十六進位字元、且連得上 peer uid 0 的 abstract socket，一律視為引擎介面。預設 `revoke` 會撤銷金鑰；管理員選擇 `warn` 時則保留金鑰並持續顯示警告。名稱本身不提供豁免。名稱辨識只用來顯示提示：這通常表示有人在這台主機以 root 執行 `docker` 指令（例如 `sudo docker compose …`）。`revoke` 下，執行期間個人帳號會暫停；指令結束、環境檢查通過且自動恢復可用後，通常幾分鐘內會恢復。若要避免此情況，請用 docker 群組的一般使用者執行 docker，或將開發容器移到其他主機。

AI 員工的 Bash／工具仍以 gateway 的 uid 執行。保護依賴 non-dumpable、root 擁有的金鑰與程式、沒有 sudo／linger 權限，以及無法存取引擎 socket。員工仍能修改可寫資料、停止 gateway、要求已授權操作，並讀取自己回合取得的存取權杖。主機不得授予 `duduclaw` 帳號存取容器引擎或其他特權代理的權限。這些檢查只涵蓋啟動時與每分鐘檢查的常見端點，並非無條件的隔離保證。未識別名稱的 abstract socket、其他路徑的引擎 socket、特權代理、其他提權途徑、主機 root 遭入侵，以及已在 gateway 內執行的惡意程式不在防護範圍；兩次檢查之間也可能出現新的存取途徑。執行檔擁有者檢查防止服務 uid 修改程式；它不能驗證惡意 root 管理員交來的程式簽章。

根金鑰不提供通用沙箱。持有金鑰時，同樣禁止在 gateway 內載入 ONNX：NER 遮罩、本機 embedding 與 Kokoro 無法使用；需要遮罩時請使用正規表示式規則。同時包含資料與根金鑰的備份可解開憑證。只在儀表板更換保護金鑰不能防止還原舊資料；懷疑根金鑰外洩時，主機管理員應停止服務、安全更換根金鑰、重設保護並重新連結帳號。

原生 systemd 安裝、不同 sudo 版本與 socket 探測仍需在目標發行版實測。單元測試或 macOS 編譯通過不代表原生 Linux 驗收通過。

## 選擇容器引擎政策

預設 `engine_socket_action = "revoke"`：引擎控制介面可被存取時，拒絕接收金鑰或撤銷已持有的金鑰。若必須與開發容器共用主機且需維持服務，主機管理員可選擇 `warn`。例外只適用於原生 Linux 模式下已確認的 `engine_socket_reachable`，涵蓋啟動交接與每 60 秒檢查。其他失敗仍會拒絕或撤銷，包括附加群組、執行檔權限、cgroup／unit、user namespace、容器環境、可讀取根金鑰路徑、socket 表無法讀取或格式錯誤，以及診斷失敗。政策不會變更 systemd 隔離，也不會開放依賴容器的功能。

改成「只警告」後，主機上有程式開放容器引擎控制介面時，DuDuClaw 不會停用同仁的 Google 等個人帳號。能操作容器引擎的程式等同擁有整台主機的最高權限，若 AI 員工能碰到它，就可能讀出解密金鑰。只建議在您能確認 AI 員工碰不到容器引擎、且需要在同一台主機執行容器時使用；更安全的做法是把開發用的容器移到另一台主機，或以一般帳號（docker 群組）執行 docker。

請在自己的終端機執行，勿在 AI 工作階段內操作。使用經驗證且由 root 擁有的程式；若 root 的 PATH 找不到 `duduclaw`，請改用 `/opt/duduclaw/bin/duduclaw`：

```sh
sudo duduclaw service protected-linux policy show
sudo duduclaw service protected-linux policy engine-socket warn --yes
sudo duduclaw service protected-linux policy engine-socket revoke
```

指令要求 root，並拒絕 AI 工作階段。改成 `warn` 時會印出風險說明，必須加上 `--yes`。唯一政策來源是 `/etc/duduclaw/protected-policy.toml`，不讀取服務帳號的 `config.toml`。Gateway 在接收金鑰與每次定期判斷前，以固定的目錄及檔案描述符開啟政策：檔案須為 root 擁有的普通檔、僅一個硬連結、大小有限且格式嚴格正確；所有上層目錄也須為 root 擁有，檔案與目錄均不得允許群組或其他人寫入。缺少、讀取失敗或驗證不符時，一律採 `revoke`。root 指令以暫存檔、同步檔案、原子更名及同步目錄的順序寫入。

政策變更會追加到 root 擁有的 `/etc/duduclaw/protected-policy-audit.jsonl`：先同步操作意圖，再原子寫入政策，最後記錄變更。Gateway 下次讀取時會匯入安全稽核索引。`warn` 下偵測到可存取引擎時，會記錄含探測細節的 WARN，憑證保護面板在狀況持續期間顯示警告。警告開始與結束各留一筆稽核，不會每 60 秒重複記錄同一狀態。面板提供目前政策、風險及切換指令，沒有修改按鈕。政策變更於下一次檢查套用；已撤銷的金鑰仍須經下節的恢復流程，由 root 啟動器重新交接。

警告轉態會先同步保存至 `<home>/state/engine-socket-warning-events.json`，兩份稽核紀錄匯入成功後才確認移除。這份服務帳號擁有的日誌僅供診斷，不能授予政策權限。root 稽核紀錄上限為 4 MiB；達上限後，指令會拒絕變更，須由管理者先封存紀錄。既有政策目錄須可供服務讀取及穿越；指令會拒絕權限過嚴的目錄，不會自行修改既有目錄權限。

政策變更與執行期警告採用各自的事件允許清單，以及 `root:`／`runtime:` ID 空間。每次匯入先從固定描述符驗證的 root 日誌重建政策索引，並在重建時依 ID 去重；服務帳號可寫的 JSONL 與 SQLite 收據不再代表 root 事件已匯入。損壞的執行期紀錄會逐筆略過、計數並警告，不阻擋 root 匯入。儀表板也重新讀取 root 日誌，排除服務端的政策變更副本，並標示 `root`／`runtime` 來源；同 UID 程式仍可偽造執行期診斷。

## 容器引擎介面暴露後的恢復

同一台主機上的其他程式（常見於開發者啟動的容器）可能暴露 root 擁有的引擎控制介面，即使 `duduclaw` 不在 Docker 群組也可能發生。警告與憑證保護狀態會列出路徑及原因（可寫入、連線成功或非預期 errno），或 abstract socket 名稱及 peer uid。最新撤銷原因、詳細資訊及 UTC 時間保存在 `<home>/state/root-key-status.json`；`<home>` 通常是 `/var/lib/duduclaw/.duduclaw`。記錄不含金鑰。請依顯示的介面找出並停止暴露它的程式；建議把開發容器移到其他主機。若命中舊版 containerd shim，請升級 containerd。

自動恢復預設開啟，但必須先驗證已安裝的 root 擁有 unit 與目前範本完全相符，包含啟動上限及退出碼 75 的重啟指令，且 root 擁有的重新載入成功標記必須與 unit 內容完全相同。安裝與升級會在變更 unit 前使標記失效，僅在 `daemon-reload` 成功，且由 root 執行 `systemctl show` 確認實際生效的啟動間隔、次數上限與強制重啟退出碼（包含 drop-in 覆寫）符合要求後，才原子寫入標記；驗證失敗則保持無標記並關閉自動恢復；缺少、無法讀取或不符時不會自動重啟，狀態會提示由管理者執行 `service upgrade --protected-linux`。升級會在內容不同時原子更新 unit／啟動器並執行 `daemon-reload`，既有安裝也必須完成此步驟。自動恢復適用於 `engine_socket_reachable` 及 `engine_socket_probe_failed`（探測失敗原本共用前者的代碼）。兩種政策遇到不完整探測都會拒絕或撤銷金鑰，後續檢查通過才可恢復。撤銷後每 60 秒繼續執行相同檢查，連續通過三次才會要求一次正常關機；中途失敗會重新計數。Gateway 完成正常關機流程後以退出碼 75 結束，由 systemd 重啟 root 啟動器。啟動器與新 gateway 會重新執行全部金鑰交接檢查，通過才取得金鑰。遭拒絕的程序不會自行恢復金鑰，仍持有金鑰的程序也不會觸發恢復重啟。執行檔擁有者或權限、附加群組、cgroup／unit、user namespace、容器環境及可存取根金鑰路徑等問題，須由管理員修復，不會觸發自動恢復。

每小時的權威限制由 root 管理的 systemd unit 執行：`StartLimitIntervalSec=3600`、`StartLimitBurst=4`，每個 systemd 計數區間總共最多啟動四次，恢復重啟、一般失敗與人工啟動共用額度。首次啟動也計一次，因此第一個區間最多再啟動三次；後續區間最多四次，不另留一般失敗額度。到達 `start-limit-hit` 後，systemd 會停止自動重啟；單純等待不會安排下一次嘗試。主機管理員修復原因後，須在限制允許時重新啟動服務，或明確重設失敗狀態。詳見 [systemd 啟動限制文件](https://www.freedesktop.org/software/systemd/man/latest/systemd.unit.html#StartLimitIntervalSec=interval)。`<home>/state/root-key-recovery.json` 的滾動小時預算、鎖檔及診斷記錄僅供輔助；同 uid 員工仍能修改或阻止寫入，但不能重設 systemd 計數。狀態檔透過固定目錄描述符開啟，禁止跟隨符號連結，並檢查擁有者、權限與單一硬連結。診斷只接受已知的原因／恢復狀態、有效 RFC3339 時間，以及最多 512 bytes 的跳脫後詳細資訊；CLI、日誌與 RPC 都會將終端控制及不可見格式字元轉成可見跳脫文字。狀態檔讀取或寫入失敗時會停止自動恢復，個人連線維持暫停；請依回報錯誤檢查儲存空間與存取權限。重啟也會短暫中斷其他 gateway 連線。若要維持人工重啟方式，在 `config.toml` 設定：

```toml
[credential_protection]
auto_recover = false
```

新版安裝的 unit 在 `Restart=on-failure` 外另有 `RestartForceExitStatus=75`，並於 `[Unit]` 設定上述兩項啟動限制。`service upgrade --protected-linux` 寫入的 unit 與啟動器範本，來自**執行升級的那個版本**。從 v1.79.3 第一次升級時，執行的是舊版程式，只會替換執行檔，不會取得新 unit；舊版 unit 已可透過 `Restart=on-failure` 重啟退出碼 75，但沒有新的每小時限制，自動恢復也會維持關閉。升級完成後再執行一次下方的 `service install --protected-linux`，才會寫入新版 unit 與啟動器並啟用自動恢復；之後的升級會在內容不同時自動更新。主機管理員可重設 systemd 計數，修改 home JSON 則不能。驗證並安裝新版後，主機管理員可執行 `sudo /opt/duduclaw/bin/duduclaw service install --protected-linux` 更新內嵌啟動器與 unit。這會保留資料與根金鑰、重新載入 systemd，並重啟原本執行中的 gateway；如有本機 unit 覆寫設定，也請一併確認。
