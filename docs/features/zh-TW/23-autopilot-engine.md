# Autopilot 規則引擎

> 事件驅動的自動化——當*這件事*發生，就做*那件事*，並配備一塊在規則失控時自動跳脫的斷路器面板。

---

## 比喻：配有斷路器面板的智慧家庭

智慧家庭運行於簡單的規則：*當前門在日落後開啟，打開走廊燈。* *當溫度降到 18°C 以下，啟動暖氣。* 你不需要監督這些：它們會自行觸發，隨事件發生而即時反應。

但天真的規則引擎有一種故障模式：某條規則觸發了一個事件，而該事件又再次觸發同一條規則。燈亮起，觸動了動作感測器，於是燈又亮起，無限循環。設計良好的家庭配有**斷路器面板**。若某個迴路在短時間內抽取過多電流，斷路器就會跳脫並將其隔離，讓房子其餘部分繼續運作。

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

匯流排容量為 8,192（足以吸收一波事件爆發），不會讓緩慢的資料庫寫入對生產者形成反壓。若消費者落後到足以丟失事件，引擎會記錄落後計數，讓維運者可調查緩慢的 DB 或調高容量。

---

## 事件型別

規則可以訂閱十三種 `AutopilotEvent`。每種都攜帶一個 payload，會被攤平成一個欄位映射供條件比對：

| 事件 | `event_name` | 觸發時機 | 關鍵欄位 |
|------|--------------|----------|----------|
| **TaskCreated** | `task_created` | Task Board 上出現新 task | task 物件（id、title、priority……） |
| **TaskUpdated** | `task_updated` | task 的欄位被更新 | task 物件 |
| **TaskStatusChanged** | `task_status_changed` | task 在狀態間移動 | `task_id`、`from`、`to`、task 物件 |
| **ActivityNew** | `activity_new` | Activity Feed 出現新的一筆 | activity 物件 |
| **ChannelMessage** | `channel_message` | channel 上收到訊息 | `channel`、`agent_id`、`text` |
| **AgentIdle** | `agent_idle` | 某個 agent 進入閒置 | `agent_id`、`idle_minutes` |
| **CronTick** | `cron_tick` | 從不觸發：有這個型別，但沒有任何程式送出它。v1.67.1 起新規則不能用它（見下文） | `now` |
| **RunAtRisk** | `run_at_risk` | 預測判斷執行中的任務正走向失敗 | `agent_id`、`session_id`、`score`、`level`、`reasons` |
| **OsFileEvent** | `os_file` | agent 的 `[os_watch]` 監看到檔案變動 | `agent_id`、`path`、`kind`（created / modified / removed / renamed）、`file_name`、`extension` |
| **OsFrontmostEvent** | `os_frontmost` | 前景 App 或視窗標題改變 | `agent_id`、`app`、`window_title`、`prev_app` |
| **Tick** | `tick` | 常駐感知來源（`[[tick.sources]]`，v1.68.0 起可在系統設定 → 進階設定 → 自動化引擎 →「常駐感知」編輯）產生一筆觀測 | `source`、`ts`、擷取欄位及其 `prev_` / `delta_` / `pct_` 衍生欄位 |
| **SecurityEvent** | `security_event` | warning 或 critical 等級的稽核事件，或安全態勢改變（只接受閘道內部產生的事件，見「只限閘道內部的事件」） | `severity`、`event_type`、`agent_id`、`source` |
| **OdooEvent** | `odoo_event` | Odoo 輪詢或 `POST /webhook/odoo` 回報 ERP 變更 | `event_type`、`model`、`record_id`、`record`（其頂層純量欄位也會攤平） |

