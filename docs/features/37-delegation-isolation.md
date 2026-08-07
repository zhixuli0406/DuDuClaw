# 部門與階級隔離

DuDuClaw 1.52 引入一套針對多人團隊的委派管制。你的 AI 員工現在需要遵循組織結構，不是對任何人都言聽計從。

## 問題狀況

早期版本只有單層父子檢查——祖父無法直接指派工作給孫層員工（越級被拒），同部門同事互派也被擋。後端路徑（任務隊列、自動化規則、外部 A2A 呼叫）根本沒檢查，任何人都可以偽造委派指令。

## 三層政策，自主選擇

在 `config.toml` 或儀表板「進階設定」裡配置 `[delegation] policy`：

| 政策 | 行為 | 用途 |
|------|------|------|
| `department`（預設） | 上下級可派工、同部門橫向協作、白名單配對都允許 | 大多數團隊 |
| `hierarchy` | 只允許上下級與白名單配對，沒有同部門橫向 | 嚴格的指揮鏈 |
| `open` | 舊版本行為，除了「不能派給自己」之外不做檢查 | 緊急回退用 |

白名單配對在 `department` 與 `hierarchy` 兩種政策下都有效；只有 `open` 用不到它（本來就全開）。

預設值 `department` 適合一般協作——既已修正越級指派漏洞，同時保留同事互相支援的彈性。

## 工作流程受影響的地方

派工的三種方式都受管制：

1. **直接指派** — 從儀表板、Telegram 或 API 呼叫 `send_to_agent` / `spawn_agent`
   - 頭目能指派給直屬下級，也能指派給孫層（祖父→孫現已允許）
   - 同部門員工之間可互相指派
   - 其他情況被拒，收到明確錯誤訊息說明是誰對誰、缺什麼關係

2. **任務派遣** — 在任務板建立任務（`tasks_create`）或改派（`tasks_update` 的
   `assigned_to`）時指派給某人
   - 檢查同1，拒絕時任務創建/改派失敗
   - 接手自己的任務（`tasks_claim`）不受限
   - Dashboard 的指派操作不受限（都是人類操作）

2b. **多步驟計畫與例行工作** — `create_task` 的每個步驟可以指定執行者、
   `schedule_task` 可以幫別人排定期工作
   - 兩者都是委派，建立當下就檢查；`create_task` 的步驟另外在真正派工前再檢查一次
   - 幫自己排程、指定自己執行不受限

3. **自動化規則** — autopilot 規則裡的 delegate 動作
   - 用 `autopilot` 系統身分檢查，自動放行
   - 規則本身由儀表板守門，需要管理員設定

4. **外部 A2A 呼叫** — 其他系統透過 ACP 協議呼叫 `message/send` 委派任務
   - 預設被拒（fail-closed）
   - 如果信任的外部夥伴需要，設定 `[acp] trusted = true` 才開通
   - 一旦開通，外部呼叫用 `a2a-client` 身分，同樣受委派政策管制

## 同部門是什麼

每個 AI 員工的 `agent.toml [agent] department` 欄位定義所屬部門。

- 同 department 值的員工被視為「同部門」，在 `department` 政策下可互相指派
- 欄位留空或不設 = 無部門，不會與任何人算同部門
- 部門值是純文字對比，例如 `sales` 與 `Sales` 視為不同部門

## 跨部門合作：白名單配對

銷售部主管和倉庫主管不同部門，但要彼此派工？使用白名單：

```toml
[delegation]
policy = "department"
allow = [
  ["sales-lead", "warehouse-lead"],
  ["HR-manager", "finance-lead"]
]
```

- 每筆是一對兩個員工 ID，無序（`["A", "B"]` 等於 `["B", "A"]`）
- 配對後兩人可互相指派，不需上下級關係
- 自我配對（`["x", "x"]`）會被忽略
- 手動編輯設定檔時，格式壞掉的項目（不是恰好兩個字串、有空字串）會被忽略並警告，
  不影響其他配對；ID 拼錯不會被偵測，只是那組配對不會生效
