# 持續任務、任務中送指示與停止

持續任務（設定與程式裡叫 responsibility）讓一位 AI 員工在一段期間內，依排程或特定事件反覆醒來做同一件事。每次醒來都會建立一個新的 goal 任務，這裡稱為「一次執行」（程式裡叫 occurrence）。它有自己的期限、迭代上限與花費上限，照一般 goal 任務的流程驗收。兩次執行之間沒有任何任務存在，所以等待期間不佔執行名額，也不呼叫模型。

同一批改動另外加了兩個對所有 goal 任務都能用的控制：任務進行中替 AI 員工補充指示（送指示），以及把任務連同子任務一起停下來。

這一版能用的介面：

| 想做的事 | 在哪裡做 |
|---|---|
| 建立、修改、暫停、恢復、停用、重新啟用持續任務 | 指令列 `duduclaw responsibility …`，每個變更都要管理員在儀表板核准 |
| 查看持續任務、執行紀錄、喚醒紀錄 | 指令列（查詢指令不需核准） |
| 送指示給進行中的 goal 任務 | 儀表板任務詳情頁 |
| 停止任務與子任務 | 儀表板任務詳情頁（立即生效），或指令列（要核准） |

儀表板的 `/responsibilities` 頁面見下方「持續任務頁面」。

## 開關與設定

兩個開關預設都關。設定寫在 `~/.duduclaw/config.toml`，每次使用都會重讀，改完不用重啟閘道。

```toml
[responsibilities]
enabled = false                    # 持續任務總開關
max_stop_at_days = 30              # 結束時間最遠可以設在幾天後（1–90，超過 90 以 90 計）
event_poll_batch = 500             # 每次巡檢最多讀幾筆事件（1–2000）
max_notifications_per_period = 10  # 每個預算期最多推播幾次，0 表示不推播
max_event_wakes_per_period = 12    # 每個預算期最多幾次由事件喚醒
operator_approval_minutes = 30     # 指令列變更在管理員核准後多久內可以套用（1–1440）

[goal_loop]
steering_enabled = false           # 任務中送指示的開關
```

- 持續任務靠派工引擎運作（`[dispatch] enabled`，v1.59 起預設開）。派工引擎關閉時，建立持續任務會被拒絕，既有的也不會醒。引擎關閉期間，事件讀取位置不會前進，所以重新打開時，那段期間的事件（最多到 `events.db` 保留的 7 天）會被當成新事件讀進來，可能喚醒持續任務。
- `[responsibilities]` 這一段寫錯格式時，整個功能視為關閉。`operator_approval_minutes` 超出範圍或讀不到時用 30。
- 把 `enabled` 改回 `false`：不再建立、不再喚醒，AI 員工的三個持續任務工具會從工具清單消失，指令列會放寬權限或花費的動作也一律拒絕。已有的紀錄保留。正在跑的那一次執行照普通 goal 任務做完；重新打開功能後，下一次巡檢會補上它的結算。
- `steering_enabled` 關閉時，新的指示送不出去；已經送出、還在等下一輪的指示照常交給 AI 員工，不會丟掉。

## 一次執行從哪裡來

喚醒來源有三種，每種都只記下一筆「該醒了」的紀錄，真正建立任務的只有派工引擎每 30 秒一次的巡檢：

- **排程**：cron 加時區，例如平日早上九點。閘道停機錯過的時間點，開機後最多補一次（只看 24 小時內最近的一個時間點），不會補成一串。
- **事件**：只接受 `task.created`、`task.updated` 兩種事件，而且事件必須屬於這位 AI 員工（事件內容的 `assigned_to` 等於它）。這兩種事件由 MCP 任務工具寫入（AI 員工的工具，或操作者的 MCP 金鑰）；在儀表板上做的變更不會寫入任何事件。合約若指定其他事件，包含 `activity.new`，會被拒絕。可以加條件，寫法與自動化規則的條件相同。
- **決定**：AI 員工在執行中用 `responsibility_ask` 問操作者一個問題，操作者回答後喚醒下一次執行。

同一時間只會有一次執行在跑。上一次還沒結束（包含停在等人處理的狀態）時，新的喚醒紀錄會留著，等上一次結束後合併成一次。兩次執行之間至少隔 `min_wake_interval_secs` 秒；執行名額被其他 goal 任務佔滿時，也先不建立，避免任務還沒開始就把期限燒掉。

