# app 相容層：在 DuDuClaw OS 上跑 Windows / Android 應用程式

DuDuClaw OS 本身是一套 Linux 基底的作業系統，但很多人手上仍有離不開的 Windows 或 Android 應用程式。**app 相容層**就是為此而生：透過一組可插拔的「相容層元件（runner）」，讓這些應用程式能在 DuDuClaw OS 上安裝、執行。做法參考 SteamOS 讓 Windows 遊戲能在 Linux 上跑的方式（Proton），並把同一套架構延伸到 Windows 桌面應用程式與 Android 應用程式。

**先設定期望**：這不是「什麼應用程式都保證能跑」。每一個相容層元件都誠實標示自己「支援什麼、不支援什麼、目前缺什麼」，而不是含糊地說「應該可以」。

## compat.d 是什麼

`compat.d` 是相容層元件的**登記目錄**。每個元件（例如 Bottles、Waydroid）在這裡放一份宣告檔，描述：

- 這個元件是從哪個系統把應用程式接進來的（Windows 遊戲／Windows 應用程式／Android／「連回你自己的 Mac」）
- 怎麼啟動它
- 需要哪些工具才能運作

宣告檔分兩層存放，**後者可覆蓋前者**：

| 層級 | 位置 | 誰寫入 |
|---|---|---|
| 出貨層 | `/usr/share/duduclaw/compat.d/` | 系統映像出廠內建 |
| 資料層 | `~/.duduclaw/compat.d/`（或你設定的資料目錄） | 你自己或維運人員手動放入的覆蓋版本 |

同一個元件 ID 若兩層都有，以資料層為準——這讓你不用重灌系統就能覆寫、微調一個元件的設定。

第三方也能用同一套規則掛入自己的相容層元件（例如社群維護的 Proton 替代版本），不需要 DuDuClaw 額外支援。

> 這一版（CP-1）只做「登記與健檢」：`duduclaw compat list` 會告訴你有哪些元件、缺什麼工具，但還不會幫你一鍵啟動應用程式本身——這是後續版本的工作。

## 查看目前有哪些相容層元件

```bash
duduclaw compat list
```

輸出範例：

```
相容層 runner（3 個）：

ID             名稱                             來源系統        狀態
------------------------------------------------------------------
bottles        Bottles（Windows 應用轉譯）        windows-app     ready
waydroid       Waydroid（Android 應用容器）       android         missing: waydroid, lxc-start
               → 見 docs/guides/app-compat.md 的 Waydroid 自裝入口段：...
windows-vm     Windows 應用程式（完整 Windows 虛擬機） windows-app     missing: docker, xfreerdp3
               → 尚未設定：先執行 `duduclaw compat windows-vm setup`（會走過資源門檻建議、硬體虛擬化檢查、授權責任揭露，再啟動容器）。
```

`狀態`欄誠實反映現況：

- `ready`：需要的工具都在，元件可用
- `missing: <工具名>`：元件已登記，但還缺某些必要工具（見下方每個元件的說明）
- `malformed`：宣告檔本身寫壞了（通常是維運端手動編輯出錯），不影響其他元件正常顯示

需要機器可讀格式（例如寫腳本判斷）時加 `--json`：

```bash
duduclaw compat list --json
```

## Windows 桌面應用程式：Bottles

Bottles 透過 Wine 相容層執行 Windows 應用程式，走 Flathub 通道安裝：

```bash
flatpak install flathub com.usebottles.bottles
```

安裝完成後，`duduclaw compat list` 會把 `bottles` 的狀態從 `missing: flatpak`（或元件本身未安裝）轉為 `ready`。

也可以直接在桌面圖形啟動器（Launcher）的「可安裝」分類裡找到 Bottles，點「安裝」走同一套確認流程（顯示下載大小與安裝位置），不必打指令列。這個分類的相容性標示是「Partial」（能用但明列缺什麼）：Bottles 本身在 DuDuClaw OS 上實測可跑（QEMU 活測：從 Flathub 裝進系統、GUI 正常開啟、wine 跑起真的 Windows 執行檔 notepad 並畫出視窗），標示之所以不是「已驗證」，是因為這個等級說的是「拿它跑 Windows 軟體」這件事的整體可用範圍——下方明列的不支援清單（新版 Office、LINE、AutoCAD 2018+）依然成立。

**Bottles 適用範圍請照實看待**——它只承諾：

- 舊版本、單機版、綠色版工具
- 在 Wine 相容性資料庫（AppDB）上被評為「Silver」以上等級的軟體（例如舊版 Photoshop CC、舊版 Acrobat）

**明確不承諾能跑（社群實測結果一致判定不可用）**：

- 近三年（2023 年後）的 Microsoft Office
- LINE Windows 版
- AutoCAD 2018 以後版本

這些應用程式即使裝了 Bottles，實測結果也大機率無法正常使用。若你需要這類軟體 100% 相容，正確路線是「真的一台 Windows」——見下一節「Windows 應用程式（完整虛擬機）」，而不是勉強用 Bottles 硬跑。

