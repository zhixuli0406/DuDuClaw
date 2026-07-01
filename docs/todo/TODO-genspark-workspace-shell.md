# TODO:方案 B —「工作空間」雙層體驗(Genspark 對齊)

> **目標**:在現有「Calm Glass」儀表板之上,新增一個 Genspark 風格的**消費級外殼**
> (中央 prompt bar + 能力啟動網格 + Claw「您的第一位 AI 員工」入口),
> 同時**完整保留**現有 power-user 儀表板作為「進階模式」。
> 「一個框」降低門檻;「完整儀表板」留給深度使用者 —— 這是 Genspark 給不了的自架 / 隱私內核。
>
> **設計依據**:[web/DESIGN.md](../../web/DESIGN.md)(Calm Glass,本就以 genspark/Linear/Notion 為 clarity 參照)。
> **參照畫面**:Genspark AI 工作空間 4.0(深色底 + 中央 prompt bar + 工具網格)、Genspark Claw(「您的第一位 AI 員工」四張價值卡)。
> **慣例**:`[ ]` 待辦 / `[~]` 進行中 / `[x]` 完成。括號內為精確切點。
>
> **圖例**:🔴 阻塞後續 · 🟡 可平行 · 🧪 含測試 · 📝 純文件/設定 · 🎨 純前端視覺

---

## 0. 範圍

**做(In scope)**
- 新「工作空間 Workspace」著陸頁(簡易模式),預設給新使用者 / 個人版。
- 中央 prompt bar:複用既有 `/ws/chat` + `chat-store`,加上 agent / 模型選擇、連接器狀態、語音入口。
- 能力啟動網格:把 DuDuClaw 既有功能(agents / skills / tasks / odoo / inference / wiki / channels…)包成 Genspark 式啟動卡。
- 「Claw — 您的第一位 AI 員工」入口:對應一個常駐的個人預設 agent + 四張價值卡。
- 簡易 ⇄ 進階模式切換,持久化偏好。
- **桌面應用打包(Phase D)**:Tauri sidecar 殼 + 完整生命週期管理 + 簽章 / 公證 / 自動更新發佈管線。

**不做(Out of scope,明確排除)**
- ❌ 不改任何後端 RPC / WS 協定簽章(`send` / `session_info` / `/ws/chat` 保持不變);workspace 只是**新的前端組裝**。
- ❌ 不引入 credit / 計費制(方案 C,不在此)。
- ❌ 不新建「結果工廠」型 agent(簡報 / 影片生成等)——本期只做**外殼與既有能力的重新組裝**。
- ❌ 不動 `src/i18n/` 以外既有頁面的行為(純新增路由與組件)。

---

## 1. Genspark → DuDuClaw 能力對照(啟動網格資料來源)

| Genspark 分類 | Genspark 項目 | DuDuClaw 對應 | 路由 / 來源 |
| --- | --- | --- | --- |
| AI 員工 | Claw | 常駐個人 agent(預設 agent) | `/webchat` + `agents-store` |
| 辦公套件 | AI 簡報 / 表格 / 文件 | (本期不做,標 `coming-soon`) | — |
| 建構 | 設計/原型・程式碼・儀表板與 CRM | Skills · Odoo CRM · Dashboard | `/skills` `/odoo` `/` |
| 建構 | (推論) | 本地推論 / 模型路由 | `/inference` |
| 內容創作 | AI 聊天 | WebChat | `/webchat` |
| 工具 | 所有智能體 / 會議筆記 | Agents · Memory · Tasks · Wiki | `/agents` `/memory` `/tasks` `/wiki` |
| (左欄) | 技能 / 工作流程 / 雲端硬碟 | Skills · Autopilot · Shared Wiki | `/skills` `/`(autopilot) `/shared-wiki` |

> 每張啟動卡 = `{ id, label(i18n), icon(lucide), to, group, minRole?, enterprise?, status: 'ready'|'coming-soon' }`。
> 沿用 `nav-model.ts` 的 `minRole` / `enterprise` 過濾邏輯,避免重造權限規則。

---

## Phase 0 — 骨架與模式切換(無行為變更)  🔴

