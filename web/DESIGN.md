# DuDuClaw Dashboard — Design System (Soft Play v2「嘟嘟事務所」)

> Single source of truth for the v2 dashboard. Every page MUST follow this.
> v2 supersedes the v1 "Calm Glass" console (kept only as colour DNA + engineering
> discipline). Design authority: `commercial/docs/TODO-dashboard-redesign-v2-2026-07-10.md`
> (Fable, 2026-07-10). zh-TW first; en / ja-JP parity mandatory.
>
> **v2 thesis (one line):** *paperclip 的骨架 × openhuman 的血肉 — 「可愛、遊戲化」是
> 全介面的血液，不是某一頁的貼紙。* Every AI staff member is a **character with a face** on
> every screen; every completed action gives emotional feedback; the company's growth is
> visible, computable, and celebrated — all from **real data, never faked**.

## 1. Design intent (理解設計意圖)

心智模型：**一間 AI 員工的事務所**。每個畫面回答三問 — ① 現在發生什麼事 → ② 有沒有需要我拍板
→ ③ 我該怎麼做 — 但 v2 用**角色與世界演出來**（掃一眼就懂），用 **paperclip 面板做下去**（快而準）。

| Pillar | v1 Calm Glass | v2 Soft Play |
| --- | --- | --- |
| Surface | flat hairline `panel`, no shadow | **soft card**: `rounded-2xl` + `shadow-soft` 軟浮（暖白），glass 仍只給 chrome/overlay |
| Identity | agent = 文字列 + 狀態點 | agent = **程序化向量角色**（`CharacterAvatar`），一套資料三種呈現，全站唯一視覺身分 |
| Mascot | emoji 字元（作廢） | **DuDu** 純 SVG 角色（13 表情 + 眨眼 + viseme），跨路由陪伴 |
| State | 靜態卡片 | **世界舞台**（等距辦公室，state-driven）把狀態具象化，並入首頁 |
| Reward | 無 | **遊戲化四件套**：XP/等級、成就牆、日報卡、慶祝時刻（全真數據） |
| Layout | 兩欄（左導航/中內容） | **三欄殼**：左 Sidebar / 中內容 / **右 PropertiesPanel（PanelContext）** |
| Radius/Motion | `rounded-xl`, 冷靜 | radius 升一級（card 2xl / bubble 3xl）+ pop/spring/celebrate motion，全走 token |

**保留（色彩與品牌 DNA）**：stone 中性 ramp、amber 稀缺 accent、semantic（emerald/amber/rose/sky）、
🐾 品牌、lucide 18px、深淺主題跟系統、三語 i18n、`tabular-nums`、**token-only 紀律（禁 hardcode hex/px）**。

**Anti-goals**：emoji 當角色、遊戲化造假（前端估算 XP）、破壞 prompt-cache 的每輪抖動、
把世界舞台當唯一入口（所有物件動作必有 Sidebar/⌘K 等價路徑）、彩虹 accent、內容區重投影。

## 2. Tokens (Soft Play 層，`src/index.css`)

只加不減。Use utilities/vars, never hardcode hex or px.

- **Radius**：`--radius-card: 1rem`（2xl）、`--radius-control: 0.75rem`、`--radius-bubble: 1.5rem`。
- **Shadow**：`--shadow-soft`（`0 2px 8px /4%, 0 8px 24px /6%` 雙層）、`--shadow-pop`（hover 抬升）；
  dark 模式用 `--ring-soft` 補償（避免暗底陰影糊）。`.panel` utility 已轉軟卡呈現。
- **Motion**：`--motion-pop: 180ms cubic-bezier(.34,1.56,.64,1)`（過衝彈）、`--motion-fade: 200ms ease-out`；
  keyframes `pop-in / fade-up / badge-pop / confetti-fall / dudu-hop / character-blink|work|wave`
  **全部 gate 在 `@media (prefers-reduced-motion: no-preference)`**；另有全域 reduce 斷路器
  （`* { animation-duration:.01ms; transition-duration:.01ms }`）。
