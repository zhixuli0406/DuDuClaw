# Google Workspace ツールと安全モデル

設定は [Google を接続する](google-setup.md)を参照してください。ネイティブ REST は九サービス・三十八ツール、Apps Script は三サービス・状態確認込み八アクションに対応します。連携がオフでもダッシュボードは表示され、ツールは `[integrations] google_workspace = true` で有効になります。共用 OAuth 認証情報は同梱しません。

## 何が使えるようになるか

2つのスコープ(`google:read` / `google:write`)で管理される、38個のエージェント向け
MCPツールです。

| ツール | 分類 | 内容 |
|------|-------|--------------|
| `google_status` | read | 経路別の身分・認証情報を診断。サービス読取の証明にはならず、連携または社員の Google が無効なら非表示。 |
| `gmail_search` | read | Gmailのクエリ構文(`from:… is:unread`など)でメールボックスを検索。送信者／件名／日付／スニペットを返す。 |
| `gmail_read` | read | 1件のメッセージを全文読み取る: ヘッダー、プレーンテキスト本文(長い場合は切り詰め)、添付ファイル一覧(ファイル名とサイズのみ、ダウンロードは行わない)。 |
| `google_chat_spaces` | read | 接続したユーザーのスペース、グループチャット、DM を一覧表示。Chat 読み取りの有効化が必要。 |
| `google_chat_messages` | read | 最近の Chat メッセージを検査済みの参照データとして読み取り。添付は名前のみ。 |
| `google_chat_send` | send | 依頼した認証済みメンバー本人のアカウントで既存スペースにテキストを送信。`[google] chat_send = true` まで無効で、送信のたびに本人の確認が必要。[本人として Google Chat を送信する](google-personal-connections.md#本人として-google-chat-を送信する)を参照。 |
| `gmail_send_draft` | send | 毎回確認して承認済みの下書きを送信。 |
| `calendar_delete_event` | delete | 毎回確認して指定バージョンの予定を削除。通知の既定はなし。 |
| `gmail_create_draft` | write | Gmailの**下書き**を作成する。送信は行わない。送信には `gmail_send_draft` の個別確認が必要。 |
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
| `docs_append` | write | Docの**末尾**にテキストを追記する。 |
| `docs_create` | write | マイドライブのルートに文書を作成。本文は任意。 |
| `docs_replace_text` | write | 文書内の一致する文字列を置換。既定は大文字・小文字を区別。 |
| `sheets_create` | write | スプレッドシートを作成。シート名も指定可能。 |
| `sheets_update` | write | 最大10,000セルを更新。既定はRAW。 |
| `calendar_update_event` | write | 予定を編集。参加者のカレンダーに影響するため、常に send 承認ルールを適用。 |
| `slides_read` | read | プレゼンテーションのテキストをスライドごとに読み取る(図形、グループ化された図形、表のセル)。 |

この5つの書き込みツールと Gmail の返信フィールド（`thread_id`、`in_reply_to`、`references`）にはネイティブ REST 接続が必要です。Apps Script Bridge では非対応のツールと返信フィールドを非表示にし、バックエンドの変更時に `notifications/tools/list_changed` を送信します。Gmail の `to` と `cc` はカンマ区切りのメールアドレスを指定でき、各フィールドの上限は1,000文字です。各アドレスを検証し、制御文字は拒否します。

書き込みツールの結果には Google が実際に返した内容だけを示します。`calendar_create_event` は追加した参加者の人数だけを示し（いない場合は「Attendees: none」）、誰かに通知したとは書きません。イベント ID が返らない場合はエラーです。`gmail_create_draft` で下書き ID が返らない場合もエラーです。`sheets_append` の行数・セル数は Google が返した場合だけ表示します。Apps Script 経由では行は常にスプレッドシートの**最初のシート**に追加され、結果にはそのことと、指定した範囲が使われなかったことを示します。Apps Script の応答は、その操作の成功時の形式を満たす場合だけ成功とみなし、空の応答・`ok: false`・あらゆる `error` 項目は理由付きのツールエラーになります。

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

上表は既存のツールです。新しい九つのドライブ書き込みツールのパラメーター、操作スイッチ、効果クラスは [Drive 書き込みリファレンス](google-mcp.md#クラウドドライブへの書き込みツール)を参照してください。新ツールにはネイティブ REST と `google:write` が必要です。既存の Docs／Sheets 書き込み六ツールは従来の規則を維持します。

### 安全設計

- **送信は毎回確認が必要です。** `gmail_create_draft` は下書きを保存し、`gmail_send_draft` は新しいカードを人が確認したときだけ送信します。
- **読み取りは読み取りのままです。** read分類のツールはGmailやCalendarの
  内容を一切変更できません。
- **Forms と Slides は読み取り専用です。** Drive 書き込みには個別に制御する九つのツールがあり、四つの操作スイッチは既定でオフです。[ツールとフォルダ規則](google-mcp.md#クラウドドライブへの書き込みツール)を参照してください。
- **書き込み権限を分離します。** 通常の Drive 読み取りは `drive.readonly` を使います。独立した書き込み接続は `drive`、`documents`、`spreadsheets` を要求し、資格情報は gateway 内に保持します。録画の取り込みは別途 `drive.file`、Slides は `presentations.readonly` を使います。
- **オプションの承認ゲート。** より慎重にしたい場合は、書き込み系ツールを
  エージェントの`agent.toml [capabilities] approval_required_tools`に
  リストアップすることで、下書き・イベント・スプレッドシートへの書き込みの
  たびにHITL(human-in-the-loop)承認を待たせることができます。

  ```toml
  [capabilities]
  approval_required_tools = ["gmail_create_draft", "calendar_create_event", "sheets_append", "gtasks_create", "gtasks_complete", "docs_append", "docs_create", "docs_replace_text", "sheets_create", "sheets_update", "calendar_update_event"]
  ```

## Google コンテンツの作成と編集

以下のネイティブツールは既存の `documents`、`spreadsheets`、`calendar.events` OAuth スコープを使用します。`google:write` が必要で、`google:read` のみの社員には表示されず、呼び出しも拒否されます。個人接続には各サービスのスコープも必要です。ドメイン全体の委任でも同じ名前を使います。新ツールと返信下書きにはネイティブ REST 接続が必要です。Apps Script Bridge は対応しません。

| ネイティブツール | 引数と動作 |
|---|---|
| `docs_create` | 必須の `title`、任意の `body`。マイドライブのルートに文書を作成します。 |
| `docs_replace_text` | `document_id`、空でない `find`、`replace`。`match_case` は既定で `true`。一致する全文字列を置換し、`occurrencesChanged` を返します。 |
| `sheets_create` | 必須の `title`、任意の `sheet_names` 配列（最大100件）。マイドライブのルートにスプレッドシートを作成します。 |
| `sheets_update` | `spreadsheet_id`、A1形式の `range`、二次元 `values` 配列。1回最大10,000セル。`value_input_option` の既定は `RAW`。数式や日付を解釈する場合は `USER_ENTERED` を指定します。 |
| `calendar_update_event` | `event_id`、任意の `calendar_id`（既定 `primary`）、変更するフィールドを1つ以上指定します：`summary`、`description`、`location`、RFC3339形式の `start`／`end`、メールアドレス配列の `attendees`（`[]` で参加者を削除）。すべての呼び出しに `calendar_create_event` と同じ `send` 動作ルールと承認経路を適用します。`sendUpdates=none` でも参加者のカレンダーが変わる可能性があります。通知パラメーターの既定値は引き続き `none` で、`notify_attendees=true` の場合は `all` です。 |

テキストは最大20,000文字、タイトルは1,000文字、シート名は100文字、A1範囲は1,000文字です。上限を超える入力は動作ルール・承認・ダイジェストの生成前に拒否され、切り詰められません。結果の `structuredContent` は、後続の呼び出しに使える `documentId` または `spreadsheetId` と `url`、予定の場合は `id` と `htmlLink` を含みます。表示する Google の応答は注入検査を受けます。本文付き文書の作成は2回のリクエストを使うため、本文の挿入が失敗した場合は再試行する前にドライブで作成済み文書を確認してください。

既存スレッドの返信下書きには、`gmail_create_draft` の引数に `thread_id` と `in_reply_to`（元メールの RFC `Message-ID`、例：`<original@example.com>`）を追加します。件名は元メールと合わせ、必要に応じて返信の接頭辞を付けます。任意の `references` は元メールの Message-ID を含む References チェーンで、既定は `in_reply_to` です。Gmail の `threadId`、`In-Reply-To`、`References` が設定されます。呼び出し側がヘッダーを渡すため、作成専用接続でも読み取りスコープを追加する必要はありません。下書きの保存のみで、送信しません。

### 自社運用の Google MCP から移行する

hcatwn-google などの自社運用サーバーを置き換える場合は、スキル内の名前を下表で対応させてください。別名の登録は行いません。移行前に各サーバーの引数スキーマも確認してください。

| 自社運用ツール名 | DuDuClaw ネイティブ名 |
|---|---|
| `calendar_list` | `calendar_list_events` |
| `gmail_draft` | `gmail_create_draft` |

新しい Docs、Sheets、予定編集のフローには上のネイティブ名を使います。他の自社運用サーバーでは名前が異なる場合があるため、そのツール一覧を確認してください。Google 公式のリモート MCP は Google の名前を保持する別の連携です。

## 個人の Google Chat 会話を読む（オプトイン）

`google_chat_spaces` は接続したユーザーが参加するスペース、グループチャット、DM を一覧表示します。リソース名、表示名、種類、Google が返す場合は最終活動時刻を含みます。`limit` は既定 20（1–50）、`filter` は `SPACE`、`GROUP_CHAT`、`DIRECT_MESSAGE` のいずれかです。

`google_chat_messages` は厳密に検証した `spaces/...` の最近のメッセージを新しい順に読みます。`limit` は既定 30（1–100）、任意の `since` は RFC3339、`thread` は同じスペース内のスレッドリソース名です。送信者、作成時刻、本文（1件最大 2,000 文字）、スレッド、添付ファイル名を返し、添付はダウンロードしません。表示名がない送信者は `users/...` で示し、People API や追加のメンバー権限は使用しません。応答全体は最大 30,000 文字です。

1. 管理者がダッシュボードの「管理 → 連携 → Google」タブで **Google Chat 読み取り**を有効にします（`[google] chat_read = true`、既定 false）。
2. ユーザーは「自分の接続」から自分の Google アカウントを再接続し、Chat の読み取り権限を両方選択します。追加スコープは `https://www.googleapis.com/auth/chat.spaces.readonly` と `https://www.googleapis.com/auth/chat.messages.readonly`。無効時は既存ユーザーに追加同意を求めません。
3. OAuth クライアントの Google Cloud プロジェクトで **Google Chat API** を有効化し、**Chat アプリ**を構成します。ユーザー認証でも必要です。Workspace 管理者によるアプリ許可が必要な場合もあります。

Gmail 読み取りと同じ社員・アカウントの紐付け、スコープ、承認、私有データの送信制御を使用します。既存の身分ポリシーが選択したアカウントを使い、任意のユーザー指定は受け付けません。Apps Script はこの Chat 読み取りに対応しません。DM とメンションを受信する Chat ボットとは別の機能です。無効化すると既存トークンでの新たな読み取りも拒否しますが、Google 側の同意を取り消すものではありません。

応答は信頼しない参照データとして包み、グループ履歴と同じ注入検査を行います。1件でも検出すると応答全体を `not available` として拒否します。監査にはスペース ID と件数のみを記録し、本文は残しません。固定データ・ローカルテストは実アカウントでの成功を保証しないため、納品前に実際の同意・読み取りを検証してください。


## AI 社員ごとに別の Google アカウントを使う（会社の委任、v1.76.0）

既定では、すべての AI 社員が下で設定する 1 つの Google アカウント（会社の共用アカウント）を使います。Google Workspace の顧客は、社員ごとに指定した同僚として使わせることもできます。営業アシスタントは `alice@customer.com` のメールボックスで読んで下書きし、日程アシスタントは `bob@customer.com` のカレンダーを扱います。ドメイン全体の委任を設定したサービスアカウント 1 つを全員で使い、社員ごとに代わりに動く同僚を指定します。

### 必要なもの

- Google Workspace のドメインと、1 つのクライアント ID を認可できる特権管理者（Google 管理コンソール → セキュリティ → アクセスとデータ管理 → API の制御 → ドメイン全体の委任を管理）。認可するスコープはダッシュボードに表示されるリストと完全に同じにします。
- 資格情報の保護に対応する鍵の保存方式を使っていること（「セキュリティ」ページ、[セキュリティ防御](../../features/ja-JP/05-security-defense.md#特定の人の認証情報をシステムのキーチェーンに置くv1760)を参照）。サービスアカウントの鍵はこの保護鍵で封印されるので、AI 社員が読めるファイルにはなりません。macOS、Windows、および person-credentials オーバーレイで起動した Docker（[Docker ガイド](docker.md#74-個人アカウントperson-credentials-オーバーレイ)）で使えます。[ネイティブ Linux の保護システムサービス](linux-protected-service.md)も利用できます。通常の Docker インストールと Linux ユーザーサービスでは、同僚として使うことは拒否されます。

### 設定手順

1. **セキュリティ → 資格情報の保護 → システムのキーチェーンに移す。**
2. **連携 → Google → 会社の委任 → 鍵ファイルを取り込む。** サービスアカウントの JSON 鍵を選びます。gateway が確認して封印し、`<home>/protected/google-service-account.sealed`（所有者のみ読める）に保存します。取り込み後は元のファイルを削除してください。パネルに特権管理者へ渡すクライアント ID とスコープのリストが表示されます。
3. **委任できるドメイン。** これらのドメインの同僚だけを指定できます（ドメインが完全に一致する場合のみ、サブドメインは不可）。空欄なら共用サービスアカウントの `subject` と同じドメインだけ、どちらもなければ誰も指定できません。
4. **社員ページ → 接続 → 変更 → 同僚として利用（ドメイン委任）**で同僚の会社メールアドレスを入れて保存します。
5. **接続テスト**は各スコープを個別に確認し、Gmail が利用可能なら同じアドレスを返すか確認してから、各サービスのスコープで発行を確認します。Gmail・カレンダー・ドライブ・Tasks は読み取り専用エンドポイントを確認します。Sheets・Docs・Slides・Forms はアクセス可能なサンプルを探し、そのサービスのリソースを実際に GET します。サンプルがなければ未検証のままで、トークン発行だけをデータ取得成功とは扱いません。何も送信せず、キャッシュも使わず書き込みません。

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

`[google]` のない社員は `default_identity` を使うので、既存のインストールの動作は変わりません。`none` は 29 個の Google ツールを隠して呼び出しも拒否します。不正な値は共用アカウントへ切り替えず拒否します。Enterprise では `owner` と `user:<id>` を使えますが、有効な直接 Owner と本人の明示的な同意が必要です。[個人 Google 接続（v1.76.0）](google-personal-connections.md)を参照してください。

### この社員を通じて誰がアカウントを使えるか

同僚のメールボックスは個人のデータなので、社員が使うのは一部の依頼者の場合だけです（`requesters`）：

| 設定 | 許可される依頼者 |
|---|---|
| `bound_people`（既定） | この社員に紐づく人（権限は問わない）と管理者。ダッシュボード、WebChat、検証済みのチャネル紐付けのいずれでも可。スケジュールやシステムの作業（cron、heartbeat、Agent Mail）。これらの人から委任された作業 |
| `owner_only` | その同僚本人の DuDuClaw アカウントとスケジュール作業 |
| `anyone` | この社員と会話できる人（RPC で設定するときは `confirm_anyone: true` が必要） |

Web サイトの公開訪問者（WebChat ウィジェット、Odoo ライブチャット）はどの設定でも拒否されます。アカウントに紐づかないチャネルの発言者も、`anyone` でなければ拒否されます。

**Google Chat の発言者（v1.82）**：管理者が **ユーザー → 本人確認を設定** で Chat のユーザー ID を紐付けた場合、または——管理者が既定でオフの任意設定 `[google] chat_identity_from_account`（管理 → 連携 → Google）をオンにしている場合に限り——本人が **自分の接続** で自分の Google アカウントを連携した場合（Google アカウント ID で照合。この自動照合は**実際の Google Workspace ではまだ検証されていません**。ご自身の Workspace で 2 つの ID が一致することを確認していない限り、設定はオフのままにしてください）、ここではその本人として扱います。メールアドレスでの照合はしません。v1.82 より前はすべての Chat 発言者が未紐付けで、`anyone` でしか通りませんでした。詳しくは [ダッシュボード設定 → ユーザー → 本人確認を設定](dashboard-settings.md#ユーザー--本人確認を設定メッセージアプリの-idv182) を参照してください。

### 使えないとき

ツールの呼び出しは失敗し、理由と対処できる人を伝えます。黙って共用アカウントに切り替えることはありません。理由は次のとおりです：呼び出し元が検証済みの社員ではない。依頼者に権限がない。メールアドレスが許可されたドメインにない。同僚の DuDuClaw アカウントが停止または退職済み。資格情報の保護が準備できていない（保護鍵が差し替えられた場合は封印済みの鍵が開けないので、保護をリセットして鍵を取り込み直す）。鍵が取り込まれていない。Google が委任を拒否した（`unauthorized_client` はたいてい管理コンソールのスコープ不足、`invalid_grant` はアカウントが存在しないか無効）。gateway が動いていない。

「同僚のアカウントが使えないときは会社の共用アカウントを使う」（`fallback = "home"`）にチェックすると、最後のグループ（鍵が未取り込み、同僚の停止、Google の拒否や接続不能）では共用アカウントを使い、結果の冒頭にその旨が付きます。依頼者の拒否、設定の誤り、保護の問題では切り替えません。

### ほかに変わること

- **`google_status`** は最初に、この社員が誰として動いているかを示します。同僚として動いている場合は、共用アカウントの詳細ではなく、今トークンを取得できるかどうかを報告します。
- **Agent Mail** は、届いたメールを担当する社員の身元でメールボックスを読みます。それが同僚の場合、メールボックスのページは管理者とその同僚だけが見られます（動作の変更。共用アカウントのメールボックスは従来どおりマネージャー以上）。メールボックスはインストールごとに 1 つのままです。
- **外部の Google MCP サーバー**（`bearer_token = "oauth://google"`）は HTTP マウントで社員が選択したアカウントを使います。権限が通らない Google マウントだけを省き、ほかの設定済みサーバーは残します。詳しくは次節を参照してください。
- **監査**：gateway がトークンを発行または拒否するたびに `tool_calls.jsonl` に `google_token` の行（社員、身元、アカウント、依頼者の種類、ツール、拒否コード）を書き、同僚の身元で Google ツールを呼ぶたびに `google_call` の行を書きます。どちらにもトークンは含まれません。社員の身元を変えると、アカウントの監査ログに `agent.google_identity_set`、`security_audit.jsonl` に `agent_google_identity_set` が残ります。

### 社員専用 API key と Google MCP マウント（v1.76.0）

検証済みの社員専用 API key に `google:read` または `google:write` が明示されている場合、閉じた 29 個の Google ツールのうち対応するものを使えます。`admin` だけでは代用できず、実在する社員と読める設定が必要です。一般の外部キーに付与できる権限やほかの連携は変わりません。OAuth 認証からこの実行用の証明は作られません。社員に紐づかない正規の内部カスタム連携キーは、従来の Home 経路を保ちます。

社員の許可／拒否ツール、動作ルール、PolicyKernel、期限付き権限、承認要求は引き続き適用されます。承認待ちの間も、受理した社員、ツール、最終引数は固定されます。キーの失効、社員の削除、権限変更は拒否となり、プロセスの別の身元や別アカウントには切り替わりません。gateway が Google 操作を実行し、外部 MCP クライアントにはサイズ制限付きの結果だけを返します。Google bearer は返しません。

キーが証明するのは社員であり、人間の依頼者ではありません。同僚のアカウントを使う場合は **Unknown** として扱い、`bound_people` と `owner_only` は Home fallback があっても拒否します。操作員が明示的に `anyone` を設定した場合に限って使えます。公開 Web 訪問者は引き続き拒否されます。

社員の HTTP MCP マウントでは `bearer_token = "oauth://google"` を gateway 内で社員のアカウントに解決します。実際の呼び出しごとに依頼者、選択アカウント、ドメイン、同僚の状態、資格情報保護を確認し、接続の再利用時も省略しません。HTTP bearer を使わない stdio の同僚アカウントマウントは省きます。設定に従って本当に Home fallback を使った呼び出しには別の説明テキストを付けます。同僚のアカウントで成功した呼び出しに Home 利用とは表示しません。元の JSON、秘匿化、CCR の構造は保ちます。

Agent Mail は最後に完了したポーリング時刻、アカウント利用不可件数、接続エラー件数も記録します。アカウント利用不可や状態ファイルの読取失敗は空のメールボックスと区別し、未実行を「成功・0 件」とは扱いません。インストール単位のメールボックスと既存の閲覧制限は変わりません。

専用 gateway 実行経路では GET／HEAD 以外の REST 要求は一度だけ送信し、通信エラー、429、5xx で書き込みを自動再送しません。GET／HEAD は一度の再試行を保ちます。この制限は専用実行経路に適用され、従来の Home 呼び出し元の共通関数の動作は変わりません。

### 制限

- `gmail.compose` があれば、アクセストークンを持つ人はメールを送れます。下書き作成は保存のみで、送信ツールには毎回確認が必要ですが、制限のないシェルを持つ社員は、自分の同僚向けに発行されたトークンで Google を直接呼べます。
- シェルを持つ社員は gateway と同じ OS ユーザーで動きます。[セキュリティ防御](../../features/ja-JP/05-security-defense.md#特定の人の認証情報をシステムのキーチェーンに置くv1760)のキーチェーンの制限を参照してください。
- 実際の Workspace ではまだ検証していません：管理コンソールの実際の流れ、Google の実際のエラー文字列、Gmail を無効にした組織部門、Google 自身のリモート MCP エンドポイントが委任トークンを受け付けるかどうか。


## What was ruled out, and why

**ローカルでAppleScript経由でMail.app／Calendar.appを読む方法。** カレンダーはうまくいっており、すでに出荷済みです（`os_calendar_today`）。1日分の予定を問い合わせるだけなので、クエリが小さく済むからです。メールはそうはいきません。macOS 15上の実際の54,000通が入ったメールボックスで計測したところ、
`Mail.inbox.messages.dateReceived()`は17秒かかり、
`whose({dateReceived: …})`と`whose({readStatus: false})`はどちらも60〜90秒以内に応答を返せませんでした。`inbox.messages`が返す順序も時系列順ではないため、安価なインデックスアクセスでは任意の古いメールが返ってくることになります。Spotlightはメールメッセージをインデックス化していません（`kMDItemKind == 'Mail Message'`は何も返しません）。`~/Library/Mail`を直接読むにはFull Disk Accessに加えて、ドキュメント化されておらずバージョンごとに変わるSQLiteスキーマへの対応が必要です。GmailについてはこちらもApps Scriptを使います。


## 組織の承認と ID の選択（v1.85）

ドメイン全体の委任では、ツールが属するサービス（Gmail、カレンダー、Docs、Sheets、Drive、Slides、Forms、Tasks）のスコープだけを要求します。トークンはサービスアカウント、代理対象、並べ替え・重複除去したスコープ集合ごとにキャッシュします。別サービスの未承認スコープが既存サービスを停止させることはありません。接続テストは各スコープの承認結果とサービスへのアクセス結果を分けて表示します。発行成功だけでは API やサンプルファイルへのアクセスを証明しません。

`[google] chat_read = true` の場合、委任でも `chat.spaces.readonly` と `chat.messages.readonly`（接頭辞は `https://www.googleapis.com/auth/`）を使用できます。Google 管理コンソールで両方を承認し、Google Chat API を有効にしてください。設定の既定値はオフのままです。設定ウィザードでは Chat 読み取りを選択済みで表示しますが、管理者が保存するまで書き込みません。接続済みでも Chat のスコープまたはローカル用途が不足するメンバーには「Google Chat を追加するには再接続してください」と表示します。本人名義での Chat 送信には、別途 `chat_send` の許可と送信ごとの本人確認が必要です。[本人として Google Chat を送信する](google-personal-connections.md#本人として-google-chat-を送信する)を参照してください。

AI 社員の担当者に組織の承認を連動させる手順：

1. メンバーを Owner として紐付けます。Owner が複数いる場合は「社員 → 接続」で主担当者を選びます。
2. 「組織の承認：担当者に連動」（`identity = "delegate:owner"`）を選択し、許可ドメインと封印済みサービスアカウント鍵を設定します。

   権限変更後も、実行中の1回の呼び出しは以前の担当者の権限で完了する場合があります。その後の呼び出しでは毎回権限を再確認します。
3. 「連携 → Google → 組織の代理承認」で AI 社員、現在の担当者、ログインメール、確認状態を確認し、各行のメールを承認します。保護モード以外でも使用できます。有効な管理者と、既存のパスワード＋認証コードによる追加認証、セッションに結び付いた CSRF トークンが必要です。本人が変更したメールの場合、別途確認した旨も明示的に承認します。
4. 接続テストを実行します。呼び出しごとに gateway が現在の主担当者のログインメールから対象を決めます。ツール引数は使いません。未確認、担当者変更（元に戻した場合も含む）、メール変更、無効な担当者、読み取り失敗の場合は拒否し、管理者に再確認を求めます。`requesters` の既定値は `owner_only` で、定期実行は既存の要求者ルールに従います。この ID では `fallback = "home"` は使用できません。保護モードのより厳しいテナント経路と要求者の制限も引き続き適用されます。

確認 RPC は `google.delegation.attest_subject {agent_id, subject, user_id, acknowledge_self_set?, csrf_token}` です。サーバーは表示されたユーザー ID とメールを現在の担当者と照合し、管理者、対象ユーザー、メールを監査に記録します。個人 OAuth の本人同意を代行するものではありません。

管理者は `owner` と `user:<id>` でも「接続テスト」（`google.identity.test {agent_id}`）を使用できます。接続記録、本人同意、スコープと用途がカバーするツール、OAuth refresh、tokeninfo のスコープ比較だけを確認し、Gmail、Drive、Calendar などのデータエンドポイントは呼びません。操作した管理者と対象メンバーを監査し、本人の検証済み通知先に通知します。テスト自体は本人同意や AI の使用許可を付与しません。

別途管理する外部 Google ツールを使う場合：

```toml
[google]
identity = "external"
external_server = "Partner Google MCP" # 任意、120 文字まで
```

接続タブには「外部ツールで提供」と表示します。ネイティブ Google ツールは `none` と同様に非表示・呼び出し拒否となります。この設定は外部サーバーを構成・認可しません。名前は表示用テキストで、制御文字は拒否します。旧版は未知の ID またはキーを拒否するため、共用アカウントへフォールバックしません。

### 毎回確認する Gmail 送信と Calendar 削除

`gmail_send_draft {"draft_id":"r…"}` と `calendar_delete_event {"event_id":"…","calendar_id":"primary","notify_attendees":false}` は `google:write` と gateway のネイティブ REST 実行が必要です。Apps Script は両ツールを非表示にし、直接呼び出しも拒否します。bearer だけを渡す従来の Home 経路も非表示・拒否となります。社員専用 Google 接続か、gateway のアカウントに紐づくコネクターを使用してください。

呼び出すたびに新しい確認カードが必要です。`allow`、Admin 権限、永続ワークフロー権限でも省略できず、`block` は引き続き拒否します。送信カードは Google から取得した送信元、To／Cc／Bcc、件名、本文プレビュー、添付ファイル名とサイズを表示します。承認はアカウント、社員の世代、下書き ID、操作、MIME ハッシュに紐づきます。gateway は再確認して承認済み MIME を送信し、内容やアカウントの変更時は再承認が必要です。宛先ポリシーは全 To／Cc／Bcc を確認し、曖昧なアドレス構文を拒否します。

削除カードは予定名、タイムゾーン付き開始・終了、参加者、カレンダー、繰り返し全体の削除、通知設定を表示します。承認したバージョンを `If-Match` で確認し、変更時は再承認を要求します。`calendar_id` の既定は `primary`、通知は明示的に `sendUpdates=none` です。`notify_attendees=true` のときだけ `all` を使用します。空の HTTP 204 は成功です。削除時は既存の `calendar.calendarlist.readonly`（または上位の Calendar スコープ）で、プライマリカレンダーのアカウントと選択したカレンダーの名前・タイムゾーンも取得します。

MIME 全体は gateway のメモリに保持し、承認には必要なプレビューとハッシュを保存します。拒否・期限切れ・使用済み・不一致の承認では変更しません。書き込みのタイムアウトや結果不明時は `result_unconfirmed` を返します。自動再送は行わず、再試行の前に Google で結果を確認してください。確認期限は 90 秒です。

通信契約は [Gmail 下書き](https://developers.google.com/workspace/gmail/api/guides/drafts)と [Calendar の条件付き変更](https://developers.google.com/workspace/calendar/api/guides/version-resources)に従います。
