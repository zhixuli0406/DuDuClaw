# 自訂 MCP 工具開發指南

> 如何為 DuDuClaw 的 MCP Server 新增工具
> 適用版本：v0.12.0+

---


## 取得與查證呼叫者身分（Getting and verifying the caller identity）

本節適用 gateway 管理的執行。獨立的 `duduclaw agent` 終端互動模式尚未接入這份 gateway registry。安裝／版本／登入狀態探測，以及選用的 `/model` 選單探測，也不屬於帶身分的工作回合。

AI 無法藉由修改環境變數、參數或工作目錄冒用其他身分。gateway 管理的每次執行都有自己的短效憑證，而且 gateway 只回答兩種程序的查詢：它為該次執行啟動的 CLI 程序，以及這個程序的子孫程序。原生 Linux 受保護模式會以不同作業系統帳號隔離員工。受保護模式不簽發 run credential，使用自己的 capability records；`duduclaw identity whoami` 會明確提示此模式使用獨立的 capability 身分。

DuDuClaw 啟動的本機工具可讀取 `DUDUCLAW_RUN_CREDENTIAL`，每次呼叫工具時向本機 gateway 查證，或執行下列指令。不要信任 cwd、顯示名稱、工具參數或 `DUDUCLAW_ON_BEHALF_OF`。**只在本機查詢，絕不轉送憑證**；不要把它放進網址、命令列參數、日誌、工具結果或對話紀錄。

gateway 另外設定兩個非機密變數。`DUDUCLAW_IDENTITY_ENDPOINT` 指向有版本號的本機 IPC 端點：Unix 上是 `unix:v1:<絕對路徑>`，socket 放在 `<gateway home>/run/` 底下的私有目錄（僅擁有者可存取，權限 0700）；Windows 上是 `pipe:v1:\\.\pipe\duduclaw-identity-<hex>`，這個 named pipe 的 DACL 只允許目前使用者，並拒絕遠端連線。`DUDUCLAW_IDENTITY_SERVER`（`v1:<pid>:<啟動時間>`）是 gateway 的程序身分。用戶端寫出任何憑證位元組之前，會先向作業系統確認連線另一端是哪個程序，PID 與程序啟動時間都相符才繼續。只有持有 `gateway.lock` 的 gateway 程序會啟動這個端點。`<gateway home>/run/identity-endpoint` 只供操作者診斷，CLI 不會從這個檔案取得端點或伺服器身分。舊版的 `127.0.0.1` HTTP listener 與 `DUDUCLAW_IDENTITY_URL` 已移除，也沒有退回舊路徑：gateway 會在每次啟動時移除這個變數，若執行環境仍帶有它，CLI 一律視為 inactive。

**誰可以查詢。** gateway 對每條連線向核心取得呼叫者的 PID（Unix socket 的 peer credentials，Windows 則是 named pipe 的用戶端 PID），從不採用請求內容裡的值。接著從呼叫者沿父程序往上走，直到本次執行啟動的 CLI 程序，途中每個程序都核對 PID 與啟動時間。父程序的啟動時間不能晚於子程序，整條鏈最多 64 層。鏈上每個程序以及本次執行的 CLI 程序都必須仍在執行。不在這棵程序樹裡的呼叫者，即使拿出正確的憑證也會被拒絕。實際上：

- 本次執行的 CLI 直接啟動、或經由 `mcp-proxy` 啟動的自訂 MCP server、hook 或腳本，都是子孫程序，可以查證身分。你的工具以子程序方式執行的 `duduclaw identity whoami` 也一樣。
- 在執行之外啟動的 daemon 或服務（launchd、systemd 單元、你自己先開好的伺服器、屬於其他執行的程序）不是子孫程序，即使把憑證複製到它的環境變數，也只會收到 `{"active":false}`。請在本次執行啟動的程序裡查證，只把查證結果交給這類服務，不要交出憑證。
- 脫離後被重新指定父程序的程序（例如兩次 fork 後由 init 或 launchd 接手的 daemon）會斷掉這條鏈而被拒絕。本次執行的 CLI 程序結束後，它所有子孫程序手上的憑證都隨之失效。