規則會宣告它關心哪個 `trigger_event`，因此 `channel_message` 規則不會看到 `task_created`。`autopilot.create` 接受 `cron_tick` 以外的十二個名稱；`autopilot.update` 只在規則原本就是 `cron_tick` 時接受它，讓這種已存規則能照常儲存；把其他規則改成 `cron_tick` 會和建立時一樣被拒絕。要定時執行的工作請交給排程器（「例行工作」頁，或帶 `schedule` 的 `tasks_create`）。這十三個名稱仍都是 `sequence` 規則合法的 `first` / `then` 事件。引擎另有一個內部事件 `cep_trigger`，由序列比對器發出以觸發已比對成功的規則，不能當 `trigger_event` 使用。

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
| `not_in` | 欄位不是值陣列中的任何一個 |
| `gt` / `gte` | 欄位在數值上大於（或等於） |
| `lt` / `lte` | 欄位在數值上小於（或等於） |
| `contains` | 字串包含子字串，或陣列包含某值 |

**不存在**的欄位永遠無法滿足任何比較，包括 `eq null`。這是刻意的：曾經允許不存在的欄位匹配 `eq null`，導致某條規則對每個事件大量觸發。缺失即不匹配，沒有例外。

### 沒有條件

沒有條件的規則，在觸發事件每次發生時都會執行。「沒有條件」包括：建立時沒帶 `conditions`、`null`、`{}`、`{ "all": [] }`。`{ "any": [] }` 可以儲存，但永遠不會匹配。

**v1.67.1 行為變更。** `autopilot.create` 沒收到 `conditions` 時會存成 `{}`。v1.67.1 之前，引擎把 `{}` 當成欄位為空的單一條件，這種規則從來不會觸發。v1.67.1 起，這些規則會在觸發事件每次發生時執行。`null` 與 `{ "all": [] }` 本來就是全部匹配。要找出會開始觸發的已啟用規則，在 gateway 主機上執行下列指令，不想要的就停用或編輯：

```bash
sqlite3 ~/.duduclaw/autopilot.db \
  "SELECT id, name, trigger_event FROM autopilot_rules
   WHERE enabled = 1 AND sequence IS NULL AND trim(conditions) = '{}';"
```

在儀表板用編輯按鈕打開這種規則，會看到「目前沒有設定條件：每次觸發事件發生時都會執行。」

### 寫入時驗證

`autopilot.create`、`autopilot.update`（以及啟用歸納出的規則）在儲存前會檢查條件樹。每個節點都必須是物件：值為條件陣列的 `all` / `any` 群組，或是帶非空字串 `field` 的單一條件，`op` 可省略（預設 `eq`），有寫就必須是上表的運算子。巢狀最多 16 層。拒絕訊息會寫出出錯節點的路徑，例如 `conditions.all[1]: a condition needs a non-empty "field" (or use an "all"/"any" group)`。v1.67.1 之前這種條件會被存下來，然後永遠不匹配。

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

文字欄位（`prompt`、`text`）是範本：`{task.title}` 會換成事件裡的欄位值。v1.82 起雙大括號 `{{task.title}}` 視為同一個欄位；過去會在文字裡多留一個 `}`。任務事件的資料放在 `task` 底下，所以標題是 `task.title`，不是 `title`。不存在的欄位會換成空字串。

---

## 執行前等人核准（`require_approval`）

任何動作都可以加上 `"require_approval": true`。這種規則被觸發時**不會直接執行**動作。**v1.82 行為改變：** v1.81 以前這個設定在運作中的閘道沒有作用——動作會立刻執行，歷史紀錄顯示「成功」。升級後，原本就設了 `require_approval: true` 的規則會開始停下來等核准。

```
規則觸發 ─> 依實際執行時的樣子套好動作內容
        ─> 建立「一張」核准單（類型 autopilot_action），凍結：
             規則編號＋規則版本、觸發事件、
             套好的動作（對象＋提示／技能／訊息內容）
        ─> 歷史紀錄：等待核准（awaiting_approval，不執行任何事）

管理員在儀表板核准 ─> 距離核准不到 15 分鐘？                       ── 否 ─> expired
                     規則仍存在、仍啟用、版本與凍結時相同？        ── 否 ─> stale_after_approval
                     規則的斷路器是關閉的？                        ── 否 ─> blocked_breaker_open
                              │ 是（檢查期間規則的修改會先等待）
                              v
                        核准單用掉（只能用一次）─> 凍結的動作執行一次 ─> success／failure
管理員拒絕，或 1 小時沒人處理 ─> denied／expired（不執行）
```

