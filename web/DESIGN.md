# DuDuClaw Design System

> 儀表板（`web/`）的單一設計真相源。每個頁面、每個新元件都必須遵循本文件。
> 技術基座：Vite + React 19 + React Router 7 + Tailwind v4（`@theme inline` + CSS 變數）。
> 語言：zh-TW 為主，en / ja-JP 三語平權。
> 元件庫位置：`web/src/components/mds/`（本文件簡稱「mds」）。

本設計系統的一句話：**四層 surface 疊出深度、OKLCH token 決定每一個顏色、克制的動效、圓角與陰影成體系**——目標是一個安靜、精準、資訊密度高但不擁擠的工作台，掃一眼就懂「現在怎麼了、要不要我拍板、我該怎麼做」。

---

## 1. 設計語言概述

### 1.1 Surface 四層哲學（核心）

介面的深度不靠陰影堆疊，而靠四層由暗到亮的表面。理解這四層就理解了整個佈局：

| 層 | Token | 角色 | 典型用途 |
|---|---|---|---|
| 1 | `--app-shell` | 最外框（最暗） | app 殼、sidebar 底 |
| 2 | `--page-canvas`（= `--background`） | 內容「生活」的畫布 | 列表 / 看板 / 對話所在的頁面底 |
| 3 | `--surface`（= `--card`） | 有邊界的內容群組 | 卡片、面板 |
| 4 | `--surface-raised`（= `--popover`） | 浮層 | menu / select / dialog / tooltip |

App Shell 是浮島式（inset）：sidebar 與內容區各自 `rounded-xl`，四周留 `p-2`，內容區帶 `ring-1 ring-surface-border` + `--surface-shadow`，浮在 `--app-shell` 底之上。互動狀態另有 `--surface-hover` / `--surface-selected` / `--surface-border`。

### 1.2 OKLCH 色彩 token

所有顏色寫成 OKLCH，定義在 `src/index.css` 的 `:root`（light）與 `.dark`（dark），再由 `@theme inline` 映射成 Tailwind color utility（`--color-surface: var(--surface)` 等）。**永遠用 utility / 變數，禁止 hardcode hex 或 px。**

語義色成對出現（背景 + `-foreground`）：`--primary`、`--secondary`、`--muted`、`--accent`、`--destructive`、`--brand`、`--success` / `--warning` / `--info`，加上 `--border` / `--input` / `--ring`。Sidebar 有自己一組 `--sidebar-*`。

- **Dark 主題不是把亮度乘負值**：`--primary` 在 dark 反轉為近白（`oklch(0.92 …)`），`--border` 改用半透明白（`oklch(1 0 0 / 10%)`）。改 token 一律雙主題同步。
- **品牌色**：`--brand` 目前是藍（hue 255，light `0.55 0.16` / dark `0.65 0.16`），chart 色相跟隨 brand hue。這是刻意的中性偏冷基調；`index.css` 的 brand 區塊留有註解，日後要換回 amber 只需改這一處而不動其他層。
- **圖表色 `--chart-1..5`**：以 brand 為 chart-1，向外堆疊「主→次→三級」層次。Light 遞減飽和、dark 反轉亮度（最重要最亮）。純 SVG 圖表一律讀這五個 token 上色，不引入圖表庫。

### 1.3 三層陰影

陰影按浮起高度分三級，各有 light / dark 兩套，一律用 token 不手寫：

| Token | 用途 |
|---|---|
| `--surface-shadow` | 卡片（貼地的內容群組） |
| `--menu-shadow` | menu / select / popover（中浮） |
| `--floating-shadow` | dialog / sheet（高浮） |

慣例：浮層 = `shadow-[var(--…-shadow)] ring-1 ring-surface-border`；tooltip 例外，用更輕的 `border border-border bg-popover`。

### 1.4 Radius 體系

基準 `--radius: 0.625rem`（10px），`@theme` 內以 calc 推導整套：`sm` ×0.6 / `md` ×0.8 / `lg` ×1 / `xl` ×1.4 / `2xl` ×1.8 / `3xl` ×2.2 / `4xl` ×2.6。

| 用途 | class |
|---|---|
| 按鈕 / 輸入 / select trigger | `rounded-lg` |
| 卡片 / dialog / sheet | `rounded-xl` |
| menu item / dropdown item | `rounded-md` |
| badge（膠囊） | `rounded-4xl` |

### 1.5 字體與字級

