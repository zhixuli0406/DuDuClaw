# MCP ツールを増やす：MCP レジストリ、リモートサーバー、アプリ統合サービス

DuDuClaw の AI 従業員は、自分の `.mcp.json` に並んだ MCP サーバーを通じてツールを使います。組み込みカタログ（Playwright、Browserbase、Filesystem、Memory）と「URL からインポート」に加えて、ダッシュボードでは次のことができるようになりました。

1. **公式 MCP レジストリを検索**し、サーバーを従業員ひとりにインストールする（`MCP → MCP レジストリ`）。
2. **リモート MCP サーバー**（Streamable HTTP）に OAuth サインイン、API トークン、または認証なしで**接続**する（`MCP → リモートサーバー`）。認証情報は Gateway が暗号化して保存・更新し、従業員にはツールだけが渡されます。
3. Marketplace タブから**ホスト型のアプリ統合サービス**（Zapier、Composio）に接続する。1 つのリモートエンドポイントの裏に何千ものサードパーティアプリがあります。
4. **サードパーティのツールを動作の種類で分類し、制御する**（第 4 節）。
5. **リモートサーバーからイベントを受け取る**（MCP Events、第 5 節）。何かが起きたときに自動化ルールや継続責任が作業を始められます。

接続は管理者だけが行えます。レジストリの検索はサインインしていれば誰でもでき、管理者以外のインストールはインストール申請となり、通常どおりマネージャー → 管理者の承認を経ます。

## 1. MCP レジストリの検索とインストール

`MCP → MCP レジストリ` で名前やテーマを入力して検索します。結果は `https://registry.modelcontextprotocol.io` から取得します（Gateway はこのホストにしか接続せず、結果を約 10 分キャッシュし、2 MiB を超える応答は拒否します）。各結果にはパッケージ種別（`npm`、`pypi`、`oci`）、ホスト型エンドポイントの有無（`remote`）、バージョン、リポジトリ、必須設定、インストールできない場合はその理由が表示されます。

| 理由 | 意味 |
|------|------|
| DuDuClaw が実行できるパッケージやリモートがない | `nuget`、`mcpb` などのパッケージのみ |
| 旧式の SSE 接続のみ | 2024-11-05 の HTTP+SSE プロトコル。ネイティブブリッジは非対応 |
| カスタムリクエストヘッダーが必要 | 例：`X-API-Key`。ブリッジが送るのは `Authorization` だけ |
| 非推奨／削除済み | レジストリ上の表示 |

**インストール**では従業員とサーバー名を選び、パッケージの必須環境変数を入力します（その従業員の `.mcp.json` に保存され、オペレーターの OS ユーザーだけが読めます）。パッケージとホスト型エンドポイントの両方がある場合は、どちらを使うか選べます。

Gateway 側の処理（`mcp.registry_install`）：

- 表示されたバージョンの `server.json` を取得し直す。
- 「URL からインポート」と同じパーサーに渡す（`npm → npx -y <pkg>@<version>`、`pypi → uvx <pkg>==<version>`、`oci → docker run -i --rm <image>`。バージョンは表示どおりに固定）。
- 管理者のインストールは `mcp.import.install` と同じ、セキュリティスキャン付きのインストール経路（スキャン失敗で拒否、ロック付きの `.mcp.json` 書き込み）。それ以外の人のインストールは `mcp.install_request` になり、承認後にインストールされます。

ホスト型エンドポイントはリモートサーバー（次節）としてインストールされ、従業員が使う前に管理者が接続する必要があります。

## 2. リモート MCP サーバー

### 仕組み

従業員の `.mcp.json` に入るリモートサーバーの項目は次のとおりです。

```json
"zapier": {
  "command": "/usr/local/bin/duduclaw",
  "args": ["mcp-remote-bridge", "--agent", "nova", "--server", "zapier"],
  "env": { "DUDUCLAW_HOME": "/home/me/.duduclaw" }
}
```

