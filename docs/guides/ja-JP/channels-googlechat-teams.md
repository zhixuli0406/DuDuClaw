# Google Chat & Microsoft Teams チャネル

Google Chat チャネルの認証情報は Chat アプリ（`chat.bot`）用で、Gmail、Drive、Calendar は接続しません。データツールは [Google 接続](google-setup.md)で別途設定します。ダッシュボードの保存は即時反映し、設定ファイルを手編集した場合は再起動します。

DuDuClawはGoogle ChatとMicrosoft Teamsを、Telegram／LINE／Discord／Slack／
WhatsApp／Feishu／WebChat と並ぶチャネルとしてサポートしています。Teams と Google Chat HTTP モードは公開 HTTPS URL が必要です。Google Chat Pub/Sub モードは不要です。

両チャネルとも以下を備えています。

- **Markdown対応の返信**：LLMが生成したmarkdownは各プラットフォームのネ
  イティブ記法に変換されます(Google Chatは`*bold*`／`<url|text>`リンク、
  Teams markdownでは表が等幅ブロックにダウングレードされます)。
- **入力中インジケーター**：Teamsは本物のタイピング中インジケーターを表
  示します(3秒ごとに更新)。Google Chatにはタイピング表示用のAPIがないた
  め、DuDuClawはまずプレースホルダーメッセージ(「🤔 思考中…」)を送信し、
  それをその場で編集して更新します。
- **リアルタイム進捗**：長時間かかるagentタスクの実行中は、ツールの活動
  状況とagentのTODOタスクボード(`TodoWrite`由来)がメッセージ編集を通じ
  てリアルタイムに表示され、最終的な返信が届くと削除されます。

### 返信の表示と配信

Google Chat の表はリストとして表示します。各行の最初のセルが太字の見出し、残りが「列名: 値」となり、内容は省略しません。箇条書きは `•` に変換し、見出しと複数行の太字も Chat 記法へ変換します。中国語の太字の前後に空白は追加しません。

ツールの詳細とタスクリストは内部ユーザー（管理者、または回答する社員に直接紐づいた検証済みアカウント）のみに表示します。他の送信者には一般的な進捗だけを表示します。設定変更のお知らせは次の内部ユーザーへの返信まで保留します。アクセス拒否、ミュート、人による対応中は空の返信の警告を出さず、可能なら一時メッセージを削除します。

最終回答の送信開始後は進捗更新を止めます。一時メッセージの更新が失敗した場合、短い待機後に一度再試行し（レート制限では最大3秒）、再度失敗したら新規メッセージで送ります。元の一時メッセージは可能なら中立的な案内に変更し、残りの回答も送信を試みます。最初の更新が成功して応答だけ失われた場合、最初の部分が重複する可能性があります。

Google Chat の回答には思考の要約を付けません。既存の設定に従い、内部ユーザーには所要時間の一行だけを表示でき、他の送信者には表示しません。返信言語が明示的に `zh-TW` の場合は限定的な字表で繁体字へ変換し、コードと URL は保持します。自動言語ではユーザーの入力言語に合わせる指示を各ターン末尾に追加します。

返信中にゲートウェイが再起動した場合、既存の「🤔 思考中…」を「返信が中断されました。もう一度メッセージを送ってください。」に変更します。通知を新規投稿せず、元のメッセージを編集します。起動ごとに最大 200 件、30 秒まで処理し、Google API の一時的な障害は次回の再起動時に再試行します。24 時間を超えた復旧記録は削除します。Pub/Sub で処理中に中断したメッセージは自動で再実行されないため、再送してください。

実際の Google Chat クライアントでは未検証です（尚未在真實 Google Chat 用戶端驗證）。Web とモバイルで表リスト、中国語の太字、複数行の太字、一時メッセージの置換を確認してください。

## 複数の AI スタッフで一つの Chat アプリを共有する（v1.85）

管理者は「チャネル → Google Chat → 動作」で「送信者ごとに振り分け（共有 Chat アプリ）」を有効にできます。初期状態はオフで、会社の共有アプリだけに適用されます。スタッフ専用アプリには影響しません。保存後、次のメッセージから反映され、再起動は不要です。