```sh
duduclaw identity whoami --json

```

有效時回傳 `active: true`、`agent_id`、`principal`、`session_id`、`turn_id`、`task_id`、`runtime`、`issued_at` 與 `expires_at`（Unix 秒數）。人的身分為 `{"type":"human","user_id":"…"}`；排程、心跳與任務為 `{"type":"background","kind":"…"}`。未確認的交辦人歸為 background／unattributed，不會變成管理員。對話與任務代號可以是 null。這些值來自 gateway 的登記，不能由模型自填；工具仍須依查證後的身分檢查自己的權限規則。

格式錯誤、不存在、過期或撤銷的憑證，不在本次執行程序樹內的呼叫者，無法取得連線來源，以及超過限速的請求，一律只收到 `{"active":false}`。每個有效憑證各有每分鐘 600 次的限流桶，隨最多 8,192 筆 registry entry 保存；格式錯誤與未知請求另共用一桶。憑證缺失或無效、`DUDUCLAW_IDENTITY_ENDPOINT` 或 `DUDUCLAW_IDENTITY_SERVER` 缺失或格式不符、伺服器身分不符，或 gateway 無法連線時，CLI 以非零狀態結束。查證失敗就拒絕，不跨次快取有效結果。

無法執行 CLI 的工具可以直接實作協定：每條連線只處理一個請求，不支援 HTTP、重新導向、代理或持續連線。先送 4 位元組 big-endian 長度，接著一個 JSON 物件 `{"version":1,"operation":"introspect","credential":"ddc_run_…"}`（最多 4 KiB），讀取一個同樣格式的 JSON 回應（最多 16 KiB），再寫一個位元組 `0x01` 表示已收到；整段交換須在 3 秒內完成。寫出第一個位元組之前，用戶端必須先完成上面說的伺服器身分檢查。所用平台或語言做不到這一步時，請改用 CLI。

每次 CLI 嘗試都使用新的 `ddc_run_` 憑證，由 32 個隨機位元組產生。gateway 只存 SHA-256 雜湊，最多保留 8,192 筆。gateway 把憑證綁定到啟動的程序之後，憑證才能使用。執行結束、錯誤或取消時由 guard 撤銷；期限為執行逾時加 60 秒，最長六小時；gateway 重啟後全部失效。本機 API 工具迴圈每啟動一個 stdio MCP server，就給它一張綁定該 server 程序的憑證。`mcp-proxy` 會把憑證、`DUDUCLAW_IDENTITY_ENDPOINT` 與 `DUDUCLAW_IDENTITY_SERVER` 傳給本機第三方伺服器，舊身分變數（包括 `DUDUCLAW_IDENTITY_URL`）仍會移除。Codex 需支援 MCP `env_vars`，憑證只走環境變數，不放進 argv；各安裝版本的 shell／MCP 繼承仍待實機驗證。

**限制。** 程序樹檢查可以擋下偽造的聲明，無關程序就算複製到憑證也用不了。以下情況不在保證範圍內：

- Windows 上，同一帳號的程式若在建立程序時刻意指定父程序（`PROC_THREAD_ATTRIBUTE_PARENT_PROCESS`），可以通過祖先檢查。named pipe 也只回報 PID，讀取 PID 到開啟該程序之間仍有短暫空窗。
- macOS，以及沒有 `SO_PEERPIDFD`（6.5 版加入）的 Linux 核心，核心只回報 PID，無法固定住程序，因此仍有很短的 PID 重用空窗。
- 注入合法子孫程序的程式碼、替其他程序轉送請求的合法子孫程序，以及把已連線的 socket 或 pipe handle 交給其他程序，這些都等同該子孫程序本身。

請只執行可信任的本機工具；不可信任的工作需要作業系統隔離。受保護模式不簽發 run credential，使用自己的 capability records；`duduclaw identity whoami` 會明確提示此模式使用獨立的 capability 身分。既有 `DUDUCLAW_AGENT_TOKEN` 與 DuDuClaw MCP server 的身分驗證維持原樣。


## Overview

DuDuClaw 透過 stdin/stdout 上的 JSON-RPC 2.0，曝露 200+ 個 MCP 工具（截至 v1.56 為 206 個）。本指南說明如何新增與 Claude Code 整合的自訂工具。

