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

# 3. 最初の HTTP server を停止し、トンネルの HTTPS URL で再起動
duduclaw http-server --bind 127.0.0.1:8765 \
  --public-origin https://mcp.example.com --trusted-proxy 127.0.0.1
```

外部公開用のURLを取得したら、claude.aiの「設定 → コネクタ → カスタムコネクタを追加」
（設定 → 連接器 → 新增自訂連接器）に貼り付ける：

```
https://<あなたのURL>/mcp
```

claude.aiは自動的にOAuthディスカバリー（RFC 9728 → RFC 8414 → 動的登録）を実行し、
DuDuClawの認可ページへ遷移する。ページ上で**内部MCP APIキー**（「管理 → 連携 → MCP API
キー」で「外部クライアント」をオフにして作成し、無効化も期限切れもしていないキー。v1.82 以降
これらのキーは `config.toml` にはない）を貼り付け、「接続に同意する」（同意連線）を
クリックすれば完了する。

## 固定 origin とプロキシの設定

外部向け OAuth では、実際の固定 HTTPS origin を指定して起動・再起動します。

```bash
duduclaw http-server --bind 127.0.0.1:8765 \
  --public-origin https://mcp.example.com --trusted-proxy 127.0.0.1
```

ドメインとプロキシ IP は実環境に置き換えてください。quick tunnel は先に URL を取得し、その URL を `--public-origin` に指定して `http-server` を再起動します。URL が変わると再認可が必要なため、本番では固定ドメインを推奨します。

| CLI オプション | デフォルトと動作 |
| --- | --- |
| `--public-origin <https-origin>` | 未指定時は `http://` と実際の bind socket アドレスを使用。指定時は HTTPS origin のみ許可し、認証情報・query・fragment・`/` 以外のパスを拒否します。issuer、resource、discovery、token、revoke の URL はすべてこの値を使用し、`Host`／`X-Forwarded-Proto` は反映しません。 |
| `--trusted-proxy <IP>` | デフォルトは空。信頼する各プロキシの正確な IP を繰り返し指定します。TCP 接続元がリスト内の場合のみ `X-Forwarded-For` を使用し、右から信頼済み hop を飛ばして最初の非信頼 IP を選びます。不正なヘッダーは TCP 接続元に戻ります。プロキシは実際の接続元 IP を上書き・追加し、信頼済み hop の偽装を防ぐ必要があります。 |
| `--allow-insecure-lan` | デフォルトは無効。非 loopback の平文待受を明示的に許可し、起動時に警告します。テスト専用で、操作者キーやトークンが盗聴される危険があります。TLS や転送ヘッダーの信頼は有効になりません。 |

非 loopback bind は、HTTPS `--public-origin` と 1 個以上の信頼済みプロキシ、または明示的な insecure opt-in がなければ起動前に拒否します。`--allow-insecure-lan` がない非 loopback 待受では、TCP 接続元が不明または信頼リスト外の要求を、本文の読み取り・認証・枠の消費前に HTTP 403 で拒否します。ヘルスチェックや未知のルートも対象で、転送ヘッダーでは回避できません。Loopback bind でのローカルプロキシ接続は引き続き利用できます。待受自体は HTTP で、プロキシからバックエンドも平文です。信頼済みプロキシで TLS を終端し、この区間のネットワークを保護してください。設定は CLI オプションであり、`config.toml` キーや環境変数の追加はありません。

平文 LAN テストでは実際の LAN IP を bind してください（例：`--bind 192.168.1.10:8765 --allow-insecure-lan`）。`0.0.0.0` などの wildcard はクライアントが利用できる canonical アドレスではありません。

認可とトークン要求は RFC 8707 `resource` を受け付け、`<canonical-origin>/mcp` との完全一致を要求します。省略時はこの URI のみを使用し、空文字・別 URI・矛盾する重複パラメーターは `invalid_target` になります。同意、コード交換、更新、アクセストークン検証のすべてで同じ audience を維持します。origin の変更時は再接続が必要で、audience のない旧コード・トークンは拒否します。

動的登録は、解決後の接続元 IP ごとに全試行を毎分 3 回まで数えます。独立した全体枠は毎分 10 回で、JSON・metadata 検証と容量確認を通り、クライアントを作成できる要求だけが消費します。不正な要求や容量不足は全体枠を消費しません。200 クライアントの上限では、作成から 1 時間以上経過した未認可の登録から最古のものを削除できます。有効なコード・認可・アクセス／更新トークンに参照されるクライアント、認可履歴のあるクライアント、猶予期間中の登録は容量回収の対象外です。候補がなければ登録を拒否します。通常の期限は未認可で 24 時間、認可済みで非活動 90 日です。成功した同意やトークン利用だけが活動時間を延長します。保存変更時に期限切れを削除し、期限切れや容量回収後は再登録が必要です。制限はプロセス単位で再起動時にリセットされるため、複数サーバーではプロキシに合計制限を設定してください。