確認済みの一対一 DM では、既存の `@` メンションが優先されます。指定がなければ、イベント内の Google Chat ユーザー ID と、管理者が「ユーザー」画面で登録した手動の紐付けからメンバーを特定し、AI アバターに渡します。アバターが未設定の場合はメイン AI を使います。この振り分けには Google アカウントの自動照合を使いません。表示名、メールアドレスの文字列、メッセージ本文から本人と判断することもありません。スタッフごとに会話履歴が分かれ、`/new` などの会話コマンドは振り分け先の会話だけを操作します。

紐付けが見つからない、読み取りに失敗した、利用可能なアバターやメイン AI がない、スタッフがアーカイブ済み・利用不可、保護モードの Personal 権限に合致しない場合は、管理者が設定した「未開通のユーザーへの返信」だけを送ります。モデルの呼び出しや会話の作成は行わず、会社の既定スタッフにも切り替えません。同じ送信者への短時間の繰り返し返信は制限されます。

返信はプレーンテキストで、1,000 文字以内です。使用できるプレースホルダーは `{onboarding_url}` だけです。有効な HTTPS 公開ダッシュボード URL が設定され、認証情報・クエリ・フラグメントを含まない場合に置き換わります。適切な URL がなければプレースホルダーを削除します。**アプリに DM を送れる人は誰でも、この返信内のダッシュボード URL を見ることができます。** 初期設定の返信は管理者への連絡を案内するだけです。

**Chat アプリの名前とアイコンは会社全体で共通です。** 別の AI スタッフに振り分けても、Google Chat 上のアプリの表示は変わりません。グループとスペースでは従来どおり、メンションされたスタッフ、またはチャネルの既定スタッフが応答します。送信者のアバターへの振り分けは行いません。スペースの種類が不明な場合は共有スペースとして扱います。

## Google Chat

### 前提条件

Chat が有効な Google Workspace Business／Enterprise アカウントと、Chat アプリを許可する管理者が必要です。Cloud プロジェクトの作成または直接所有、Chat API の有効化、サービスアカウントとキーの作成ができる必要があります。ローカルでは Bash、Python 3、`gcloud` と自分の `gcloud auth login` が必要です。Cloud Shell にはこれらのツールがあります。HTTP モードには公開 HTTPS URL が必要です。Pub/Sub モードは外向き接続のみで動作し、公開 URL は不要です。

### ワンステップで作成・インポート

新しい専用 Google Cloud プロジェクトを推奨します（既定の動作）。DuDuClaw ホストで実行します。

自動インポートには稼働中の DuDuClaw gateway と管理者ログインが必要です（MFA が必要な場合はその認証も行います）。

```bash
# HTTP: 自分の公開 gateway アドレスに置き換えます。
duduclaw channels googlechat bootstrap --endpoint-url https://chat.example.com/webhook/googlechat
```

プロジェクト、Chat API、サービスアカウントを準備し、最後のクラウド操作で非公開 JSON キーを作成してから設定をインポートします。`--project-id your-project-id` は直接所有する既存プロジェクトを再利用します。グループや継承による所有権は安全側で拒否します。追加オプションは `--app-name`、`--agent`、`--out`、`--dry-run`（クラウドもローカルも変更しません）です。既定 ID は `duduclaw-chat-<random6>`。キーは専用出力ディレクトリの `service-account.json` に 0600 で保存します。再実行では一致するキーを再利用します。インポートした認証情報は暗号化して保存します。HTTP モードで作成後に後続のスクリプト処理が失敗した場合、新しいクラウドキーの取り消しとローカルファイルの削除を試みます。取り消しに失敗した場合やキー ID を読み取れない場合は、手動対応が必要であることを表示します。 スクリプト成功後に CLI のインポートが失敗した場合、CLI はキーファイルを保持して非ゼロで終了します。新しいサービスアカウントキーは引き続き有効です。エラーにはファイルパス、再インポートと手動取り消しの完全なコマンドが表示されます。CLI はキーの取り消しやキーファイルの削除を自動実行しません。