Claude CLI はこれを通常の stdio MCP サーバーとして起動します。非表示コマンド `duduclaw mcp-remote-bridge` は、すべての JSON-RPC メッセージ（リクエスト、通知、レスポンス）を Streamable HTTP でサーバーに転送し、サーバーの応答（JSON または `text/event-stream`）を書き戻します。`Mcp-Session-Id` を保持し、ネゴシエートした `MCP-Protocol-Version` を送り、各リクエストに最新の `Authorization: Bearer …` を付けます。

URL とすべての認証情報は `<home>/remote_mcp/servers.json`（権限 0600、プロセス間ロック）にだけ保存され、Gateway のマシン固有キーファイル（AES-256-GCM。チャネルトークンや OAuth トークンを守っているのと同じ鍵）で暗号化されます。暗号化できない場合は何も保存しません。`.mcp.json`、ブリッジのコマンドライン、環境変数のどこにも秘密情報は入りません。平文で残るのは、従業員、サーバー名、サインイン方式、URL のホスト、状態、時刻、リフレッシュトークンの有無だけです。

これは、DuDuClaw が以前リモートサーバー用に書き込んでいた `npx -y mcp-remote <url>` を置き換えるものです。このパッケージは Gateway マシン上でブラウザを開いてサインインし（ヘッドレスの Gateway や DuDuClaw OS アプライアンスでは不可能）、トークンを `~/.mcp-auth` に平文で保存します。現在これが書き込まれるのは、マニフェストの `"type": "sse"` の項目（ネイティブブリッジが扱えない旧式の接続方式）だけで、インポートのプレビューにその旨が表示されます。

### 接続する

`MCP → リモートサーバー → サーバーに接続`（またはレジストリでのインストール後の**接続**、Marketplace の統合サービスカードの**接続**）で、従業員、サーバー名、URL、サインイン方法を選びます。

- **サインイン（OAuth）**——Gateway がサーバーの認可サーバーを見つけ、「DuDuClaw」という名前でクライアント登録し、ダッシュボードが新しいタブでサインインページを開きます。承認するとプロバイダーがブラウザを `<ダッシュボード>/oauth/mcp/callback` に戻し、Gateway がコードを交換してトークンを保存し、`.mcp.json` に項目を書き込み、戻るリンク付きの小さな「Connected」ページを表示します。ダイアログは完了を自動で検知します。
- **API トークン**——一度貼り付けると `Authorization: Bearer <token>` として送られます。保存前に `initialize` リクエストで確認します。
- **なし**——サインイン不要のサーバー用（同様に事前確認します）。

**切断**は保存済みの認証情報を削除し、サーバーは残します（後で再接続できます）。**削除**は記録と `.mcp.json` の項目も消します。従業員のサーバー一覧からリモートサーバーを削除した場合も同じです。

プロバイダーの認可サーバーのメタデータに `revocation_endpoint`（RFC 7009）があれば、Gateway はリフレッシュトークン（なければアクセストークン）の失効も依頼します。ローカルの削除の後にバックグラウンドで最大 10 秒行うため、プロバイダーが遅い・失敗しても認証情報がディスクに残ることはありません。結果は監査 `remote_mcp_token_revocation`（`revoked`、`http_<ステータス>`、`failed`、`timeout`）に記録されます。Bearer トークンやエンドポイントのないプロバイダーはここから失効できないので、プロバイダー側で取り消してください。

### 追加ヘッダーとサーバーストリーム（任意）

接続ダイアログの「追加ヘッダーとサーバーストリーム」：

