# 原生 Linux 受保護服務

員工個別隔離架構（預計在之後的版本提供）將 gateway、AI 員工、文件解析與特權更新分開。每位經確認的員工有永久的 Linux 身分；每次執行有獨立的程序、檔案系統與網路命名空間。必要隔離元件不可用時，員工執行會被拒絕。

**開發狀態：** 已實作簽章驗證後的未啟用世代暫存、安裝器遷移轉接、首位管理員設定、唯讀安裝檢查與固定路徑的協調移除。完整新安裝、簽章啟用、遷移平台與 gateway 綜合就緒介面仍未完成；本輪未做 root 實機安裝／移除驗收。服務顯示 `active` 或世代已暫存，都不代表隔離就緒。詳見[實作與驗收紀錄](../../architecture/native-linux-isolation.md)。

**目前能安裝的內容：** `service install --protected-linux` 不會安裝 spawnd、egress 或獨立更新服務，也沒有任何流程會建立遷移計畫。在隔離架構的新安裝流程完成前（預計在之後的版本提供），受保護服務就是下方「單一執行檔受保護服務（目前版本）」一節說明的 v1.80 單一執行檔 gateway 服務，本指南描述的員工個別隔離不適用。

## 全新主機的隔離安裝（v1.83.0 起）

從 v1.83.0 起，全新主機的受保護安裝只能透過已獨立驗證的 `duduclaw-bootstrap install` 進行。在從未安裝過 DuDuClaw 的主機上執行 `sudo duduclaw service install --protected-linux`，只會印出下方步驟，不會變更任何東西。所有受保護安裝都不再接受 `--migrate-from`。已經在執行 v1.80–v1.82 單一執行檔受保護服務（或留有它的檔案）的主機，這個流程會拒絕，要改走升級流程。隔離安裝需要團隊版。

安裝器會在變更主機前檢查以下儲存需求：

- 安裝 `e2fsprogs`；`/usr/sbin/mkfs.ext4` 解析後必須是由 root 管理的一般執行檔，上層目錄也必須可信任。
- 核心 loop 模組必須可用（`modprobe loop`）；`/dev/loop-control` 必須是字元裝置 10:237。
- `/var/lib` 所在檔案系統的可用空間至少要有 20 GiB 或總容量的 10%（取較大值，無條件進位），並且通過原有的「安裝包大小兩倍再加 1 GiB」空間檢查。底層含 device-mapper 時，也需要 `dmsetup`（`dmsetup`／`lvm2` 套件）。

1. 以 root 建立下載目錄：`install -d -m 0700 -o root -g root /root/duduclaw-migrate`。
2. 把以下檔案存成檔案放進該目錄：`duduclaw-bootstrap`、`duduclaw-isolated-linux-<架構>-<版本>.tar` 與它的 `.minisig`、`.authorization.json`、`.authorization.json.signatures.json`，以及團隊版授權檔 `license.json`（root 擁有，群組與其他人不可寫）。
3. 用 `sha256sum -c` 與官網及授權 console 公布的值比對，兩處必須一致。
4. 執行 `sudo /root/duduclaw-migrate/duduclaw-bootstrap install [--dns-upstream <IP:53>] [--host-public-address <IP>]...`。安裝程式會自己啟動 Gateway，並設為開機自動啟動。

| 選項 | 說明 |
| --- | --- |
| `--dns-upstream <IP:53>` | 只用在沒有 systemd-resolved 的主機：專用 DNS 轉送服務的唯一上游。有 systemd-resolved 時會被拒絕；本機迴路位址（例如 `127.0.0.1` 上的本機 DNS）與 IPv4 鏈路本地位址（`169.254.0.0/16`，例如雲端虛擬機的中繼資料服務或 DNS 位址）也會被拒絕：DNS 轉送服務的網路規則只限制主機、不限制連接埠，轉送服務因此也連得到中繼資料服務。如果雲端虛擬機唯一的 DNS 解析器是鏈路本地位址，請安裝並啟用 systemd-resolved（安裝程式會直接使用它，不需要 `--dns-upstream`），或指定一個位址可路由的 DNS 解析器。安裝程式會把這個位址寫進 DNS 轉送服務的網路規則；它不會加進員工的對外連線規則。要更換上游，請重新執行安裝程式；不要修改 DNS 轉送服務的單元檔（就緒檢查會拒絕修改過的檔案），也不要加入覆寫設定（隔離執行服務的網路規則檢查會拒絕）。 |
| `--host-public-address <IP>` | 不在網路介面上的本機公開位址（雲端 1:1 NAT）；可重複，最多 64 個。員工連不到這些位址。 |
| `--enable-now` | 已不需要，也沒有作用：安裝程式一定會讓 Gateway 保持執行。仍然接受這個選項，既有的指令稿不必修改；有加時安裝程式會顯示一行提醒。 |

安裝程式在變更任何東西之前先檢查主機，並一次列出所有未通過的項目：Ubuntu 22.04、Ubuntu 24.04 或 Debian 12；systemd 249 以上；核心 5.15 以上；cgroup v2；`/var/lib` 為 ext4 或 XFS；`/etc/login.defs` 的系統 ID 上限有效（原廠檔案將 `SYS_UID_MAX`／`SYS_GID_MAX` 註解掉；未設定時依 shadow 的規則，採一般帳號／群組下限減一，下限預設為 1000）；70000–99999 沒有任何帳號、群組或 subordinate ID 範圍；sshd 主設定包含 `/etc/ssh/sshd_config.d/*.conf`；`cron.allow`／`at.allow` 沒有允許 DuDuClaw 帳號；磁碟空間足夠；有效的團隊版授權；沒有先前的 DuDuClaw 安裝。使用外部帳號服務（LDAP、SSSD 等）的主機，這個安裝程式不支援。

接著依序：確認服務帳號沒有任何程序；發布已驗證的更新信任根、世代與固定路徑程式；建立服務帳號、目錄與服務單元（包含 Gateway 單元，但還不設為開機啟動）；依簽章政策寫入對外連線與 DNS 設定。然後依照開機時相同的順序啟動服務：各個 socket 與 spawnd、一致性檢查（spawnd 已開放）、egress、Gateway，最後是更新服務。安裝程式會等到更新服務回報已安裝的版本（最多 2 分鐘），以及 spawnd 回報所有就緒檢查都通過（最多 2 分半）；spawnd 若未在時限內就緒，訊息會列出還沒通過的檢查。之後啟用每晚更新 timer（`duduclaw-auto-upgrade.timer`），跑綜合就緒檢查，通過後才把 Gateway 設為開機自動啟動。只有完整的就緒檢查在 Gateway 執行中通過，安裝才會回報成功。

**續接：** 進度記錄在 `/var/lib/duduclaw-protected/isolation-bootstrap.json`。安裝中斷時（錯誤、重開機、斷電），修正畫面上的原因後再執行同一個 `duduclaw-bootstrap install` 命令，會從第一個未完成的步驟繼續。續接時可以省略選項；給了不同的值會被拒絕。換成另一個安裝包會被拒絕，要先回退。安裝未完成前，`service install`、`upgrade`、`uninstall --protected-linux` 都會被拒絕。

**回退：** 在最後一步（提交）之前，執行 `sudo /root/duduclaw-migrate/duduclaw-bootstrap rollback`（下載目錄裡仍要有同一份安裝包），主機會回到安裝前的狀態：先停止並停用服務，再移除已安裝的程式與服務單元、更新服務的信任根、狀態與世代、spawnd 的狀態、`egress.json`、`dns-broker.json`、根金鑰，以及這次安裝產生的 Gateway 資料。保留：系統帳號、空的 Gateway 資料目錄與授權檔、受保護模式標記，以及防止安裝較舊安裝包的紀錄。刪除時不跟隨連結、不跨掛載點；若仍有殘留，會列出路徑，可以再執行同一個命令。提交後不能回退，請改用 `service uninstall --protected-linux`。

## 使用者會遇到的變化

以下變化要等隔離架構提供後才適用，目前的安裝不適用。

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