- 從儀表板存檔時規則更嚴：任何一個 ID 找不到對應的 AI 員工就整筆拒絕儲存並指出是哪一個，
  而且一次最多 200 組配對

儀表板「進階設定 → 委派權限」卡提供編輯界面，每個配對同時顯示兩方部門徽章，一眼看出協作意圖。

## 儀表板設定（僅管理員）

**進階設定 → 委派權限**

三個控制項：

1. **政策選擇**（單選）：department / hierarchy / open
   - 每項附一句說明
   - `open` 附紅色風險警語："放棄所有檢查，回到舊版本行為"

2. **跨部門協作**（配對列表）
   - 每列顯示兩個 AI 員工名稱 + 部門徽章
   - 點「+」新增配對（agent picker 下拉）、點「✕」刪除
   - 變更立即生效，無需重啟

3. **狀態指示**
   - 當前政策、配對數量
   - 若有設定警告（例如無效 ID）會顯示

## 審計與除錯

拒絕的委派嘗試都會留痕，依攔截點分成兩個檔案：

- 派工真正要開始執行時被擋（bus 隊列、多步驟計畫）→ `~/.duduclaw/security_audit.jsonl`，
  事件型別 `delegation_denied`
- MCP 工具當下被擋（`send_to_agent` / `spawn_agent` / `tasks_create` / `tasks_update` /
  `create_task` / `schedule_task`）→ `~/.duduclaw/tool_calls.jsonl`，同樣是
  `delegation_denied`；`create_agent` / `agent_update` 的組織調整被擋則是 `org_placement_denied`

`security_audit.jsonl` 的一筆長這樣：

```json
{
  "timestamp": "2026-08-06T10:30:00Z",
  "event_type": "delegation_denied",
  "agent_id": "bob",
  "severity": "warning",
  "details": {
    "sender": "alice",
    "target": "bob",
    "reason": "different_department",
    "policy": "department",
    "path_kind": "bus_dispatch",
    "task_id": "…",
    "message": "委派遭拒：…"
  }
}
```

協助排查：
- 檢查 agent 的 `department` 欄位有沒有拼對
- 核對白名單配對是否正確（大小寫敏感）
- 若整個團隊交不通工作，改成 `hierarchy` 或 `open` 確認是部門隔離問題

## 升級須知

從早期版本升級到 1.52 時：

- **前門路徑零回歸**：父子派工全部仍然通過（新政策更寬鬆）
- **後門路徑變嚴**：直接 append bus_queue.jsonl（有帶發送者欄位時）、任務板指派、
  多步驟計畫、例行工作、外部 A2A 呼叫這些之前無檢查的路徑現在都會執行隔離
  - 若有自動化腳本依賴舊的「任意人派任意人」行為，會看到拒絕錯誤
  - 解法：設定 `policy = "open"` 暫時回退，或補齊組織結構（加 department、補白名單）
  
- **`[acp] trusted` 預設 false**：外部 A2A 呼叫現在被拒，需明確開通

建議保持預設 `department` 政策。即使舊版本沒有任何檢查，新政策對現有合法指派零影響，只在不合理的穿插嘗試時發出警告。

## 技術細節

### 誰視為「系統」（不受隔離管制）

以下身分的指派自動放行：

- `dashboard`（儀表板操作，人類身分）
- `webhook`（來自 webhook 的排程任務）
- `goal-loop-driver`（自主目標迴圈）
- `cron`（排定任務）
- `heartbeat`（心跳回應）
- `autopilot`（自動化規則，前提是規則本身經驗證）

外部 A2A 呼叫預設用 `a2a-client` 身分，**不在清單內**。僅當 `[acp] trusted = true` 時才納入。

上述名稱（加上 `a2a-client`、`default`、以及任何 `__` 開頭的名稱）是**保留字**，
不能拿來建立 AI 員工——否則等於幫自己發一張通行證。

### 這道防線管得到什麼、管不到什麼

管得到：AI 員工之間透過平台功能互相派工的每一條路徑（MCP 工具、任務板、多步驟計畫、
例行工作、任務隊列、外部 A2A）。這是**組織權限邊界**，讓「誰能叫誰做事」跟著組織圖走。