### P0.1 UI 模式 store
- [x] 🔴🧪 新檔 `web/src/stores/ui-mode-store.ts`:`type UiMode = 'workspace' | 'dashboard'`;`mode`、`setMode`、`toggle`。
- [x] localStorage 持久化(key `duduclaw-ui-mode`),解析失敗 fail-safe 預設值(見 P0.2)。
- [x] 🧪 單元:預設值正確、`toggle` 來回、localStorage 損毀 → 回退預設、SSR/無 `window` 不炸。

### P0.2 預設模式決策
- [x] 🔴 規則:個人版(`status.edition_profile === 'personal'`)或無既有偏好 → 預設 `workspace`;企業版 → 預設 `dashboard`。沿用 `useSystemStore` 既有欄位,不新增後端欄位。
- [x] 🧪 單元:三種組合(個人 / 企業 / 已存偏好覆蓋預設)。

### P0.3 路由掛載(不動既有路由)
- [x] 🔴 `web/src/App.tsx`:`FirstRunGate` 內新增 `<Route path="workspace" element={<WorkspacePage />} />`(lazy,比照既有 `lazyPage`)。
- [x] 🔴 著陸導向:`index` route 依 `ui-mode` 決定 render `WorkspacePage` 或 `DashboardPage`(用一個 `HomeRoute` wrapper,**不**用重導向以免閃白)。
- [x] 🧪 `WorkspacePage.test.tsx` 基本掛載(空狀態不炸)。

---

## Phase 1 — 工作空間著陸頁:Hero + Prompt Bar  🔴

### P1.1 頁面骨架
- [x] 🔴🎨 新檔 `web/src/pages/WorkspacePage.tsx`:置中欄(`max-w-[860px] mx-auto`),由上而下 = Hero 標題 → PromptBar → LauncherGrid。沿用 `app-ambient` 深色舞台,**不**包 `<Page>`(workspace 要全幅留白,有別於儀表板節奏)。
- [x] 🎨 Hero:標題「DuDuClaw 工作空間」+ 副標(i18n);品牌 🐾 + amber 漸層,呼應 Sidebar logo。

### P1.2 PromptBar 組件(複用 chat 管線)
- [x] 🔴 新檔 `web/src/components/workspace/PromptBar.tsx`:大圓角輸入框(`rounded-2xl`、`panel` 表面),placeholder「問任何問題,交辦任何任務」(i18n)。
- [x] 🔴 送出行為:複用 `useChatStore`(`connect` / `send` / `connectionState`)。送出後 → 切到對話檢視(見 P1.3),**不**新建第二條 WS。
- [x] 🔴 Enter 送出 / Shift+Enter 換行;`isStreaming` 或未連線時禁用送出(對齊 `WebChatPage` `canSend`)。
- [x] 🟡 附件:複用 `WebChatPage` 的 `readFileAsBase64` + 20MB 守衛(抽到 `web/src/lib/attachments.ts` 共用,避免複製)。
- [x] 🧪 `PromptBar.test.tsx`:Enter 觸發 `send`、空字串不送、串流中禁用、附件超限提示。

### P1.3 對話檢視銜接(送出後不離開 workspace)
- [x] 🔴 `WorkspacePage` 狀態機:`idle`(顯示 Hero+Grid)→ `conversing`(顯示對話流)。`messages.length > 0` 即進入 `conversing`。
- [x] 🔴 對話流複用 `WebChatPage` 的 `MessageBubble` / `TypingIndicator`(抽成共用組件 `web/src/components/chat/` 後兩頁共用,避免分叉)。
- [x] 🟡 「＋ 新對話」回到 `idle`(呼叫 `chat-store.reset`)。
- [x] 🧪 idle → 送出 → conversing → reset → idle 的轉換測試。

---

## Phase 2 — Prompt Bar 進階控制(對齊「標準 / 連接器 / 對話」)  🟡

