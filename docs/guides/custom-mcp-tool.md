# Custom MCP tool development guide

> How to add new tools to DuDuClaw's MCP Server
> Applies to: v0.12.0+

---


## Getting and verifying the caller identity

An AI cannot claim another identity by changing environment variables, parameters, or its working directory. Each gateway-managed run gets its own short-lived credential, and the gateway answers for it only when the question comes from the CLI process it spawned for that run or from one of that process's descendants. Native Linux protected mode isolates employees as separate users. In protected mode no run credential is issued: it keeps its own capability records, and `duduclaw identity whoami` reports that protected mode uses its own capability identity.

This section covers gateway-managed runs. Standalone `duduclaw agent` terminal sessions do not register with this gateway registry. Installation/version/auth probes and the opt-in `/model` menu probe are not identity-bearing work turns.

For local tools launched by DuDuClaw, read `DUDUCLAW_RUN_CREDENTIAL` and verify it with the local gateway on **each tool call**, or use the CLI below. Do not infer identity from cwd, a display name, tool arguments, or `DUDUCLAW_ON_BEHALF_OF`. Query only locally, never forward the credential. Do not put it in URLs, argv, logs, tool results or transcripts.

The gateway also sets two non-secret variables. `DUDUCLAW_IDENTITY_ENDPOINT` names a versioned local IPC endpoint: `unix:v1:<absolute path>` for a Unix socket inside a private directory under `<gateway home>/run/` (owner-only, mode 0700), or `pipe:v1:\\.\pipe\duduclaw-identity-<hex>` on Windows for a named pipe whose DACL grants only the current user and which rejects remote clients. `DUDUCLAW_IDENTITY_SERVER` (`v1:<pid>:<start time>`) is the gateway's process identity. Before writing any credential bytes, the client asks the operating system which process is on the other end of the connection and refuses unless both the PID and the process start time match. Only the gateway process holding `gateway.lock` starts the endpoint. `<gateway home>/run/identity-endpoint` is written for operator diagnostics only; the CLI never takes the endpoint or the server identity from it. The `127.0.0.1` HTTP listener and `DUDUCLAW_IDENTITY_URL` from earlier builds are removed with no fallback: the gateway strips that variable from every spawn, and the CLI treats a run that still has it set as inactive.

**Who can ask.** For each connection the gateway reads the caller's PID from the kernel (Unix socket peer credentials, or the named-pipe client PID on Windows), never from the request. It then walks the parent chain from the caller up to the CLI process it spawned for this run, checking the PID and start time of every process on the way. A parent can never be younger than its child, and the chain may have at most 64 parent edges. Every process on the chain, and the run's CLI process itself, must still be running. A caller outside that tree is refused even when it presents the correct token. In practice:

- A custom MCP server, hook or script that the run's CLI starts, directly or through `mcp-proxy`, is a descendant and can verify its identity. So is `duduclaw identity whoami` when your tool runs it as a child process.
- A daemon or service started outside the run (a launchd or systemd unit, a server you started yourself, a process belonging to another run) is not a descendant and receives `{"active":false}`, even if the credential is copied into its environment. Verify inside the process the run started, and pass only the verified result, never the credential, to such a service.
- A process that detaches and is reparented (for example a double-forked daemon adopted by init or launchd) loses the chain and is refused. When the run's CLI process exits, the credential stops working for all of its descendants.

```sh
duduclaw identity whoami --json

```

An active response contains `active: true`, `agent_id`, `principal`, `session_id`, `turn_id`, `task_id`, `runtime`, `issued_at` and `expires_at` (Unix seconds). A human principal is `{"type":"human","user_id":"…"}`; cron, heartbeat and task work use `{"type":"background","kind":"…"}`. Unverified requesters are background/unattributed, never an administrator. Session/task ids can be null. These are gateway records, not statements supplied by the model. Check the verified principal against your tool's own authorization rules; identity alone grants no permission.

Malformed, unknown, expired and revoked credentials, callers outside the run's process tree, missing peer information and rate-limited requests all receive exactly `{"active":false}`. Each valid credential has its own 600-query/minute bucket, bounded by the 8,192-entry registry; malformed/unknown requests share a separate bucket. The CLI exits non-zero for a missing/inactive credential, a missing or malformed `DUDUCLAW_IDENTITY_ENDPOINT` or `DUDUCLAW_IDENTITY_SERVER`, a server identity mismatch, or an unavailable gateway. Always deny when verification fails; do not cache an active result across calls.

For tools that cannot run the CLI, the protocol is one request per connection, with no HTTP, redirects, proxies or keep-alive. Send a 4-byte big-endian length followed by one JSON object, `{"version":1,"operation":"introspect","credential":"ddc_run_…"}` (at most 4 KiB), read one framed JSON response (at most 16 KiB), then write a single `0x01` byte as acknowledgement; the exchange must finish within 3 seconds. Your client must make the server identity check described above before writing the first byte. If your platform or language cannot do that, run the CLI instead.

Each CLI attempt receives a fresh opaque `ddc_run_` credential made from 32 random bytes. The gateway retains only SHA-256 hashes, with at most 8,192 live entries. A credential becomes usable only after the gateway has bound it to the spawned process. A guard revokes it when the attempt ends, errors or is cancelled; expiry is the run timeout plus 60 seconds, capped at six hours, and a gateway restart invalidates every credential. In local API tool loops, each stdio MCP server the loop starts gets its own credential, bound to that server process. `mcp-proxy` forwards the credential, `DUDUCLAW_IDENTITY_ENDPOINT` and `DUDUCLAW_IDENTITY_SERVER` to local third-party servers while still stripping the older identity variables, including `DUDUCLAW_IDENTITY_URL`. Codex requires MCP `env_vars` support; the credential travels through the process environment, never its command line. Actual Codex shell/MCP propagation still needs live validation for the installed version.

**Limits.** The process-tree check prevents forged claims and stops an unrelated process from using a credential it copied. It does not cover these cases:

- On Windows, a program running under the same account that deliberately sets its parent process at creation (`PROC_THREAD_ATTRIBUTE_PARENT_PROCESS`) can pass the ancestry check. The named pipe also reports only a PID, so a short window remains between reading it and opening that process.
- On macOS, and on Linux kernels without `SO_PEERPIDFD` (added in 6.5), the kernel reports a PID rather than a pinned process handle, which leaves a narrow PID-reuse window.
- Code injected into a legitimate descendant, a legitimate descendant that relays requests for another process, and a connected socket or pipe handle passed to another process all act as that descendant.

Keep local tools trusted; use OS isolation for hostile workloads. In protected mode no run credential is issued: it keeps its own capability records, and `duduclaw identity whoami` reports that protected mode uses its own capability identity. The existing `DUDUCLAW_AGENT_TOKEN` scheme and DuDuClaw MCP server identity verification are unchanged.


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