- `--font-sans`：**Inter Variable**（self-host，`@fontsource-variable/inter`）+ zh-TW fallback（`PingFang TC` / `Microsoft JhengHei` / `Noto Sans TC`）。禁 CDN。
- `--font-mono`：**Geist Mono Variable** + `ui-monospace` fallback，無 CJK fallback（刻意）。機器值 / 計數一律 `font-mono tabular-nums`。
- `<html>` 掛 `antialiased font-sans` + `text-autospace: ideograph-alpha ideograph-numeric`（CJK 與拉丁 / 數字間自動留白）。
- **字重只用 400 / 500**；詳情 hero 與 Settings 分頁標題可到 600（`font-semibold`）。禁 bold 濫用。

| 用途 | class |
|---|---|
| 頁 header 標題 | `text-sm font-medium`（配 size-4 icon） |
| 卡片 / dialog 標題 | `text-base font-medium leading-snug`（dialog `leading-none`） |
| 詳情 hero / Settings 分頁標題 | `text-xl sm:text-2xl font-semibold` |
| 內文 / 表格 / 選單 | `text-sm` |
| meta / label / badge / tooltip | `text-xs` |
| 計數 / 機器值 | `font-mono text-xs tabular-nums text-muted-foreground` |

### 1.6 圖示

統一使用 **lucide-react**，具名匯入（`XxxIcon`），mds 元件內 SVG 自動 `size-4`（xs/sm 尺寸自動降為 size-3 / size-3.5）。禁 emoji 當功能圖示。

### 1.7 動效與 reduced-motion 紀律

- 浮層進退場：`duration-100` + `animate-in fade-in-0 zoom-in-95` / `animate-out fade-out-0 zoom-out-95`，方向性 `slide-in-from-*-2`（`tw-animate-css`）。實作走 base-ui 的 `data-*` 過場屬性。
- Sidebar 開合 `transition-[width] duration-200 ease-out`；右側 detail panel 220ms `cubic-bezier(0.22,1,0.36,1)`（按鈕觸發才動畫，拖曳為 snap）。
- 按鈕按下 `active:translate-y-px`；tooltip delay 500ms；popover `sideOffset=4`；頂部導航進度條 2px `bg-brand` 掃動。
- **reduced-motion 是硬紀律**：`index.css` 所有 keyframes 都 gate 在 `@media (prefers-reduced-motion: no-preference)`；另有全域斷路器 `@media (prefers-reduced-motion: reduce)` 把 `animation-duration` / `transition-duration` 壓到 `0.01ms`。JS 驅動的動態（世界舞台、慶祝粒子）另有 JS gate。新增任何動效都必須落在這道 gate 之後。

---

## 2. 元件庫（`components/mds/`）

從 barrel `@/components/mds` 匯入。頁面**組合**這些元件，不重新 style（不要往 mds 元件塞 ad-hoc class 蓋掉它的 variant）。

### 2.1 視覺原語（primitives）

| 元件 | 說明 |
|---|---|
| `Button` / `buttonVariants` | CVA 驅動，8 variant × 8 size（見 §2.3） |
| `Card` 家族 | `Card` / `CardHeader` / `CardTitle` / `CardDescription` / `CardAction` / `CardContent` / `CardFooter`；`data-size=sm` 縮排 |
| `Badge` / `badgeVariants` | 膠囊，5 variant（default / secondary / destructive / outline / ghost） |
| `Input` / `Textarea` | `h-8 rounded-lg border-input`，`aria-invalid` 轉紅 |
| `Select` 家族 | trigger + content + item + label + separator，尾 ChevronDown |
| `Dialog` 家族 | overlay `bg-black/10 backdrop-blur-xs` + 置中 content + header/footer |
| `DropdownMenu` 家族 | content / item（destructive variant）/ label / separator / shortcut |
| `Tabs` 家族 | `TabsList` / `TabsTab` / `TabsPanel`，`default` 與 `line` 兩式 |
| `Segmented` | 分段控制（報表頁切系列 / 時間範圍） |
| `Table` 家族 | 語義表格原語 |
| `Tooltip` 家族 | 輕框，delay 500 |
| `Popover` 家族 | Display / Filter 類彈出面板 |
| `Switch` / `Checkbox` / `Separator` / `Skeleton` | 基礎控制與骨架 |
| `Sheet` 家族 | 行動抽屜 / 右側面板 |
| `Empty` | 空 / 錯誤狀態（圓底 icon + title + 說明 + 選配 action） |
| `Spinner` | braille 等寬 spinner |
| `SubmitButton` | ArrowUp / Square / Loader2 三態送出鈕（對話輸入） |

### 2.2 版型件（layout layer）

