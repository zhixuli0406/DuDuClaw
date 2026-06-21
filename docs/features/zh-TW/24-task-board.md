# Task Board 與 Activity Feed

> Agent 即隊友的任務管理——一塊由人類與 AI agent 共同操作、認領與回報站立會議的共享 Kanban 看板。

---

## 比喻：共享的 Kanban 看板

想像辦公室牆上一塊實體 Kanban 看板。卡片在欄位間移動——*待辦 → 進行中 → 完成*——還有一條*受阻*車道接住卡住的工作。人類產品負責人釘上新卡片。隊友走過來，從「待辦」撕下一張卡片，貼到「進行中」自己的名字底下，站立會議時報告做了什麼。

現在讓一部分隊友變成 AI agent。它們讀同一塊看板。它們認領同一批卡片。它們發出同樣的站立會議更新。唯一的差別是它們*如何*碰到看板：人類在 web dashboard 上用滑鼠；agent 從自己的推理迴圈裡呼叫工具。

DuDuClaw 的 Task Board 正是如此——一塊由 SQLite 支撐的看板，有兩道門通向它。一道給人類（web dashboard），一道給 agent（MCP 工具）。兩者移動的是同一批卡片。Activity Feed 則是大家都讀的站立會議紀錄。

---

## 一個儲存層，兩道存取層

看板與動態都活在單一 SQLite 資料庫中（`tasks.db`，WAL 模式，5 秒 busy_timeout 以支援多行程安全）。所有操作都匯流到單一 `TaskStore`——但有兩種截然不同的方式抵達它：

```
   人類                                      AGENT
   (web dashboard)                          (在推理迴圈內)
        |                                        |
        v                                        v
 ┌──────────────────┐                 ┌────────────────────────┐
 │ Dashboard WS RPC │                 │  Agent-facing MCP tools │
 │  tasks.list      │                 │   tasks_list            │
 │  tasks.create    │                 │   tasks_create          │
 │  tasks.update    │                 │   tasks_update          │
 │  tasks.remove    │                 │   tasks_claim           │
 │  tasks.assign    │                 │   tasks_complete        │
 │  activity.list   │                 │   tasks_block           │
 │                  │                 │   activity_post         │
 │                  │                 │   activity_list         │
 └────────┬─────────┘                 └───────────┬────────────┘
          |                                       |
          +──────────────┐         ┌──────────────+
                         v         v
                    ┌─────────────────────┐
                    │   TaskStore (單一)   │
                    │   tasks.db (SQLite)  │
                    │   - tasks   table    │
                    │   - activity table   │
                    └─────────────────────┘
```

兩道門沒有哪道是「主要」的。人類建立卡片、agent 認領它；agent 建立子任務、人類重新指派它。因為兩者寫的是同一批列，看板永遠是單一事實來源——不需同步、不需鏡像、不會漂移。

### Dashboard RPC 集合（人類）

透過已驗證的 dashboard WebSocket 提供服務。這些驅動 web UI 的 Task Board 頁面與 Activity Feed。

| RPC | 用途 |
|-----|------|
| `tasks.list` | 為看板視圖列出／過濾任務 |
| `tasks.create` | 建立卡片；廣播一個 `activity.new` 事件 |
| `tasks.update` | 編輯欄位／在欄位間移動卡片 |
| `tasks.remove` | 刪除卡片 |
| `tasks.assign` | 將卡片重新指派給某個 agent（`tasks.update` 的薄包裝） |
| `activity.list` | 讀取近期 Activity Feed 事件 |

### MCP 工具集合（agent）

透過 MCP server 暴露給 AI runtime。這些讓 agent 能看到自己的佇列、認領工作、回報進度、完成卡片——全程不需人類介入。