### 事件喚醒的規則

- 只認訂閱生效之後發生的事件。新建訂閱、停用再啟用、功能關掉再打開，都不會把之前的事件當成新的。
- AI 員工自己製造的事件不會喚醒自己：它用自己的任務工具建立或更新的任務，以及它自己那幾次執行的任務更新。這些事件會記成被略過的喚醒紀錄，方便除錯。由一次性幫手（`spawn_agent`）或同事建立、再指派給這位 AI 員工的任務不算它自己的事件，可以喚醒它；下一條的每個預算期事件喚醒上限會限制這種情況。
- 每個預算期最多 `max_event_wakes_per_period` 次由事件喚醒，超過的事件喚醒在這一期直接略過，不會留到下一期。
- 事件內容只當資料使用：它會成為那次執行說明裡的引用區塊，並做注入掃描；任務的指派對象、期限、標籤、驗收標準、預算與工具權限全部來自持續任務本身，事件改不了。
- 事件存在 `events.db`，保留 7 天。閘道停機超過保留期，中間的事件會漏掉，系統會在動態牆記一筆缺口紀錄。
- 已知限制：閘道停機期間把功能關掉再打開，這一段切換閘道看不到，訂閱的起點不會因此重設。

## 花費與次數上限

每個持續任務都要設下列上限，單位是美分（cents，與 `monthly_budget_cents` 相同）：

| 欄位 | 意思 |
|---|---|
| `occurrence_cost_cap_cents` | 一次執行最多花多少。每一輪派工前檢查，超過就轉人工處理。花費在一輪結束後才寫入，所以不是硬上限（見下方說明） |
| `period_cost_limit_cents` | 一個預算期（`budget_period`：`day`／`week`／`month`，依 `budget_timezone` 的日曆計算）最多花多少 |
| `period_occurrence_limit` | 一個預算期最多執行幾次（1–96） |
| `occurrence_hours` | 一次執行最長幾小時（1–72），到了就轉人工 |
| `max_consecutive_failures` | 連續失敗幾次就自動暫停（1–10，預設 3） |
| `stop_at` | 整個持續任務的結束時間，必填 |

計算方式：