- **Status tokens（paperclip P6 系統化）**：`--status-agent-{idle,running,paused,error}`（4）
  + `--status-task-{backlog,todo,in_progress,in_review,done,blocked,cancelled}`（7）
  + 每個對應 `--status-*-icon-*` **AA 對比變體**（OKLCH 實算 ≥4.5:1，雙主題）。
  badge / row / chart / 世界 emote **同一套**。
- **Agent 漸層**：`--agent-{1..10}{a,b}` 10 對暖調漸層（amber/coral/sage/sky/violet… 家族）——
  角色 avatar 底、org 節點、世界 sprite tint **同源**。
- **遊戲化 hue**：`--xp`（amber 家族）、`--coin`（amber-600）。
- **Neutral / Accent / Semantic**（沿用 v1）：stone ramp；amber 稀缺 accent；emerald/amber/rose/sky。
- **Type**：page title `text-2xl font-semibold tracking-tight`、section `text-sm font-semibold`、
  body `text-sm`、meta `text-xs`；機器值一律 `<Mono>` + `tabular-nums`。

## 3. Surfaces

| Utility | Use | Recipe |
| --- | --- | --- |
| `panel` | **Content cards**（default） | 暖白 near-opaque fill + `shadow-soft`（dark 用 ring） + `rounded-card` |
| `panel-hover` | clickable cards | hover 抬升至 `--shadow-pop` + border-brighten |
| `glass-chrome` | sidebar / header | 重 blur（仍保留給 chrome） |
| `glass-overlay` | dialogs / menus / popovers / right Sheet | 強 fill + blur |

## 4. 角色系統（v2 最重要新資產 —「可愛」的載體）

**每位 AI 員工 = 一個 seeded-by-id 的向量角色**（`components/character/`）。純 SVG + CSS，無新依賴，
theme-agnostic（agent 漸層 token 雙主題可讀）。

- `character-gen.ts`：`characterFor(agentId)` 純函數 → `{ tintIndex(1..10), accessory, blinkSeedMs }`
  （antenna 權重最高，openhuman 同款邏輯）。
- `CharacterAvatar`（`components/character/CharacterAvatar.tsx`）三種呈現，一套資料：

| variant | 觸發 | 用途 | 形態 |
| --- | --- | --- | --- |
| `avatar` | `size < 64` | 列表列 / assignee / 留言 / 活動流 / Sidebar | 圓臉 + tint + 配件 + 眨眼 |
| `bust` | `size ≥ 64` 或 `variant="bust"` | 員工卡 / 詳情 hero / org 節點 | 半身 + 姿勢驅動手臂 |
| `sprite` | 世界舞台 | 等距角色（PixiJS 貼圖，tint 同源） | 見 §7 |

- **poses**（`poses.ts`）：`idle / working / blocked / sleeping / celebrating / waving`——bust 全身反映，
  avatar 只動眼/嘴。
- **StatusEmote 頭頂表情泡**：`working 💻 / blocked ⚠️ / awaiting-approval ✋ / sleeping 💤 / error 😵 /
  celebrating 🎉`，語彙與 `--status-agent-*` 同源。
- **`animated` prop**（預設 true）：關掉即渲染靜態終態；眨眼/微動一律 gate 在 reduced-motion 之外。
- **a11y**：wrapper `role="img"` + `aria-label`（name→id fallback），內層 SVG `aria-hidden`。
- **紀律**：**所有出現 agent 名字的地方一律帶 avatar**；v1 的 `AgentStatusGlyph`（文字+點）降為 fallback。

## 5. DuDu 吉祥物（`components/mascot/`）

- 純 SVG React 元件（`DuDu.tsx`），無 Rive/Lottie，可版控可 diff。造型：圓潤爪爪獸（🐾 具象化），amber 主色。
- **13 表情**（`faces.ts`）：`sleep / idle / listening / thinking / speaking / happy / concerned /
  curious / proud / celebrating / writing / reading / waving`。
