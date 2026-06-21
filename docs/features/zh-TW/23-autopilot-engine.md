# Autopilot 規則引擎

> 事件驅動的自動化——當*這件事*發生，就做*那件事*，並配備一塊在規則失控時自動跳脫的斷路器面板。

---

## 比喻：配有斷路器面板的智慧家庭

智慧家庭運行於簡單的規則：*當前門在日落後開啟，打開走廊燈。* *當溫度降到 18°C 以下，啟動暖氣。* 你不需要監督這些——它們會自行觸發，隨事件發生而即時反應。

但天真的規則引擎有一種故障模式：某條規則觸發了一個事件，而該事件又再次觸發同一條規則。燈亮起，觸動了動作感測器，於是燈又亮起，無限循環。設計良好的家庭配有**斷路器面板**——若某個迴路在短時間內抽取過多電流，斷路器就跳脫並將其隔離，讓房子其餘部分繼續運作。

DuDuClaw 的 Autopilot 規則引擎就是你 agent 機群的這套智慧家庭。規則監聽事件（建立了一個 task、收到一則 channel 訊息、某個 agent 進入閒置），檢查條件，然後派發動作（委派工作、發送通知、執行 skill）。而每條規則都有自己的三態斷路器，因此一個自我增強的迴圈只會讓*那條規則*跳脫，而不會拖垮整個引擎。

---

## 運作方式

引擎建構於 `tokio::broadcast` 事件匯流排之上。生產者（WebSocket handler、cron 排程器、channel 監聽器）將 `AutopilotEvent` 發布到匯流排上；`AutopilotEngine` 是唯一的消費者，針對每個事件評估每一條規則：

```
事件生產者                       事件匯流排            規則評估              動作派發
─────────────────                ──────────            ───────────────       ───────────────
WebSocket handler ──┐
Cron 排程器        ─┼──> broadcast::channel(8192) ──>  AutopilotEngine ──> ┌─ delegate
Channel 監聽器     ─┤         (AutopilotEvent)          對每條規則：       ├─ notify
MCP bridge         ─┘                                   1. 事件相符？       └─ run_skill
                                                        2. 條件通過？
                                                        3. 斷路器閉合？
                                                        4. → 派發動作
                                                        5. → 寫入 history
```

匯流排容量為 8,192——足以吸收一波事件爆發，而不會讓緩慢的資料庫寫入對生產者形成反壓。若消費者落後到足以丟失事件，引擎會記錄落後計數，讓維運者可調查緩慢的 DB 或調高容量。

---

## 五種事件型別

引擎訂閱五種 `AutopilotEvent`。每種都攜帶一個 payload，會被攤平成一個欄位映射供條件比對：

| 事件 | `event_name` | 觸發時機 | 關鍵欄位 |
|------|--------------|----------|----------|
| **TaskCreated** | `task_created` | Task Board 上出現新 task | task 物件（id、title、priority……） |
| **TaskStatusChanged** | `task_status_changed` | task 在狀態間移動 | `task_id`、`from`、`to`、task 物件 |
| **ChannelMessage** | `channel_message` | channel 上收到訊息 | `channel`、`agent_id`、`text` |
| **AgentIdle** | `agent_idle` | 某個 agent 進入閒置 | `agent_id`、`idle_minutes` |
| **CronTick** | `cron_tick` | 排程器發出週期性 tick | `now` |

規則會宣告它關心哪個 `trigger_event`，因此 `channel_message` 規則根本不會看到 `cron_tick`。

---

## 條件

規則的條件是一棵小型 JSON 樹。最上層可以是 `all`（每個子項都須通過）或 `any`（至少一個子項通過）群組，而每個葉節點以一個運算子比較單一欄位與期望值：

```
{ "all": [ <condition>, <condition>, ... ] }   ← 每個子項都須為真
{ "any": [ <condition>, <condition>, ... ] }   ← 至少一個子項為真
```

葉節點條件依路徑查找欄位並套用運算子：

| 運算子 | 意義 |
|--------|------|
| `eq` | 欄位等於期望值 |
| `neq` | 欄位不等於期望值 |
| `in` | 欄位是某個值陣列的其中之一 |
| `gt` / `gte` | 欄位在數值上大於（或等於） |
| `lt` / `lte` | 欄位在數值上小於（或等於） |
| `contains` | 字串包含子字串，或陣列包含某值 |

