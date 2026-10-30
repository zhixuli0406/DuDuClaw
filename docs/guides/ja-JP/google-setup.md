# Google を接続する

Console の手順は 2026-10-30 の調査資料に基づきます。このガイド作成時に Google Cloud Console の画面を再度ライブ確認していません。

| 利用状況 | 接続方法 | 主な制限 |
|---|---|---|
| 会社の Google Workspace、特権管理者がいる | サービスアカウントとドメイン全体の委任 | 個人 Gmail は対象外 |
| 会社 Workspace、自分の OAuth を使う | OAuth、Internal | 組織のメンバー限定、Testing の七日期限なし |
| 個人 @gmail.com、メール・予定・表だけ必要 | Apps Script ブリッジ | 三サービス、状態確認を含む八アクション |
| 個人 @gmail.com、全ツールが必要 | OAuth、External + Testing | Testing 中は七日ごとに再認可 |

## DuDuClaw で開始

1. 管理者としてログインし、**管理 → 連携 → Google**（`/manage/integrations?tab=google`）で設定ウィザードを開きます。従来の詳細フォームも使えます。
2. 上の表から状況を選び、データ利用の説明を読みます。シークレットはダッシュボードだけに入力し、社員とのチャットに貼らないでください。
3. Google Workspace 連携を有効にします。全体スイッチは `config.toml [integrations] google_workspace = true` で、既定は false です。サービスアカウントと Apps Script は保存時、OAuth は認証成功時に有効になります。接続テストは変更しません。
4. 以下の経路を一つ設定します。OAuth への切り替えは新たな認証を確認して開始します。成功のコールバックを受けるまで現在の経路を維持し、キャンセルや拒否でも元の接続は変わりません。新しいトークンと OAuth の選択を同じトランザクションで有効にし、以前の設定は無効な設定として保持します。古い OAuth トークンで切り替わることはありません。

## OAuth