- **追加ヘッダー**：1 行に 1 つ `名前: 値`。このサーバーへのすべてのリクエスト（確認、ブリッジ、MCP Events）に付きます。値は暗号化された記録の中に保存され、`.mcp.json`、argv、環境変数には出ません。状態一覧には名前だけが表示されます。空欄なら保存済みのものを使います。拒否されるもの：DuDuClaw 自身が設定するヘッダー（`Authorization`、`Host`、`Content-Length`、`Content-Type`、`Accept`、`Mcp-Session-Id`、`MCP-Protocol-Version`、`Last-Event-ID`、`Cookie`、`Connection`、`Transfer-Encoding`、`Proxy-` や `Sec-` で始まる名前など）、HTTP の token でない名前、制御文字や非 ASCII を含む値、17 個以上。OAuth メタデータの探索リクエストには付きません。
- **サーバーから自発的に届くメッセージを受け取る**（既定オフ）：`notifications/initialized` の後、ブリッジは Streamable HTTP の任意の `GET` ストリームを開き、届いたメッセージをすべて従業員に渡します。切れたら（1 秒から倍々で 30 秒まで）`Last-Event-ID` 付きで開き直し、`405`（サーバーにそのストリームがない）や `404`（セッション切れ）で止まります。固定解決の HTTP クライアントは 10 分でタイムアウトするため、静かなストリームも少なくとも 10 分ごとに開き直されます。

トークンは有効期限の 1 分前から自動更新され、サーバーが `401` を返したときにもう一度更新します。プロバイダーがリフレッシュトークンを拒否すると、接続は**再サインインが必要**と表示されます。それまでの間、そのサーバーへの従業員の呼び出しは失敗し、管理者の再接続が必要だというメッセージが返ります。

### OAuth フローの詳細（MCP 認可仕様）

1. 認証なしの `initialize` を送り、`401` 応答の `WWW-Authenticate: Bearer resource_metadata="…"` から保護リソースメタデータ（RFC 9728）を得ます。この値がない場合は `/.well-known/oauth-protected-resource/<path>`、次に `/.well-known/oauth-protected-resource` を試します。メタデータは同じオリジンのものでなければなりません。
2. 認可サーバーのメタデータ（RFC 8414、次に OpenID Connect discovery。パス挿入形式を先に試す）を読みます。`issuer` が一致し、`code_challenge_methods_supported` に `S256` があることが必要で、なければ仕様どおり拒否します。
3. 動的クライアント登録（RFC 7591）：パブリッククライアント、`token_endpoint_auth_method = none`、リダイレクト URI は `<ダッシュボードのオリジン>/oauth/mcp/callback`。プロバイダーに登録エンドポイントがない場合は、ダイアログで**自分の OAuth クライアントを使う**を開き、そのリダイレクト URI で登録したクライアント ID（とシークレット）を入力します。
4. PKCE S256 付きの認可コードフロー。認可・トークン・更新の各リクエストに RFC 8707 の `resource` パラメーター（サーバーの正規 URL）を付けます。
5. `state` は 32 バイトの乱数で、Gateway のメモリにだけ保持され、従業員・サーバー・verifier・ダッシュボードのオリジンに紐づき、有効期限 10 分・一回限りです。
6. リフレッシュトークンはローテーションされます。プロバイダーから新しいものが来れば古いものを置き換えます。同じ接続の更新はロックファイルで直列化されるため、2 つのブリッジプロセスが同じリフレッシュトークンを使うことはありません。

Client ID Metadata Documents（CIMD）は実装していません。公開 URL 上の `client.json` が必要で、セルフホストの Gateway にはそれがないためです。

### サインイン結果を受け取れるダッシュボードのアドレス

リダイレクト先は、いま使っているダッシュボードのアドレス（`window.location.origin`）です。Gateway が受け付けるのは次のとおりです。

- ループバック（`localhost`、`127.0.0.1`、`[::1]`）。http・https どちらも可。
- `config.toml [gateway] allowed_origins`（または `DUDUCLAW_ALLOWED_ORIGINS`）に載っている `https` のアドレス。ホストとポートの完全一致で比較します。

それ以外のアドレス（LAN の IP `http://192.168.1.20:18789`、ホスト名 `http://duduclaw.local:18789`、`allowed_origins` にない https など）はリダイレクトを直接受け取れません。OAuth 2.1 で https 以外のリダイレクト URI が許されるのはループバックだけだからです。その場合は 2 段階でサインインします。

