# GitHub統合(issue/PR検索 + 読み取り + コメント)

GitHubアカウントを接続すると、あなたのAI社員はissueとpull requestを検
索し、全文を読み、コメントを投稿できるようになります。DuDuClawは
GitHub REST APIにネイティブに接続するため、サードパーティのMCP server
をインストールする必要はありません。アクセストークンはDuDuClawの暗号
化されたOAuth vaultに保存されます。

## 何が使えるようになるか

2つのスコープ(`github:read` / `github:write`)で管理される、5個のエー
ジェント向けMCPツールです。

| ツール | 分類 | 内容 |
|------|-------|--------------|
| `github_status` | read | 接続状態の診断: 接続済みか、付与されたスコープ。ローカルの状態のみを読み取る。 |
| `github_search_issues` | read | GitHubの検索構文(`repo:owner/name is:open label:bug`)でissueとPRを検索する。repo／number／title／state／is_pr／updated／urlを返す。 |
| `github_issue_read` | read | 1件のissueを読み取る: タイトル、状態、作者、本文(長い場合は切り詰め)、直近10件のコメント。 |
| `github_pr_read` | read | 1件のPRを読み取る: メタデータ(base／head／state／merged／mergeable)と変更ファイル一覧(filename／status／additions／deletions、最大50ファイル)。diffの内容は取得しない。 |
| `github_issue_comment` | write | issueまたはPRにコメントを投稿する。**公開される。** |

### 安全設計

- **コメントは公開されます。** `github_issue_comment`は誰でも見える発言
  を投稿します。対外的なコミュニケーションとして扱ってください。**承
  認の後ろにゲートすることを推奨します:**

  ```toml
  [capabilities]
  approval_required_tools = ["github_issue_comment"]
  ```

- **読み取りは読み取りのままです。** read分類のツールはGitHub上の何も
  変更できません。
- **diff本体は含みません。** `github_pr_read`は変更ファイルと増減行数
  を一覧表示しますが、diffの内容を取得することは決してなく、応答サイ
  ズを一定の範囲に保ちます。
- **最小権限。** 要求するのは`repo`スコープのみです(プライベート
  repositoryの読み取り／コメントに必要)。公開repoのみの利用でも問題な
  く動作し、このスコープは付与された場合にプライベートrepoも合わせて
  カバーするだけです。

## 前提条件: GitHub OAuth Appを作成する

自分自身のGitHub OAuth Appを用意する必要があります(DuDuClawは共有の
認証情報を同梱しません)。初回セットアップ:

1. [GitHub → Settings → Developer settings](https://github.com/settings/developers)
   を開く。
2. **OAuth Apps**の下で**New OAuth App**をクリックする。
3. **Authorization callback URL**を正確に次の値に設定する:

   ```
   http://localhost:18789/api/mcp/oauth/callback
   ```

4. appを登録し、**Client ID**をコピーする。続けて**Generate a new
   client secret**をクリックして**Client secret**を取得する。

要求されるscopeは以下のとおりです:

```
repo
```

`repo`は、そのアカウントから見える公開・プライベート両方のrepository
上のissueとpull requestに対する読み取り + コメント権限を付与します。
公開repoしか必要ない場合でも`repo`で接続して構いません。これはプライ
ベートrepoも同時に解放する最小のscopeです。

## ダッシュボードから接続する

1. **Manage → Integrations → Tool servers**(`/manage/integrations`)に
   移動する。
2. **Services that need authorization**までスクロールし、**GitHub**
   カードを探す。
3. カードの**Configure**をクリックする。Client IDとClient secretを貼
   り付ける。ダイアログには登録すべき正確なcallback URLも表示されるの
   で、上の手順3で入力した値と一致している必要がある。
4. GitHubの同意ウィンドウが開く。アクセスを承認すると、カードは
   **Authenticated**に切り替わる。

client認証情報は永続化されます(secretは保存時に暗号化)。これにより、
後で再認可する際にsecretを再入力する必要はありません。

## トークンについて

従来型のGitHub OAuth Appのトークンには**有効期限がありません**
(`expires_at`は空、これが正常かつ健全なデフォルトです)。もしあなたの
OAuth Appで**token expiration**を有効にしている場合、GitHubは
`refresh_token`を発行します。DuDuClawはトークンが期限切れになると、保
存されているclient認証情報を使ってその場でトークンを更新します。どち
らの形態も自動的に処理されます。

## トークン交換の詳細(気になる方向け)

GitHubのtoken endpointはデフォルトで**form-encoded**形式を返します。
DuDuClawは`Accept: application/json`を送ることで、代わりにJSONを返さ
せます。これはOAuth層で処理済みで、こちら側で設定する必要はありませ
ん。

## トラブルシューティング

- **「GitHub is not connected.」**: トークンが保存されていません。ダッ
  シュボードから接続してください。
- **`401 Unauthorized`**: 認可が取り消された、または無効です。再接続
  してください。
- **`403`**: 多くはプライベートrepositoryに対して`repo`スコープが不
  足しているか、rate limitに達しています。`repo`が付与されていない場
  合`github_status`に注記が表示されるので、再接続して付与してください。
- **`404`「not found」**: owner／repo／numberを確認するか、プライベー
  トrepositoryに対して`repo`を付与してください。
- **同意時にcallback URLが一致しない**: OAuth AppのAuthorization
  callback URLは正確に`http://localhost:18789/api/mcp/oauth/callback`
  である必要があります。

いつでも`github_status`を実行すれば、その場で診断結果を確認できます。
