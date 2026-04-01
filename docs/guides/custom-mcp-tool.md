# Custom MCP Tool Development Guide

> How to add new tools to DuDuClaw's MCP Server
> Applies to: v0.12.0+

---

## Overview

DuDuClaw exposes 52+ MCP tools via JSON-RPC 2.0 over stdin/stdout. This guide explains how to add custom tools that integrate with Claude Code.

## Architecture

```
Claude Code (client)
    ↕  JSON-RPC 2.0 (stdin/stdout)
DuDuClaw MCP Server (crates/duduclaw-cli/src/mcp.rs)
    ↕  Rust function calls
Tool handlers (gateway, agent, memory, inference, etc.)
```

## Step 1: Define the Tool

Add a new `ToolDef` entry to the `TOOLS` array in `crates/duduclaw-cli/src/mcp.rs`:

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

### Naming Conventions

- Use `snake_case` for tool names
- Group related tools with a common prefix: `odoo_*`, `model_*`, `cost_*`
- Keep names concise but descriptive

### Parameter Rules

- `required: true` — Claude Code must provide this parameter
- `required: false` — optional, tool handler must supply a default
- All parameters are passed as JSON values (`serde_json::Value`)

## Step 2: Implement the Handler

Add a match arm in the `handle_tool_call()` function:

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

### Error Handling

Return errors as structured JSON, not panics:

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

### Async Operations

All tool handlers run in a Tokio async context. Use `.await` for I/O:

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

## Step 3: Test the Tool

### Unit Test

Add a test in the same file or a dedicated test module:

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

### Manual Test with Claude Code

```bash
# Start the MCP server
duduclaw mcp-server

# In another terminal, verify the tool appears in the tool list
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | duduclaw mcp-server
```

Then configure Claude Code to use DuDuClaw as an MCP server:

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

## Step 4: Document the Tool

Add the tool to the tool listing in `crates/duduclaw-cli/src/mcp.rs` comments and update `docs/CLAUDE.md` if it represents a significant capability.

## Patterns & Best Practices

### Accessing Agent State

Most tools need access to agent config or state:

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

### Accessing Memory

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

### Rate Limiting

For tools that call external APIs, use rate limiting:

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

### Security Checklist

Before merging a new tool:

- [ ] Input validation on all parameters
- [ ] No hardcoded secrets
- [ ] Rate limiting for external API calls
- [ ] SSRF protection for URL parameters (use `web_fetch` patterns)
- [ ] Audit logging for sensitive operations
- [ ] Respect `CapabilitiesConfig` (check `allowed_tools` / `denied_tools`)
- [ ] Feature gate check if tool is Pro/Enterprise only

## JSON-RPC Protocol Reference

### Request Format

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

### Response Format (Success)

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

### Response Format (Error)

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

## Tool Categories

When adding tools, follow the existing category naming:

| Prefix | Category | Examples |
|--------|----------|---------|
| `send_*` | Messaging | `send_message`, `send_photo`, `send_sticker` |
| `web_*` | Web/Search | `web_search`, `web_fetch_cached`, `web_extract` |
| `agent_*` | Agent management | `agent_status`, `agent_update`, `agent_remove` |
| `memory_*` | Memory operations | `memory_search`, `memory_store` |
| `model_*` | Model management | `model_list`, `model_load`, `model_unload` |
| `inference_*` | Inference control | `inference_status`, `inference_mode` |
| `llamafile_*` | Llamafile lifecycle | `llamafile_start`, `llamafile_stop` |
| `cost_*` | Cost telemetry | `cost_summary`, `cost_agents`, `cost_recent` |
| `odoo_*` | Odoo ERP | `odoo_crm_leads`, `odoo_sale_orders` |
| `skill_*` | Skill ecosystem | `skill_search`, `skill_list` |
| `browserbase_*` | Browser sessions | `browserbase_session`, `browserbase_cost` |
| (none) | Standalone | `emergency_stop`, `tool_approve`, `schedule_task` |