1. サインインページはブラウザを `http://127.0.0.1:<ダッシュボードのポート>/oauth/mcp/callback`、つまり「ブラウザが動いているコンピューター」のループバックに戻します（RFC 8252 §7.3、すべての OAuth 2.1 サーバーが受け付けます）。ブラウザが Gateway マシン上にあれば、それは Gateway 自身なので通常どおり完了します。
2. 別のコンピューターではそのページは開けません（「接続できません」）。これは想定どおりです。アドレスバーのアドレスをすべてコピーし、接続ダイアログに表示される欄に貼り付けてください（RPC `mcp.remote_complete`）。Gateway は `/oauth/mcp/callback` 上のループバックアドレスで、ホスト・ポート・パスがこのサインインで登録したリダイレクトと一致するものだけを受け付け、state は一度しか使えません。貼り付けるアドレスには一回限りの認可コードと state しか含まれず、PKCE の検証子は Gateway から出ません。

コールバックページは Gateway 自身が `/oauth/mcp/callback` で返し、ログインは不要です（一回限りの state が防御です）。

### Gateway が接続してよいアドレス

関係するすべての URL（サーバー、そのメタデータ、認可サーバーの各エンドポイント）は `https` で、公開インターネットのアドレスにだけ解決されなければなりません（`duduclaw_core::net_addr::is_public_ip`）。解決したアドレスはその接続で固定され、POST ではリダイレクトに従わず、メタデータ取得の GET ではリダイレクト先を再検査します。`http` が許されるのは Gateway マシン上のサーバー（`localhost`、`127.0.0.0/8`、`::1`）だけで、その場合に限りメタデータもループバックを指せます。プライベートネットワーク（`10.x`、`192.168.x` など）のサーバーは拒否します。

### 秘匿化（Redaction）

RFC-23 の秘匿化が有効なとき、ブリッジ項目は他の stdio サーバーと同様に `duduclaw mcp-proxy` でラップされ、リモートツールの結果も秘匿化されます。（このときプロキシは第 4 節のツール制御をブリッジに任せ、二重に確認しません。）

## 3. ホスト型アプリ統合サービス：Zapier と Composio

Marketplace タブには 2 枚のリモートカードがあります。

| カード | 既定のエンドポイント（MCP レジストリより） | サインイン |
|--------|--------------------------------------------|------------|
| Zapier | `https://mcp.zapier.com/api/v1/connect` | OAuth |
| Composio | `https://connect.composio.dev/mcp` | OAuth |

**接続**を押すとエンドポイント入力済みのリモート接続ダイアログが開きます。アカウントに表示されるエンドポイントが違えば置き換え、プロバイダーから API トークンを受け取っていればトークン方式に切り替えてください。DuDuClaw にはアカウントや鍵は同梱されていません。

従業員がこれらのツールに送る内容はすべてプロバイダーのクラウドを経由し、プロバイダーはそこで接続したすべてのアプリを操作できます。Zapier や Composio のアカウントでは、この従業員に必要なアプリと操作だけを有効にし、できれば従業員ごとに別の接続を使ってください。

## 4. サードパーティツールの動作の種類

DuDuClaw 自身のツールにはそれぞれ動作の種類（`read`、`draft`、`send`、`publish`、`purchase`、`delete`、`modify`、`admin`）があり、従業員の `[capabilities] action_rules` で種類ごと・ツールごとに許可・確認・ブロックできます（`docs/features/05-security-defense.md`）。従業員のほかの MCP サーバーのツールも、DuDuClaw がその経路上にある場所で分類されます。`.mcp.json` から起動するサーバーは `duduclaw mcp-proxy`、リモートサーバーはリモートブリッジです。

**分類**は、サーバーが `tools/list` で各ツールに宣言する `annotations` から決まります。

| アノテーション | 種類 |
|----------------|------|
| `destructiveHint: true` | `delete` |
| `readOnlyHint: true`、サーバーが `trusted_read_hint_servers` にある | `read` |
| `readOnlyHint: true`、サーバーがない | `modify` |
| それ以外（アノテーションなしを含む） | `modify` |

アノテーションはサーバーの自己申告で、誰も確認しません。`destructiveHint` はどのサーバーからでも信じます（ツールを厳しくするだけなので）。`readOnlyHint` は管理者が列挙したサーバーだけ信じます。