ローカルに gcloud がない場合は [Cloud Shell を開き](https://shell.cloud.google.com/cloudshell/editor?cloudshell_git_repo=https://github.com/zhixuli0406/DuDuClaw&cloudshell_tutorial=scripts/googlechat/cloudshell-tutorial.md&show=terminal)、`bash scripts/googlechat/bootstrap.sh --endpoint-url https://chat.example.com/webhook/googlechat` を実行します。キーを安全に DuDuClaw ホストへダウンロードし、`chmod 600 /private/path/service-account.json` で権限を制限してから、ローカルパスでインポートします。Windows では通常のキーファイルを自分のユーザープロファイル内に保存してください。再解析ポイントは拒否しますが、所有者と ACL 権限は検証しません。

```bash
duduclaw channels googlechat import --project-number 123456789012 --key-file /private/path/service-account.json
```

### Google Chat で設定を完了

表示された Chat Configuration リンクを開きます。この設定画面の API はないため、最後は手動で行います。

1. **作成時に「Build this Chat app as a Google Workspace add-on」（この Chat アプリを Google Workspace アドオンとして構築する）のチェックを外してください。** アプリ名（最大 25 文字）、公開 HTTPS アバター URL、説明を入力します。スクリプトは既存の DuDuClaw アバター URL を仮定しません。
2. **Interactive features**、1 対 1 メッセージ、スペース参加を有効にします。
3. HTTP エンドポイントを入力し、**Authentication Audience → Project number** を選びます。
4. **Visibility** に自分の Workspace メールを入力して保存し、アプリをスペースに追加するか直接メッセージを送り、実際の返信を確認します。

リソース作成やインポート成功だけでは Chat の設定・送受信の確認にはなりません。ダッシュボードにはコピー可能なコマンド、Cloud Shell と設定値があります。従来の入力欄は「手動設定」に残ります。

### 送信者を見分ける（v1.82）

Google Chat の送信者は既定では不明な人として扱われるため、`[google] requesters = "bound_people"`／`owner_only` は拒否し、`@` で呼べるのも既定のスタッフと公開スタッフだけです。次の場合に、送信者はダッシュボードのアカウントとして見分けられます。

- 管理者が **ユーザー → 本人確認を設定 → Google Chat** で Chat のユーザー ID（`users/<数字>`）を紐付けた（既定ではこの方法だけです）。
- 任意・**既定はオフ**：管理者が「管理 → 連携 → Google」で **接続済みの Google アカウントで Google Chat の送信者を識別する**（`[google] chat_identity_from_account = true`）をオンにし、本人が **自分の接続** で自分の Google アカウントを連携している。送信者の ID をその Google アカウントの ID と照合し、一致すれば本人とみなします。**実際の Google Workspace ではまだ検証されていません**。ご自身の Workspace で 2 つの ID が一致することを確認していない限り、オフのままにしてください。

メールアドレスでの照合はしません。人ではない送信者（別の Chat アプリ）からのメッセージは無視し、Google Chat の ID には DuDuClaw からの通知を送りません。Google Chat ではログインコードや 2 段階認証コードを受け取れません。詳細と 1 人 1 ID のルール：[ダッシュボード設定 → ユーザー → 本人確認を設定](dashboard-settings.md#ユーザー--本人確認を設定メッセージアプリの-idv182)。

### キーポリシー

キー作成が `FAILED_PRECONDITION` になった場合は、組織ポリシー管理者にプロジェクト例外の審査を依頼します。2024-05-03 以降に作成した組織は通常キー作成を制限します。スクリプトは次を表示して停止し、自動でポリシーを変えません。

```bash
gcloud resource-manager org-policies disable-enforce iam.disableServiceAccountKeyCreation --project=your-project-id
gcloud resource-manager org-policies disable-enforce iam.managed.disableServiceAccountKeyCreation --project=your-project-id
```

managed constraint はインストール済み gcloud の対応を管理者が確認し、必要なら Organization Policies 画面で変更します。[Google のキーポリシーガイド](https://cloud.google.com/iam/docs/troubleshoot-org-policies)も参照してください。

### 付録：手動設定

プロジェクト、Chat API、サービスアカウントと JSON キーを作成し、上記の Chat 設定を行います。「チャネル → Google Chat を追加 → 手動設定」にプロジェクト番号と JSON キー全体を入力します。保存後にメッセージをテストします。HTTP は `chat@system.gserviceaccount.com` とプロジェクト番号の JWT を検証します。未対応の Workspace アドオン形式には、アドオン設定を外すよう診断を表示します。

### Pub/Sub：公開 gateway URL は不要

Unix の DuDuClaw ホストで実行します。

```bash
duduclaw channels googlechat bootstrap --connection pubsub
# 作成済みのキーと pull subscription をインポートする場合:
duduclaw channels googlechat import --connection pubsub --project-number 123456789012 --key-file /private/path/service-account.json --subscription projects/your-project-id/subscriptions/duduclaw-chat-events-sub
```

Cloud Shell では `bash scripts/googlechat/bootstrap.sh --connection pubsub` を使い、キーを安全にダウンロードしてからホストで表示されたインポートコマンドを実行します。社員専用アプリには `--agent alice` を追加し、アプリごとに専用プロジェクトを使います。スクリプトは Chat と Pub/Sub API、`projects/<project-id>/topics/duduclaw-chat-events`、配信保持期間 1 日の pull subscription を準備します。Google Chat の発行元にトピックへの発行権限、アプリのサービスアカウントにそのサブスクリプションの購読・メタデータ読取権限を付与します。リソース検証と IAM 設定を終えてからキーを作成します。既存のサブスクリプションは指定トピックと一致する必要があり、設定は自動変更しません。インポート時と worker 起動時にも Google からサブスクリプションを読み戻します。Pub/Sub 設定に失敗した場合、キーを保持して手動取り消しコマンドを表示し、破壊的な gcloud コマンドは実行しません。

Chat Configuration で **Build this Chat app as a Google Workspace add-on** をオフにし、**Cloud Pub/Sub** を選んで表示されたトピックを貼り付けます。公開 HTTP エンドポイントは不要です。その他のアプリ・可視性設定を完了し、実際にメッセージを送ってください。gateway は Google OAuth、Pub/Sub、Chat API への HTTPS 接続が必要です。

gateway は [`spaces.messages.get`](https://developers.google.com/workspace/chat/api/reference/rest/v1/spaces.messages/get) で毎回 Google Chat からメッセージを読み直します。アプリ認証の `https://www.googleapis.com/auth/chat.bot` を使い、管理者承認が必要な Chat スコープは要求しません。pull 用トークンは別途 `https://www.googleapis.com/auth/pubsub` のみを使います。Pub/Sub 本文は信頼しない通知として扱い、厳密に検証した Chat メッセージのリソース名だけを検索に使用します。送信者、スペース、本文、添付、スレッドはすべて API 応答から取得し、その送信者に許可リスト・ペアリング・承認ルールを適用します。メッセージが存在しない、アクセスできない（403／404 を含む）、形式やスペースが一致しない場合は診断を記録して破棄し、ターンを作成しません。診断が出た場合はアプリのアクセス権と `chat.bot` を確認してください。スペース追加など取得可能なメッセージを持たないイベントは社員ターンを開始しません。

永続 inbox はイベント種別と Chat メッセージのリソース名で重複を除去します。新しい Pub/Sub message ID で再発行しても、保持中のイベントは再実行されません。記録は 7 日間保持し、API の作成時刻が 7 日より古いメッセージは削除後も拒否します。受理したイベントは保存後に ACK します。再起動後は未開始の処理を再開でき、実行中に中断した処理は uncertain として自動再実行しません。キューが満杯なら ACK して破棄し、ターンを開始しません。HTTP モードと同じく、スペースには再送を促す通知を送ります（同じ送信者には 1 分に 1 回、同じスペースには 10 秒に 1 回まで）。これはエンドツーエンドの exactly-once 保証ではありません。Unix 専用 inbox はリソース参照のみを保存し、本文・送信者情報・添付を保存しません。アプリ stream（社員／共有アプリとプロジェクト番号）ごとに 1,000 行、pending／queued／running 10 件を上限とし、他の stream 用の容量を確保します。全体の上限は引き続き 10,000 行、処理待ち・実行中 100 件、参照 2 KiB、データベース 64 MiB で、実行中・待機中の記録は削除しません。Unix 以外では同等の所有者／ACL 検証が整うまで Pub/Sub のキーファイルのインポートを拒否します。Google API の読み戻しと実際の送受信は別途実機確認が必要です。 インポート時と worker 起動時は exactly-once 配信が有効なサブスクリプションを拒否するため、Google Cloud で無効にしてください。無効・期限切れ ACK（HTTP 400）では古い配信バッチのみを破棄して再取得し、永続重複除去記録は保持します。一時的な ACK 失敗ではバッチを保持して再試行します。

### コマンド・アクセス・ペアリング

チャットコマンド（`/new`、`/reset`、`/compact`、`/pair`、`/status` など）は通常のメッセージと同じく、同じスペースでアクセス確認を通ります。ブロックされた送信者には何も返さず、未ペアリングの送信者にはペアリングの案内を返します。

スペースの会話はメンバー全員・全スレッドで 1 つを共有します（社員専用の Chat アプリは同じスペース内に自分の会話を持ちます）。共有の会話を消去・圧縮（`/new`、`/reset`、`/compact`）できる人は次のとおりです。

| インストールの状態 | アプリとの 1 対 1 のダイレクトメッセージ | スペース、グループチャット（複数人の DM）、種類不明 |
|---|---|---|
| ユーザーアカウントがない、または検証済みのチャネル ID を持つ有効な**管理者**／**マネージャー**がいない（個人利用など） | アクセス確認を通った人なら誰でも | アクセス確認を通った人なら誰でも |
| 検証済みのチャネル ID を持つ有効な管理者またはマネージャーが 1 人以上いる | アクセス確認を通った人なら誰でも | 検証済みのチャネル ID が有効な管理者またはマネージャーに紐付いている人だけ。それ以外の人には「只有管理員可以清除這個聊天室的對話。」と返します |
| ユーザー情報は存在するが読めない | アクセス確認を通った人なら誰でも | 拒否 |

ブロックされた送信者や未ペアリングの送信者は、どの場合もこれらのコマンドを使えません。2 行目では、未検証の ID や照会の失敗は拒否します。スペースの種類はメッセージ自体（HTTP のイベント、または Pub/Sub モードで `spaces.messages.get` により読み直したメッセージ）から判断します。メッセージに含まれない場合、gateway はアプリとして Google Chat に問い合わせ（`spaces.get`）、結果を 24 時間保持します。1 対 1 のダイレクトメッセージとは、1 人とアプリの間のダイレクトメッセージです。複数人のグループチャットは共有の会話として扱います。それでも種類が分からない場合は複数人のスペースとして扱います。Telegram のグループ、Discord のサーバーチャンネル、Slack のチャンネル、LINE のグループ、Teams のグループチャットとチャンネル、Feishu と DingTalk のグループ、および「新しい会話」ボタンにも同じルールが適用されます。

ペアリング（`require_pairing`）：`pairing_manage` ツール（`action: "generate"`）で次のどちらかを対象にコードを発行します。

- `users/<id>`：メンバー本人の Chat アカウント。そのメンバー本人だけが使え、他の人が入力しても失敗します。
- `googlechat:spaces/<id>`：スペース全体。ペアリング後はそのスペースの全員が通過します。

`/pair <コード>` は通常のメッセージと同じく、まず送信者、次にスペースで照合します。社員専用の会話の対象（`googlechat:spaces/<id>#agent:<社員 ID>`）はスペースそのものとして保存・照合されるため、社員専用 Chat アプリでのペアリングも有効になります。以前のバージョンがこの接尾辞付きで保存したペアリングは誰も通過させていませんでした。アップグレード後も同じで、スペース全体が急に開放されることはありません。必要な場合は新しいコードを発行してください。コードの有効期限は 5 分で、5 回間違えるとロックされ、同じ対象で累計 15 回失敗すると新しいコードを発行できなくなります。

### ダイレクトメッセージ、判断ボタン、ダッシュボードへのリンク（v1.84）

**1 対 1 のダイレクトメッセージは個人の会話です。** 1 人のユーザーとアプリとのダイレクトメッセージは、LINE や Telegram の個別チャットと同じく個人の会話として扱うようになりました。そのため、Google Chat の ID が確認・紐付け済みのメンバーは、閲覧を許可された個人用・制限付きのナレッジコレクションの内容を受け取れ、個人の会話でだけ有効になる社員の機能（人物メモ、記憶）も適用されます。メンバーが読める範囲は引き続き紐付けと権限で決まり、ダイレクトメッセージ自体は何の権限も与えません。紐付けのない送信者は、ダイレクトメッセージでも個人用・制限付きのナレッジコレクションを読めません。スペースとグループチャットは共有の会話のままです。判定にはメッセージについて Google が返すスペースの種類（`spaceType` が `DIRECT_MESSAGE` で、かつ `singleUserBotDm` が真）を使い、会話 ID は使いません。送信者は人間のアカウントである必要があり、種類が不明な場合は共有として扱います。ダイレクトメッセージから委任された作業は、そのダイレクトメッセージとしては扱いません。

**ゴール判断カードのボタン。** アプリが HTTP でイベントを受け取る場合、ゴールの「人的対応が必要」カードに「再試行」「完了にする」「諦める」「自分で対応」のボタンが付きます（表示は繁体字中国語）。Pub/Sub でイベントを受け取るアプリは信頼できるクリックを受け取れないため、テキストのカードのままとし、ダッシュボードの未処理の判断ページで対応するよう案内します。HTTP か Pub/Sub かは、共有アプリでは `[channels] googlechat_connection` と `googlechat_pubsub_subscription`、社員専用アプリではその社員の `[channels.googlechat] connection` と `pubsub_subscription` で判断し、はっきりしない場合はボタンを付けません。誰が押せるか、押した後どうなるかは[ゴールループ](goal-loop.md)を参照してください。

**チャットメッセージ内のダッシュボードへのリンク。** 通知や判断カードのダッシュボードへのリンクには、`[dashboard]`、`[general]`、`[gateway]` の順で最初に有効な `public_url` を使います。有効な値は http または https で、ホストが localhost やループバックアドレスではなく、ユーザー名、クエリ、フラグメントを含まないものです。無効な値はスキップし、セクションごとに 1 回警告を記録します。どれも有効でない場合、コンテナ内で動く gateway（`/.dockerenv` がある、または `DUDUCLAW_IN_CONTAINER` の値がちょうど `1`、`true`、`yes` のいずれか）はリンクを付けず、通常のマシン上の gateway は `http://localhost:<port>` のリンクを残します。招待リンクも同じ順序を使いますが、引き続き https が必要です。

### 制限

- Google Chat は引き続き、AI が生成したファイルや画像を添付送信できません。制限を明示し、ホストのパスや、アカウントを持たない可能性のあるメンバーへの Dashboard 案内は表示しません。管理者にファイルを依頼するか、ファイル送信に対応したチャネルを利用してください。定期実行の返信も、送信前にファイル配信マーカーを処理します。
- Drive の添付は自動ダウンロードしません。検証済みのファイル ID を AI に渡し、接続済み Google ID で Drive 読み取りツールを試せるようにします。アクセス権と対応ファイル形式の制限は残ります。直接添付のダウンロード失敗は明示し、HTTP エラー応答をファイルとして保存しません。貼り付けた Drive リンクの処理は変更していません。
- プロアクティブ送信の宛先には `spaces/<id>` が必要です。403／404 の場合は、アプリが会話に参加していない可能性を説明し、相手から一度メッセージを送るか管理者にインストールを依頼するよう案内します。レート制限やサーバー障害は一時的な失敗として表示します。ユーザーから DM スペースの解決や、ユーザーとしての送信は行いません。

## Microsoft Teams

> Office 365 Connectors／incoming webhooksは**2026年5月に廃止**され、
> 現在サポートされている唯一の伝送経路は本物のAzure Botです。

### セットアップ

1. **Entra appの登録**(Azure portal → App registrations → New
   registration、*single tenant*を選択)。**Application(client)ID**と
   **Directory(tenant)ID**を控え、**client secret**を作成する。
2. **Azure Botリソース**(Create a resource → Azure Bot、無料の**F0**
   プランで十分。Teamsのメッセージは常に無料)。既存のApp IDを使い、
   *Configuration → Messaging endpoint*を
   `https://<your-host>/webhook/teams`に設定し、*Channels → Microsoft
   Teams*を有効化する。
3. DuDuClawを設定する:

```toml
[channels]
teams_app_id = "00000000-0000-0000-0000-000000000000"
teams_app_password = "<client secret>"   # stored encrypted as *_enc
teams_tenant_id = "<tenant id>"          # empty = legacy multi-tenant bot
```

4. **Teams appパッケージ**：`manifest.json`(schema ≥1.19、
   `bots[].botId`は自分のApp ID、scopeは`personal`／`team`／
   `groupChat`)を含むzipを作成し、`color.png`(192×192)と
   `outline.png`(32×32)を加える。Teams → Apps → *Manage your apps* →
   *Upload a custom app*からアップロード(組織のカスタムappポリシーが
   必要)、または組織のカタログ経由で配布する。
5. gatewayを再起動する。ログに
   `✅ Microsoft Teams webhook ready at /webhook/teams`と表示されれば
   成功。

### 補足

- 受信アクティビティは常にBot FrameworkのJWKS
  (`login.botframework.com`)に対してfail-closedで検証され、audienceは
  自分のApp IDと一致し、トークンの`serviceUrl`クレームはそのアクティビ
  ティと一致している必要があります。single-tenant登録に対しては、Entra
  テナントスコープのフォールバック検証も用意されています。
- 受信メッセージのたびに**conversation reference**が保存されます
  (`~/.duduclaw/teams_conversations.json`、上限500件)。これにより、
  delegation callbackの転送やComputer Useのスクリーンショット／確認メッ
  セージのような能動的な送信が、後からその会話に届けられます。能動的な
  送信が届くには、その会話が一度以上botにメッセージを送っている必要が
  あります。
- 返信には`textFormat: markdown`が使われます。Teamsは通常のメッセージ
  内で表や見出しをレンダリングしないため、DuDuClawは表を等幅コードブ
  ロックに、見出しを太字にダウングレードします。
- チャネル(channel)内では、botは自分に@メンションされたメッセージしか
  受け取りません(Teamsプラットフォーム自体の仕様)。このメンションは
  agentに渡される前に取り除かれます。

## フォーマット対応表(全チャネル)

| チャネル | ネイティブ形式 | 表 | 入力中インジケーター |
|------|----------|------|----------|
| Telegram | HTML parse mode(`<b>`、`<pre><code>`、`<blockquote>`) | 等幅`<pre>` | `sendChatAction`を4秒ごとに送信 |
| Discord | markdown + embeds | 等幅code fence | `POST /typing`を8秒ごとに送信 |
| Slack | ネイティブの`markdown`ブロック(フォールバック時はmrkdwn) | ネイティブ対応 | `assistant.threads.setStatus` |
| LINE | プレーンテキスト + Flex bubble | key-value形式のレコード | ローディングアニメーション(1:1、≤60秒) |
| WhatsApp | `*bold*`／`~strike~`／```` ```コードブロック | 等幅code fence | `typing_indicator`(≤25秒、受信時に発火) |
| Feishu | インタラクティブなCard 2.0 markdown | ネイティブ対応 | なし(メッセージで進捗を表示) |
| Google Chat | Chat markup(`*bold*`、`<url\|text>`) | 太字の行見出し＋列名と値のリスト | プレースホルダー + その場での編集 |
| MS Teams | markdownアクティビティ | 等幅code fence | `typing`アクティビティを3秒ごとに送信 |
| WebChat | 生のmarkdown(ダッシュボード側でレンダリング) | ネイティブ対応 | `progress` WSイベント |

## グループ会話コンテキストの設定

Admin が「チャンネル → グループ設定」でグループごとに保存を有効化します。Telegram は BotFather の `/setprivacy` を無効にするか、bot 管理者として受信します。Slack は `message.channels`／`message.groups` と `channels:history`／`groups:history` が必要で、変更後は App を再インストールしチャンネルへ招待します。Discord は MESSAGE CONTENT INTENT が必要です。Google Chat と Teams は App へのメンションのみを受信する場合があります。Teams の全チャンネル受信には `ChannelMessage.Read.Group` などの RSC が必要です。保存期間と参照権限は[グループ会話コンテキスト](../../features/ja-JP/72-group-context.md)を参照してください。本変更で実際のプラットフォーム送受信は未検証です。

### Google Chat の返信とプレビューリンク

DM は既定でメイン画面に返信します。スレッド内のメッセージ（`threadReply = true`）には同じスレッドで返信し、スペースの動作は従来どおりです。「チャンネル → Google Chat → 動作 → DM の返信先」で「メイン画面」（既定）または「常にスレッド内」を選択できます。変更はダッシュボードの管理者のみ可能で、設定はチャンネル設定データベースに保存されます。`config.toml` に追加しないでください。会話セッションは引き続きスペース全体で共有します。

Google ドキュメント／スプレッドシートのプレビューリンクは信頼できないメッセージ本文として AI に渡されます。重複を除き HTTPS リンクを最大 10 件、各 2048 バイトまで保持し、制御文字を含むリンクは除外します。ゲートウェイはリンクにアクセスしません。読み取りツールの `links` 配列もスキャン対象です。名前がない場合は読みやすい代替名を使います。リソース名と保存パスはツール操作専用で、利用者には元の添付ファイル名を表示します。OAuth 権限の追加は不要です。
