# Custom MCP tool development guide

> DuDuClawのMCP Serverに新しいツールを追加する方法
> 対象バージョン：v0.12.0+

---


## 呼び出し元の身分を取得・検証する（Getting and verifying the caller identity）

An AI cannot claim another identity by changing environment variables, parameters, or its working directory. On installs where all AI employees run as the same operating-system user, a process that can read other processes' environments could borrow another run's credential while that run is active; the credential stops working when the run ends. Native Linux protected mode isolates employees as separate users. In protected mode no run credential is issued: it keeps its own capability records, and `duduclaw identity whoami` reports that protected mode uses its own capability identity.

この節は gateway が管理する実行を対象とします。単独の `duduclaw agent` ターミナルセッションはこの gateway registry に未接続です。インストール・バージョン・認証状態の確認と、任意の `/model` メニュー確認も身分付きの作業ターンではありません。

AI は環境変数、引数、作業ディレクトリを変更して別の身分を名乗ることはできません。すべての AI 従業員が同じ OS ユーザーで動く環境では、他のプロセスの環境変数を読めるプロセスが、別の実行中の資格情報を借用できる場合があります。その実行が終了すると資格情報は無効になります。ネイティブ Linux の保護モードは従業員を別々の OS ユーザーに隔離します。保護モードでは run credential を発行せず、独自の capability records を使います。`duduclaw identity whoami` は、このモードが独自の capability identity を使うことを明示します。

DuDuClaw が起動したローカルツールは `DUDUCLAW_RUN_CREDENTIAL` を読み、ツールを呼び出すたびにローカル gateway で検証してください。下記の CLI も利用できます。cwd、表示名、ツール引数、`DUDUCLAW_ON_BEHALF_OF` は証明になりません。**照会はローカルのみ。資格情報を転送しないでください。** URL、argv、ログ、ツール結果、会話記録にも含めないでください。非機密の `DUDUCLAW_IDENTITY_URL` は dashboard とは別の listener を指し、`127.0.0.1` の一時ポートだけを使用します。起動できるのは `gateway.lock` を保持するプロセスだけです。CLI はこの変数を優先し、未設定なら `<gateway home>/run/identity-url`（所有者のみ、Unix では 0600）を読み、非 loopback URL を拒否します。URL は接続先のヒントであり、身分証明ではありません。

```sh
duduclaw identity whoami --json

```

有効な応答には `active: true`、`agent_id`、`principal`、`session_id`、`turn_id`、`task_id`、`runtime`、`issued_at`、`expires_at`（Unix 秒）が含まれます。人間は `{"type":"human","user_id":"…"}`、cron・heartbeat・task は `{"type":"background","kind":"…"}` です。未確認の依頼者は background／unattributed で、管理者にはなりません。session と task は null の場合があります。モデルの申告ではなく gateway の登録情報です。検証結果をツール独自の権限規則に照合してください。身分確認だけでは権限は付与されません。

形式不正、不明、期限切れ、失効済みはすべて HTTP 200 と `{"active":false}` を返します。非 loopback、接続元不明、レート制限超過も同じ拒否応答です。有効な資格情報ごとに毎分 600 回の独立したバケットを持ち、registry の最大 8,192 件に収まります。形式不正・不明な要求は別の共通バケットを使います。`Forwarded`、`X-Forwarded-For`、`X-Forwarded-Host`、`X-Real-IP`、`Via`、`Origin` がある要求も同じ inactive 応答です。リクエストは最大 1 KiB です。資格情報がない場合、無効な場合、gateway に接続できない場合は CLI が非ゼロで終了します。検証失敗時は拒否し、有効という結果を次の呼び出しに持ち越さないでください。

CLI の各試行に、32 ランダムバイトから生成した新しい `ddc_run_` 資格情報を発行します。gateway は SHA-256 ハッシュのみを最大 8,192 件保持します。終了・エラー・キャンセルで guard が失効させ、有効期限は実行タイムアウト＋60 秒、最長 6 時間です。gateway 再起動ですべて無効になります。ローカル API ツールループの lease は tool registry の終了まで保持します。`mcp-proxy` は新しい変数をローカルのサードパーティーサーバーへ渡し、従来の身分変数は引き続き除去します。Codex は MCP `env_vars` 対応が必要で、資格情報を argv に含めません。各バージョンの shell／MCP 継承は実機検証が必要です。

この保証は申告の偽造を防ぐもので、同じ OS ユーザーのプロセスによる有効な bearer 資格情報の窃取は防げません。ローカルツールは信頼できるものを使い、敵対的な処理には OS 隔離を使用してください。保護モードでは run credential を発行せず、独自の capability records を使います。`duduclaw identity whoami` は、このモードが独自の capability identity を使うことを明示します。既存の `DUDUCLAW_AGENT_TOKEN` と DuDuClaw MCP server の身分検証は変更しません。


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
| （なし） | 単独ツール | `emergency_stop`, `tool_approve` |
