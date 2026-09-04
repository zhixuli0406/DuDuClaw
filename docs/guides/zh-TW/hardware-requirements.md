# DuDuClaw OS 硬體需求與相容性指南

DuDuClaw OS 是一套 **x86-64 的 Agent-Native 作業系統**，跑在一般消費級 PC、自組 PC 或迷你主機上——不需要特殊硬體、不需要獨立顯卡，2015 年後的主流 x86 電腦幾乎都能跑。

本文件把硬體分成**兩層**，這是理解全文最重要的框架，別混為一談：

1. **能跑 DuDuClaw OS 本體的硬體**——x86-64 + AVX2（x86-64-v3）+ UEFI + SSD。消費級迷你主機、自組 PC、甚至多數 x86 筆電只要符合條件就支援。
2. **跑不了 OS、但在 DuDuClaw 生態有明確角色的硬體**——樹莓派這類 ARM SBC、ESP32/Arduino 這類微控制器（MCU），它們的定位是**感測端點（sensor endpoint）**：透過 DuDuClaw 的 resident sensing（`[[tick.sources]]`）把資料餵給跑在 x86 硬體上的 agent，不是拿來跑 OS 本體。

先看「硬性條件」（不符就開不了機），再依用途查配置表或相容性檢查清單。

## 目錄

