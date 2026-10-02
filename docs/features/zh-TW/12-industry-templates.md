# 產業模板與 Odoo ERP 橋接

> 開箱即用的商業智慧：幾分鐘內部署領域專家 Agent。

---

## 比喻：餐廳的套餐 vs. 單點

當你開一家新餐廳，有兩個選擇：
- **單點**：每道菜從零設計。最大彈性，最大工作量。
- **套餐**：從經過驗證的組合開始，然後依口味客製化。快速上線，容易迭代。

DuDuClaw 的產業模板就是 Agent 部署的套餐。每個模板包含 Agent 在特定產業運作所需的一切（人格、行為規則、領域知識），開箱即用。

---

## 產業模板

### 模板內容

每個模板是一個完整的 Agent 入門套件：

```
templates/{industry}/
├── SOUL.md           # 針對產業調校的 Agent 人格
├── CONTRACT.toml     # 產業專屬行為邊界
├── agent.toml        # Agent 設定
└── …                 # 產業附加檔：餐飲 FAQ.json + PROACTIVE.md、
                      # 製造 SOP-template/、貿易 price-list-template.csv
```

**SOUL.md** — Agent 的人格預設了符合產業的溝通風格：
- 製造業 Agent 使用精準、以數據為導向的語言
- 餐飲業 Agent 溫暖、服務導向，自然地處理食物相關查詢
- 貿易業 Agent 簡潔、數字導向、有風險意識

**CONTRACT.toml** — 行為邊界反映產業法規：
- 製造業 Agent 未經人工確認不得核准設備重新啟動
- 餐飲業 Agent 談到菜色時一律附上過敏原警語
- 貿易業 Agent 每份報價都要寫明貿易條件（FOB/CIF/EXW）

**附加檔** — Agent 工作時用的起始資料：餐飲有 FAQ 與主動檢查排程，製造有設備異常 SOP 範本，貿易有價目表範本。請把佔位值換成自己的資料。

### 可用模板

**製造業** — 工廠營運助理：監看生產、以嚴重度標籤回報異常、轉達 SOP 步驟、協調交接班。

**餐飲業** — 客服助理：回答詢問與菜單問題、接受訂位，談到菜色時列出過敏原。

**貿易業** — 國際貿易助理：回覆買家詢價，依價目表報價並附貿易條件與報價有效期，追蹤買賣雙方之間的訂單。

### 客製化流程

模板是起點，不是束縛：

```
步驟 1：部署模板（`duduclaw wizard`，從選單選產業）
步驟 2：客製化人格——編輯 SOUL.md 以符合你的品牌語氣
步驟 3：調整邊界——修改 CONTRACT.toml 以符合特定合規要求
步驟 4：新增領域知識——匯入菜單、供應商、流程到 wiki
步驟 5：讓演化接手：SOUL.md 維持你寫的樣子（Agent 不能修改），
        Agent 學到的是一條條小型 playbook 規則，每條連結一個 eval 案例，
        不再有幫助時單獨退休，全部維持在客製化的契約邊界內（見 features/38）
```

---

## 一種包格式

產業模板、專家包（一整支團隊）、職務組合（preset），是同一件事的三種形狀：一份宣告，產出已經配置好的 AI 員工。現在它們共用同一份 manifest schema 與同一個指令。

`pack.toml` 用 `kind` 說明自己是哪一種：

```toml
[pack]
schema  = 1
id      = "clinic-team"
kind    = "team"        # "preset" = 一份職務設定｜"team" = 一支團隊｜"template" = 單人產業板模
tier    = "free"        # "free" | "premium"
version = "1.0.0"
label   = "醫美／牙醫診所"
description = "一位前台加五位部門同事"

[[pack.agents]]
name = "clinic-assistant"
role = "front_desk"
```

一個指令讀得懂、也裝得起全部三種：

```bash
duduclaw pack list                 # 已安裝的包、本機職務組合、內建目錄裡可裝的東西
duduclaw pack inspect ./my-team    # 正規化後的樣子，並說明這份檔案是哪一代格式
duduclaw pack install ./my-team    # 依 kind 分流；付費內容只在一處檢查授權
```

