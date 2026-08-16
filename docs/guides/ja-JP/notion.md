# Notion統合(検索 + 読み取り + 追記)

Notionのworkspaceを接続すると、あなたのAI社員はページとデータベース
を検索し、ページの内容を全文読み取り、ページにメモを追記できるように
なります。DuDuClawはNotion REST APIにネイティブに接続するため、サード
パーティのMCP serverをインストールする必要はありません。アクセストー
クンはDuDuClawの暗号化されたOAuth vaultに保存されます。

## 何が使えるようになるか

2つのスコープ(`notion:read` / `notion:write`)で管理される、4個のエー
ジェント向けMCPツールです。

| ツール | 分類 | 内容 |
|------|-------|--------------|
| `notion_status` | read | 接続状態の診断: 接続済みか。ローカルの状態のみを読み取る。 |
| `notion_search` | read | このintegrationに共有されたページとデータベースを検索する(Notionの検索構文はタイトルに一致)。id／title／type／last-edited／urlを返す。 |
| `notion_page_read` | read | 1ページを丸ごと読み取る: メタデータに加え、プレーンテキストに平坦化されたページ本文(paragraph／heading／list／to-do／quote／code／callout／tableなど一般的なblock型に対応、最大約200 block)。 |
| `notion_page_append` | write | 既存のページに新しいparagraph blockとしてテキストを追記する(空行以外の各行が1つのblockになる)。既存の内容を削除・上書きすることは決してない。 |

### 安全設計

- **追記専用の書き込みです。** `notion_page_append`はページ末尾に
  paragraph blockを追加するだけで、削除や上書き用のツールはありませ
  ん。
- **読み取りは読み取りのままです。** read分類のツールはNotion上の何も
  変更できません。
- **明示的な共有が必要です。** あなたのintegrationは、明示的に共有し
  たページ／データベースしか見ることができません(Notion側: ページを
  開く → ••• → Connections → integrationを追加)。それ以外には一切ア
  クセスできません。
- **共有wikiではなく、外部の知識ソースです。** Notionのコンテンツは照
  会と引用のためだけに提供されます。DuDuClawの共有wikiへ自動的にコ
  ピーされることは**決して**なく、2つの知識ストアは分離されたままで
  す。
- **任意の承認ゲート。** より慎重にしたい場合は、書き込みツールを
  agentの`agent.toml [capabilities] approval_required_tools`にリスト
  アップしてください:

  ```toml
  [capabilities]
  approval_required_tools = ["notion_page_append"]
  ```

## 前提条件: Notion OAuth integrationを作成する

自分自身のNotion integrationを用意する必要があります(DuDuClawは共有の
認証情報を同梱しません)。初回セットアップ:

1. [Notion → My integrations](https://www.notion.so/my-integrations)を
   開く。
2. **New integration**をクリックする。integration typeを**Public**に
   設定する(public integrationだけがOAuth client ID／secretを公開す
   る。internal integrationは固定tokenを使い、OAuthフローを持たない)。
3. そのintegrationの**OAuth Domain & URIs**の下に、次のredirect URIを
   正確に追加する:

   ```
   http://localhost:18789/api/mcp/oauth/callback
   ```

4. **OAuth client ID**と**OAuth client secret**をコピーする。
5. AIにアクセスさせたいページ／データベースをそのintegrationに共有す
   る(各ページを開く → ••• → Connections → integrationを選択)。

## ダッシュボードから接続する

1. **Manage → Integrations → Tool servers**(`/manage/integrations`)に
   移動する。
2. **Services that need authorization**までスクロールし、**Notion**
   カードを探す。
3. カードの**Configure**をクリックする。OAuth client IDとsecretを貼り
   付ける。ダイアログには登録すべき正確なcallback URLも表示されるの
   で、上の手順3で入力した値と一致している必要がある。
4. Notionの同意ウィンドウが開く。付与するworkspaceとページを選んで承
   認すると、カードは**Authenticated**に切り替わる。

client認証情報は永続化されます(secretは保存時に暗号化)。これにより、
後で再認可する際にsecretを再入力する必要はありません。

## トークンについて

Notionのアクセストークンは**長期間有効で期限切れになりません**。
Notionはrefresh tokenも発行しません。つまり:

- 接続状態の画面には有効期限が表示されません。これは正常であり、バグ
  ではありません。
- 更新すべきものは何もありません。もしNotion側でトークンが取り消され
  た場合、ツールは`401`を返し、再接続するよう案内します。

## トークン交換の詳細(気になる方向け)

Notionのtoken endpointは一般的なOAuthの慣例とは異なります。form POST
ではなく、**HTTP Basic auth**(`client_id:client_secret`)とJSON body
を要求し、さらにauthorize URLには`owner=user`が必要です。これらはす
べてOAuth層の`notion` providerブランチで自動的に処理されるため、こち
ら側で設定する必要は一切ありません。

## トラブルシューティング

- **「Notion is not connected.」**: トークンが保存されていません。
  ダッシュボードから接続してください。
- **`401 Unauthorized`**: integrationのトークンが取り消されました。再
  接続してください。
- **`403` ／ `404`「not found」**: そのページ／データベースがまだ
  integrationに共有されていません。共有してから(ページ → ••• →
  Connections)再試行してください。

いつでも`notion_status`を実行すれば、その場で診断結果を確認できます。
