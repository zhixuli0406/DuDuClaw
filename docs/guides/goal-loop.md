# 自主目標迴圈（Goal Loop）

在對話裡丟一個目標，AI 員工就會自主規劃、執行、自我驗收，做到完成或卡住時回來通知你。這一頁說明從頻道使用 `/goal` 的方式、自主程度（AutonomyLevel）分級、相關設定鍵，以及卡住轉人工時的按鈕語意。

整套預設關閉，不影響現有的一問一答對話。啟用需要在 `config.toml` 設定 `[dispatch] enabled = true`（詳見下方設定）。

---

## `/goal` 指令

在任何已接通的頻道（Telegram / Discord / Slack / LINE / …）對 AI 員工輸入：

| 指令 | 行為 |
|------|------|
| `/goal <目標描述>` | 建立一個自主目標任務，指派給當前對話的 AI 員工。沒有另外指定驗收標準時，以目標描述本身當作驗收基準。 |
| `/goal <目標> \|\| <驗收標準>` | 用 `\|\|` 分隔：前半是目標，後半是驗收標準（判官核可的依據）。 |
| `/goal <目標> \|\| <驗收標準> \|\| outcome:<spec>` | 再加一段結構化產出驗收（見下方「結構化產出驗收」）。交付前先跑零成本的 deterministic 校驗，未達標直接退回修正，不燒判官。 |
| `/goal status` | 列出當前 AI 員工進行中的目標任務（短碼 / 狀態 / 第幾輪）。 |
| `/goal` | 顯示用法說明。 |

**範例**

```
/goal 整理這批客戶資料成月報並寄出 || 報表含每月營收圖表，寄到 boss@example.com
/goal 產出 Q3 月報 || 含每月營收圖表 || outcome:files:report.docx
```

建立後會回覆確認訊息，包含任務短碼、上限輪數，以及「完成或卡住會在這裡通知你」。任務進度與需人工的通知會**推回你發起的這個對話**（來源頻道），而不只是 AI 員工的 `[proactive]` 通知頻道。

> 若尚未啟用自主派工引擎（`[dispatch] enabled = true`），任務仍會建立，但確認訊息會提醒你它不會自動開始執行。

---

## 外層進度看板

目標任務的每個狀態轉移都會推一則簡短（一到三行）的進度訊息回來源對話：

- 開始執行 / 重試（第 N/上限 輪）
- 驗收中
- 未通過 → 修正後重試（附驗收判官回饋摘要）
- 完成 ✅（附結果摘要）
- 卡住 → 需要你決定（同時另外推送審批按鈕）

同一任務同一狀態不會重複推播。來源對話不存在時，退回 AI 員工的 `[proactive]` 頻道；兩者都沒有時只寫入儀表板 Activity Feed，不打擾你。

---

## AutonomyLevel 自主程度五級

每個 AI 員工的自主程度由 `agent.toml [capabilities] autonomy_level` 一個刻度控制。未設定 / 無法解析 → 預設 **Approver**（保守：只有卡住或需人工才問你）。

| 級別 | 行為 |
|------|------|
| `operator` | 迴圈完全不自主驅動；任務建立後靜置，由人手動推進。 |
| `collaborator` | 第一次派工前需人工核准（kickoff 審批），核准後自主重試到完成。 |
| `consultant` | 同 collaborator 的 kickoff 審批。 |
| `approver` | **預設**。無 kickoff 閘；卡住 / 需人工時才轉人工審批。 |
| `observer` | 全自動；需人工時只通知、不等待（任務自動結束）。 |

```toml
# agent.toml
[capabilities]
autonomy_level = "approver"
```

---

## 階段性授權工具（scoped_tools，v1.41）

高風險工具可以宣告為「持授權才可用」：列在 `scoped_tools` 的工具，AI 員工沒有拿到有效授權（grant）前一律拒絕，且授權只活在單一任務的生命週期內——任務結束（通過、駁回、轉人工、取消）時全部自動撤銷，不會殘留到下一件事。

```toml
# agent.toml
[capabilities]
scoped_tools = ["shared_wiki_delete", "odoo_execute"]  # 這些工具需逐任務授權
grant_ttl_secs = 3600                                   # 授權硬性存活上限（秒），預設 3600
```

取得授權的兩條路：