- **API**：`<DuDu face viseme size animated label />`；`speaking` 由 **viseme**（`visemes.ts`，7 shape +
  `lerpViseme`）驅動嘴型；眨眼 clock（`useDuduClock`，thinking 4.2s / 其他 2.6s）。
- **臉的來源**：`stores/mascot-store.ts` `useDuduFace()` 是 roster+inbox+連線 的**純衍生**
  （`lib/mascot-mood.ts` 邏輯）；**唯一例外 = transient face**（`useMascotTransientStore.setTransientFace(face, ms)`）：
  成長時刻（`growthEventBus` achievement_unlocked / level_up）推 `proud` 4s 後回衍生值。
  a11y：wrapper `role="img"` + `aria-label`；狀態承載時給 label，純裝飾時 `aria-hidden`。
- **出現點規範**：對話頁置中主舞台（idle→waving→listening→thinking→speaking→happy 生命週期）／
  首頁問候旁 sm 版／**每個 `EmptyState` 配 pose**（空任務=掃把、空知識=抱書、搜尋無果=聳肩）／
  錯誤與斷線（`IncidentBanner` concerned）／onboarding 接待員（welcome/wizard/GuidedTour）／
  Inbox Zero celebrating。
- **Tauri 桌寵**（`/mascot-overlay`）：第二 window（transparent/always-on-top/skip-taskbar），
  與 web 版**共用 store/表情引擎/SVG**；已知限制：overlay 在 AuthGuard 外，pending 讀不到則靜態 sleep。

## 6. 三欄殼與右欄 PanelContext

`components/layout/` — 左 Sidebar（可收合成 icon rail）｜中 BreadcrumbBar + `<Outlet>`｜
**右 PropertiesPanel 320px**。CommandPalette、Toast、CelebrationLayer 掛根。

- **PanelContext**（`ui/PropertiesPanel` + `PanelProvider`，hook `usePanel()`）：任何頁面用
  `panel.setPanel({ title, content })` + `panel.setSheetOpen(true)` 注入右欄；`panel.clearPanel()` 收合。
  桌機為固定右欄（偏好記憶）；**行動版轉底部 Sheet**（glass-overlay）。
  典型消費者：`/tasks/:id` IssueProperties、`/inbox` ApprovalDetailPanel、org 節點員工卡。
- **Sidebar**（§4.2 設計）：公司牌（名稱 + Lv.N）→ ＋交辦任務（primary）→ 首頁/收件匣(badge)/對話 →
  工作分節 → **員工活體區**（有 live run 只列在跑的，avatar+live dot+run 數；否則近期 3）→ 公司分節。
  非 mgr 整節依 `lib/nav-visibility.ts` 隱藏。
- **Header/HUD**：常駐 **CoinChip**（今日成本，點→帳務）+ **XP 膠囊**（Lv + 進度，點→/growth，
  `levelUpNonce` 觸發 badge-pop）+ 鈴鐺（inbox badge）+ ⌘K + 主題切換。
- **MobileBottomNav** 5 格：首頁／收件匣／**＋交辦（中央大鈕）**／任務／對話。
- **全站慣例**：機器值 `<Mono>` + `lib/format.ts` formatter（XP/coin/duration）；breadcrumb 貫穿；
  列表偏好 localStorage（key 前綴 `duduclaw:ui:*` / `duduclaw:home:*` / `duduclaw:tasks:*`）；
  多分頁單 leader 輪詢（`hooks/useSharedLeaderQuery`）。

### 6.1 新 primitives（`ui/` barrel）

`PropertiesPanel`+`PanelProvider` / `PropertyRow`+`PropertySection` / `InlineEditor` / `StatusIcon`（可點改狀態）/
`PriorityIcon` / `LiveBadge` / `SpeechBubble` / `CoinChip` / `XpBar` / `AchievementBadge` /
`CelebrationLayer`+`celebrate()`（全域 confetti/彩帶 portal，reduced-motion 無粒子只留 toast）/
`SwipeToArchive` / `GroupHeader`。沿用：`Page/PageHeader/Card/StatCard/Tabs/Button/Badge/EmptyState/Skeleton/Toolbar/Field/Mono`。