管不到（設計上的已知邊界，不是 bug）：

- **舊格式任務**：1.52 版對完全沒有發送者欄位的隊列任務仍然放行，只記一筆 warning
  （避免升級時把還在排隊的工作全部打掉）。下一版改為拒絕。
- **設定檔層級的改動**：從 v1.52 起，PreToolUse hook 凍結了敏感的組織資料欄位——agent 無法透過 Write/Edit/Bash 工具改寫
  `agent.toml` 的 `name` / `reports_to` / `department`、`config.toml` 的 `[delegation]` / `[acp]` 段、`.mcp.json` 身分區塊、
  `.claude/settings.json` 或 `identity.key`。更改這些設定必須走儀表板或 `agent_update` MCP 工具，由經過審驗的正式管道進行。
  跨員工檔案修改（例如改別人的 SOUL.md）亦被拒絕。非 Claude runtime（codex/gemini 等）在 workspace-write 沙箱下無法寫入 `~/.duduclaw/` 
  目錄，提供沙箱層防線。只有 FullAccess 沙箱例外，屬操作者顯式選擇的極端權限。
- **系統與人類發起的操作**：儀表板、webhook、排程、自動化規則本來就是操作者的意志，
  一律放行。

### 可見範圍過濾

`list_agents` 和 `agent_status` 指令只回傳 caller 有權看見的員工清單。

根據政策，可見範圍包括：

- 自己
- 自己的整個子樹
- 自己的所有祖先
- （`department` 政策）同部門的所有員工
- （所有政策）白名單配對夥伴

試圖探測不可見員工（例如 `agent_status bob` 但 alice 無權看 bob）會回「not found or not visible」，不區分兩者。

### 禁止自助提權

`create_agent` 建立新員工時，caller 只能把新員工掛在自己或自己子樹內的節點。

- 頭目只能建立自己的直屬下級，或接在下級底下
- 不能建立員工掛到主管底下（除非操作者就是那個主管或更上層）
- Dashboard 建立員工不受限（人類操作）

## 身分驗證（進階）

### MCP 身分 Token 機制

從 v1.52 起，每個 agent 的 MCP 呼叫都帶上一組簽名身分 token，防止冒充。

**工作原理**：

1. Gateway 啟動時在 `~/.duduclaw/identity.key` 生成 256-bit 隨機密鑰（檔案權限 0600）
2. Spawn 子 agent 時，gateway 產生簽名 token（HMAC-SHA256，綁定 agent ID）
3. Token 注入環境變數 `DUDUCLAW_AGENT_TOKEN`，子 agent 啟動時帶著它
4. MCP server 端驗證 token 有效性與授權者身分

**軟硬模式**：

- **軟模式**（預設）：`config.toml [delegation] require_identity_token = false`
  - 缺失或無效 token 只發警告，不拒絕 MCP 啟動
  - 用來容錯：升級中或 token 一時未同步的過渡階段

- **嚴格模式**：`config.toml [delegation] require_identity_token = true`
  - 無效身分直接拒絕 MPC 啟動，失敗訊息寫入日誌
  - 適合安全要求高的環境

**升級順序很重要**：先重啟 gateway（讓 agent 的 MCP 設定重新簽署），再改成嚴格模式。
順序反了 agent 會以無效 token 啟動，進嚴格模式後立即被拒。

### 組織資料保護

#### 凍結目標

Agent 經檔案工具（Write/Edit/Bash）的變更會被 PreToolUse hook 攔截：

| 檔案 | 欄位 / 區塊 | 原因 |
|------|-----------|------|
| `agent.toml` | `[agent]` 的 `name`, `reports_to`, `department` | 改這些等於改組織圖，自助提權漏洞 |
| `config.toml` | `[delegation]`, `[acp]` 全段 | 政策設定涉及全團隊安全，不能隨意改 |
| `.mcp.json` | `DUDUCLAW_AGENT_ID`, `DUDUCLAW_AGENT_TOKEN` | 身分令牌，改掉等於冒充別人 |
| `.claude/settings.json` | 整個檔案 | 權限清單等敏感設定統一由儀表板管理 |
| `identity.key` | （整個檔案） | 簽章密鑰，任何更動都破壞身分驗證 |

