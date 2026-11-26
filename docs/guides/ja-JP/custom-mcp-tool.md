# Custom MCP tool development guide

> DuDuClawのMCP Serverに新しいツールを追加する方法
> 対象バージョン：v0.12.0+

---


## 呼び出し元の身分を取得・検証する（Getting and verifying the caller identity）

この節は gateway が管理する実行を対象とします。単独の `duduclaw agent` ターミナルセッションはこの gateway registry に未接続です。インストール・バージョン・認証状態の確認と、任意の `/model` メニュー確認も身分付きの作業ターンではありません。

AI は環境変数、引数、作業ディレクトリを変更して別の身分を名乗ることはできません。gateway が管理する実行ごとに短期間有効な資格情報を発行し、gateway が照会に応答するのは、その実行のために起動した CLI プロセスか、その子孫プロセスからの照会だけです。ネイティブ Linux の保護モードは従業員を別々の OS ユーザーに隔離します。保護モードでは run credential を発行せず、独自の capability records を使います。`duduclaw identity whoami` は、このモードが独自の capability identity を使うことを明示します。

DuDuClaw が起動したローカルツールは `DUDUCLAW_RUN_CREDENTIAL` を読み、ツールを呼び出すたびにローカル gateway で検証してください。下記の CLI も利用できます。cwd、表示名、ツール引数、`DUDUCLAW_ON_BEHALF_OF` は証明になりません。**照会はローカルのみ。資格情報を転送しないでください。** URL、argv、ログ、ツール結果、会話記録にも含めないでください。

gateway は機密でない変数を 2 つ設定します。`DUDUCLAW_IDENTITY_ENDPOINT` はバージョン付きのローカル IPC エンドポイントです。Unix では `unix:v1:<絶対パス>` で、ソケットは `<gateway home>/run/` 配下の非公開ディレクトリ（所有者のみ、権限 0700）に置きます。Windows では `pipe:v1:\\.\pipe\duduclaw-identity-<hex>` で、この名前付きパイプの DACL は現在のユーザーだけを許可し、リモートクライアントを拒否します。`DUDUCLAW_IDENTITY_SERVER`（`v1:<pid>:<起動時刻>`）は gateway のプロセス身分です。クライアントは資格情報を 1 バイトでも書き込む前に、接続先がどのプロセスかを OS に問い合わせ、PID とプロセス起動時刻の両方が一致しなければ中止します。エンドポイントを起動するのは `gateway.lock` を保持する gateway プロセスだけです。`<gateway home>/run/identity-endpoint` は運用者の診断用で、CLI はこのファイルからエンドポイントやサーバー身分を取得しません。以前の `127.0.0.1` の HTTP listener と `DUDUCLAW_IDENTITY_URL` は廃止され、フォールバックもありません。gateway は起動のたびにこの変数を除去し、まだ設定されている実行では CLI が inactive として扱います。

**照会できるプロセス。** gateway は接続ごとに呼び出し元の PID をカーネルから取得します（Unix ソケットの peer credentials、Windows では名前付きパイプのクライアント PID）。要求の内容は使いません。続いて呼び出し元から親をたどり、この実行のために起動した CLI プロセスまで、途中の各プロセスの PID と起動時刻を確認します。親が子より後に起動していることはあり得ず、親子関係は最大 64 段までです。途中のすべてのプロセスと、この実行の CLI プロセス自体がまだ動作している必要があります。このツリー外の呼び出し元は、正しいトークンを提示しても拒否されます。具体的には次のとおりです。

- この実行の CLI が直接、または `mcp-proxy` 経由で起動したカスタム MCP サーバー、hook、スクリプトは子孫プロセスなので身分を検証できます。ツールが子プロセスとして実行する `duduclaw identity whoami` も同様です。
- 実行の外で起動したデーモンやサービス（launchd や systemd のユニット、自分で先に起動したサーバー、別の実行に属するプロセス）は子孫ではないため、資格情報を環境変数にコピーしても `{"active":false}` を受け取ります。この実行が起動したプロセスの中で検証し、そのようなサービスには資格情報ではなく検証結果だけを渡してください。
- 親から切り離されて親が付け替えられたプロセス（二重 fork 後に init や launchd に引き取られたデーモンなど）はたどれなくなり、拒否されます。この実行の CLI プロセスが終了すると、すべての子孫プロセスで資格情報は使えなくなります。