主機需有可信任的系統工具，包括 `getent`、`id`、`useradd`、util-linux、systemd 工具（含 `systemd-tmpfiles`）及 curl。員工 loop 儲存空間需要 ext4 或 XFS、受支援且非 thin provisioning 的底層裝置鏈，並保留員工配額與 TCB 所需空間。未知儲存配置或容量不足會使就緒檢查失敗。`/etc/nsswitch.conf` 的 passwd 與 group 兩行只能列出 `files` 與 `systemd`（Ubuntu、Debian 的預設值，以及使用 authselect `minimal` 設定檔的 RHEL／Fedora）。列出其他模組的主機（LDAP／SSSD 的 `sss`、Google Compute Engine OS Login 的 `oslogin`／`cache_oslogin`、`compat`、`[SUCCESS=merge]` 這類方括號動作子句）不受支援，就緒檢查會失敗：這類模組可能在員工 UID 範圍內加入帳號，而這個版本無法在外部目錄中保留該範圍。

## 單一執行檔受保護服務（目前版本）

在隔離元件可以安裝之前（預計在之後的版本提供），`service install --protected-linux` 安裝的是 v1.80 的單一執行檔受保護 gateway：root 擁有的程式 `/opt/duduclaw/bin/duduclaw`、啟動器 `/usr/local/libexec/duduclaw/protected-launcher`、服務單元 `duduclaw-gateway.service` 及版本下限 `/var/lib/duduclaw-protected/version-floor`。在主機安裝隔離架構之前（預計在之後的版本提供），AI 員工與 v1.80 相同，以 gateway 的服務帳號執行，並套用 v1.80 的憑證保護；員工也與 v1.80 相同，可直接從儀表板建立。本指南描述的隔離執行方式（spawnd 啟動、個人專屬員工、員工配置提案）只會在確實安裝了隔離架構的主機上啟用。

**新主機。** 沒有版本下限紀錄時，只有在能確認主機從未安裝過 DuDuClaw 的情況下，安裝才會繼續：沒有 `duduclaw` 帳號，而且下列路徑都不存在：`/var/lib/duduclaw`、`/var/lib/duduclaw-protected/root.key`、`/var/lib/duduclaw-protected/version-floor`、`/var/lib/duduclaw-protected/version-pending`、`/opt/duduclaw/bin/duduclaw`、啟動器、`/etc/systemd/system/duduclaw-gateway.service`、`/etc/duduclaw/migration-import.json`、`/var/lib/duduclaw-migrate`、`/var/lib/duduclaw-updated/state.json`、`/opt/duduclaw-isolated/current`。只要其中任何一項存在或無法確認，缺少的版本下限就可能屬於 v1.79.3 安裝，安裝器會顯示遷移訊息並拒絕。不要為了通過檢查刪除這些路徑，其中可能有資料與金鑰。

使用 `--migrate-from` 時，安裝器會在建立帳號或其他任何變更之前先檢查來源目錄；讀取期間有變動的來源會被拒絕，請先停止使用者模式的 gateway。建立帳號前，安裝器會寫入只有 root 可存取的標記 `/var/lib/duduclaw-protected/fresh-install-pending`。之後若安裝失敗，只要標記完好，且版本下限、根金鑰、`/etc/duduclaw/migration-import.json`、`/var/lib/duduclaw-migrate`、`/var/lib/duduclaw-updated/state.json` 與 `/opt/duduclaw-isolated/current` 仍不存在，重新執行同一指令即可接續安裝。若前一次已複製資料，請加上 `--force-migrate`（前一份複本會保留為備份）。記錄版本下限後或執行 `uninstall` 時會移除標記。

這個標記是安裝器記錄「此主機已開始全新安裝」的檔案，必須是 root 擁有、權限 0600 的一般檔案，內容與安裝器寫入的完全相同。標記存在但未通過這項檢查時，安裝器不會把主機視為新主機，並會拒絕安裝。root 可以用 `sudo ls -l /var/lib/duduclaw-protected/fresh-install-pending` 查看；只有在主機沒有版本下限、也沒有根金鑰，而且確定要全新安裝時，才以 root 移除（`sudo rm /var/lib/duduclaw-protected/fresh-install-pending`）。移除後，安裝器仍會套用上述新主機條件。

```sh
# 從 root 擁有的位置執行已驗證的程式（見「安裝與就緒檢查」）。
sudo ./duduclaw service install --protected-linux --enable-now
```

**v1.80.x 主機。** 版本下限紀錄為 1.80.0 以上的主機，繼續使用 v1.80 的升級方式：

```sh
sudo /opt/duduclaw/bin/duduclaw service upgrade --protected-linux [--version X.Y.Z]
```

未指定 `--version` 時使用最新版本。指令會先檢查設定的 AI CLI（見下方），再下載官方發行版、驗證檢查碼、簽章與內嵌版本，以原子方式替換程式；服務單元與啟動器內容不同時一併更新，重新確認自動復原設定，記錄新的版本下限，並重新啟動執行中的 gateway。目標版本必須比執行指令的程式新，而且執行指令的程式不能比已記錄的版本下限舊（否則會寫入較舊的服務單元與啟動器），請使用已安裝的 `/opt/duduclaw/bin/duduclaw`。若要改用已驗證過的較新程式，請改執行該程式的 `sudo ./duduclaw service install --protected-linux`；只有在版本下限仍存在時，重新安裝才會保留資料與根金鑰。`service uninstall --protected-linux` 會移除版本下限，但保留資料、根金鑰與帳號，因此之後的安裝會被視為版本下限不明的主機而拒絕。

**儀表板。** 沒有獨立更新服務時，儀表板無法更新受保護安裝。畫面會說明這點並顯示上面的指令，不會送出任何要求。

當安裝改由獨立更新服務管理後（`/var/lib/duduclaw-updated/state.json` 與 `/opt/duduclaw-isolated/current` 都存在），`service upgrade --protected-linux` 必須指定 `--version`，而且只會向該服務送出要求；即使無法連線，也不會自行替換程式。

## 安裝與就緒檢查

由管理員在自己的終端機執行，不能在 AI 工作階段內操作。取得 x86_64 或 aarch64 的完整發行包，放在 root 擁有、上層目錄不可由其他帳號寫入的暫存區，完成驗證後才以 root 執行。Linux 封存檔含分開的元件執行檔；只複製 `duduclaw` 無法安裝這個架構。

完整安裝需要以下身分與資料：

| 項目 | 必要狀態 |
| --- | --- |
| 系統身分 | `duduclaw`、`duduclaw-runsup`、`duduclaw-egress`、`duduclaw-dl`、`duduclaw-dns`；靜態 ID 小於 `SYS_UID_MAX`，避開 DynamicUser 與員工範圍 |
| 員工身分 | 永久登記，預設 70000–99999；只在員工確認後配置 |
| 可信程式 | Gateway、spawnd、trampoline、egress／forwarder／DNS broker、更新服務／凍結啟動器、解析器、匯入器、遷移命令與 MCP stdio 橋接；由 root 擁有 |
| 服務 | Gateway、spawnd socket／service、egress socket／service、選用的 DNS broker、更新服務、parse socket／template、TCB 與員工 slice |
| 自我檢測帳號 | `duduclaw-selftest`：隔離自我檢測降權後使用的帳號；與其他服務帳號相同的系統 ID，無家目錄、不能登入、自有群組、無額外群組、已鎖定 |
| 自我檢測單元 | `/etc/systemd/system/duduclaw-isolation-selftest.service`，root:root 0644，內容與隨附版本完全相同且不得有覆寫設定；沒有 `[Install]`：安裝程式不會啟用或啟動它，由 spawnd 啟動 |
| SSH 拒絕檢測程式 | `/usr/lib/duduclaw/duduclaw-isolation-probe`，root:root 0755，已簽章元件；以無特權的隔離測試帳號 `duduclaw-selftest` 執行，並具有讀取 journal 的權限（`systemd-journal` 群組），絕不以 root 執行；沒有專用帳號 |
| SSH 拒絕檢測單元 | `/etc/systemd/system/duduclaw-isolation-probe@.service` 範本，root:root 0644，內容與隨附版本完全相同；沒有 `[Install]`：安裝程式不會啟用或啟動它，每個員工帳號與 sshd 連接埠一個執行個體（`duduclaw-isolation-probe@duduclaw-emp-<uid>.<port>.service`） |
| 解析根目錄 | `/usr/lib/duduclaw/parse-root` 與 `/usr/lib/duduclaw/parse-root-tree`，root:root 0755，不能放主機資料。`parse-root` 初始為空；`parse-root-tree` 只含已驗證的解析元件、其執行期函式庫，以及解析服務單元需要的空掛載點目錄（`.tmp`、`dev`、`proc`、`run`、`sys`、`tmp`、`var`、`var/tmp`），全部由安裝程式建立 |
| 簽章狀態 | Bootstrap 信任、相容的元件世代、runtime 登錄表、更新槽與狀態、遷移及就緒檢查接線 |