## 7. 世界舞台（首頁心臟，`components/world/`；降級鏈是重點）

- **技術**：PixiJS，`Application.init({ preference:'webgl' })`——**禁 WebGPU**（嵌入殼 adapter hang 死坑）；
  init **10s timeout** + 錯誤面板 + 重試。600×600 原生場景等比縮放。palette 從 CSS token 讀值，主題即時重建。
- **資料驅動**（openhuman state-driven）：控制器（`useWorldState`）訂閱 `agents.status` + `activity.subscribe`
  推 `AgentState{x,y,action,facing,say}`；渲染器（`stage-scene`）只平滑逼近，**不含業務邏輯**。
  泡泡用 `Intl.Segmenter` CJK-safe 截斷，8s 淡出。
- **可點物件**：員工→右欄員工卡；佈告欄(inbox badge)→/inbox；白板→/tasks；大門→通道狀態；金庫[mgr]→帳務。
- **降級鏈（`resolveStageMode` 純函數，已測）**：
  `reduced-motion` **或** WebGL 不可用 **或** 使用者切「⊞ 清單」 → **靜態插畫 + 角色頭像格**
  （`WorldStageStatic`，同資料、無 canvas）；**行動版預設 static**（省電，可覆蓋）；toggle 記憶
  `duduclaw:home:stage-mode`，`stagePossible` 才顯示切換鈕。**世界永遠是增強，不是唯一入口。**
- **a11y**：舞台容器 `role="img"` + `aria-label`（`home.stage.aria`）；靜態版同樣具名。

## 8. 遊戲化紀律（全部真數據，禁止造假）

**核心原則：XP/等級/成就是「既有事實的計分呈現」，不影響任何 agent 行為；判定是純函數、重算永遠可重現。**

- **持久層在 gateway**（拍板 ③=B）：`growth.db`（SQLite/WAL）+ RPC `growth.snapshot` / `growth.daily_report`；
  XP/成就判定引擎在後端（讀 analytics/tasks/skills/wiki/reliability 既有真實資料），單一權威、跨裝置一致、
  時間戳可稽核。**前端只讀不算**：`stores/growth-store.ts` 快取 snapshot 並 **diff 前後** 只在真轉態時
  觸發 §8.1 慶祝一次（首個 snapshot 永不慶祝，避免 firehose）。RPC 不可用 → 顯示「成長資料暫不可用」，
  不做前端估算。
- **XP 來源**：完成任務 +12／技能習得 +25／知識頁 +8／例行連續 +5／成就一次性。等級 `Lv=floor(sqrt(XP/100))`。
- **成就牆**（`components/growth/achievements-def.ts`，i18n key）：id 對映後端（first_agent / first_task_done /
  tasks_100 / knowledge_100 / skills_10 / inbox_zero_streak_7 / custom_skill_first / custom_skill_saved_100h …）。
  `available:false` 的成就（無每日快照 / 無 per-skill 計數）顯示「暫不可用」**而非假 0 進度**。
- **員工個人成長**：員工 Lv（完成數+技能推導，前端**明標推算基準**）、徽章=真技能、心情=prediction/mistake 訊號。
- **日報結算卡**（`DailyReportCard`）：每日首開一次，讀 `growth.daily_report`。
- **慶祝時刻總表**（`CelebrationLayer` 接點；全部尊重 reduced-motion → 降級為靜態 toast）：

| 時刻 | 效果 |
| --- | --- |
| 任務完成 | confetti 小 + 角色 celebrating + `+12 XP` 浮出 |
| Inbox Zero | 彩帶 + DuDu celebrating + 稱讚 copy |
| 成就解鎖 | badge-pop + **DuDu proud（transient face 4s）** + toast |
| 員工入職 | 世界大門走入 + 歡迎泡泡 |
| 升級 | XP 膠囊 badge-pop（`levelUpNonce`）+ DuDu proud |