1. **AI 員工自行申請**：呼叫 MCP 工具 `capability_request { tool, reason, task_id? }`，會轉成一則審批（與其他審批走同一個通知/儀表板介面）；你核准後授權生效，逾時未決視同拒絕。
2. **目標任務開工時一併授予**：goal 任務的 tags 加 `grant:<工具名>`，kickoff 審批（collaborator/consultant 級）通過時原子性授予，任務結束自動收回。

判定一律 fail-closed：授權資料庫讀不到就是沒有授權。未列入 `scoped_tools` 的工具完全不受影響。

---

## 設定鍵

### `config.toml`（全域）

```toml
[dispatch]
enabled = true          # 啟用自主派工引擎（含 goal loop 驅動器）。預設 false
policy = "fixed_hierarchy"  # 派工策略（選哪個 AI 員工接任務）。見下方「派工策略」。預設 fixed_hierarchy
grounding_precheck_enabled = true  # 驗收前的證據落地預檢（見「證據落地預檢」）。預設 true

[task_forward_model]    # 任務層前瞻模型（見同名章節）。預設整組關閉
enabled = false

[goal_loop]
iteration_cap = 8        # 困難目標的硬性派工上限，超過 → 轉人工。預設 8
iteration_cap_simple = 3 # 簡單目標的派工上限（動態判官深度）。預設 3
wall_clock_hours = 24    # 從建立起算的牆鐘預算（小時），超過 → 轉人工。預設 24
max_concurrent = 3       # 同時在飛的目標任務上限（防 spawn 風暴）。預設 3
tick_secs = 30           # 驅動器輪詢週期（秒）。預設 30
stalled_secs = 600       # 派工後未被認領視為停滯、可重派的秒數。預設 600
planner_enabled = false  # 開啟後允許把目標拆成帶依賴的子任務 DAG（見「平行子任務」）。預設 false

[dispatch_guard]        # 回饋路徑斷路器（防再生型無限迴圈）
window_secs = 60        # 滑動窗長度（秒）。預設 60
max_in_window = 20      # 一個窗內允許的派工次數，超過即熔斷。預設 20
cooldown_secs = 60      # 熔斷後拒絕派工的冷卻秒數。預設 60
max_hop_depth = 5       # 委派鏈跨行程 re-spawn 的深度上限。預設 5
```

所有區塊都可省略；缺省 / 部分設定一律退回上表的內建預設。未知的 `policy` 值一律退回 `fixed_hierarchy` 並記一筆警告。

---

## 平行子任務（依賴 DAG）

開啟 `[goal_loop] planner_enabled = true` 後，建立目標時會先讓 AI 員工「試著」把目標拆成一組帶依賴標注的子任務（例如：先各自查兩個資料源、再彙整）。拆出來的子任務會各自進 Task Board，`depends_on` 全部完成的子任務會**並行**開跑，各自獨立驗收。並行度仍受 `max_concurrent` 與 `dispatch_guard` 斷路器約束，不會繞過。

- **非強制**：模型判斷不需要拆（或回覆無法解析）時，就退回單一任務，行為與關閉時完全一致。
- **循環依賴防護**：拆出來的計畫若含循環依賴（或索引越界），整份計畫作廢、退回單一任務並記警告——絕不落地一個壞掉的 DAG。
- **上游卡住不孤兒化**：某個子任務的上游依賴走到 `failed` / `cancelled` / `needs_human`（或依賴不存在），下游會**繼承升級**一起轉人工，讓你看到整條被卡住的分支；上游只是還在跑時，下游該輪凍結、下一輪再看。

預期效益以「多資料源查詢型」目標最大；獨立重測顯示加速約 1.25 倍（非論文自報的 3.7 倍），請以 eval 實測為準再推廣。

---

## 派工策略（DispatchPolicy）

`[dispatch] policy` 決定「選哪個 AI 員工接一項目標任務」。預設 `fixed_hierarchy` 的行為與過去完全相同（派給任務原本指派的員工）。

| 策略 | 行為 |
|------|------|
| `fixed_hierarchy` | **預設**。派給任務原本的 `assigned_to`，不改動。零 LLM 成本、完全確定性。 |
| `round_robin` | 依「任務類別」（有標籤取第一個標籤，否則取優先級）在員工名冊中輪詢分派。狀態僅存記憶體，重啟即從頭。 |
| `llm_select` | 由工具用 LLM 從名冊挑最合適的員工。**失敗關閉**：輸出不在名冊內、或解析/LLM 失敗，一律退回 `fixed_hierarchy` 的結果，絕不派給捏造的員工。不硬編碼任何模型名（走設定的工具用 runtime）。 |

