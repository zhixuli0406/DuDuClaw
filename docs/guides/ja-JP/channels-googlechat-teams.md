# Google Chat & Microsoft Teams チャネル

Google Chat チャネルの認証情報は Chat アプリ（`chat.bot`）用で、Gmail、Drive、Calendar は接続しません。データツールは [Google 接続](google-setup.md)で別途設定します。ダッシュボードの保存は即時反映し、設定ファイルを手編集した場合は再起動します。

DuDuClawはGoogle ChatとMicrosoft Teamsを、Telegram／LINE／Discord／Slack／
WhatsApp／Feishu／WebChat と並ぶチャネルとしてサポートしています。Teams と Google Chat は公開 HTTPS URL が必要です。

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

## Google Chat

### 前提条件

Chat が有効な Google Workspace Business／Enterprise アカウントと、Chat アプリを許可する管理者が必要です。Cloud プロジェクトの作成または直接所有、Chat API の有効化、サービスアカウントとキーの作成ができる必要があります。ローカルでは Bash、Python 3、`gcloud` と自分の `gcloud auth login` が必要です。Cloud Shell にはこれらのツールがあります。gateway は公開 HTTPS URL が必要です。公開 URL が不要な Pub/Sub モードは今後のバージョンで提供予定です。

### ワンステップで作成・インポート

新しい専用 Google Cloud プロジェクトを推奨します（既定の動作）。DuDuClaw ホストで実行します。

自動インポートには稼働中の DuDuClaw gateway と管理者ログインが必要です（MFA が必要な場合はその認証も行います）。

```bash
# HTTP: 自分の公開 gateway アドレスに置き換えます。
duduclaw channels googlechat bootstrap --endpoint-url https://chat.example.com/webhook/googlechat
```

プロジェクト、Chat API、サービスアカウントを準備し、最後のクラウド操作で非公開 JSON キーを作成してから設定をインポートします。`--project-id your-project-id` は直接所有する既存プロジェクトを再利用します。グループや継承による所有権は安全側で拒否します。追加オプションは `--app-name`、`--agent`、`--out`、`--dry-run`（クラウドもローカルも変更しません）です。既定 ID は `duduclaw-chat-<random6>`。キーは専用出力ディレクトリの `service-account.json` に 0600 で保存します。再実行では一致するキーを再利用します。インポートした認証情報は暗号化して保存します。作成後に後続のスクリプト処理が失敗した場合、新しいクラウドキーの取り消しとローカルファイルの削除を試みます。取り消しに失敗した場合やキー ID を読み取れない場合は、手動対応が必要であることを表示します。 スクリプト成功後に CLI のインポートが失敗した場合、CLI はキーファイルを保持して非ゼロで終了します。新しいサービスアカウントキーは引き続き有効です。エラーにはファイルパス、再インポートと手動取り消しの完全なコマンドが表示されます。CLI はキーの取り消しやキーファイルの削除を自動実行しません。

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

### キーポリシー

キー作成が `FAILED_PRECONDITION` になった場合は、組織ポリシー管理者にプロジェクト例外の審査を依頼します。2024-05-03 以降に作成した組織は通常キー作成を制限します。スクリプトは次を表示して停止し、自動でポリシーを変えません。

```bash
gcloud resource-manager org-policies disable-enforce iam.disableServiceAccountKeyCreation --project=your-project-id
gcloud resource-manager org-policies disable-enforce iam.managed.disableServiceAccountKeyCreation --project=your-project-id
```

managed constraint はインストール済み gcloud の対応を管理者が確認し、必要なら Organization Policies 画面で変更します。[Google のキーポリシーガイド](https://cloud.google.com/iam/docs/troubleshoot-org-policies)も参照してください。

### 付録：手動設定

プロジェクト、Chat API、サービスアカウントと JSON キーを作成し、上記の Chat 設定を行います。「チャネル → Google Chat を追加 → 手動設定」にプロジェクト番号と JSON キー全体を入力します。保存後にメッセージをテストします。HTTP は `chat@system.gserviceaccount.com` とプロジェクト番号の JWT を検証します。未対応の Workspace アドオン形式には、アドオン設定を外すよう診断を表示します。

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
| Google Chat | Chat markup(`*bold*`、`<url\|text>`) | 等幅code fence | プレースホルダー + その場での編集 |
| MS Teams | markdownアクティビティ | 等幅code fence | `typing`アクティビティを3秒ごとに送信 |
| WebChat | 生のmarkdown(ダッシュボード側でレンダリング) | ネイティブ対応 | `progress` WSイベント |

## グループ会話コンテキストの設定

Admin が「チャンネル → グループ設定」でグループごとに保存を有効化します。Telegram は BotFather の `/setprivacy` を無効にするか、bot 管理者として受信します。Slack は `message.channels`／`message.groups` と `channels:history`／`groups:history` が必要で、変更後は App を再インストールしチャンネルへ招待します。Discord は MESSAGE CONTENT INTENT が必要です。Google Chat と Teams は App へのメンションのみを受信する場合があります。Teams の全チャンネル受信には `ChannelMessage.Read.Group` などの RSC が必要です。保存期間と参照権限は[グループ会話コンテキスト](../../features/ja-JP/72-group-context.md)を参照してください。本変更で実際のプラットフォーム送受信は未検証です。