- 一次執行的花費包含記在它底下的任務（子任務、孫任務）。AI 員工在這次執行的某一輪中建立的任務，由系統放到該輪正在處理的任務底下，依據是系統知道的輪次，不看模型的要求；員工指定的上層任務必須是那個任務或它的子任務，否則建立會被拒絕。如果系統傳下來的輪次資訊存在但是空的或格式錯誤，建立會被拒絕，不會當成「沒有輪次」。一個任務最多 200 個未結束的子任務（已結束的不算；排程與提醒不是子任務），一條鏈最多 64 層。
- 預設的上層任務只適用於持續任務的輪（這次執行的任務，或它底下的任務，包括由任務看板喚醒的子任務）。在一般 goal 任務的輪裡，以及不屬於任何執行的任務的任務看板喚醒中，沒有預設的上層任務。這個版本對所有呼叫者新增兩條規則：員工指定的上層任務需要與它有關係（v1.69 寫入時不檢查），而 `kind="goal"` 接受上層任務（v1.69 會忽略它）。
- 這個放置方式的前提，是員工無法改變自己的 MCP 伺服器怎麼啟動。凍結員工 `.mcp.json` 裡的 `duduclaw` 項目（指令、參數與環境變數），並在每次啟動時正規化它的環境，是另一項平台修正，必須先於這個功能合併；在它上線之前，改寫那個項目的員工可以在這次執行之外建立任務。
- 子任務自己從任務看板（心跳）被喚醒的那幾次，花費記在該子任務上，所以會算進它所屬的那次執行。
- 不是所有任務都會進到這棵樹。經由不知道輪次的 MCP 伺服器建立的任務（從 Bash 啟動的，或 Grok、Gemini CLI 執行環境下，它們的 MCP 設定是存好的檔案），只有在 AI 員工把這次執行指定為上層任務時才掛在底下。交給其他 AI 員工的工作與一次性幫手也不在樹裡（見「不計入花費的項目」）。
- 上限在每一輪之前檢查，用的是上一輪已記錄的花費。所以進行中的那一輪，以及同時在跑的子任務，可能在下一次檢查之前就讓一次執行超過上限，超過多少不受這個設定限制。AI 員工自己的月預算（`agent.toml [budget]`）也在每一輪之前檢查，用完就把這次執行轉人工處理。
- 系統在把某次執行的一輪交給 AI 執行環境之前，會先記錄這一輪已開始。沒有這筆紀錄的一輪不花錢：被派工閘取消的一輪、在開始前就被拒絕的一輪（例如被委派檢查拒絕），以及還在佇列中等待的一輪（閘道重啟之後也一樣）。所以暫停再恢復一次執行，不會用掉它的上限。有這筆紀錄的一輪算作跑過，即使執行環境之後失敗、在員工認領任務之前失敗，或閘道在這一輪中途當機而訊息回到佇列。
- 跑過但花費沒有量到的一輪，以單次上限全額計，不當成 0。完全沒有 token 數的用量紀錄（有些執行環境不回報，見下）視為沒量到。讀不到成本資料時寧可不醒，並在動態牆記一筆。
- 每種執行環境都會記錄每次執行的花費：Claude、Codex、Gemini、Antigravity、Grok 與 OpenAI 相容。Grok 不回報 token 用量，所以數字是依送出與收到文字的長度估計。沒有回報用量的 Antigravity 與 Gemini CLI 執行、沒有用量事件的 Codex 執行，以及從別的執行環境容錯切換過來的 Claude 一輪（它回報的是 0），都算沒量到。使用這類執行環境時，不論它是員工的還是驗收判官的（`[dispatch] judge_provider`），一輪沒量到就會用掉整個單次上限，所以一次執行通常只有一輪，那一輪被駁回時就轉人工處理。建立持續任務（命令列與 RPC）時會把這一點以警告印出，`duduclaw doctor` 會列出這類持續任務。
- 預算期額度用完，持續任務進入「預算暫停」，等下一期自動恢復，期間累積的喚醒合併成一次。
- 期限、迭代次數與花費在重試、重啟之間都不會歸零；睡一天醒來不會多出一天的額度。

不計入花費的項目（`duduclaw responsibility get` 的 `cost_not_counted` 會列出）：

- 轉人工時的後續軌跡模擬
- 開工核准的推播文字
- 派工策略的選擇
- MCP 子行程裡判斷動作可否撤回的判官
- 委派給其他 AI 員工的工作
- 用 `spawn_agent` 啟動的一次性幫手
- 沒有輪次資訊、也沒有放在這次執行底下的任務（見上方）
- 用 `tasks_create` 加 `schedule` 建立的 cron 例行工作與提醒

通知評分（下面「通知」一節）會花一次輔助模型呼叫，這一次有計入那次執行的花費。

## 一次執行怎麼結束

| 任務結果 | 記成 | 是否算一次失敗 |
|---|---|---|
| 驗收通過 | done | 否，連續失敗歸零 |
| 失敗、被取消 | failed／cancelled | 是 |
| AI 員工用 `tasks_block` 卡住 | blocked | 是，並發出通知 |
| 管理者（Manager 以上角色）在儀表板停止，或管理員核准的指令列停止 | stopped | 否 |
| 權限只到 Operator 的帳號在儀表板停止 | stopped | 是 |

最後一列的用意：AI 員工的操作者不能靠停掉正在失敗的那一次，躲過連續失敗暫停。

停在「等人處理」（needs_human）的那一次不會結算，會一直擋住下一次喚醒，直到有人在儀表板處理。

已過期限的一次執行，即使它的持續任務已暫停或到期，也會轉人工處理，並在那時釋放名額（到期限之前，暫停或到期的執行仍佔著名額）；暫停不會讓執行一直掛著。員工的月預算用完時，那次執行同樣轉人工處理，原因標明是預算。

執行的任務由系統管理：AI 員工與儀表板的一般任務編輯都不能把它改派給別人，也不能改它的標題、說明、驗收標準、標籤、期限等控制欄位；狀態與進度照常可以更新。把某位 AI 員工未完成的任務交接給別人（離職交接）時，執行任務會留在原員工身上；要停下它們，請停止這些任務或停用持續任務。

## 指令列