**不存在**的欄位永遠無法滿足任何比較——包括 `eq null`。這是刻意的：曾經允許不存在的欄位匹配 `eq null`，導致某條規則對每個事件大量觸發。缺失即不匹配，沒有例外。

---

## 三種動作型別

當規則的事件相符、條件通過、且斷路器閉合時，引擎會派發三種動作之一：

| 動作 | 作用 | 必要欄位 |
|------|------|----------|
| **delegate** | 為目標 agent 排入一個 bus task | `target_agent`、`prompt` |
| **notify** | 向 channel 發送訊息 | `channel`、`chat_id`、`text` |
| **run_skill** | 以目標 agent 身分執行 skill | `target_agent`、`skill_name` |

`run_skill` 最為敏感，因為 agent 與 skill 名稱都來自規則設定。引擎會驗證兩者：

```
run_skill 動作：
     |
     v
target_agent 必須為英數字（allowlist） ─── 否則拒絕
     |
     v
skill_name 必須為安全的檔名 stem（allowlist） ── 拒絕 "../passwd"、"skill/subdir"
     |
     v
canonicalize(skills_dir/skill_name)
     |
     v
正規化後路徑必須 start_with canonicalize(skills_dir) ── 若逃逸則拒絕
     |
     v
執行
```

路徑包含檢查（`canonicalize()` + `starts_with`）意味著即使某個 skill 名稱越過了字元集 allowlist，也無法引用 agent skills 目錄之外的檔案。

---

## 斷路器

每條規則都有自己的三態斷路器。這是引擎對抗自我增強迴圈的防線——某條規則的動作產生了一個事件，而該事件又再次觸發同一條規則（`task_created → delegate → agent 建立 task → task_created → ...`）。

```
            ┌──────────────────────────────────────────────┐
            │                                              │
            v                                              │
      ┌──────────┐   60 秒內 >= 10 次觸發       ┌────────┐  │
      │  CLOSED  │ ─────────────────────────>  │  OPEN  │  │
      │ (正常)   │                             │(已封鎖) │  │
      └──────────┘ <───────────────────────┐  └────────┘  │
            ^      安靜的探測視窗            │       │      │
            │      (未重新跳脫)              │       │ 冷卻 60 秒後
            │                               │       v      │
            │                          ┌──────────────┐    │
            └──────────────────────────│   HALF-OPEN  │    │
                                       │ (1 次探測 ok)│────┘
                                       └──────────────┘
                                          探測視窗內再次
                                          觸發 → OPEN
```

- **Closed** — 正常運作。觸發在 60 秒滑動視窗內計數。視窗內 10 次觸發會使斷路器跳脫至 **Open**。
- **Open** — 此規則的所有觸發在 60 秒冷卻期內被封鎖。引擎其餘部分照常運作。
- **HalfOpen** — 冷卻後，允許一次探測觸發。若探測視窗內又有一次觸發（迴圈仍活躍的徵兆），斷路器重新跳脫回 Open。安靜的探測視窗則使其回到 Closed。

狀態轉換會記錄到 `autopilot_history` 並呈現在 Activity Feed 上，讓維運者能準確看到某條規則何時、為何被節流。

---

## 規則定義

規則持久化於 SQLite，包含名稱、enabled 旗標、`trigger_event`、`conditions` 樹與 `action`。以下是一條將緊急新 task 委派給待命 agent 的規則：

```json
{
  "name": "urgent task → on-call",
  "enabled": true,
  "trigger_event": "task_created",
  "conditions": {
    "all": [
      { "field": "task.priority", "op": "eq", "value": "urgent" },
      { "field": "task.title", "op": "contains", "value": "incident" }
    ]
  },
  "action": {
    "kind": "delegate",
    "target_agent": "oncall",
    "prompt": "An urgent incident task was just created. Triage it."
  }
}
```

一條在 agent 閒置過久時向 channel 發訊的 `notify` 規則：

