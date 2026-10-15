# Remote MCP：claude.aiや任意のMCPクライアントから自分のDuDuClawに直接接続する

`duduclaw http-server` は標準的な**MCP Streamable HTTP**エンドポイント（`POST /mcp`）と、
完全な**OAuth 2.1**認可フローを提供する。これにより、claude.aiのカスタムコネクタ
（Custom Connector）、Claudeモバイルアプリ、MCP Inspector、またはremote MCPサーバーに
対応する任意のアプリから、自分でホストするDuDuClawに直接接続し、記憶やナレッジベースなどの
ツール群を利用できる。

## クイックスタート

```bash
# 1. HTTPサーバーを起動する（デフォルトではloopbackのみにバインド）
duduclaw http-server --bind 127.0.0.1:8765

# 2. 外部公開が必要な場合はトンネルを開く（または自前のリバースプロキシ／ドメインを使う）
duduclaw tunnel          # Cloudflare quick tunnel。画面にhttps URLが表示される
```

外部公開用のURLを取得したら、claude.aiの「設定 → コネクタ → カスタムコネクタを追加」
（設定 → 連接器 → 新增自訂連接器）に貼り付ける：

```
https://<あなたのURL>/mcp
```

claude.aiは自動的にOAuthディスカバリー（RFC 9728 → RFC 8414 → 動的登録）を実行し、
DuDuClawの認可ページへ遷移する。ページ上で**内部MCP APIキー**（`config.toml [mcp_keys]`
内で `is_external = false` になっているキー）を貼り付け、「接続に同意する」（同意連線）を
クリックすれば完了する。

## ChatGPT から使う

ChatGPT は DuDuClaw をカスタムコネクタ（開発者モードの「アプリ」）として追加できます。ストアには何も公開されず、コネクタはあなたの ChatGPT アカウントの中にだけあります。

1. `duduclaw http-server` を固定の **https** アドレスで公開します（リバースプロキシや名前付きトンネル。ChatGPT は OpenAI のサーバーから接続するため、ループバックや LAN のアドレスでは届きません）。
2. ChatGPT で開発者モードを有効にし（設定 → アプリとコネクタ → 詳細設定。提供プランは OpenAI 次第）、サーバー URL `https://<あなたのアドレス>/mcp`、認証 OAuth でコネクタを作成します。
3. ChatGPT は認可サーバーを見つけ（RFC 9728 → RFC 8414）、動的クライアント登録（`POST /oauth/register`）か Client ID Metadata Document（`client_id` が DuDuClaw の読みに行く https URL。2026-10-08 から対応、下記）で自分を登録します。サードパーティのガイドによるとリダイレクト URI は `https://chatgpt.com/connector_platform_oauth_redirect` です。DuDuClaw は任意の https リダイレクト URI を受け付けるので、特別な設定は要りません。
4. DuDuClaw の同意ページが開いたら、**内部** MCP API キーを貼り付けて承認します。

ChatGPT が得る権限はすべての OAuth クライアントと同じ外部クライアント等級（下記）です：基本ツールに加え、最大で `memory:read`、`memory:write`、`wiki:read`、`wiki:write`、`messaging:send`。コネクタ、実行系、名簿、Admin のツールは OAuth では決して公開されません。

Client ID Metadata Document：`client_id` がパス付きの `https://` URL で登録済み id でない場合、DuDuClaw はそれを取得します（公開アドレスのみ、固定解決、リダイレクトに従わない、5 秒、5 KiB、1 時間キャッシュ）。文書の `client_id` は URL と一致し、`redirect_uris` は https（またはループバックの http）で実際に使うものを含む必要があり、公開クライアント（`token_endpoint_auth_method` がないか `none`）だけを受け付けます。認可サーバーのメタデータは `client_id_metadata_document_supported: true` を宣言します。

未検証：実際の ChatGPT アカウントでは試していません。リダイレクト URI と登録方法はサードパーティのコネクタガイドによるもので（執筆時に OpenAI 自身のページは取得できませんでした）、フローはローカルのリクエストでのみテストしています。

## 認可モデル（重要）

OAuthが発行するアクセストークンは**常に「外部クライアント」ティア**として扱われ、外部向け
ツール面と同じscopeポリシーを共有する。別ルールは存在しない：

- 基本ツール面（7つの基本ツール）は常に利用可能。
- コネクタが要求するscopeは**外部に付与可能なホワイトリスト**
  （`memory:read` / `memory:write` / `wiki:read` / `wiki:write` / `messaging:send`）に
  絞り込まれる。コネクタ系（Odoo/Google/Notion）、実行系、名簿、Adminは、クライアントが
  どう要求しようとも**OAuth経由では絶対に開放されない**。