名冊 = `<home>/agents/` 下的員工目錄。名冊為空時，`round_robin` / `llm_select` 都退回原指派（不孤兒化）。改派會寫回任務的 `assigned_to`，讓 heartbeat 拉取與活動記錄一致。

---

## 結構化產出驗收（outcome schema，WP2.4）

`/goal … || outcome:<spec>` 讓你在自由文字的驗收標準之外，再加一層**機器可校驗**的產出契約。當 AI 員工回報完成、任務進入 `review` 時，這層契約會在**驗收判官（LLM）之前**先跑一次 **deterministic、零 LLM 成本**的校驗：

- **校驗不通過** → 任務直接退回 `revising`，回饋訊息帶具體缺陷（缺哪個欄位、少哪個檔），**完全不呼叫判官**。這是擋判官假陽性的防線：結構上明顯不合格的產出，不會被過度寬鬆的判官放行，也不浪費一次判官 LLM call。
- **校驗通過** → 才進判官，且判官 prompt 會附上「結構化產出驗收已通過 deterministic 校驗」的註記，讓判官專注在品質面向。

三種 spec 型別：

| spec | 意義 |
|------|------|
| `outcome:text` | 預設。無結構化契約，行為與未加 outcome 時完全一致（不持久化、判官前不跑任何校驗）。 |
| `outcome:json:<JSON Schema>` | JSON Schema 子集（`object` / `array` / `string` / `number` / `integer` / `boolean`，支援 `properties` / `required` / `items`）。校驗 AI 員工最終回覆中的 ` ```json ` 區塊（找不到 fenced 區塊則退而解析整段回覆）。缺欄位 / 型別不符都會列成具體缺陷。 |
| `outcome:files:<glob,glob>` | 斷言 AI 員工工作目錄下有符合每個 glob 的產出檔（支援 `*`／`?`）。例：`outcome:files:report.docx, out/*.pdf`。 |

**範例**

```
/goal 匯出這季營收數字 || outcome:json:{"type":"object","required":["revenue","month"],"properties":{"revenue":{"type":"number"},"month":{"type":"string"}}}
/goal 產出季報並存檔 || 需含營收圖表 || outcome:files:report.docx,charts/*.png
```

**界線與 fail-closed 行為：**

- **路徑穿越拒絕**：`files:` 的 glob 若是絕對路徑、家目錄（`~`）、或含 `..` 上層目錄，一律在 `/goal` 建立時就拒絕（fail-closed，任務不建立），校驗時再擋一次。工作目錄基準是 `<home>/agents/<agent>/`。
- **畸形 spec 拒絕**：`json:` 不是合法 JSON 物件、`files:` 空清單、未知型別前綴 → `/goal` 直接回錯誤、不建立任務（不會靜默降級成 text）。
- **持久化**：spec 以單一 `outcome:<base64url>` 標籤存在任務既有的 `tags` 欄位（base64url 不含逗號，不與標籤分隔衝突），**不改資料庫 schema**。`text` 不持久化。
- **與 planner 的關係**：設了 outcome spec 時會略過 `planner_enabled` 的子任務拆分——結構化契約針對單一最終交付物，不套用到每個被拆出的子任務。
- **判官仍是後盾**：標籤若毀損無法解碼，deterministic 校驗跳過、直接交給判官（判官照樣把關），不會因為觀測性缺口而卡住任務。

## 證據落地預檢（grounding precheck，v1.53）

AI 員工回報完成、任務進入驗收時，在呼叫驗收判官之前會先跑一道零 LLM 的
證據預檢：把最終回覆與這次任務實際的工具執行紀錄（稽核日誌裡的工具結果）
比對。回覆若宣稱查到了什麼、做了什麼，卻找不到任何一段與真實工具結果重疊
的內容，就直接退回修正，不燒判官。兩個防偽細節：

- **自我回音不算證據**：像 `tasks_complete` 這類會把 AI 員工自己寫的摘要
  原樣回傳的工具，列在排除名單上，不能拿自己的話證明自己。
- **自己餵進去的不算**：AI 員工放進工具呼叫參數裡的文字會從證據中扣除，
  只有工具真正回傳的內容才算數。

`config.toml [dispatch] grounding_precheck_enabled = false` 可關閉。沒有
任何工具紀錄可比對時（例如純對話型任務），預檢會跳過（Skip），不誤傷。

---

## 任務層前瞻模型（task forward model，v1.53，預設關閉）

開啟後，goal loop 在每次派工前會依過往同類任務的統計先「預測」這次執行
大概會如何（會不會失敗、大概動用哪些工具類別），執行結束後把預測與實際
觀察比對並記錄成轉移，讓系統對「做這類事會發生什麼」累積出任務層的世界
模型。所有 runtime（claude / codex / gemini / openai-compat）通用：

- **預測分層退化**：有同類統計用統計、沒有就用整體邊際、再沒有用先驗
  預設值，冷啟動不花任何 LLM 費用。
- **觀察誠實分級**：每筆觀察都標記證據保真度（原生工具事件 / 只有稽核
  日誌 / 無證據），不會把「沒看到」當成「沒發生」。
- **`<state>` 狀態區塊**：任務進行中，提示裡會注入一個結構化的目前狀態
  區塊，AI 員工可透過回覆中的狀態更新標籤修訂它；搭配（狀態, 行動）訪問
  圖，同一狀態重複做同一動作兩次以上會提早轉人工（震盪偵測）。
- **預示警告**：預測顯示這次派工大機率失敗時，派工提示會附上警告脈絡，
  但不直接擋下（預測是輔助，不是閘門）。
- **任務規則歸納**：同型轉移重複出現時，以確定性模板歸納成任務規則注入
  後續提示（上限 2 條），與其他學習規則共用同一套 helpful/harmful
  生命週期，表現不好會自動退休。

```toml
# config.toml
[task_forward_model]
enabled = false   # 預設關閉；開啟後整套 predict-act-verify 生效
```

---

## 動態判官深度（MaAS）

驗收判官的檢核面向數量會隨目標難度縮放，省下不必要的判官 LLM 成本：

- **簡單目標**（短、單步、無多步/研究/比較/部署/遷移等關鍵詞）：判官只查兩個面向 **correctness + safety**，派工上限用 `iteration_cap_simple`（預設 3）。
- **困難目標**：完整三面向 MAV panel **correctness + completeness + safety**，派工上限用 `iteration_cap`（預設 8）。

**safety 面向在任何深度都保留**（失敗關閉精神）：降深度只裁掉 completeness 的細緻度，安全檢核永不裁撤。難度由本地零 LLM 啟發式（長度 + CJK-aware token 估算 + 關鍵詞）判定，判官深度與派工上限用的是同一套判定，兩者一致。

### `agent.toml`（每個 AI 員工）

```toml
[capabilities]
autonomy_level = "approver"
irreversible_tools = ["send_email"]          # 一律需人工核准的不可逆工具
maybe_irreversible_tools = ["Bash", "http_post"]  # 由 judge 判定是否需升人
```

---

## needs_human 按鈕語意

任務轉「需人工」時（達派工上限 / 牆鐘超時 / 連續兩輪駁回且回饋雷同 / 驗收判官在重試預算耗盡時仍不通過 / 上游依賴子任務卡住而繼承升級），會推送三顆按鈕到 AI 員工的控制頻道：

| 按鈕 | 動作 |
|------|------|
| 重試 | 任務回到待重試（`pending`），下一輪驅動器再派工。 |
| 標記完成 | 直接標記完成（`done`）。 |
| 放棄 | 取消任務（`cancelled`）。 |

按鈕決策是**冪等且失敗關閉**的：只會從 `needs_human` 狀態轉出，重複按或狀態已變一律無效（no-op）。`collaborator` / `consultant` 的 kickoff 審批同理，逾時未決＝拒絕（fail-closed）。

自 v1.53 起，轉人工的審批會附上一段**模擬預覽**（simulate-before-act）：若你選擇讓任務繼續，接下來三步大概會發生什麼。模擬產生有 15 秒上限，逾時就不附模擬、照常送出審批（不會因此卡住）；模擬引用的知識庫內容限唯讀 namespace，且模擬敘述本身不能決定某個動作是否可逆（不能自證安全）。儀表板審批卡片會渲染這段預覽。

---

## 終止保證

驅動器（而非模型）掌握硬邊界，卡住的目標不可能無限迴圈：完成訊號只認驗收判官核可（不信任 AI 自評「做完了」）；派工上限、牆鐘上限、並行上限、進度震盪偵測、回饋路徑斷路器各自獨立生效，任何一條踩線即轉人工或熔斷。