## Windows 應用程式（完整虛擬機）

Bottles 只轉譯 Windows API，遇到近三年的 Microsoft 365、AutoCAD 2018 以後版本、需要印表機或 USB 憑證載具的記帳／ERP 軟體，一律不承諾能用（見上一節）。這一類軟體需要「真的一台 Windows」，DuDuClaw OS 為此內建另一條路線：在本機建立一台完整的 Windows 虛擬機，透過無縫視窗（看起來就像一般應用程式的視窗，不是整台電腦的畫面）執行單一 Windows 應用程式。

**這不是預裝的功能**：DuDuClaw 不隨機出貨、不代購、不代管任何 Windows 授權金鑰，也不預先下載 Windows 安裝映像。整套流程一鍵引導，但每一步都由你觸發、你確認。

### 硬體需求

| 項目 | 下限 | 建議 |
|---|---|---|
| 本機（值班機）總記憶體 | — | 16GB 以上 |
| 虛擬機常駐記憶體 | 4GB | 8GB |
| 虛擬機磁碟空間 | 32GB | 依應用程式需求（Windows 本身＋安裝的軟體） |

本機記憶體低於建議門檻時，`duduclaw compat windows-vm setup` 只會印出提醒，不會擋下你繼續——這是建議值，不是硬性限制。

### 授權責任揭露

在你執行 `setup` 之前，請先確認以下三點——這不是 DuDuClaw 的條款，是真實的授權現況：

1. **你需要自備 Windows 11 Pro 以上版本的合法授權。** 這台虛擬機安裝的是 Windows，授權責任由你自行承擔，DuDuClaw 不提供、也不附贈授權。
2. **Home 版不支援 RemoteApp 無縫視窗。** 這是上游 WinApps 專案文件明文列出的硬性需求，不是 DuDuClaw 的限制——用 Home 版授權，應用程式仍可能裝得起來，但「無縫視窗」這個體驗跑不出來。
3. **你電腦／筆電隨附的 OEM 授權，通常不含虛擬化使用權利。** 這是 Microsoft 官方授權條款的原文立場——用機器原廠內建的 Windows 授權裝進這台虛擬機，可能違反授權條款。

`setup` 執行時會完整印出這份揭露，並要求你明確確認（互動終端機輸入確認字樣，或帶 `--yes` 代表你已閱讀並同意）才會繼續往下走。

### 使用流程

```bash
# 1. 引導設定：資源門檻建議 → 硬體虛擬化檢查 → 授權揭露確認 → 產生設定 → 啟動容器
duduclaw compat windows-vm setup

# 可調整資源與 Windows 版本（皆有預設值，不帶也可以）：
duduclaw compat windows-vm setup --ram 16 --disk 128 --version 11

# 非互動環境（腳本/CI）：--yes 代表你已閱讀並同意上述授權責任
duduclaw compat windows-vm setup --yes

# 2. 查看容器狀態
duduclaw compat windows-vm status

# 3. 安裝完成後，以無縫視窗啟動一個 Windows 應用程式
duduclaw compat windows-vm app winword.exe --name "Word"
```

`setup` 只會啟動一個空的 Windows 虛擬機容器——**Windows 安裝映像不會預先下載或內建**，容器第一次啟動後才會自行觸發下載，你可以用瀏覽器打開 `http://127.0.0.1:8006` 監看安裝進度（首次安裝需要一段時間，實際長短依網路狀況與你選擇的 Windows 版本而定）。安裝完成前，`compat windows-vm app` 還無法正常連線。

`app` 子命令透過 FreeRDP 3 的 RemoteApp（RAIL）模式連線，需要在有 X11／XWayland 顯示的圖形工作階段內執行（跟 Bottles／Wine 一樣的前提）；密碼一律經由標準輸入傳遞給 RDP 用戶端，不會出現在指令列參數或處理程序清單裡。

### 把 Windows 應用程式釘進啟動器

裝好某個 Windows 應用程式後，用 `app-add` 把它釘進桌面的圖形啟動器（Launcher），不用每次都打指令列：

```bash
# 釘選一個應用程式（<執行檔> 跟 setup 完成後 app 子命令用的路徑寫法相同）
duduclaw compat windows-vm app-add winword.exe --name "Word"

# 查看目前釘了哪些
duduclaw compat windows-vm app-list

# 移除釘選（<執行檔> 必須跟當初 app-add 輸入的完全一致，不支援模糊比對）
duduclaw compat windows-vm app-remove winword.exe
```

釘選成功後，最長等 60 秒（Launcher 背景重新掃描的週期）就會以「<名稱>（Windows）」的樣式出現在啟動器的「應用程式」清單裡；點下去會在背景執行 `duduclaw compat windows-vm app <執行檔>` 幫你把無縫視窗開起來——跟你手動在指令列打是同一條路徑，只是省了打字。這類項目有自己的「疊窗」圖示（前後兩層視窗，代表程式在虛擬機裡、以無縫視窗投到桌面）；因為虛擬機內的程式拿不到真實圖檔，所有釘選項共用這一枚誠實的來源標記，不逐項偽造。