### P2.1 Agent / 模型選擇器(對應「標準 ▾」)
- [x] 🔴 `web/src/components/workspace/AgentModelPicker.tsx`:下拉選 agent(來源 `agents-store`)。
- [x] 🔴 切 agent → 通知 chat 重新繫結 session。**先確認** `/ws/chat` 是否支援指定 agent;若否,於 `chat-store` 增 `selectAgent(id)`(送既有 `session_info` 協定可接受的欄位,**不**改協定形狀)。⚠️ 若後端僅綁預設 agent,本項降級為「唯讀顯示當前 agent + model」,改 agent 走 `/agents`(於文件註記取捨)。
- [x] 🟡 顯示當前 `model`(已在 `session_info`)。
- [x] 🧪 picker 顯示當前 agent + model、開選單、Manage 導向 `/agents`(實作為降級路徑:後端 `/ws/chat` 綁預設 agent,故唯讀顯示 + 管理深連,而非即時 rebind)。

### P2.2 連接器狀態(對應「連接器」)
- [x] 🟡 `web/src/components/workspace/ConnectorChips.tsx`:顯示已啟用整合(Channels / MCP / Odoo / Inference)狀態膠囊,點擊深連到對應頁(`/channels` `/mcp` `/odoo` `/inference`)。
- [x] 🟡 資料來源複用既有 store(`mcp-store` / channels / odoo);**唯讀**,不在此做設定。
- [x] 🧪 chips 依可用整合渲染、`minRole`/`enterprise` 隱藏一致於 nav-model。

### P2.3 語音入口(對應「對話」)— 視 feature 14 狀態
- [x] 🟡 若 voice pipeline 已上線:加白色 pill「語音」按鈕,連到語音流程;否則渲染 `coming-soon` 不可點 + tooltip。
- [x] 📝 在本檔註記 voice 後端就緒度,避免畫死 UI。

---

## Phase 3 — 能力啟動網格(Launcher Grid)  🔴

### P3.1 啟動卡資料模型
- [x] 🔴 新檔 `web/src/components/workspace/launcher-model.ts`:依 §1 對照表定義 `LauncherCard[]`(分組:AI 員工 / 建構 / 內容創作 / 工具 / 整合)。型別含 `status: 'ready'|'coming-soon'`、`minRole?`、`enterprise?`。
- [x] 🔴 過濾複用 `hasMinRole` + `isPersonal && enterprise`(與 `Sidebar` 同邏輯,抽 `lib/nav-visibility.ts` 共用)。
- [x] 🧪 過濾測試:manager/admin/personal 三情境卡片可見性。

### P3.2 LauncherCard / LauncherGrid 組件
- [x] 🔴🎨 `web/src/components/workspace/LauncherCard.tsx`:`panel-hover` 卡 + lucide 圖示 + 標題 + 一句說明;`coming-soon` 灰階不可點 + Badge。
- [x] 🔴🎨 `web/src/components/workspace/LauncherGrid.tsx`:分組標題 + responsive grid(行動 2 欄 / 桌面 4–5 欄),呼應 Genspark 分組橫排。
- [x] 🎨 圖示彩色強調:在 `index.css` 加一組受控的 launcher accent token(**有限**色板,遵守 DESIGN.md「amber 為主、避免彩虹」——彩色僅用於 launcher 圖示,其餘維持 amber)。📝 於 DESIGN.md §2 補一行說明此例外。
- [x] 🧪 卡片點擊導航、`coming-soon` 不導航、鍵盤可達(focus ring)。

---

## Phase 4 — Claw:您的第一位 AI 員工  🟡

> 對應 Genspark Claw 著陸區。DuDuClaw 名稱本就含「Claw」—— 把預設個人 agent 品牌化為「Claw」。

### P4.1 Claw Hero 區塊
- [x] 🟡🎨 `web/src/components/workspace/ClawHero.tsx`:標題「Claw — 您的第一位 AI 員工」+ 四張價值卡(i18n):
  - 「您自己的機器,隱私至上」← **自架單機**(對比 Genspark 雲端,這是我們的真差異)
  - 「了解您,記住一切」← cognitive memory(`/memory`)
  - 「在 LINE / Telegram / Discord 完成任務」← channels(`/channels`)
  - 「內建超能力」← skills / inference / odoo / browser