- 同意ページに貼り付けるのは内部キーでなければならない（外部キーは自分自身をアップグレード
  できない）。

トークンの詳細：アクセストークンは1時間、リフレッシュトークンは30日間有効で使用のたびに
ローテーションされる。認可コードは1回限り有効で10分間。PKCE S256は必須。すべてのトークンは
SHA-256ハッシュとしてのみディスクに保存される（`~/.duduclaw/mcp_oauth_issued.json`、
パーミッション0600）。

## エンドポイント一覧

| パス | 用途 |
|---|---|
| `POST /mcp` | 標準MCPエンドポイント：`server/discover`／`tools/list`／`tools/call`／`subscriptions/listen`（2026-07-28）、`initialize`／`ping`（旧世代）。[プロトコルバージョン](#プロトコルバージョン)を参照 |
| `GET /.well-known/oauth-protected-resource` | RFC 9728リソースメタデータ（401の `WWW-Authenticate` ヘッダーがここを指す） |
| `GET /.well-known/oauth-authorization-server` | RFC 8414認可サーバーメタデータ |
| `POST /oauth/register` | RFC 7591動的クライアント登録（public client） |
| `GET /oauth/authorize` → `POST /oauth/decision` | 認可コードフロー＋オペレーター同意ページ |
| `POST /oauth/token` | トークンの発行／更新 |
| `POST /mcp/v1/call`、`GET /mcp/v1/stream` | 既存のDuDuClaw REST/SSE面（変更なし） |

静的Bearerキー（`ddc_…`）とOAuthトークン（`ddc_oauth_…`）は同じ `Authorization: Bearer`
インターフェースを共有する。スクリプトや自社の連携は引き続き静的キーを使えばよく、OAuthは
claude.aiのように「OAuthしか話さない」クライアント向けに用意されている。

## プロトコルバージョン

`POST /mcp` と stdio の `duduclaw mcp-server` は、MCP の二つの世代の両方に対応しています。

- **新世代 2026-07-28**：`initialize` はありません。各リクエストが `params._meta` にバージョンとクライアント機能（`io.modelcontextprotocol/protocolVersion`、`io.modelcontextprotocol/clientCapabilities`）を持ちます。`server/discover`・`tools/list`・`tools/call`・`subscriptions/listen` に対応します。HTTP では各リクエストに `MCP-Protocol-Version` と `Mcp-Method` ヘッダーが必要で、`tools/call` には `Mcp-Name` も必要です。ヘッダーの欠落や本文との不一致は `400` と `-32020`、未対応のバージョンは対応バージョン一覧付きの `-32022` になります。
- **旧世代 2024-11-05 〜 2025-11-25**：`initialize` で交渉します。要求したバージョンをそのまま返し、知らないバージョンには 2025-11-25 を返します。`ping` にも応答します。

ツール一覧が変わったとき：stdio の旧世代クライアントには従来どおり `notifications/tools/list_changed` が届きます。新世代クライアントには、`toolsListChanged: true` を付けて開いた `subscriptions/listen` の上でだけ届きます。HTTP エンドポイントは状態を持たず、呼び出し元のツールが変わったことを知る手段がないため、購読には通知種別なしで確認応答を返し、すぐに購読を終了します。HTTP クライアントは `tools/list` の `ttlMs`（5 秒）が切れたら読み直してください。

2026-10-16 に公式 TypeScript クライアント `@modelcontextprotocol/client` 2.3.1 で、2026-07-28 固定・自動・旧世代の三つのモードを stdio と HTTP の両方で確認しました。

## セキュリティに関する補足

- ブラウザのオリジン（`Origin` ヘッダー）はloopbackと `config.toml [gateway]
  allowed_origins` のホワイトリスト（アンカーマッチ）に照合され、それ以外はすべて403となる。
  ブラウザ以外のクライアントには影響しない。
- quick tunnelのURLは起動のたびに変わる。本番の外部公開では固定ドメイン（リバースプロキシ
  またはCloudflareのnamed tunnel）を使うこと。OAuthフローのissuerはリクエストの
  `Host`／`X-Forwarded-Proto` から導出される。
- 発行済みの接続をすべて失効させるには `~/.duduclaw/mcp_oauth_issued.json` を削除すればよい
  （次のリクエストから即座に無効になる）。
