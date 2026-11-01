# Google を接続する

このガイドと設定ウィザードに沿って、Google アカウントを接続します。

| 利用状況 | 接続方法 | 主な制限 |
|---|---|---|
| 会社の Workspace、特権管理者あり | サービスアカウントとドメイン全体の委任 | 個人 Gmail は対象外 |
| 会社の Workspace、自分の OAuth を使用 | OAuth、Internal | 組織のメンバー限定、Testing（テストモード）の七日期限なし |
| 個人 Gmail：メール、カレンダー、スプレッドシート | Apps Script ブリッジ | 三サービス、状態確認を含む八アクション |
| 個人 Gmail：すべての Google ツール | OAuth、External + Testing | Testing 中は七日ごとに再認可 |

## DuDuClaw で開始

1. 管理者としてログインし、**管理 → 連携 → Google**（`/manage/integrations?tab=google`）で **Google を接続** を選びます。従来のフォームは **詳細設定** から開けます。
2. 上の表から状況を選びます。OAuth の経路にはデータ利用の説明が表示されるので、読んで確認します。シークレットはダッシュボードだけに入力し、社員とのチャットに貼らないでください。
3. 以下の経路を一つ設定します。サービスアカウントや Apps Script の認証情報を保存するか、OAuth 認可が成功すると、連携が自動で有効になります。OAuth への切り替えでは新しい認可が成功するまで現在の接続を維持します。キャンセルや拒否では元の接続が残ります。

## OAuth

