# Google を接続する

ダッシュボードをパスのプレフィックス配下で公開する場合は、`config.toml` に `[dashboard] public_url = "https://host/agents"` を設定し、パスも含めてください。nginx の例：`location /agents/ { proxy_pass http://127.0.0.1:18889/; }`。Google Cloud には `https://host/agents/api/me/google/callback` を登録します（リモート MCP は `https://host/agents/oauth/mcp/callback`）。公開 URL とブラウザーの検証済みダッシュボード origin が一致する場合だけ、このプレフィックスを OAuth に使用します。LAN アクセスと URL の貼り付けフローの動作は変わりません。修正を含むバージョンへの更新と登録済みコールバック URL の変更後は、v1.79.1 用に追加したプレフィックスなしの `/api/me/google/callback` と `/me/connections` の nginx 回避ルールを削除できます。

Gmail の検索と読取では、AI 従業員に渡す前にログインリンクと近くのワンタイムコードをマスクします。サードパーティ MCP、Remote MCP、Bash は対象外です。

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


### 会社の Google ログイン

会社ログインは設定済みの会社 Google OAuth クライアント ID とシークレットを使用します。サービスアカウントの委任や Apps Script ではログインできません。既定は無効で、管理者のリスク確認と許可する `hd` ドメインが必要です。個人版では利用できません。ダッシュボード画面は未実装です。Google 設定が提供されたら、そこからログインを設定してください。

`[dashboard] public_url` に外部から接続できるダッシュボードのベース URL を設定し、パスのプレフィックスも含めます。HTTPS を使ってください。HTTP は localhost またはループバックだけ利用できます。Google Cloud の Web OAuth クライアントの **Authorized redirect URIs** に `<public_url>/api/login/google/callback` を完全一致で登録します。`public_url = "https://company.example/agents"` なら `https://company.example/agents/api/login/google/callback` です。ベース URL の末尾にはスラッシュを付けません。個人接続やリモート MCP のコールバックは別途残してください。

会社の本番利用で Internal を選べるのは、全員が Google Cloud プロジェクト自身の Workspace／Cloud Identity 組織に所属する場合だけです。DuDuClaw の許可ドメインは Google の対象ユーザーを拡大しません。他組織のユーザーには External が必要で、本番公開と必要な Google 検証を行います。会社ログインの本番運用で同意画面を Testing のままにしないでください。通常 Testing は登録済みテストユーザー 100 人までで、認可と更新トークンは 7 日で失効します。Google は基本的な本人情報のスコープだけを要求する場合を例外としています。このログインは online アクセスで `openid email profile` のみ要求します。個人 Google 接続はサービス用スコープも要求するため、その例外に該当しません。[Google の対象ユーザールール](https://support.google.com/cloud/answer/15549945?hl=en)を参照してください。

ログイン連携と本人の個人 Google 接続は同じ Google アカウント（`sub`）を使う必要があります。既存の個人接続が別アカウントならログイン連携を拒否します。ログインだけでは Gmail、Drive などの個人サービスを接続しません。[ログインの安全性](../../features/ja-JP/05-security-defense.md#会社の-google-ログイン)と[設定](dashboard-settings.md#会社の-google-ログイン)も参照してください。

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
5. 社員ごとに別の同僚を代理する場合は **セキュリティ → 認証情報保護 → システムのキーチェーンへ移す**、続いて **連携 → Google → 詳細設定 → 会社の委任 → 鍵ファイルを取り込む** を使います。この別経路は鍵を保護し、対応 macOS または Windows キーチェーン、または person-credentials オーバーレイで起動した Docker（[Docker ガイド](docker.md#74-個人アカウントperson-credentials-オーバーレイ)。ボタン名は「認証情報の保護を有効にする」）が必要です。[ネイティブ Linux の保護システムサービス](linux-protected-service.md)も対応し、同じ「認証情報の保護を有効にする」を使います。通常の Docker インストールと Linux ユーザーサービスは対象外です。許可ドメインと社員の接続タブで同僚を設定します。[設定と安全モデル](google-workspace.md#ai-社員ごとに別の-google-アカウントを使う会社の委任v1760)も参照してください。
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

## ドライブへの書き込み認可を準備する

作成、変更、移動、ゴミ箱への移動は管理者が個別に許可し、既定はすべてオフです。有効化や制限の緩和にはリスクへの明示的な同意が必要で、管理者と時刻を記録します。追加の再認証は要求しません。各操作は既定でメンバー本人の確認が必要です。管理者は操作ごとに確認を無効にするか、管理者の追加承認を要求できます。フォルダリストが空なら、作成と変更はアカウントが書き込めるすべての場所で可能ですが、移動とゴミ箱への移動は使えません。ダッシュボード操作機能は本リリースの設定ページとともに提供予定で、書き込み設定と個人の書き込み接続のブロックはまだ実装されていません。

ドメイン全体の委任では、Workspace の特権管理者が **Google 管理コンソール → セキュリティ → アクセスとデータ管理 → API の制御 → ドメイン全体の委任を管理** で、サービスアカウントの認可リストに次の三つを追加します。他のサービスに必要な既存スコープも保持してください。

```text
https://www.googleapis.com/auth/drive
https://www.googleapis.com/auth/documents
https://www.googleapis.com/auth/spreadsheets
```

これらは書き込み用です。通常の `drive.readonly` では書き込めません。委任を使う同僚が個別に書き込み同意を行う必要はありません。Google 管理コンソールの変更後に実際にトークンを取得できるかは、Workspace での受け入れ確認が必要です。

OAuth による会社共用接続では、管理者が会社アカウントについて追加の Google 書き込み同意を一度行います。資格情報は通常の接続とは別に保存します。社員が同僚自身の Google 接続を使う場合は、各本人が二回目の書き込み同意を行ってください。[個人接続](google-personal-connections.md)を参照してください。

会社共用接続でサービスアカウントを使う場合、鍵ファイルを厳重に管理してください。完全な Drive スコープを認可した後は、CLI が読める鍵で DuDuClaw の書き込みスイッチとは独立して書き込みトークンを発行できる可能性があります。gateway に封印保存する委任鍵を優先してください。鍵に無制限にアクセスできるプロセスは製品内の確認では隔離できません。

Apps Script の会社接続と保護モードは Drive 書き込みに対応しません。ツールは非表示になり、呼び出しも拒否します。既存の Docs／Sheets 書き込みツールは従来の規則を維持します。[書き込みツールのリファレンス](google-mcp.md#クラウドドライブへの書き込みツール)を参照してください。
