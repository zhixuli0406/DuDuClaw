# DuDuClaw OS 桌面版：人與 AI 共用一台機器

> 桌面版就是一台你照常使用的電腦，AI 員工跟你住在同一台機器上，做自己的
> 事，不會佔用你的螢幕、游標或專注力：你一碰鍵盤或滑鼠，AI 在你桌面上正
> 在操作的東西就立刻停下來。

---

## 這是什麼

整碟 DuDuClaw OS 映像（`duduclaw-image-appliance`）開機後進入的是 DuDuClaw
自己的桌面：自己的 Wayland compositor（`duduclaw-comp`）與 shell
（`duduclaw-shell`），不是隨便一套 Linux 桌面外掛一個 agent。人坐在這台
機器前，把它當自己的電腦用：瀏覽器、辦公軟體、遊戲，透過相容層跑
Windows 與 Android 應用程式；同時 DuDuClaw gateway 與它的 AI 員工在同一
台機器上運作，回覆聊天通道、跑目標、感知環境，需要時也會親自操作桌面上
的應用程式。

讓共用機制真正能運作的設計規則只講一次，並且由 compositor 強制執行：
**你的桌面就是你的；人的輸入永遠優先。** 底下所有規則都不仰賴 agent 自
己乖乖聽話。input seat、凍結與急停都由 compositor 持有，agent 行程完全
碰不到這些東西。

## 桌面

- **主畫面、視窗切換器、鎖定畫面、控制中心、通知中心**：一套精簡桌面，
  鍵盤就能操作全部功能。完整快捷鍵列表見
  [51-os-keyboard-shortcuts.md](51-os-keyboard-shortcuts.md)。
- **Cmd+K 交辦列**：在任何應用程式裡叫出這條列，用白話把任務交給 AI 員
  工。它是桌面版進入目標迴圈的入口，跟聊天通道用的是同一套機制
  （[34-goal-loop.md](34-goal-loop.md)）。
- **首次開機精靈**：第一次開機時設定語言、網路、管理員帳號和幾項偏好；
  live 安裝器 ISO 本身也是一套圖形化精靈。
- **AI runtime 授權**：精靈其中一步列出映像內建 runtime 的每一家 AI 服務
  （Claude Code、Codex、Gemini CLI、Grok、Qwen、Kimi、Copilot、Kiro、
  Cursor、Mistral Vibe、OpenCode，以及純 API 金鑰的幾家），每一列標明目
  前是未設定、已存金鑰還是已登入。可以貼上 API 金鑰（走跟管理面同一條加
  密的 `[[accounts]]` 路徑），CLI 有登入流程的也可以直接登入帳號：機器會
  代跑那支 CLI 自己的登入，把裝置代碼和登入網址顯示在畫面上，並提供按鈕
  用機器上的瀏覽器開啟。用消費者訂閱登入前會先顯示風險，你勾選同意才會
  開始——2026 年 3 月起 Anthropic 與 Google 在伺服器端擋掉第三方產品使用
  訂閱憑證，已經有帳號被停權，所以建議走 API 金鑰。整步可以一鍵略過；完
  成頁的摘要會寫最後授權了幾家。
- **輸入與音訊**：fcitx5 中文輸入法、PipeWire / WirePlumber 音訊，
  XWayland 讓 X11 應用程式也能跑。
- **應用程式**：Chromium、LibreOffice 和 Steam 預先以 Flatpak 形式離線裝
  好；Windows 桌面應用程式走 Bottles，完整 Windows 走 KVM 虛擬機 + RDP，
  Android 應用程式走 Waydroid。哪些有承諾、哪些沒有，寫在
  [應用程式相容性指南](../../guides/zh-TW/app-compat.md)裡。

## 共用如何不打擾你

compositor 認得一個共駕 session 的三種駕駛模式：

| 模式 | 誰在操作你的桌面 | 時機 |
|---|---|---|
| **人駕**（預設） | 你。這個共用桌面上，agent 完全沒有輸入權限。 | 除非你交辦了需要 GUI 的任務，否則永遠如此。 |
| **影子** | 你的桌面上沒有人在動：agent 在一個獨立的無頭第二輸出
（`duduclaw-shadow-0`）上工作，想看的話角落還有一個小小的子母畫面
（PiP）預覽。 | GUI 任務的預設落點。你的視窗、焦點與游標都不受影響，你
可以繼續看影片或打遊戲。 |
| **共駕**（旁觀 / 接手） | agent 在你看得見的那個桌面上操作，你全程盯著
每一步。 | 只有在任務目標就是你正在用的那個視窗、你把預覽拉到前台，或
某個敏感步驟需要你在場時，才會發生。 |

以下規則全部由 `duduclaw-comp` 強制執行，不靠 agent：

- **專屬的 seat。** agent 透過一個獨立的 seat（`duduclaw-agent`）注入輸
  入，所以稽核紀錄裡每個事件都能歸屬到它身上，而且它的游標形狀和顏色都
  跟你的不一樣。
