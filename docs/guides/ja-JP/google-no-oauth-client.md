# Google Workspace without creating an OAuth client

デフォルトの経路（[google-workspace.md](../google-workspace.md)）では、顧客ごとにGoogle Cloud OAuthクライアントを作成してもらう必要があります。この手順を省略できる代替経路が2つあります。アカウントの種類で選んでください。IT管理者のいるWorkspaceドメインならドメイン全体委任、個人の`@gmail.com`アカウントならApps Scriptブリッジです。

もう存在しない経路が1つあります。**IMAP/SMTP経由のアプリパスワード**です。Googleは2025年3月にすべてのGoogleアカウントで基本認証を無効化しました（Workspaceは2025年5月に完了）。そのためIMAP、POP、SMTP、CalDAV、CardDAVはすべてOAuthが必須になっています。アプリパスワードの生成を勧めるガイドを見かけたら、それは古い情報です。

---

## Option A — service account with domain-wide delegation

**Google Workspaceドメインが必要です。** 個人の`@gmail.com`アカウントはどのドメインにも属さないため、なりすましの対象にできません。この場合はOption Bを使ってください。

サービスアカウントはDuDuClawを運用する側が所有します。顧客のスーパー管理者がそのclient idを一度承認するだけで、以降DuDuClawはそのドメイン内のユーザー宛にトークンを発行できます。同意画面が出ることもなく、重要な点として**Googleのアプリ審査やCASAレビューも一切不要**です。これこそがOAuth経路が開発者自身のドメイン以外の顧客に提供できない理由でもあります。

### 顧客への説明