## ChatGPT から使う

ChatGPT は DuDuClaw をカスタムコネクタ（開発者モードの「アプリ」）として追加できます。ストアには何も公開されず、コネクタはあなたの ChatGPT アカウントの中にだけあります。

1. `duduclaw http-server` を固定の **https** アドレスで公開します（リバースプロキシや名前付きトンネル。ChatGPT は OpenAI のサーバーから接続するため、ループバックや LAN のアドレスでは届きません）。
2. ChatGPT で開発者モードを有効にし（設定 → アプリとコネクタ → 詳細設定。提供プランは OpenAI 次第）、サーバー URL `https://<あなたのアドレス>/mcp`、認証 OAuth でコネクタを作成します。
3. ChatGPT は認可サーバーを見つけ（RFC 9728 → RFC 8414）、動的クライアント登録（`POST /oauth/register`）か Client ID Metadata Document（`client_id` が DuDuClaw の読みに行く https URL。2026-10-08 から対応、下記）で自分を登録します。サードパーティのガイドによるとリダイレクト URI は `https://chatgpt.com/connector_platform_oauth_redirect` です。DuDuClaw は任意の https リダイレクト URI を受け付けるので、特別な設定は要りません。
4. DuDuClaw の同意ページが開いたら、**内部** MCP API キーを貼り付けて承認します。

ChatGPT が得る権限はすべての OAuth クライアントと同じ外部クライアント等級（下記）です：基本ツールに加え、最大で `memory:read`、`memory:write`、`wiki:read`、`wiki:write`、`messaging:send`。コネクタ、実行系、名簿、Admin のツールは OAuth では決して公開されません。

Client ID Metadata Document：`client_id` がパス付きの `https://` URL で登録済み id でない場合、DuDuClaw はそれを取得します（公開アドレスのみ、固定解決、リダイレクトに従わない、5 秒、5 KiB、1 時間キャッシュ）。文書の `client_id` は URL と一致し、`redirect_uris` は https（またはループバックの http）で実際に使うものを含む必要があり、公開クライアント（`token_endpoint_auth_method` がないか `none`）だけを受け付けます。認可サーバーのメタデータは `client_id_metadata_document_supported: true` を宣言します。

CIMD のキャッシュ未登録 URL への取得試行は、解決後の接続元 IP ごとに毎分 12 回、プロセス全体で同時 4 ワーカーまでです。空き枠を 100 ミリ秒以内に取得できない場合は HTTP 503 `temporarily_unavailable` を返します。失敗や 5 秒の取得待ち時間を超えた結果は client_id URL ごとに 30 秒間キャッシュし、成功・失敗を合わせて最大 64 件保持します。5 秒の待ち時間には DNS 解決も含みます。要求のタイムアウトやキャンセル後も、実際の DNS・ネットワーク処理が終わるまでワーカーは枠を保持し、残存処理による同時実行数の超過を防ぎます。キャッシュの利用は取得試行枠を消費しません。

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

## 接続の再検証と取り消し（v1.80.0）

認可に使った内部 MCP キーは、有効な状態を維持する必要があります。認可コードの交換、トークン更新、各 access token 検証時にキーを再確認します。削除、期限切れ、メタデータ変更（スコープを含む）、設定の読み取り失敗で認可は無効になります。更新は `invalid_grant` を返し、access token を含む認可のトークンファミリー全体を削除します。同意で付与できる範囲は内部キーの権限までで、更新では元のスコープを維持または縮小できます。使用済み refresh token の再利用もファミリー全体を無効にします。

Admin は「管理 → 統合 → MCP API キー」で、有効な OAuth 認可のアプリ名、スコープ、認可日時、最終使用日時を確認し、取り消せます。`mcp_oauth_grants.list` と `mcp_oauth_grants.revoke` RPC は Admin 専用です。一覧は認証情報やそのハッシュを返しません。取り消しは後続のリクエストを拒否しますが、実行中のツール呼び出しは中止しません。