`kind` 決定安裝路線：職務組合寫進職務組合庫（`presets/<id>/preset.toml`，再用 `duduclaw preset bind` 綁到某位員工），團隊包與產業板模走 [features/32](32-expert-packs.md) 描述的完整安全管線。`tier` 是唯一的付費判定：以前有四段程式各自從目錄路徑推「這是不是付費內容」，現在包自己帶著 tier，只有一個判斷式讀它。

**三種舊格式到 v1.68.0 都還能用。** `expert.toml`、`team.toml`、`preset.toml` 原樣讀取，磁碟上一個字都不改——付費內容樹裡的法規條文是逐字人審過的，不交給機器轉檔。`duduclaw expert install` 與 `duduclaw preset` 維持為同一條程式路徑的別名。想看舊檔在新 schema 下長什麼樣，跑 `duduclaw pack inspect <dir> --emit-canonical`：它只印出來，不寫檔。

---

## Odoo ERP 橋接

### 問題

AI Agent 可以*談論*商業運營，但無法*執行*它們。Agent 可能知道客戶需要一張發票，但它無法在你的 ERP 系統中建立一張，除非它有橋接。

### 解決方案

DuDuClaw 包含一個中介軟體，將 Agent 直接連接到 Odoo（全球使用最廣泛的開源 ERP 系統之一）：

```
使用者：「幫我建一張客戶 ABC 的銷售訂單，Widget X 10 個」
     |
     v
Agent 理解意圖
     |
     v
Agent 呼叫 MCP 工具：odoo_sale_create_quotation，再呼叫 odoo_sale_confirm
     |
     v
DuDuClaw Odoo Bridge 轉譯為 JSON-RPC 呼叫
     |
     v
Odoo ERP 系統建立銷售訂單
     |
     v
Bridge 回傳結果（訂單編號、金額）
     |
     v
Agent：「已建立銷售訂單 SO-2024-0042，客戶 ABC。
        Widget X 10 個，合計：$1,500。」
```

### 可用操作（17 個 MCP 工具）

橋接提供以下工具（`crates/duduclaw-cli/src/mcp/tools_def/odoo.rs`）：

- **連線**：`odoo_connect`、`odoo_status`
- **CRM**：`odoo_crm_leads`（列出潛客）、`odoo_crm_create_lead`、`odoo_crm_update_stage`
- **銷售**：`odoo_sale_orders`（列出訂單）、`odoo_sale_create_quotation`、`odoo_sale_confirm`
- **庫存**：`odoo_inventory_products`（搜尋產品）、`odoo_inventory_check`（庫存水位）
- **會計**：`odoo_invoice_list`、`odoo_payment_status`
- **通用**：`odoo_search`（搜尋任一模型）、`odoo_execute`（呼叫模型方法）、`odoo_report`、`odoo_partner_search`、`odoo_schema_fields`

### 版本偵測

Odoo 有兩個版本：社區版（CE，開源）和企業版（EE，付費）。部分功能僅在 EE 可用。

橋接自動處理：首次連接時偵測 Odoo 版本（CE 或 EE），只暴露偵測到版本支援的 MCP 工具。無需設定。橋接會探測 Odoo 實例並自動適應。

### 事件同步

除了執行操作，橋接還可監聽 Odoo 中的事件。兩條管道餵進同一條自動化匯流排，**兩條都預設關閉**，要自己打開：

| 管道 | 開關 | 運作方式 |
|---|---|---|
| 輪詢 | `config.toml [odoo] poll_enabled`（預設 `false`） | 每隔 `poll_interval_seconds`（60–86400，預設 60）向 Odoo 查詢 `poll_models` 裡 `write_date` 在上一輪之後變動的記錄。 |
| Webhook | `config.toml [odoo] webhook_enabled`（預設 `false`） | Odoo 的 automated action 帶著 `[odoo] webhook_secret` 的共用密鑰 POST 到 `POST /webhook/odoo`。 |

