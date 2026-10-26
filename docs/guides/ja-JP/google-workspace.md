# Google Workspace統合(全8サービス、ネイティブ対応)

> **公開ステータス(v1.49.0で更新):** ダッシュボードの「Integrations →
> Google」タブは、デフォルトで**表示されるようになりました**。以前のバージョンでは
> GoogleのOAuthアプリ審査待ちのため非表示にしていましたが、審査が制限するのは
> 「自前のOAuth clientを持ち込む」経路だけであり、サービスアカウントによる
> ドメイン全体委任やApps Scriptブリッジには影響しないことが確認できたため、
> タブを隠しておく理由がなくなりました。ただし表示されていることは、ツールが
> 有効であることを意味しません。バックエンドには独立したマスタースイッチ
> `config.toml [integrations] google_workspace`があり、**デフォルトは
> `false`**です。オフのままだと認証情報の設定や接続テストは通りますが、
> ツールがAI従業員の前に現れることはなく、ダッシュボードには明確な黄色い
> 警告が表示されます。3つの認証経路の選び方については
> [google-workspace-integration.md](../google-workspace-integration.md)を参照してください。

> **設計上の判断(D5、2026-08-04):** DuDuClawは共有のGoogle OAuth認証情報を
> 同梱しません。ユーザーは自分自身のOAuth clientを用意する(あるいは代わりに
> DWD／Apps Script経路を使う)必要があり、DuDuClawはその結果得られたトークンを
> 保存・更新するだけで、自前のclient id／secretを同梱することは決してありません。
> 判断の背景は
> [google-workspace-integration.md](../google-workspace-integration.md)(D5)を
> 参照してください。

Googleアカウントを接続すると、あなたのAI従業員はメールの検索・閲覧、返信下書きの
作成、カレンダーの一覧表示、イベントの作成(Google Meetリンク付き)、Googleスプレッド
シートの行の読み取り・追記、Googleフォームの回答の読み取り、Google Tasksの管理が
できるようになります。DuDuClawはGoogleのREST APIにネイティブに接続するため、
サードパーティのMCP serverをインストールする必要はありません。アクセストークンは
DuDuClawの暗号化されたOAuth vaultに保存され、自動的に更新されます。

**8つのWorkspaceサービスすべてがネイティブに対応**しています。Gmail、Calendar、
Sheets、Drive、Docs、Slides、Forms、TasksはすべてGA(一般提供)のREST API上で
動くため、ここで説明する内容はGoogleのDeveloper Previewプログラムに一切依存せず、
どの顧客でも利用できます。(Google自身が提供する公式のremote MCP serverは
8つ中6つをカバーしますが、Preview限定であり、利用規約上Pre-GA APIを自分の
ドメイン外のユーザーに公開することを禁じています。こちらは上級者向けの
オプトインとして引き続き利用可能です: [google-mcp.md](../google-mcp.md))。

## 何が使えるようになるか

2つのスコープ(`google:read` / `google:write`)で管理される、19個のエージェント向け
MCPツールです。

