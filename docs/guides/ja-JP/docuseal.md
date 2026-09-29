# DocuSeal——文書署名ワークフロー

[DocuSeal](https://github.com/docusealco/docuseal)はオープンソースのDocuSign代替(クラウドまたはセルフホスト)です。DuDuClawはDocuSeal**公式のMCPサーバー**を`[[mcp.external]]`でマウントして接続します——DuDuClaw側のラッパーは介在しません。

> **2026-09の変更。** DuDuClawはかつてファーストパーティのstdioラッパーcrate(`duduclaw-docuseal-mcp`、10ツール)を同梱していました。`scripts/release.sh`でビルドされたことが一度もなく、利用者が自分で`cargo build`する必要があり、またDocuSealは2026-03に公式MCPサーバーを提供しています。このラッパーは削除されました。以下の公式サーバーを使ってください。

## 公式サーバーのマウント

DocuSealセルフホストのMCPエンドポイントは`https://<host>/mcp`です。インスタンスの**Settings → MCP Server**でbearerトークンを生成し、[MCP Bridge](../mcp-bridge.md)経由でマウントします:

```toml
[[mcp.external]]
name = "docuseal"
url = "https://sign.example.com/mcp"
headers = { Authorization = "Bearer secret://local/docuseal_mcp_token" }
allowed_tools = [
  "search_templates", "load_template", "create_template",
  "send_document", "search_documents",
]
```

署名依頼の送信は外向きで半不可逆な操作です——送信ツールを`[capabilities] approval_required_tools`に入れてHITL承認を通すことを検討してください。

## 公式サーバーの守備範囲

5つのツール: テンプレート検索、テンプレート読み込み、テンプレート作成、文書送信、文書検索。**セルフホスト専用**です——クラウドテナント(`api.docuseal.com` / `.eu`)にMCPエンドポイントはありません。

DocuSealクラウドを使っている場合、あるいはより広いREST面(アーカイブ、再送信、prefill更新、署名済みファイルのダウンロードURL)が必要な場合は、`X-Auth-Token`ヘッダーを付けて[DocuSeal REST API](https://www.docuseal.com/docs/api)を直接呼んでください——自作の小さなMCPサーバー経由でも、エージェントのHTTPツール経由でも構いません。

## 署名完了 → 自動通知(webhook)

DocuSealのwebhookはUIでしか設定できません(クラウド: Console → Webhooks、セルフホスト: Settings → Webhooks)——APIでは設定できません。`form.completed` / `submission.completed`を自動化の入口に向ければ、「完了時にチャンネルへ通知/タスクを作成」というautopilotルールに繋げられます。ペイロードの外枠は`{"event_type", "timestamp", "data"}`、署名ヘッダーは`X-Docuseal-Signature`(`<unix_ts>.<hex_hmac>`、`<ts>.<raw_body>`に対するHMAC-SHA256、±300秒の許容)です。
