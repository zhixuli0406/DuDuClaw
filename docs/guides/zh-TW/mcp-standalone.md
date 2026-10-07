# 單獨使用 MCP server

DuDuClaw 的 MCP server 可以不裝 DuDuClaw gateway、單獨執行，讓 Claude Code、Codex 或 Cursor 有一份跨工作階段保留的記憶庫與 Markdown wiki。這頁說明拿到哪些工具、怎麼設定，以及和完整平台的關係。

## 設定

需要 Node.js（用來跑 `npx`），不會在全域安裝任何東西。

### Claude Code

```bash
npx duduclaw mcp init --client claude-code
```

指令會在 `~/.duduclaw` 不存在時建立它、簽發一把金鑰，問過你之後替你執行 `claude mcp add`（user 範圍）。加 `--yes` 可以不問直接做。開新的 Claude Code 工作階段、執行 `/mcp`，應該看到 `duduclaw` 已連線。

Claude Code 裡已經有 `duduclaw` 時，指令會先看它是誰寫的。之前 `mcp init` 寫的會直接取代。其他的（別的金鑰、同名的另一個 server、讀不懂的設定檔）會在簽發金鑰前就停下，並把那一筆列出來（密鑰遮蔽）；要取代它請加 `--replace`。取代前舊的那筆會存到 `~/.duduclaw/mcp_init/claude-duduclaw-<時間>.json`（只有你讀得到），新的加不進去時會把舊的放回去。

沒有安裝 Claude Code CLI，或你回答不要，指令會印出完整的 `claude mcp add …` 讓你自己貼上執行。

### Codex

```bash
npx duduclaw mcp init --client codex
```

會印出一段要貼進 `~/.codex/config.toml` 的設定：

```toml
[mcp_servers.duduclaw]
command = "npx"
args = ["-y", "duduclaw@<版本>", "mcp-server"]
env = { DUDUCLAW_MCP_API_KEY = "ddc_refresh_prod_…" }
```

`<版本>` 是執行 `mcp init` 的那個版本（`duduclaw --version` 印出的值）。設定釘住這個版本，用戶端啟動的 server 就是簽發金鑰的那一版；升級後請再跑一次 `mcp init`。

### Cursor

```bash
npx duduclaw mcp init --client cursor
```

會印出 `~/.cursor/mcp.json` 的內容。檔案裡已經有別的 server 時，只把 `duduclaw` 這一個物件複製進它的 `mcpServers`。

### 指令細節

- 不帶 `--client` 時三種設定都印出來，不會替任何用戶端註冊；但和每次執行一樣，仍會簽發一把新金鑰（效期 90 天）。
- 每個 `--client` 值是各自的金鑰，記憶命名空間和 wiki 也各自分開：`--client print` 拿到的金鑰寫進 `external/standalone-print`，不是 `external/standalone-claude-code`。請用你實際要連的用戶端。
- 全域安裝過 `duduclaw`（`npm install -g duduclaw`）時，設定指向安裝好的執行檔，不經 `npx`。路徑就是執行檔啟動時的路徑，不展開連結；路徑屬於某一個 Node 版本（nvm、volta）時，切換版本後請再跑一次 `mcp init`。
- 印出的 `claude mcp add` 指令用 bash／zsh 的單引號；Windows 版改用 PowerShell／cmd 的雙引號。
- 執行時有設定 `DUDUCLAW_HOME`，印出的設定會帶同一個 `DUDUCLAW_HOME`，server 才會開同一個資料目錄。
- 金鑰只顯示一次，DuDuClaw 在 `~/.duduclaw/mcp_tokens.db` 只存雜湊。用戶端則把金鑰以明文存在自己的設定檔（`~/.claude.json`、`~/.codex/config.toml`、`~/.cursor/mcp.json`），讀得到那個檔案的人都能用這把金鑰，直到它過期或被撤銷。效期 90 天；到期再跑一次 `mcp init` 拿新的，確認可用後用 `duduclaw mcp revoke-token <jti>` 撤銷舊的（指令會列出同一個用戶端還有效的舊金鑰）。`duduclaw mcp list-tokens` 列出全部金鑰。
- 在 DuDuClaw AI 員工的工作階段裡執行會被拒絕，訊息會列出是哪個環境變數。你自己的 shell 若 export 了 `DUDUCLAW_MCP_API_KEY` 也算，unset 後再執行即可。

## 拿到哪些工具

金鑰帶四個 scope：`memory:read`、`memory:write`、`wiki:read`、`wiki:write`。`tools/list` 剛好列出這四個 scope 叫得動的 24 個工具：

| 類別 | 工具 |
|---|---|
| 記憶 | `memory_store`、`memory_search`、`memory_read`、`memory_fetch_batch`、`memory_get_history`、`memory_get_at`、`memory_alias_add`、`memory_alias_list`、`memory_improve` |
| 使用者側寫 | `user_profile_record`、`user_profile_get`、`user_code_profile` |
| 程式碼地圖 | `code_map`（`root` 指定的目錄，沒指定時是 server 的工作目錄） |
| Wiki | `wiki_write`、`wiki_read`、`wiki_ls`、`wiki_search`、`wiki_stats`、`wiki_lint`、`wiki_graph`、`wiki_export`、`wiki_dedup`、`wiki_rebuild_fts`、`wiki_share` |

資料放在哪：