```toml
[capabilities]
trusted_read_hint_servers = ["github"]   # .mcp.json のサーバー名
action_rules = [
  { effect = "modify", verdict = "ask" },
  { tool = "github.delete_repository", verdict = "block" },   # または "mcp__github__delete_repository"
]
```

既定の代償：悪意あるサーバーは削除を行うツールに `readOnlyHint: true` と付けられます。信じればすべての `read` ルールと読み取り専用レーンを通ってしまいます。信じなければ、列挙されていないサーバーの本当に読み取り専用のツールも変更として扱われます（`modify` が確認・ブロック・非表示になる場所では同様）。サーバーが一覧化する前に呼ばれたツールはアノテーションがないので `modify` です。

**適用**はプロキシとブリッジで、従業員の `action_rules` に従います（一覧化と呼び出しのたびに読み直し、DuDuClaw のツールと同じ規則。`tool` ルールは `<サーバー>.<ツール>` または `mcp__<サーバー>__<ツール>`、`<サーバー>.*` でそのサーバーの全ツール）。

- `block`：従業員が見る `tools/list` から外し、呼び出しはサーバーに届く前に JSON-RPC エラー `-32003` で返します（監査 `third_party_tool_refused`）。
- `ask`：呼び出しは ApprovalBroker の判断を待ちます（`mcp_call` カード、5 分）。拒否・期限切れ・承認システムの停止はすべて拒否です（監査 `third_party_tool_approval`）。
- **読み取り専用レーン**（`DUDUCLAW_LANE=explore`：ハートビートの能動チェックと MCP Events で始まる作業）：種類が `read` のツールだけを一覧化・呼び出しでき、列挙されていないサーバーのツールはすべて隠れます。

`action_rules` キーがなく通常レーンなら何も変わりません。秘匿化が有効なとき、従業員に `action_rules` キーがあるとき、または読み取り専用レーンで起動するとき、`.mcp.json` の stdio サーバーはプロキシを通ります。リモートサーバーは常にブリッジを通ります。

**ダッシュボード**：`MCP → リモートサーバー →「サードパーティのツールと動作の種類」` で、従業員ごとに各サーバーがプロキシまたはブリッジ経由で最後に一覧化したツール（`<home>/mcp_tool_effects/<従業員>/<サーバー>.json`）を、現在の設定で再計算した種類と判断付きで表示します。セッションが DuDuClaw 経由で一覧化した後にだけ表示されます。スナップショットは表示専用で、制御は常にその時点の一覧に従います。

**対象ランタイム**：Claude CLI（`.mcp.json` のサーバーを起動し、spawn は書き換えた設定を渡します）。Codex、Gemini、Antigravity、Grok の従業員には DuDuClaw 自身のサーバーだけが登録され、openai-compat のツールループも `duduclaw mcp-server` しか起動しないため、制御すべきサードパーティサーバーはありません。`.mcp.json` の `url`／`type` 項目（CLI が直接接続）は対象外なので、リモートサーバーとして接続してください。

## 5. リモートサーバーからのイベント（MCP Events）

草案の MCP Events 拡張に対応したリモートサーバーは、何かが起きたとき（新しいインシデント、新着メール…）に Gateway へ知らせられます。実装したのは Triggers & Events ワーキンググループの設計スケッチ（`modelcontextprotocol/experimental-ext-triggers-events` の `docs/design-sketch-proposal.md`、2026-02-19 付けの草案）の webhook モードです。

**設定**

1. サーバーが到達できるアドレスを Gateway に与えます：`config.toml [mcp_events] public_base_url = "https://hooks.example.com"`（Gateway のポートへのリバースプロキシやトンネル。`https` のみ、テスト時はループバックに限り `http`）。購読ごとのコールバックは `<public_base_url>/webhook/mcp-events/<id>` です。
2. **リモートサーバー**でそのサーバーに接続します（サインイン方式は問いません）。
3. 「リモートサーバーからのイベント」でサーバーを選び、イベント名を入力して**購読**します。Gateway はサーバーに問い合わせ（`initialize` が `capabilities.events` を宣言している必要があります）、`whsec_` 署名用シークレットを作り、イベント名ごとに `events/subscribe`（`delivery: { mode: "webhook", url, secret }`、`ttlMs` は 1 日）を呼び、サーバーの検証チャレンジに応答します。スイープ（起動時とその後 10 分ごと）が各許可の `refreshBefore` の前に購読し直します。