```
Odoo 事件發生：
  - 新潛客建立
  - 訂單狀態變更
  - 發票逾期
     |
     v
輪詢捕捉到變更  或  Odoo 打到 /webhook/odoo
     |
     v
一則 `odoo_event` 進入自動化匯流排
     |
     v
由你的自動化規則決定怎麼辦：
  - 通知銷售團隊新潛客
  - 把後續追蹤派給某個 AI 員工
  - 為逾期發票發送付款提醒
```

規則寫法跟其他觸發事件一樣，觸發事件名稱是 `odoo_event`。除了 `event_type`／`model`／`record_id`，記錄的頂層純量欄位也會直接攤平上來，所以條件可以直接寫 `{"field": "state", "op": "eq", "value": "sale"}`。

v1.67.1 起也可以在儀表板建立這種規則（設定 → 自動化 → 新增規則，觸發事件選「Odoo 資料變動時」），條件欄位直接輸入 Odoo 的欄位名稱，例如 `state`。v1.67.1 之前儀表板表單建不出任何規則。

這個端點對外可達，所以行為刻意保守：

- `webhook_enabled` 關著時整條路由回 **404**——預設安裝不會洩漏這個端點存在。
- 密鑰缺漏或不符回 **401**；**密鑰設成空字串也一律拒絕**，半套設定寧可什麼都不收。
- 輪詢要同時滿足三件事才會啟動：`poll_enabled` 打開、Odoo 連線已設定、`poll_models` 至少有一個合法模型名稱。缺任一項就完全不會起背景工作。

這將 Agent 從被動的工具使用者轉變為主動的商業參與者。

---

## 模板 + ERP 橋接的組合威力

```
製造業模板 + Odoo Bridge：
  Agent 監控庫存水位（Odoo）→
  偵測到關鍵物料庫存不足 →
  自動建立採購訂單 →
  透過設定的通道通知生產經理

餐飲業模板 + Odoo Bridge：
  Agent 收到大型外燴訂單（通道訊息）→
  檢查食材可用性（Odoo 庫存）→
  建立銷售訂單（Odoo 銷售）→
  標記任何過敏原疑慮（wiki 知識）→
  向客戶確認

貿易業模板 + Odoo Bridge：
  Agent 收到市場資料更新 →
  與投資組合部位交叉比對（Odoo）→
  辨識超過風險門檻的部位 →
  發送警報給交易員，附建議操作
```

---

## 這為什麼重要

### 加速見效

沒有模板時，部署產業專屬 Agent 需要：研究產業術語和流程、撰寫人格檔案、定義行為邊界、建立領域知識庫、測試和迭代。有了模板，這些步驟都已預建。幾分鐘內即可部署。

### 作業深度

Odoo 橋接將 Agent 從對話助手轉變為作業工具。它們不只是*建議*建立發票，而是*建立*發票。彌合 AI 建議與商業行動之間的鴻溝。

### 標準化

模板編碼了產業最佳實踐。從模板建立的 Agent 已經了解品質管控標準、安全規範和庫存管理實務。營運人員不需要重新發明這些知識。

### 可組合性

模板、ERP 橋接和演化系統無縫合作。模板提供起點，ERP 橋接提供作業能力，演化系統基於實際表現持續改進 Agent；這一切都在契約的安全邊界之內。

---

## 與其他系統的互動

- **演化引擎**：從模板部署的 Agent 像其他 Agent 一樣演化。模板是起點，不是永久狀態。
- **行為契約**：每個模板包含針對產業合規要求量身打造的契約。
- **記憶系統**：wiki 中的領域知識透過記憶系統索引和可搜尋。
- **通道整合**：模板 Agent 支援所有 11 個通訊通道。
- **成本管理**：ERP 橋接操作在 CostTelemetry 中追蹤以供預算可見。

---

## 總結

產業模板與 Odoo ERP 橋接解決了 Agent 部署的「最後一哩」問題：從通用 AI 到真正能在現實世界*做事情*的領域專家。模板提供知識和人格；ERP 橋接提供作業能力；演化系統確保持續改進。