- [x] 🎨 四卡用 `panel`,點擊深連到對應頁。
- [x] 📝 文案刻意對比 Genspark:強調「跑在你自己的機器、資料不出機、無 credit 焦慮、多 runtime 不鎖定」。

### P4.2 Claw 入口接線
- [x] 🟡 「立即開始」CTA → 聚焦 PromptBar(`idle` 對話);「深入設定」→ `/agents`。
- [x] 🟡 首次無 agent 時,銜接既有 `FirstRunGate` / `WelcomePage`,不繞過 onboarding。
- [x] 🧪 CTA 行為測試。

---

## Phase 5 — 模式切換 UX 與外殼整合  🔴

### P5.1 模式切換控制
- [x] 🔴🎨 `web/src/components/layout/ModeToggle.tsx`:簡易 ⇄ 進階 segmented control。
- [x] 🔴 置於 `Header.tsx`(右上,接近主題切換);寫入 `ui-mode-store`。
- [x] 🧪 切換更新 store + 路由表現一致。

### P5.2 簡易模式收斂側邊欄(對齊 Genspark 窄圖示欄)
- [x] 🟡🎨 `Sidebar.tsx`:`workspace` 模式時改窄圖示欄(首頁 / 技能 / Claw / 工作流程 / 雲端硬碟 / 更多),`dashboard` 模式維持現有 6 群完整 nav。**用既有 `navGroups` 子集**對應,不另造資料源。
- [x] 🟡 `MainLayout.tsx`:依模式切換側邊欄寬度與內距(workspace 全幅、dashboard 現狀)。
- [x] 🧪 兩模式側邊欄項目正確、角色過濾仍生效。

### P5.3 互通與深連
- [x] 🟡 啟動卡 / 連接器 chip 點擊 → 進入 dashboard 模式對應頁(切 `ui-mode` 並導航),回上一頁可返 workspace。
- [x] 🧪 深連後模式狀態正確。

---

## Phase 6 — i18n · a11y · 驗收  🔴🧪

### P6.1 i18n(三語齊備)
- [x] 🔴📝 新增所有 workspace 鍵到 **三本** catalogue:`web/src/i18n/zh-TW.json`(SoT)、`en.json`、`ja-JP.json`。命名空間 `workspace.*` / `launcher.*` / `claw.*`。
- [x] ⚠️ 依 DESIGN.md §7b:平行子代理**不可**同時改 `i18n/`;i18n 由單一提交統一加。

### P6.2 a11y / 韌性(`audit` / `critique`)
- [x] 🔴 PromptBar `aria-label`、focus-visible ring;啟動卡鍵盤可達 + role；對話流 `aria-live`。
- [x] 🟡 `prefers-reduced-motion` / `prefers-reduced-transparency` 沿用既有;深色對比 WCAG AA。
- [x] 🟡 空 / 載入 / 錯誤狀態(無 agent、WS 斷線、整合未設定)皆有 `EmptyState`。

### P6.3 驗收門檻
- [x] 🔴🧪 `cd web && npx tsc -b` 綠;`npx vitest run` 全綠(含新測試)。
- [x] 🔴🧪 `npm run build` 成功;workspace 為獨立 route chunk(沿用 lazy 切分)。
- [~] 📝 手測腳本:個人版首次 → 落在 workspace;送一句 → 進對話;切進階 → 完整儀表板;重整 → 模式記憶留存。(邏輯均有單元測試覆蓋;**瀏覽器實機走查待執行**——此環境無瀏覽器)
- [ ] 📝 截圖 light / dark 兩主題,對照 Genspark 4.0 做 critique。(**阻塞:需 GUI/瀏覽器**)

---

## 風險與競品註記  ⚠️📝