**受信**（`POST /webhook/mcp-events/{id}`、常にマウント）：不明な id ⇒ `404`、256 KiB 超 ⇒ `413`、購読ごとに毎分 120 件超 ⇒ `429`。Standard Webhooks の署名（`webhook-id`、`webhook-timestamp`、`webhook-signature`、`id.timestamp.body` の HMAC-SHA256、定数時間比較、複数署名可）と 5 分以内のタイムスタンプが必要で、なければ `401`。`X-MCP-Subscription-Id` が送られたら、サーバーが返した id でなければなりません。同じ `webhook-id` は成功を返して破棄します。制御メッセージ：`verification` はチャレンジを返し、`gap` は監査に記録、`terminated` は購読を終了（以後の配信は `410`）。購読していないイベント名は `410`（再送なし）。

受け付けたイベントは `events.db` の `mcp.event` 行になり、購読、従業員、サーバー、イベント名と id、レーン、時刻、イベントの `data` を持ちます。`data` は `input_guard` でスキャンされ（該当すると `suspicious: true`。破棄はしません）、16 KiB を超えると切り詰めたテキストに置き換えられます。これはデータであり、指示ではありません。

- **自動化ルール**：トリガー `mcp_event`（項目 `server`、`name`、`agent_id`、`lane`、`suspicious`、`data.*`）。ここから作られるプロンプトの先頭には固定のセキュリティ注意書きが付きます。
- **継続責任**：イベントソース `mcp.event`、購読の従業員のものです。

**既定は読み取り専用。** イベントで始まる作業（自動化ルールの `delegate` や `run_skill`）は読み取り専用レーンで動きます。キューのメッセージに `lane = "explore"` が付き、Claude CLI は `DUDUCLAW_LANE=explore`（DuDuClaw のツールは `read`／`draft` のみ）、組み込みツールは `Read`、`Glob`、`Grep`、`WebFetch`、`WebSearch` だけ、サードパーティサーバーは制御付きプロキシ経由（第 4 節）で起動します。これは[読み取り専用の継続タスク](continuous-responsibilities.md)と同じレーン・同じフラグで、規則も同じです：OpenAI 互換ランタイムの従業員は実行でき、DuDuClaw のツールは同じく制限され、`agent.toml [mcp.external]` のサーバーはマウントされません。ほかのランタイムやタスクサンドボックスの従業員は開始前に拒否され（`explore_lane_unsupported`）、MoA モデルとローカル推論のみの設定も拒否されます（ハイブリッドのローカル振り分けは使いません）。購読時に「通常モードを許可」をオンにすると、イベントが従業員の通常の権限で作業を始められます。継続責任を起こすのはこの購読だけで（occurrence は通常の目標タスクです）、読み取り専用の購読のイベントはそこで `dropped(explore_lane)` と記録されます。

**シークレット**：Gateway のキーファイルで暗号化し `<home>/mcp_events/subscriptions.json`（0600）に保存します。「署名用シークレットを交換」は購読の更新で新しいものをサーバーに送り、古いものは 15 分間受け付けます。「購読を解除」はまずローカルの購読を消し（コールバックはすぐ `404`）、その後ベストエフォートで `events/unsubscribe` を呼びます。監査：`mcp_event_subscription_created`／`_rotated`／`_revoked`／`_refresh_failed`、`mcp_event_delivered`、`mcp_event_delivery_rejected`、`mcp_event_control`（id、名前、件数のみ）。