```json
{
  "name": "idle agent alert",
  "enabled": true,
  "trigger_event": "agent_idle",
  "conditions": {
    "all": [ { "field": "idle_minutes", "op": "gt", "value": 30 } ]
  },
  "action": {
    "kind": "notify",
    "channel": "telegram",
    "chat_id": "12345",
    "text": "An agent has been idle for over 30 minutes."
  }
}
```

---

## 規則 CRUD：Dashboard RPC + MCP

規則可從兩個介面管理，兩者皆 fail-closed（需要 Admin scope）：

```
Dashboard (web UI)                         Agent (MCP)
──────────────────                         ───────────
autopilot.list    ── 列出所有規則          autopilot_list ── 對規則集的
autopilot.create  ── 新增規則                              唯讀檢視
autopilot.update  ── 編輯規則
autopilot.remove  ── 刪除規則
autopilot.history ── 執行記錄
```

每次 `create` / `update` 都會**在寫入時**驗證 `trigger_event` 與 `action` 結構——格式錯誤的規則會立即被拒絕，而非稍後在 `autopilot_history` 中靜默失敗。每次執行（成功、錯誤或斷路器轉換）都會附加一列，帶有狀態與錯誤脈絡。

---

## MCP → events.db 橋接

引擎的行程內生產者可直接 `send()` 到 broadcast 匯流排上。但源自*獨立行程*的事件——MCP server、Python adapter——無法觸及記憶體內的 channel。橋接是位於 `<home>/events.db` 的 SQLite 事件匯流排：

```
MCP server (獨立行程)                  Gateway 行程
─────────────────────────────         ────────────────
emit "task.created" ──> events.db ──> 背景讀取器
                        ┌──────────┐   fetch_since(last_id)
                        │ id (PK,  │        |
                        │  AUTOINC)│        v
                        │ ts       │   重新以 AutopilotEvent
                        │ type     │   發出到 broadcast
                        │ payload  │   匯流排上
                        └──────────┘
```

此 SQLite 匯流排取代了舊版的 `events.jsonl` 檔案匯流排。使其安全的欄位與保證：

- **WAL 模式 + busy_timeout** — 多個行程可並行寫入而不會損毀行（沒有 `events.jsonl` 輪替競態，沒有半行隱患）。
- **單調遞增 `id INTEGER PRIMARY KEY AUTOINCREMENT`** — 讀取器只需輪詢 `fetch_since(last_id)`；不需游標檔，不需重讀。
- **內建保留期** — 背景 prune 會刪除超過 7 天截止點的列，因此表格永不無界增長。

---

## 為什麼這很重要

### 會反應的 Agent，而不只是回應

沒有 autopilot，agent 只有在人類傳訊時才行動。有了規則，機群會對自身的維運事件作出反應——新 task 自動路由給正確的專員、閒置 agent 收到提醒、事故觸發通知——全都無需人類介入。

### 迴圈無法拖垮引擎

任何事件驅動系統中最危險的故障就是自我增強迴圈。每條規則的斷路器將失控規則隔離至 60 秒逾時，而其他每條規則照常運作。壞規則只會降級為「被節流」，永遠不會變成「引擎當機」。

### 從構造上即安全

`run_skill` 對 agent 與 skill 名稱進行 allowlist 驗證，*並*確認解析後的路徑停留在 skills 目錄內。`eq null` 無法匹配不存在的欄位。CRUD 在寫入時驗證結構。每道閘門皆 fail closed。

### 跨行程而無檔案匯流排的隱患

`events.db` 橋接讓獨立的 MCP 行程能餵入引擎，而無共享 JSONL 檔的輪替競態、半行損毀或權限怪癖。WAL 處理並行；單調 id 處理排序；prune 處理增長。

---

## 總結

智慧家庭自動對事件作出反應，而斷路器面板防止失控迴圈把房子燒掉。DuDuClaw 的 Autopilot 規則引擎將兩者帶給你的 agent 機群：一條 broadcast 事件匯流排、帶有 `all`/`any` 條件的宣告式規則、三種經驗證的動作型別，以及每條規則一個三態斷路器。行程內生產者直接發布；行程外生產者透過 WAL 支撐的 `events.db` 橋接餵入。你可以放心讓它無人監督運行的自動化——因為斷路器會在迴圈造成損害之前先跳脫。