- [硬性條件](#硬性條件三項不符就跑不起來)
- [配置表](#配置表)
- [自組 PC 相容性檢查清單](#自組-pc-相容性檢查清單)
- [x86 筆電評估](#x86-筆電評估)
- [推薦消費級機型](#推薦消費級機型可直接買)
- [已知驅動缺口](#已知驅動缺口選機時避開)
- [非 x86 硬體與 IoT 端點（樹莓派／Arduino／ESP32）](#非-x86-硬體與-iot-端點樹莓派arduinoesp32)
- [兩層總矩陣](#兩層總矩陣)
- [為什麼不能用 Mac / ARM 模擬驗證](#為什麼不能用-mac--arm-模擬驗證)
- [燒錄開機媒體的正確做法](#燒錄開機媒體的正確做法)

## 硬性條件（三項，不符就跑不起來）

1. **x86-64 CPU 且支援 AVX2**（＝ x86-64-v3 baseline：AVX、AVX2、BMI1、BMI2、F16C、FMA、LZCNT、MOVBE、OSXSAVE）
   - Intel：Haswell（2013，第 4 代 Core）以上；Atom 衍生的 Celeron/Pentium N 系列則要到 **Gracemont**（Alder Lake-N，如 N100/N200/N305，2023 年後）才支援，更早的 Goldmont/Goldmont Plus/Tremont 世代不一定有 AVX2（見下方「自組 PC 相容性檢查清單」的細節與查證方式）。
   - AMD：**Excavator**（2015，行動端 APU）起，接著 Zen/Zen+/Zen 2/Zen 3/Zen 4/Zen 5（2017 起）全數支援；更早的 Bulldozer/Piledriver/Steamroller 世代（FX 系列桌機、部分 APU，2011–2014）**不支援** AVX2/BMI2，即使名義上也是「x86-64」。
   - ❌ **不支援 arm64**：包括 Apple Silicon Mac、樹莓派、ARM 迷你主機。OS image 是 x86-64 編譯，ARM 機器（或在 ARM Mac 上用 UTM/QEMU 模擬）開不了機或極慢，不是支援路徑。
2. **UEFI 開機**（2012 後的主機板皆有）
   - A/B 原子更新 + systemd-boot 依賴 UEFI；傳統 BIOS-only 機器不支援。
   - Secure Boot 目前可關（自簽鏈尚未強制）。
3. **SSD**（A/B 更新與 gpui 桌面對 IO 敏感；HDD 會很卡）

## 配置表

| 項目 | 最低 | 建議 | 舒適（打遊戲 / 本地 LLM） |
|---|---|---|---|
| **CPU** | x86-64-v3（須 AVX2）：Intel Haswell↑ / AMD Excavator↑ | Intel N100 / N305、AMD Ryzen 5 | AMD Ryzen 7 8845HS |
| **記憶體** | 4 GB | 8 GB | 16 GB+ |
| **儲存** | 64 GB SSD | 128 GB SSD | 256 GB+ SSD |
| **顯示 / GPU** | 無需獨顯（CPU 內顯即可） | 內顯 | 內顯 / 獨顯 |
| **韌體** | UEFI | UEFI | UEFI |
| **網路** | 有線 or WiFi（iwd） | — | — |

> **為什麼不需要獨立顯卡？** DuDuClaw OS 目前走 **llvmpipe 純軟體渲染**（CPU 就能畫桌面），這是它對消費級硬體特別友善的關鍵——買最便宜的無獨顯迷你主機也能跑完整圖形桌面。有內顯/獨顯只是未來硬體加速的空間，不是必要。

## 自組 PC 相容性檢查清單

自組 PC（DIY 組裝機）只要符合上面的硬性條件就支援，沒有「認證清單」這種東西——但實務組裝有幾個常見雷區，逐項檢查：

| 檢查項 | 通過條件 | 常見雷 |
|---|---|---|
| **主機板 / 晶片組** | Intel 8 系晶片組（LGA1150，對應 Haswell）以上，或 AMD AM4 / AM5（2017 起）；主機板本身要有 UEFI | 太舊的 BIOS-only 古董板（2012 年前）直接不支援 |
| **CPU** | 見上方「硬性條件」的 Intel/AMD 世代表 | 二手老 AMD FX（Bulldozer/Piledriver 系列）、老 NAS 常用的低階 Atom/Celeron J 系列（Bay Trail/Cherry Trail/Goldmont 世代）可能沒有 AVX2，即使是「x86-64」也開不了機。**選購前務必用 `lscpu` 或 CPU-Z 查 flags 是否含 `avx2`**，不要只看 CPU 品牌/年份 |
| **GPU** | 免獨顯即可（llvmpipe 軟體渲染） | 有獨顯（NVIDIA/AMD/Intel Arc）不會出錯，但目前也不會被自動拿來加速——只是預留空間 |
| **網卡（有線）** | Intel（I219/I225/I226 等）、Realtek 主流晶片（RTL8111/8168 系列）Linux 支援成熟 | **RTL8125（2.5GbE）**：核心內建 `r8169` 驅動雖已合入基本支援，但社群多次回報穩定度不如 Realtek 官方 out-of-tree `r8125` DKMS（[Arch Linux 論壇](https://bbs.archlinux.org/viewtopic.php?id=262120)）；選機時避開，或預先準備 DKMS 模組 |
| **網卡（Wi-Fi）** | Intel AX 系列、MediaTek `mt7921`/`mt7925` 系列主線支援良好 | **MT7927（Wi-Fi 7 晶片）**：截至查證日仍**沒有主線驅動**，`mt7925e` 明確拒絕綁定其 PCI ID，社群僅能靠 `mediatek-mt7927-dkms` 這類 out-of-tree 套件補位（[jetm blog](https://jetm.github.io/blog/posts/mt7927-wifi-the-missing-piece/)）；選購前用 `lspci -nn` 確認 M.2 卡實際晶片，別只看主機板型錄 |
| **儲存** | NVMe 或 SATA SSD 皆可 | HDD 會很卡（A/B 更新 + 桌面對 IO 敏感），不建議 |
| **開機模式** | 主機板 BIOS/UEFI 設定內「Boot Mode」需為 **UEFI**（非 CSM/Legacy） | 不少主機板出廠預設開 CSM 相容模式，要手動切成純 UEFI；「Fast Boot」/「Ultra Fast Boot」選項可能跳過 USB 開機選單，建議先關閉再燒 USB 開機 |
| **Secure Boot** | 開/關皆可 | 目前自簽鏈尚未強制，但部分主機板預設開啟，若開機失敗可先在 BIOS 關閉排除變因 |

## x86 筆電評估

技術上，只要符合硬性條件（x86-64 + AVX2 + UEFI + SSD）的筆電就能跑 DuDuClaw OS——但 **DuDuClaw OS 目前沒有針對筆電做過專門相容性驗證**（appliance 專案鎖定的目標硬體是桌面迷你主機，見下方推薦機型），下列是 Linux 筆電支援的一般已知風險點，屬通則性提醒，不是 DuDuClaw 實測結果：

- **電源管理 / ACPI**：電池續航、風扇曲線、合蓋休眠等行為常需要廠牌專屬 ACPI 補丁；社群支援較好的品牌（如 ThinkPad、Tuxedo）風險較低，電競/超輕薄機種風險較高。
- **Wi-Fi / 藍牙**：筆電常內建與桌機 M.2 卡不同型號的 MediaTek/Qualcomm/Realtek 專屬模組，同樣可能踩到 MT7927 這類主線驅動缺口，開機後務必 `lspci -nn` 確認。
- **觸控板**：現代 Precision Touchpad（I2C HID）在近期核心支援已經成熟；較舊的 PS/2 模擬式觸控板手勢/多點觸控可能不完整。
- **Secure Boot**：多數 OEM 筆電出廠預設開啟，需自行進 BIOS/UEFI 關閉才能開 USB 開機安裝。
- **觸控螢幕、指紋辨識、HDR 面板**等進階功能不在 DuDuClaw OS 設計範圍內，不保證可用。

結論：拿筆電跑 DuDuClaw OS 技術上可行，但屬於「自行驗證」等級——建議先用 USB 開機試跑，不要直接覆蓋唯一系統碟。

## 推薦消費級機型（可直接買）

| 檔位 | 機型舉例 | 說明 |
|---|---|---|
| 💰 便宜 | Intel N100 迷你主機（Beelink / GMKtec，約 NT$4,000–6,000） | 被動散熱、省電，跑桌面＋文書＋上網足夠 |
| ⚖️ 均衡 | Beelink SER 系列（Ryzen）、Intel NUC | 效能與價格平衡 |
| 🎯 目標機型 | Intel N305 / AMD Ryzen 8845HS 迷你主機 | 專案鎖定的目標硬體，跑遊戲 / 本地 LLM 較從容 |

## 已知驅動缺口（選機時避開）

以下硬體目前**驅動支援不足**，選機時避開，或另備一張相容的網卡：

- **MT7927 WiFi 7 晶片**——截至查證日仍無主線驅動，`mt7925e` 拒絕綁定其 PCI ID，唯一解法是預先準備社群 DKMS 套件（[jetm blog](https://jetm.github.io/blog/posts/mt7927-wifi-the-missing-piece/)、[GitHub](https://github.com/danmeedev/mt7927-bazzite)）。這與「要不要自維護 kernel」無關——即使吃發行版套件也一樣抓不到，因為上游還沒寫驅動。
- **RTL8125 2.5G 有線網卡**——核心內建 `r8169` 已合入基本支援（[LKML](https://lkml.kernel.org/netdev/de076b11-2523-4116-ec08-b7e331497509@gmail.com/)），但多篇社群回報穩定度不如 Realtek 官方 out-of-tree `r8125` DKMS（[Arch 論壇](https://bbs.archlinux.org/viewtopic.php?id=262120)），確切合入版號與目前穩定度差距**未找到可信一手來源**，標記未查證，需要實機測試驗證。

選機前建議確認主機板/迷你主機用的網路晶片型號（`lspci -nn`），避開上述兩顆，或至少心裡有底需要額外裝 DKMS 模組。

## 非 x86 硬體與 IoT 端點（樹莓派／Arduino／ESP32）

這一節談的硬體**全部跑不了 DuDuClaw OS 本體**，但這不代表它們沒用——它們在 DuDuClaw 生態裡有明確的第二層角色：**感測端點**。

### 樹莓派

**為什麼跑不了 DuDuClaw OS**：樹莓派 4 用 Cortex-A72、樹莓派 5 用 Cortex-A76，兩者都是 64 位元 **ARMv8-A（aarch64/arm64）架構**，不是 x86-64（[Raspberry Pi 5 規格，經 Wikipedia 交叉確認](https://en.wikipedia.org/wiki/Raspberry_Pi)：四核 Cortex-A76 @ 2.4GHz，1/2/4/8/16GB RAM 選項）。DuDuClaw OS image 是純 x86-64 編譯，這是**指令集不相容**，不是驅動缺口——跟「不能用 Apple Silicon Mac 開機」是同一個原理（見下方章節），沒有任何軟體更新能解決，只能靠重新編譯一份 ARM64 image。

樹莓派的開機機制也和 x86 PC 不同：它預設走 Broadcom 專屬的閉源 GPU 韌體 + device-tree 開機流程，**沒有標準 PC 那種內建 UEFI**（Pi 4/5 可以裝選配的 Raspberry Pi UEFI 韌體，但那是額外的專案，不是開箱即用），這點在評估「未來要不要出 ARM64 build」時很關鍵。

**若未來要出 ARM64 build，成本評估**（誠實列工作項目，不給假工時數字）：

- 這條路並非全新起點——DuDuClaw 舊版（現已凍結）的 Debian/mkosi appliance 建置管線裡本來就有一條 `APPLIANCE_ARCH=arm64` 的「smoke-build」路徑（`appliance/mkosi.conf.d/10-arch-arm64.conf`），但那條路徑的用途明確寫著「本機在 Apple Silicon 上建置 + QEMU 煙霧測試」，該檔案自己也註明「x86-64 remains the shipping target」——也就是說它用的是**通用 arm64 Debian 核心**，從來沒在真實 ARM 硬體（更不用說樹莓派）上開機驗證過，跟「支援樹莓派」是兩回事。
- 目前的主力建置管線（Yocto / `meta-duduclaw`）則完全只有 x86-64 的 machine 設定（`duduclaw-genericx86-64.conf` / `duduclaw-qemux86-64.conf`），還沒有任何 arm64 或樹莓派專屬的 machine 定義。
- 真要支援樹莓派，工作量級大致落在這幾塊：①一個全新的 Yocto machine BSP（Yocto 生態圈已有 `meta-raspberrypi` 這類上游 BSP 層可以起步，但仍需自行整合進本專案的 image/更新管線）②樹莓派專屬開機鏈——沒有標準 UEFI，需要額外的 Pi UEFI 韌體或改走 U-Boot，現有的 UEFI + systemd-boot A/B 雙槽更新設計要重新驗證是否適用③graphics 棧（目前 x86-64 走 llvmpipe 軟體渲染）要在 aarch64 上重新驗證整條鏈路④x86-64-v3 的 CPU 調優完全不適用 ARM，需要建立一條獨立的 tune/sstate 快取，建置矩陣直接翻倍⑤「舒適檔位」主打的 Steam/遊戲用途在 ARM Linux 上沒有官方支援，這塊功能等於要整個拿掉⑥樹莓派專屬的 Broadcom 顯示/Wi-Fi 韌體與驅動需要另外調查整合。
- 整體屬於「新增一個完整平台目標」等級的工程量，不是「加一個編譯選項」的等級。沒有可靠依據可以給出精確工時，這裡刻意只列工作項目、不給假數字。

**樹莓派在 DuDuClaw 生態的正確定位**：跑一份標準 Linux（Raspberry Pi OS / Debian arm64 等），當**邊緣 agent 節點或 IoT 閘道器**——在樹莓派上寫一支小服務讀取感測器/GPIO/攝影機狀態並以 HTTP 暴露出來，讓跑在 x86 主機上的 DuDuClaw gateway 透過 `[[tick.sources]]` 把資料拉進來（實際接入方式與下方「感測端點怎麼接進 resident sensing」相同，包含 LAN 私網 IP 的限制）。也適合當多顆 Arduino/ESP32（透過 BLE/LoRa/Zigbee 等短距協定）的彙整節點，統一對外提供一個端點，減少 DuDuClaw 這端要管理的來源數量。

### Arduino / ESP32

**為什麼是微控制器（MCU），不是可以跑 OS 的電腦**：

- **Arduino Uno**：核心晶片 ATmega328P，**8 位元** MCU，**2 KB SRAM**、32 KB Flash、16 MHz（[Arduino 官方文件](https://docs.arduino.cc/hardware/uno-rev3/)）。這是純微控制器，「連跑一個精簡 Linux 都不可能」——這是**資源等級**問題，不是「架構不相容」問題：即使假設把它換成 x86 指令集，2KB 記憶體也跑不動任何現代作業系統。
- **ESP32**：Xtensa LX6（部分新款是 LX7 或 RISC-V），多為雙核 240MHz，內建 SRAM 約 256–768 KiB（依型號，原始 ESP32 為 520 KiB），**沒有 MMU**（[Wikipedia ESP32](https://en.wikipedia.org/wiki/ESP32)）。原生只能跑 FreeRTOS 或裸機（ESP-IDF SDK），不支援 Linux——即使新款加裝 PSRAM 擴充到數 MB，仍遠低於現代 Linux 桌面環境所需的記憶體量級與 MMU 虛擬記憶體管理能力。

兩者的正確定位：**感測端點**——靠 Wi-Fi（ESP32 原生支援）或加裝網路模組（Arduino 常見搭配 ESP8266/ESP32 擴充板）把感測值送出去，由更上層的裝置（樹莓派或 x86 主機）去拉取或接收。

### 感測端點怎麼接進 DuDuClaw 的 resident sensing

先講清楚一個容易誤解、但**直接來自程式碼**（`crates/duduclaw-gateway/src/tick_config.rs` / `tick_source.rs` / `tick_source_poll.rs` / `tick_source_ws.rs`）的架構限制，而不是憑印象：

1. **`http_poll` 與 `websocket`（非 loopback）都會過 `web_fetch::validate_url` 這道 SSRF 閘，私有網段 IP（`192.168.x.x` / `10.x.x.x` / `169.254.x.x` 等）會被直接拒絕**——`tick_config.rs` 的測試 `ssrf_urls_disable_the_source` 明確驗證 `http://192.168.1.10/x` 這類位址必須被拒。這代表 **DuDuClaw gateway 沒辦法直接 `http_poll` 一台架在家用/辦公室 LAN 裡的 ESP32**——這是刻意的安全邊界，不是 bug 或缺口。
2. **`websocket` 這個 tick kind 本質是 WS 客戶端，主動撥號連出去**，不是 WS 伺服器（見 `tick_source_ws.rs` 的 `connect_source`）。所以「ESP32 主動推給 DuDuClaw」這個直覺說法並不完全準確——正確講法是「DuDuClaw 撥號連到 ESP32（或某個 relay）開的 WS 伺服器，連上之後由對方決定何時推送 frame」。而且非 loopback 的 `wss://` 同樣要過上述 SSRF 閘，一樣擋掉 LAN 私網 IP。

因此，**LAN 內的 IoT 裝置實務上有三種合法接入路徑**：

1. **`command`**（需先在 `config.toml` 打開 `[tick] allow_command_sources = true`，預設關閉、fail-closed）：讓一段 operator 自寫的 argv（例如 `curl` 或一支呼叫感測器 API 的腳本）去打 LAN 裝置——這段子行程的網路呼叫**不經過** tick_source 自己的 SSRF 閘（該閘只套用在 `http_poll` 分支），所以可以合法打到私網 IP。這是目前最直接的 LAN 感測器接入方式。
2. **`file_tail`**：在跑 DuDuClaw gateway 的主機上另外排一支 cron/systemd timer 腳本，定期打 LAN 裝置的 API，把結果 append 成一行行 JSON 寫進本機 log 檔；`file_tail` 只讀檔案、完全不碰網路，自然不受 SSRF 閘限制。
3. **`websocket` + 本機 relay（loopback）**：在跑 gateway 的同一台主機上另外起一個小型 relay 服務（接 ESP32 的 MQTT 發佈，或反過來輪詢它），relay 自己在 `ws://127.0.0.1:PORT` 開一個 WS 伺服器，DuDuClaw 的 `websocket` tick source 連到這個 loopback 位址——這正是程式碼註解裡明講的「documented local-relay path」設計意圖。

**實際範例：ESP32 溫濕度感測器 → tick → autopilot 自動反應**

1. ESP32 接 DHT22，韌體寫一支極簡 HTTP server，`GET /sensor` 回傳 `{"temp":32.1,"humidity":58}`。
2. gateway 主機上排一支 cron 腳本（每分鐘跑一次），走 `command`/`file_tail` 其中一種路徑打這個 API。用 `file_tail`：

   ```bash
   # cron: 每分鐘把讀值 append 進 log
   curl -s http://192.168.1.50/sensor >> /var/log/duduclaw/esp32-temp.jsonl
   echo >> /var/log/duduclaw/esp32-temp.jsonl
   ```

3. `config.toml` 設定：

   ```toml
   [tick]
   enabled = true

   [[tick.sources]]
   id = "esp32-temp"
   kind = "file_tail"
   path = "/var/log/duduclaw/esp32-temp.jsonl"
   json_fields = { temp = "/temp", humidity = "/humidity" }
   ```

   （若想讓 DuDuClaw 自己去拉、省掉外部 cron，改用 `command`：`kind = "command"`、`command = ["curl", "-s", "http://192.168.1.50/sensor"]`、`interval_secs = 60`，並在 `[tick]` 加上 `allow_command_sources = true`。）

4. 每次有新行進來，DuDuClaw 會把 `{temp, humidity}` 自動加上 `prev_temp` / `delta_temp` / `pct_temp`，廣播成 `AutopilotEvent::Tick{source:"esp32-temp", fields:{...}}`（純 Rust 端處理，零 LLM 成本）。
5. Autopilot 規則可以直接寫：

   ```json
   {
     "trigger_event": "tick",
     "conditions": {
       "all": [
         { "field": "source", "op": "eq", "value": "esp32-temp" },
         { "field": "temp", "op": "gt", "value": 30 }
       ]
     },
     "action": {
       "type": "notify",
       "channel": "telegram",
       "chat_id": "...",
       "text": "機房溫度 {temp}°C 超過 30 度，請檢查空調"
     }
   }
   ```

溫度超過門檻時 agent 自動發訊息通知——這正是 resident sensing 的設計哲學：System 1（便宜、常駐、確定性的 Rust 規則）負責過濾，System 2（雲端 agent）只有規則命中才被喚醒，全程直到真正需要判斷才觸發 LLM。

## 兩層總矩陣

### 第一層：能跑 DuDuClaw OS 本體（x86-64 硬體）

| 硬體類別 | 支援狀態 | 在生態的角色 |
|---|---|---|
| 自組 x86 PC（Intel Haswell↑ / AMD Excavator↑、UEFI、SSD） | ✅ 完整支援 | 主機 / OS 本體 |
| x86 消費級迷你主機（N100 / N305 / 8845HS 等） | ✅ 完整支援，推薦檔位 | 主機 / OS 本體 |
| x86 筆電（符合硬性條件） | ⚠️ 理論可行，未專門驗證 | 主機 / OS 本體（風險自負） |
| 老舊 AMD FX/Bulldozer 系列、老 Atom（Bay Trail/Cherry Trail/Goldmont 世代） | ❌ 不支援（缺 AVX2） | 不適用 |
| Apple Silicon Mac（原生） | ❌ 不支援（ARM 架構） | 不適用，僅可當開發端跑 gateway（非 OS 本體） |
| Apple Silicon Mac + QEMU/UTM 模擬 x86 | ⚠️ 技術上能開機但極慢 | 不建議實際使用，僅供快速看一眼開機畫面 |

### 第二層：當周邊 / 感測端點（無法跑 OS 本體）

| 硬體類別 | 支援狀態 | 在生態的角色 |
|---|---|---|
| 樹莓派 4 / 5（ARM64 SBC） | ❌ 無法跑 DuDuClaw OS（ARM 架構不相容） | ✅ 可當邊緣 agent 節點 / IoT 閘道器，經 `http_poll`/`command`/`file_tail` 餵資料 |
| 其他 ARM Linux SBC（原理相同，未逐一驗證型號） | ❌ 同上 | ✅ 同上 |
| ESP32 系列（Wi-Fi/BLE MCU） | ❌ 無法跑任何 Linux（無 MMU、KB 級 SRAM） | ✅ 感測端點，經 `command`/`file_tail`/loopback `websocket` relay 接入 |
| Arduino Uno / Nano 等 8 位元 MCU | ❌ 同上（資源等級更低） | ✅ 同上，通常需搭配網路模組（如 ESP8266 擴充板）才能連網 |

## 為什麼不能用 Mac / ARM 模擬驗證？

DuDuClaw OS 是 x86-64。在 Apple Silicon Mac 上用 UTM/QEMU 模擬 x86-64 是**軟體模擬（TCG）**，會很慢、顯示與輸入未必順，只適合「快速看一眼開機畫面」，**不適合實際使用**。真正的體驗要燒到 USB 插 x86 UEFI 真機，才是原生速度。

## 燒錄開機媒體的正確做法

每個 release 每種 machine 有兩式產物，燒法不同（產物清單、驗簽公鑰與指令見 [DuDuClaw-OS repo](https://github.com/zhixuli0406/DuDuClaw-OS) 的 README「快速開始」；燒之前先驗 `.minisig` 與 `.sha256`）：

- **安裝器 `.iso`（建議）**：用 balenaEtcher 或 `dd` 燒到 USB，或燒成光碟；UEFI 開機進圖形安裝精靈，選目標 SSD 安裝後重開。這是唯一支援「光碟／Boot from ISO」開機的產物。
- **整碟 `.wic.zst`**：`zstd -d` 解壓後直接寫進目標磁碟（或寫到 USB 當硬碟開機）：

```bash
sudo dd if=duduclaw-os-*.wic of=/dev/rdiskN bs=4m status=progress
# rdiskN 換成目標裝置的實際編號，先用 diskutil list（macOS）或 lsblk（Linux）確認、注意別選錯碟
```

⚠️ **`.wic` 不能燒成光碟、也不能用「Boot from ISO」/ QEMU cdrom 開機**：GPT 磁碟映像走光碟（`/dev/sr0`）路徑時，kernel `sr` 驅動的 `GENHD_FL_NO_PART` 限制讓光碟不建 GPT 分割節點，開機鏈找不到分割。要從光碟或 ISO 開機，請用安裝器 `.iso`：它是 ISO9660 live 環境，不依賴開機媒體上的分割表。

## 相關文件

- [appliance-build.md](appliance-build.md) — DuDuClaw OS image 取得與建置（OS 線已移至 DuDuClaw-OS repo）
- [deployment-guide.md](deployment-guide.md) — 部署（服務端）
- [features/41-resident-sensing.md](../../features/zh-TW/41-resident-sensing.md) — resident sensing 完整功能說明（`http_poll` / `command` / `file_tail` / `websocket` 四種來源、SSRF 防護、rate cap、delta 推導）