## 9. 資訊架構（v2 路由，`App.tsx` + `layout/nav-model.ts` 單一 nav 源）

心智模型不變（AI 員工公司），落點方式全變。**舊 v1 legacy 別名全部保留 301。**

```
/                首頁「事務所」 — 問候HUD + 世界舞台 + 需要我 + 正在進行直播卡
/inbox           收件匣         — paperclip 五 tab（我的/最近/未讀/阻塞/全部）混流 + j/k 鍵盤流
/chat            對話           — DuDu 置中 + 工具步驟樹；/webchat 301
/tasks           任務           — 列表⇄看板雙視圖
/tasks/:id       任務詳情       — 中央單欄 + 右欄 IssueProperties + 底部 tabs（旗艦面）
/routines        例行工作
/reports         產出
/forks           並行分身 [mgr]
/agents          員工名冊       — 角色卡 roster
/agents/:id/:tab 員工詳情       — bust 立繪 + 心情 + 徽章 + run 帳本
/org             團隊 [mgr]     — 組織圖（節點=角色頭像卡）
/memory          記憶（Brain）
/skills          技能           — 我的技能⇄市集 tab
/skills/new      自建技能精靈   — 人類×Agent 協作起草 + 上級核准（4 步 DuDu 陪同）
/skills/custom/:id 自建技能詳情 — 狀態/內容/審批歷史/省時統計
/knowledge       知識庫         — 個人⇄共享 tab
/growth          成長           — 公司等級/XP/成就牆/日報卡
/manage/*        管理室 [mgr+]  — 11 節點樹（ManageShell subnav）
/login /welcome /wizard          — 入口三頁，DuDu 接待員化
/mascot-overlay                  — Tauri 桌寵迷你路由
/world → 301 /   （世界併入首頁，不再孤立）
```

RPC → 頁面**零遺漏**映射：見設計文件附錄 A（164 dispatch arm 逐條落點，`connect.challenge` 為連線層，
零無家可歸）。

### 9.1 Measure / terminology table（zh-TW UI copy，拍板 2026-07-08，沿用）

非工程受眾 — 內部術語永不外露。新 copy 必先進本表、再進三語 catalogue。

| Concept | zh-TW UI copy | Never show |
| --- | --- | --- |
| agent | **AI 員工** | agent / 代理 |
| session | 對話 | session |
| skill | 技能 | — |
| cron | 例行工作 | cron |
| — | — | MCP / runtime / PTY |

## 10. Per-page rebuild checklist（可重複方法）

1. **Read** 現頁；列出每個 data source（stores/api）與 action。**保留所有行為** — 這是視覺/結構重設計。
2. `<Page>` + `<PageHeader title subtitle actions>` 包起。
3. 用 `Card/Section/StatCard/Tabs` 取代 ad-hoc；agent 名字處掛 `CharacterAvatar`；機器值用 `<Mono>`。
4. 軟卡化：`glass-card`→`panel`（軟浮）；radius 升 `rounded-card`；spacing 依 §2；空資料用 `EmptyState`（配 DuDu pose）。
5. 屬性/詳情走**右欄 PanelContext**；慶祝接點掛 `CelebrationLayer`。
6. i18n：所有字串走 `intl.formatMessage`，新 key **同 commit 進三語 catalogue**（zh-TW/en/ja-JP 完全一致）。
7. **Verify**：`npx tsc -b` + `npx vitest run` + `npm run build` 綠；心中跑 `audit`/`critique`
   （a11y、對比、overflow、鍵盤、reduced-motion）。
8. 不動 store/api signature；diff 行為保守。

## 11. Reusable refactor workflow（把流程變成可重複方法）

v2 的可重播配方 — 未來任何再設計/新頁批次沿用：