解析器可能留下[安裝參考](../../../crates/duduclaw-parse-worker/README.md)列出的已知空掛載點目錄。遇到其他內容應先調查，不要遞迴清空解析根目錄。Spawnd 負責員工的 SSH、cron、at 與 user manager 封鎖，並驗證主機上的有效狀態；不要手動放寬這些控制來通過檢查。

若主機安裝了 `/usr/sbin/sshd`，spawnd 會自行證明每個員工帳號的 SSH 都被拒絕，不需要任何安裝收據：它先自行檢查該帳號實際生效的 sshd 設定，再透過 systemd 為該帳號與 sshd 連接埠啟動 SSH 拒絕檢測單元。檢測程式以無特權的隔離測試帳號 `duduclaw-selftest` 執行（具有讀取 journal 的權限），以該員工帳號對執行中的 sshd 實際嘗試一次公鑰登入與一次連接埠轉送，兩者都必須被 `DenyUsers duduclaw-emp-*` 規則拒絕，並由 sshd 自己的 journal 紀錄確認。只有當 sshd 自己記錄它接受了檢測程式的金鑰時，才會認定該帳號可以登入；sshd 沒有記錄的成功（例如 SSH 連接埠上是其他程式在回應）只算無法判定。這份證明只保存在記憶體中；當 sshd 重新啟動、該帳號實際生效的 sshd 設定、帳號本身、已安裝的檢測程式或其單元、簽章版本或開機有任何變化，以及每次 spawnd 重新啟動之後，都會重新證明，而且無論如何至少每 6 小時重新證明一次（spawnd 無法透過 systemd 識別執行中的 sshd 時為每 10 分鐘）；更新會在舊證明到期之前開始，正常的主機不會出現空檔。在某位員工的證明完成之前，只有這位員工暫時無法執行（not ready），其他員工不受影響。spawnd 建立新員工時不會等待它的證明：第一次證明進行期間，spawnd 回報「等待中」，gateway 會在最多兩分鐘內重送同一個配置要求，期間不會撤銷任何東西。若證明仍未完成，該項目維持「正在套用」，遷移確認頁顯示「帳號安全驗證尚未完成，請稍後重試」；在 spawnd 持續執行且十分鐘內重試該項目，即可接續同一個帳號（超過十分鐘或 spawnd 重新啟動後，未完成的帳號會被停用，套用會像任何已停用的帳號一樣失敗，必須重新提出建立）。同時最多只能有四位新員工處於等待中；其中一位有結果之前，更多的建立會被拒絕。若證明顯示該帳號可以登入，該帳號會被永久停用。

有 sshd 的主機還需要 `/usr/bin/ssh`、`/usr/bin/ssh-keygen` 與 `/usr/bin/journalctl`、`duduclaw-selftest` 帳號（由安裝程式建立）與 `systemd-journal` 群組，sshd 必須在 127.0.0.1（其設定中的第一個 `Port`）上監聽，並把日誌寫入 systemd journal。不支援設定為 `UsePAM no` 的 sshd：此時 sshd 會在套用 `DenyUsers` 之前就以帳號已過期為由拒絕員工帳號，檢測無法確認拒絕原因，員工永遠不會就緒。沒有 sshd 的主機會略過這項檢測。

安裝器先驗證固定路徑的完整元件，再準備帳號、目錄與 unit。Linux 發行包已加入凍結的更新啟動器；離線工具可將已簽署的資料與元件組成 bootstrap inbox。經獨立驗證的 `duduclaw-updated --bootstrap-stage` 會檢查編譯時固定的根指紋、根金鑰門檻簽章、target 簽章與全部元件雜湊，再發布**尚未啟用的世代**；不選擇 `current`、不建立 trust／state／slots、不驗證舊版過渡雙簽，也不啟動服務。尚無可直接照做的完整新安裝步驟。

可執行 `sudo duduclaw service readiness --protected-linux`（`ops` 為相同入口的別名），唯讀檢查執行檔、系統帳號、目錄權限、unit 內容與 systemd 實際屬性，並分項回報。每個未通過的群組都會在下方列出原因。I1 的綜合就緒介面尚未接上，`MissingComposite` 一律拒絕，不能因此放行員工執行。自訂 DNS drop-in 尚未納入驗證，目前會使 unit 檢查失敗。已由新版管理的安裝再次執行安裝時，只跑唯讀檢查，不改寫世代執行檔或服務單元。

這個命令回報兩個分開的結果。**平台就緒**（四個安裝群組與九項主機檢查）是 `sudo duduclaw service readiness --protected-linux --platform-only` 唯一檢查的內容；全部通過時最後一行為「平台檢查均已通過。」，不會宣稱可以執行員工工作。不加 `--platform-only` 時，命令還會讀取 Gateway 自己的就緒狀態：執行中的 Gateway 會在內容變更時、且至少每 30 秒，改寫 `/run/duduclaw-gateway-update/isolation-readiness.json`（只含固定的元件、原因與啟動元件名稱）。Gateway 的狀態不會改變任何平台檢查的結果。平台檢查全部通過後有三種結果：

| Gateway 狀態 | 最後一行 | 結束代碼 |
|---|---|---|
| 本次開機、不超過 90 秒、已就緒 | 「所有就緒檢查均已通過，可以執行員工工作。」（或資料遷移待確認的句子） | 0 |
| 新的狀態、尚未就緒 | `[資訊]` 列出每個未就緒元件 `名稱(原因)` 與啟動組裝尚缺的項目，接著「平台檢查均已通過，但 Gateway 尚未就緒，員工工作仍暫停。」 | 非 0 |
| 不存在、過期、屬於其他開機或無效（Gateway 未執行或剛啟動） | 「[資訊] 無法取得 Gateway 的就緒狀態（Gateway 未執行、剛啟動，或狀態檔無效）。」，接著拒絕確認可以執行員工工作 | 非 0 |

安裝程式自己的就緒步驟只使用平台結果，因為安裝剛完成時，Gateway 的隔離工具服務可能仍在背景組裝。Gateway 的日誌會列出相同的尚缺項目，已知原因時附上固定原因，例如 `ConnectorProviders(catalog_file_unsafe)`（連接器目錄 `isolation-connectors.db` 或其 SQLite 附屬檔不是由 Gateway 帳號擁有、僅限擁有者存取的單一連結檔案）或 `ExecutionPlans(invalid_helper_runtime)`（已簽署的輔助執行環境不符合其固定協定；政策中完全沒有輔助執行環境是有效的，這些工具類別只是不提供）。Gateway 會在背景重試組裝，修正原因後不需要重新啟動。

## 從 v1.79.3 遷移

不要原地替換舊執行檔，也不要把舊資料庫複製進新 home。舊環境與員工共用 UID，其權威資料必須視為不可信。安裝器已將 v1.79.3 跨入 v1.80（含無法確認原版本下限、且無法確認為新主機的情況，見上方單一執行檔一節）的要求交給 G 遷移流程，固定讀取 root 擁有的 `/var/lib/duduclaw-migrate/plan.json`；計畫缺少或無法驗證時，在停止舊服務前拒絕，不退回原地更新。

管理員流程如下：