- **你一碰，就凍結。** 實體 seat 傳來的任何鍵盤或滑鼠事件，都會在下一個
  agent 指令被處理之前，先讓 agent 的 seat 凍結：在 QEMU 與容器環境裡實
  測 3–4 ms，對照設計目標的 50 ms 上限。被凍結的指令是直接丟棄，不會排
  隊，所以不會有事後「幫你補回來」這種事。重新連線也不會解除凍結。
- **交還是明確動作。** Super+Enter，或 shell 內建的交還按鈕。刻意不做
  「閒置 N 秒自動恢復」這種設計：隱性恢復遲早會出意外。
- **Super+Esc 是急停。** 它會直接終止整個 session，agent 無法攔截，也無
  法停用它。
- **Watch mode（人在場模式）。** 敏感情境下，接下來的操作流程需要你在
  場：你離開，session 就暫停，你回來才會繼續，這是交還規則裡唯一的例
  外。
- **你不會沒注意到的邊框。** agent 共駕時螢幕會出現琥珀色邊框；進入接手
  狀態會轉成暗紅色；沒有 session 時什麼都不會畫。
- **日常使用不受影響。** 聊天回覆、目標迴圈、排程工作和型別化 MCP 工具
  都不會碰桌面。你用機器的時候，影子工作照樣在跑。凍結唯一會擋下的，是
  你桌面上那一個前台共駕 session。

## AI 在你桌面上能做什麼

- **先走型別化工具，GUI 是最後手段。** DuDuClaw 自己的應用程式一律透過
  gateway 的工具操作，不會去碰它們的像素。第三方應用程式優先走一份原生
  API、CLI 與 D-Bus 介面的登錄表（如果存在的話），再退到 accessibility
  tree（AT-SPI2）操作 GTK / Qt / Chromium 應用程式。這個版本不支援以截
  圖為基礎的操作。
- **預設關閉。** 共駕是每個 agent 各自的 capability（`[capabilities]
  codrive`），fail-closed；`codrive_run` 工具屬於 Admin 權限範圍。
- **有後果的動作一律先問。** 發送、購買、刪除、授權：都會透過跟
  DuDuClaw 其他動作相同的 broker 開出一張審批卡，在核准之前什麼都不會
  注入。被拒絕、過期，或 broker 連不上，都會中止這一步。
- **憑證永遠不會落到 agent 手上。** 登入、密碼與付款步驟，agent 會主動
  把桌面交棒給你（`take_over`）；這段期間 agent 的感知同步凍結。網路銀
  行和 CAPTCHA 在拒絕清單上，連審批卡都不會開。
- **看到的東西一律先當資料處理。** 從螢幕或 accessibility tree 讀到的文
  字，送進模型之前一律先圈成資料，密碼欄位絕不會被讀取，看起來像注入的
  文字會被消毒並記進紀錄。注入風險不是零；人工審查是最後一道防線。
- **私有、有身分驗證的通道。** 注入用的 socket 一律要 token 驗證，永遠
  不公開。在值班機上，gateway 跑在一個使用者底下，compositor/shell 跑在
  另一個使用者底下，所以 agent 行程在結構上就是碰不到人這一側的 shell
  控制 socket。每個事件都會落進 JSONL 稽核紀錄。

## 目前狀態

- v0.1.0 整碟映像內建這套桌面。共駕功能已經編譯進去，但**預設關閉**。
- 目前已驗證的項目（用真實輸入事件，不是模擬器）：凍結／交還／急停、目
  標高亮、稽核、socket token 輪替、帶子母畫面的影子工作區、agent 主動交
  棒、watch mode、以及邊框狀態，驗證環境是容器（Xvfb + 真實 Chromium）
  與 QEMU 虛擬機。
- 尚未驗證：真實 DRM 硬體 backplane（包含雙螢幕邊框幾何）、實機上的交還
  按鈕、gateway 到 compositor 的狀態往返，以及實機上的 accessibility
  tree click-through（這項測試目前暫停，等能重跑再繼續）。目前還沒有
  session 錄影功能。
- 值班機本身的安全態勢（唯讀 root、防火牆、權限分離）與信任鏈建置選項，
  寫在 [50-duduclaw-os-appliance.md](50-duduclaw-os-appliance.md)裡。

## 延伸閱讀

- [50-duduclaw-os-appliance.md](50-duduclaw-os-appliance.md)：值班機的版
  本、安裝、裝置頁面與安全設計。
- [51-os-keyboard-shortcuts.md](51-os-keyboard-shortcuts.md)：全部快捷
  鍵，包含 Super+Enter / Super+Esc。
- [34-goal-loop.md](34-goal-loop.md)：任務交給 AI 員工之後會發生什麼事。
- [42-human-takeover.md](42-human-takeover.md)：同一條「人開口就優先」的
  規則，套用在聊天通道上。
- [33-os-native-perception.md](33-os-native-perception.md)：AI 在機器上
  感知環境的方式，不會碰到你的桌面。