```bash
duduclaw responsibility list [--agent <員工>]
duduclaw responsibility get <id>
duduclaw responsibility occurrences <id>
duduclaw responsibility fires <id>

duduclaw responsibility create --file contract.json [--confirm]
duduclaw responsibility update-contract <id> --file contract.json [--confirm]
duduclaw responsibility pause <id> [--reason <文字>] [--confirm]
duduclaw responsibility resume <id> [--confirm]
duduclaw responsibility disable <id> [--reason <文字>] [--confirm]
duduclaw responsibility enable <id> [--confirm]
duduclaw responsibility clear-failures <id> [--confirm]
duduclaw responsibility stop <task_id> [--confirm]
```

所有子指令（包含查詢）在 AI 員工的工作階段中都會被拒絕。

### 合約檔範例

```json
{
  "owner_agent_id": "support-lead",
  "objective": "整理昨天到現在還沒回覆的客服工單，列出需要我處理的三件。",
  "acceptance_template": "列出最多三件工單，每件附工單編號與一句原因。",
  "schedule": { "cron": "0 9 * * 1-5", "timezone": "Asia/Taipei" },
  "event_subscriptions": [],
  "occurrence_hours": 2,
  "occurrence_cost_cap_cents": 50,
  "budget_period": "week",
  "budget_timezone": "Asia/Taipei",
  "period_cost_limit_cents": 300,
  "period_occurrence_limit": 7,
  "min_wake_interval_secs": 3600,
  "max_consecutive_failures": 3,
  "stop_at": "2026-11-01T00:00:00Z",
  "notification_policy": { "enabled": true, "on": ["result", "needs_decision", "paused"] }
}
```

規則：排程與事件訂閱至少要有一種；`min_wake_interval_secs` 至少 300；`period_cost_limit_cents` 不能小於 `occurrence_cost_cap_cents`；`stop_at` 必須在未來且不超過 `max_stop_at_days`；事件訂閱的 `timeout_at`（選填）要介於現在與 `stop_at` 之間，到了還沒等到事件，會以「沒等到」的身分醒來一次；AI 員工必須存在。

事件訂閱的寫法：

```json
"event_subscriptions": [
  {
    "event_name": "task.updated",
    "filter": { "all": [ { "field": "status", "op": "eq", "value": "blocked" } ] },
    "timeout_at": "2026-10-20T00:00:00Z"
  }
]
```

`filter` 省略時，這位 AI 員工的每一筆該類事件都算。

### 核准流程

指令列無法證明是誰下的指令：有 Bash 的 AI 員工跟你用的是同一個作業系統帳號。所以每個會改變狀態的動作，包含 `stop`、`pause`、`disable`，都要管理員在儀表板核准才會生效。

1. 不加 `--confirm` 執行：只印出這次變更的內容，什麼都不改。`create` 與 `update-contract` 在這一步也會檢查合約（上限、排程、AI 員工是否存在）；不合格的合約會在送出任何請求之前就被拒絕。
2. 加上 `--confirm` 執行：建立一筆核准請求，出現在儀表板的待辦清單。這類請求只能由管理員（Admin）在儀表板決定，通道裡的按鈕或回覆不會生效。請求 24 小時內沒有決定會自動拒絕。
3. 管理員核准後，在 `operator_approval_minutes`（預設 30 分鐘）內，再執行一次完全相同的指令（含 `--confirm`），變更套用一次。

核准綁定動作、對象、變更內容與對象當下的狀態。狀態變了，舊核准就不能用：例如核准了一次暫停，之後有人恢復又再暫停，原本那筆核准不會再生效，並會連同一筆稽核被撤回。同一個動作、同一個對象、同樣內容的請求會合併成一筆。同一個動作、同一個對象最多可以有 3 筆內容不同、還沒決定的請求；第 4 筆會被拒絕，直到其中一筆被決定或過期。同一個對象每小時最多推播 2 次，超過的請求只留在儀表板收件匣並寫一筆稽核，這些請求也不會再發提醒推播。