## Architecture

```
Claude Code (client)
    ↕  JSON-RPC 2.0 (stdin/stdout)
DuDuClaw MCP Server (crates/duduclaw-cli/src/mcp.rs)
    ↕  Rust function calls
Tool handlers (gateway, agent, memory, inference, etc.)
```

## Step 1: Define the tool

在 `crates/duduclaw-cli/src/mcp.rs` 的 `TOOLS` 陣列中新增一筆 `ToolDef`：

```rust
ToolDef {
    name: "my_custom_tool",
    description: "Brief description of what this tool does",
    params: &[
        ParamDef {
            name: "input",
            description: "The input parameter",
            required: true,
        },
        ParamDef {
            name: "options",
            description: "Optional configuration",
            required: false,
        },
    ],
},
```

### Naming conventions

- 工具名稱一律使用 `snake_case`
- 相關工具用共同前綴分組：`odoo_*`、`model_*`、`cost_*`
- 名稱盡量簡短但要能表意

### Parameter rules

- `required: true`：Claude Code 必須提供這個參數
- `required: false`：選填，tool handler 要自行補上預設值
- 所有參數都以 JSON 值傳遞（`serde_json::Value`）

## Step 2: Implement the handler

在 `handle_tool_call()` 函式裡新增一個 match arm：

```rust
"my_custom_tool" => {
    let input = get_string_param(&params, "input")?;
    let options = params.get("options")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    // 這裡放你的邏輯
    let result = do_something(input, options).await?;

    Ok(json!({
        "status": "ok",
        "result": result
    }))
}
```

### Error handling

錯誤要回傳結構化 JSON，不要 panic：

```rust
// 好：結構化錯誤
if input.is_empty() {
    return Ok(json!({
        "status": "error",
        "error": "input parameter cannot be empty"
    }));
}

// 不好：panic
assert!(!input.is_empty());  // tool handler 裡絕對不要這樣做
```

### Async operations

所有 tool handler 都跑在 Tokio async context 裡，I/O 一律用 `.await`：

```rust
"my_async_tool" => {
    let url = get_string_param(&params, "url")?;

    let response = reqwest::get(&url).await
        .map_err(|e| DuDuClawError::Network(e.to_string()))?;

    let body = response.text().await
        .map_err(|e| DuDuClawError::Network(e.to_string()))?;

    Ok(json!({ "status": "ok", "content": body }))
}
```

## Step 3: Test the tool

### Unit test

在同一個檔案或獨立的測試模組裡加測試：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_my_custom_tool() {
        let params = json!({
            "input": "test value",
            "options": "custom"
        });

        let result = handle_tool_call("my_custom_tool", &params).await;
        assert!(result.is_ok());

        let value = result.unwrap();
        assert_eq!(value["status"], "ok");
    }
}
```

### Manual test with Claude Code

```bash
# 啟動 MCP server
duduclaw mcp-server

# 在另一個 terminal 驗證工具有出現在工具清單裡
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | duduclaw mcp-server
```

接著把 Claude Code 設定成用 DuDuClaw 當 MCP server：

```json
// .mcp.json
{
  "mcpServers": {
    "duduclaw": {
      "command": "duduclaw",
      "args": ["mcp-server"]
    }
  }
}
```

## Step 4: Document the tool

把工具加進 `crates/duduclaw-cli/src/mcp.rs` 的工具清單註解，若它代表某項重要能力，也一併更新 `docs/CLAUDE.md`。

## Patterns & best practices

### Accessing agent state

大多數工具需要存取 agent 的設定或狀態：

```rust
"agent_info_tool" => {
    let agent_name = get_string_param(&params, "agent")?;
    let agents_dir = duduclaw_agent::get_agents_dir();
    let config = duduclaw_agent::load_agent_config(&agents_dir, &agent_name)?;

    Ok(json!({
        "status": "ok",
        "agent": config.identity.name,
        "role": format!("{:?}", config.identity.role),
    }))
}
```

### Accessing memory

```rust
"memory_tool" => {
    let agent_id = get_string_param(&params, "agent_id")?;
    let query = get_string_param(&params, "query")?;

    let engine = SqliteMemoryEngine::open(&memory_db_path(&agent_id))?;
    let results = engine.search(&query, 10).await?;

    Ok(json!({
        "status": "ok",
        "memories": results.iter().map(|m| json!({
            "content": m.content,
            "tags": m.tags,
            "importance": m.importance,
        })).collect::<Vec<_>>()
    }))
}
```

### Rate limiting

呼叫外部 API 的工具要加上速率限制：

```rust
use duduclaw_security::rate_limiter::RateLimiter;

