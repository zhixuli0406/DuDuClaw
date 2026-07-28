# OS 原生感知與主動關懷

> 你的 AI 員工會注意到你在這台機器上的工作方式——只看你指定它看的、只記彙總,而且只在值得打擾你的時候開口。

---

## 這是什麼

一層 opt-in 的感知層(v1.42–1.46),讓指定的 agent 感知本機 OS 活動——指定資料夾裡的檔案變化、目前前景是哪個 app——並以三種方式運用:

1. **自動化** — OS 事件餵進既有的 autopilot 規則引擎(`os_file` / `os_frontmost` 事件),OS 頁提供一鍵範本。
2. **記憶** — 每日蒸餾把原始事件流濃縮成幾條關於使用者工作習慣的 temporal memory 事實。
3. **關懷** — 每小時一次的主動檢查讀取當日 app 使用摘要,可能往你最近的對話送出一則簡短、有用的訊息。

一切預設關閉。每一層都是 `agent.toml` 裡各自獨立的開關。

## 感知層

**檔案系統監看**(`os_events.rs`):`[capabilities] os_native = true` 且 `[os_watch] paths` 清單非空的 agent,會得到一個 per-agent、有防抖與速率限制的 watcher(`debounce_ms`、`max_events_per_min`)。事件進入 autopilot 引擎本來就在消費的同一條 broadcast bus。Watcher 支援熱重載:透過 `agents.update` 編輯 `[os_watch]` 會就地停止/重啟該 agent 的 watcher,不必重啟 gateway。Per-agent 計數器持久化到 `<home>/os_watch_stats.json`,供 `os_watch_status` MCP 工具讀取。

**前景輪詢**(`os_frontmost.rs`):`[os_watch] frontmost_poll_secs` 為正值時,啟動一條低頻輪詢迴圈。只有前景 app 或視窗真的變了才會發事件——閒置的桌面什麼都不產生。每次 app 切換往 `<home>/os/<agent>/frontmost-<date>.jsonl` 的每日 JSONL log 追加一行 `{"ts","app"}`;只保留今天與昨天的檔案。輪詢器是純感知來源:它從不計算 idle 狀態(那仍屬 heartbeat 路徑)。

## 資料最小化

隱私規則在蒐集端就強制,不是只在寫入端:

- **視窗標題永不寫入磁碟。** 每日 log 只存 app 名稱 + 時間戳——關懷摘要需要的最小集合。
- **檔名不離開事件本身。** Footprint 彙總在做任何事之前,先把每個路徑縮減到其所在目錄,所以風險最高的子字串(`quarterly-layoffs-draft.docx`)在源頭就被丟棄。
- **每個感知到的字串都先過感知消毒器**(`sanitize_perception_text`),才進入 prompt、彙總 key 或記憶——所有 OS 感知路徑共用同一道邊界。
- **未 opt-in 的 agent 永不被追蹤。** 沒開 footprint 旗標的 agent 根本不會被加進記憶體內的追蹤 map,而不只是持久化時跳過而已。

## Footprint → Temporal Memory

`footprint_distill.rs` 訂閱同一條事件 bus,按 agent、按 UTC 日彙總:前景 app 秒數、活躍目錄計數、逐小時活動直方圖。每天一次(追蹤日換日時),把結束的那天蒸餾成**至多三條**確定性的 `(subject="user", predicate, object)` triple,經 `store_temporal` 寫入——寫入率是 O(agents × days),絕不是每事件一列。既有的 supersession chain 會自動把昨天的 `daily_active_app` 事實收尾,Ebbinghaus retrievability 讓持續被喚起的內容排名更高。

要緊的細節:

- 以 `[os_watch] footprint = true` opt-in,疊在 `os_native` 之上——只開檔案系統監看不會取得 footprint 記憶。
- Bucket 在每次 distill tick 快照到 `<home>/os/<agent>/footprint-aggregate.json`(原子 tmp+rename);重啟最多丟一個檢查間隔,不會丟掉一整天。
- 每筆寫入蓋上 `origin = "agent_derived"`(信任天花板 0.6,依 v1.41 寫入時 origin 綁定),並帶敏感度標籤:`daily_active_app` / `active_hours` → Personal,`active_directory` → Internal。