v1.70.0 之後的版本起，等待中的請求如果是在已經改變的狀態下提出的，要等再執行同一個指令時才會被撤回（`state_changed`）並另建一筆，從那時起不再占用那 3 筆的名額；在那之前它照樣占用名額，直到 24 小時後過期。狀態變了的已核准請求也是在重跑時一樣撤回。兩個終端機同時重跑同一個已核准的指令時，只有一個會套用，另一個會說這筆核准已被另一次執行用掉或作廢，什麼都不做。只要 gateway 替員工行程設定的任何一個環境變數存在（空值也算），這個指令就會拒絕執行。這些規則與其他操作者指令共用，見〈[指令列的操作者動作共用一道核准](../../features/zh-TW/05-security-defense.md)〉。

`stop` 也綁定任務當下的狀態，而執行中的 goal 任務狀態常常變動，所以核准過的指令列停止，重新執行時常常已經對不上，得再請求一次。要停止請用儀表板的按鈕。

核准卡片的內容由伺服器產生，列出 AI 員工、動作、花費與次數上限、排程與事件喚醒條件。持續任務的工作內容會當成引用資料顯示，並截短到 80 字，因為那段文字可能由別人撰寫。卡片也會寫明「這筆請求由本機指令列建立，系統無法確認下指令的人是誰」，不確定是誰建立的就拒絕。

每次提出、套用、拒絕都會寫一筆安全稽核，事件名稱分別是 `responsibility_cli_requested`、`responsibility_cli_applied`、`responsibility_cli_refused`。

功能關閉時，會放寬權限或花費的動作（`create`、`update-contract`、`enable`、`resume`、`clear-failures`）直接拒絕；`pause`、`disable`、`stop` 照樣可以提出。

### 三種控制的差別

| 控制 | 作用 | 不影響 |
|---|---|---|
| `pause` 暫停協調 | 不再喚醒；正在跑的那一次不派下一輪 | 已經在跑的那一輪會跑完、已提交的結果照常驗收；喚醒紀錄繼續累積，`resume` 後合併成一次 |
| `disable` 停用之後的排程 | 取消所有訂閱，丟掉還沒處理的喚醒 | 正在跑的那一次（要停它請另外停止那個任務）。`enable` 會用原本的條件重新訂閱，停用期間錯過的不補。`enable` 會保留連續失敗次數：因為失敗而暫停的持續任務，啟用後仍是因失敗而暫停，只有清除失敗才會重設次數（RPC 需要 Manager 以上，命令列需要 Admin 核准）。核准過的 `update-contract` 仍可為之後的執行調高 `max_consecutive_failures` |
| `stop` 停止任務 | 停止一個任務與它所有子任務 | 持續任務本身；之後到時間還是會醒 |

全部停下來的做法：先 `disable`，再停止正在跑的那一次。

## 持續任務頁面

`/responsibilities`（導覽：工作 → 持續任務）依員工分組，列出你有綁定的員工的所有持續任務。每一列顯示狀態、是否唯讀執行、下次叫醒時間、本預算期花費與上限、本期次數與上限、連續失敗與上限、到期日。資料全部來自既有 RPC（`responsibilities.list`／`.get`／`.occurrences`），頁面不畫任何示範資料。

- **功能關閉時**：`responsibilities.status` 回報兩個開關。`[responsibilities] enabled` 或 `[dispatch] enabled` 任一關閉時，頁面明說並列出兩個設定鍵。既有項目仍可暫停或停用（功能關閉時伺服器只接受這兩個收窄的動作）。
- **動作**：暫停、恢復、停用、啟用需要對該員工的 Operator；清除連續失敗另需 Manager 以上。每次呼叫都送出畫面上的 `control_epoch`，伺服器回「期間被改過」時頁面重新整理而不自動重送。詳細視窗顯示驗收標準、目前這一輪（附既有的停止按鈕）與最近 20 輪的結果與花費。
- **新增**：三個範本：**每日簡報（唯讀）**、**每週回顧（唯讀）**、**自訂**。唯讀範本一律帶 `lane = "explore"`、每個預算期最多跑一次、每輪 1 小時、上限很小（可調整）；自訂範本預設唯讀，可取消勾選。所有數值由伺服器照指令列同樣的規則檢查。

### 唯讀執行（`lane = "explore"`）

持續任務的合約可以帶 `"lane": "explore"`（RPC `responsibilities.create`／`update_contract`，以及指令列的合約檔）。這個欄位存在合約的 scope 裡，因此算進合約雜湊；沒有這個欄位的持續任務與以前逐位相同。其他值一律拒絕（`invalid_lane`）。