static LIMITER: OnceLock<RateLimiter> = OnceLock::new();

"external_api_tool" => {
    let limiter = LIMITER.get_or_init(|| RateLimiter::new(10, Duration::from_secs(60)));
    if !limiter.check("external_api") {
        return Ok(json!({
            "status": "error",
            "error": "rate limit exceeded, try again in 60s"
        }));
    }
    // ... 呼叫外部 API
}
```

### Security checklist

合併新工具之前先確認：

- [ ] 所有參數都有做輸入驗證
- [ ] 沒有寫死秘密（hardcoded secrets）
- [ ] 呼叫外部 API 的地方有速率限制
- [ ] URL 參數有做 SSRF 防護（參考 `web_fetch` 的做法）
- [ ] 敏感操作有稽核紀錄（audit logging）
- [ ] 若工具僅限 Pro/Enterprise 使用，有做 feature gate 檢查

`agent.toml [capabilities] allowed_tools` / `denied_tools` 已經不需要逐工具各自檢查：所有呼叫（stdio、HTTP/SSE，以及 openai-compat tool-loop 內部 MCP client）都會經過共用的
`McpDispatcher::dispatch_tool_call` 這個統一節點（`mcp_dispatch.rs`），它會拿呼叫者的 `[capabilities]` 允許／拒絕清單去比對工具的基礎名稱（比對前會先去掉 `mcp__<server>__` 這類前綴，且 `denied_tools` 永遠優先於 `allowed_tools`），確認過關才會呼叫到你的 handler。在 `handle_tool_call()` 裡新註冊的工具會自動套用這層保護。這修補了一個真實存在的破口：在強制檢查搬到這個節點之前，`allowed_tools` / `denied_tools` 只作用在 Claude CLI spawn 的 `--allowedTools` / `--disallowedTools` 旗標上，也就是說直接跟 MCP server 對話（繞過 CLI spawn）的呼叫者完全不受這兩個設定約束。

## JSON-RPC protocol reference

### Request format

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "tools/call",
  "params": {
    "name": "my_custom_tool",
    "arguments": {
      "input": "value",
      "options": "config"
    }
  }
}
```

### Response format (success)

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [
      {
        "type": "text",
        "text": "{\"status\":\"ok\",\"result\":\"...\"}"
      }
    ]
  }
}
```

### Response format (error)

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "error": {
    "code": -32602,
    "message": "Missing required parameter: input"
  }
}
```

## Tool categories

新增工具時請沿用既有的分類命名：

| 前綴 | 類別 | 範例 |
|--------|----------|---------|
| `send_*` | 訊息傳送 | `send_message`, `send_photo`, `send_sticker` |
| `web_*` | 網頁／搜尋 | `web_search`, `web_fetch_cached`, `web_extract` |
| `agent_*` | Agent 管理 | `agent_status`, `agent_update`, `agent_remove` |
| `memory_*` | 記憶操作 | `memory_search`, `memory_store` |
| `model_*` | 模型管理 | `model_list`, `model_load`, `model_unload` |
| `inference_*` | 推論控制 | `inference_status`, `inference_mode` |
| `llamafile_*` | Llamafile 生命週期 | `llamafile_start`, `llamafile_stop` |
| `cost_*` | 成本遙測 | `cost_summary`, `cost_agents`, `cost_recent` |
| `odoo_*` | Odoo ERP | `odoo_crm_leads`, `odoo_sale_orders` |
| `skill_*` | Skill 生態系 | `skill_search`, `skill_list` |
| （無） | 獨立工具 | `emergency_stop`, `tool_approve` |