クライアントは HTTPS の `POST /oauth/revoke` に、`application/x-www-form-urlencoded` で `client_id`、`token`、任意の `token_type_hint` を送信できます。公開クライアントのみをサポートし、認証方式は token endpoint と同じ `none` です。トークンのクライアント紐づけを確認し、access token と refresh token のどちらでもファミリー全体を取り消します。不明なトークンや別クライアントのトークンには HTTP 200 を返し、他の認可は変更しません。client ID または token の欠落は拒否します。hint は無視される場合があります。メタデータには `revocation_endpoint` と `revocation_endpoint_auth_methods_supported` を公開します。[RFC 7009](https://www.rfc-editor.org/rfc/rfc7009) を参照してください。

**アップグレード時**：同意キーとファミリー情報のない旧 OAuth コード・トークンは拒否されます。アプリで接続を再認可してください。共通ストアは読み取り・更新・保存の全体をロックし、同時更新や登録、ダッシュボードからの取り消しで失効済み認可が復元されることを防ぎます。ストアの破損や読み取り失敗時は拒否します。

`duduclaw mcp issue-refresh-token` が発行する `ddc_refresh_*` は、`duduclaw mcp-server` の **stdio 専用**認証情報です。発行時に警告し、HTTP Bearer と SSE のクエリ認証では用途を説明して拒否します。`http-server` では静的 MCP API キーまたは OAuth を使い、既存の認証経路を維持します。CLI の更新用認証情報は `/oauth/token` では交換できません。stdio は呼び出しごとにスコープ、期限、失効を検証します。

検証範囲：実際のブラウザーでの受け入れ検証は未実施です。

OAuth は承認ごとに使用済み更新トークンのダイジェストを含むアクセス／更新レコードを最大 2,048 件、全体でトークン 16,384 件、承認 1,024 件、保存容量 16 MiB に制限します。上限到達時は現在の更新トークンを消費せず発行を拒否します。使用済みダイジェストは期限まで保持し、再利用検出とローテーション済みトークンによる失効を維持します。匿名要求は IP ごとに毎分 120 回です。有効な Bearer は独立した有界の主体枠（毎分 60 回）と既存のクライアント制限を使用し、匿名枠を使い切っても認証済み主体の枠は消費しません。IP は上記の信頼済みプロキシ設定で解決し、未設定ではプロキシ配下の匿名利用者が TCP 接続元枠を共有します。同時認証処理数も制限します。 枠の分類には毎秒最大 1 回更新する有界のダイジェストキャッシュを使用し、認可には使用しません。新しい認証情報の独立枠への反映には最大 1 秒かかります。保護された要求は最新状態を再検証するため、古い hint で失効を回避できません。静的キーの hint は最大 1 MiB の設定と 4,096 件までで、それを超えるキーは匿名枠を通って通常の認証に進みます。トークン交換は (client_id, 解決後の接続元 IP) ごとに毎分 12 回、さらに client_id や Bearer の分類によらず IP ごとに毎分 120 回までです。攻撃元の不正な交換要求で、同じ公開 client_id を利用する別の接続元の枠は消費されません。制限テーブルと保存用バックグラウンド処理数も有界です。アクセス検証は読み取り専用で、許可された利用の最終使用時刻はブロッキング処理用ワーカーで毎分最大 1 回更新します。承認一覧の設定エラーは空の一覧ではなく利用不可として返します。ダッシュボードの失効は操作者、承認 ID、アプリ名、権限、結果を監査し、認証情報やダイジェストは記録しません。監査書き込みに失敗すると、失効が適用済みの場合もエラーを報告します。

## エンドポイント一覧

| パス | 用途 |
|---|---|
| `POST /mcp` | 標準MCPエンドポイント：`server/discover`／`tools/list`／`tools/call`／`subscriptions/listen`（2026-07-28）、`initialize`／`ping`（旧世代）。[プロトコルバージョン](#プロトコルバージョン)を参照 |
| `GET /.well-known/oauth-protected-resource` | RFC 9728リソースメタデータ（401の `WWW-Authenticate` ヘッダーがここを指す） |
| `GET /.well-known/oauth-authorization-server` | RFC 8414認可サーバーメタデータ |
| `POST /oauth/register` | RFC 7591動的クライアント登録（public client） |
| `GET /oauth/authorize` → `POST /oauth/decision` | 認可コードフロー＋オペレーター同意ページ |
| `POST /oauth/token` | トークンの発行／更新 |
| `POST /oauth/revoke` | トークンファミリーの取り消し（RFC 7009、公開クライアント） |
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
  またはCloudflareのnamed tunnel）を使い、`--public-origin` に設定してください。
