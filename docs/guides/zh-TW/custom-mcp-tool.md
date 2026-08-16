# Custom MCP tool development guide

> 如何為 DuDuClaw 的 MCP Server 新增工具
> 適用版本：v0.12.0+

---

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

    // Your logic here
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
// Good: structured error
if input.is_empty() {
    return Ok(json!({
        "status": "error",
        "error": "input parameter cannot be empty"
    }));
}

// Bad: panic
assert!(!input.is_empty());  // Never do this in a tool handler
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
# Start the MCP server
duduclaw mcp-server

# In another terminal, verify the tool appears in the tool list
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
    // ... call external API
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
| （無） | 獨立工具 | `emergency_stop`, `tool_approve`, `schedule_task` |