1. 從官網及登入後的授權 console，取得 bootstrap 驗證程式、遷移包的 SHA-256 與 root metadata 指紋，兩處必須一致。將下載內容存為檔案，放在 root 擁有的 0700 目錄；執行驗證程式前，先用系統 `sha256sum -c` 比對。舊封存檔簽章不足以建立新信任根，不要把下載內容用管線交給 shell。
2. 經驗證的安裝器檢查 bootstrap 簽章、準備簽章平台，並產生不可由其他帳號修改的 root 遷移計畫。不要自行修改計畫或製造成功讀回資料。
3. 遷移程式會在**停止舊服務前**驗證平台，再停用舊啟動來源、隔離舊資料、產生驗證過的加密快照、輪替金鑰，啟動乾淨且只接受匯入的 gateway。低權限匯入器將已知內容轉成有界紀錄；權威資料只成為未生效提案。
4. 隔離驗證通過後，依安裝器提供的設定流程建立新的管理員。舊 session、密碼、MFA 及 CLI 登入狀態不會恢復；審核高風險項目前先註冊 MFA。
5. 開啟「**系統 → 遷移確認**」，檢查完整值、核准或拒絕提案、重試套用失敗項目，完成要求的憑證輪替。員工 ID 與儲存空間只在確認後配置；在有 SSH 的主機上，每位員工的第一次配置需要約一分鐘驗證帳號的 SSH 拒絕；若頁面顯示驗證尚未完成，請重試該項目。個人帳號由新的擁有者重新連結。
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

`service install --protected-linux` 與 `service upgrade --protected-linux` 結尾會檢查設定中的主要推論 runtime，以及各員工 `agent.toml` 選用的 runtime；有遷移時讀取遷移後的目的地設定。檢查以服務帳號及服務 PATH 確認執行權限，並排除被隱藏的路徑。只安裝在原使用者家目錄的 CLI 會明確標示為服務無法使用。主要 runtime 缺少時會印出警告，並以非零狀態結束。只有刻意稍後才設定 runtime 時，才在上述任一指令加上 `--allow-missing-runtime`；此旗標不會代為安裝或登入。安裝結尾的檢查失敗不會撤回已執行的安裝。`service upgrade --protected-linux` 會先執行此檢查，再下載發行版或送出更新要求；檢查失敗時不做任何變更。

遷移只複製 `.duduclaw` 資料，**不會搬移 AI CLI 或其登入憑證**。Claude 已安裝但服務帳號尚未登入時，安裝流程會顯示上面的服務帳號登入指令。請另行登入，不要複製原使用者家目錄的憑證。Gateway 啟動時若找不到設定的推論執行檔，會記錄 ERROR 與修復方式；runtime 狀態與 doctor 也會顯示受保護服務專用的指引。

## 隔離員工的模型連線資料

使用 OpenAI 相容 runtime 的隔離員工，只從自己家目錄中的 `/home/employee/.duduclaw-api/providers.json` 讀取端點與 API 金鑰；Gateway 的帳號、設定與環境變數都不會被使用。主機管理員以 root 執行下列指令寫入或移除這份資料：

```sh
sudo duduclaw service provider set --protected-linux --employee <員工名稱> < providers.json
sudo duduclaw service provider set --protected-linux --employee-id <32 個小寫十六進位字元> < providers.json
sudo duduclaw service provider set --protected-linux --employee <員工名稱> --clear
```

文件**只從標準輸入讀取**：不接受命令列參數、環境變數或檔案路徑，直接在終端機執行而沒有導入檔案時會被拒絕並顯示格式提示；上限 65,536 位元組。格式如下：

```json
{"providers":[{"name":"main","api_key":"<金鑰>","base_url":"https://provider.example/v1"}]}
```

- 可有 1 至 32 個名稱不重複的提供者；`base_url` 必須是 https，且不可含帳號密碼、查詢字串或片段；可選填 `protocol`（`chat_completions`、`responses`、`anthropic` 或 `gemini`）。格式不正確時只會顯示固定訊息，不會回顯內容。
- 金鑰存放在**該員工自己的家目錄**（員工擁有、權限 0600），該員工的 runtime 可以讀取。請使用專屬於這位員工的金鑰，不要放入 Gateway 或共用帳號的金鑰。
- 在此指令中，每次變更都需要主機 root。只有在主機上沒有執行中的工作、且該員工未在使用時才會寫入，否則請稍後再試。每次執行都會重新寫入。成功時只顯示員工、提供者數量，以及各提供者名稱與 `https://主機[:連接埠]`，不會顯示金鑰。
- 移除請加 `--clear`（不讀取標準輸入）。員工被停用時資料也會一併移除；員工退役時，資料會隨其儲存空間一起銷毀。

### 從儀表板設定

Gateway 也接受從儀表板送出同一份資料（Gateway 端已完成；儀表板頁面在後續套件提供）：

- 只有目前仍是管理員的帳號，在重新驗證身分（step-up：密碼加雙因素驗證，5 分鐘內有效，並綁定該儀表板登入工作階段；與連線帳號、遷移決策使用同一份驗證）後，才能設定或移除。可對任何已配置的隔離員工操作，包括其他同仁的個人專屬員工，規則與連線帳號相同。每次操作都會留下稽核紀錄：操作者、員工、結果，以及各提供者名稱與端點來源，不含金鑰或端點路徑。
- Gateway 只在記憶體中把金鑰轉交給 root，由 root 以與主機指令相同的方式寫入員工家目錄。金鑰不會寫入 Gateway 的磁碟、資料庫、設定、日誌、稽核或錯誤訊息，轉交後 Gateway 會清除記憶體中的副本。驗證規則與主機指令相同；此外，名稱、金鑰與端點含引號、反斜線或控制字元時會被拒絕。
- 儀表板的狀態每位員工只顯示提供者名稱、協定、端點來源（`https://主機[:連接埠]`）與最後一次經儀表板確認變更的時間。**狀態只反映經由儀表板做的變更**：用上方主機指令寫入或移除的資料不會顯示在這裡。其他同仁的個人專屬員工預設隱藏，管理員需重新驗證身分才能顯示（會留下稽核紀錄）。
- 主機上有執行中的工作或該員工正在使用時，變更會以「忙碌」拒絕，請待員工閒置後再試。隔離服務沒有確認變更時，結果會顯示為「未確認」，狀態維持不變；再次送出同一變更是安全的。

### 員工無法執行時

隔離執行失敗時，Gateway 日誌會有一筆警告 `isolated run refused`，欄位為 `runtime`、`agent`（員工代號）、`reason`、`class`、`exit`（結束碼，或 `signal`、`timeout`、`incomplete`）與 `stderr_len`：

```sh
sudo journalctl -u duduclaw-gateway.service --no-pager | grep 'isolated run refused'
```

OpenAI 相容執行環境的 `reason` 是執行程式的固定原因（清單見 `crates/duduclaw-api-worker/README.md` 的「Diagnostics」）。其他執行環境，以及任何不是單一已知原因的輸出，都記為 `reason="unrecognised"`。執行環境本身的輸出不會寫入日誌，也不會顯示給任何人。在聊天頻道中，可看到管理診斷資訊的人會看到下列其中一句固定訊息；其他人看到一般的失敗訊息，網站公開訪客只會看到訪客訊息：

| 顯示的訊息 | `class` | 要檢查的項目 |
| --- | --- | --- |
| 這位 AI 員工尚未設定模型連線資料，或設定內容無法使用；請管理員到『受保護模式 → 模型連線資料』檢查。 | `employee_credentials` | 這位員工的模型連線資料是否存在、格式正確，且包含所要求的提供者（見上方）。 |
| 連不到這位 AI 員工設定的模型服務；請管理員檢查端點位址與對外連線。 | `provider_transport` | `base_url` 的主機、TLS，以及對外連線政策是否允許該主機。 |
| 模型服務拒絕了這次請求；請管理員檢查這位 AI 員工的金鑰與模型名稱。 | `provider_refused` | 金鑰、模型名稱、提供者端的額度。 |
| 模型服務的回覆無法使用；請管理員檢查端點是否為相容的服務。 | `provider_response` | `base_url` 與 `protocol`（`chat_completions`、`responses`、`anthropic`、`gemini`）是否符合提供者的 API。 |
| 這次工作超過時間上限而停止。 | `timeout` | 這次工作的時間上限，或提供者回應過慢。 |
| 受保護模式尚無這次工作的隔離執行授權，或所選執行設定尚未通過驗證；請由管理員檢查服務狀態。 | 其他類別或 `unrecognised` | 就緒狀態與日誌中的 `reason`、`exit`；見「就緒檢查故障排除」。 |

如果這次工作**在執行開始之前**就被拒絕，日誌改為記一筆警告 `isolated run not started`，欄位為 `runtime`、`agent`（員工代號）與 `site`；`site` 是固定代號，指出在哪一步被拒絕。如果執行已經開始，卻沒有留下上面那一行就結束（例如管線不存在，或正常結束但結果無法使用），則記一筆 `isolated run refused after start`，欄位相同。每次失敗的受保護工作只會有這三種之中的一行。日誌不含請求內容、路徑，也不含員工代號以外的識別資訊。對使用者顯示的訊息不變。

