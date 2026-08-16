# Custom MCP tool development guide

> DuDuClawのMCP Serverに新しいツールを追加する方法
> 対象バージョン：v0.12.0+

---

## Overview

DuDuClawはstdin/stdout上のJSON-RPC 2.0経由で、200以上のMCPツールを公開しています（v1.56時点で206個）。本ガイドでは、Claude Codeと連携するカスタムツールを追加する方法を説明します。

## Architecture

```
Claude Code (client)
    ↕  JSON-RPC 2.0 (stdin/stdout)
DuDuClaw MCP Server (crates/duduclaw-cli/src/mcp.rs)
    ↕  Rust function calls
Tool handlers (gateway, agent, memory, inference, etc.)
```

## Step 1: Define the tool

`crates/duduclaw-cli/src/mcp.rs`の`TOOLS`配列に新しい`ToolDef`エントリを追加します。

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

- ツール名は`snake_case`を使う
- 関連するツールは共通の接頭辞でグループ化する：`odoo_*`、`model_*`、`cost_*`
- 名前は簡潔かつ説明的に保つ

### Parameter rules

- `required: true` — Claude Codeが必ずこのパラメータを渡さなければならない
- `required: false` — 任意項目。tool handler側でデフォルト値を用意する
- すべてのパラメータはJSON値（`serde_json::Value`）として渡される

## Step 2: Implement the handler

`handle_tool_call()`関数にmatchアームを追加します。

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

エラーはpanicではなく、構造化されたJSONとして返します。

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

すべてのtool handlerはTokioの非同期コンテキスト上で動きます。I/Oには`.await`を使ってください。

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

同じファイル内、または専用のテストモジュールにテストを追加します。

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

続いて、Claude CodeがDuDuClawをMCP serverとして使うよう設定します。

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

`crates/duduclaw-cli/src/mcp.rs`のツール一覧コメントにそのツールを追記し、重要な機能を表す場合は`docs/CLAUDE.md`も更新してください。

## Patterns & best practices

### Accessing agent state

多くのツールはagentの設定や状態へのアクセスを必要とします。

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

外部APIを呼ぶツールにはレート制限をかけてください。

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

新しいツールをマージする前に確認してください。

- [ ] すべてのパラメータに入力検証があるか
- [ ] 秘密情報がハードコードされていないか
- [ ] 外部API呼び出しにレート制限があるか
- [ ] URLパラメータにSSRF対策があるか（`web_fetch`のパターンを使う）
- [ ] 機微な操作に監査ログが残るか
- [ ] Pro/Enterprise限定のツールであればfeature gateのチェックがあるか

`agent.toml [capabilities] allowed_tools` / `denied_tools`は、もはやツールごとに個別チェックする必要はありません。stdio、HTTP/SSE、そしてopenai-compat tool-loop内部のMCP clientを含むすべての呼び出しは、共通の`McpDispatcher::dispatch_tool_call`という単一のチョークポイント（`mcp_dispatch.rs`）を経由します。ここで呼び出し元の`[capabilities]`許可／拒否リストがツールのベース名と照合され（`mcp__<server>__`のような修飾子は照合前に取り除かれ、`denied_tools`は常に`allowed_tools`より優先されます）、通過してはじめてあなたのhandlerが呼ばれます。`handle_tool_call()`に新しく登録したツールは自動的にこの保護下に入ります。これは実在した抜け道を塞ぐものでした。この強制がチョークポイントに移される前は、`allowed_tools` / `denied_tools`はClaude CLI spawnの`--allowedTools` / `--disallowedTools`フラグにしか届いておらず、MCP serverと直接話す呼び出し元（CLI spawnを迂回する経路）はこれらの制限を一切受けていませんでした。

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

新しいツールを追加する際は、既存のカテゴリ命名規則に従ってください。

| 接頭辞 | カテゴリ | 例 |
|--------|----------|---------|
| `send_*` | メッセージ送信 | `send_message`, `send_photo`, `send_sticker` |
| `web_*` | Web／検索 | `web_search`, `web_fetch_cached`, `web_extract` |
| `agent_*` | Agent管理 | `agent_status`, `agent_update`, `agent_remove` |
| `memory_*` | メモリ操作 | `memory_search`, `memory_store` |
| `model_*` | モデル管理 | `model_list`, `model_load`, `model_unload` |
| `inference_*` | 推論制御 | `inference_status`, `inference_mode` |
| `llamafile_*` | Llamafileライフサイクル | `llamafile_start`, `llamafile_stop` |
| `cost_*` | コストテレメトリ | `cost_summary`, `cost_agents`, `cost_recent` |
| `odoo_*` | Odoo ERP | `odoo_crm_leads`, `odoo_sale_orders` |
| `skill_*` | Skillエコシステム | `skill_search`, `skill_list` |
| （なし） | 単独ツール | `emergency_stop`, `tool_approve`, `schedule_task` |