- **誰能決定**：只有目前的管理員，在儀表板（收件匣／審批）決定。通訊頻道只會收到沒有按鈕、也不含動作內容的提醒：只說有一個自動化規則的動作等管理員到儀表板審核、期限與編號（語言依 `[general] default_language`）。
- **管理員看得到會執行的全部內容**：收件匣為這類核准單提供專用畫面，一律完整顯示凍結的動作——完整的提示（`run_skill` 含技能本文）或完整訊息與它的通道和傳送對象、目標 AI 員工、佇列通道、觸發事件的 AI 員工、觸發事件識別、規則名稱與完整版本摘要、決定期限與最晚執行時間。內容不截斷，過長時可捲動。雙向文字控制字元、控制字元與零寬／隱藏字元會以 `\u{202E}` 這類代碼顯示。凍結的動作超過 64 KiB 時不會送審（`approval_error`，不執行）。
- **核准的就是執行的**：核准單裡是套好的提示或訊息；核准後只執行這一份、只執行一次。完整內容顯示前「核准」按鈕無法按；核准時必須附上畫面上這張核准單的摘要值（`expected_digest`），閘道對任何用戶端都一樣：沒有附上或與儲存的核准單不符就拒絕核准。拒絕不需要摘要值。核准單內容與凍結的不符時一律拒絕（`approval_error`）。
- **規則改過就不執行舊的核准**：核准使用前規則被修改（觸發、條件、動作）、停用或刪除，已核准的動作不執行（`stale_after_approval`）。最後一次檢查與用掉核准單是在規則修改必須等待的期間內完成（這個閘道與其他行程都一樣），修改無法插在中間。只改名稱不算修改。
- **迴圈仍會擋下**：已核准的動作要執行時，若這條規則的斷路器是開啟的，就不執行（`blocked_breaker_open`），核准單用掉並通知管理員。自主暫停也會在這時再檢查一次（`skipped`）。
- **核准的有效時間很短**：已核准的動作只會在決定後 **15 分鐘**內執行（固定值）。超過這個時間——包括核准時間晚於現在——記為 `expired`，核准單用掉並通知管理員。
- **送不了審就不執行**：無法建立核准單時（核准服務沒有接上、核准資料庫拒收——例如套好的文字含有 API 金鑰——凍結的動作超過 64 KiB，或任何儲存錯誤），動作不執行。歷史紀錄寫 `blocked_no_approval_service` 或 `approval_error`，並通知管理員（動態牆、儀表板、已綁定通訊頻道的管理員），同一條規則每小時最多一次。（若這次觸發已記入 `autopilot.db`、之後才建立核准單失敗，這次觸發仍會有自己的動態牆項目，只有聊天通知受此限制。）
- **每個結果都會通知，閘道中斷也一樣**：每次需要核准的觸發結束時（已執行、執行失敗、被拒絕、逾時、規則已變更、已攔下、結果不明），都只有一筆歷史紀錄和一則動態牆項目。結果、它的歷史紀錄，以及「還有哪些通知沒送出」會一起保存；閘道若在寫入動態牆或送出聊天通知前中斷，持有資料目錄鎖的閘道重新啟動後會補送，執行期間也每隔幾秒檢查一次。聊天通知要確實送出，或已排入勿擾時段後的佇列，才算完成；送出失敗會以逐漸拉長的間隔重試（30 秒、2 分鐘、8 分鐘……最多 6 次），動態牆項目不受影響。儀表板即時事件盡力送出。
- **必須讓人知道的結果不會被靜音**：`execution_uncertain`、`stale_after_approval` 與 `blocked_breaker_open` 一律推播到管理員的聊天頻道（每次需要核准的觸發一次），不受「每條規則每小時一次」限制；其他需要通知的結果，同一條規則每小時最多推播一次。`execution_uncertain` 的通知會說明怎麼處理：查看目標 AI 員工最近的活動（或目標聊天室），若動作沒有執行，請手動重新觸發這條規則。
- **只有一個閘道執行規則**：只有持有資料目錄鎖的閘道行程會把 `events.db` 送進自動化事件匯流排、執行事件序列比對與執行規則；同一個資料目錄上的第二個閘道一律不做。另外，同一個觸發事件對同一個規則版本最多只建立一張核准單（核准紀錄中有持久的識別鍵），同一個事件不會產生兩次核准。
- **重新啟動**：已核准的動作由持有資料目錄鎖的閘道執行，通常在決定後幾秒內。閘道停機期間核准的動作，重新啟動後只有在距離核准不到 15 分鐘時才會執行（否則記為 `expired` 並通知）。執行途中重新啟動也不會執行兩次：還沒開始的會重新檢查，最多執行一次；可能已執行的記為 `execution_uncertain`、通知管理員，不會重做。
- **值必須是布林**：`autopilot.create`／`autopilot.update` 會拒絕不是 `true` 或 `false` 的 `require_approval`。已存的規則若是其他值（v1.82 以前寫入或手動修改），一律當作需要核准，並在啟動日誌提醒。
- **狀態會保存，且只有擁有者能讀**：每次需要核准的觸發都在 `autopilot.db` 留一列（`autopilot_approval_dispatch`：凍結的動作、摘要值、事件識別鍵、核准單編號與結果）。`autopilot.db` 的開啟方式與 `approvals.db` 相同：只有擁有者可讀寫（`0600`，WAL／SHM 檔也一樣），且絕不經由符號連結開啟；無法這樣開啟時，自動化引擎不會啟動，任何規則都不會執行。`duduclaw-import` 接受這個 v1.82 的 `autopilot.db`（catalog `autopilot-v182.sql`），且不讀取這張表。

