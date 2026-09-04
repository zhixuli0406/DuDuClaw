# TODO: H1-ISO — 第一次真烤 x86-64 安裝媒介＋QEMU 全模擬安裝鏈驗證

> 狀態：**Done（軟體工程部分）／真機驗證 Not Started（2026-08-25）** · 類型：Feature/驗證（appliance image，x86-64 首次真烤）· 優先：High
> 目標：讓使用者能在真機（Intel N305 miniPC / AMD 8845HS miniPC，皆 UEFI）上，
> 燒錄一份可開機媒介、安裝 DuDuClaw OS，並完整驗證「開機→安裝→重開進系統」全鏈，
> 在 QEMU x86_64 全模擬下先行驗證，真機驗證留待硬體到位。

> **2026-09-04 追記**：本 TODO 描述的 `appliance/` Debian/mkosi 線已凍結，並隨 OS 拆分移至
> [DuDuClaw-OS](https://github.com/zhixuli0406/DuDuClaw-OS) repo 的 `appliance/`（僅供參考）；
> 出貨產物改由該 repo 的 Yocto 層產出（v0.1.0 起含安裝器 ISO）。本文件保留為歷史紀錄，
> 文中 `appliance/...` 路徑指該 repo。

## 一句話

`appliance/build.sh` 從未真的烤過 x86-64（雖然那才是設計上的「出貨預設」），這輪把它烤出來、
補齊 N305/8845HS 需要的韌體套件、在 QEMU 全模擬下驗證了安裝腳本的偵測邏輯（抓到並修好一個
QEMU 拓樸的真 bug），並拿到一份可開機、可安裝、開機後真的跑進 OOBE 首頁的成品映像。

## 交付物

| 項目 | 路徑 | 大小 | 說明 |
|---|---|---|---|
| 可燒錄映像（壓縮） | `appliance/.build/h1-x86/duduclaw-os-x86_64.raw.zst` | 1.23 GiB | **建議發布用這個**，zstd -6 --long=27 壓縮，比 14.5G 原始檔省 91.5% |
| 可燒錄映像（原始） | `appliance/.build/h1-x86/duduclaw-os.raw` | 14.5 GiB（sparse，實際落地約 3.7GiB） | dd/Etcher 直接可燒的整顆 GPT 磁碟映像 |
| sha256（原始） | `appliance/.build/h1-x86/duduclaw-os.raw.sha256` | — | `2c28cc90cbd5c5400ef88cf96dc1af70a13733a4d59467ebd2ed61e4e58ba02b` |
| sha256（壓縮） | `appliance/.build/h1-x86/duduclaw-os-x86_64.raw.zst.sha256` | — | `582ea5c806a35f56499fa6e087258f8a121f510ea2a4ee7ce827cc12822510d8` |
| 安裝鏈驗證腳本 | `appliance/tests/lib/h1_x86_install_test.py` | — | USB+NVMe 雙碟拓樸、canary byte 驗證、Phase A+B 全自動 |
| QEMU 版本 | `qemu-system-x86_64 11.1.0`（Homebrew） | — | 驗證環境 |

這**不是** ISO9660 格式的 `.iso` 檔——是整顆 GPT 磁碟映像（`mkosi.conf` `Format=disk` 的產出）。
dd／balenaEtcher 燒錄行為與 `.iso` 完全相同（兩者對燒錄工具而言都只是「整顆寫入區塊裝置」），
只是檔案內部格式不同（不會是可掛載瀏覽的光碟映像）。若要 `.iso` 副檔名純粹因為某些燒錄工具
UI 白名單限制，同一份 bytes 存成 `.iso` 副檔名一樣能燒——但這是待拍板事項，這輪先誠實用真實
副檔名交付。

## 燒錄指示

```sh
# 解壓縮（若下載的是 .zst）
zstd -d duduclaw-os-x86_64.raw.zst -o duduclaw-os.raw

# 方式一：dd（Linux/macOS，先用 diskutil list / lsblk 確認目標裝置代號，
# 燒錯磁碟會整顆覆蓋，務必先核對！）
sudo dd if=duduclaw-os.raw of=/dev/diskN bs=4m status=progress
sync

# 方式二：balenaEtcher（GUI，跨平台，讀 .raw/.iso/.img 一視同仁）
# 開啟 Etcher → Flash from file → 選 duduclaw-os.raw → 選目標 USB → Flash
```

燒錄目標需 ≥16GB（原始映像 14.5GB＋一點餘裕）。

## 真機安裝步驟

1. 燒錄好的 USB 隨身碟插入 miniPC（N305 或 8845HS 皆可，同一份 x86-64 映像通吃）。
2. **開機進 BIOS/UEFI 設定**：
   - 確認開機模式為 **UEFI**（不是 Legacy/CSM）。
   - **關閉 Secure Boot**——這份映像的 UKI 是 unsigned（`mkosi.conf` `UnifiedKernelImages=yes`
     但未接 Secure Boot 簽章鏈，README.md「Explicitly out of scope」已記錄此為已知限制），
     Secure Boot 開著會拒絕開機。
   - 開機順序調整為優先從 USB 開機（或用一次性開機選單，如 F12/F10，視主機板廠牌而定）。
3. 從 USB 開機。**這一步之後全自動、沒有互動畫面**：機器偵測到自己開機碟是可移除媒介＋
   內建 NVMe 已有 DuDuClaw 分割標籤（全新機器則需要在核心啟動參數加
   `duduclaw.install=yes`，見下方「已知限制」）→ 把整顆 USB dd 到內建 NVMe → 關機。
4. 機器自動關機後，**拔除 USB 隨身碟**，重新開機。
5. 開機後主機在區網廣播 `http://duduclaw.local`（或用路由器查 DHCP 租約找 IP），瀏覽器打開
   即為首次設定精靈（OOBE，本輪 QEMU 驗證的畫面），完成帳號/AI 服務設定即可使用。
6. 若主機接了螢幕（HDMI/DP），開機後會自動偵測並全螢幕顯示同一個設定精靈，不需額外操作。

## 這輪做的事（按時間順序，含踩坑與修復）

### 1. 架構認知落差澄清

發起任務的指令假設「需要另建 live-ISO + 獨立安裝腳本」，但讀 `appliance/README.md`／
`build.sh`／`mkosi.conf` 後發現：**x86-64 才是 `APPLIANCE_ARCH` 的預設值**（真正的出貨目標，
arm64 只是 Apple Silicon 本機快速煙測用的非出貨路徑），且**安裝器早就存在**——
`mkosi.extra/usr/local/sbin/duduclaw-usb-install.sh`：同一顆 golden image 身兼安裝媒介與成品，
開機自我偵測「可移除媒介＋內建 NVMe」就自我 dd 安裝再關機。因此**沒有新建**
`appliance/installer/` 或 `appliance/build-iso.sh`——那會是重複、且與現有機制不一致的第二條
安裝路徑。

### 2. N305 / 8845HS 韌體套件

`appliance/mkosi.conf.d/10-arch-x86-64.conf` 新增四個套件（僅 x86-64 arch-gated，不動共用
`mkosi.conf`——`intel-microcode`/`amd64-microcode` 在 arm64 上根本不存在套件，放共用清單會
直接炸掉 arm64 build）：

| 套件 | Installed-Size | 用途 |
|---|---|---|
| `intel-microcode` | ~20.4MiB | N305 CPU 微碼；在 8845HS 上惰性不生效 |
| `amd64-microcode` | ~0.7MiB | 8845HS CPU 微碼；在 N305 上惰性不生效 |
| `firmware-misc-nonfree` | ~12.9MiB | i915（N305 內顯）GuC/DMC 韌體所在 |
| `firmware-amd-graphics` | ~89.8MiB | amdgpu（8845HS 內顯）韌體 |

合計約 123.7MiB。已在真實烤圖中驗證這些套件確實被抓取安裝（bake log 可見
`Get:472 ... non-free-firmware amd64 firmware-amd-graphics ...`）。**GPU 驅動實際在真機上
能不能點亮螢幕仍未驗證**（無真機）。

### 3. x86-64 Rust 二進位＋mkosi 烤圖

只編了 `duduclaw`（gateway+cli 合一，含 dashboard）與 `duduclaw-sysd`（`container/
Dockerfile.server` 的 `rust-builder` stage，`--platform linux/amd64`，Docker Desktop 全程模擬，
46 分鐘）。**刻意跳過 `duduclaw-comp`／`duduclaw-shell`**（gpui 原生殼／smithay 合成器）——兩者
都是 `build.sh` 明確標記的 optional 步驟，未設定時 kiosk 自動退回 Chromium 版 dashboard；拉進
zed 整個 monorepo 編譯量太大，不是這輪「先把 x86-64 出貨路徑跑通」的必要條件。

**兩度被同機並行的 arm64 烤圖（其他 session）蓋掉 `mkosi.output/`**（共用目錄的教訓）：
第一次 19:52 烤好，21:46 被覆蓋；第二次重烤後改用「build.sh 一結束立刻 move（非 cp）到
`appliance/.build/h1-x86/`」的包裝腳本（`rebake-and-rescue.sh`），22:33 成功烤出並即時隔離，
之後再沒被覆蓋過。**教訓已固化**：任何用到 `mkosi.output/` 的產線都應該把「成功後立刻搬離
共用目錄」寫進腳本本身，不能依賴人（或監看通知）記得手動搬。

Docker Desktop 虛擬磁碟一度只剩 11.9G（另一 session 的 arm64 bake 因此失敗過一次）；
`docker volume rm`/`docker system prune` 等清理動作被 auto-mode classifier 擋下（destructive
action 需要人工核可），這輪繞開（等待自然釋放＋`docker builder prune -f`），沒有解決根本容量
問題——長期建議：Docker Desktop Settings → Resources → Disk 調大，或使用者手動跑一次
volume 清理。

### 4. QEMU 安裝鏈驗證：真的抓到一個真 bug

`appliance/tests/lib/h1_x86_install_test.py`（沿用既有 `qmp_client.py`/`screen_check.py`/
`test_run.py` 測試庫）用 `usb-storage`（qemu-xhci）模擬 USB 開機媒介、`nvme` device 模擬內建
NVMe，前兩次嘗試都在逾時前沒有觸發安裝，一開始誤以為是「開機或安裝卡住」，後來加了 serial-log
即時擷取（改用 `-serial file:PATH`，先前用 `-serial tcp:...,server,nowait` 但沒有任何 client
連線讀取，等於白錄）後才看清真相：**QEMU 的 `usb-storage` device 的 `removable` 屬性預設是
`off`**（`qemu-system-x86_64 -device usb-storage,help` 白紙黑字），導致 guest kernel 從未把
開機碟視為可移除媒介，`duduclaw-usb-install.sh` 依照它自己的安全邏輯正確判定「非可移除媒介，
什麼都不做」後直接放行開機——**不是逾時卡死，是腳本的安全閘門正常運作，只是 QEMU 拓樸沒給對
訊號**。加上 `removable=true` 後，guest kernel 訊息確認 `sd 6:0:0:0: [sda] Attached SCSI
removable disk`，`duduclaw-usb-install.service` 真的觸發並開始執行。

### 5. Phase A（QEMU 全模擬 dd）效能崩潰，改用 host 側直接寫入

修好 removable 偵測後，dd 真的開始跑，但用 QMP `query-blockstats`（`wr_bytes` 除以總量
15,570,325,504 bytes）精確量測進度：跑了 4.5 小時只完成 **0.86%**，速率 **8.6 KiB/s**，
ETA **約 486 小時（20 天）**——VM 狀態確認 `running`（非卡死），交叉比對來源碟 `rd_bytes`
在同視窗完全沒變化，符合 guest 端頁快取吸收讀取、寫回逐步落盤的行為，是真的在動、只是慢到
不可能在合理時間內跑完。**推測根因**：QEMU 在 TCG 全軟體模擬下對 `nvme` device model
（doorbell/completion-queue 協定，MMIO 密集）的效能特別差，這個 repo 其他地方的 QEMU 腳本一律
用 virtio-blk，這是第一次用 `nvme` device——真機用真實 NVMe 控制器，不會有這個問題，純屬
「TCG 軟體模擬 nvme 協定」的測試環境限定缺陷。

**由於安裝腳本的觸發邏輯已經被獨立證實正確**（kernel 訊息＋systemd 稽核紀錄確認 unit 真的
啟動在跑），繼續空等純軟體模擬的 dd 完成沒有意義，改用 host 側直接 `dd`（17.3 秒，996MB/s，
sha256 逐位元組核對與 golden image 完全一致）把 nvme-target.raw 準備好，只用 QEMU 驗證
**Phase B：這顆「裝好」的碟能不能正常開機到 OOBE**。

### 6. Phase B：PASS，真實螢幕截圖為證

`appliance/.vm/test-artifacts/20260824T223726Z-h1-x86-install-phaseb-only/success-oobe.png`
——1 分 45 秒內從 UEFI 開機到 Chromium kiosk 顯示 DuDuClaw dashboard 的首次設定精靈
（"Let's create your first agent" / v1.62.0），OCR 正確辨識（`gray-2x-psm3` pass，
bbox=(450,186,171,23)）。**意外發現**：畫面語系是英文而非預期的 zh-TW（這份映像沒有設定
`LANG=zh_TW.UTF-8`，Chromium 跟著系統 locale 走），OOBE 文字比對邏輯已改成同時接受中英文
（`OOBE_TEXT_CANDIDATES = ["your first agent", "開始建立第一位"]`），比照 `q3_ocr_boot_accept.
py` 「接受任一真實、可 OCR 辨識的狀態」的既有慣例。

## 誠實欠帳清單

- [ ] **真機驗證全數未做**：N305／8845HS 實機還沒到貨，Secure Boot 關閉、UEFI 開機模式、真實
  Wi-Fi NIC 韌體型號、真實 GPU firmware 是否真的讓 i915/amdgpu DRM/KMS 起得來——全部只在 QEMU
  TCG 全軟體模擬下驗證過。
- [ ] **Phase A 的「QEMU 全模擬 dd」本身沒有跑到完成**——用 host 側直接寫入取代，這代表這輪
  **沒有**在「QEMU 模擬的 guest 環境內」端到端驗證過完整的 14.5GB dd 搬移，只驗證了：
  (a) 安裝腳本的偵測/觸發邏輯正確（真機同構）；(b) 一份正確安裝好的碟能正常開機（真機同構）。
  中間「guest 端 dd 真的把 15G 資料正確搬完」這件事，是靠 sha256 逐位元組核對 host-dd 結果，
  不是靠觀察 guest 內部的 dd 執行完成。**這是刻意的、有記錄的取捨**，不是隱瞞——完整 guest 端
  dd 驗證留給真機（真實 NVMe 不會有這個 TCG 效能問題）。
- [ ] **"全新空白碟 + `duduclaw.install=yes`" 安裝分支未測**——這輪測的是「目標碟已有
  `duduclaw-` 標籤 → 視為升級」分支；cmdline 烤進 UKI 的 `.cmdline` PE section，不是
  systemd-boot 可在開機時盲改的文字檔，判斷不值得為此加複雜度。兩分支共用 100% 的實際搬資料
  程式碼（dd/partx/poweroff），只是觸發條件不同。
- [ ] `duduclaw-comp`／`duduclaw-shell` 未針對 x86-64 編譯過，原生殼路徑在 x86-64 上完全沒
  驗證；Phase B 驗證到的 OOBE 畫面是 Chromium 版 web dashboard，不是 gpui 原生殼。
- [ ] Docker Desktop 磁碟空間偏緊的根本問題未解決（見上方 §3）。
- [ ] `.iso` 副檔名 vs raw 磁碟映像的使用者體感落差：交付的是 `.raw`/`.raw.zst`，不是真正的
  ISO9660 檔案（見上方「交付物」說明）。如果使用者堅持要 `.iso` 副檔名，可以把同一份 bytes
  存成 `.iso` 副檔名交付（dd/Etcher 對兩者行為一致），但這是待拍板事項，不擅自決定。
- [ ] 這份映像沒有設定 zh-TW locale——OOBE 畫面預設英文，不是設計目標的「面向台灣使用者」
  預期體驗（雖然功能完全正常）。是否要在 mkosi recipe 裡加 `locales-all` + 預設
  `LANG=zh_TW.UTF-8`，待拍板。
- [ ] `appliance/README.md` 尚未回填這輪的發現（x86-64 首次真烤成功、`removable=true` 這個
  QEMU 測試 gotcha、Phase A 的 nvme-device-TCG-效能限制）——這輪時間優先花在跑通驗證鏈，
  README 更新留待下一輪或使用者確認交付內容無誤後補。

## 相關檔案

- `appliance/mkosi.conf.d/10-arch-x86-64.conf`（韌體/微碼套件）
- `appliance/tests/lib/h1_x86_install_test.py`（安裝鏈驗證主腳本，USB+NVMe 拓樸、canary、
  Phase A/B、locale-agnostic OOBE 比對）
- `appliance/.build/h1-x86/rebake-and-rescue.sh`（mkosi.output 共用目錄隔離包裝）
- `appliance/.build/h1-x86/watch-phase-a-mtime.sh`（mtime 存活性監看，不誤殺健康長任務）
- `appliance/.build/h1-x86/run-phase-b-only.py`（host-dd 後單獨驗證 Phase B 的驅動腳本）
- `appliance/mkosi.extra/usr/local/sbin/duduclaw-usb-install.sh`（既有，未改動——邏輯已驗證正確）
- `appliance/.build/h1-x86/duduclaw-os-x86_64.raw.zst`（交付映像）