| 元件 | 說明 |
|---|---|
| `Sidebar` 全家 | `SidebarProvider` / `Sidebar` / `SidebarRail` / `SidebarHeader/Content/Footer` / `SidebarGroup(+Label)` / `SidebarMenu(+Item/Button/Badge)` / `SidebarTrigger` / `SidebarInset` + `useSidebar` / `useIsMobile` |
| `PageHeader` | 每頁自帶的 `h-12` header（無全域 topbar） |
| `CollectionPageHeader` / `CollectionPageState` / `toCompactAction` | 集合頁 header + 空 / 載入 / 錯誤狀態 |
| `BreadcrumbHeader` / `BreadcrumbSegment` | 詳情頁麵包屑 header |
| `SettingsShell` / `SettingsTab` / `SettingsSection` / `SettingsCard` / `SettingsRow` / `SettingsSaveState` | Settings 式版型原語（見 §3.3） |
| `ResizablePanelGroup` / `ResizablePanel` / `ResizableHandle` | list+detail split（包 react-resizable-panels） |
| `ListGridContainer` / `ListGridHeader(Cell)` / `ListGridRow` / `ListGridCell` / `useRowLink` | Linear 風列表 + 虛擬捲動 + container query（見 §3.1） |
| `ActorAvatar` | 統一頭像（見 §2.4） |
| `NavProgress` | 頂部路由進度條（接 Suspense pending） |

### 2.3 Button variant 對映表

Base：`inline-flex items-center justify-center rounded-lg border border-transparent text-sm font-medium` + `focus-visible:ring-3 focus-visible:ring-ring/50` + `active:translate-y-px` + `disabled:opacity-50`。

| variant | 語義 | recipe |
|---|---|---|
| `default` | 主要動作（近黑 / dark 近白） | `bg-primary text-primary-foreground hover:bg-primary/80` |
| `outline` | 次要 | `border-border bg-background hover:bg-muted` |
| `brand` | 品牌強調 CTA | `bg-brand text-brand-foreground hover:bg-brand/90` |
| `brandSubtle` | 輕品牌底 | `border-brand/28 bg-brand/7 hover:bg-brand/12` |
| `secondary` | 群組內次動作 | `bg-secondary hover:bg-secondary/80` |
| `ghost` | 無底（icon 鈕 / 列內動作） | `hover:bg-muted` |
| `destructive` | 破壞性 | `bg-destructive/10 text-destructive hover:bg-destructive/20` |
| `link` | 純文字連結 | `text-primary hover:underline` |

Size：`default h-8` / `xs h-6` / `sm h-7` / `lg h-9` / `icon size-8` / `icon-xs size-6` / `icon-sm size-7` / `icon-lg size-9`。

Badge variant：`default`（bg-primary）/ `secondary` / `destructive`（`bg-destructive/10 text-destructive`）/ `outline` / `ghost`。

### 2.4 ActorAvatar

全站唯一頭像元件。`actorType`（`user` / `agent` / `system` / `squad`）決定 lucide fallback 圖示；sizes `xs`16 / `sm`20 / `md`24 / `lg`32 / `xl`40 / `2xl`56（px）；可 `showStatusDot`（右下 `size-1.5 rounded-full` 可用性彩點）。**紀律：全站出現 AI 員工名字的地方一律帶 avatar**，樣式為圓形 + `ring-1`、無漸層光暈。

---

## 3. 五種版型範式

每個頁面都落在以下五種範式之一。選對範式、套對骨架，頁面就與全站一致。

### 3.1 集合頁（ListGrid）

列表 / 名冊 / 資源清單。`CollectionPageHeader`（entity icon + `h1 text-sm font-medium` + count（mono）+ description）+ `ListGrid`。

```
<div className="flex flex-1 min-h-0 flex-col">
  <CollectionPageHeader icon={…} title count description action={toCompactAction(…)} />
  <ListGridContainer template="…" virtual={…}>
    <ListGridHeader> … <ListGridHeaderCell sortable/> … </ListGridHeader>
    <ListGridRow>  … <ListGridCell/> …  </ListGridRow>   // useRowLink 整列導航
  </ListGridContainer>
</div>
```

要點：單行管理列 `h-12`、雙行列（帶 avatar）`h-16`；`hover:bg-accent/40`、選中 `bg-accent/30`；整列導航用 `useRowLink`（非 `<a>` 包整列），真連結放 name cell；checkbox / kebab hover 才現形且 `stopPropagation`。container query 響應式（`@2xl` 顯示全欄，以下收次要欄）；虛擬捲動 header `sticky top-0`。

