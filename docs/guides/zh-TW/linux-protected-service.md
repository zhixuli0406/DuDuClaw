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

完整、已驗證的安裝可使用既有 runner 介面：

```sh
sudo /usr/lib/duduclaw/current/duduclaw-migrate run --plan /var/lib/duduclaw-migrate/plan.json
sudo /usr/lib/duduclaw/current/duduclaw-migrate status --plan /var/lib/duduclaw-migrate/plan.json
```

這是介面範例，**不是已完成端到端驗收的部署步驟**。固定計畫及簽章平台須由經驗證的安裝器提供，目前仍無完整可用的安裝產出；缺少 `duduclaw-migration-platform` 時，runner 會在停機前拒絕。首位管理員已接上驗證 root 對端的 Unix socket 與 loopback 設定頁，token 放在網址 fragment，帳號與新密碼以同一交易建立；遠端管理員須先建立該本機連接埠的 SSH 通道。root 擁有的 `/etc/duduclaw/migration-import.json` 持久保存遷移模式，避免重啟後 `/run` 資料消失便退回一般管理員初始化。這些接線不代表完整遷移成功。不要把 `prepare --stop-after-keys` 當作升級捷徑，它會刻意讓安裝停留在離線狀態。

確認頁需要有效的 Admin session、密碼及已註冊驗證器的新驗證碼；授權有效五分鐘，綁定目前 session。高風險項目須逐項核准，批次核准只限低風險項目。核准與套用成功是不同狀態；不支援的類別繼續封鎖，需由支援的流程重新輸入。舊待核准項目不會變成新授權。

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