這種持續任務每一次執行的每一輪，都在原本為 heartbeat 主動檢查做的唯讀 explore lane 裡跑：

| 執行環境 | 行為 |
|---|---|
| Claude CLI | 啟動時帶 `DUDUCLAW_LANE=explore`（DuDuClaw MCP 伺服器繼承後只列出、只執行 `read`／`draft` 工具）；`--tools` 只剩 `Read`、`Glob`、`Grep`、`WebFetch`、`WebSearch`（扣掉 `denied_tools`）；`--allowedTools` 只剩 DuDuClaw MCP 工具與這些內建工具（不會比員工自己的允許清單寬；`.mcp.json` 其他伺服器的工具不自動核准） |
| OpenAI 相容執行環境、本地推論工具迴圈 | 沒有內建工具；MCP 子程序拿到 `DUDUCLAW_LANE=explore` |
| Codex、Gemini CLI、Antigravity、Grok、通用 CLI | 拒絕：這一輪在派工前失敗（`explore_lane_unsupported`），而且這些執行環境的 `execute` 裡也有同樣的拒絕，以防備援切換到它們 |
| 任務沙箱（`[container] sandbox_enabled`） | 拒絕（沙箱給員工一個 shell） |

被拒絕的一輪算一次不成功的執行，所以連續失敗最後會讓持續任務暫停。lane 讀不出來也會讓這一輪失敗（`explore_lane_unreadable`）。未涵蓋：之後由 heartbeat 叫醒的子任務不在 lane 內（在 lane 內 `tasks_create` 屬於 `modify`，會被拒絕，所以唯讀執行本身建不了子任務）；尚未在真實 gateway 上跑過。

## 儀表板任務詳情頁

### 送指示

goal 任務的詳情頁有「下一輪的指示」區塊。寫下的內容會在下一輪開始時交給 AI 員工，正在跑的這一輪不會被打斷。

- 一則最多 4000 字。每個任務同時最多 10 則還沒送出的指示。
- 只有那一輪真的派出去，才算送達，畫面會顯示「已排進第 N 輪」。這句話代表指示放進了那一輪的訊息，不代表員工採用了。那一輪在開始前被拒絕或取消時，指示退回「等待下一輪交給員工」。一輪一旦開始，即使之後失敗，指示仍算已送達。
- 指示不會改變驗收標準。交給員工時會附註：與驗收標準衝突時以驗收標準為準。判官只看建立時凍結的驗收標準，看不到這些指示。
- 指示內容會做注入掃描。命中時不會擋下（這是操作者自己寫的內容，但可能是轉貼的外部文字），任務頁會在那則指示旁標示「這則內容含有像是指令的文字」，交給 AI 員工時也會附註這段只供參考，不能當成新的權限或驗收標準。
- 任務先結束或被停止時，還沒送出的指示會標成「沒有送出」並註明原因。
- 帶著指示的那一輪一律由 AI 員工單人執行，不走一位員工四角色的團隊回合。
- 需要權限：對這位 AI 員工有 Operator 以上的權限。AI 員工沒有送指示的工具。

範圍要改（驗收標準要換），請停止任務、另建新任務，或對持續任務用 `update-contract`；改的是下一次執行，正在跑的那一次照舊。

### 停止

任務詳情頁的「停止任務」按鈕，對任何沒有被鎖住的任務都能用，需要對該 AI 員工有 Operator 以上的權限。這是有真實身分的途徑，按下就生效，不需要另外核准。確認要按兩次。

確認框打開時會記下你看到的任務版本。按下「確定停止」前任務若已變動（例如又跑完一輪），會顯示「任務在你打開這個視窗之後變動了」，請看過目前的狀況再按一次；系統不會自動拿新版本替你送出。任務已經結束時會告訴你不需要停止。

停止之後：

- 任務與所有子任務標成已取消，無法恢復；要重做請建立新任務。
- 很大的任務樹會分批處理：根任務與第一批立即取消，其餘在之後的巡檢陸續取消。停止期間，任何人都不能在這棵樹底下新建子任務，也不能認領或完成樹裡的任務，也沒有東西會派出樹裡的任務：派工引擎不會，心跳的任務看板喚醒也不會。
- 佇列裡等著派出的那一輪會被取消，佇列裡等著為樹中任務執行的任務看板喚醒也會被取消，綁在這些任務上還沒決定的核准會作廢，已準備但還沒執行的外部動作不會再執行。
- 已經發出去的委派是另一個任務，不會跟著停。