#### 正確的變更管道

需要改這些設定時：

- **修改 `name`、`reports_to`、`department`** → 儀表板「AI 員工 → 詳情 → 編輯」，或用 MCP `agent_update` 工具
- **調整权限或新增工具** → 儀表板「AI 員工 → 進階設定」，或編輯 `agent.toml [capabilities]` 再手動指定（不走檔案工具）
- **改委派政策或白名單** → 儀表板「進階設定 → 委派權限」，或直接編輯 `config.toml [delegation]` 再重啟 gateway
- **新增 MCP server** → 編輯 `.mcp.json` 的 `tools` 陣列（不要改身分區塊），儀表板「進階設定 → MCP 伺服器」手動新增

攔截會記在 `~/.duduclaw/tool_calls.jsonl` 帶 `org_placement_denied` 標記，便於除錯。

#### 系統身分無限制

系統發送者（dashboard、webhook、cron、autopilot）操作不受限，可改任何設定。這是由設計保證的：這些來源都是操作者的意志體現。

### 白名單輸入的彈性

Dashboard 委派權限卡編輯配對時，兩個欄位都接受：

- **AI 員工顯示名**（`agent.toml [agent] display_name`）：例如「Alice」
- **目錄名**（`agent.toml [agent] id`）：例如「alice-engineer」

輸入時檢索會同時掃描兩個欄位，但儲存一律正規化為目錄名，保證配對穩定不會因為顯示名改動而失效。

### 團隊佈署時的自動部門

使用儀表板的「部署新團隊」功能時，不需要逐一編輯成員的 department 欄位：

1. 上傳或選擇産業範本（`team.toml` 會定義 `[team] industry`，例如 `retail`, `healthcare`）
2. 佈署時所有前台 agent（客服、銷售等）+ 背景 worker（資料整理、報表等）
3. 的 `department` 自動設為該產業代碼
4. 產業包單獨安裝時也帶上 department，新員工加進去時手動指定同部門或用儀表板一次改多個

這樣全隊自動同部門，符合大多數產業團隊的組織結構。需要跨部門協作再補白名單配對。

## 常見 Q&A

**Q：升級後員工派不動工作了**

A：檢查錯誤訊息。若是「不同部門」，補齊 `department` 欄位或加白名單配對。若訊息寫「層級不足」，檢查 `reports_to` 樹結構。

**Q：要不要開啟 `policy = "open"`**

A：不建議。預設 `department` 等同「人人有權派下屬，同事可幫忙」——這是常見協作模式。回到 `open` 只應該是臨時除錯用。

**Q：白名單配對有數量上限嗎**

A：從儀表板存檔時一次最多 200 組。需要更多通常代表該調整組織結構或部門劃分，而不是繼續加例外。

**Q：新員工 `department` 欄位要填什麼**

A：填部門代碼就行（例如 `sales`、`engineering`、`hr`）。字面對比，大小寫敏感。空值或不設 = 無部門，不與任何人算同部門，只能透過上下級或白名單協作。

## 組織資料的權威來源

從 v1.52 起，組織結構有一個中央權威儲存，防止多人編輯導致不一致。

### `org.toml` — 組織圖的單一事實來源

Gateway 首次啟動時，自動掃描所有 agent 目錄下的 `agent.toml`，把組織資料（`reports_to`、`department`）匯入到 `~/.duduclaw/org.toml` 中央檔案。
之後所有對組織結構的變更都透過這個檔案進行：

```toml
# ~/.duduclaw/org.toml — v1.52+ 新增
[agents."alice-engineer"]
display_name = "Alice"
reports_to = "sales-lead"
department = "sales"

[agents."bob-qa"]
display_name = "Bob"
reports_to = "alice-engineer"
department = "sales"
```

這個檔案由 gateway 管理，直接編輯時格式壞掉可能導致啟動失敗，建議改用指令。