| MCP 工具 | 用途 |
|----------|------|
| `tasks_list` | 查看你的佇列（預設為呼叫者；`assigned_to='*'` 看全部） |
| `tasks_create` | 新增卡片；`created_by` 自動設為呼叫者 |
| `tasks_update` | 編輯欄位（標題／描述／優先級／標籤） |
| `tasks_claim` | 原子性接手一張未指派的卡片並設為 `in_progress` |
| `tasks_complete` | 將卡片標記為 `done`，可附完成摘要 |
| `tasks_block` | 將卡片標記為 `blocked`，需附原因 |
| `activity_post` | 發一則進度紀錄，*不*變更任務狀態 |
| `activity_list` | 讀取近期動態（預設為呼叫者） |

這正是 Multica「Agent 即隊友」設計的核心：agent 不只是你呼叫的一個函式——它是一位會盯著看板、撿起卡片、發站立會議的同事。

---

## 任務生命週期

一張卡片有一個 `status` 與一個 `priority`。狀態在一個小而可預測的流程中移動：

```
                tasks_create / tasks.create
                          |
                          v
                      ┌────────┐
                      │  todo  │◄──────────────┐
                      └───┬────┘               │
                          │ tasks_claim        │ （透過
                          v                    │  tasks.update
                   ┌─────────────┐             │  重新開啟）
                   │ in_progress │             │
                   └──┬───────┬──┘             │
       tasks_block    │       │  tasks_complete│
                      v       v                │
                  ┌─────────┐ ┌──────┐         │
                  │ blocked │ │ done │─────────┘
                  └────┬────┘ └──────┘
                       │  （解除阻塞 → tasks_update 回到 todo / in_progress）
                       └──────────────────────────────►
```

完成卡片會自動蓋上 `completed_at`。阻塞卡片會記錄一個顯示在卡片上的 `blocked_reason`。認領是一個 **compare-and-set**：`tasks_claim` 只在卡片目前未指派時才成功，所以兩個 agent 不會搶到同一張卡片。

### Status 值

| Status | 意義 |
|--------|------|
| `todo` | 已建立、尚未開始（建立時的預設） |
| `in_progress` | agent 或人類正在處理 |
| `blocked` | 卡住——攜帶一個 `blocked_reason` |
| `done` | 完成——蓋上 `completed_at` |

### Priority 值

| Priority | 排序（urgent → low） |
|----------|---------------------|
| `urgent` | 0（最先浮現） |
| `high` | 1 |
| `medium` | 2（預設） |
| `low` | 3 |

任務也攜帶 `tags`、選用的 `parent_task_id`（用於子任務，附循環偵測，讓卡片不能成為自己的祖先）、`created_by` 與 `assigned_to`。

---

## 即時 Activity Feed

每一次有意義的狀態變更都會寫入一列 `activity`，並即時推送給 dashboard 訂閱者。當 web UI 建立卡片或 agent 移動卡片時，gateway 會透過 WebSocket 廣播一個 `activity.new` 事件——所以 Activity Feed 不需重新整理即會更新。

```
agent 呼叫 tasks_claim
        |
        v
TaskStore: UPDATE status=in_progress, assigned_to=agent
        |
        +─► append_activity(task_assigned)
        |
        +─► broadcast_event("activity.new", …)  ──►  Dashboard
                                                     （即時動態更新）
```

`activity_post` 的存在正是為了讓 agent 能說「仍在處理中，遷移已過半」——*不*變更卡片狀態的站立會議留言，而非欄位移動。

---

## 待辦任務自動注入 Prompt

看板不只是 agent *可以*去查的東西——它們未完成的工作會被自動推入它們的上下文。當 gateway 組裝 agent 的 system prompt 時，會建立一個 `## Your Task Queue` 區段：

