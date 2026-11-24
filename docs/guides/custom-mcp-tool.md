# Custom MCP tool development guide

> How to add new tools to DuDuClaw's MCP Server
> Applies to: v0.12.0+

---


## Getting and verifying the caller identity

An AI cannot claim another identity by changing environment variables, parameters, or its working directory. On installs where all AI employees run as the same operating-system user, a process that can read other processes' environments could borrow another run's credential while that run is active; the credential stops working when the run ends. Native Linux protected mode isolates employees as separate users. In protected mode no run credential is issued: it keeps its own capability records, and `duduclaw identity whoami` reports that protected mode uses its own capability identity.

This section covers gateway-managed runs. Standalone `duduclaw agent` terminal sessions do not register with this gateway registry. Installation/version/auth probes and the opt-in `/model` menu probe are not identity-bearing work turns.

For local tools launched by DuDuClaw, read `DUDUCLAW_RUN_CREDENTIAL` and verify it with the local gateway on **each tool call**, or use the CLI below. Do not infer identity from cwd, a display name, tool arguments, or `DUDUCLAW_ON_BEHALF_OF`. Query only locally, never forward the credential. Do not put it in URLs, argv, logs, tool results or transcripts. The non-secret `DUDUCLAW_IDENTITY_URL` points to a dedicated listener on `127.0.0.1` with an ephemeral port, separate from the dashboard router. Only the gateway process holding `gateway.lock` starts it. The CLI reads this variable first, then `<gateway home>/run/identity-url` (owner-only, 0600 on Unix), and refuses non-loopback URLs. The URL is a routing hint, not identity proof.

```sh
duduclaw identity whoami --json

```

An active response contains `active: true`, `agent_id`, `principal`, `session_id`, `turn_id`, `task_id`, `runtime`, `issued_at` and `expires_at` (Unix seconds). A human principal is `{"type":"human","user_id":"…"}`; cron, heartbeat and task work use `{"type":"background","kind":"…"}`. Unverified requesters are background/unattributed, never an administrator. Session/task ids can be null. These are gateway records, not statements supplied by the model. Check the verified principal against your tool's own authorization rules; identity alone grants no permission.

Malformed, unknown, expired and revoked credentials all return HTTP 200 with exactly `{"active":false}`; non-loopback peers, missing peer information and rate-limited requests receive the same refusal. Each valid credential has its own 600-query/minute bucket, bounded by the 8,192-entry registry; malformed/unknown requests share a separate bucket. Requests with `Forwarded`, `X-Forwarded-For`, `X-Forwarded-Host`, `X-Real-IP`, `Via` or `Origin` also receive the uniform inactive response. Each request is limited to 1 KiB. The CLI exits non-zero for a missing/inactive credential or an unavailable gateway. Always deny when verification fails; do not cache an active result across calls.

Each CLI attempt receives a fresh opaque `ddc_run_` credential made from 32 random bytes. The gateway retains only SHA-256 hashes, with at most 8,192 live entries. A guard revokes it when the attempt ends, errors or is cancelled; expiry is the run timeout plus 60 seconds, capped at six hours, and a gateway restart invalidates every credential. Local API tool loops retain a lease for the lifetime of their tool registry. `mcp-proxy` forwards this variable to local third-party servers while still stripping the older identity variables. Codex requires MCP `env_vars` support; the credential travels through the process environment, never its command line. Actual Codex shell/MCP propagation still needs live validation for the installed version.

This prevents forging claims, not theft of another live bearer credential: processes sharing an OS user can potentially read one another's environment. Keep local tools trusted; use OS isolation for hostile workloads. In protected mode no run credential is issued: it keeps its own capability records, and `duduclaw identity whoami` reports that protected mode uses its own capability identity. The existing `DUDUCLAW_AGENT_TOKEN` scheme and DuDuClaw MCP server identity verification are unchanged.


## Overview

DuDuClaw exposes 200+ MCP tools (206 as of v1.56) via JSON-RPC 2.0 over stdin/stdout. This guide explains how to add custom tools that integrate with Claude Code.

## Architecture

```
Claude Code (client)
    ↕  JSON-RPC 2.0 (stdin/stdout)
DuDuClaw MCP Server (crates/duduclaw-cli/src/mcp.rs)
    ↕  Rust function calls
Tool handlers (gateway, agent, memory, inference, etc.)
```

## Step 1: Define the tool

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

### Naming conventions

- Use `snake_case` for tool names
- Group related tools with a common prefix: `odoo_*`, `model_*`, `cost_*`
- Keep names concise but descriptive

### Parameter rules

- `required: true` — Claude Code must provide this parameter
- `required: false` — optional, tool handler must supply a default
- All parameters are passed as JSON values (`serde_json::Value`)

## Step 2: Implement the handler

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

### Error handling

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

### Async operations

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

## Step 3: Test the tool

### Unit test

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

### Manual test with Claude Code

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

## Step 4: Document the tool

Add the tool to the tool listing in `crates/duduclaw-cli/src/mcp.rs` comments and update `docs/CLAUDE.md` if it represents a significant capability.

## Patterns & best practices

### Accessing agent state

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

### Security checklist

Before merging a new tool:

- [ ] Input validation on all parameters
- [ ] No hardcoded secrets
- [ ] Rate limiting for external API calls
- [ ] SSRF protection for URL parameters (use `web_fetch` patterns)
- [ ] Audit logging for sensitive operations
- [ ] Feature gate check if tool is Pro/Enterprise only

`agent.toml [capabilities] allowed_tools` / `denied_tools` no longer need a
per-tool check: every call — stdio, HTTP/SSE, and the openai-compat
tool-loop's internal MCP client alike — is dispatched through the shared
`McpDispatcher::dispatch_tool_call` choke point (`mcp_dispatch.rs`), which
enforces the caller's `[capabilities]` allow/deny list against the tool's
base name (an `mcp__<server>__` qualifier is stripped before matching, and
`denied_tools` always wins over `allowed_tools`) before your handler is ever
invoked. A new tool registered in `handle_tool_call()` is covered
automatically. This closed a real gap: before this enforcement moved to the
choke point, `allowed_tools` / `denied_tools` only reached the Claude CLI
spawn's `--allowedTools` / `--disallowedTools` flags, so a caller that talked
to the MCP server directly (bypassing the CLI spawn) was unrestricted by
them.

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
| (none) | Standalone | `emergency_stop`, `tool_approve` |
