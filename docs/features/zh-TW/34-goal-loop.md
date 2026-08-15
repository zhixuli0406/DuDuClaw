# 自主目標迴圈

> 在聊天裡丟下一個目標;agent 規劃、執行、由獨立判官驗收直到完成——或者某條硬界限觸發,交給人裁決。

---

## 這是什麼

目標迴圈(v1.37)把一問一答變成「給一個目標 → agent 迴圈跑到完成 → 卡住就升級給人」。從任何已連接的通道(Telegram / Discord / Slack / LINE / ……)輸入:

```
/goal <goal description> || <acceptance criteria>
/goal 產出 Q3 月報 || 含每月營收圖表 || outcome:files:report.docx
/goal status
```

這會在 Task Board 上建立一個 `goal_mode` 任務,蓋上來源通道與聊天室的戳記,進度會推回發起它的那段對話。省略 `||` 段落時,目標描述本身就是驗收基準,回覆同時會附一段四要素引導(目標/輸入/輸出格式/約束)與 3-5 條 outcome 式驗收標準建議,幫助下次交付得更精確;可選的第三段 `outcome:<spec>` 加上一份機器可驗的產出契約(JSON Schema 子集或 workdir 檔案 glob),在判官*之前*以零 LLM 成本、確定性地執行——結構上不合格的產出直接彈回修改,不花一次判官呼叫。

驗收標準在建立當下就會凍結成不可變的基準,判官與下方的第一階段評估器一律讀這份凍結基準;agent 身分呼叫 `tasks_update` 想改自己 goal 任務的驗收標準會被拒絕並留審計紀錄,只有儀表板操作者能編輯顯示用的可變副本(但不會回頭改變凍結基準)。

派工引擎自 v1.59 起**預設開啟**(閒置時只做週期性 SQLite 輪詢,零 LLM 成本);不想要時在 `config.toml` 設 `[dispatch] enabled = false`,或到儀表板「設定 → 自動化」關閉「派工引擎」開關(免重啟熱生效)。

## Driver

`GoalLoopDriver`(`goal_loop.rs`)是外層迴圈。每 30 秒(`tick_secs` 可調)找出等待執行的 `goal_mode` 任務,把一則工作訊息排進既有的 message queue——與通道訊息同一條喚醒軌——agent 於是循原封不動的管線認領、執行、完成。閉環如下:

```
driver enqueue ─▶ dispatcher ─▶ agent works ─▶ goal task → review
     ▲                                              │
     └── reject → pending (+judge feedback) ◀── acceptance judge ──▶ pass → done
```

被駁回時,任務帶著判官回饋回到 `pending`;下一個 tick 立即把該回饋放進工作訊息重新派工——一個 Generator-Verifier 重試迴圈。每次狀態轉換都往來源對話推一則簡短(1–3 行)進度,同狀態去重。

## 驗收判官——絕不採信自我宣告

「完成」只由 verifier 宣告,不由 worker。agent 回報完成後,任務進入 `review`。先跑一個便宜的**第一階段評估器**——無工具、單次 LLM 呼叫,輸出三選一 JSON 判定(`continue`／`candidate_complete`／`blocked`):`continue` 跳過判官團,直接拿評估器的 `next_step` 當回饋重新派工(計入迭代上限);`blocked` 直接轉 `needs_human`;只有 `candidate_complete` 才進下面的判官團。評估器故障(逾時/解析失敗/呼叫錯誤)一律降級直接跑判官團,絕不因此自動通過或自動拒絕。設定 `[dispatch] two_stage_judge`(預設 `true`;`false` 退回每輪都跑判官團的舊行為)。

進判官團後,`DispatchEngine` 跑一個**三面向 MAV 判官團**——正確性、完整性、安全性——一次 LLM 呼叫,走帳號輪替器。三項全過才算通過;解析失敗、面板 JSON 截斷/畸形、或判官錯誤都會把任務停在 `needs_human`,絕不自動通過(fail-closed)——截斷的 JSON 片段不會再落到舊版單一 token 掃描器,不會被片段裡剛好出現的 `pass` 字樣誤判通過。這是對經典 loop trap(agent 自述成功、系統照單全收)的防禦。

判官團 prompt 也內建四條紀律(無 config 開關,永遠套用):**反棘輪**(驗收標準沒變時不得每輪找新毛病)、**只稽核不自建證據**(只能比對 agent 提交的證據與工具稽核摘要,不得自行想像補寫)、**反契約外擴張**(驗收標準沒寫的事項不得作為駁回理由)、**agent 自稱完成不是證據**。

判官深度隨目標難度縮放(本地、零 LLM 的啟發式):簡單的單步目標用兩面向檢查(正確性 + 安全性)與較低的迭代上限;困難目標用完整判官團。安全面向在任何深度都不會被拿掉。