- 記憶：`~/.duduclaw/memory.db`，命名空間 `external/standalone-<用戶端>`（例如 `external/standalone-claude-code`）。每個各自跑過 `mcp init` 的用戶端有自己的命名空間。
- Wiki：`~/.duduclaw/agents/standalone-<用戶端>/wiki/`。
- 共享 wiki（`~/.duduclaw/shared/wiki/`）：`wiki_share` 把你某一頁的摘要複製過去，存成 `sources/standalone-<用戶端>--<頁名>.md`，作者是你的 client id。`wiki_write` 帶 `scope="shared"` 會被拒絕（`-32003`）：共享 wiki 的寫入是以 AI 員工身分進行的，這把金鑰不是員工。共享 wiki 的讀取（`wiki_ls`、`wiki_read`、`wiki_search`、`wiki_stats`、`wiki_lint` 帶 `scope="shared"`）只看得到所有呼叫者都能看的頁面：沒有 `departments/<部門>/` 的頁面，也沒有用 `visible_to_departments` 限定的命名空間。

這把是外部金鑰。好處是 wiki 留在用戶端自己的目錄，用戶端也無法在參數裡指定別的命名空間或員工 id；代價是它只能帶外部用戶端可以有的 scope（`memory:*`、`wiki:*`、`messaging:send`），`mcp init --scopes` 給其他 scope 會被拒絕。

這些 scope 以外的工具不會列出，替 AI 員工動作的工具（下一節）也不會列出。直接用名稱呼叫會被 server 拒絕（`-32003`）；清單跟著權限檢查走，不取代它。

### 哪些沒包含、為什麼

2026-10-07 用 1.70.1 執行檔、全新資料目錄、一把持有所有非 admin scope 的金鑰，每個工具各呼叫一次的結果。前三列記錄的是這些工具原本的行為；現在 server 對所有不是 AI 員工的金鑰一律拒絕：

| Scope 或工具 | 沒有 gateway 時的結果 |
|---|---|
| `working_state_get`／`_set`／`_clear`／`_handoff` | 拒絕（`-32003`）。它們替執行 server 的 AI 員工動作（沒有員工時回 `unknown agent: dudu`） |
| `memory_search_by_layer`、`memory_successful_conversations`、`memory_episodic_pressure`、`memory_consolidation_status` | 拒絕（`-32003`）。它們讀的是預設員工的記憶，不是用戶端的 |
| `shared_wiki_delete`、`wiki_namespace_status`、`canvas_push`、`canvas_clear` | 拒絕（`-32003`）。它們以預設員工身分判斷或動作 |
| `messaging:send`（`send_message`、`send_photo`、`send_sticker`、`synthesize_speech`、`transcribe_audio`） | 需要在 `config.toml` 設好通道（`Unknown channel`）或外部語音服務 |
| `mail:read`／`mail:send` | 不能授予外部金鑰；`mail_*` 和上面幾列一樣會被拒絕。收發信的工作在 gateway 裡 |
| `team:handoff` | 不能授予外部金鑰；需要 gateway goal loop 派出的任務 |
| `odoo:*`、`notion:*`、`google:*`、`github:*` | 需要在儀表板設定整合 |
| `discovery:execute` | `discovery requires an explicit signed caller identity` |
| `skill:execute`（`office_script`） | 不能授予外部金鑰；寫進 AI 員工的目錄 |
| `identity:read`、`files:read` | 可以執行，但不能授予外部金鑰；它們的資料（人員名冊、`~/.duduclaw/attachments`）要透過平台設定 |
| `fork:execute`、`os:native`、`recording`、`db:read` | 需要逐員工的能力開關 |
| 其他（`tasks_*`、`web_fetch_cached`、`agent_*` 等） | `Insufficient scope: Admin required` |

## 升級到完整平台

`duduclaw run` 會在同一個資料目錄啟動 gateway 和儀表板，單獨模式的金鑰照常可用。哪些會帶過去、哪些不會：

- AI 員工看不到單獨模式的記憶：員工讀自己的命名空間（員工 id），單獨模式讀 `external/standalone-<用戶端>`。
- 單獨模式的 wiki 留在 `agents/standalone-<用戶端>/wiki/`。這個目錄沒有 `agent.toml`，gateway 只把它當存放處，不當員工：不會在裡面寫 MCP 設定，也不能用 `standalone-` 開頭的名稱建立員工。用 `wiki_share` 分享過的頁面在共享 wiki，員工讀得到。
- 只有 gateway 才有的工具是給員工用的，不會出現在單獨模式的金鑰。要讓用戶端用更多工具，用 `duduclaw mcp issue-refresh-token` 另外簽一把金鑰。

## 疑難排解

- `MCP authentication failed: DUDUCLAW_MCP_API_KEY environment variable not set. Run: duduclaw mcp init --client claude-code`：用戶端啟動 server 時沒帶金鑰。重跑 `mcp init`，或補上 `env` 設定。
- `API key not found in registry`：金鑰被撤銷，或 server 讀的資料目錄和 `mcp init` 寫入的不同（檢查 `DUDUCLAW_HOME`）。
- `API key expired`：金鑰效期 90 天，重跑 `mcp init`。

## 上架到 MCP Registry

Registry 用的 metadata 是 `distribution/registries/mcp/server.json`；送件步驟（擁有者執行，每次 release 之後）見 [`distribution/registries/README.md`](../../../distribution/registries/README.md)。
