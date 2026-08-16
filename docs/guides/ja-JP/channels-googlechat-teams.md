# Google Chat & Microsoft Teams チャネル

DuDuClawはGoogle ChatとMicrosoft Teamsを、Telegram／LINE／Discord／Slack／
WhatsApp／Feishu／WebChatと並ぶ一級チャネルとしてサポートしています。どち
らもwebhookベースで、gatewayが公開HTTPS経由で到達可能である必要がありま
す。

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

### セットアップ

1. Google Cloudプロジェクトを作成(または既存のものを再利用)し、
   **Google Chat APIを有効化**する。**プロジェクト番号**(project
   number、IAM & Admin → Settingsに表示)を控えておく。
2. 同じプロジェクトに**サービスアカウント**を作成し、そのJSONキーをダウ
   ンロードする。ドメイン全体委任(domain-wide delegation)は不要で、
   Chat app自体がprincipal(scopeは`chat.bot`)になる。
3. Chat APIの**Configuration**設定ページを開く。app名／アバター／説明
   を設定し、*Interactive features*を有効化、*Receive 1:1 messages*と
   *Join spaces and group conversations*にチェックを入れ、**HTTP
   endpoint URL**を`https://<your-host>/webhook/googlechat`に設定する。
4. *Authentication Audience*で**Project number**を選択する。
5. DuDuClawを設定する(`config.toml`、またはダッシュボード → チャネル →
   `googlechat`を追加):

```toml
[channels]
googlechat_project_number = "123456789012"
# Paste the full service-account JSON (stored encrypted as *_enc)
googlechat_service_account_json = '{ "type": "service_account", ... }'
```

6. gatewayを再起動する。ログに
   `✅ Google Chat webhook ready at /webhook/googlechat`と表示されれば
   成功。

### 補足

- 受信リクエストは常にfail-closedで検証されます。`Authorization:
  Bearer`のJWTは`chat@system.gserviceaccount.com`が署名したものであ
  り、audienceは自分のプロジェクト番号と一致している必要があります。
- 返信は`spaces.messages.create`経由で非同期に送信されます(同期ウィン
  ドウはわずか30秒しかなく、agentタスクには短すぎるため)。スレッドは
  `REPLY_MESSAGE_FALLBACK_TO_NEW_THREAD`によって正しく紐づけられます。
- 未公開のChat appは自分のWorkspace組織内からしか見えません。より広い
  配布範囲が必要な場合はGoogle Workspace Marketplaceへの公開が必要で
  す。

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
