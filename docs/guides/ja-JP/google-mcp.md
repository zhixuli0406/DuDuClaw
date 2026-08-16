# Google公式リモートMCPマウント(上級者向けオプション、出荷経路ではない)

> **まずここを読んでください**:DuDuClawのGoogle Workspaceサポート経路は**ネイティブツール**です——
> Gmail / Calendar / Sheets / Drive / Docs / Slides / Forms / Tasksの8サービス全てが
> GA REST API経由、19個のMCPツールで提供されます。
> [google-workspace.md](../google-workspace.md)を参照してください。
> **通常の利用でこのページを読む必要はありません。**
>
> このページで説明する公式リモートMCPは**上級者向けオプション**であり、2つのハード制約があります:
> ①まだDeveloper Previewであり、資格の申請が必要。②Program Termsにより、GA以前は
> **自社ドメイン/組織以外**のエンドユーザーがあなたのアプリを通じてPre-GA APIを
> 使用することは禁止されている——つまり**顧客への出荷はできません**(各顧客が
> 個別に資格とGCPプロジェクトを申請する場合を除く)。
> そのため2026-07-30に、製品面は完全にネイティブツール経路とし、本マウントは
> 自社利用と自ら申請する意思のある上級ユーザーにのみ提供する、と決定されました。

## 8サービスのカバレッジ状況

| サービス | ネイティブツール(**出荷経路**、資格制限なし) | 公式MCP(preview、自社利用限定) |
|---|---|---|
| Gmail | ✅ 4ツール | ✅ `preset = "google:gmail"`(13ツール) |
| Calendar | ✅ 2 | ✅ `google:calendar`(9) |
| Sheets | ✅ 2 | ✅ `google:sheets`(7) |
| Drive | ✅ 2 | ✅ `google:drive`(8) |
| Docs | ✅ 2 | ✅ `google:docs`(2) |
| Slides | ✅ 1(読み取り専用) | ✅ `google:slides`(2) |
| Forms | ✅ 2 | ❌ 公式なし |
| Tasks | ✅ 4 | ❌ 公式なし |

公式MCPのツール面はGmail/Drive/Sheetsにおいてネイティブより充実しています
(ラベル管理、権限照会、数式書き込みなど)——これが唯一の優位点で、代償は
previewの資格制限と出荷不可という制約です。ネイティブツールの総数は
19(`google_status`を含む)です。

FormsとTasksの「公式なし」は検証済みの事実であり、推測ではありません:
`formsmcp.googleapis.com` / `tasksmcp.googleapis.com`は実測で404が返り、
GoogleのMCPドキュメントにもこの2サービスへの言及は一切ありません
(coming soonとしてすら記載がない)。また公式にはChat
(`preset = "google:chat"`、利用可能)とPeople(エンドポイント名が異なり、
presetには未収録)もありますが——この2つは現時点でネイティブツール側の
対応がありません。

### 各サービスのツール一覧(実測`tools/list`)

- **Gmail**:`search_threads` `get_thread` `get_message` `create_draft`
  `list_drafts` `list_labels` `create_label` `label_message` `unlabel_message`
  `label_thread` `unlabel_thread` `apply_sensitive_message_label`
  `apply_sensitive_thread_label`。**送信用ツールはありません**——できるのは
  下書き作成まで。
- **Calendar**:`list_calendars` `list_events` `get_event` `search_events`
  `suggest_time` `create_event` `update_event` `delete_event`
  `respond_to_event`
- **Drive**:`search_files` `list_recent_files` `get_file_metadata`
  `get_file_permissions` `read_file_content` `download_file_content`
  `create_file` `copy_file`
- **Docs**:`read_doc` `update_doc`
- **Sheets**:`get_spreadsheet` `get_values` `update_values` `update_formulas`
  `update_spreadsheet` `insert_dimension` `copy_sheet_to_another_spreadsheet`
- **Slides**:`read_presentation` `update_presentation`