```sh
duduclaw identity whoami --json

```

有効な応答には `active: true`、`agent_id`、`principal`、`session_id`、`turn_id`、`task_id`、`runtime`、`issued_at`、`expires_at`（Unix 秒）が含まれます。人間は `{"type":"human","user_id":"…"}`、cron・heartbeat・task は `{"type":"background","kind":"…"}` です。未確認の依頼者は background／unattributed で、管理者にはなりません。session と task は null の場合があります。モデルの申告ではなく gateway の登録情報です。検証結果をツール独自の権限規則に照合してください。身分確認だけでは権限は付与されません。

形式不正、不明、期限切れ、失効済みの資格情報、実行のプロセスツリー外の呼び出し元、接続元情報の欠落、レート制限超過は、すべて `{"active":false}` だけを返します。有効な資格情報ごとに毎分 600 回の独立したバケットを持ち、registry の最大 8,192 件に収まります。形式不正・不明な要求は別の共通バケットを使います。資格情報がない・無効、`DUDUCLAW_IDENTITY_ENDPOINT` または `DUDUCLAW_IDENTITY_SERVER` がない・形式不正、サーバー身分の不一致、gateway に接続できない場合は CLI が非ゼロで終了します。検証失敗時は拒否し、有効という結果を次の呼び出しに持ち越さないでください。

CLI を実行できないツール向けのプロトコルは、1 接続につき 1 要求で、HTTP、リダイレクト、プロキシ、持続接続には対応しません。4 バイトのビッグエンディアン長に続けて JSON オブジェクト 1 つ `{"version":1,"operation":"introspect","credential":"ddc_run_…"}`（最大 4 KiB）を送り、同じ形式の JSON 応答（最大 16 KiB）を 1 つ読み取ってから、受信確認として 1 バイトの `0x01` を書き込みます。やり取り全体は 3 秒以内に終える必要があります。最初のバイトを書き込む前に、上記のサーバー身分確認を必ず行ってください。使用するプラットフォームや言語でそれができない場合は CLI を使ってください。

CLI の各試行に、32 ランダムバイトから生成した新しい `ddc_run_` 資格情報を発行します。gateway は SHA-256 ハッシュのみを最大 8,192 件保持します。資格情報は、gateway が起動したプロセスに結び付けた後で初めて使えるようになります。終了・エラー・キャンセルで guard が失効させ、有効期限は実行タイムアウト＋60 秒、最長 6 時間です。gateway 再起動ですべて無効になります。ローカル API ツールループでは、起動する stdio MCP サーバーごとに、そのサーバープロセスに結び付いた資格情報を発行します。`mcp-proxy` は資格情報、`DUDUCLAW_IDENTITY_ENDPOINT`、`DUDUCLAW_IDENTITY_SERVER` をローカルのサードパーティーサーバーへ渡し、`DUDUCLAW_IDENTITY_URL` を含む従来の身分変数は引き続き除去します。Codex は MCP `env_vars` 対応が必要で、資格情報を argv に含めません。各バージョンの shell／MCP 継承は実機検証が必要です。

**制限。** プロセスツリーの確認は申告の偽造を防ぎ、無関係なプロセスがコピーした資格情報を使うことも防ぎます。次の場合は保証の対象外です。

- Windows では、同じアカウントで動くプログラムがプロセス作成時に意図的に親プロセスを指定すると（`PROC_THREAD_ATTRIBUTE_PARENT_PROCESS`）、祖先チェックを通過できます。名前付きパイプは PID しか返さないため、PID の取得からそのプロセスを開くまでの短い隙間も残ります。
- macOS と、`SO_PEERPIDFD`（6.5 で追加）のない Linux カーネルでは、カーネルが固定されたプロセスハンドルではなく PID を返すため、PID 再利用のわずかな隙間が残ります。
- 正規の子孫プロセスに注入されたコード、他のプロセスの要求を中継する正規の子孫プロセス、他のプロセスに渡された接続済みのソケットやパイプのハンドルは、いずれもその子孫プロセスとして扱われます。

ローカルツールは信頼できるものを使い、敵対的な処理には OS 隔離を使用してください。保護モードでは run credential を発行せず、独自の capability records を使います。`duduclaw identity whoami` は、このモードが独自の capability identity を使うことを明示します。既存の `DUDUCLAW_AGENT_TOKEN` と DuDuClaw MCP server の身分検証は変更しません。


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
