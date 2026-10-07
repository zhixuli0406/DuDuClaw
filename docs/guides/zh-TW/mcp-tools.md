# MCP 工具：AI 員工看得到哪些、為什麼

DuDuClaw 的 MCP server 透過標準的 `tools/list` 宣告工具。這頁解釋兩件最容易誤解的事：

1. 為什麼 AI 員工看到的工具，比伺服器實作的少；
2. 工具說明被限長之後，原本那些長篇細節去哪了。

要新增工具請先看 [custom-mcp-tool.md](../custom-mcp-tool.md)；這頁講的是宣告面，不是實作。不裝 gateway、單獨使用 MCP server，見 [mcp-standalone.md](mcp-standalone.md)。

## 規則：看得到 ⇔ 叫得動

`tools/list` 只列出呼叫端**當下真的叫得動**的工具。下面每一道過濾，都對應派工層本來就會擋的同一道閘，所以「AI 員工讀到的清單」與「伺服器會接受的呼叫」永遠是同一組。

這既是正確性問題，也是成本問題。工具 schema 是**每次開機都要付的固定 prompt 成本**：CLI 每個 session 讀一次 `tools/list`，整包在第一個使用者 token 之前就進了模型的上下文。一個註定被拒的工具 schema 等於付兩次錢——token 付一次，模型繞著一個用不了的工具規劃再付一次。

隱藏工具**不是**授權決定。呼叫未列出的工具仍然會打到真正的閘，仍然會被拒絕，而且回的是閘自己的訊息。

### 被過濾的項目