```sh
sudo journalctl -u duduclaw-gateway.service --no-pager | grep -E 'isolated run (not started|refused)'
```

| `site` 群組 | 代號 | 要檢查的項目 |
| --- | --- | --- |
| 前置檢查（尚未使用任何授權） | `preflight.model`、`preflight.runtime_policy`、`preflight.model_policy`、`preflight.redaction`、`preflight.interceptor`；`authorize.agent_dir_missing`、`authorize.effort`、`authorize.utility_call`、`authorize.work_dir_override`、`authorize.agent_dir`、`authorize.capabilities_policy`、`authorize.native_gate`、`authorize.runtime_kind`、`authorize.explore_lane`、`authorize.tool_restrictions`、`authorize.codex_home`、`authorize.product_policy`、`authorize.interceptor`、`authorize.runtime_id`、`authorize.max_turns`；`step_limit.agent_dir`、`step_limit.unavailable` | 員工的 `agent.toml`（`[model] effort`、`[capabilities]`）、執行環境與模型的產品政策、遮罩與 CCR 設定。 |
| 授權（一次性的執行授權） | `authorization.ingress.*`：提出要求的人——`no_scope`、`consumed`、`capture.*`（訊息進來時的登入檢查：`no_web_credential`、`web_identity`、`channel_conflict`、`channel_mapping`、`requester`、`user_db`、`user_inactive`、`epoch`、`blocking`）、`agent_path`、`home_mismatch`、`no_turn`、`no_session`、`no_attended_proof`、`attended_proof_invalid`、`nested_dispatch`、`principal_mismatch`、`proof_expired`、`identity_recheck`、`blocking`；`authorization.issue.*`：核發授權——`boot_owner`、`product_authority`、`lifecycle`、`snapshot`、`epoch`、`principal`、`binding`、`policy_revision`、`tools`、`human`、`selectors`、`capability`、`users_recheck`、`policy_changed`、`lifecycle_recheck`、`snapshot_recheck`、`facts_recheck`；以及 `authorization.consumed`、`authorization.installed_mismatch`、`authorization.agent_label`、`authorization.home`、`authorization.principal_context`、`authorization.background_issue`、`authorization.ingress_issue` | 這個人的帳號仍有效、不需要更改密碼，且可以指派工作給這位員工；登入仍有效；員工的歸屬（共用組織或個人擁有者）與配置狀態。 |
| 執行計畫 | `request.encode`、`plan.readiness`、`plan.boot_owner`、`plan.home_mismatch`、`plan.employee_unconfirmed`、`plan.root_publication`、`plan.blocking`、`plan.unavailable` | 就緒狀態；員工已確認；root 發布的設定有提供這個執行環境給這位員工。 |
| 啟動準備 | `launch.readiness`、`launch.boot_owner`、`launch.boundary_unhealthy`、`launch.employee_unconfirmed`、`launch.employee_mismatch`、`launch.product_prepare`、`launch.binding`、`launch.blocking`、`launch.resolver`、`launch.connectors`、`launch.worker_plans`、`launch.factories`、`launch.dispatcher`、`launch.protected_launch`、`launch.authority_pairing`；`launch.bind.<step>`（執行已預約之後；`<step>` 與下方 `isolated launch preparation refused` 相同） | 這位員工的受保護工具邊界（工具投影、連接器）；`launch.bind.*` 見下方的表。 |
| 要求開始執行 | `spawn.prepare`、`spawn.not_protected`、`spawn.admission`；`spawn.backend.unavailable`、`.denied`、`.would_deadlock`、`.not_found`、`.timeout`、`.io`、`.protocol`；`spawn.start.permission_denied`、`.not_found`、`.timed_out`、`.other` | `spawn.backend.*` 是隔離服務（或其控制通道）拒絕時的固定類別。隔離服務拒絕開始執行時不會寫日誌，所以 `duduclaw-spawnd` 日誌沒有紀錄，並不能排除這一組。 |
| 開始之後（`isolated run refused after start`） | `run.stdin_missing`、`run.stdout_missing`、`run.stderr_missing`、`run.input`、`run.output_drain`、`run.stderr_drain`、`run.wait`、`run.refused`；`output.parse`、`output.empty`、`output.utf8` | 執行環境的結果；已有 `isolated run refused` 那一行時，不會再多記。 |
| `unlabelled` | — | 沒有代號的拒絕：這是缺陷，請回報。 |

`spawn.backend.*` 看不出是閘道還是隔離服務拒絕了開始執行。若是閘道自己拒絕（在送出開始要求之前，或沒有送出），它會在 `isolated run not started` 那一行之前，另外記一筆警告 `isolated spawn refused locally`，只有一個固定欄位 `step`（不含識別資訊、路徑或內容）。如果 `spawn.backend.*` 那一行之前沒有這筆警告，就是隔離服務拒絕的（它不會為此寫日誌，見上表「要求開始執行」一列）。

```sh
sudo journalctl -u duduclaw-gateway.service --no-pager | grep -E 'isolated (spawn refused locally|run not started)'
```

| 時機 | `step` | 代表什麼 |
| --- | --- | --- |
| 預約執行 | `admission`、`tracked_runs`、`reservation_reply`、`reservation_queue`、`endpoint_directory`、`endpoint_socket`、`endpoint_connect`、`peer_identity` | 就緒狀態改變；閘道的執行表已滿；預約回覆無法使用（到期時間必須在閘道時鐘的 30 秒內）；這位員工在此閘道已有 32 個未結束的執行；控制通道的目錄、socket 或對端檢查失敗。 |
| 預約之後、開始之前 | `launch_prepare`、`policy_binding`、`task_binding`、`pty_size` | 受保護的啟動環境拒絕（閘道為這次執行重新檢查此人的授權，或工具邊界發出這次執行的能力憑證：就緒狀態、員工的歸屬、執行環境的 MCP 註冊）；準備好的計畫與預約的執行不符；預約結束後才綁定任務；終端機大小無效。 |
| 組成開始要求 | `admission`、`workspace_delivery`、`workspace`、`entry`、`reservation_state`、`resource_limits`、`inputs`、`attachments`、`request_invalid`、`connection_state`、`live_check`、`fd_plan` | 就緒狀態改變；工作區證明不符；計畫指定的不是員工自己的工作區；執行不存在、屬於其他員工、已經開始，或 30 秒的預約已過期；執行環境紀錄的輸出或輸入額度為零；請求超過輸入額度；附件太多；要求無效（例如步數上限不在 1–1000）；控制連線已關閉；即時授權檢查拒絕；描述子配置不符。 |
| 送出之後 | `admission_after_send`、`start_reply` | 開始要求送出期間就緒狀態改變（這次執行會被取消）；非預期的回覆。 |
| 開始之後、提供工具之前 | `launch_activate` | 受保護的啟動環境拒絕了這次執行的 MCP 端點（見下方 `isolated launch activation refused`）。已開始的執行會被取消，能力憑證也會撤銷。 |
| 其他控制要求 | `cancel_state`、`live_status_reservation`、`work_op`、`helper_op` | 取消已放棄或已被拒絕；狀態回覆指向另一個預約；該路徑不允許的操作。 |

`step` 為 `launch_prepare` 時，閘道會在它之前再記一筆警告 `isolated launch preparation refused`，只有一個固定欄位 `step`，指出是哪一項檢查拒絕（不含識別資訊、路徑或內容）。這一輪的 `isolated run not started` 那一行則會顯示 `site="launch.bind.<step>"`，而不是 `spawn.backend.denied`。

```sh
sudo journalctl -u duduclaw-gateway.service --no-pager | grep -E 'isolated (launch (preparation|activation) refused|spawn refused locally|run not started)'
```