1. **Understand intent** — 目標美學 + 品牌約束寫進 §1 before/after 表。設計 skills 參考：
   `teach-impeccable`（蒐集）、`critique`（評估）、`design-taste-frontend`（乾淨美學，**adapt 不照抄**：
   保留 Lucide + 🐾 + glass-for-chrome + 角色系統）。
2. **Tokens first** — 擴 `src/index.css`（`@theme` + CSS vars + `@utility`）；加 Soft Play 層（§2）。禁 hardcode。
3. **Primitives**（`components/ui/` + barrel）— 小可組合集（§6.1）；頁面 compose，never re-style。
4. **Shell + IA** — 單一 nav 源（`nav-model.ts`）、三欄殼、HUD；group i18n key 進三語。
5. **Reference surface** — 手工先遷一個旗艦面（`/tasks/:id`）；`npx tsc -b` 綠才 fan out。
6. **Fan out** — 平行 sub-agent 分批（~6/批）。每 agent 拿：本檔 + barrel + 旗艦面 + §10 checklist。
   **Anti-conflict 鐵律：**
   - agents **不編輯 `src/i18n/`**（並發寫入危害）——重用既有 key，或用暫存 `_wN-*-keys.json` 交波間 gate 合併；
   - agents **不跑 `tsc -b`**（build-info race）——orchestrator 每批跑 ONE 合併 `tsc -b`；
   - **⛔ agents 絕對禁止用任何 git 寫入指令驗證**（`stash / checkout / reset / add / commit`）。
     **教訓（W7 事故）**：子代理在多方 WIP 樹上跑 `git stash` 驗證、pop 衝突致樹半殘 + WIP 被 stage +
     13 個已刪測試檔自 HEAD 復活，指揮花大量 context 收斂。**驗證一律直接讀 `tsc`/`vitest` 輸出，不碰 git。**
7. **Verify**（§10 point 7）— `tsc -b` + `vitest run` + `npm run build` + `audit`/`critique` 綠；
   修 criticals（focus ring、search `aria-label`、對比、reduced-motion）。
   **Gotcha**：部分 `*.test.tsx` 斷言頁面 H1 文案 — 規格變更（如 roster 預設改角色卡）要**同步更新測試**
   （這是規格變更，非遷就實作），非規格變更則保留原 title i18n key。

## 12. Accessibility & resilience（`audit` / `harden` 驅動）

- **WCAG 2.1 AA 對比**雙主題：軟卡 `shadow-soft` 下實測 — 次要文字 `text-stone-500 dark:text-stone-400`
  在暖白 panel（≈99% 白）4.7–4.8:1、dark panel 6.9:1，**達 AA**；status icon 用 `-icon-` OKLCH 變體（≥4.5:1）。
- **focus-visible ring** 全互動：`Button`/`RosterCard`/`HireSlotCard`/InboxRow 動作鈕/view-toggle tab/wizard 步驟
  皆 `focus-visible:ring-2 ring-amber-500/50`；InboxRow 走 `role="option"` roving-selection（j/k + selection ring）。
- **鍵盤**：native `<dialog>` + `showModal()`（`components/shared/Dialog.tsx`）→ Esc 關閉 + focus-trap 天然具備
  （CreateTaskModal / DailyReportCard / remove-confirm 皆沿用）；`/skills/new` 為**路由頁非 modal**（正常 tab 流）。
- **reduced-motion**：全 keyframes gate 在 `no-preference` + 全域 reduce 斷路器；世界舞台/CelebrationLayer/
  viseme/桌寵 另有 JS gate（`resolveStageMode`、`celebrate()` 粒子抑制）。
- **canvas / SVG**：世界容器 `role="img"`+label；`CharacterAvatar`/`DuDu` wrapper `role="img"`+label、內層 SVG `aria-hidden`。
- Text overflow：truncate + title；tables scroll-x；長 CJK wrap。每個 async surface 有 loading + empty + error。