### 三個指令管理組織結構

#### 1. `duduclaw org sync` — 同步本機 agent.toml 到權威檔

操作者在終端執行（**不是 AI 員工工作階段裡執行**）：

```bash
duduclaw org sync
```

掃描所有 agent 目錄下 `agent.toml [agent]` 的 `reports_to` / `department` 欄位，更新到 `~/.duduclaw/org.toml`。
如有衝突（例如 alice-engineer 在權威檔裡是 sales，但本機 `agent.toml` 寫的是 engineering），指令會列出每一筆差異並提示確認。

**什麼時候用**：
- 手動編輯了某個 agent.toml 的組織欄位，想把改動同步到權威檔
- 從舊版本升級來，agent.toml 還有組織資料但權威檔空空的

#### 2. `duduclaw org show` — 檢視目前的組織結構

```bash
duduclaw org show
```

以樹狀或表格顯示所有員工、誰匯報給誰、各員工的部門。便於確認組織圖是否符合預期。

#### 3. `duduclaw doctor` — 診斷組織資料不一致

```bash
duduclaw doctor
```

掃描並回報以下問題：
- 權威檔遺失或格式錯誤
- agent.toml 中的 `reports_to` 或 `department` 與權威檔不一致（稱為「漂移」）
- 懸空的 reports_to（指向不存在的員工）
- 循環依賴（A→B→C→A）

輸出格式清楚列舉每個問題與建議修復步驟。

### 為什麼要搬——組織結構是決策，不是數據

舊版本允許 agent 手改自己目錄下的 `agent.toml [agent]` 欄位，直接影響委派判定（例如改 `reports_to` 就改了上司）。
這創造了一個漏洞：一個員工可以自助改組織圖，達成提權目的。

新版本把組織資料改成**中央權威**，代表：
- **誰決定誰匯報給誰**：是操作者（人類），不是 AI 員工
- **手改不生效**：編輯 agent.toml 的組織欄位只會被 doctor 檢舉漂移，不會自動改變委派行為
- **改組織要走流程**：儀表板或 `duduclaw org sync` 指令，確保每一次變更都有人類的簽核意圖

## 跨員工檔案隔離

AI 員工之間應該涇渭分明，一個員工不應能改動另一個員工的檔案。

### 檔案工具禁止跨境

v1.52 之後，Write / Edit / Bash 工具對檔案路徑有邊界檢查：

- **同員工目錄內 ✅**：alice-engineer 能讀寫 `~/.duduclaw/agents/alice-engineer/` 下的所有檔案
- **其他員工目錄內 ❌**：alice-engineer 試圖修改 bob-qa 的 SOUL.md 或記憶檔 → **工具執行被拒**，寫入 `tool_calls.jsonl` 帶 `access_denied` 標記
- **全局敏感檔 ❌**：alice-engineer 無法改 `~/.duduclaw/config.toml`、`org.toml`、`identity.key` 等全局檔案

### PreToolUse hook 攔截層

任何檔案 Write / Edit / Bash 操作都經 hook 檢查，拒絕後立即返回錯誤。無需等到檔案 I/O，防止競態與日誌洩漏。

### 沙箱層防線 — 非 Claude runtime

Codex、Gemini、Antigravity 等非 Claude runtime，在 `workspace-write` 沙箱下無法寫入 `~/.duduclaw/` 目錄。
即使 PreToolUse 有漏洞，沙箱會在系統層阻止。只有 FullAccess 沙箱（操作者顯式選擇的極端權限）會開放，此時應視為授予該員工臨時全域存取。

### 合法跨員工變更的管道

若需改動另一個員工的設定（例如主管調整下屬的 department）：

1. **員工基本資料** — 儀表板「AI 員工 → 詳情 → 編輯」（由人類/系統操作者進行）
2. **組織結構** — `duduclaw org sync` 或儀表板「進階設定 → 組織結構」
3. **能力權限** — 儀表板「AI 員工 → 進階設定」或用 MCP `agent_update` 工具（需管理員驗證）

這些管道都是人類決策的體現，不會被員工自助繞過。