**已知限制**

- `[approval] forbid_self_approval` 不涵蓋撰寫規則的人：核准單由背景的 `autopilot` 送出，規則也沒有記錄建立者，所以撰寫規則的管理員可以核准它的動作。
- 閘道若剛好在聊天通知送出之後、記錄完成之前中斷，重新啟動後會再送一次這則通知（動態牆項目不會重複）。
- 核准的可信度建立在 AI 員工無法寫入閘道的資料檔（`approvals.db`、`autopilot.db`）上——所有核准都一樣——原生 Linux 隔離模式提供這道邊界。

需要核准的規則的歷史結果：`awaiting_approval`、`success`／`failure`（核准後執行）、`denied`、`expired`、`stale_after_approval`、`blocked_breaker_open`、`blocked_no_approval_service`、`approval_error`、`execution_uncertain`、`skipped`（執行時遇到自主暫停，或同一個事件已為這條規則送審過）。

---

## 斷路器

每條規則都有自己的三態斷路器。這是引擎對抗自我增強迴圈的防線：某條規則的動作產生了一個事件，而該事件又再次觸發同一條規則（`task_created → delegate → agent 建立 task → task_created → ...`）。

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
    "type": "delegate",
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
    "type": "notify",
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

每次 `create` / `update` 都會**在寫入時**驗證 `trigger_event`、`conditions` 條件樹與 `action` 結構。格式錯誤的規則會立即被拒絕，而非稍後在 `autopilot_history` 中靜默失敗。每次執行（成功、錯誤或斷路器轉換）都會附加一列，帶有狀態與錯誤脈絡。

### 儀表板的規則表單

在「設定 → 自動化」建立與編輯規則。v1.67.1 之前，這個表單送出的動作用 `agent_id`、`prompt_template`，伺服器要的是 `target_agent`、`prompt`；它還提供不存在的 `schedule` 觸發事件，寫入的 `from_status`／`idle_minutes`／`cron` 也不是欄位／運算子／值的條件格式。伺服器每次都拒絕儲存，所以從儀表板建不出任何規則。v1.67.1 起：

