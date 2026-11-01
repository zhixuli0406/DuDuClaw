# Google Workspace ツールと安全モデル

設定は [Google を接続する](google-setup.md)を参照してください。ネイティブ REST は八サービス・十九ツール、Apps Script は三サービス・状態確認込み八アクションに対応します。連携がオフでもダッシュボードは表示され、ツールは `[integrations] google_workspace = true` で有効になります。共用 OAuth 認証情報は同梱しません。

## 何が使えるようになるか

2つのスコープ(`google:read` / `google:write`)で管理される、19個のエージェント向け
MCPツールです。

| ツール | 分類 | 内容 |
|------|-------|--------------|
| `google_status` | read | 経路別の身分・認証情報を診断。サービス読取の証明にはならず、連携または社員の Google が無効なら非表示。 |
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

## AI 社員ごとに別の Google アカウントを使う（会社の委任、v1.76.0）

既定では、すべての AI 社員が下で設定する 1 つの Google アカウント（会社の共用アカウント）を使います。Google Workspace の顧客は、社員ごとに指定した同僚として使わせることもできます。営業アシスタントは `alice@customer.com` のメールボックスで読んで下書きし、日程アシスタントは `bob@customer.com` のカレンダーを扱います。ドメイン全体の委任を設定したサービスアカウント 1 つを全員で使い、社員ごとに代わりに動く同僚を指定します。

### 必要なもの

- Google Workspace のドメインと、1 つのクライアント ID を認可できる特権管理者（Google 管理コンソール → セキュリティ → アクセスとデータ管理 → API の制御 → ドメイン全体の委任を管理）。認可するスコープはダッシュボードに表示されるリストと完全に同じにします。
- 資格情報の保護をシステムのキーチェーンに移してあること（「セキュリティ」ページ、[セキュリティ防御](../../features/ja-JP/05-security-defense.md#特定の人の認証情報をシステムのキーチェーンに置くv1760)を参照）。サービスアカウントの鍵はこの保護鍵で封印されるので、AI 社員が読めるファイルにはなりません。macOS と Windows で使えます。Linux と Docker には対応するキーチェーンがまだなく、同僚として使うことは拒否されます。

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

`[google]` のない社員は `default_identity` を使うので、既存のインストールの動作は変わりません。`none` は 19 個の Google ツールを隠して呼び出しも拒否します。不正な値は共用アカウントへ切り替えず拒否します。Enterprise では `owner` と `user:<id>` を使えますが、有効な直接 Owner と本人の明示的な同意が必要です。[個人 Google 接続（v1.76.0）](google-personal-connections.md)を参照してください。

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

### 社員専用 API key と Google MCP マウント（v1.76.0）

検証済みの社員専用 API key に `google:read` または `google:write` が明示されている場合、閉じた 19 個の Google ツールのうち対応するものを使えます。`admin` だけでは代用できず、実在する社員と読める設定が必要です。一般の外部キーに付与できる権限やほかの連携は変わりません。OAuth 認証からこの実行用の証明は作られません。社員に紐づかない正規の内部カスタム連携キーは、従来の Home 経路を保ちます。

社員の許可／拒否ツール、動作ルール、PolicyKernel、期限付き権限、承認要求は引き続き適用されます。承認待ちの間も、受理した社員、ツール、最終引数は固定されます。キーの失効、社員の削除、権限変更は拒否となり、プロセスの別の身元や別アカウントには切り替わりません。gateway が Google 操作を実行し、外部 MCP クライアントにはサイズ制限付きの結果だけを返します。Google bearer は返しません。

キーが証明するのは社員であり、人間の依頼者ではありません。同僚のアカウントを使う場合は **Unknown** として扱い、`bound_people` と `owner_only` は Home fallback があっても拒否します。操作員が明示的に `anyone` を設定した場合に限って使えます。公開 Web 訪問者は引き続き拒否されます。

社員の HTTP MCP マウントでは `bearer_token = "oauth://google"` を gateway 内で社員のアカウントに解決します。実際の呼び出しごとに依頼者、選択アカウント、ドメイン、同僚の状態、資格情報保護を確認し、接続の再利用時も省略しません。HTTP bearer を使わない stdio の同僚アカウントマウントは省きます。設定に従って本当に Home fallback を使った呼び出しには別の説明テキストを付けます。同僚のアカウントで成功した呼び出しに Home 利用とは表示しません。元の JSON、秘匿化、CCR の構造は保ちます。

Agent Mail は最後に完了したポーリング時刻、アカウント利用不可件数、接続エラー件数も記録します。アカウント利用不可や状態ファイルの読取失敗は空のメールボックスと区別し、未実行を「成功・0 件」とは扱いません。インストール単位のメールボックスと既存の閲覧制限は変わりません。

専用 gateway 実行経路では GET／HEAD 以外の REST 要求は一度だけ送信し、通信エラー、429、5xx で書き込みを自動再送しません。GET／HEAD は一度の再試行を保ちます。この制限は専用実行経路に適用され、従来の Home 呼び出し元の共通関数の動作は変わりません。

### 制限

- `gmail.compose` があれば、アクセストークンを持つ人はメールを送れます。ツールは下書きを作るだけですが、制限のないシェルを持つ社員は、自分の同僚向けに発行されたトークンで Google を直接呼べます。
- シェルを持つ社員は gateway と同じ OS ユーザーで動きます。[セキュリティ防御](../../features/ja-JP/05-security-defense.md#特定の人の認証情報をシステムのキーチェーンに置くv1760)のキーチェーンの制限を参照してください。
- 実際の Workspace ではまだ検証していません：管理コンソールの実際の流れ、Google の実際のエラー文字列、Gmail を無効にした組織部門、Google 自身のリモート MCP エンドポイントが委任トークンを受け付けるかどうか。


## What was ruled out, and why

**ローカルでAppleScript経由でMail.app／Calendar.appを読む方法。** カレンダーはうまくいっており、すでに出荷済みです（`os_calendar_today`）。1日分の予定を問い合わせるだけなので、クエリが小さく済むからです。メールはそうはいきません。macOS 15上の実際の54,000通が入ったメールボックスで計測したところ、
`Mail.inbox.messages.dateReceived()`は17秒かかり、
`whose({dateReceived: …})`と`whose({readStatus: false})`はどちらも60〜90秒以内に応答を返せませんでした。`inbox.messages`が返す順序も時系列順ではないため、安価なインデックスアクセスでは任意の古いメールが返ってくることになります。Spotlightはメールメッセージをインデックス化していません（`kMDItemKind == 'Mail Message'`は何も返しません）。`~/Library/Mail`を直接読むにはFull Disk Accessに加えて、ドキュメント化されておらずバージョンごとに変わるSQLiteスキーマへの対応が必要です。GmailについてはこちらもApps Scriptを使います。