| `step` | 代表什麼 |
| --- | --- |
| `recheck.expired` | 執行授權已過期。授權在要求受保護執行時核發，有效 30 分鐘，且不會超過網頁登入本身的到期時間；請重新登入。系統時鐘跳動也會出現在這裡。 |
| `recheck.agent` | 這位員工已不在閘道的即時員工清單中。 |
| `recheck.identity` | 授權之後，員工的生命週期（配置、確認、root 讀回）或此人的帳號資料（組織、存取世代、政策世代、登入世代）有變動，例如仍在套用中的員工核准或歸屬變更、登出所有裝置、第二道驗證變更。 |
| `recheck.policy_revision` | 某個政策來源存在但無法讀取或解析、員工的 `agent.toml` 沒有 `[agent] status = "active"`，或缺少 `config.toml`。來源：員工實際生效的 `agent.toml`、`CONTRACT.toml`（可省略）、`preset_bindings.toml`、`org.toml`、`config.toml`、自訂遮蔽設定檔、`KILLSWITCH.toml`。 |
| `recheck.revision_changed` | 授權之後某個政策來源有變動：`agent.toml` 的 `[capabilities]`、`[permissions]`、`[agent]` 的 `reports_to`／`department`／`role`；`CONTRACT.toml`；預設組合的綁定；`org.toml` 的上層鏈；`config.toml` 的 `[delegation]`、`[acp]`、`[provenance]`、`[integrations]`、`[redaction]`；自訂遮蔽設定檔；`KILLSWITCH.toml`。對話紀錄、記憶、`.claude/`、`.mcp.json`、`SOUL.md` 與技能都不是來源。 |
| `recheck.epoch`、`epoch_after_snapshot` | 檢查期間（下方狀態查詢之前或之後）此人的帳號資料有變動。 |
| `recheck.background` | 僅限排程或委派的工作：它的背景紀錄已不再授權這次執行。 |
| `live_snapshot` | 隔離服務對這個已預約執行的狀態回覆失敗，或 250 毫秒內沒有回覆（之後這次執行的控制連線會關閉）。請檢查 `duduclaw-spawnd` 日誌與主機負載。 |
| `binding_mismatch` | 隔離服務回報這個執行屬於其他員工，或已不在預約狀態。 |
| `revoked_before`、`revoked_after` | 準備期間這次執行被取消（例如這一輪被停止）。 |
| `verify_blocking` | 閘道用來做授權檢查的阻塞工作池忙碌超過 2 秒，或檢查超過 10 秒（`users.db`／`apply.db` 或 root 讀回太慢）。 |
| `prepared_missing`、`state_lock`、`associations` | 同一個啟動被準備兩次、其狀態已損壞，或閘道的已綁定執行表（4096）已滿或已有這個執行：這是缺陷，請回報。 |

閘道自己的檢查通過後，工具邊界會登記這次執行（`inner_prepare.<substep>`），各步驟如下：

| `step` | 要檢查什麼 |
| --- | --- |
| `inner_prepare.revoked`、`inner_prepare.revoked_after` | 工具邊界準備期間這次執行被取消。 |
| `inner_prepare.reservation.<class>`（`unavailable`、`denied`、`would_deadlock`、`not_found`、`timeout`、`io`、`protocol`） | 閘道對這個預約的紀錄：`denied` 表示 30 秒的預約已過期、控制連線已關閉，或已經開始；`not_found` 表示此閘道不認得這次執行。 |
| `inner_prepare.plan_class`、`inner_prepare.plan_runtime`、`inner_prepare.plan_pty` | 為這一輪準備的計畫，其執行類別、執行環境或終端機模式與實際啟動不符：這是缺陷，請回報。 |
| `inner_prepare.registry.publication_read`、`inner_prepare.registry.generation` | 閘道無法讀取 root 的執行環境公告 `/run/duduclaw-spawnd/publication.json`（擁有者、群組、權限、不同開機，或超過 1 秒的有效期：請確認 `duduclaw-spawnd` 正在執行且未卡住），或公告指向另一個已安裝世代（更新後請重新啟動閘道）。 |
| `inner_prepare.registry.runtime_missing`、`.runtime_duplicate`、`.mcp_disabled`、`.script_executable` | 公告中沒有（或有兩筆）這個執行環境的紀錄、紀錄為 `mcp: false`，或可執行檔是腳本：已安裝的簽章政策沒有提供帶工具的這個執行環境。 |
| `inner_prepare.registry.acceptance_missing` | 已安裝的套件沒有這個執行環境的 MCP 驗收紀錄：請安裝以 `--accept-mcp` 建置的套件。 |
| `inner_prepare.registry.acceptance.<check>`（`runtime`、`version`、`runtime_digest`、`bridge_digest`、`projection_digest`、`inherited_fd`、`initialize_exit`、`parent_exec_exit`、`mcp_child_exec_exit`、`exclusive_config_exit`、`transcript_digest`）、`inner_prepare.registry.projection_invalid`、`inner_prepare.registration_runtime`、`inner_prepare.projection` | 已安裝套件的 MCP 驗收紀錄與公告的執行環境不符（驗收時用的是另一個執行環境二進位檔、版本或橋接程式，或另一個版本的政策工具）。請重新安裝相符的套件；對該套件執行 `duduclaw-isolation-policy verify` 會指出同一項檢查。 |
| `inner_prepare.issue.live_snapshot` | 核發能力憑證時的即時授權重新檢查失敗（原因同上方的 `recheck.*` 與 `live_snapshot`）。 |
| `inner_prepare.issue.not_reserved`、`.reservation`、`.expiry`、`.time` | 隔離服務已不再把這個預約回報為預約中，或預約已過期（系統時鐘跳動後也會出現）。 |
| `inner_prepare.issue.principal`、`.binding`、`.policy_revision`、`.agent_slug`、`.binding_principal` | 這一輪擷取的授權與即時授權不同（此人、員工的歸屬或政策在期間有變動），或此人不能代表這位員工的歸屬行事。 |
| `inner_prepare.issue.grants` | 這一輪的資源授權（工作區、連接器、網站來源）對這位員工不被接受。 |
| `inner_prepare.issue.encode.<rule>`（`resource_mode`、`interval`、`duplicate_grant`、`other`） | 這次執行的能力憑證無法編碼：`interval`——這一輪的授權在能力憑證核發前已過期（或系統時鐘跳動）；其他則是缺陷（請回報）。 |
| `inner_prepare.issue.random`、`.signer`、`.state_lock`、`.capacity`、`.duplicate`、`inner_prepare.employee_mismatch`、`.prepared_missing`、`.state_lock`、`.pending_duplicate`、`.argv`、`.clock`、`.registry.unreported` | 作業系統亂數來源或系統時鐘失敗，或是缺陷（請回報）。 |
| `inner_prepare.verify_blocking` | 同上方 `verify_blocking`，發生在工具邊界的阻塞檢查。 |

執行開始後，工具邊界會啟用它的 MCP 端點。若在這裡被拒，會記一筆 `isolated launch activation refused`（只有一個固定的 `step`：`inner_activate.<substep>`），接著是 `isolated spawn refused locally step="launch_activate"`；這一輪的 site 為 `launch.activate.<step>`。已開始的執行會被取消，能力憑證也會撤銷。

| `step` | 要檢查什麼 |
| --- | --- |
| `inner_activate.mcp_fd_missing`、`inner_activate.mcp_receipt.<class>` | 隔離服務沒有交出這次執行的 MCP 端點（執行提早結束，或缺少 I/O 交接）。請檢查 `duduclaw-spawnd` 日誌。 |
| `inner_activate.bridge.registry.*` | 同上方 `inner_prepare.registry.*`，在開始後重新讀取。 |
| `inner_activate.bridge.publish.*`（上方 `registry` 的名稱）、`.publish.policy`、`.publish.derive`、`.publish.record`、`.publish.registration_changed` | 準備與啟用之間公告有變動（更新或授權變更），或已不再對這位員工提供這個執行環境。 |
| `inner_activate.bridge.publish.run_receipt_read` | root 的單次執行 MCP 收據 `/run/duduclaw-spawnd/mcp-<run>.json` 不存在、來自其他開機，或超過 60 秒。請檢查 `duduclaw-spawnd` 日誌。 |
| `inner_activate.bridge.publish.receipt.<group>`（`generation`、`run`、`employee`、`runtime`、`digests`、`projection_digest`、`mount`、`acceptance`）、`inner_activate.bridge.receipt_binding`、`.bridge.registration_runtime`、`.bridge.projection`、`inner_activate.projection_digest` | root 對已掛載工具設定的讀回與為這次執行準備的不符。請重新安裝相符的套件；若持續發生請回報。 |
| `inner_activate.bridge.platform` | 不是 Linux。 |
| `inner_activate.boundary_activate.live_mismatch`、`.live_snapshot`、`.time` | 端點送達時執行已不在進行中、此人的授權已改變、即時檢查失敗，或能力憑證已過期。 |
| `inner_activate.delivery`、`.receipt_control`、`.boundary_activate.delivery`、`.boundary_activate.unknown`、`.boundary_activate.binding`、`.boundary_activate.revoked`、`inner_activate.pending_missing`、`.revoked_after`、`.active_duplicate` | 端點不屬於這次執行的預約，或期間執行被取消；否則是缺陷。 |
| `inner_activate.boundary_activate.random`、`.boundary_activate.state_lock`、`inner_activate.state_lock`、`.clock`、`.verify_blocking` | 作業系統亂數來源、系統時鐘或阻塞檢查工作池失敗，或是缺陷。 |