- **Genspark Claw 已搶佔「隱私 / AI 員工 / 跑在你的機器」定位**(`您自己的雲端電腦,隱私至上`)。但其「雲端電腦」仍是 **Genspark 託管沙箱**;DuDuClaw 是**真・自架單一 binary**——文案務必把差異點到位:**「不是我們幫你開一台雲端電腦,而是它就跑在你自己的機器上,資料從不離開」**。這是唯一守得住的差異,Phase 4 文案是戰略要點,不只是 UI。
- **不要把 workspace 做成「結果工廠」的半套**:辦公套件(簡報/影片)本期一律 `coming-soon`,寧缺勿濫,避免和 Genspark 的雲端規模硬碰(那是方案 C 的賭注,目前不建議)。
- **協定零變更是硬約束**:任何需要改 `/ws/chat` 或後端 RPC 的工項(P2.1 切 agent)必須先驗證後端能力,不能就降級——避免外殼工程倒逼核心改動。
- **模式切換不可造成既有使用者困惑**:企業版 / 已有偏好者預設仍是 dashboard(P0.2),workspace 是「新增選項」不是「取代」。

---

## Phase D — 桌面應用打包(Tauri sidecar)  🟡

> 對應 Genspark Claw 頁面的「下載桌面應用」。策略:**Tauri 2 + 既有 `duduclaw` binary 當 sidecar**,
> 核心程式碼**零改動**,WebView 指向 `localhost:<port>`,工作空間外殼直接跑在原生視窗內。
> ⚠️ 注意:既有 `crates/duduclaw-desktop` 是 Computer Use(enigo/xcap)控制層,**不是** App 殼,勿混用。
> 真正的工時在**生命週期管理(D2)**與**簽章 / 公證 / 發佈(D4)**,不在 UI。

> **狀態說明(本批次)**:此環境**無 Tauri 工具鏈、無顯示器、無簽章憑證**,故桌面 App
> 無法 `tauri build` / `dev` 實際驗證。標記語意:
> `[x]` 已寫且**已驗證**(純邏輯 `rustc --test` 通過 / 設定檔語法驗證 / 文件完成);
> `[~]` 程式碼/設定**已寫完但未經 build 驗證**(待工具鏈);
> `[ ]` **阻塞**(需 Tauri 工具鏈執行 / Apple·Windows 憑證 / 乾淨機器),尚未完成。
> 已交付檔案:`src-tauri/`(`tauri.conf.json`、`Cargo.toml`、`build.rs`、`src/{main,sidecar,lifecycle}.rs`、
> `capabilities/default.json`、`entitlements.plist`、`icons/`、`binaries/`、`.gitignore`)、
> `scripts/desktop/*`、`.github/workflows/desktop-release.yml`、`docs/guides/desktop-{build,release}.md`。
> `lifecycle.rs` 純邏輯已 `rustc --test` 編譯 + 6 測試全綠;`tauri.conf.json`/`capabilities` JSON 與 CI YAML 已通過語法檢查。
>
> 📘 **解除阻塞的完整步驟**(工具鏈 / Apple·Windows 憑證 / Linux / 自動更新):見
> [docs/guides/desktop-unblock.md](../guides/desktop-unblock.md) —— 每個 `[ ]`/`[~]` 項都有對應的關卡、指令與驗收方式。

### D0 骨架(本機自用,不簽章)  🔴
- [~] 🔴 新目錄 `src-tauri/`(Tauri 2):`tauri.conf.json`、`Cargo.toml`、`build.rs`、`src/main.rs`。**Release**:WebView 在 setup 後 `navigate` 到 `http://127.0.0.1:<port>`(gateway embedded dist)。**Dev(debug build)**:WebView 停在 Vite `devUrl :5173`(live 工作空間 + HMR,proxy `/ws` `/api` 到 gateway),`#[cfg(debug_assertions)]` 不 navigate —— 避免 embedded(編譯時)dist 蓋掉本機改動;sidecar 仍 spawn 提供後端。
- [~] 🔴 sidecar 設定:release `duduclaw` 以 `externalBin: ["binaries/duduclaw"]` 註冊;App 啟動 → spawn `duduclaw run --yes`(完整 server = gateway + dashboard + channels;**非** `gateway`,也**沒有** `start` 子命令 —— 後者是 launchd 時代的筆誤,已修),退出 → `stop()` 終止。
- [~] 🟡 視窗:預設 1280×840 / 最小 960×640、置中、深色 `#1c1917` 背景、`visible:false` 直到 ready 才 `show()`(防白閃)。
- [x] 📝 `docs/guides/desktop-build.md`:本機 `tauri dev` / `tauri build` / 圖示生成 / 生命週期說明。
- [x] 📝 `scripts/desktop/gen-icons.sh`:`cargo tauri icon` 主路徑 + macOS `sips`/`iconutil` 降級(產 PNG + `.icns`;`.ico` 仍需 Tauri CLI/ImageMagick,腳本明確告警)。**仍缺品牌 raster 來源** `web/public/paw-1024.png`(需提供 ≥1024² 方形 PNG)。
- [ ] 🧪 冒煙:`tauri dev` 起得來、WebView 顯示登入頁、可送一句 chat(**阻塞:無工具鏈/顯示器**)。