| ツール | 分類 | 内容 |
|------|-------|--------------|
| `google_status` | read | 接続状態の診断: 接続済みか、付与されたスコープ、トークンの有効性。ローカルの状態のみを読み取る。 |
| `gmail_search` | read | Gmailのクエリ構文(`from:… is:unread`など)でメールボックスを検索。送信者／件名／日付／スニペットを返す。 |
| `gmail_read` | read | 1件のメッセージを全文読み取る: ヘッダー、プレーンテキスト本文(長い場合は切り詰め)、添付ファイル一覧(ファイル名とサイズのみ、ダウンロードは行わない)。 |
| `gmail_create_draft` | write | Gmailの**下書き**を作成する。送信は行わない。送信は常に人間が手動で行う。 |
| `calendar_list_events` | read | プライマリカレンダーのイベントを一覧表示する(デフォルトは今後7日間)。 |
| `calendar_create_event` | write | 実際に外部から見えるイベントを作成する。Google Meetリンクをオプションで付与可能。 |
| `sheets_read` | read | スプレッドシートのセル範囲を読み取る(スプレッドシートIDまたはシートのURL全体を受け付ける)。最大200行の整形済みの値を返す。 |
| `sheets_append` | write | `USER_ENTERED`入力モードでスプレッドシートに1行追記する(数値／日付／数式は手入力したのと同じように解釈される)。 |
| `forms_get` | read | フォームの構造を読み取る: タイトル、説明、および各質問の`question_id`、種類、選択肢。 |
| `forms_list_responses` | read | フォームに送信された回答を一覧表示する(最大50件)。回答は`question_id`をキーとするため、`forms_get`と組み合わせてIDをタイトルに対応付ける。 |
| `gtasks_lists` | read | アカウントのGoogle Tasksリスト(id + タイトル)を一覧表示する。`@default`を使うとルックアップなしでデフォルトリストを指定できる。 |
| `gtasks_list` | read | 1つのリスト内のタスクを一覧表示する(デフォルトは未完了のみ。`show_completed=true`で完了済み・非表示のものも含む)。 |
| `gtasks_create` | write | ユーザーのGoogle Tasksに実際のタスクを作成する。 |
| `gtasks_complete` | write | タスクを完了済みとしてマークする。 |
| `drive_search` | read | ファイル**名と全文**でDriveを検索する(ゴミ箱内のファイルは除外、新しい順)。オプションで厳密なMIMEフィルターを指定可能。 |
| `drive_read` | read | Driveのファイルをテキストとして読み取る: Docs／Slidesはプレーンテキストとしてエクスポート、Sheetsは(**最初のシートのみ**)CSVとしてエクスポート、テキスト系のファイルはそのまま読み取る。バイナリ型はメタデータと注記のみを返し、バイナリ内容そのものは返さない。 |
| `docs_read` | read | Google Documentの文書順にテキストを読み取る(表のセルのテキストを含む)。 |
| `docs_append` | write | Docの**末尾**にテキストを追記する。追記のみで、既存の内容を書き換えたり削除したりするツールはない。 |
| `slides_read` | read | プレゼンテーションのテキストをスライドごとに読み取る(図形、グループ化された図形、表のセル)。 |

> **意図的にSlides書き込みツールは提供していません**: DuDuClawのオフィス
> 文書スイートはすでに実際の`.pptx`ファイルを生成できます。これはSlidesの
> `batchUpdate`要素APIを操作するより安全で、出力の質も上回ります。

> **命名規則:** Google Tasks用のツールは`gtasks_*`という名前です。DuDuClaw
> 自身のタスクボードは`tasks_*`(`tasks_list` / `tasks_create` /
> `tasks_complete` / …)のままです。これは2つの別個のシステムであり、
> エージェントが「自分の作業キュー」と「ユーザーのGoogle Tasks」を混同
> しないよう、意図的に異なるプレフィックスを使っています。

**FormsとTasksには公式のMCP serverが存在しません**(2026-07-30に検証済み:
`formsmcp`／`tasksmcp`の両エンドポイントとも404を返し、GoogleのMCP文書にも
この2つのサービスは記載されていません)。これがここでネイティブに提供している
理由です。

### 安全設計

- **下書きは決して送信されません。** `gmail_create_draft`は下書きを保存する
  だけで、「送信」ツールは存在しません。送信は常に人間の判断です。
- **読み取りは読み取りのままです。** read分類のツールはGmailやCalendarの
  内容を一切変更できません。
- **Forms、Drive、Slidesは読み取り専用です。** フォームの作成・編集、Driveへの
  書き込み、プレゼンテーションの変更を行うツールはありません。書き込み用ツールが
  あるのはGmail(下書き)、Calendar、Sheets、Docs(追記)、Tasksだけです。
- **最小権限。** Driveは`drive.readonly`のみを要求します(`drive`や
  `drive.file`は要求しません。Driveファイルを作成するツールが存在しないため)。
  Slidesは`presentations.readonly`を要求します。Docsが完全な`documents`
  スコープを必要とするのは、`docs_append`が書き込みを行うためだけです。
- **オプションの承認ゲート。** より慎重にしたい場合は、書き込み系ツールを
  エージェントの`agent.toml [capabilities] approval_required_tools`に
  リストアップすることで、下書き・イベント・スプレッドシートへの書き込みの
  たびにHITL(human-in-the-loop)承認を待たせることができます。

  ```toml
  [capabilities]
  approval_required_tools = ["gmail_create_draft", "calendar_create_event", "sheets_append", "gtasks_create", "gtasks_complete", "docs_append"]
  ```

## 認証経路を選ぶ