在受保護模式下，若被拒絕的工作無法從文字判斷原因，結尾的原因代碼會是 `safety_gate`，不再是 `unknown`。

## 升級與解除安裝

獨立更新服務目前還無法安裝（預計在之後的版本提供）；現有主機請依上方單一執行檔一節升級。完成遷移及更新服務佈署後，Admin 可在儀表板要求正式版更新，或使用已安裝的 CLI：

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
| UID／帳號／SSH／IAM 失敗 | 處理保留範圍衝突、別名、附加群組、登入／cron／at 或 linger。passwd 或 group 使用 `files`／`systemd` 以外的 NSS 模組（例如 SSSD 或 GCE OS Login）不受支援。在有 sshd 的主機上某位員工一直未就緒時：查看 journal 中的 `SSH denial probe duduclaw-emp-<uid>: …` 與 `journalctl -u 'duduclaw-isolation-probe@*'`；不要移除 `DenyUsers` 的設定片段。 |
| 儲存／配額失敗 | 確認底層裝置鏈、硬性配額、可用容量與主機保留空間；不能用磁碟用量輪詢代替硬性配額。 |
| DNS／egress 失敗 | 檢查 root 指定的 resolved Varlink 或 DNS broker 路徑；不要替 egress 服務加入私有 resolver IP 例外。DNS 就緒（`host_dns_route_unverified`）是 spawnd 自己的即時檢查，不讀安裝收據：spawnd 約每分鐘請 egress 經由該路徑解析簽章政策中的 `dns_probe_name`，至少要得到一個公開位址才算通過。簽章政策版本低於 v4、egress 沒有執行，或這台主機解析不到該名稱時，都維持未驗證（請檢查對外 DNS 與 egress 日誌中的 `dns_probe` 紀錄）。 |
| 解析根目錄失敗 | 檢查兩個根目錄的擁有者、權限與非預期內容；不要複製主機 `/etc` 或資料進去。 |
| 遷移平台或設定連結不可用 | 保留既有環境，取得完整驗證過的整合；不要搬移舊權威或繞過遷移閘。 |
| 等待確認／輪替 | 在遷移確認頁完成支援的確認、重試與新憑證設定；核准本身不代表遷移完成。 |
| 更新已受理但未 committed | 追蹤持久要求狀態與就緒錯誤，不要同時手動替換執行檔。 |

### spawnd 日誌中的帳號驗證紀錄

當 `registry_verified`、`accounts_locked`、`ssh_live_negative_checks`、`cron_at_acl_and_jobs_verified` 與 `external_iam_reservation_verified` 同時失敗時，spawnd 的 journal 會指出失敗的步驟。每行只印一次，步驟或帳號改變時才會再印：

```text
duduclaw-spawnd: employee account verification failed: step=<label> account=<duduclaw-emp-NNNNN|->
duduclaw-spawnd: allocation failed: step=<label>
```

第一行來自全主機的帳號檢查，會檢查員工範圍內每個 `duduclaw-emp-*` 帳號，包含已退役（tombstone）的帳號。`account=-` 表示主機層級的步驟。第二行在每次員工配置失敗時印一次，但同一個 label 在 10 秒內最多印 5 次；同一 label 之後的失敗只計數，並在這 10 秒結束時以一行 `duduclaw-spawnd: allocation failed: step=<label> suppressed=<count>` 回報。SSH 探測尚未完成或待處理保留數已達上限都不算失敗，不會記錄。label 是固定識別字，兩行都不含檔案內容或錯誤文字。就緒輸出與 `readiness.json` 的檢查名稱不變。

| label | 失敗的項目 |
| --- | --- |
| `login_defs`、`subids` | `/etc/login.defs`、`/etc/subuid` 或 `/etc/subgid` 無法讀取或格式錯誤。 |
| `registry_rows` | 員工登錄表與主機 uid 範圍不符，或有無法續行的保留。 |
| `nss_modules` | passwd 或 group 使用 `files`／`systemd` 以外的 NSS 模組。 |
| `enumerate` | `getent passwd`／`group` 無法讀取，或在員工範圍內有別名或外來項目。 |
| `passwd_row`、`shadow_row`、`groups` | 帳號的 passwd 列（`/nonexistent`、`/usr/sbin/nologin`）、shadow 列（`!*`、已過期）或群組清單（只能有自己的 gid）不正確。 |
| `cron_acl`、`at_acl` | `/etc/cron.deny` 或 `/etc/at.deny` 沒有拒絕該帳號，或被 allow 檔覆蓋。 |
| `cron_spool`、`cron_tables` | 該帳號有使用者 crontab，或 `/etc/crontab`、`/etc/cron.d` 中有以它執行的行（該處有無法讀取或非 root 擁有的檔案也會失敗）。 |
| `at_spool` | at spool 目錄（`/var/spool/cron/atjobs`、`/var/spool/cron/atspool`、`/var/spool/at`、`/var/spool/at/spool`）中有該帳號擁有的項目、目錄存在但無法讀取，或列出的項目在檢查擁有者前消失或無法檢查。atd 開始執行工作時會把它從 `atjobs` 移除，所以項目消失不會被視為無害。下一次更新會重新檢查，像其他帳號的工作剛結束這類短暫變化會因此解除。 |
| `linger`、`sessions` | 有 linger 檔，或該帳號的 `user@<uid>.service` 或 logind session 仍在執行。 |
| `ssh_config`、`ssh_probe` | `sshd -t`／`sshd -T` 沒有拒絕該帳號或沒有 port，或 SSH 拒絕探測沒有 Denied 結果。 |
| `userdb_service`、`userdb_user`、`userdb_group` | `/run/systemd/userdb` 無法讀取，或 userdb 的使用者或群組紀錄觸及員工命名空間。 |
| `local_identity`、`candidate`、`owner_inventory` | 配置時：新帳號已存在身分或別名、候選 uid 查詢失敗，或 spawnd 的檔案擁有者盤點失敗。 |
| `login_barriers`、`create_group`、`create_user`、`lock_user`、`quarantine` | 配置或復原時：deny 項目或 sshd 設定片段、groupadd、useradd（或它建立的郵件 spool）、usermod／chage／loginctl 鎖定，或停止該帳號的程序失敗。 |
| `reserve`、`create_account`、`readback` | 某個配置步驟失敗，且沒有更細的 label。例如 `reserve` 表示員工 uid 範圍已用完。 |
| `existing_row`、`persist_reservation`、`persist_final`、`store_poisoned`、`clock` | 配置時：該員工已有已退役或無法續行的登錄列；建立帳號前或決定結果後寫入登錄表失敗；本程序先前寫入失敗後，登錄儲存拒絕再寫入；或無法讀取單調時鐘。 |
| `unlabelled` | 來自 spawnd 已標記檢查以外的錯誤。配置不會產生此 label。 |

`at` 套件可以保留：spawnd 以 root 直接讀取它的 spool，從不執行 `/usr/bin/at`。不要為了消除紀錄而手動修改 deny 檔、spool 或帳號。修正原因後，下一次更新會印出 `evidence complete`。配置失敗會永久退役該 uid，提案維持「失敗」。修正原因後，請在遷移確認頁重試該失敗項目的補償，之後新的提案會取得下一個 uid。

### spawnd 日誌中的啟動拒絕紀錄