検証済みと仮定：読めたのは設計スケッチだけです（ChatGPT の対応についての OpenAI のページは取得できませんでした）。poll と push の配信、カーソルと再生（購読は常に「今」から）、`deliveryStatus`、`maxAgeMs`、`events/list`、購読の `arguments`（常に `{}`）、任意の `v1a` サーバー署名は未実装です。テストはローカルの偽サーバーに対してのみです。

## RPC 一覧

| メソッド | 対象 | 用途 |
|----------|------|------|
| `mcp.registry_search { query, cursor? }` | サインイン済み | 検索（固定ホスト、キャッシュあり） |
| `mcp.registry_install { name, version?, agent_id, remote?, server_name?, env? }` | サインイン済み（管理者以外 ⇒ インストール申請） | スキャン付きの経路でインストール |
| `mcp.remote_connect { agent_id, name, url?, auth, bearer?, redirect_origin?, client_id?, client_secret?, headers?, server_stream? }` | 管理者 | 接続。`oauth` は `authorize_url` を返す。`headers` `{名前: 値}` は保存済みのものを置き換える |
| `mcp.remote_status { agent_id? }` | 管理者 | 秘密情報を含まない記録（`header_names`、`server_stream`） |
| `mcp.remote_disconnect { agent_id, name, forget? }` | 管理者 | 認証情報の削除（`forget` で項目も削除）、可能ならプロバイダーで失効 |
| `mcp.tool_effects { agent_id }` | 管理者 | 最後に一覧化されたサードパーティツールと種類・判断 |
| `mcp.events_subscribe { agent_id, server, event_types, mode? }` | 管理者 | 購読（`mode` は既定 `explore`、または `normal`） |
| `mcp.events_list { agent_id? }` | 管理者 | 秘密情報を含まない購読 |
| `mcp.events_unsubscribe { id }` | 管理者 | 削除してからサーバーで購読解除 |
| `mcp.events_rotate { id }` | 管理者 | 署名用シークレットの交換 |

監査イベント（`security_audit.jsonl`）：`remote_mcp_connect_started`、`remote_mcp_connected`、`remote_mcp_connect_failed`、`remote_mcp_disconnected`、`remote_mcp_token_revocation`（従業員、サーバー、ホスト、サインイン方式。URL のパスやトークンは含みません）、および第 4・5 節のイベント。`duduclaw doctor` の「員工 MCP 設定中的其他伺服器」行は、ブリッジ項目をホストと接続状態付きで表示します。

## 対象外／未検証

- 実際の Zapier・Composio アカウントや、実在するサードパーティの OAuth プロバイダーではテストしていません。検証はローカルの偽の認可サーバーと偽の MCP サーバーに対してのみです。
- ブリッジは POST への応答として切れたイベントストリームを再開しません。サーバー起点の `GET` ストリームはサーバーごとのオプトインで、ローカルの偽サーバーでのみテストしています。
- 旧式の HTTP+SSE サーバーは引き続き `npx mcp-remote` を使います（上記）。必須ヘッダーを宣言するレジストリ項目は今もインストール不可と表示されるので、URL で接続して「追加ヘッダー」を使ってください。
- プロバイダーでの失効は、認可サーバーが `revocation_endpoint` を宣言している場合だけで、ベストエフォートです。
- サードパーティツールの分類は第 4 節のとおりサーバーのアノテーションを信じます。ダッシュボードが読むスナップショットは従業員自身のプロセスツリーが書きます。
- MCP Events：第 5 節の最後を参照。Gateway はサーバーが受け付ける https アドレスで到達できる必要があります。
- デスクトップアプリのダッシュボードのオリジン（`tauri://…`）はリダイレクト先として受け付けません。ブラウザからサインインしてください。
- 従業員は Gateway と同じ OS ユーザーで動きます。制限のない Bash を持つ従業員はキーファイルや記録ファイルを読めたり、自分の `--agent` でブリッジを手動起動したりできます。ブリッジはプロセスの `DUDUCLAW_AGENT_ID` と異なる `--agent` を拒否しますが、本当の隔離は Bash を与えないことです。
- 進行中のサインインは Gateway のメモリにだけあります。サインイン中に Gateway を再起動した場合はやり直してください。