## 主動關懷

Heartbeat 排程器按各 agent 的排程跑主動檢查:

```text
check due? → quiet hours? skip → rate limit? skip
  → call Claude with the check prompt + MCP tools
  → response contains PROACTIVE_OK → discard silently
  → otherwise → send to the notify target
```

- **內建預設檢查**:`os_native` agent 沒有手寫的 `PROACTIVE.md` 時,使用 `DEFAULT_OS_CARE_CHECKS`——一套保守的內建規則(「連續工作 2 小時 → 建議休息;23:00 之後還在做 → 一句簡短關心;會議佔滿一天 → 主動提議整理待辦」)。預設答案是 `PROACTIVE_OK`——寧靜默,勿噪音。過去 agent 蒐集了一整天資料卻從不行動,因為沒有 `PROACTIVE.md` 時檢查會靜默跳過。
- **OS context**:`frontmost_daily_summary` 讀每日 JSONL log,產出 app 用時排行摘要(超過 30 分鐘的空檔視為「走開了」,不足 1 分鐘的 app 丟棄,取前 6 名,app 名稱經消毒)。只有 app 名稱與時長,永無視窗內容。
- **安靜時段 + 速率限制**:支援時區、可跨午夜的安靜時段,加上滑動視窗的每小時訊息上限。
- **通知目標 fallback**:沒有明確目標時,訊息送往該 agent 在 `sessions.db` 中最近一個*可推播*的通道對話(Discord/Telegram/LINE/Slack/WhatsApp/Feishu/Google Chat/Teams——WebChat 是 pull-only,排除在外)。

## 主動閘

規則驅動的打擾另有一道獨立的 LLM 評分閘(`proactive_gate.rs`),以新的 autopilot action `proactive_notify` 落地——確定性的 `notify` 規則原封不動,照舊觸發。流程(ContextAgent 式主動評分):

1. 消毒所有事件文字,以 persona context 與當前可打擾度分數組出評分 prompt。
2. 一次 utility LLM 呼叫回傳 `proactive_score ∈ 1..=5`。
3. 動態門檻 `base + round(interruptibility × 2)`——使用者越忙,門檻越高。
4. 分數 ≥ 門檻 → 放行;其餘一切——包括 LLM 錯誤、解析失敗、逾時——→ **壓下**(fail-closed:不確定就絕不打擾)。

預設基礎門檻 3,預設每個 agent 每滾動小時至多 4 則主動通知。每個決策都往 `<home>/proactive_gate.jsonl` 寫一行稽核。

## 一鍵範本

過去要寫一條 OS 規則,得先背事件名稱、手寫 condition JSON——所以沒人寫。OS 頁現在提供範本卡片(`OsAutomationTemplates.tsx`):**檔案範本**(「這個資料夾有東西進來時,行動」,並同時透過 `os.settings.update` 追加監看路徑),與 **app 範本**(「這個 app 到前景時,在這個通道提醒我」)。各自填一張小表單,經正常的 `autopilot.create` RPC 建立真正的 autopilot 規則——伺服器端驗證、斷路器保護,與任何手寫規則無異。

## Opt-in 開關與版本配額

| 層 | 開關 | 預設 |
|---|---|---|
| OS 原生席位 | `[capabilities] os_native = true` | 關 |
| 檔案系統監看 | `[os_watch] paths = [...]` | 空(不監看任何東西) |
| 前景輪詢 | `[os_watch] frontmost_poll_secs` | 0 / 未設定(不輪詢) |
| Footprint 記憶 | `[os_watch] footprint = true` | 關 |
| 主動閘 | `[proactive] enabled = true` | 關 |

版本分級是**配額鎖,不是能力鎖**:個人版恰好允許一個 OS 原生 agent;付費層無上限。任何版本都不移除、不降級任何功能——只限制同時能感知 OS 的席位數。
