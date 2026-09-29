# MCP 工具：AI 員工看得到哪些、為什麼

DuDuClaw 的 MCP server 透過標準的 `tools/list` 宣告工具。這頁解釋兩件最容易誤解的事：

1. 為什麼 AI 員工看到的工具，比伺服器實作的少；
2. 工具說明被限長之後，原本那些長篇細節去哪了。

要新增工具請先看 [custom-mcp-tool.md](../custom-mcp-tool.md)；這頁講的是宣告面，不是實作。

## 規則：看得到 ⇔ 叫得動

`tools/list` 只列出呼叫端**當下真的叫得動**的工具。下面每一道過濾，都對應派工層本來就會擋的同一道閘，所以「AI 員工讀到的清單」與「伺服器會接受的呼叫」永遠是同一組。

這既是正確性問題，也是成本問題。工具 schema 是**每次開機都要付的固定 prompt 成本**：CLI 每個 session 讀一次 `tools/list`，整包在第一個使用者 token 之前就進了模型的上下文。一個註定被拒的工具 schema 等於付兩次錢——token 付一次，模型繞著一個用不了的工具規劃再付一次。

隱藏工具**不是**授權決定。呼叫未列出的工具仍然會打到真正的閘，仍然會被拒絕，而且回的是閘自己的訊息。

### 被過濾的項目

| 過濾條件 | 來源 | 關閉／空值時的效果 |
|---|---|---|
| 外部用戶端白名單 | `principal.is_external` | 只列 7 個工具 |
| Google Workspace | `config.toml [integrations] google_workspace` | 隱藏 19 個 Google 工具 |
| GitHub | `config.toml [integrations] github` | 隱藏 5 個 `github_*` |
| `denied_tools` / `allowed_tools` | `agent.toml [capabilities]` | 被拒的隱藏；允許清單非空時其餘全隱藏 |
| `os_native` | `agent.toml [capabilities]` | 隱藏 6 個 `os_*` 自動化工具 |
| `recording` | `agent.toml [capabilities]` | 隱藏 5 個錄製工具 |
| `system_operator` | `agent.toml [capabilities]` | 隱藏 19 個值班機操作工具 |
| `codrive` | `agent.toml [capabilities]` | 隱藏 `codrive_run` / `codrive_status` |
| `computer_use` | `agent.toml [capabilities]` | 隱藏 7 個 `computer_*` |
| `db_sources` | `agent.toml [capabilities]` | 隱藏 4 個 `db_*` |
| `[fork] enabled` | `agent.toml` | 隱藏 6 個分支工具 |
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

## 已棄用的別名仍會列出

已棄用的工具名稱仍會出現在 `tools/list`，說明前面加 `[deprecated → …]` 前綴——因為隱藏它等於讓它叫不動，那和棄用緩衝期的用意正好相反。完整的舊 → 新對照表在 [deprecations.md](../deprecations.md)。

## 相關文件

- [custom-mcp-tool.md](../custom-mcp-tool.md) — 新增工具
- [mcp-bridge.md](../mcp-bridge.md) — 掛載外部 MCP server
- [remote-mcp.md](../remote-mcp.md) — HTTP/OAuth 傳輸
- [../../spec/task-packet.md](../../spec/task-packet.md) — TaskPacket 規格
- [../../spec/reversible-context-ccr.md](../../spec/reversible-context-ccr.md) — `duduclaw_ccr_*` 工具，由直連 API 的 tool loop 注入，不經 MCP server 派送
