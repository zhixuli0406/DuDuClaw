# 原生 Linux 的個人連線受保護服務

> 下方安裝與使用者資料搬移章節描述既有 v1.79.3 服務；開發中的更新流程另行實作且尚未完成。變更既有安裝前，請先讀[升級與解除安裝](#升級與解除安裝)。

官方 Linux CLI 執行檔需要 **glibc ≥ 2.31**：Ubuntu 20.04+、Debian 11+、RHEL／Rocky Linux 9+。較舊系統請使用 Docker。此基準適用於 CLI／gateway（含桌面版的 CLI sidecar）；桌面圖形介面另需符合 WebKitGTK 等系統函式庫需求。

這個模式讓原生 Linux 的「我的連線」可使用個人 Google 與遠端 MCP OAuth。Gateway 以專用的 `duduclaw` 帳號執行，啟動時接收 root 準備的金鑰。一般使用者服務不會收到這把金鑰。另見[個人 Google 連線](google-personal-connections.md)。

## 準備主機

需要以 systemd 為 PID 1、採單一 cgroup v2 階層的 Linux，以及 GNU coreutils、GNU tar、curl、minisign、`getent`、`id`、`useradd`、util-linux `setpriv`。請在真人操作的終端機透過 sudo 安裝，不能在 AI 工作階段或容器內執行。必須使用初始 user namespace：`/proc/self/uid_map` 與 `/proc/self/gid_map` 各自只能有一行 `0 0 4294967295`（空白可不同）。Rootless 容器與重新映射身分的 user namespace 都會被拒絕。

專用帳號的 uid/gid 不得為 0，shell 必須是 `nologin` 或 `false`，不能有任何 sudo 指令權限、不能開啟 linger，也不能屬於 `sudo`、`wheel`、`admin`、`adm`、`docker`、`lxd`、`incus`、`libvirt` 群組；`systemd-journal` 可以保留。不要讓其他服務共用這個帳號：每次啟動交付金鑰前，會終止這個 uid 的所有非殭屍程序。

## 安裝與啟動

請將 `X.Y.Z` 改為已發布的版本，並先確認 `/opt` 及所有上層目錄屬於 root、其他帳號無法寫入。以下 sudo 指令在 root 擁有、權限為 0700 的 `/opt` 暫存目錄下載官方封存檔與 `.minisig`，以發布公鑰驗證封存檔的完整位元組，再解壓執行檔。只有驗證通過的執行檔會在該 root shell 內執行。發布的 Linux 檔名為 `duduclaw-linux-x64.tar.gz` 與 `duduclaw-linux-arm64.tar.gz`，各自附有同名 `.minisig`。

```sh
sudo /usr/bin/env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin /bin/sh -eu <<'ROOT'
version=X.Y.Z
case "$(uname -m)" in
  x86_64) asset=duduclaw-linux-x64.tar.gz ;;
  aarch64|arm64) asset=duduclaw-linux-arm64.tar.gz ;;
  *) echo 'Unsupported architecture' >&2; exit 1 ;;
esac
umask 077
stage=$(mktemp -d /opt/duduclaw-install.XXXXXXXX)
trap 'rm -f -- "$stage/$asset" "$stage/$asset.minisig" "$stage/duduclaw"; rmdir -- "$stage"' EXIT
cd "$stage"
base="https://github.com/zhixuli0406/DuDuClaw/releases/download/v$version"
curl -q --proto '=https' --tlsv1.2 -fL "$base/$asset" -o "$asset"
curl -q --proto '=https' --tlsv1.2 -fL "$base/$asset.minisig" -o "$asset.minisig"
minisign -Vm "$asset" -x "$asset.minisig" \
  -P 'RWTh5pOpk0YmdBgm3VyB2bzxFtajNLXr7zFDhbcc75TgM8YfeV+NSzXh'
tar -xzf "$asset" --no-same-owner --no-same-permissions duduclaw
chown root:root duduclaw
chmod 0755 duduclaw
"$stage/duduclaw" service install --protected-linux --enable-now
ROOT
sudo systemctl status duduclaw-gateway.service
sudo journalctl -u duduclaw-gateway.service -n 100 --no-pager
```

不要從使用者可寫入的 npm 安裝目錄或家目錄執行 sudo 安裝；AI 員工可能在核心以 root 啟動前替換程式。若解析後的執行檔或任何上層目錄不屬於 root，或允許非 root 寫入，安裝程式會警告並要求 `--i-verified-this-binary`。這個確認參數不能讓已經以 root 執行的不可信程式變安全，請使用上面的驗證與暫存流程。

不加 `--enable-now` 只安裝服務，之後用 `sudo systemctl enable --now duduclaw-gateway.service` 啟動。開啟儀表板完成設定，再到「安全 → 憑證保護」啟用。確認顯示「獨立金鑰（AI 員工讀不到）」後，再讓使用者連結個人帳號。CLI 程序不會繼承 gateway 的金鑰，終端機查到的憑證狀態不能代表 gateway。

root 擁有的程式放在 `/opt/duduclaw/bin/duduclaw`，內嵌的啟動腳本安裝到 `/usr/local/libexec/duduclaw/protected-launcher`，服務檔是 `/etc/systemd/system/duduclaw-gateway.service`。資料位於 `/var/lib/duduclaw/.duduclaw`，根金鑰位於 `/var/lib/duduclaw-protected/root.key`（root:root、0400、一個連結、32 位元組）。服務使用 `KillMode=control-group`，系統檔案唯讀，只開放資料與金鑰目錄寫入。

## 從使用者安裝遷移

先備份舊資料並停止舊 gateway。由原本的使用者服務擁有者執行：

```sh
systemctl --user stop duduclaw.service
systemctl --user disable duduclaw.service
sudo /opt/duduclaw/bin/duduclaw service install --protected-linux \
  --migrate-from "$HOME/.duduclaw" --enable-now
```

請先完成上面的驗證安裝；若預計在首次啟動前遷移，安裝時省略 `--enable-now`。若新 gateway 已建立資料，請先保留備份，取代時明確加上 `--force-migrate`。再從原使用者自己的終端機執行遷移，不要使用 root 登入 shell。`SUDO_UID` 與 `SUDO_GID` 必須存在、有效且非零。安裝程式透過 `getpwuid` 查詢該 UID 的家目錄，只接受其中的 `.duduclaw`；指定路徑解析後必須完全相同，來源樹的每個目錄與檔案都必須屬於 `SUDO_UID`。`/var/lib/duduclaw-protected`、`/var/lib/duduclaw`、`/etc`、`/root`、`/proc`、`/sys` 以下的來源一律拒絕。逐層固定檔案描述元的 `openat` 檢查也會拒絕符號連結，包括上層目錄的連結。

安裝程式不會代替另一位使用者執行前兩個指令。它會把該目錄複製到 `/var/lib/duduclaw/.duduclaw`，並交給專用帳號。目的地已存在時拒絕，除非明確加上 `--force-migrate`。`--force-migrate` 會把原目的地保留為 `/var/lib/duduclaw/.duduclaw.previous-<UUID>`。符號連結、硬連結與特殊檔案會被拒絕。取代資料前先停止系統服務、另存備份。檢查舊設定的絕對路徑；`ProtectHome=yes` 會讓原本使用者家目錄的路徑無法使用。

請重新啟用憑證保護，並請每位使用者重新連結個人 Google 與遠端 MCP。舊的個人憑證密文綁定另一把金鑰，新安裝無法解開。AI runtime 執行檔應安裝在 root 管理的系統路徑；舊使用者的 CLI 安裝、登入檔與 shell 啟動檔不會自動提供給新帳號。

## 升級與解除安裝

**開發分支：受保護更新尚未可供部署。** 儀表板與 CLI 現在改向獨立的 root 更新服務提出要求。必須先由隔離遷移套件建立服務、帳號與信任根；只替換 v1.79.3 的執行檔或 timer 不會完成這些準備。

管理員可從儀表板要求更新至較新的正式版本，並依持久保存的要求識別碼追蹤結果。CLI 必須明確指定版本：

```sh
sudo duduclaw service upgrade --protected-linux --version X.Y.Z
```

跨越更新服務 socket 的資料只有版本與要求識別碼，呼叫端不能指定路徑、網址、命令或信任金鑰。服務驗證對端身分、簽章清單、檔案雜湊、架構、期限、版本下限與序號。已接受、下載中與已暫存都不代表安裝成功；回應中斷時儀表板保留識別碼繼續追蹤，只有 `committed` 才顯示成功。服務無法連線時，不會退回舊安裝器。

Linux 更新程式已實作整組版本切換、固定服務重啟與實際版本／協定／執行檔讀回，通過才提高版本下限。中斷後會繼續同一筆交易，就緒不確定時保持工作准入關閉。跨套件的排空、遷移／runtime 與就緒端點仍須串接；缺少端點就拒絕更新。遷移 bootstrap 的雙簽章流程、真實 FIDO2 簽章與 Linux 實機更新驗收尚未完成。請勿啟用這份原始碼的無人值守更新。

既有 **v1.79.3 安裝**應保留經驗證、固定版本的 v1.79.3 程式及其操作指引。該歷史版本會下載並驗證 minisign 簽章封存檔，以原子替換更新程式，再重啟原本執行中的服務；儀表板只提供手動更新。其 root 管理的 `version-floor` 與 `version-pending` 分別限制一般降版及保留中斷目標。這些歷史檔案不是新更新服務的狀態或信任存放區。

既有 v1.79.3 的解除安裝指令仍為：

```sh
sudo /opt/duduclaw/bin/duduclaw service uninstall --protected-linux
```

它會停止並停用舊服務，移除其服務檔、執行檔與啟動器，**不會刪除** `/var/lib/duduclaw` 或 `/var/lib/duduclaw-protected`，包括根金鑰；只移除舊的版本下限及待完成意圖。這個指令不能當作新隔離安裝的清理或回退流程。部署前須由遷移套件提供新安裝的遷移與復原指引。

## 自動升級（僅供開發）

目前的 `scripts/linux-protected/auto-upgrade/` 保留既有 timer 時間：每天當地時間 03:30，最多隨機延遲 20 分鐘，`Persistent=false` 讓早上開機不補跑。helper 改為呼叫固定、由 root 管理的 `/usr/libexec/duduclaw-updated-launch --check-latest`；加上 `--dry-run` 只查候選版本，不送出 Apply。

候選版本查詢由低權限的 `duduclaw-dl` 帳號執行 curl，清除繼承設定，將 API 回應限制在 1 MiB 並完整解析 JSON。最新發行紀錄必須符合 `draft=false`、`prerelease=false` 與正式版本格式。未簽章的索引只決定候選版本，安裝仍須通過 root 更新服務對官方簽章清單的驗證。相同或較舊版本不送出更新；連線、帳號、狀態或更新服務出錯便停止，不退回舊流程。要求端 unit 的逾時為六分鐘，不寫入發行檔案。

請勿把這份開發中的 helper 複製到 v1.79.3 安裝，也不要啟用其 timer。既有 v1.79.3 夜間排程應保留先前審閱過的固定版本 helper，等完整隔離遷移提供後再轉換。不要從會移動的 `main` 取得正式環境使用的 helper。

原始碼安裝器仍檢查 root:root 擁有權、一般檔案型別與上層目錄不可被他人寫入，拒絕符號連結與 sticky `/tmp`，並從 root 私有快照安裝。這些檢查不能證明程式碼來源；日後部署前仍須審閱精確 commit 及 root 副本，不要從使用者可寫入的 checkout 執行 root 安裝器。詳見 [helper README](../../../scripts/linux-protected/auto-upgrade/README.md)。

若要移除已安裝的排程而保留 gateway 與資料，使用先前審閱過的安裝器副本：

```sh
sudo sh /root/duduclaw-auto-upgrade/install.sh --uninstall
```

升級仍在進行時，解除安裝會先停用 timer，但拒絕移除；請等作業結束後重試。

## 保護範圍與限制

**功能限制：**原生受保護模式透過 `InaccessiblePaths` 阻擋容器引擎，並以 `PrivateDevices` 隔離裝置，因此無法使用依賴容器的任務沙箱、電腦操作及依賴硬體的功能。這是刻意的限制：AI 員工不得接觸容器引擎。

啟動腳本確認執行檔、所有上層目錄及自身安裝位置屬於 root，且其他使用者不能寫入。確認帳號與清除同 uid 殘留程序後才開啟金鑰，接著降 uid/gid、清空附加群組與所有 capabilities，開啟 `no_new_privs`。Gateway 接收金鑰前先設為 non-dumpable，再檢查 systemd 父程序、精確的 cgroup v2 記錄 `0::/system.slice/duduclaw-gateway.service`、執行檔權限、程序權限，以及自身無法開啟根金鑰。原生安裝程式、開啟金鑰前的啟動腳本，以及 Rust 驗證都會拒絕非初始 user namespace。若存在 `/run/.containerenv`、`/run/systemd/container`，或 PID 1 環境含 `container=`，也會拒絕。root 安裝程式與啟動腳本必須能讀取 `/proc/1/environ`；一般權限的 gateway 只有在權限不足時可略過該讀取。偵測到容器時仍使用容器規則，不能靠模式變數取得原生保護。

已知 Docker／Podman／containerd socket 路徑必須不存在或不可存取：寫入權限探測與非阻塞連線都必須被拒。`DOCKER_HOST` 必須未設定。自訂 slice、cgroup v1／混合階層、非預期的 sudo 輸出、無法確認的探測一律拒絕。啟動拒絕原因可在 journal 或憑證保護卡片查看；不要放寬權限繞過檢查。

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

Gateway 在啟動時及每 60 秒重新檢查常見引擎 socket 的可達性與附加群組。定期檢查失敗時，會原位清零持有的金鑰、將保護狀態改為拒絕並記錄警告；個人連線在重新啟動前都無法使用。也會檢查 `/proc/net/unix` 的監聽中 abstract socket：名稱以 `/` 分段後，任一段完全等於 `docker`、`containerd`、`podman`、`buildkit`、`lxd`，或以這些名稱接 `.`、`-`、`_` 開頭，就會列為候選；不以任意子字串比對。 名稱命中本身不會造成拒絕：Gateway 會以短逾時的非阻塞連線取得 `SO_PEERCRED`，只有連得上且 peer uid 為 0 才視為風險。無法連線的候選與無關的格式錯誤行會略過。啟動時無法讀取 `/proc/net/unix` 仍拒絕接收金鑰；定期檢查遇到讀取失敗則記錄警告，僅保留這項探測先前的判定，路徑及群組檢查照常執行。

AI 員工的 Bash／工具仍以 gateway 的 uid 執行。保護依賴 non-dumpable、root 擁有的金鑰與程式、沒有 sudo／linger 權限，以及無法存取引擎 socket。員工仍能修改可寫資料、停止 gateway、要求已授權操作，並讀取自己回合取得的存取權杖。主機不得授予 `duduclaw` 帳號存取容器引擎或其他特權代理的權限。這些檢查只涵蓋啟動時與每分鐘檢查的常見端點，並非無條件的隔離保證。未識別名稱的 abstract socket、其他路徑的引擎 socket、特權代理、其他提權途徑、主機 root 遭入侵，以及已在 gateway 內執行的惡意程式不在防護範圍；兩次檢查之間也可能出現新的存取途徑。執行檔擁有者檢查防止服務 uid 修改程式；它不能驗證惡意 root 管理員交來的程式簽章。

根金鑰不提供通用沙箱。持有金鑰時，同樣禁止在 gateway 內載入 ONNX：NER 遮罩、本機 embedding 與 Kokoro 無法使用；需要遮罩時請使用正規表示式規則。同時包含資料與根金鑰的備份可解開憑證。只在儀表板更換保護金鑰不能防止還原舊資料；懷疑根金鑰外洩時，主機管理員應停止服務、安全更換根金鑰、重設保護並重新連結帳號。

原生 systemd 安裝、不同 sudo 版本與 socket 探測仍需在目標發行版實測。單元測試或 macOS 編譯通過不代表原生 Linux 驗收通過。

## 規劃中的 v1.80 遷移界線

舊版受保護安裝器會拒絕從 v1.80 以前直接替換程式升級至 v1.80 以後，包含預覽版。版本下限紀錄缺失時也會拒絕跨越；升級時同時檢查目前執行版本與已儲存版本下限。全新 v1.80 安裝須使用新的安裝流程。目前尚未接上經獨立驗證的遷移與啟動流程，請勿複製舊資料庫到新安裝以繞過拒絕。

管理員可開啟「系統 → 遷移確認」，先在帳號安全頁啟用雙因素驗證，再輸入密碼與新的驗證碼。核准前請查看完整內容；只有低風險提案可批次核准，尚未套用的提案可批次拒絕。重新載入會顯示實際套用讀回；失敗或中斷可用同一提案重試。不支援的類別暫時不能核准，仍待輪替的憑證會讓遷移保持未完成。root 遷移命令與 gateway 啟動整合尚未成為可發行的升級流程，需先完成已驗簽安裝器及 Linux 實機驗收；勿直接套用到既有安裝。詳見[實作說明](../../architecture/native-linux-isolation.md)。

尚未發行的匯入器已加入低權限啟動與專用匯入模式。舊帳號與 MCP 設定只進確認清單，不會自動啟用；確認後的 stdio MCP 與員工指示檔會經產品驗證、發布及讀回。含憑證或尚未支援的項目須重新輸入，或維持拒絕；未確認員工的內容先保留在封存中。完整升級仍需要已簽章的平台整合與 Linux 端到端驗收。

技能、hook 與部分政策也會經產品驗證，並以本次操作的收據讀回；一般編輯已替換的檔案或部門，不會被遷移補償刪改。舊核准一律失效。Telegram 綁定、時間排程與資料庫唯讀授權還須確認 Shared 員工的實際配置；缺少隔離平台整合時維持拒絕。