當閘道記錄 `isolated run not started`（例如 `site="spawn.backend.unavailable"`）時，spawnd 的 journal 會為這次被拒絕的啟動記一行：

```text
duduclaw-spawnd: start refused: step=<label>
```

這行不含員工 ID、執行 ID、路徑或請求內容。傳給閘道的錯誤碼不變。准入關卡的 label 可能來自 `ReserveRun`、`StartRun` 或排隊中執行的准入。

這行依 label 限制頻率：10 秒內同一個 label 只印前 5 次拒絕。之後同一 label 的拒絕只計數，並在這 10 秒結束時改印一行計數：

```text
duduclaw-spawnd: start refused: step=<label> suppressed=<count>
```

每次拒絕都會印出，或計入某個 `suppressed=` 計數。印出的行數加上這些計數，就等於被拒絕的啟動次數。

| label | 拒絕啟動的原因 |
| --- | --- |
| `frame`、`peer`、`request`、`fds`、`run_id` | 控制 socket：無法解碼的訊框、非閘道的連線端、未通過自身驗證的請求、與請求不符的描述元，或無法產生隨機執行 ID。 |
| `clock`、`update_drain`、`stopping`、`not_reserved`、`reservation`、`pty`、`queue_full`、`launch_policy`、`input_bytes`、`input_fds`、`internal` | 准入：時鐘倒退、更新排空已關閉、正在停止該員工的執行、沒有或錯誤／過期的保留、PTY 不符、佇列已滿、啟動政策無效，或輸入額度不足。 |
| `employee_not_admitted` | 該員工不是 spawnd 登錄表中的 Ready 列。 |
| `not_ready(<check>)`、`report_stale`、`report_invalid`、`pty_budget`、`landlock_evidence`、`protocol`、`boot_clock`、`policy_missing`、`poisoned` | 主機就緒：指名的 readiness.json 檢查為 false、證據報告超過 1 秒或屬於其他開機、沒有 PTY 額度或 Landlock 證據、未載入已驗證的政策，或 spawnd 需要重新啟動。 |
| `scan_stale`、`scan_failed`、`scan_busy`、`intruder`、`view_busy` | UID 監視器的掃描過舊、失敗或被鎖住，發現該員工在執行外的程序，或准入檢視被鎖住。 |
| `account_unknown`、`ssh_pending`、`account(<step>)` | 該員工沒有登錄列、SSH 拒絕探測尚無結果，或帳號驗證在 `<step>` 失敗（上表的 label）。 |
| `entitlement_missing`、`runtime_not_entitled`、`runtime_ambiguous`、`pty_not_allowed`、`turn_limit` | 授權：該員工沒有授權、所要求的 runtime 沒有或有兩個已授權設定檔、不允許 PTY，或步數上限超出簽署範圍。 |
| `queue_timeout(<blocker>)`、`queue_cancelled(<blocker>)` | 排隊中的執行在 `<blocker>` 擋住時逾時或被閘道取消：准入 label、`host_runs`、`employee_busy`、`host_ptys`、`cleanup` 或 `cleanup_incomplete`。 |
| `uid_monitor_register`、`worker_mailbox`、`run_fence`、`cleanup`、`cleanup_incomplete`、`storage_usage`、`disk_quota`、`run_journal`、`storage_attach`、`storage_receipt`、`workspace` | 啟動（尚未建立 unit）：監視器登記、生命週期信箱、執行已取消、清理讀回、儲存用量或配額、執行日誌、映像掛載或其收據，或工作區。 |
| `mcp_channel`、`egress_channel`、`egress_register`、`output_relay`、`stdin_pipe`、`supervisor_identity`、`nonce`、`bootstrap_plan`、`uid_monitor`、`mcp_projection`、`anchors`、`mounts`、`mcp_record`、`native_profile`、`helper_mounts`、`seal`、`trampoline_prepare`、`bootstrap_invalid`、`unit_policy` | 啟動（尚未建立 unit）：通道或管線、egress 服務的登記（未知的設定檔或沒有 egress 控制連線）、bootstrap 計畫、MCP 投影、發布錨點、掛載、原生設定檔、trampoline 或 unit 政策。 |
| `unit_start`、`unit_cgroup`、`bind_unit`、`launch_poll`、`startup_timeout` | systemd 拒絕 unit、unit 的 cgroup 或 trampoline 綁定失敗、在 Running 前啟動失敗，或未在啟動額度內到達 Running。 |
| `host_gate`、`host_policy`、`host_start` | 僅測試用轉接器。 |

在此版本之前，配置日誌中已是 Ready 的員工出現 `employee_not_admitted` 是一個缺陷：spawnd 只在啟動時讀取可准入的員工，因此 spawnd 啟動後才配置的員工會一直被拒絕，直到 spawnd 重新啟動。現在 spawnd 會先發布可准入的員工，再回覆配置、授權、停用或退役請求。在配置或授權的回覆之後送出的 `ReserveRun`，會以更新後的員工與授權檢查；停用或退役的回覆之後，該員工會被拒絕。

受保護員工環境不提供容器引擎與依賴主機硬體的功能。所有必要元件均需通過檢查才能恢復工作；套件測試通過或本指南都不能當作正式環境驗收。

## v1.80.x 已發行的單一執行檔受保護服務

以下各節說明 v1.80.x 發行版的受保護服務：gateway 是 root 安裝的單一執行檔，`service upgrade --protected-linux` 會在程序內驗證並替換執行檔。這些內容仍是上述發行版，以及隔離架構沿用的憑證保護控制（引擎 socket 政策與自動復原）的參考。目前的版本對單一執行檔安裝仍使用這種程序內升級（見上方「單一執行檔受保護服務」）；只有由獨立更新服務管理的安裝會改送更新要求，而且 CLI 不會自行替換執行檔。

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

啟動腳本確認執行檔、所有上層目錄及自身安裝位置屬於 root，且其他使用者不能寫入。確認帳號與清除同 uid 殘留程序後才開啟金鑰，接著降 uid/gid、清空附加群組與所有 capabilities，開啟 `no_new_privs`。Gateway 接收金鑰前先設為 non-dumpable，再檢查 systemd 父程序、精確的 cgroup v2 記錄（只接受 `0::/system.slice/duduclaw-gateway.service`，或服務單元設 `Slice=duduclaw-tcb.slice` 時的 `0::/duduclaw.slice/duduclaw-tcb.slice/duduclaw-gateway.service`，隔離架構即是後者）、執行檔權限、程序權限，以及自身無法開啟根金鑰。原生安裝程式、開啟金鑰前的啟動腳本，以及 Rust 驗證都會拒絕非初始 user namespace。若存在 `/run/.containerenv`、`/run/systemd/container`，或 PID 1 環境含 `container=`，也會拒絕。root 安裝程式與啟動腳本必須能讀取 `/proc/1/environ`；一般權限的 gateway 只有在權限不足時可略過該讀取。偵測到容器時仍使用容器規則，不能靠模式變數取得原生保護。

預設 `revoke` 政策下，已知 Docker／Podman／containerd socket 路徑必須不存在或不可存取：寫入權限探測與非阻塞連線都必須被拒。`DOCKER_HOST` 必須未設定。自訂 slice、cgroup v1／混合階層、非預期的 sudo 輸出、無法確認的探測一律拒絕。啟動拒絕原因可在 journal 或憑證保護卡片查看；不要放寬權限繞過檢查。

服務另外套用以下設定：

```ini
InaccessiblePaths=-/run/docker.sock -/var/run/docker.sock -/run/containerd -/run/podman -/var/run/podman -/run/user -/run/docker
PrivateDevices=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
LockPersonality=yes
SystemCallArchitectures=native
```

依 [systemd.exec](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html) 規則，每個 `-` 前綴會忽略不存在的路徑。這些設定隱藏常見引擎端點、隔離硬體裝置、禁止修改核心／模組／cgroup，並限制執行模式及 syscall 架構。刻意不設定 `RestrictSUIDSGID`：在 systemd 255 上它會讓所有 `openat2` 呼叫失敗，而 gateway 以 `openat2` 的路徑解析限制開啟檔案。啟動器不建立 set-ID 檔案，gateway 以 `no_new_privs` 執行。未設定 `RestrictNamespaces`，因為 gateway 的 Codex／bwrap 工作沙箱需要 user、mount、PID 與 network namespace。

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