## 硬防護——界限由 driver 持有

終止由 driver 保證,而不是信任模型:

| 防護 | 預設 | 觸發時 |
|---|---|---|
| 迭代上限(每任務派工數) | 8(困難目標)、3(簡單目標) | `needs_human` |
| 自建立起算的 wall clock | 24 h | `needs_human` |
| 並行目標任務數 | 3 | 排隊,不派工 |
| 停滯偵測 | 連續兩輪駁回回饋的 **gap 指紋**相同(從回饋抽取 `path:line` 引用與反引號關鍵詞正規化而成,不是逐字比對——換句話說的同一個 gap 也算;抽不到任何引用/關鍵詞才退回逐字比對) | 提前 `needs_human` |
| 提前收工偵測 | 九條 zh+en 正則比對 agent 最後一段非空文字(如自簽 `VERDICT:`、「請稍後再來查看」、「請你審核一下」) | 記遙測與 activity 事件,提示帶進下一輪判官/評估器輸入——不會自己駁回或卡住任務 |
| In-flight 去重 | 已派工但未認領的任務在停滯逾時(600 s)前不重新排入 | 重新派工 |
| 跨程序斷路器(`dispatch_guard`) | 60 s 滑動視窗內 20 次派工 | 冷卻拒絕 |
| 委派 hop 深度 | 5 | 拒絕派工 |
| gateway 重啟時的復活行為(`resume_on_restart`) | `pause`(**預設**)——開機把所有 in-flight `goal_mode` 任務轉 `needs_human`(原因 `gateway_restart`) | `auto` 時重啟後 in-flight 任務照常接續(這項設定出現前的唯一行為);可在 `config.toml` 或儀表板「設定→自動化」切換(`system.update_config` 只收 `"auto"`/`"pause"`) |

全部在 `config.toml` 的 `[goal_loop]` / `[dispatch_guard]` / `[dispatch]` 之下,區段缺席時使用內建預設。

## needs_human 升級

任務停在 `needs_human` 時,`goal_notify.rs` 往來源對話推送一則帶四個動作的審批訊息——**重試 / 標記完成 / 中止 / 交給我**(fallback 到該 agent 的 `[proactive]` 控制通道)。一則訊息主要動作上限 3 個,因此重試/標記完成留在主要層,中止/交給我收進各平台自己的次要層:Telegram、Discord 各是第二排按鈕,Slack 是原生 `overflow` 選單;LINE 沒有對應的次要選單機制,這兩個動作不會出現在 LINE 的快速回覆裡,改以文字說明並附儀表板連結。其他無按鈕通道用文字 fallback,儀表板也有一欄 needs_human 看板。

**交給我**只認領任務(`claimed_by`),不解決它——任務仍留在 `needs_human`,而驅動器的派工候選查詢本就不看這個狀態,所以不需要額外的狀態轉換就已經停止自動重試。這是目前實作的第一層(停止自動迴圈＋標記＋收斂卡片);把整段對話控制權轉交給人是後續階段的功能,尚未實作。

決策具冪等性且 fail-closed:重試/標記完成/中止只從 `needs_human` 狀態轉出,所以過期或重複按下都是 no-op;交給我沒有終態可比對,重複按(甚至換另一位有權限的人按)就是重新認領,不會報錯。

## 自主等級

每個 agent 的韁繩長度就是一顆旋鈕:`agent.toml [capabilities] autonomy_level`。缺席或無法解析時,預設為保守的 `approver`。

| 等級 | 行為 |
|---|---|
| `operator` | 迴圈永不自動驅動;任務停著,直到有人推它。 |
| `collaborator` / `consultant` | 首次派工需要人類的開工核准(ApprovalBroker,1 h TTL,過期即拒絕)。之後自主跑到完成。 |
| `approver` | **預設。** 沒有開工閘;只在 `needs_human` 時諮詢人類。 |
| `observer` | 完全自主;`needs_human` 只通知,不等待。 |

## ActionGuard:三值不可逆性

再往上疊一層,按工具呼叫生效(`approval.rs`,承 Magentic-UI 的 ActionGuard):

- `irreversible_tools` — **一律**需要人類核准。
- `maybe_irreversible_tools` — 由 LLM 判官針對*這一次的呼叫*裁決;有風險(或判官失敗/逾時)就升級給人,安全則自動放行。Fail-closed。
- 未列出 — 走既有的 allowed/denied/policy 流程,不加新摩擦。

與舊有 `approval_required_tools` 的合併採取其嚴者,既有設定的語意原封不動。

完整操作參考——config key、派工政策、平行子任務 DAG、outcome schema——見 [`docs/guides/goal-loop.md`](../../guides/goal-loop.md)。