| 過濾條件 | 來源 | 關閉／空值時的效果 |
|---|---|---|
| 外部用戶端白名單 | `principal.is_external` | 只列 7 個舊白名單工具加上明確授予 scope 的工具，接著再套下一列的 scope 規則 |
| 非員工呼叫者的 scope | `principal.scopes`，適用於不持有 `admin`、也不是 AI 員工的金鑰（不是 gateway 內部金鑰、不是單一員工金鑰、也不是 gateway 為員工啟動的行程） | 只列金鑰持有其最低 scope 的工具；替行程本身的 agent 動作的工具（`working_state_*`、`canvas_*`、`shared_wiki_delete`、`memory_search_by_layer` 與三個整併狀態查詢等）不列，對所有外部金鑰也不列；呼叫時 dispatch 閘依同一條規則拒絕（`process_agent_tool`）。見 [mcp-standalone.md](mcp-standalone.md) |
| Google Workspace | `config.toml [integrations] google_workspace` | 隱藏 19 個 Google 工具 |
| GitHub | `config.toml [integrations] github` | 隱藏 5 個 `github_*` |
| `denied_tools` / `allowed_tools` | `agent.toml [capabilities]` | 被拒的隱藏；允許清單非空時其餘全隱藏。清單項目完全相同才算，或用結尾的 `*` 比對：`*`、`mcp__duduclaw__*`（全部 DuDuClaw 工具）、`mcp__duduclaw__odoo_*`／`memory_*`（從開頭比對的前綴）。`mcp__<其他伺服器>__…` 不會對到 DuDuClaw 工具；其他位置的 `*` 只是一般字元。三份審批清單與 `scoped_tools` 用同一套規則 |
| `os_native` | `agent.toml [capabilities]` | 隱藏 6 個 `os_*` 自動化工具 |
| `recording` | `agent.toml [capabilities]` | 隱藏 5 個錄製工具 |
| `system_operator` | `agent.toml [capabilities]` | 隱藏 19 個值班機操作工具 |
| `codrive` | `agent.toml [capabilities]` | 隱藏 `codrive_run` / `codrive_status` |
| `computer_use` | `agent.toml [capabilities]` | 隱藏 8 個 `computer_*`（見下方 [`computer_*`](#computer_由-gateway-執行的-session)） |
| `db_sources` | `agent.toml [capabilities]` | 隱藏 4 個 `db_*` |
| `[fork] enabled` | `agent.toml` | 隱藏 6 個分支工具 |
| `[responsibilities] enabled` | `config.toml` | 隱藏 3 個 `responsibility_*`（見下方[持續任務工具](#responsibility_get--responsibility_followup--responsibility_ask持續任務工具)） |
| `scoped_tools` | `agent.toml [capabilities]` ＋ 有效授權 | 在階段性授權生效前一律隱藏 |

新建的 AI 員工以上全部都沒開，這正是重點：預設部署只為自己用得到的工具付費。

### 中途變更如何傳到 AI 員工

以前隱藏工具等於永久拿不到，因為 MCP 用戶端只在開場讀一次 `tools/list`。現在伺服器在 `initialize` 回應中宣告 `tools.listChanged`，並在呼叫端的可見集合**真的改變**時送出 `notifications/tools/list_changed`——比較的是集合，所以只是碰了設定檔但內容沒變，不會送出任何通知。

所以這串流程不需要重啟：

1. AI 員工撞到 `scoped_tools` 拒絕，或操作者要授權某個能力。
2. `capability_request` 核准（或儀表板存了 `agent_update`，或有人改了 `agent.toml`）。
3. 幾秒內伺服器察覺變更並通知用戶端。
4. 用戶端重讀 `tools/list`，工具就在那了。

階段性授權在任務進入任一終態時撤銷，同一套機制也會把工具收回去。

### 注意：AI 員工無法自行發現 `scoped_tools` 的名稱

列在 `scoped_tools` 裡的工具在授權生效前是隱藏的，所以 AI 員工沒辦法從 `tools/list` 讀到名稱再去申請。請用別的方式告訴它——寫進 `SOUL.md`、寫成 playbook 規則，或在目標開工時用 `grant:<tool>` 標籤直接鑄出授權。這是裁剪唯一有代價的地方，而且是刻意的：宣告一個當下就會被拒的工具，正是上面那條規則要消滅的失敗模式。

### v1.68.0 起的例外：列出但會拒絕

兩道新的閘門會拒絕呼叫，但不會把工具從 `tools/list` 拿掉：

- `agent.toml [permissions]`：寫成 `false` 的旗標會拒絕 `create_agent`（`can_create_agents`）；`send_to_agent`、`spawn_agent`（`can_send_cross_agent`）；`create_reminder` 與帶 `schedule` 的 `tasks_create`（`can_schedule_tasks`）；`skill_hub_install`、`shared_skill_adopt`、`skill_graduate`、`skill_pin`、`skill_from_recording`（`can_modify_own_skills`）。拒絕時回 JSON-RPC 錯誤 -32003 並記稽核 `permission_denied`。`agent.toml` 存在但讀不到或無法解析時，這些工具一律拒絕；檔案不存在則放行。不帶 `schedule` 的 `tasks_create` 仍然允許，所以這些旗標是逐次呼叫檢查。臨時角色成員建立時 `can_create_agents`、`can_modify_own_skills`、`can_schedule_tasks` 為 `false`。
- `config.toml [odoo] features_*`：Odoo 工具照樣列出，呼叫到已關閉模組的模型時逐次拒絕（project 與 hr 預設關閉）。

### 紀錄關係檢查：列出但會拒絕

有些工具會變更或觸發屬於某位 AI 員工的紀錄。它們對所有呼叫者都照常列出，每次呼叫時再檢查關係：

- **任務**：`tasks_update` 與帶 `task_id` 的 `activity_post`，任務的受派者、認領者或建立者可以直接操作；`tasks_complete` 與 `tasks_block` 只認受派者與認領者；`tasks_claim` 可以認領未指派或本來就指派給自己的任務。其他情況都要與受派者有委派關係（同部門、`reports_to` 上下級，或白名單配對，依 `[delegation] policy`）。未指派、未認領的任務要先認領。用 `tasks_update` 把別人的任務改派給自己，一律要有這個關係。
- **任務欄位**：AI 員工不能改 goal 模式任務的 `title` 或 `description`（`acceptance_criteria` 則所有 MCP 呼叫者都不能改），也不能在 `tasks_update` 或 `tasks_create` 的 `tags` 新增、移除或調換以 `outcome:`、`grant:` 開頭的 tag 與 `auto-research` tag。
- **新任務的上層任務**：閘道會告訴 MCP 伺服器這一輪在處理哪個任務（`DUDUCLAW_TASK_ID`，和核准卡片帶的是同一個值）。這個任務是呼叫者自己的、而且屬於[持續任務](continuous-responsibilities.md)的某一次執行（執行本身或它底下的任務，包括任務看板喚醒的子任務）時，沒給 `parent_task_id` 的 `tasks_create`（含 `kind="goal"`）會掛在它底下，員工自己給的 `parent_task_id` 必須是那個任務或它的子任務，否則拒絕。這個值存在但是空白或格式不對時一律拒絕，不會當成「沒有輪次」。其他情況（一般 goal 輪次、執行之外任務的看板喚醒、沒有輪次資訊）不預設上層任務。這一版對所有呼叫者都新增的規則：給了 `parent_task_id` 需要與上層任務有關係（指派、認領、建立或委派規則），v1.69 原本不檢查；`kind="goal"` 也接受 `parent_task_id`，v1.69 原本忽略；AI 員工在一個任務底下最多建 200 個尚未結束的子任務（`schedule` 建的例行工作與提醒不是子任務）。從 Bash 啟動的伺服器，以及 Grok、Gemini CLI 執行環境，拿不到輪次資訊；在那裡建立的任務除非員工指定這次執行為上層任務，否則不會掛到執行底下，也不計入它的花費。掛對位置還要靠員工改不了自己 `.mcp.json` 裡的 `duduclaw` 項目，這是另一項平台修補，必須先合併。
- **例行工作**：`update_cron_task`、`delete_cron_task`、`pause_cron_task`、`run_cron_task` 要求呼叫者就是這筆例行工作的執行員工，或與它有關係。以 `name` 指定時只作用於一筆；多筆同名會被拒絕，並列出候選 id。
- **提醒**：`create_reminder` 的 `agent_id` 不是呼叫者自己時，要與該員工有關係。
- **對自己用 `agent_update`**：AI 員工不能對自己送出 `reports_to`、`db_sources`、`db_sources_add`、`db_sources_remove`、`budget_cents` 或 `role`（稽核 `agent_authority_refused`）。修改下屬不變。

操作者（不對應任何 AI 員工的 MCP 金鑰，且行程不是為某位員工啟動的）不受限。行程沒有員工身分時，內部共用金鑰不擁有任何紀錄，所以碰任何人的紀錄都會被拒絕。身分是系統 sender 名稱（`dashboard`、`cron` 等）的行程會被當成不受信任。每次拒絕都記在 `tool_calls.jsonl`。詳見[任務看板](../../features/zh-TW/24-task-board.md)、[委派隔離](../../features/zh-TW/37-delegation-isolation.md)。

## 說明字數預算

每個工具的 `description` 上限 **200 bytes**，每個參數說明上限 **200 bytes**（僅一個列明的例外，見下）。這個上限由測試強制，不是靠自律。

新增或修改工具時的兩個結果：

- **寫「做什麼」與「什麼會被拒」。** 安全關鍵句——「這不會寄出」「這是公開可見的」「超限整筆拒絕、絕不截斷」——留在說明裡。設計理由、範例、內部代號不留。
- **長版寫在這一頁。** 從說明連到這頁的段落，或該工具自己的規格頁。

### 唯一的例外

`team_handoff` 的 `packet` 參數帶著完整的 TaskPacket 形狀（上限 1,024 bytes）。`build_tool_schema` 把每個參數都宣告成裸 JSON-Schema 字串，所以參數說明是唯一能寫出真實形狀的地方——而 TaskPacket 是整筆拒絕不截斷的，看不到形狀的 AI 員工就產不出合法的封包。完整規格在 [../../spec/task-packet.md](../../spec/task-packet.md)。

所有例外集中在一份清單（`PARAM_CAP_EXEMPTIONS`），並有測試在例外不再必要時失敗。

## 從說明搬出來的長版細節

### `codrive_run` — 三段式執行階梯

每個步驟依序嘗試三段，能用高的就用高的：

- **C-L2 — `api_action`。** 在碰 GUI 之前，先呼叫 `target_app` 已登錄的第三方原生 API/CLI/D-Bus 動作。只要你要的 app＋動作有登錄就優先用它。`action` 是該 app 的短登錄識別字（chromium 的 `open_url`、networkmanager 的 `state`…），`params` 是該動作的 payload，派送時對該動作自己的 schema 驗證。登錄查無或執行失敗會落回該步驟的 `action` 欄位——所以即使設了 `api_action`，`action` 仍然必填。
- **C-L3 — `locate`。** 用 `(role, name)` 在 `target_app` 的 AT-SPI2 無障礙樹查出 `move`/`click` 的座標，取代手猜像素。對版面、解析度、佈景變動的耐受度高得多。對 `text` / `key_name` / `wait` / `take_over` 無效。查無則落回字面 `x`/`y`。
- **C-L1 — 字面 `x`/`y`。** 前兩段缺席或失敗時的最後手段。

其他腳本層規則：

- `target_app` 是**腳本層級單一欄位**，不是逐步驟。一份腳本只驅動一個 app；要驅動第二個 app 就再發一次 `codrive_run`。
- 每個有後果的步驟（`send` / `submit` / `delete` / `purchase` / `other`）在任何一段派送之前都會停下來等人核准。命中拒絕清單（銀行頁面、繞過 CAPTCHA…）則直接拒絕，連連線都不會嘗試。
- 登入／密碼／付款步驟（`take_over`，或 `credential` 類別）會把共用桌面交還給人。你永遠不自己用任何一段送出憑證文字；人把控制權交回來後腳本才續跑。
- 共用桌面上只要有人動手，AI 的座位立刻凍結。被丟掉的那一步會在人交回控制權後自動重試一次。
- `watch_mode: true` 在本次執行剩餘時間內啟用閒置監看。
- 最多 50 步。

### `working_state_handoff` — 結構化模式

兩種模式：

- **純筆記。** 不傳 `status`；沿用舊行為，約 1,200 字處靜默截斷。
- **結構化（Ralph-loop 式）。** 傳 `status`，並與 `next_steps` / `evidence` / `blocker` 一起校驗：
  - `continue` — 必須有 `next_steps`，不得有 `blocker`。
  - `complete` — 必須有 `evidence`，且 `blocker` 與 `next_steps` 都不得存在。沒有證據的自稱完成、或還留著下一步的完成，一律拒絕：「我做完了」不是證據。
  - `blocked` — 必須有具體的 `blocker`。

  合併後的內容由 `config.toml [memory] working_state_handoff_max_bytes`（預設 16384，CJK 安全位元組計算）限制。超過**整筆拒絕**，絕不靜默截斷——因為截掉的可能正是讓這份交接成為權威的證據。

### `skill_search` — 怎麼挑 source

除非你已經知道技能在哪，否則不要動 `source`。

- `all`（預設）— 已設定的 hub 加上這個 AI 員工學到的技能庫，依名稱去重並標註來源。Hub 結果依相關度 × 信任 × 安裝數 × 新鮮度排序，官方第一方技能保底進前段。
- `github` — 預期在公開 GitHub repo 裡的技能。
- `hub` — 策展登錄庫（`anthropic-skills`、`github`、`clawhub`、`lobehub`、`skills-sh`）；用 `hub` 參數收斂到單一個。
- `bank` — 只看這套部署自己學到的技能。此來源不接受 `hub` 參數。

### `evolution_toggle` — 停滯偵測子欄位

除標準旗標外，`field` 還接受 `stagnation_enabled`（bool）、`stagnation_window_seconds`（60–604800）、`stagnation_trigger_threshold`（1–1000）、`stagnation_action`（`log_only` | `suppress`）。見 [evolution-switches.md](../evolution-switches.md)。

### `execute_program`：腳本在哪裡執行

腳本在腳本沙箱裡執行，也就是用 `config.toml [container.sandbox] image` 指定的 image 起一個容器（與[任務沙箱](task-sandbox.md)同一個 image，不會自動下載）。容器以主機使用者身分執行（主機行程是 root 時改用 `1000:1000`，WSL2 上一律如此），丟棄所有 capability、`no-new-privileges`、唯讀根檔案系統、沒有網路、2 GiB 記憶體且不用 swap、256 個行程、1 顆 CPU，以及一個小的 `/tmp` tmpfs。只掛載一個放腳本的私有目錄，唯讀掛在 `/workspace`。`timeout_seconds`（預設 30，最多 300）之外還有 600 秒的硬上限；stdout 與 stderr 合成一份輸出回來（讀取上限 2 MiB，回覆上限 1 MiB）；呼叫被取消時容器會被強制移除。macOS 與 Linux 用 Docker；Windows 先試 WSL2，再試 Docker。

沙箱不能用時（沒有 Docker、image 不在本機、`[container.sandbox]` 無效等），腳本**不會執行**：工具回傳 `Script sandbox unavailable (<代碼>): …` 並附上 `docker pull <image>` 指令，同時寫入稽核事件 `script_sandbox_unavailable`。舊版遇到這種情況會默默改在主機上執行。要恢復舊行為，設 `[container.sandbox] script_when_unavailable = "run_unsandboxed"`（與任務沙箱的 `when_unavailable` 是不同的鍵），之後每次在主機上執行都會記一筆 `script_sandbox_bypassed`。

腳本無法回頭呼叫平台工具：容器裡沒有 RPC socket。

### `computer_*`：由 gateway 執行的 session

這八個工具為每位 AI 員工驅動一個電腦操作 session：一個帶虛擬顯示器與 kiosk 瀏覽器的隔離容器。MCP server 只負責把每次呼叫透過 loopback 轉給 gateway（`POST /api/internal/computer-use`，每個請求各自簽章）；容器由 gateway 持有，所有檢查也由 gateway 執行，所以 gateway 必須在執行中。除非 `agent.toml [capabilities] computer_use = true`，否則這些工具是隱藏的。

| 工具 | 參數 | 備註 |
|---|---|---|
| `computer_session_start` | `task` 字串，選填；`width` 整數 320–1920；`height` 整數 240–1200 | 每位員工一個 session。結果會列出各項上限、高風險動作能否在聊天中確認，以及 `computer_navigate` 能開哪些網站 |
| `computer_screenshot` | 無 | MCP 圖片區塊（PNG，已遮罩），後面接一個文字區塊，寫已用動作數與剩餘時間。整張圖被遮掉時，文字會如實說明，並寫出原因（多個視窗、焦點視窗敏感或讀不到、偵測失敗）與下一步 |
| `computer_click` | `x`、`y` 整數（必填）；`button` 字串 `left`/`right`；`double` 布林 | `double` 只限左鍵 |
| `computer_type` | `text` 字串（必填），1–2,000 字元 | 稽核只記字元數 |
| `computer_key` | `key` 字串（必填）：字母、數字、`+`、`-`、`_` | 例如 `Return`、`ctrl+s` |
| `computer_scroll` | `x`、`y` 整數（必填）；`direction` 字串 `up`/`down`（預設 `down`）；`amount` 整數 1–20（預設 3） | |
| `computer_navigate` | `url` 字串（必填） | 只接受 `https://`，主機必須剛好在該員工的 `[capabilities.computer_use_config] allowed_domains` 上且在 session 啟動時解析成功，連接埠不寫或為 443，不得帶使用者名稱或密碼，最長 2,000 位元組。沒有白名單時 session 沒有網路，呼叫會被拒絕 |
| `computer_session_stop` | `session_id` 字串，選填 | 移除容器 |

整數與布林參數也接受數字字串與 `"true"`/`"false"` 字串。點擊、輸入、按鍵、捲動與導覽各算一個動作，計入 `max_actions`（預設 50）。各項上限、審批與確認規則、網路白名單及其殘留風險，見[瀏覽器自動化](../../features/zh-TW/08-browser-automation.md)。

### `belief_stats` / `belief_settle`：已驗證與自報的結算

只有經過平台價格交叉驗證的結算才計入校準。目前沒有任何正式路徑提供這個交叉驗證：`belief_settle` 把每一筆結算都記成員工自己的回報（`settle_source = "agent_unverified"`），所以現有部署的校準狀態都是「沒有已驗證的結算」。

`belief_settle` 回傳結算後的資料列，加上 `counts_toward_calibration`（布林值）；值為 `false` 時另附 `note`，說明這筆結算有記錄但不計入校準。

`belief_stats` 回傳的內容（與儀表板 `belief.summary` 的 `stats` 相同，另加一個 `note`）：

| 欄位 | 意義 |
|------|------|
| `n_submitted` | 提交過的所有信念，不論是否已結算 |
| `n_settled_all` | 所有已結算的信念（`verified.n + self_reported.n`） |
| `calibration_status` | `no_verified_settlements`、`insufficient_samples`（已驗證 1–29 筆）或 `calibrated`（30 筆以上） |
| `verified` | `n`、`hits`，以及 `hit_rate`、`hit_rate_wilson_low`、`mean_brier`、`overconfidence`（不是 `calibrated` 時都是 `null`） |
| `self_reported` | `n` 與描述用的 `hit_rate`（`n` 為 0 時是 `null`），不是校準 |
| `per_subject[]` | `subject`、`verified`（`n`、`hits`、`mean_brier`）、`self_reported`（`n`） |

舊版的扁平欄位（`n_total`、`n_settled`、`insufficient_samples`、最上層的 `hit_rate` 等）已移除。見[信念迴圈](../../features/zh-TW/46-belief-loop.md)。

### `create_agent` / `agent_remove`：移除後的名稱會被保留

`agent_remove` 會把該員工移到 `~/.duduclaw/agents/_trash/`，並回覆該員工已被移除、管理員可以還原、名稱已被保留，不會回傳路徑。接著 `create_agent` 會對所有 MCP 呼叫端拒絕這個名稱，條件是 trash 裡還有對應項目、`org.toml` 仍記錄這個 id 但沒有對應目錄，或 trash 無法列出；換一個名稱則可以建立。操作者可以從 Dashboard 或終端機重用這個名稱。透過 HTTP 且使用非內部金鑰時，兩個工具都以該金鑰自己的 client id 作為身分執行。細節見[委派隔離](../../features/zh-TW/37-delegation-isolation.md#被移除員工的名稱仍被保留)。

### `responsibility_get` / `responsibility_followup` / `responsibility_ask`：持續任務工具

[持續任務](continuous-responsibilities.md)的三個工具。只有 `config.toml [responsibilities] enabled` 為真、而且呼叫端是 AI 員工身分時才會列出（外部用戶端看不到）；功能關閉時每次呼叫都會被拒絕。它們和 `tasks_*` 工具一樣需要 Admin scope，閘道自己的金鑰帶有這個 scope。

| 工具 | 誰能呼叫 | 規則 |
|---|---|---|
| `responsibility_get` | 任何 AI 員工；操作者金鑰可以讀任何持續任務 | 不帶 `responsibility_id`：列出呼叫者自己的。帶 id：回傳摘要（排程、訂閱、本期花費與次數、進行中的那一次、`cost_not_counted`）。別的員工的持續任務會回「找不到」，除非委派規則讓呼叫者與擁有者有關係 |
| `responsibility_followup` | 擁有這個持續任務的 AI 員工，而且只能在它自己的那一次執行進行中、持續任務為 active 時 | 在 `due_at`（RFC 3339）安排一次性喚醒，至少距現在 `min_wake_interval_secs`，不晚於 `stop_at`。計入本期次數上限，額度用完就拒絕；不會被悄悄改到別的時間 |
| `responsibility_ask` | 同 `responsibility_followup` | 一個問題（最多 1000 字），最多 5 個選項（每個最多 1000 字）；`ttl_secs` 預設一天，限制在 60 秒到 `stop_at` 之間。問題會做注入掃描，給操作者看時以引用文字呈現並截到 200 字，掃描命中時附警告。推播走持續任務的通知條件與每期上限。答案在下一次執行以資料形式交給員工，不給任何權限。停止那一次執行會一併撤回這個問題 |

每個持續任務同時最多一個由 AI 員工安排、還在等的喚醒，所以等待中的 `responsibility_followup` 與 `responsibility_ask` 互斥。操作者金鑰不能用這兩個喚醒工具，因為那等於冒充員工。封閉的錯誤代碼：`not_found`、`not_active`、`no_open_occurrence`、`invalid_due_at`、`period_occurrence_limit`、`agent_followup_limit`、`epoch_changed`、`invalid_question`。

沒有建立、修改、恢復或重新啟用持續任務的工具，也沒有送指示的工具。

## 已棄用的別名仍會列出

已棄用的工具名稱仍會出現在 `tools/list`，說明前面加 `[deprecated → …]` 前綴——因為隱藏它等於讓它叫不動，那和棄用緩衝期的用意正好相反。完整的舊 → 新對照表在 [deprecations.md](../deprecations.md)。目前沒有任何 MCP 工具處於棄用狀態：v1.66.0 那一批別名已在 v1.69.0 移除。

## 相關文件

- [custom-mcp-tool.md](../custom-mcp-tool.md) — 新增工具
- [mcp-bridge.md](../mcp-bridge.md) — 掛載外部 MCP server
- [remote-mcp.md](../remote-mcp.md) — HTTP/OAuth 傳輸
- [../../spec/task-packet.md](../../spec/task-packet.md) — TaskPacket 規格
- [../../spec/reversible-context-ccr.md](../../spec/reversible-context-ccr.md) — `duduclaw_ccr_*` 工具，由直連 API 的 tool loop 注入，不經 MCP server 派送