同じ19個のツールを認可する方法は3通りあります。違いは誰が設定を行うか、
そしてGoogleが事前にアプリを審査している必要があるかどうかです。

| | 個人の @gmail.com | Workspaceドメイン | 誰が設定するか | ツールの網羅範囲 |
|---|---|---|---|---|
| **OAuth client**(下記) | ✅ | ✅ | 各顧客が自分自身のGoogle Cloud OAuth clientを作成する | 全19個 |
| **サービスアカウント + ドメイン全体委任** | ❌ | ✅ | ドメインのスーパー管理者が1つのclient idを認可する | 全19個 |
| **Apps Scriptブリッジ** | ✅ | ✅(管理者がApps Scriptを無効化していない限り) | エンドユーザーが自分のアカウントにスクリプトをデプロイする | Gmail / Calendar / Sheetsのみ |

複数が設定されている場合、優先順位はサービスアカウント → OAuth vault →
Apps Scriptブリッジの順です。ブリッジはカバーするツールが最も少ないため
最後に配置されています。実際に有効になっているソースは`google_status`が
表示します。

認証情報を必要としない2つの経路については
[google-no-oauth-client.md](../google-no-oauth-client.md)に記載しています。AI 社員ごとに同僚として使わせる方法は[AI 社員ごとに別の Google アカウントを使う](#ai-社員ごとに別の-google-アカウントを使う会社の委任未リリース)を参照してください。

## AI 社員ごとに別の Google アカウントを使う（会社の委任、未リリース）

既定では、すべての AI 社員が下で設定する 1 つの Google アカウント（会社の共用アカウント）を使います。Google Workspace の顧客は、社員ごとに指定した同僚として使わせることもできます。営業アシスタントは `alice@customer.com` のメールボックスで読んで下書きし、日程アシスタントは `bob@customer.com` のカレンダーを扱います。ドメイン全体の委任を設定したサービスアカウント 1 つを全員で使い、社員ごとに代わりに動く同僚を指定します。

### 必要なもの

- Google Workspace のドメインと、1 つのクライアント ID を認可できる特権管理者（Google 管理コンソール → セキュリティ → アクセスとデータ管理 → API の制御 → ドメイン全体の委任を管理）。認可するスコープはダッシュボードに表示されるリストと完全に同じにします。
- 資格情報の保護をシステムのキーチェーンに移してあること（「セキュリティ」ページ、[セキュリティ防御](../../features/ja-JP/05-security-defense.md#特定の人の認証情報をシステムのキーチェーンに置く未リリース)を参照）。サービスアカウントの鍵はこの保護鍵で封印されるので、AI 社員が読めるファイルにはなりません。macOS と Windows で使えます。Linux と Docker には対応するキーチェーンがまだなく、同僚として使うことは拒否されます。

### 設定手順

1. **セキュリティ → 資格情報の保護 → システムのキーチェーンに移す。**
2. **連携 → Google → 会社の委任 → 鍵ファイルを取り込む。** サービスアカウントの JSON 鍵を選びます。gateway が確認して封印し、`<home>/protected/google-service-account.sealed`（所有者のみ読める）に保存します。取り込み後は元のファイルを削除してください。パネルに特権管理者へ渡すクライアント ID とスコープのリストが表示されます。
3. **委任できるドメイン。** これらのドメインの同僚だけを指定できます（ドメインが完全に一致する場合のみ、サブドメインは不可）。空欄なら共用サービスアカウントの `subject` と同じドメインだけ、どちらもなければ誰も指定できません。
4. **社員ページ → 接続 → 変更 → 同僚として利用（ドメイン委任）**で同僚の会社メールアドレスを入れて保存します。
5. **接続テスト**は全スコープでトークンを取得し、Gmail が同じアドレスを返すか確認してから、各サービスのスコープで発行を確認します。Gmail・カレンダー・ドライブ・Tasks は読み取り専用エンドポイントを確認します。Sheets・Docs・Slides・Forms はアクセス可能なサンプルを探し、そのサービスのリソースを実際に GET します。サンプルがなければ未検証のままで、トークン発行だけをデータ取得成功とは扱いません。何も送信せず、キャッシュも使わず書き込みません。

「設定済み・接続テストが必要」はローカル設定の確認であり、鍵の復号や Google の承認を証明しません。委任ツールの呼び出しごとに gateway が現在の設定、要求者の権限、アカウント状態を再確認し、発行結果のみをキャッシュします。

同じ操作は RPC でもできます（すべて管理者）：`google.delegation.get`、`google.delegation.import_key`、`google.delegation.remove_key`、`google.delegation.settings_set`、`agents.google_identity_set`、`google.identity.test`。`agents.google_identity_get` にはその社員の閲覧権限が必要です。

### 設定値

```toml
# agent.toml（管理者が設定。社員は Write/Edit で変更できません）
[google]
identity = "delegate:alice@customer.com"  # home | none | delegate:<email>
fallback = "none"           # none（既定）| home
requesters = "bound_people" # bound_people（既定）| owner_only | anyone

# config.toml
[integrations.google_identity]
default_identity = "home"   # [google] のない社員が使うもの：home（既定）| none
allowed_subject_domains = ["customer.com"]
```

`[google]` のない社員は `default_identity` を使うので、既存のインストールの動作は変わりません。`none` は 19 個の Google ツールを隠して呼び出しも拒否します。不正な値は共用アカウントへ切り替えず拒否します。Enterprise では `owner` と `user:<id>` を使えますが、有効な直接 Owner と本人の明示的な同意が必要です。[個人 Google 接続（未リリース）](google-personal-connections.md)を参照してください。

### この社員を通じて誰がアカウントを使えるか

同僚のメールボックスは個人のデータなので、社員が使うのは一部の依頼者の場合だけです（`requesters`）：

| 設定 | 許可される依頼者 |
|---|---|
| `bound_people`（既定） | この社員に紐づく人（権限は問わない）と管理者。ダッシュボード、WebChat、検証済みのチャネル紐付けのいずれでも可。スケジュールやシステムの作業（cron、heartbeat、Agent Mail）。これらの人から委任された作業 |
| `owner_only` | その同僚本人の DuDuClaw アカウントとスケジュール作業 |
| `anyone` | この社員と会話できる人（RPC で設定するときは `confirm_anyone: true` が必要） |

Web サイトの公開訪問者（WebChat ウィジェット、Odoo ライブチャット）はどの設定でも拒否されます。アカウントに紐づかないチャネルの発言者も、`anyone` でなければ拒否されます。

### 使えないとき

ツールの呼び出しは失敗し、理由と対処できる人を伝えます。黙って共用アカウントに切り替えることはありません。理由は次のとおりです：呼び出し元が検証済みの社員ではない。依頼者に権限がない。メールアドレスが許可されたドメインにない。同僚の DuDuClaw アカウントが停止または退職済み。資格情報の保護が準備できていない（保護鍵が差し替えられた場合は封印済みの鍵が開けないので、保護をリセットして鍵を取り込み直す）。鍵が取り込まれていない。Google が委任を拒否した（`unauthorized_client` はたいてい管理コンソールのスコープ不足、`invalid_grant` はアカウントが存在しないか無効）。gateway が動いていない。

「同僚のアカウントが使えないときは会社の共用アカウントを使う」（`fallback = "home"`）にチェックすると、最後のグループ（鍵が未取り込み、同僚の停止、Google の拒否や接続不能）では共用アカウントを使い、結果の冒頭にその旨が付きます。依頼者の拒否、設定の誤り、保護の問題では切り替えません。

### ほかに変わること

- **`google_status`** は最初に、この社員が誰として動いているかを示します。同僚として動いている場合は、共用アカウントの詳細ではなく、今トークンを取得できるかどうかを報告します。
- **Agent Mail** は、届いたメールを担当する社員の身元でメールボックスを読みます。それが同僚の場合、メールボックスのページは管理者とその同僚だけが見られます（動作の変更。共用アカウントのメールボックスは従来どおりマネージャー以上）。メールボックスはインストールごとに 1 つのままです。
- **外部の Google MCP サーバー**（`bearer_token = "oauth://google"`）は HTTP マウントで社員が選択したアカウントを使います。権限が通らない Google マウントだけを省き、ほかの設定済みサーバーは残します。詳しくは次節を参照してください。
- **監査**：gateway がトークンを発行または拒否するたびに `tool_calls.jsonl` に `google_token` の行（社員、身元、アカウント、依頼者の種類、ツール、拒否コード）を書き、同僚の身元で Google ツールを呼ぶたびに `google_call` の行を書きます。どちらにもトークンは含まれません。社員の身元を変えると、アカウントの監査ログに `agent.google_identity_set`、`security_audit.jsonl` に `agent_google_identity_set` が残ります。

### 社員専用 API key と Google MCP マウント（未リリース）

検証済みの社員専用 API key に `google:read` または `google:write` が明示されている場合、閉じた 19 個の Google ツールのうち対応するものを使えます。`admin` だけでは代用できず、実在する社員と読める設定が必要です。一般の外部キーに付与できる権限やほかの連携は変わりません。OAuth 認証からこの実行用の証明は作られません。社員に紐づかない正規の内部カスタム連携キーは、従来の Home 経路を保ちます。

社員の許可／拒否ツール、動作ルール、PolicyKernel、期限付き権限、承認要求は引き続き適用されます。承認待ちの間も、受理した社員、ツール、最終引数は固定されます。キーの失効、社員の削除、権限変更は拒否となり、プロセスの別の身元や別アカウントには切り替わりません。gateway が Google 操作を実行し、外部 MCP クライアントにはサイズ制限付きの結果だけを返します。Google bearer は返しません。

キーが証明するのは社員であり、人間の依頼者ではありません。同僚のアカウントを使う場合は **Unknown** として扱い、`bound_people` と `owner_only` は Home fallback があっても拒否します。操作員が明示的に `anyone` を設定した場合に限って使えます。公開 Web 訪問者は引き続き拒否されます。

社員の HTTP MCP マウントでは `bearer_token = "oauth://google"` を gateway 内で社員のアカウントに解決します。実際の呼び出しごとに依頼者、選択アカウント、ドメイン、同僚の状態、資格情報保護を確認し、接続の再利用時も省略しません。HTTP bearer を使わない stdio の同僚アカウントマウントは省きます。設定に従って本当に Home fallback を使った呼び出しには別の説明テキストを付けます。同僚のアカウントで成功した呼び出しに Home 利用とは表示しません。元の JSON、秘匿化、CCR の構造は保ちます。

Agent Mail は最後に完了したポーリング時刻、アカウント利用不可件数、接続エラー件数も記録します。アカウント利用不可や状態ファイルの読取失敗は空のメールボックスと区別し、未実行を「成功・0 件」とは扱いません。インストール単位のメールボックスと既存の閲覧制限は変わりません。

専用 gateway 実行経路では GET／HEAD 以外の REST 要求は一度だけ送信し、通信エラー、429、5xx で書き込みを自動再送しません。GET／HEAD は一度の再試行を保ちます。この制限は専用実行経路に適用され、従来の Home 呼び出し元の共通関数の動作は変わりません。

### 制限

- `gmail.compose` があれば、アクセストークンを持つ人はメールを送れます。ツールは下書きを作るだけですが、制限のないシェルを持つ社員は、自分の同僚向けに発行されたトークンで Google を直接呼べます。
- シェルを持つ社員は gateway と同じ OS ユーザーで動きます。[セキュリティ防御](../../features/ja-JP/05-security-defense.md#特定の人の認証情報をシステムのキーチェーンに置く未リリース)のキーチェーンの制限を参照してください。
- 実際の Workspace ではまだ検証していません：管理コンソールの実際の流れ、Google の実際のエラー文字列、Gmail を無効にした組織部門、Google 自身のリモート MCP エンドポイントが委任トークンを受け付けるかどうか。

## 前提条件: Google OAuth clientを作成する

自分自身のGoogle OAuth clientを用意する必要があります(DuDuClawは共有の
認証情報を同梱しません)。初回セットアップ:

1. [Google Cloud Console → Credentials](https://console.cloud.google.com/apis/credentials)
   ページを開く(先にプロジェクトを作成／選択)。
2. プロジェクトに対して以下の8つのAPIを有効化する(APIs & Services →
   Library)。またはコマンド1つで:

   ```bash
   gcloud services enable gmail.googleapis.com calendar-json.googleapis.com \
     sheets.googleapis.com drive.googleapis.com docs.googleapis.com \
     slides.googleapis.com forms.googleapis.com tasks.googleapis.com \
     --project=PROJECT_ID
   ```
3. OAuth同意画面を設定する(ExternalまたはInternal)。アプリが「Testing」の
   ままであれば、自分自身のGoogleアカウントをテストユーザーとして追加する。
4. **Web application**タイプの**OAuth client ID**を作成する。
5. **Authorized redirect URIs**に、正確に以下を追加する:

   ```
   http://localhost:18889/api/mcp/oauth/callback
   ```

   18889はgatewayのデフォルトポートです。別のポートで実行している場合
   (`DUDUCLAW_PORT`)は、そのポートを代わりに登録してください。ダッシュボードの
   セットアップ手順には、gatewayが実際にリッスンしているポートから導出された
   正確なURIが表示されます。ここが一致していないと、失敗は静かに起こります。
   Googleはブラウザを何も待ち受けていないポートにリダイレクトするため、
   トークンが届くことはなく、ページは「未接続」のままになります。

6. 生成された**Client ID**と**Client secret**をコピーする。

要求されるスコープは以下のとおりです:

```
https://www.googleapis.com/auth/gmail.readonly
https://www.googleapis.com/auth/gmail.compose
https://www.googleapis.com/auth/calendar.events
https://www.googleapis.com/auth/spreadsheets
https://www.googleapis.com/auth/drive.readonly
https://www.googleapis.com/auth/documents
https://www.googleapis.com/auth/presentations.readonly
https://www.googleapis.com/auth/forms.body.readonly
https://www.googleapis.com/auth/forms.responses.readonly
https://www.googleapis.com/auth/tasks
https://www.googleapis.com/auth/userinfo.email
```

> **スコープ変更(v1.45):** Sheetsツールのために`spreadsheets`スコープが
> 追加されました。v1.45より前に接続されたGoogleアカウントは、トークンが
> このスコープの追加前に発行されたものであるため、Sheets APIから`403`を
> 受け取ります。`google_status`は不足しているスコープを警告し、ダッシュボードから
> 再接続するとフルセットのスコープで再度同意を求められます。
>
> **スコープ変更(v1.47):** 新しいネイティブツールのために、Drive
> (`drive.readonly`)、Docs(`documents`)、Slides
> (`presentations.readonly`)、Forms(`forms.body.readonly`、
> `forms.responses.readonly`)、Tasks(`tasks`)が追加されました。上記と
> 同じルールが適用されます。古いトークンは`403`と再認証の案内を受け取ります。
> Integrations → Googleから再接続して再度同意してください。

## ダッシュボードから接続する

1. **Integrations → Google**(`/manage/integrations?tab=google`)に移動する。
2. Client IDとClient secretを貼り付け、**Connect Google**をクリックする。
3. Googleの同意ウィンドウが開きます。アクセスを承認すると、ウィンドウに
   成功が表示され、ダッシュボードは**Google is connected**に切り替わります。

client認証情報は永続化されます(secretは保存時に暗号化)。これによりアクセス
トークンを自動的に更新でき、後で再認可する際にsecretを再入力する必要も
ありません。

切断するには、接続済み画面で**Disconnect**をクリックします。保存されている
client認証情報は保持されるためワンクリックで再接続できますが、アクセス
トークンは削除されます。

## 更新(リフレッシュ)の仕組み

Googleはオフラインアクセスが要求された場合にのみrefresh tokenを発行するため、
接続フローはGoogle向けに自動的に`access_type=offline&prompt=consent`を
付与します。アクセストークンが期限切れになると、`get_valid_google_token`が
保存されているclient認証情報でrefresh grantを実行し、新しいトークンを保存して
処理を継続します。更新ができない場合(refresh tokenがない、または保存された
認証情報が見つからない)、ツールは明確なメッセージを返し、Integrations →
Googleページに戻って再接続するよう案内します。

## スコープ変更後の再認可

この統合が提供される前(古いスコープセット)に認可されたトークンは、新しい
書き込みAPIから`403`を受け取ります。ツールはこれを検知し、付与すべき
スコープの一覧を含む案内を返します。Integrations → Googleから再接続すると、
現在のスコープセットで再度同意できます。

## トラブルシューティング

- **「Google is not connected.」**: トークンが保存されていません。
  ダッシュボードから接続してください。
- **`401 Unauthorized`**: 認可が取り消された、または無効です。
  再接続してください。
- **スコープ一覧付きの`403`**: トークンに必要なスコープが不足しています。
  再接続して再同意してください。
- **同意中のRedirect URI不一致**: Google OAuth clientのredirect URIは
  正確に`http://localhost:18889/api/mcp/oauth/callback`である必要があります。

いつでも`google_status`を実行すれば、その場で診断結果を確認できます。