### D1 與既有啟動方式互斥  🔴
- [~] 🔴 偵測既有 launchd / CLI gateway(`lifecycle::plan_gateway` → `is_listening` port 探測);**已在跑 → `Attach` 不重啟**,否則 `Spawn`。避免雙實例。
- [x] 🔴 自啟 sidecar vs 附掛既有 gateway:**自動判定**(已在跑則附掛)+ `DUDUCLAW_DESKTOP_MODE=auto|attach|spawn` 環境變數**手動覆寫**(`lifecycle::desktop_mode` / `decide_plan`,`rustc --test` 驗證;設定面板可後續只寫此變數)。
- [x] 📝 於 `docs/guides/desktop-build.md`「Relationship to launchd」說明取代 / 並存。
- [ ] 🧪 兩情境端到端(無既有 → 自啟;有既有 → 附掛)(**阻塞:需執行**)。

---

### D2 生命週期管理(核心工時)  🔴

#### D2.1 單一實例鎖
- [~] 🔴 `tauri-plugin-single-instance`:第二次開 App → `show_main_window` 聚焦既有視窗。
- [~] 🔴 sidecar 啟動前跨進程回收:`reclaim_orphan()` 讀 pidfile → SIGTERM/taskkill;`sidecar_pidfile` 路徑單元測試通過。
- [ ] 🧪 連點兩次 App → 僅一進程一視窗(**阻塞:需執行**)。

#### D2.2 Port 衝突處理
- [x] 🔴 port 選擇邏輯:`configured_port` + `candidate_ports`(18789..=18797)+ `plan_gateway` 擇空 port;`rustc --test` 已驗證(4 測試)。實際 port 在 setup 後傳給 WebView `navigate`(不寫死)。
- [x] 🟡 port 來源:`DUDUCLAW_PORT` 環境變數 > **`config.toml [gateway] port`** > 預設 18789(`lifecycle::configured_port` / `resolve_preferred_port_from` / `config_port_from_str`,依賴無關的 line scanner,fail-safe)。**雙實例修正**:attach 偵測掃描所有已知 port(env/config/default),非預設 config port 不再誤開第二個 gateway。
- [x] 🧪 `candidate_ports` / `configured_port` / `resolve_preferred_port_from` / `config_port_from_str` / `known_ports_from` / `decide_plan`(attach-vs-spawn 全矩陣,可注入 liveness probe)單元測試(含 u16 溢位、非法值、雙實例情境 fail-safe)已通過 — `rustc --test` 14 測試全綠。

#### D2.3 優雅關閉
- [~] 🔴 `RunEvent::ExitRequested` + tray quit → `SidecarManager::stop()` → `child.kill()`(gateway 收訊號 flush);**附掛模式不殺**外部 gateway。
- [~] 🔴 孤兒回收:pidfile 寫於 spawn、清於 terminate;啟動時 `reclaim_orphan` 回收前次殘留。
- [ ] 🧪 正常退出無殘留 / 崩潰後重啟回收(**阻塞:需執行**)。

#### D2.4 託盤(Tray)— Genspark 風格常駐
- [~] 🟡🎨 託盤圖示 + `tray_status_label`:running(含 port)/ stopped / error。
- [~] 🟡 託盤選單:開啟 DuDuClaw、重啟背景服務、狀態列、結束。
- [~] 🟡 「關窗不退出」:`WindowEvent::CloseRequested` → `prevent_close` + `hide`。
- [ ] 🧪 狀態反映 / Start-Stop 驅動 sidecar(**阻塞:需執行**)。

