# 原生 Linux 的個人連線受保護服務

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

## 為服務帳號安裝 AI CLI

服務以 `duduclaw` 帳號執行，`HOME=/var/lib/duduclaw`、`PATH=/usr/sbin:/usr/bin:/sbin:/bin`。AI CLI 必須安裝在這些系統 PATH 目錄之一，且服務帳號必須有讀取與執行權限。`ProtectHome=yes` 會隱藏 `/home`、`/root` 與 `/run/user`；即使 root 找得到 `/home/operator/.local/bin/claude`，服務也無法使用。在系統目錄放一個指向隱藏家目錄的符號連結也無效。

Claude Code 請先把 `<ver>` 換成要安裝的版本，再執行：

```sh
cd / && sudo -H npm install -g --prefix /usr @anthropic-ai/claude-code@<ver>
sudo -u duduclaw -H claude auth login
```

`-H` 與 `--prefix /usr` 都要保留，否則原使用者的 `~/.npmrc` prefix 設定可能讓 npm 又把 CLI 裝回使用者家目錄。若設定使用 Codex、Grok、AGY 或 Gemini，也要安裝到相同的服務可讀系統 PATH，並依各 CLI 的登入方式替服務帳號登入。

`service install --protected-linux` 與 `service upgrade --protected-linux` 結尾會檢查設定中的主要推論 runtime，以及各員工 `agent.toml` 選用的 runtime；有遷移時讀取遷移後的目的地設定。檢查以服務帳號及服務 PATH 確認執行權限，並排除被隱藏的路徑。只安裝在原使用者家目錄的 CLI 會明確標示為服務無法使用。主要 runtime 缺少時會印出警告，並以非零狀態結束。只有刻意稍後才設定 runtime 時，才在上述任一指令加上 `--allow-missing-runtime`；此旗標不會代為安裝或登入。檢查失敗不會撤回已執行的安裝或升級。

遷移只複製 `.duduclaw` 資料，**不會搬移 AI CLI 或其登入憑證**。Claude 已安裝但服務帳號尚未登入時，安裝流程會顯示上面的服務帳號登入指令。請另行登入，不要複製原使用者家目錄的憑證。Gateway 啟動時若找不到設定的推論執行檔，會記錄 ERROR 與修復方式；runtime 狀態與 doctor 也會顯示受保護服務專用的指引。

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

使用已安裝、由 root 擁有的執行檔升級：

```sh
sudo /opt/duduclaw/bin/duduclaw service upgrade --protected-linux
# Select a published version instead of latest:
sudo /opt/duduclaw/bin/duduclaw service upgrade --protected-linux --version X.Y.Z
```

只有 `/proc/self/exe` 精確指向 `/opt/duduclaw/bin/duduclaw`，且程式與所有上層目錄都屬於 root、禁止非 root 寫入時，才能升級。它從官方 GitHub Release 下載 Linux 封存檔（預設最新版），重用 updater 內嵌的 minisign 公鑰驗證完整下載內容，再解壓至 root 擁有、權限為 0700 的暫存目錄。替換使用同目錄暫存檔、fsync 與 rename；升級期間不會以 root 執行新程式。驗證失敗不會變更已安裝程式；系統服務原本有在執行時才會重新啟動。現有資料與根金鑰保留。儀表板在這個模式顯示手動更新，不會替換程式。

```sh
sudo /opt/duduclaw/bin/duduclaw service uninstall --protected-linux
```

解除安裝會停止並停用服務，移除服務檔、執行檔與啟動腳本，**不會刪除** `/var/lib/duduclaw` 或 `/var/lib/duduclaw-protected`，包括根金鑰；該目錄只移除版本下限及待完成的版本意圖。重新安裝可沿用保留的資料與金鑰。

升級只接受比目前執行版本新的穩定 semver `X.Y.Z`。安裝與升級也拒絕低於 `/var/lib/duduclaw-protected/version-floor` 的版本；此檔案由 root:root 擁有、權限為 0600，在執行檔替換成功後原子寫入最高已安裝版本。檔案不存在時以目前版本為下限，內容損壞則拒絕操作。升級沒有回退選項。緊急回退必須由 root 刻意解除安裝（會移除版本下限），再用舊版封存檔執行上方驗簽後首次安裝流程；資料與根金鑰仍保留。 替換中斷時，root-only 的寫入意圖（`version-pending`）也會保留已驗證目標的下限；請以該版本或較新版重做驗簽安裝，或刻意解除安裝後清除。

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

**功能限制：**原生受保護模式透過 `InaccessiblePaths` 阻擋容器引擎，並以 `PrivateDevices` 隔離裝置，因此無法使用依賴容器的任務沙箱、電腦操作及依賴硬體的功能。這是刻意的限制：AI 員工不得接觸容器引擎。

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