```
build_pending_tasks_section(agent_id):
     |
     v
拉取此 agent 的未完成任務 (in_progress → todo → blocked)
     |
     v
依優先級排序 (urgent → low)，最多取 5 筆
     |
     v
渲染為項目符號 + MCP 工具提醒：
  ## Your Task Queue (7 pending)
  1. [urgent] Fix Discord reconnect loop [in progress]
  2. [high]   Draft Q3 release notes
  3. [medium] Review marketplace skill PR — blocked: needs API key
  +2 more — call tasks_list to see all

  Use `tasks_list`, `tasks_claim`, `tasks_update`,
  `tasks_complete`, `tasks_block` to manage these,
  and `activity_post` to report progress.
```

如果 agent 沒有未完成任務，此區段會被完全省略以保持 prompt 精簡。儲存層透過 gateway 持有的單一共享 SQLite 連線讀取（避免高流量頻道回覆上的 WAL 寫鎖爭用），僅在注入尚未執行時才退回逐次開啟。

---

## 排程器層級的拉取：喚醒閒置 Agent

自動注入涵蓋了*已經*在回覆訊息的 agent。但大多數正式環境的 agent 設定 `heartbeat.enabled = false`，閒置等待直到有頻道訊息抵達——所以指派給它們的卡片永遠不會被撿起。

`HeartbeatScheduler` 在**排程器層級**解決這點：在每個 30 秒 tick，它掃描*整個* agent registry（不只是啟用 heartbeat 的 agent），並對每個執行 `poll_assigned_tasks`：

```
每 30 秒排程器 tick：
     |
     v
對 registry 中的每個 agent（無論 heartbeat.enabled）：
     |
     v
poll_assigned_tasks(agent)：
   - 有指派給此 agent 的最高優先級 `todo` 嗎？
   - 有任何 `in_progress` 任務停滯超過 30 分鐘 (updated_at 過舊)？
     |
     v
入列一則喚醒訊息 (message_queue.db) 提醒 agent
去 tasks_claim 那筆待辦，或對停滯的那筆 activity_post 進度
     |
     v
冷卻閘門：若同一則提醒在 < 1 小時內已送出則略過
   （以既有佇列列上的 LIKE 標記判斷——無額外 schema）
```

少了這個，Multica「Agent 即隊友」設計就退化成「agent 只在有頻道訊息抵達時才行動」。1 小時的 `LIKE` 標記冷卻避免 30 秒 tick 對同一個 agent 連發重複提醒。

---

## 為什麼這很重要

### 真正的共享工作空間

人類與 agent 不是兩套以同步任務硬接在一起的獨立系統。它們寫的是同一批 SQLite 列。在 dashboard 建立的卡片，正是 agent 透過 MCP 認領的*同一個物件*——只有一塊看板。

### 行為像隊友的 Agent

`tasks_claim`／`tasks_complete`／`tasks_block`／`activity_post` 給了 agent 與人類同事相同的動詞：接手工作、完成它、標記阻塞、發站立會議。看板因而成為一個協調介面，而非僅是一張紀錄表。

### 工作不會遺失

自動注入讓未完成任務每一輪都出現在活躍 agent 面前；排程器層級的拉取喚醒原本永遠看不到自己佇列的閒置 agent。兩者合力補上了曾經讓任務無人路由、無人處理的缺口。

### 安全的並行

認領是 compare-and-set，所以兩個 agent 不會雙重認領。父子連結有循環檢查。儲存層以 WAL 模式搭配 busy timeout 運行，所以 dashboard 與多個 agent 能並行寫入而不破壞看板。

---

## 總結

一個團隊需要一塊大家都能看見、認領、回報的看板。DuDuClaw 的 Task Board 就是那塊看板——單一 SQLite 儲存層，配上一道人類的門（dashboard RPC）與一道 agent 的門（MCP 工具）、一個用於站立會議的即時 Activity Feed、讓工作中的 agent 永遠看得到自己佇列的 prompt 自動注入，以及一個讓閒置 agent 在工作落到自己名下時仍會被喚醒的排程器層級拉取。這正是把 AI agent 從「你呼叫的函式」轉變為「會撿起卡片的隊友」的關鍵。