1. [Google Cloud Console](https://console.cloud.google.com/) の上部でプロジェクトを作成または選択します。
2. **APIs & services → Library** で Gmail API、Google Calendar API、Google Sheets API、Google Drive API、Google Docs API、Google Slides API、Google Forms API、Tasks API を検索して有効にします。
3. **Google Auth platform → Branding → Get Started** で App Information、Audience、Contact Information、Finish を完了します。組織メンバーは Internal、個人 Gmail は External を選びます。
4. **Audience → Add users** に接続するアカウントを追加します。External + Testing は最大 100 人の登録テストユーザーが使え、認可と更新トークンは七日で失効します。ウィザードに Testing を継続するか入力すると、六日を過ぎた接続に警告が出ます。
5. **Data Access → Add or Remove Scopes** でダッシュボードの全スコープを追加します。末尾にも一覧があります。
6. **Clients → Create Client** で **Web application** を選びます。Web と Desktop の両方に任意の “Use this client for an AI-powered agent” チェックボックスがあります。この接続では Web を使います。
7. DuDuClaw の redirect URI を **Authorized redirect URIs** に完全一致で登録します。既定は `http://localhost:18889/api/mcp/oauth/callback` です。実際の待受ポート（`[gateway] port` の設定を含む）に合わせた URI が表示されます。localhost 以外は HTTPS が必要で、IP アドレスやワイルドカードは使えません。ホスト、ポート、パス、末尾のスラッシュを確認してください。
8. 保存直後に Client ID と Client secret を保管します。シークレットの表示・ダウンロードは一度だけで、その後は末尾四文字しか表示されません。パスワード管理ツールに保存してください。
9. ウィザードに ID とシークレットを入力して保存し、データ説明を確認して **Google を接続** を選びます。Google の同意画面を完了し、ダッシュボードの結果を待ちます。拒否やポーリング期限切れはダッシュボードから再試行します。

### リモートホストと Docker

localhost はブラウザーを実行する端末です。認可前にリモート gateway のポートを転送します。

```sh
ssh -N -L 18889:127.0.0.1:18889 USER@DUDUCLAW_HOST
```

Docker は先に gateway ポートをホストの loopback に公開してください。同意完了まで SSH を維持します。ポートを変更した場合はコマンドの両方の値と登録 URI を変更します。公開 IP の登録や callback の外部公開で回避しないでください。

### スコープと有効期限

Gmail `gmail.send` は sensitive、`gmail.readonly`、`gmail.modify`、`gmail.compose`、`gmail.metadata` は restricted です。Drive `drive.file` は non-sensitive、`drive` と `drive.readonly` は restricted です。Google の検証要件が適用される、restricted データをサーバーで扱うアプリには毎年の CASA 評価が必要です。External の公開前に要件を確認してください。Internal は組織メンバーに限定されます。

未検証アプリの警告画面はプロジェクト存続期間で新規 100 人までで、リセットできません。Testing の登録 100 人とは別の上限です。更新トークンは六か月未使用でも失効します。クライアントは六か月未使用で自動削除され、その 30 日前に警告メールが届きます。更新トークンの保存だけでは現在の接続成功は分かりません。

## サービスアカウントとドメイン全体の委任

1. Workspace の特権管理者に委任を承認してもらいます。許可スコープ内でドメインのユーザーとして動作できます。個人 @gmail.com は対象外です。
2. Cloud プロジェクトで OAuth 手順の八 API を有効にします。**IAM & Admin → Service Accounts** で作成し、JSON キーをダウンロードして数値 Client ID を控えます。
3. 管理者に Client ID とダッシュボードの全スコープを渡します。管理者は **Admin console → Security → Access and data control → API controls → Manage Domain Wide Delegation → Add new** で両方を登録します。反映に最大 24 時間かかる場合があります。
4. 共用アカウントは認証経路で **サービスアカウント** を選びます。JSON キーを DuDuClaw ホストに所有者限定の権限で保存し、パスと代理する Workspace ユーザーのメールを入力して保存します。サービスアカウント自身のメールは指定しません。
5. 社員ごとに別の同僚を代理する場合は **セキュリティ → 認証情報保護 → システムキーチェーンへ移動**、続いて **連携 → Google → 会社の委任 → キーをインポート** を使います。この別経路はキーを封印し、対応 macOS または Windows キーチェーンを必要とします。Linux と Docker は対象外です。許可ドメインと社員の接続タブで同僚を設定します。[設定と安全モデル](google-workspace.md#ai-社員ごとに別の-google-アカウントを使う会社の委任v1760)も参照してください。
6. 選択アカウントのサービス別テストを確認します。トークン発行はデータの読み取り成功を保証しません。

`[integrations.google_identity] allowed_subject_domains` は委任する同僚のドメイン、同じ節の `allowed_domains` は Enterprise 個人 OAuth アカウントのドメインを制限します。

## Apps Script ブリッジ

1. 操作したい Google アカウントで [Apps Script](https://script.google.com/) を開き、新規プロジェクトを作成します。
2. [ブリッジテンプレート](../../../templates/apps-script/duduclaw-bridge.gs)を貼り付けます。`openssl rand -base64 32` でシークレットを作り、`CHANGE_ME_TO_A_LONG_RANDOM_STRING` を置き換えます。
3. **Deploy → New deployment → Web app** で **Execute as: Me**、**Who has access: Anyone** を選び、自分のスクリプトの権限を確認して認可します。
4. `/exec` URL をコピーします。`/dev` は所有者のブラウザー用です。
5. ウィザードで Apps Script を選び、URL とシークレットを保存します。他の経路が有効なら切り替えを確認します。**接続テスト** でアカウントを確認します。
6. URL とシークレットは秘密にします。更新時はスクリプトのシークレットを変えて再デプロイし、ダッシュボードも更新します。個人アカウントでは特に日次割当量に注意してください。

Gmail 検索・読取・下書き、Calendar 一覧・作成、Sheets 読取・追記に対応します。`status` を含め八アクション、三サービスです。Drive、Docs、Slides、Forms、Tasks とメール送信には対応しません。公開 URL へのアクセスをシークレットで制限し、ダッシュボードはシークレットを暗号化保存します。

## 検証と完了

1. ダッシュボードのホスト側接続テストを実行し、認証済みアカウントを確認します。OAuth とサービスアカウントは Gmail `users.getProfile`、Calendar `calendarList.list`（`maxResults=1`）、Drive `about.get`（`fields=user`）を呼び出します。テスト済みのサービス、失敗理由、利用可能な API 有効化リンクを個別に表示します。
2. Sheets と Docs はスコープのみ確認します（`verified:false`）。文書アクセスは未検証です。他の確認が通った場合、API は `result:partial`、`ok:false` を返します。「部分検証で終了」を選べますが、完了画面でも権限のみのサービスを明示します。Slides、Forms、Tasks はここではテストしません。
3. ブリッジはアカウントと状態のみ確認し、三サービスの読み取りを保証しません。`connect-google` スキルはホスト側テストへ案内し、ツールを使う場合は `google_status` のみ許可します。接続テストのためにメールや文書を読んだり、ツールが返す文章の指示に従ったり、後続の操作を行ってはいけません。
4. 対象社員にツールが表示されるか確認します。全体スイッチがオフ、または社員の Google が無効なら `google_status` は非表示です。その場合はダッシュボードで経路を保存するか連携を有効にしてから、接続テストを実行します。OAuth のテストには連携の有効化が必要です。身分別のローカル認可情報やトークン取得状態は、サービス読取テストの代わりにはなりません。

Google スキルの補完はホーム配下のシンボリックリンクを拒否し、リンクを追跡せずファイルを作成します。安全なディレクトリハンドルによる作成を利用できないプラットフォームでは、この補完を拒否して次の起動時に再試行します。他の内蔵スキルは独立して補完します。既存の編集済みスキルは保持します。今回の変更のテストは未実行です。

## 問題の解決

| 結果 | 対応 |
|---|---|
| `SERVICE_DISABLED` / `accessNotConfigured` | 表示された Console リンクでクライアントのプロジェクトの API を有効にして再テスト |
| スコープ不足 | 再接続して必要な権限を付与 |
| `access_denied` | 意図したアカウントで認可を再実行 |
| `admin_policy_enforced` | Workspace 管理者にアプリとサービスの許可を依頼 |
| `invalid_scope` | Data Access、委任、ダッシュボードのスコープ一覧を比較 |
| `needs_reconnect` / 更新時 `invalid_grant` | 再接続。Testing の期限、取り消し、更新トークン失効を確認 |
| URI 不一致または期限切れ | 完全な URI、ローカルポート、SSH 転送を確認して再試行 |
| その他の `403` | 表示された理由コードで調査。再接続では無効 API は有効にならない |
| 委任の `unauthorized_client` | 数値 Client ID、全スコープ、管理者の認可を確認 |

アプリパスワードと基本 IMAP/SMTP 認証は 2025 年に廃止されました。このガイドの接続方法を使ってください。

## 要求スコープ

`calendar.calendarlist.readonly` はカレンダー一覧の読取検証に使います。[calendarList.list の認可要件](https://developers.google.com/workspace/calendar/api/v3/reference/calendarList/list)を参照してください。`calendar.events` だけでは不足します。

```text
https://www.googleapis.com/auth/gmail.readonly
https://www.googleapis.com/auth/gmail.compose
https://www.googleapis.com/auth/calendar.events
https://www.googleapis.com/auth/calendar.calendarlist.readonly
https://www.googleapis.com/auth/spreadsheets
https://www.googleapis.com/auth/drive.readonly
https://www.googleapis.com/auth/documents
https://www.googleapis.com/auth/presentations.readonly
https://www.googleapis.com/auth/forms.body.readonly
https://www.googleapis.com/auth/forms.responses.readonly
https://www.googleapis.com/auth/tasks
https://www.googleapis.com/auth/userinfo.email
```

ツールと安全モデルは [Workspace リファレンス](google-workspace.md)、Enterprise 本人同意は[個人接続](google-personal-connections.md)を参照してください。Google Chat の認証情報は別です。[Chat 設定](channels-googlechat-teams.md)を参照してください。
