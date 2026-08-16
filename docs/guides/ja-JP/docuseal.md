# DocuSeal——文書署名ワークフロー

[DocuSeal](https://github.com/docusealco/docuseal)はオープンソースのDocuSign代替(クラウドまたはセルフホスト)です。DuDuClawは`duduclaw-docuseal-mcp`を提供します——オープンソースのMCP stdioラッパーで、エージェントが「契約書生成→署名依頼送信→ステータス確認→署名済みファイル取得」という一連の流れを実行できます。

## 2つの経路、どちらを選ぶか

| 経路 | 適用 | 認証 |
|---|---|---|
| **`duduclaw-docuseal-mcp`(本ラッパー)** | クラウド(api.docuseal.com / .eu)**と**セルフホストの両方に対応。ツール面がより充実(アーカイブ、再送信、prefill更新、署名済みファイルURL) | `X-Auth-Token` APIキー |
| **DocuSeal公式内蔵MCP**(2026-03以降) | セルフホストのみ。5つのツール(search/load/create template、send、search documents) | インスタンスのSettings → MCP Serverで生成されるBearerトークン、`url = "https://<host>/mcp"`を[MCP Bridge](../mcp-bridge.md)経由で直接マウント |

## ラッパーの10個のツール

`docuseal_list_templates`、`docuseal_get_template`、
`docuseal_create_template_from_pdf`(base64またはURL。PDF内に
`{{フィールド;role=Signer1;type=signature}}`形式のtext tagsを入れると
自動でフィールドが配置される)、
`docuseal_create_submission`(署名依頼を送信し、各署名者の署名リンク
`embed_src`を返す)、
`docuseal_get_submission`(ステータス+イベント+`audit_log_url`)、
`docuseal_list_submissions`、`docuseal_archive_submission`、
`docuseal_get_submission_documents`(完了後の署名済みファイルのダウンロードURL)、
`docuseal_resend_submitter_email`、`docuseal_update_submitter`(prefill/連絡先更新)。

## 設定

環境変数:

| 変数 | 説明 |
|---|---|
| `DOCUSEAL_API_KEY` | 必須。クラウドは<https://console.docuseal.com/api>で取得、セルフホストはインスタンスのAPI設定で取得 |
| `DOCUSEAL_BASE_URL` | 任意。デフォルトは`https://api.docuseal.com`。EUクラウドは`https://api.docuseal.eu`、セルフホストは`https://<host>/api` |

`agent.toml`でのマウント(stdio):

```toml
[[mcp.external]]
name = "docuseal"
command = "duduclaw-docuseal-mcp"
env = { DOCUSEAL_API_KEY = "secret://local/docuseal_api_key" }
# self-hosted の場合は追加: DOCUSEAL_BASE_URL = "https://sign.example.com/api"
allowed_tools = [
  "docuseal_list_templates", "docuseal_get_template",
  "docuseal_create_submission", "docuseal_get_submission",
  "docuseal_get_submission_documents", "docuseal_resend_submitter_email",
]
```

送信/アーカイブは対外的かつ半不可逆なアクションです——`docuseal_create_submission`と`docuseal_archive_submission`を`[capabilities] approval_required_tools`に入れ、HITL承認を通すことを推奨します。

## 署名完了→自動通知(webhook)

DocuSealのwebhookはUIでのみ設定可能で(クラウド:Console → Webhooks、セルフホスト:Settings → Webhooks)、APIから代わりに設定することはできません。`form.completed` / `submission.completed`を自動化のエントリポイントに向けておけば、autopilotルールで「完了したらチャネルに通知/タスクを作成」を連携できます。ペイロードの外殻は`{"event_type", "timestamp", "data"}`。署名検証ヘッダーは`X-Docuseal-Signature`(`<unix_ts>.<hex_hmac>`、HMAC-SHA256を`<ts>.<raw_body>`に対して計算、許容誤差±300秒)。

## ローカル検証

```sh
cargo build -p duduclaw-docuseal-mcp
printf '%s\n%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  | DOCUSEAL_API_KEY=test ./target/debug/duduclaw-docuseal-mcp
```

2行目の応答は10個の`docuseal_*`ツールを列挙するはずです。実際のAPI呼び出し(`tools/call`)には有効なキーが必要です。HTTP層のエラーはサーバーをクラッシュさせず、`isError: true`としてエージェントに返されます。