#### D2.5 健康監控與自動重啟
- [~] 🟡 readiness:`wait_until_ready`(輪詢 `is_listening`);非預期 `Terminated` → `restart_with_backoff`(指數退避)。
- [~] 🟡 連續失敗 ≤5 次 → 進入 `Error` 態 + `notify_sidecar_failure` 原生通知,不無限重試。
- [ ] 🧪 殺 sidecar → 自動重啟;連續失敗 → error 態(**阻塞:需執行**)。

#### D2.6 子進程與 PATH(沿用既有解法)
- [~] 🔴 sidecar 以 `env("PATH", augmented_path())` spawn,涵蓋 Homebrew(Intel+ARM)/Bun/Volta/npm-global/asdf/cargo/.local/bin,對齊 `which_claude_in_home()` 精神。
- [x] 🟡 `augmented_path` / `extra_path_dirs` 去重邏輯 `rustc --test` 已驗證。
- [ ] 🧪 Finder/Dock 啟動下子進程可被發現(**阻塞:需執行**)。

#### D2.7 設定 / 資料目錄一致
- [~] 🔴 不設獨立 home:sidecar 繼承預設 `~/.duduclaw`(`DUDUCLAW_HOME` 可覆寫);與 CLI 共用 config/SQLite/wiki/jsonl。
- [x] 🧪 `duduclaw_home` / `sidecar_pidfile` 落在 `~/.duduclaw` 已單元驗證。

---

### D3 系統整合與權限(macOS 優先)  🟡

#### D3.1 Hardened Runtime entitlements
- [~] 🔴 `bundle.macOS.hardenedRuntime:true` + `entitlements.plist`(allow-jit / unsigned-exec-mem / disable-library-validation / network.client+server / dyld-env / apple-events)。
- [ ] 🧪 簽章 + hardened 後 sidecar 仍能 spawn / 連網(**阻塞:需簽章 + 執行**)。

#### D3.2 Computer Use 系統授權(加分項)
- [~] 🟡 entitlements 含 `apple-events`;Computer Use 走既有 `duduclaw-desktop`(enigo/xcap)。
- [x] 📝 授權引導寫入 `docs/guides/desktop-build.md` / `desktop-release.md`(簽章 App 才能正確觸發系統對話框)。
- [ ] 🧪 簽章 App 內截圖 / 模擬輸入(**阻塞:需簽章 + GUI**)。

#### D3.3 開機自啟 / 通知(可選)
- [~] 🟡 `tauri-plugin-autostart`(LaunchAgent)已掛載,預設關閉,capability 開放 enable/disable/is-enabled。
- [~] 🟡 `tauri-plugin-notification` 已掛載;失敗告警用原生通知。

---

### D4 簽章 · 公證 · 發佈(發佈門檻,八成工時在此)  🔴📝

#### D4.1 macOS 簽章 + 公證  ✅ 已驗證(2026-07-01,tag `desktop-v1.31.0`)
- [x] 📝 `scripts/desktop/sign-notarize-macos.sh`(codesign hardened + `notarytool` + `stapler`)+ CI `apple-actions/import-codesign-certs` 注入,憑證僅走 secrets。
- [x] 🔴 Developer ID 憑證(`Dudu Technology Ltd. 7469HYQ6HH`,到 2031-03)+ 6 個 Apple CI secrets 已設;CI `desktop-release.yml` 對 mac arm64/x86_64 **自動簽章 + 公證 + staple 成功**。
- [x] 🧪 乾淨機驗收:實測下載的 `DuDuClaw_1.31.0_aarch64.dmg` → `codesign` = Developer ID + hardened runtime;`stapler validate` = *The validate action worked!*;`spctl -a` = **accepted, source=Notarized Developer ID**(即從未裝過此憑證的 Mac 也不被 Gatekeeper 攔)。

#### D4.2 Windows 簽章
- [x] 📝 `scripts/desktop/sign-windows.ps1`(signtool SHA256 + timestamp)+ CI 步驟。
- [ ] 🟡 取得 **Authenticode 憑證**並簽 `.msi`(**阻塞:需你提供憑證**)。
- [ ] 🧪 SmartScreen 不擋(**阻塞**)。