### 3.2 詳情頁（三式）

1. **BreadcrumbHeader 式**（任務詳情）：`BreadcrumbHeader`（段落 + `ChevronRight`，leaf 可截斷）+ 右動作群。主體 = 左內容 `mx-auto max-w-4xl px-8 py-8`（標題就地編輯）+ 右 320px 可折疊 panel（行動版轉 Sheet）。
2. **Hero header 式**（員工詳情）：`border-b px-4 sm:px-6 pb-5 pt-3` 內 `mx-auto max-w-[1440px]`；麵包屑列 → `ActorAvatar` 2xl + 名（`text-xl sm:text-2xl font-semibold`）+ presence + meta 列 + 右動作。下接 `line` Tab 列（總覽 / 工作 / 能力 / 設定）；能力與設定分頁內再用 master-detail（左 `aside md:w-52 border-r p-4` rail + 右 `max-w-3xl p-4 md:p-8`）。
3. **Settings 式**（見 §3.3）。

### 3.3 Settings 式（設定 / 管理區 / 員工能力·設定子分頁）

`SettingsShell`：桌面左 vertical rail（`md:w-56 md:border-r`，分組帶 group label）+ 右 `mx-auto max-w-3xl p-4 md:p-8` 獨立捲動；行動版 rail 轉水平橫捲。分頁狀態存 `?tab=`（replace + whitelist）；空組自動消失、fail-closed 導回。

```
<SettingsShell nav={groups} value={tab} onValueChange={…}>
  <SettingsTab title description>              // h2 text-xl font-semibold + 說明
    <SettingsSection title>
      <SettingsCard>                            // divide-y divide-surface-border
        <SettingsRow label description tier>…</SettingsRow>   // label 左、控制右
      </SettingsCard>
    </SettingsSection>
    <SettingsSaveState status />                // idle / saving / saved / error
  </SettingsTab>
</SettingsShell>
```

`SettingsRow`：`min-h-16 px-4 py-3.5 sm:flex-row sm:justify-between`，控制欄寬 tier — `text` w-96 / `select-wide` w-72 / `select` w-48 / `code` w-40。

### 3.4 報表 / Usage 式

`mx-auto max-w-6xl space-y-5 p-6`：KPI 列（`grid sm:grid-cols-2 lg:grid-cols-4` 的 `rounded-lg border bg-card`，divide 分隔，數字可動畫）→ 趨勢卡（`p-4` + `Segmented` 切系列，`min-h-[240px]`，純 SVG 圖表用 `--chart-*`）→ 排行卡（grid 列：`ActorAvatar` + 名 / 進度條 `h-2 rounded-full bg-muted` + `bg-chart-1` / 數欄 `tabular-nums`）。Header 可換行（`h-auto min-h-12 flex-wrap`）+ Select 篩選 + Segmented 時間範圍。

### 3.5 List + Detail split

雙欄工作台（收件匣 / 對話 / 執行紀錄）。`ResizablePanelGroup`：左列表 320（min 240 max 480，偏好記憶）+ 右 detail（`minSize 40%`）。行動版改全螢幕切換 + `h-12 border-b` 返回列（ArrowLeft）。未選中：置中 `text-muted-foreground/30` icon + 提示。

---

## 4. App Shell 與導航

- Root：`SidebarProvider` 容器 `h-svh bg-app-shell` + 左 `Sidebar variant="inset"`（浮島，`p-2`）+ 右 `SidebarInset`（`bg-page-canvas m-2 rounded-xl ring-1 ring-surface-border shadow`）。**無全域 topbar，每頁自帶 `PageHeader`。**
- Sidebar 寬 256（可拖 200–360，存 localStorage），可折疊 icon 模式，行動版轉 Sheet 抽屜。
- 導航分組（單一來源 `layout/nav-model.ts`）：**個人**（首頁 / 收件匣 / 對話）→ **工作**（任務 / 計畫 / 執行紀錄 / 畫布 / 例行 / 時間軸 / 用量 / 並行分身）→ **公司**（員工 / 團隊 / 世界 / 記憶 / 技能 / Widget / 知識庫 / 成長）→ **設定**（管理 / 關於）。非管理者整組依可見性規則隱藏。
- Menu item：`rounded-md p-2 h-8 text-sm`，未選 `text-muted-foreground`，hover `bg-sidebar-accent/70`，active `bg-sidebar-accent text-sidebar-accent-foreground font-medium`。active 判定 `pathname === href || startsWith(href + "/")`。
- Sidebar footer 承載：主題切換、Edition / 升級卡、成本 / XP 摘要（原 header HUD 內容遷入）；語言 / 登出在公司下拉。⌘K 開 Command Palette。