1. [Google Cloud Console](https://console.cloud.google.com/) の上部でプロジェクトを作成または選択します。
2. **APIs & services → Library** で Gmail API、Google Calendar API、Google Sheets API、Google Drive API、Google Docs API、Google Slides API、Google Forms API、Tasks API を検索して有効にします。
3. **Google Auth platform → Branding → Get Started** で App Information、Audience、Contact Information、Finish を完了します。組織メンバーは Internal を選びます。個人 Gmail は External を選び、Testing（登録アカウントだけが利用するテストモード）を維持します。Publish app は押さないでください。
4. External アプリが Testing の間は、**Audience → Add users** に接続する全アカウントを追加します。最大 100 人のテストユーザーを登録でき、認可と更新トークンは七日で失効します。ウィザードで Testing を選ぶと、六日を過ぎた接続に警告が出ます。
5. **Data Access → Add or Remove Scopes** を開き、ウィザードの全スコープ一覧を **Manually add scopes** に貼り付けて追加・保存します。末尾にも同じ一覧があります。
6. **Clients → Create Client** で **Web application** を選びます。Web と Desktop の両方に任意の “Use this client for an AI-powered agent” チェックボックスがあります。この接続では Web を使います。
7. DuDuClaw が表示する redirect URI を **Authorized redirect URIs** に完全一致で登録します。画面のポートを使ってください。ネイティブ版の既定は 18889、同梱 Docker Compose は 18789 です。localhost 以外は HTTPS が必要で、IP アドレスやワイルドカードは使えません。ホスト、ポート、パス、末尾のスラッシュの違いで失敗する場合があります。
8. 保存直後に Client ID と Client secret を保管します。シークレットの表示・ダウンロードは一度だけで、その後は末尾四文字しか表示されません。パスワード管理ツールに保存してください。
9. ウィザードに ID とシークレットを入力し、データ利用の説明を確認して **Google を接続** を選びます。「Google hasn't verified this app」が出たら、自分のプロジェクトであることを確認し、Advanced、Go to（プロジェクト名）の順に選びます。同意画面の権限をすべて選択して続行し、ダッシュボードの結果を待ちます。

### リモートホストと Docker

localhost はブラウザーを実行する端末です。ローカル Docker では SSH トンネルは不要です。DuDuClaw がリモート端末で動く場合だけポートを転送します。以下は画面に 18889 が表示される場合の例です。

```sh
ssh -N -L 18889:127.0.0.1:18889 USER@DUDUCLAW_HOST
```

リモートでは認可完了まで SSH 接続を維持します。画面が 18789 など別のポートを示す場合は、コマンド内の二つの 18889 を両方変更し、画面の URI を登録します。同梱 Docker Compose は 18789 を全ネットワークインターフェースに公開し、loopback 限定にはしていません。

### スコープと有効期限

DuDuClaw が要求するスコープのうち、`gmail.readonly`、`gmail.compose`、`drive.readonly` は restricted（制限付き）です。全一覧は末尾にあります。Google の [Gmail 分類](https://developers.google.com/workspace/gmail/api/auth/scopes)と [Drive 分類](https://developers.google.com/workspace/drive/api/guides/api-specific-auth)で検証要件を確認できます。個人利用では External + Testing を維持し、アプリを公開しないでください。これらの権限を公開提供する場合、Google の検証やサーバーでのデータ処理の安全性評価が必要になることがあります。Internal は組織メンバー限定です。

未検証アプリの警告画面はプロジェクト存続期間で新規 100 人までで、リセットできません。Testing の登録 100 人とは別の上限です。更新トークンは六か月未使用でも失効します。クライアントは六か月未使用で自動削除され、その 30 日前に警告メールが届きます。更新トークンの保存だけでは現在の接続成功は分かりません。

## サービスアカウントとドメイン全体の委任

1. Workspace の特権管理者に委任を承認してもらいます。許可スコープ内でドメインのユーザーとして動作できます。個人 @gmail.com は対象外です。
2. Google Cloud プロジェクトを管理する人が、そのプロジェクトで OAuth 手順の八 API を有効にします。**IAM & Admin → Service Accounts** でアカウントを作成し、そのアカウントを開いて **Keys → Add key → Create new key → JSON** から鍵をダウンロードします。数値 Client ID は JSON の `client_id` フィールドです。
3. 管理者に Client ID とダッシュボードの全スコープを渡します。管理者は **Admin console → Security → Access and data control → API controls → Manage Domain Wide Delegation → Add new** で両方を登録します。反映に最大 24 時間かかる場合があります。
4. 共用アカウントはウィザードで **会社の Workspace、特権管理者あり** を選びます。JSON を DuDuClaw ホストに所有者限定の権限で保存し、絶対パスを入力します。例は DuDuClaw ホーム配下の `/Users/alex/.duduclaw/keys/google-sa.json` です。実際のパスに置き換えてください。`~` は展開されないので使わないでください。代理する Workspace ユーザーのメールを入力して保存します。実在するユーザーを指定してください。
5. 社員ごとに別の同僚を代理する場合は **セキュリティ → 認証情報保護 → システムのキーチェーンへ移す**、続いて **連携 → Google → 詳細設定 → 会社の委任 → 鍵ファイルを取り込む** を使います。この別経路は鍵を保護し、対応 macOS または Windows キーチェーンが必要です。Linux と Docker は対象外です。許可ドメインと社員の接続タブで同僚を設定します。[設定と安全モデル](google-workspace.md#ai-社員ごとに別の-google-アカウントを使う会社の委任v1760)も参照してください。
6. サービスチェックで選択アカウントと各サービスの結果を確認します。認可を取得してもデータの読み取り成功は保証されません。

会社の委任設定では同僚のメールドメインを制限できます。Enterprise の個人接続には別のアカウントドメイン制限があります。設定方法は [Workspace リファレンス](google-workspace.md)を参照してください。

## Apps Script ブリッジ

1. 操作したい Google アカウントで [Apps Script](https://script.google.com/) を開き、新規プロジェクトを作成します。
2. [ブリッジテンプレート](../../../templates/apps-script/duduclaw-bridge.gs)を貼り付けます。パスワード管理ツールで 30 文字以上のランダムなパスワードを生成するか、`openssl rand -base64 32` を実行し、`CHANGE_ME_TO_A_LONG_RANDOM_STRING` を置き換えます。
3. **Deploy → New deployment → Web app** で **Execute as: Me**、**Who has access: Anyone** を選びます。「Google hasn't verified this app」が出たら、自分のプロジェクトであることを確認し、Advanced、Go to（プロジェクト名）の順に選びます。同意画面の権限をすべて選択して続行します。
4. `/exec` URL をコピーします。`/dev` は所有者のブラウザー用です。
5. ウィザードで **個人 Gmail：メール、カレンダー、スプレッドシート** を選び、URL とシークレットを保存します。他の経路が有効なら切り替えを確認します。サービスチェックへ進み、**サービスチェックを実行** を押してアカウントを確認します。
6. URL とシークレットは秘密にします。更新時はスクリプトのシークレットを変え、**Deploy → Manage deployments** で既存のデプロイを編集し、**New version** を選んでデプロイしてから、ダッシュボードのシークレットを更新します。この方法では URL が変わりません。New deployment は新しい URL を作ります。既存のスクリプトもこの方法で再デプロイすると、新テンプレートのアカウント確認が適用され、実行者 Me のアカウントを返します。Apps Script の日次割当量は特に個人アカウントで制限されます。

Gmail 検索・読取・下書き、Calendar 一覧・作成、Sheets 読取・追記に対応します。`status` を含め八アクション、三サービスです。Drive、Docs、Slides、Forms、Tasks とメール送信には対応しません。公開 URL へのアクセスをシークレットで制限し、ダッシュボードはシークレットを暗号化保存します。

## サービスチェックと完了

1. ウィザードで **サービスチェックを実行** を押し、表示アカウントを確認します。OAuth とサービスアカウントでは Gmail アカウント、Calendar 一覧、Drive アカウント情報を確認します。実測済みサービス、失敗理由、利用可能な API 有効化リンクが表示されます。
2. これらのチェックが通り、Sheets と Docs の必要なスコープが付与されていれば、**完了** を選んで設定を終えます。完了画面には成功と表示され、Sheets と Docs は権限付与済みで文書は未読であることも示します。Slides、Forms、Tasks はここでは確認しません。
3. Apps Script のサービスチェックでは実行アカウントとスクリプトの状態を確認し、メールや表は読みません。通過後に **完了** を選びます。
4. 対象社員に Google ツールが表示されるか確認します。表示されない場合は Google ページで連携が有効か確認し、社員の Google 接続設定を確認します。

## 問題の解決
Google 独自のエラーページが出ると、ダッシュボードは待機し続け、エラー理由は届きません。Google のタブを閉じ、下表に従って修正してからダッシュボードでやり直します。接続済みなら **詳細設定 → 認証情報を編集 → Google を接続** の順に選びます。

| 結果 | 対応 |
|---|---|
| `SERVICE_DISABLED` / `accessNotConfigured` | 表示された Console リンクでクライアントのプロジェクトの API を有効にしてサービスチェックを再実行 |
| スコープ不足 | **認証情報を編集 → Google を接続** を選び、権限をすべて選択 |
| `access_denied` | テストユーザー一覧にないアカウントで発生。Audience → Add users に実際のアカウントを追加してやり直す |
| `admin_policy_enforced` | Workspace 管理者にアプリとサービスの許可を依頼 |
| `invalid_scope` | Data Access、委任、ダッシュボードのスコープ一覧を比較 |
| `needs_reconnect` / 更新時 `invalid_grant` | **認証情報を編集 → Google を接続** を選択。Testing の期限、取り消し、トークン失効を確認 |
| `redirect_uri_mismatch`（Error 400） | Google の Authorized redirect URIs と DuDuClaw の URI を、ポート、パス、末尾のスラッシュまで一文字ずつ比較 |
| `org_internal` | Internal アプリに組織外のアカウントを使用。同じ組織のアカウントを使う |
| 待機がタイムアウトする | 画面のポートとリモート SSH 転送を確認し、Google のタブを閉じてやり直す |
| その他の `403` | 表示された理由コードで調査。再認可では無効な API は有効にならない |
| 委任の `unauthorized_client` | 数値 Client ID、全スコープ、管理者の認可を確認 |

このガイドの接続方法を使ってください。IMAP/SMTP の設定は不要です。

## 要求スコープ

`calendar.calendarlist.readonly` はカレンダー一覧の確認に使います。[calendarList.list の認可要件](https://developers.google.com/workspace/calendar/api/v3/reference/calendarList/list)を参照してください。`calendar.events` だけでは不足します。下のカンマ区切り一覧はウィザードのコピーボタンと同じ形式です。

```text
https://www.googleapis.com/auth/gmail.readonly,https://www.googleapis.com/auth/gmail.compose,https://www.googleapis.com/auth/calendar.events,https://www.googleapis.com/auth/calendar.calendarlist.readonly,https://www.googleapis.com/auth/spreadsheets,https://www.googleapis.com/auth/drive.readonly,https://www.googleapis.com/auth/documents,https://www.googleapis.com/auth/presentations.readonly,https://www.googleapis.com/auth/forms.body.readonly,https://www.googleapis.com/auth/forms.responses.readonly,https://www.googleapis.com/auth/tasks,https://www.googleapis.com/auth/userinfo.email
```

ツールと安全モデルは [Workspace リファレンス](google-workspace.md)、Enterprise 本人同意は[個人接続](google-personal-connections.md)を参照してください。Google Chat の認証情報は別です。[Chat 設定](channels-googlechat-teams.md)を参照してください。