**還沒執行過 `setup` 的機器，啟動器完全不會出現任何 Windows 項目**——這是刻意的誠實靜默：`app-add`/`app-list` 讀寫的登記檔（`~/.duduclaw/windows-vm/apps.toml`）在 `setup` 都還沒跑過的機器上根本不存在，啟動器不會因此顯示錯誤或空白區塊，就是完全不出現這個分類。

在 DuDuClaw OS 上，這份登記檔改放在 `/data/system/windows-vm/apps.toml`（映像為 gateway 與 root 的 shell 設定 `DUDUCLAW_WINDOWS_VM_APPS_DIR=/data/system/windows-vm`）：`/data/duduclaw` 是 gateway 跑內建 AI CLI 時的家目錄、放著各家登入 token，因此是 `0700`，kiosk 殼讀不到裡面的檔案。只有登記檔搬家；`compose.yaml` 與 VM 儲存仍在 `/data/duduclaw/windows-vm`。

再次對同一個執行檔執行 `app-add` 會覆蓋原本的顯示名稱，不會產生重複項目。

### 硬體虛擬化（KVM）是硬性要求

這條路線需要本機支援硬體虛擬化（CPU 的 VT-x／AMD-V 功能，Linux 核心需啟用並載入 `kvm` 模組）。`setup` 會在授權揭露之前先檢查 `/dev/kvm` 是否存在——找不到就直接失敗並說明原因，不會退回軟體模擬（那樣的效能不具實用性，dockur/windows 本身也沒有這個備援）。

### 已知限制（本版）

- `setup`／`status` 本身仍是命令列流程（授權揭露確認、資源門檻建議這類需要人親自確認的步驟不適合塞進圖形精靈的一次性彈窗）；「應用程式一鍵出現在圖形介面的啟動器」這部分已經做了——見上一節「把 Windows 應用程式釘進啟動器」。
- Windows VM 實際能不能開機、無縫視窗好不好用，只能在真實硬體或雲端主機驗證——QEMU 測試環境本身通常沒有巢狀虛擬化，會被上面的 KVM 檢查誠實擋下。
- 釘進啟動器的 Windows 項目共用一枚「疊窗」來源圖示、Bottles 在可安裝清單裡是「雙瓶」圖示（2026-08-31 設計輪拍板）；虛擬機內個別程式的真實圖檔仍拿不到，屬平台限制而非待辦。

## Android 應用程式：Waydroid

Waydroid 是一個 Android 應用程式容器，讓你在 DuDuClaw OS 上安裝、執行手機 App（例如 LINE）。

### 為什麼需要自己動手裝

DuDuClaw OS **出廠不內建** Google 服務框架（GApps）與 ARM 應用程式轉譯元件。原因很直接：這些元件的來源不透明（源自對 Windows Subsystem for Android 的逆向工程），DuDuClaw 沒有合法散布權利，因此不會、也不能預先幫你裝好。

這表示：

1. 你需要自己完成 **Google Play 裝置認證**（每台裝置認證一次即可）——這是 Google 官方提供的自助流程，跟在一般 Android 裝置上刷機後认证的步驟相同，不是 DuDuClaw 特製的東西。
2. 若你要跑的應用程式只有 x86/x86_64 沒有的 ARM 版本，需要自行加裝 ARM 轉譯元件；這類元件品質與相容性因來源而異，DuDuClaw 無法替你把關。

`duduclaw compat list` 顯示 `waydroid` 缺件是正常現象，不是系統壞掉——它誠實反映「容器本體已登記，但底層依賴（`waydroid`／`lxc-start`）或你自己要補的認證/轉譯元件還沒到位」。

### 已知能跑的案例

LINE 已在 arm64 硬體上完整驗證過「安裝 → 啟動 → 掃碼登入」全流程可行（使用 LINE 的「副裝置（Sub device）」模式，讓你的手機保持原本登入狀態）。這是目前唯一有實測證據支持的案例；其他應用程式請自行評估，不代表全面保證。

## macOS 應用程式：我們不做本機執行

**DuDuClaw OS 永遠不會、也不能讓你在非 Apple 硬體上直接執行 macOS 應用程式**——這不是技術選擇，是 Apple 軟體授權條款與美國判例法（Apple v. Psystar）劃下的法律紅線，DuDuClaw 不會踩。

我們提供的是另一條合法、實用的路：**內建遠端連線工具，讓你從 DuDuClaw OS 桌面直接操作你自己那台 Mac**（Mac 需保持開機、你本人登入中）。你在 Mac 上的專屬軟體還是跑在你的 Mac 上，DuDuClaw OS 只是多一個操作視窗。

換句話說：DuDuClaw OS 與你的 Mac **無縫協作**，而不是取代它。