#### D4.3 Linux 打包
- [x] 🟡 `bundle.targets` 含 `appimage`/`deb`;CI 安裝 WebKitGTK 依賴 —— `desktop-v1.31.0` run 產出 `DuDuClaw_1.31.0_amd64.AppImage` + `.deb`。
- [ ] 🧪 主流發行版可執行(**待在 Linux 上實跑驗證**)。

#### D4.4 自動更新
- [~] 🔴 `tauri-plugin-updater` + `plugins.updater`(endpoint=GitHub latest.json、`createUpdaterArtifacts:true`);`pubkey` 待生成填入。
- [x] 📝 更新流程 / 版本一致性 / 簽章驗證寫入 `docs/guides/desktop-release.md`。
- [ ] 🧪 端到端更新 + 驗章拒絕不符(**阻塞:需簽章金鑰 + 發佈**)。

#### D4.5 發佈管線(CI)  ✅ 已跑通(`desktop-v1.31.0`,4/4 leg 綠)
- [x] 🔴📝 `.github/workflows/desktop-release.yml`:四 target matrix(mac arm/intel、win、linux)、build sidecar、tauri-action、Windows 簽章步驟 —— **實跑成功**,7 個產物上傳 draft release。
- [x] 🟡 版本對齊:`tauri.conf.json` version 與 workspace `1.31.0` 一致。
- [x] 📝 `docs/guides/desktop-release.md`:金鑰生成、憑證輪替、release checklist。

---

### D5 驗收門檻  🔴🧪
- [x] 🔴🧪 本機 `cargo tauri dev` 起得來、自動 spawn gateway(`run --yes`)、WebView 停 Vite、登入成功、工作空間可切換(2026-07-01 實跑)。`cargo tauri build` 產出簽章公證的 `.app`/`.dmg`。
- [x] 🔴🧪 生命週期**純邏輯**驗收:單實例回收路徑、port 優先序(env>config>default)、雙實例防護、PATH 增強、home/pidfile 路徑 —— `rustc --test` **14 測試全綠**。
- [x] 🔴 簽章後乾淨機**無 Gatekeeper 攔截**(mac 已驗:`spctl -a` = accepted / Notarized Developer ID)。Windows SmartScreen 待補 Authenticode 憑證。
- [ ] 🔴 自動更新端到端通過(updater 目前關閉;需金鑰 + 發佈,見關卡 E)。
- [ ] 📝 截圖桌面 App(light/dark)對照 Genspark 做 critique(**阻塞:需 GUI build**)。

---

## Phase D 風險註記  ⚠️📝
- **真正成本是 ops 不是 code**:D4(簽章/公證/CI)佔八成工時;建議 **D0–D2 先做不簽章本機 MVP** 驗證體驗,確認生命週期順暢再投入 D4。
- **雙實例是最大陷阱**:launchd + 桌面 App 同時拉 gateway 會搶 port / SQLite。D1 + D2.1 + D2.2 必須一起完成,缺一不可。
- **核心零改動是硬約束**:桌面殼只能透過既有 HTTP/WS/sidecar 互動;任何「為了打包去改 gateway」的需求都要先擋下,維持 binary 可獨立運行。
- **隱私定位一致性**:桌面 App 強化「跑在你自己的機器」訊息(對比 Genspark 雲端沙箱),與 Phase 4 Claw 文案同調。

---

## 建議交付順序

1. **Phase 0 → 1**(骨架 + Hero + PromptBar 複用 chat):最小可用,能送訊息即價值落地。
2. **Phase 3**(啟動網格):視覺上「像 Genspark」的關鍵。
3. **Phase 4**(Claw 定位 + 文案):戰略差異化,可與 3 平行。
4. **Phase 2 / 5**(進階控制 + 模式切換):打磨。
5. **Phase 6** 全程隨附,最後統一收 i18n 與驗收。
6. **Phase D**(桌面打包):可與 Web 外殼平行;先做 **D0–D2 不簽章本機 MVP** 驗證體驗與生命週期,**D3–D4(權限/簽章/公證/自動更新)** 留待要對外發佈時再投入。
