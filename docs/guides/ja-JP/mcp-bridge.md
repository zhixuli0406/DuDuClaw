# MCPブリッジ — 外部MCPサーバーのマウント

DuDuClawは、サードパーティの [Model Context Protocol](https://modelcontextprotocol.io) サーバーを、内蔵MCPサーバーと並べてマウントできます。これにより、Rustで手書きのコネクタを書かなくても、エージェントのツールループが外部サーバーのツールを獲得します。Plane、Chatwoot、Invoice Ninja、Gmail/Calendar、WooCommerce、その他あらゆるMCPサーバーをエージェントに接続する方法がこれです。

## MCPブリッジかネイティブコネクタか

- **MCPブリッジ**（このページ）：SaaS側（またはコミュニティ）がすでにMCPサーバーを提供している場合。設定だけでマウントでき、コードは不要です。
- **ネイティブコネクタ**（例：`duduclaw-odoo`、`duduclaw-erpnext`）：使えるMCPサーバーが存在しない場合、または汎用マウントでは得られない深いクレデンシャル分離／エディションゲーティング／監査帰属が必要な場合。

## 設定

エージェントの `agent.toml` に `[[mcp.external]]` テーブルを1つ以上追加します。

```toml
[[mcp.external]]
name = "chatwoot"
command = "npx"
args = ["-y", "@chatwoot/mcp-server-chatwoot"]
enabled = true                       # 省略可、デフォルトtrue
# envの値：リテラル文字列、gatewayプロセスの環境変数から取る `env://VAR`、
# または起動時に設定済みのシークレットマネージャーから取る
# `secret://<backend>/<name>`（シークレットはagent.tomlに書かない）。
env = { CHATWOOT_BASE_URL = "https://app.chatwoot.com", CHATWOOT_API_TOKEN = "secret://vault/chatwoot_token" }
# ツールの可視性（どちらも省略可）：
allowed_tools = ["chatwoot_list_conversations", "chatwoot_get_conversation"]  # allowlist = deny-by-default
denied_tools  = ["chatwoot_delete_conversation"]                             # 常に除外
```

**MCP Streamable HTTP**（ローカルプロセス不要）を話すリモートサーバーは、`command` の代わりに `url` でマウントします。DuDuClawが既知のベンダーなら **`preset`** を使うと、エンドポイントとクレデンシャルの取得元があらかじめ埋め込まれます（エンドポイントURLはDuDuClaw側で保持しているので、ベンダー名の変更は設定移行ではなく1行のコード修正で済みます）：

```toml
[[mcp.external]]
preset = "google:gmail"    # gmail|calendar|drive|docs|sheets|slides|chat
allowed_tools = ["search_threads", "get_thread", "create_draft"]
```

`preset = "google:<svc>"` は、Google公式のGoogle Workspace MCPエンドポイントと `bearer_token = "oauth://google"`（dashboardで連携済みのGoogleアカウント、自動更新）に展開されます。

> **Google Workspaceについてはネイティブツールを優先してください。** 8つのサービス（Gmail／Calendar／Sheets／Drive／Docs／Slides／Forms／Tasks）はすべてGA APIベースのネイティブMCPツールとして提供されています。詳細は [google-workspace.md](google-workspace.md) を参照してください。公式MCPサーバーは現時点でDeveloper Preview限定であり、利用規約により自社ドメイン外のユーザーへPre-GA APIを公開することが禁じられています。そのため、これは出荷可能な経路ではなく、上級者向けのセルフホスト・オプトインという位置づけです（トレードオフの詳細は [google-mcp.md](google-mcp.md) を参照）。

手動で書き下すとこうなります。presetを使わずに何かをマウントする場合も同じ書き方です（例：セルフホストのDocuSealインスタンスが内蔵する `/mcp` エンドポイント）：

```toml
[[mcp.external]]
name = "gmail"
url = "https://gmailmcp.googleapis.com/mcp/v1"
# bearer_token：リテラル文字列、env://VAR、secret://<backend>/<name>、
# または oauth://google（dashboardで連携済みのGoogleアカウントトークンを再利用、
# 自動更新）。`Authorization: Bearer <token>` として送信されます。
bearer_token = "oauth://google"
# headers = { X-Custom = "env://MY_HEADER" }   # 省略可、追加のヘッダー
allowed_tools = ["search_threads", "get_thread", "create_draft"]
```

フィールド一覧：

| フィールド | 必須 | 意味 |
|---|---|---|
| `name` | 推奨 | ログ用のラベル（`preset` は自身でラベルを持つ） |
| `preset` | 不要 | 組み込みのベンダー略記（`google:gmail`、`google:calendar`、`google:drive`、`google:docs`、`google:sheets`、`google:slides`、`google:chat`）。`url` とデフォルトの `bearer_token` を提供する。未知のpresetの場合はサーバーがスキップされる。`preset` と `url` を同時に指定すると曖昧なためスキップされる |
| `command` | command/url/presetのいずれか | stdioトランスポート：起動する実行ファイル（`npx`、`node`、`python`、絶対パスなど） |
| `url` | command/url/presetのいずれか | Streamable-HTTPトランスポート：リモートMCPエンドポイント（`https://` のみ。`command`/`url` を両方または両方とも指定しないエントリはスキップされる） |
| `args` | 不要 | 引数ベクター（stdioのみ） |
| `env` | 不要 | 子プロセスの環境変数（stdioのみ）。`env://VAR` はgatewayの環境変数から取得。`secret://<backend>/<name>` はシークレットマネージャーから取得（後述）。`env://`／`secret://` のクレデンシャルが欠落・解決不能な場合は**サーバー全体が無効化される**（fail-safe：トークンのないサーバーは誤動作しかねないため） |
| `bearer_token` | 不要 | HTTP認証：リテラル文字列、`env://VAR`、`secret://…`、または `oauth://google`。`Authorization: Bearer …` として送信。解決できない場合はサーバーがスキップされる |
| `headers` | 不要 | 追加のHTTPリクエストヘッダー。値は `env://` と `secret://` に対応 |
| `enabled` | 不要（デフォルトtrue） | falseにすると設定は残したままマウントしない |
| `allowed_tools` | 不要 | 設定するとこれらのツールのみが公開される（deny-by-default） |
| `denied_tools` | 不要 | allowlistに含まれていても常に除外される |

## セマンティクスと安全性

- **内蔵のduduclaw MCPサーバーは常にclient 0** です。ツール名が衝突した場合は内蔵ツールが優先され、外部側の重複はログを残して破棄されます。
- 各外部サーバーは独立して起動します。どれかが**接続に失敗した場合はそのサーバーだけスキップ**され、内蔵サーバーや他の外部サーバーはそのまま動作します。結合後の `tools/list` 自体が失敗した場合は、レジストリは**内蔵のみ**に縮退し、全ツールを失うことはありません。
- `allowed_tools` は **deny-by-default** です。allowlistを設定すると、リストにないツールは非表示になります。`denied_tools` と組み合わせて、特に危険なツールを個別にブロックできます。
- 書き込み系／不可逆なツール（課金、削除など）は、エージェントの `[capabilities] approval_required_tools` にも列挙し、HITLの `ApprovalBroker` を経由させてください。MCPブリッジが制御するのは*可視性*、承認フローが制御するのは*実行*です。

## ライブ検証の手順

設定パーサーとツールフィルターはユニットテストでカバーされています
（`crates/duduclaw-gateway/src/mcp_external.rs`、
`crates/duduclaw-llm/src/mcp_client.rs`）。実際のマウントをエンドツーエンドで検証するには、接続可能なMCPサーバーが必要です。

1. ローカルで動かせるサーバーを選びます。例えばリファレンス実装のeverything-server：
   ```bash
   # スクラッチディレクトリで、stdio経由でMCPを話すか確認
   npx -y @modelcontextprotocol/server-everything
   ```
2. テスト用エージェントの `agent.toml` に追加します：
   ```toml
   [[mcp.external]]
   name = "everything"
   command = "npx"
   args = ["-y", "@modelcontextprotocol/server-everything"]
   allowed_tools = ["echo"]   # allowlistが他を隠すことを確認する
   ```
3. gatewayを起動し、`echo` ツールを使うようなメッセージをエージェントに送ります。ログで以下を確認します：
   - `server=everything` を伴う `external MCP server mounted`
   - エージェントが `echo` は呼べるが、そのサーバーの他のツールは呼べないこと（allowlist）。
4. 一時的に `env://` クレデンシャルを未設定の変数に向けて再起動し、
   `external MCP env credential unresolved … skipping server` というログとともに
   サーバーがスキップされることを確認します。

期待される結果：エージェントはallowlistに載っている外部ツールだけを獲得し、壊れた外部サーバーが内蔵のツール面を道連れにすることはありません。

## `secret://` によるクレデンシャル解決

`env` の値は、リテラル文字列や `env://` によるプロセス環境変数の取得だけでなく、シークレットマネージャーを参照することもできます。起動時、DuDuClawは `secret://<backend>/<name>` を `~/.duduclaw/config.toml` の `[secret_manager]` 設定と照合して解決します。解決できない参照はサーバー全体を除外します（fail-safe、`env://` が欠落した場合と同じ挙動です）。

対応バックエンド：`local`（AESストア）、`vault`（HashiCorp Vault KV v2）、`env`、`onepassword`（1Password Connect）、`infisical`。例：

```toml
# config.toml
[secret_manager]
backend = "vault"
vault_addr  = "https://vault.internal:8200"
vault_token_enc = "…"          # keyfileで暗号化済み、本番では平文にしない

# agent.toml
[[mcp.external]]
name = "chatwoot"
command = "npx"
args = ["-y", "@chatwoot/mcp-server-chatwoot"]
env = { CHATWOOT_BASE_URL = "https://app.chatwoot.com", CHATWOOT_API_TOKEN = "secret://vault/chatwoot_token" }
```

`[secret_manager]` の全フィールド（1Password／Infisicalを含む）については、`crates/duduclaw-security/src/secret_manager/mod.rs` のモジュールドキュメントを参照してください。

## レシピ — よく使われるSaaSサーバー

各レシピは `agent.toml` の設定ブロックと、用意すべきクレデンシャルのセットです。マウントしてエージェントを再起動し、[ライブ検証の手順](#ライブ検証の手順)に従って確認してください。
**書き込み系／不可逆なツールには⚠が付いています。エージェントの `[capabilities] approval_required_tools` に列挙し、HITLブローカーを経由させてください。**

> ステータス：以下はすべて **PENDING-LIVE**（実機検証待ち）です。設定の形とパース処理はテスト済みですが、実際のエンドツーエンドのマウントには対応するSaaSアカウントが必要です。サーバー名は2026-07時点のエコシステムを反映しています。使用前にパッケージ名／エンドポイントを確認してください。

### Gmail／Google Calendar（Google公式リモートMCP）

```toml
[[mcp.external]]
name = "gmail"
command = "npx"
args = ["-y", "@google/gmail-mcp"]     # 現行の公式パッケージ名を確認すること
env = { GOOGLE_OAUTH_TOKEN = "secret://vault/google_oauth" }
allowed_tools = ["gmail_search", "gmail_get_thread", "gmail_create_draft"]  # 読み取りと下書きのみ
denied_tools  = ["gmail_send"]         # ⚠ 送信は自動ではなく承認フロー経由にする
```
用意するもの：Google CloudのOAuthアプリ。OAuthフローを実行してトークンを発行します。
`gmail_send` ⚠ → `approval_required_tools` へ。

### Plane（公式 `plane-mcp-server`、成熟）

```toml
[[mcp.external]]
name = "plane"
command = "npx"
args = ["-y", "@makeplane/plane-mcp-server"]
env = { PLANE_API_KEY = "secret://vault/plane_api_key", PLANE_WORKSPACE_SLUG = "my-workspace" }
allowed_tools = ["plane_list_issues", "plane_get_issue", "plane_create_issue"]
denied_tools  = ["plane_delete_issue"]  # ⚠
```
任意：一方向の同期ワーカーでPlaneのissueをTask Boardに取り込めます（IMPL-PLAN §E参照）。用意するもの：Plane APIキーとワークスペースのslug。

### Invoice Ninja（コミュニティ製 `Fuciuss/invoice-ninja-mcp`）

```toml
[[mcp.external]]
name = "invoice-ninja"
command = "npx"
args = ["-y", "invoice-ninja-mcp"]      # パッケージ名を確認すること
env = { INVOICE_NINJA_URL = "https://invoicing.example.com", INVOICE_NINJA_TOKEN = "secret://vault/invoiceninja_token" }
allowed_tools = ["in_list_invoices", "in_get_invoice", "in_create_invoice", "in_record_payment"]
```
**お金に関わる操作は不可逆です。** 書き込み系のツール（`in_create_invoice`、`in_record_payment` など）はすべて `approval_required_tools` に入れてください。用意するもの：Invoice Ninja APIトークン。

### Chatwoot（公式 `@chatwoot/mcp-server-chatwoot`）

```toml
[[mcp.external]]
name = "chatwoot"
command = "npx"
args = ["-y", "@chatwoot/mcp-server-chatwoot"]
env = { CHATWOOT_BASE_URL = "https://app.chatwoot.com", CHATWOOT_API_TOKEN = "secret://vault/chatwoot_token" }
allowed_tools = ["chatwoot_list_conversations", "chatwoot_get_conversation", "chatwoot_create_message"]
```
9つのチャネルの受信箱を1エージェントに集約し、下書き返信はApprovalBroker経由にします（送信前に人間のレビューを挟みたい場合は `chatwoot_create_message` に⚠を付ける）。用意するもの：Chatwoot APIアクセストークン。

### WooCommerce（公式ネイティブMCP — 開発プレビュー）

```toml
[[mcp.external]]
name = "woocommerce"
command = "npx"
args = ["-y", "@woocommerce/mcp-adapter"]   # WordPress MCP Adapter
env = { WP_SITE_URL = "https://shop.example.com", WP_MCP_OAUTH_TOKEN = "secret://vault/woo_oauth" }
allowed_tools = ["wc_list_products", "wc_get_order", "wc_list_orders"]
```
**WordPress MCP AdapterのOAuth 2.1を使ってください。旧来の `X-MCP-API-Key` は2026-06-23に非推奨となりました。** 用意するもの：WP MCP Adapterプラグインと、OAuthクライアント。

### DocuSeal（サーバー未提供 — `duduclaw-docuseal-mcp` を自作）

DocuSeal向けのMCPサーバーはまだ存在しません。REST＋webhook（生成 → 送信 → webhook完了通知）の小さなサーバーを自作してここにマウントし、その後上流にコントリビュートするのが現実的な道筋です。IMPL-PLAN §Dで工数M（中規模）として追跡されています。

### Monica（個人向けPRM — 薄いMCPかIdentityProviderで対応）

MCPサーバーは存在しません。`/api/contacts`（誕生日、やり取りの履歴）に対する薄いMCPを作るか、`IdentityProvider`として組み込む（`duduclaw-identity` 参照）方法があります。IMPL-PLAN §Dで追跡されています。

## ロードマップ

- サーバーごとの呼び出し監査を `tool_calls.jsonl` に記録する（現状、内蔵ツールは帰属が付くが、外部マウントは接続時にのみログされる）。