畫面上的狀態：

| 狀態 | 畫面文字 | 意思 |
|---|---|---|
| `cancel_pending` | 正在停止：進行中的工作做完這一輪就會停下。 | 還有工作在跑：已經開始的那一輪、租約還沒到期的認領、團隊回合，或正在執行的外部動作。目前沒有辦法中途打斷一輪，只能等它做完。可以按「再查一次」 |
| `cancel_pending`（另一種文字） | 正在停止：可能還有看不到的團隊工作在進行，稍後會再確認。 | 這個閘道看不到某個團隊回合是否還在跑，會等到租約上限後再判定 |
| `stopped` | 已停止 | 確認沒有任何工作還在跑 |
| `stopped_uncertain` | 已停止，但有些工作或對外動作的結果無法確認，請人工確認。 | 有結果無法確認的項目：認領的租約過期卻沒有已完成的輪次紀錄（還沒執行就被取消的一輪不算已完成）、外部動作結果不明，或任務樹超過可掃描的上限 |

停止中與已停止都不代表已經送出去的外部動作被撤回。外部動作結果不明時，需要人工去對方系統確認那個動作到底有沒有完成。

## AI 員工能用的三個工具

功能開啟時，AI 員工的工具清單會出現下面三個工具，詳細規則見 [MCP 工具](mcp-tools.md#responsibility_get--responsibility_followup--responsibility_ask持續任務工具)：

- `responsibility_get`：查看自己的持續任務。
- `responsibility_followup`：在自己的那一次執行中，安排一次後續喚醒。
- `responsibility_ask`：在自己的那一次執行中，問操作者一個問題，答案在下一次執行以資料形式交給它，不會因此取得任何權限。
  這個問題的通知只帶持續任務名稱與儀表板連結，沒有按鈕，也不會再提醒，所以實際上是在儀表板收件匣回答。若答案從別的管道送來（可信對象在通道上對這筆請求做的決定），處理方式相同。答案只會喚醒一次執行，不會改變任何上限、狀態或權限，也不會替其他待決的核准做決定。

AI 員工不能建立、修改、恢復、重新啟用持續任務，也不能放寬上限。

## 通知

結果出來、需要你決定、持續任務自動暫停或到期、停止以「結果不明」收尾時，系統一定先寫一筆動態牆紀錄。推播另外要依序通過下列條件，任一條不成立就不推：

1. 持續任務的 `notification_policy` 有開，而且這類事件在 `on` 清單裡（預設全關）。
2. AI 員工的 `agent.toml [proactive] enabled` 為真（預設關）。
3. 這一個預算期的推播次數還沒到 `max_notifications_per_period`。這個計數由閘道自己記錄，AI 員工貼的動態不會算進去。
4. 主動通知閘的評分通過。這一步會花一次輔助模型呼叫，算進那次執行的花費。

推播內容只有持續任務名稱、狀態與儀表板連結，不含結果摘要或事件內容。打擾時段會延後推播，不會丟掉。

## 安全模型與已知限制

- 指令列的閘是減速。Bash 通道的檢查會擋下 AI 員工與身分未驗證的呼叫者執行 `duduclaw`／`duduclaw-pro` 的 `responsibility` 子指令（包含查詢），但它讀的是指令文字：改名的執行檔、用變數組出來的指令都擋不住。有不受限 Bash 的 AI 員工可以繞過指令列的閘，甚至直接改資料庫。真正的隔離是不給 Bash，或開[任務沙箱](task-sandbox.md)。
- 緊急狀況。儀表板的「停止任務」按鈕會立刻停止目前這一次執行，但持續任務仍會依排程繼續醒來。儀表板沒有暫停或停用持續任務的頁面。要讓它不再醒來，可以執行 `duduclaw responsibility disable` 並等管理員核准，或在 `config.toml` 設定 `[responsibilities] enabled = false`（儀表板只在原始設定編輯器裡提供這個設定）；開關關閉時，已經在跑的那一次執行仍會當成普通 goal 任務做完，所以也要用按鈕停止它。指令列的 `stop` 要等核准。
- 指令列的停止是在指令列行程裡檢查，看不到閘道剛開始的團隊回合。對一般 goal 任務（執行任務一律單人，不受影響）有一個很窄的時間窗：指令列回報 `stopped`，那個團隊回合卻還會跑到結束。
- 持續任務的執行與帶指示的那一輪一律單人執行，不走團隊回合。
- 「不計入花費的項目」列出的模型呼叫不計入花費；單次上限是在輪與輪之間檢查，所以一次執行可能超過它，超出的量是最後一輪與同時進行的子任務所花的。
- 持續任務只能收窄事件來源與通知對象，收窄不了工具權限：一次執行能用的工具就是這位 AI 員工原本的工具。
- 事件喚醒只看得到 MCP 任務工具寫進 `events.db` 的 `task.created` 與 `task.updated`；停機超過 7 天的事件會漏掉。
- 200 個子任務的上限只檢查 AI 員工的 `tasks_create`，而且是先數再寫入，同一時間的多個呼叫可能稍微超過。閘道與操作者不受這個上限限制。
- 可能不回報用量的執行環境所觸發的花費警告，只出現在指令列與 `duduclaw doctor`；儀表板不顯示 `responsibilities.create` 回傳的 `usage_warnings`，外部裁決（`[dispatch] judge = "external"`）也不在檢查範圍內。
- 同一個資料目錄上啟動多個閘道時，只有持有 `<home>/locks/gateway.lock` 的那一個會執行持續任務的喚醒、停止對帳、指示掃描，以及修復做到一半的持久輪；其他閘道仍照常派送 goal 輪（持久輪固定的訊息 ID 讓它不會被送出兩次）。
- 如果某位員工的任務看板連續三次讀取失敗，動態紀錄會出現一則訊息，說明該員工的任務看板喚醒已經停止（每個閘道行程一次）；每一次失敗也都會以警告寫進日誌。
- `/responsibilities` 頁面可以列出、新增，以及暫停／恢復／停用／啟用／清除連續失敗；修改合約仍走指令列。

## 給開發者

- 程式：`crates/duduclaw-gateway/src/responsibility/`（喚醒 `wake.rs`、事件 `events.rs`、花費 `cost.rs`、停止 `stop.rs`、送指示 `steering.rs`、核准閘 `operator_gate.rs`、通知 `notify.rs`），資料表在 `tasks.db`。
- 儀表板 RPC（`handlers/responsibilities_rpc.rs`）：`responsibilities.status`／`create`／`list`／`get`／`occurrences`／`fires`／`update_contract`／`pause`／`resume`／`disable`／`enable`／`clear_failures`，以及 `tasks.steer`／`tasks.steering`／`tasks.stop`／`tasks.stop_status`。讀取要 Viewer、變更要 Operator（對擁有者員工），但 `clear_failures` 要 Manager，每次寫入記一筆稽核。`responsibilities.create` 也會回傳 `usage_warnings`（可能不回報用量的執行環境）。`system.update_config` 接受 `responsibilities.enabled` 與 `goal_loop.steering_enabled`。
- 指令列：`crates/duduclaw-cli/src/responsibility_cmd.rs`；核准種類 `responsibility_operator_change`，只能在儀表板由 Admin 決定（`decided_by` 以 `dashboard:` 開頭）。
- Bash 通道阻擋：共用的操作者指令比對器（`duduclaw_core::bash_operator_command_decision`，`GuardDecision::BlockedOperatorCommand`），清單在 `responsibility_cmd::OPERATOR_COMMANDS`，在 agent-file-guard hook 中串接於 LINE 收件匣指令之後。
- 每個持續任務、指示與停止 RPC 都會從帳號資料庫重新讀取呼叫者（`handlers/task_privacy.rs::live_reader_context`）；`tasks.steer`／`tasks.steering`／`tasks.stop`／`tasks.stop_status` 另外還要通過任務內容閘（`authorize_private_task_read`：員工存取權加上任務的受眾）。
- 這一輪所屬的任務會以 `DUDUCLAW_TASK_ID` 傳給所有執行環境的 MCP 伺服器（`runtime::round_task_env`）；核准卡片使用同一個值。