---

## 5. 工程紀律

1. **Token-only，禁 hardcode。** 顏色走 OKLCH token / Tailwind color utility，尺寸走 radius / spacing scale，陰影走 `--*-shadow`。任何 `#hex`、裸 `px`、手寫 rgba 陰影都是 bug。改一個 token 必須雙主題（`:root` + `.dark`）同步。
2. **i18n 三語同步。** 所有面向使用者字串走 `intl.formatMessage`；**新增 key 必在同一 commit 內補齊 `en` / `ja-JP` / `zh-TW` 三份 catalogue**，數量與 key 完全一致。UI 用使用者視角詞彙，不外露內部術語（agent 稱「AI 員工」、session 稱「對話」、cron 稱「例行工作」；MCP / runtime / PTY 等永不出現在 UI）。
3. **a11y 是預設而非附加。** WCAG 2.1 AA 對比雙主題；所有互動元件 `focus-visible` ring 統一 `ring-3 ring-ring/50`；鍵盤可達（Dialog 走 Esc + focus-trap、列表可 roving-selection）；canvas / SVG 容器 `role="img"` + `aria-label`、內層裝飾 SVG `aria-hidden`；文字 overflow 一律 truncate + `title`，表格 scroll-x，長 CJK wrap；每個 async 面都有 loading / empty / error 三態。
4. **功能元件 vs 視覺原語的邊界。** mds 是**視覺原語**——只管長相與互動語彙，不含業務邏輯。功能元件（審批風險判定、世界舞台狀態機、圖表資料計算、角色生成）**組合** mds 原語但把邏輯留在自己身上；反過來不要把業務條件寫進 mds。頁面 compose 原語，不 re-style 原語。
5. **保守遷移行為。** 重設計是換視覺 / 結構，**功能一個不減**：不動 store / api signature，行為 diff 保守。規格變更（如列表預設視圖改變）要同步更新對應 `*.test.tsx` 斷言（這是規格變更，非遷就實作）。

### 5.1 刻意殘留清單（**不是**待清理債務）

以下項目刻意不套 mds token / 樣式，改動前先確認你不是在「修正」一個刻意設計：

- **Terminal / 執行紀錄深色框**：終端輸出區維持深色等寬框（可讀性語彙），不跟隨明暗主題翻白。
- **d3 圖譜 palette**：記憶 / 知識圖譜的 d3 力導向圖有自己的節點 / 邊配色，不改用 `--chart-*`。
- **世界舞台（PixiJS）**：`/world` 的等距場景是 canvas 渲染，palette 從 CSS token 讀值即時重建，但其 sprite / tint 體系獨立於 mds 元件層。
- **平台識別色**：通道設定裡各平台（Telegram / Discord / Slack / LINE …）的品牌識別色為資料值，刻意保留以利辨識，不 token 化。
- **經銷商白牌（BrandingTab）資料值**：白牌自訂的顏色 / logo 是租戶資料，不是設計 token。

---

## 6. 新頁 / 改頁 checklist

1. Read 現頁：列出每個 data source（stores / api）與 action，**保留所有行為**。
2. 選定 §3 的版型範式，套上 `PageHeader` / `CollectionPageHeader` / `BreadcrumbHeader` / `SettingsShell` 骨架。
3. 用 mds 原語取代 ad-hoc；AI 員工名字處掛 `ActorAvatar`；機器值用 `font-mono tabular-nums`；空資料用 `Empty` / `CollectionPageState`。
4. i18n：字串走 `intl.formatMessage`，新 key 同 commit 進三語。
5. 屬性 / 詳情走右欄 panel 或 split（§3.2 / §3.5）。
6. Verify：`npm run build` + `npx vitest run` 綠；心中跑 a11y / 對比 / overflow / 鍵盤 / reduced-motion 一遍。
7. 不動 store / api signature；規格變更同步改測試。

---

## 附：驗證指令

```bash
cd web
npm run build        # 產物輸出到 crates/duduclaw-dashboard/dist/（gateway 以 rust_embed 內嵌）
npx vitest run        # 單元 / 元件測試
```

Rust 端內嵌：`crates/duduclaw-dashboard` 用 `rust_embed` `#[folder = "dist/"]` 內嵌 SPA，`cargo build -p duduclaw-gateway` 會把最新 `dist/` 打進 gateway。改動儀表板後先 `npm run build` 再 build gateway。