## 事前設定(一度だけ)

1. **Developer Preview Programに参加**:<https://developers.google.com/workspace/preview>
   (無料、承認まで数日。申請にはWorkspaceアカウントが必要)。これらの
   サーバーは**まだpreviewでありGAではありません**——利用規約によりPre-GA APIは
   自社ドメイン/組織内利用限定です。
2. **GCPプロジェクトでAPIを有効化**——各サービスは「標準API + MCP API」の
   2層が必要です:

   ```bash
   gcloud services enable \
     gmail.googleapis.com gmailmcp.googleapis.com \
     calendar-json.googleapis.com calendarmcp.googleapis.com \
     drive.googleapis.com drivemcp.googleapis.com \
     docs.googleapis.com docsmcp.googleapis.com \
     sheets.googleapis.com sheetsmcp.googleapis.com \
     slides.googleapis.com slidesmcp.googleapis.com \
     --project=PROJECT_ID
   ```

3. **Googleアカウントを接続**:ダッシュボードの「Integrations → Google(整合 → Google)」タブ。
   scopeは既にDrive / Docs / Sheets / Slidesをカバーしています
   (詳細は[google-workspace.md](../google-workspace.md))。v1.47より前に接続した
   アカウントは、新しいscopeを取得するために一度再接続が必要です。

## 認証の仕組み

`preset`はbearerを`oauth://google`に設定します——マウント時に現在有効な
アクセストークンを取得し、期限切れの際はrefresh tokenで自動的に更新します。
Googleアカウントが接続されていない場合は**サーバー全体がスキップされます**
(フェイルセーフ:エージェントはこれらのツールを失いますが、応答自体は
失敗しません)。

ダッシュボードの連携を経由せず、自分でトークンを用意することも可能です:

```toml
[[mcp.external]]
preset = "google:sheets"
bearer_token = "env://MY_GOOGLE_TOKEN"   # presetのデフォルトbearerを上書き
```

技術的な詳細:これらはステートレスなStreamable HTTPサーバーであり、
DuDuClawのMCPクライアントがネイティブにサポートしています。`npx mcp-remote`
のようなstdioプロキシは**不要です**——Googleは公式のbridgeを提供しておらず、
そのOAuthはDynamic Client Registrationに対応していないため、コミュニティ製
proxyのデフォルトフローは失敗します。

## ツール面を絞る推奨

公式サーバーは一度に多くのツールを渡してきます(Driveで8個、Gmailで13個)。
`allowed_tools`で必要なものだけを開放し、書き込み系ツールはHITL承認に
回すことを推奨します:

```toml
[[mcp.external]]
preset = "google:calendar"
allowed_tools = ["list_events", "suggest_time", "create_event"]

# agent.toml
[capabilities]
approval_required_tools = ["create_event", "update_doc", "create_file"]
```

## 既知の制限

- previewステージであり、rate limitは公開ドキュメント化されていません
  (GCP Consoleのquotaページで確認可能)。
- OAuthはインタラクティブのみ——**サービスアカウント/ヘッドレス認可経路は
  ありません**。
- Gmailに送信ツールはありません。公式リファレンスページのツール数は
  実際のエンドポイントと食い違っています(リファレンスは10個と記載、
  実際のエンドポイントは13個)——実測の`tools/list`を信頼してください。
- GAでエンドポイント名が変わった場合、変更が必要なのは`mcp_external.rs`の
  `GOOGLE_MCP_PRESETS`の1箇所のみです(利用者の`agent.toml`は変更不要)。

参考資料:
[configure-mcp-servers](https://developers.google.com/workspace/guides/configure-mcp-servers)、
[Gmail MCP reference](https://developers.google.com/workspace/gmail/api/reference/mcp)、
[Calendar MCP reference](https://developers.google.com/workspace/calendar/api/v3/reference/mcp)、
[Developer Preview Program](https://developers.google.com/workspace/preview)。