- 觸發事件清單就是 `autopilot.create` 接受的十二個事件，包括 `tick`（監控來源有新資料時）與 `odoo_event`（Odoo 資料變動時）。已存規則的觸發事件若不在清單裡（例如 `cron_tick`），會照原值顯示並保留。
- 條件是一列一列的「欄位／比較方式／比較值」，用「全部符合才執行」（`all`）或「任一符合就執行」（`any`）串起來。每種觸發事件會建議欄位名稱，也可以自己輸入。`tick` 與 `odoo_event` 的欄位名稱來自你的監控來源或 Odoo 紀錄，表單會提示這一點。`gt` / `gte` / `lt` / `lte` 的比較值必須是數字；`in` / `not_in` 用逗號分隔多個值。一列都沒有時，表單存成 `{ "all": [] }`，每次事件都會執行。
- 動作有 `delegate`、`notify`、`run_skill`，必填欄位見上方表格。`notify` 可選 telegram、line、discord、slack、whatsapp、feishu、googlechat、teams、wecom、dingtalk。
- 每條規則都有編輯按鈕，編輯時只送出有改動的欄位。表單無法攤成清單的條件、事件序列規則、以及上述三種以外的動作會顯示為鎖定，原樣保留。表單不管的動作欄位（`screen`、`context_ticks`、`require_approval` 等）也原樣保留；新規則無法在表單加上它們，請改用 `autopilot.create` / `autopilot.update`。
- 儲存被拒絕時，對話框裡會顯示伺服器的訊息。

---

## MCP → events.db 橋接

引擎的行程內生產者可直接 `send()` 到 broadcast 匯流排上。但源自*獨立行程*的事件（MCP server、Python adapter）無法觸及記憶體內的 channel。橋接是位於 `<home>/events.db` 的 SQLite 事件匯流排：

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

**只限閘道內部的事件。** 每位 AI 員工的 MCP 程序都能寫入 `events.db`，所以那裡的一列不代表事情真的發生。v1.82 起 `security_event` 只限閘道內部：只有閘道程序自己產生的事件（稽核同步、安全態勢改變）帶有閘道來源標記；從 `events.db` 讀到的 `security_event` 會被忽略並記錄，不會觸發任何規則，事件序列（`sequence`）規則也一樣。之後的閘道內部事件（例如審批事件）會加進同一份清單。

---

## 為什麼這很重要

### 會反應的 Agent，而不只是回應

沒有 autopilot，agent 只有在人類傳訊時才行動。有了規則，機群會對自身的維運事件作出反應：新 task 自動路由給正確的專員、閒置 agent 收到提醒、事故觸發通知，全都無需人類介入。

### 迴圈無法拖垮引擎

任何事件驅動系統中最危險的故障就是自我增強迴圈。每條規則的斷路器將失控規則隔離至 60 秒逾時，而其他每條規則照常運作。壞規則只會降級為「被節流」，永遠不會變成「引擎當機」。

### 從構造上即安全

`run_skill` 對 agent 與 skill 名稱進行 allowlist 驗證，*並*確認解析後的路徑停留在 skills 目錄內。`eq null` 無法匹配不存在的欄位。CRUD 在寫入時驗證結構。每道閘門皆 fail closed。

### 跨行程而無檔案匯流排的隱患

`events.db` 橋接讓獨立的 MCP 行程能餵入引擎，而無共享 JSONL 檔的輪替競態、半行損毀或權限怪癖。WAL 處理並行；單調 id 處理排序；prune 處理增長。

---

## 總結

智慧家庭自動對事件作出反應，而斷路器面板防止失控迴圈把房子燒掉。DuDuClaw 的 Autopilot 規則引擎將兩者帶給你的 agent 機群：一條 broadcast 事件匯流排、帶有 `all`/`any` 條件的宣告式規則、三種經驗證的動作型別，以及每條規則一個三態斷路器。行程內生產者直接發布；行程外生產者透過 WAL 支撐的 `events.db` 橋接餵入。你可以放心讓它無人監督運行的自動化，因為斷路器會在迴圈造成損害之前先跳脫。