正直に伝えるべきことがあります。ドメイン全体委任は、認可されたclientが承認されたスコープの範囲内でドメイン内の任意のユーザーになりすませることを意味します。Google自身の[ベストプラクティスガイド](https://support.google.com/a/answer/14437356)も、管理者に対してサードパーティへの委任は慎重にと呼びかけていますし、複数人承認（2024年8月から利用可能）を有効にしている組織では、もう1人のスーパー管理者による承認が必要です。質問攻めにされることも、断られることも想定しておいてください。

### Setup

1. Google Cloudプロジェクトでサービスアカウントを作成し、JSON鍵をダウンロードします。サービスアカウント詳細ページに表示される数値の**client id**を控えておきます。
2. 鍵ファイルをDuDuClawのホストに保存し、権限を絞ります。

   ```bash
   mkdir -p ~/.duduclaw/keys && mv ~/Downloads/sa-key.json ~/.duduclaw/keys/google-sa.json
   chmod 600 ~/.duduclaw/keys/google-sa.json
   ```

3. 顧客のスーパー管理者にclient idと以下のスコープ一覧を送ります。管理者は**Admin console → Security → Access and data control → API
   controls → Manage Domain Wide Delegation → Add new**を開き、client idとスコープを貼り付けて保存します。反映は通常すぐですが、Googleは最大24時間まで許容しています。
4. DuDuClawを設定します。dashboardから設定するか、手動で編集します。

   **Dashboard**（再起動不要）：管理 → 連携／ツール接続 → Google → 認証方式 →
   **サービスアカウント**。鍵ファイルのパスとなりすます対象のユーザーを入力し、保存を押してから接続テストを押します。実際にトークンを発行するため、緑色の結果が出れば管理者の承認が本当に反映されている証拠です。スコープ一覧の隣にはコピー用のボタンがあります。

   **手動設定：**

   ```toml
   [integrations]
   google_workspace = true

   [integrations.google_service_account]
   key_file = "keys/google-sa.json"   # relative paths resolve against ~/.duduclaw
   subject  = "boss@customer.com"      # the Workspace user to act as
   ```

   設定ファイルを手動で編集した場合はgatewayの再起動が必要です。dashboardは同じセクションに書き込むため、次のツール呼び出しから反映されます。

5. `google_status`ツールを実行すると、`Credential source: direct API
   token`と表示されます。

管理者に渡すスコープ一覧です。カンマ区切りの1行として貼り付けてください。

```
https://www.googleapis.com/auth/gmail.readonly,https://www.googleapis.com/auth/gmail.compose,https://www.googleapis.com/auth/calendar.events,https://www.googleapis.com/auth/spreadsheets,https://www.googleapis.com/auth/drive.readonly,https://www.googleapis.com/auth/documents,https://www.googleapis.com/auth/presentations.readonly,https://www.googleapis.com/auth/forms.body.readonly,https://www.googleapis.com/auth/forms.responses.readonly,https://www.googleapis.com/auth/tasks,https://www.googleapis.com/auth/userinfo.email
```

### うまくいかないとき

`unauthorized_client`が出るときは、ほとんどの場合Admin console側のスコープ一覧とDuDuClawが要求する内容が完全には一致していません。Googleは集合全体を比較するため、1つでも足りない項目があれば発行に失敗します。エラーメッセージにはclient idが含まれているので、管理者が入力した内容と突き合わせて確認できます。

設定済みだが設定ミスのあるサービスアカウントは、エラーとしてはっきり報告されます。OAuth vaultにたまたま保存されている別のトークンにこっそりフォールバックすることはありません。あなたがこの認証情報を明示的に要求している以上、設定ミスはどんなトークンが偶然存在するかによって覆い隠されるのではなく、見える形で表面化する必要があります。

---

## Option B — Apps Script bridge

**個人の`@gmail.com`でもWorkspaceでも使えます。** ユーザーが自分自身のアカウント内にスクリプトをデプロイし、DuDuClawはそのURLを呼び出します。GoogleからはユーザーがOwnerとして自分のスクリプトを実行しているようにしか見えないため、審査が必要なサードパーティアプリは存在しません。

対応範囲は一部に限られます。**Gmail（検索／読み取り／下書き）、カレンダー（一覧／作成）、スプレッドシート（読み取り／追記）**です。Drive、Docs、Slides、Forms、Tasksはこの経路では利用できず、空の結果ではなく「not available through the Apps Script bridge」という明示的なエラーが返ります。

### Setup

1. <https://script.google.com>を開き、新しいプロジェクトを作成します。
2. ファイルの内容を
   [`templates/apps-script/duduclaw-bridge.gs`](../../../templates/apps-script/duduclaw-bridge.gs)
   に置き換えます。
3. シークレットを生成し、`CHANGE_ME_TO_A_LONG_RANDOM_STRING`の部分に貼り付けます。

   ```bash
   openssl rand -base64 32
   ```

4. **Deploy → New deployment → Web app**で、以下のように設定します。
   - Execute as：**Me**
   - Who has access：**Anyone**
5. Googleが「unverified app」の同意画面を表示します。これは想定どおりで、この未検証アプリの正体はユーザー自身のスクリプトです。**Advanced → Go to (project
   name)**を選んで承認してください。
6. `/exec`のURLをコピーします（`/dev`ではありません。`/dev`はスクリプト所有者自身のブラウザセッションにしか認可されません）。
7. DuDuClawを設定します。dashboardから設定するか、手動で編集します。

   **Dashboard**（推奨。シークレットを暗号化してくれます）：管理 →
   連携／ツール接続 → Google → 認証方式 → **Apps Scriptブリッジ**。`/exec`の
   URLとシークレットを貼り付け、保存を押してから接続テストを押します。緑色の結果にはこのスクリプトを実行しているGoogleアカウントの名前が表示されるため、「間違ったログインでデプロイしてしまった」ケースをいちばん速く見つけられます。後でURLだけ書き換えてシークレットを再入力しなければ、保存済みのシークレットはそのまま維持されます。

   **手動設定**（この場合シークレットは平文で保存されます。dashboardの利用を推奨します）：

   ```toml
   [integrations]
   google_workspace = true

   [integrations.google_apps_script]
   url    = "https://script.google.com/macros/s/AKfyc.../exec"
   secret = "the string you generated in step 3"
   ```

8. `google_status`を実行すると、
   `Credential source: apps-script bridge at script.google.com`と表示されます。

### Security properties

- **URLとシークレットの組み合わせそのものが1つの認証情報です。**「Who has access: Anyone」ということは、このエンドポイントはGoogleへのログインなしに到達できることを意味し、部外者を締め出しているのはシークレットだけです。この組を1つのパスワードのように扱ってください。チャット、issue、スクリーンショットに絶対に貼り付けないでください。
- シークレットはchannel botトークンと同じ方式で、暗号化された状態で保存されます。
- DuDuClawが接続するのは`script.google.com`だけです（リダイレクト先の
  `script.googleusercontent.com`は追跡します）。通信は必ずhttpsで、パスは`/exec`で終わっている必要があります。誤入力や改ざんされた`url`は、シークレットが送信される前に拒否されます。`script.google.com.evil.test`のような紛らわしいホスト名も同様です。
- ローテーションするには、スクリプト側の`SECRET`を変更して再デプロイし、
  `config.toml`を更新してください。古いシークレットは即座に無効になります。
- このブリッジには送信機能がありません。ネイティブツールと同じく、agentができるのは下書きの準備までで、実際の送信は必ず人が行います。

### Quotas

Apps Scriptには1日あたりアカウントごとの上限があり、コンシューマーアカウントほど厳しく制限されます。この経路はインタラクティブなアシスタント用途向けであり、大量の同期処理には向きません。

---

## What was ruled out, and why

**ローカルでAppleScript経由でMail.app／Calendar.appを読む方法。** カレンダーはうまくいっており、すでに出荷済みです（`os_calendar_today`）。1日分の予定を問い合わせるだけなので、クエリが小さく済むからです。メールはそうはいきません。macOS 15上の実際の54,000通が入ったメールボックスで計測したところ、
`Mail.inbox.messages.dateReceived()`は17秒かかり、
`whose({dateReceived: …})`と`whose({readStatus: false})`はどちらも60〜90秒以内に応答を返せませんでした。`inbox.messages`が返す順序も時系列順ではないため、安価なインデックスアクセスでは任意の古いメールが返ってくることになります。Spotlightはメールメッセージをインデックス化していません（`kMDItemKind == 'Mail Message'`は何も返しません）。`~/Library/Mail`を直接読むにはFull Disk Accessに加えて、ドキュメント化されておらずバージョンごとに変わるSQLiteスキーマへの対応が必要です。GmailについてはこちらもOption Bを使います。
