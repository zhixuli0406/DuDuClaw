# Google公式リモートMCPマウント(上級者向けオプション、出荷経路ではない)

> **まずここを読んでください**:DuDuClawのGoogle Workspaceサポート経路は**ネイティブツール**です——
> Gmail / Calendar / Sheets / Drive / Docs / Slides / Forms / Tasks / Chat の九サービスが
> REST API 経由、38 個のネイティブ MCP ツールで提供されます。
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

## 2026-11-11 からのクライアント要件

2026-11-11 から Google Workspace MCP サーバーは AI agent として指定されていないクライアントを拒否します。Google Auth platform → Clients で、この上級用途専用のクライアントを作り “Use this client for an AI-powered agent” を選択してください。REST 専用アプリは対象外で、DuDuClaw のネイティブ Google ツールは REST を使います。2026-10-30 の調査に基づく記述で、Console 画面を再度ライブ確認していません。

## サービスの対応状況

| サービス | ネイティブツール(**出荷経路**、資格制限なし) | 公式MCP(preview、自社利用限定) |
|---|---|---|
| Gmail | ✅ 4 | ✅ `preset = "google:gmail"`(13ツール) |
| Calendar | ✅ 4 | ✅ `google:calendar`(9) |
| Sheets | ✅ 6 | ✅ `google:sheets`(7) |
| Drive | ✅ 7 | ✅ `google:drive`(8) |
| Docs | ✅ 6 | ✅ `google:docs`(2) |
| Slides | ✅ 1(読み取り専用) | ✅ `google:slides`(2) |
| Forms | ✅ 2 | ❌ 公式なし |
| Tasks | ✅ 4 | ❌ 公式なし |

ネイティブツールは作成と編集にも対応します。リモートツールは Google 独自のパラメーターと利用資格を維持し、ネイティブの書き込みスイッチやフォルダ確認は適用されません。制御された Drive 書き込みには下記のネイティブツールを使ってください。

FormsとTasksの「公式なし」は検証済みの事実であり、推測ではありません:
`formsmcp.googleapis.com` / `tasksmcp.googleapis.com`は実測で404が返り、
GoogleのMCPドキュメントにもこの2サービスへの言及は一切ありません
(coming soonとしてすら記載がない)。また公式にはChat
(`preset = "google:chat"`)とPeople(エンドポイント名が異なり、
preset には未収録）もあります。People のネイティブツールはありませんが、Chat には読み取りと本人確認付き送信ツールがあります。

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
- 通常の OAuth はブラウザー同意が必要です。リモートやヘッドレス環境は [Google の接続](google-setup.md#リモートホストと-docker)の SSH 転送を使えます。ネイティブのサービスアカウント委任は対応済みで、各公式リモート MCP が委任トークンを受け付けるかは別途検証が必要です。
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

`google:chat` プリセットは Chat スコープを追加しません。ネイティブ Chat の読み取りと送信は個別のオプトインで、マウントにも適切な認可が必要です。Google Chat チャネルは `chat.bot` を使う別接続です。

## チャットの認可リンク

本人の接続や用途の許可が不足する場合、ゲートウェイが確認リンクを案内できます。同じメンバーによる通常の Dashboard ログインとデータ説明への明示的な同意が必要で、ログイン資格情報や SSO ではありません。MCP ツールは ticket を受け取ったり、メンバー、アカウント、スコープ、戻り先 URL を指定したりできません。認可は外部送信の承認とは別です。アカウントの確認、公開 URL、続行については[個人 Google 接続ガイド](google-personal-connections.md)を参照してください。公式 remote MCP の利用資格と既存の権限チェックも引き続き必要です。

## 本人として Google Chat を送信する

`google_chat_send {space, text, thread?}` は、依頼した認証済みメンバー自身の Google アカウントで既存スペースにテキストを送信します。`google:write` が必要で、管理者が `[google] chat_send = true` を有効にするまでは使用できません。共有アカウント、他のメンバー、Apps Script、Chat アプリの `chat.bot` トークンへの切り替えは行いません。

個人 OAuth では、先にアカウントを接続し、「自分の接続 → Google Chat の送信」で独立した `chat_send` 用途を許可します。追加同意では、同意済みの通常サービスと `https://www.googleapis.com/auth/chat.messages.create` を明示的に要求します。以前の Google 認可の自動統合は要求しません。Google の `sub` が以前と同じで、実際にスコープが付与されている必要があります。ドメイン全体の委任でも送信用スコープを別に要求し、Workspace 管理者の許可が必要です。他の Google ツールの要求には追加しません。

毎回本人の確認が必要です。確認は５分間有効で１回限りです。カードにはアカウント、スペースのリソース名、スレッド、送信する全文と個人情報の送信に関する注意が表示されます。短文は認証済みの個別チャットで確認でき、グループからの依頼や長文は本人だけが開ける「自分の接続」ページで確認します。管理者の承認、`allow` ルール、Admin MCP scope、宛先許可リストでは代替できません。組織の外部送信承認も別途適用されます。本文、宛先、アカウント、認証情報、ポリシーが変わると再確認が必要です。

Gateway は操作ごとに固定の request ID を保存します。結果が不明な場合は `result_unconfirmed` を返し、自動再送しません。新たに送信を依頼する前に Google Chat を確認してください。添付ファイルやスペースの作成には対応していません。指定スレッドは同じスペース内のものに限ります。Google では本人が送信者として表示され、アプリ名も付きます。[Google Chat API](https://developers.google.com/workspace/chat/api/reference/rest/v1/spaces.messages/create)。

## クラウドドライブへの書き込みツール

この九つのネイティブツールには `google:write` が必要です。管理者は作成、変更、移動、ゴミ箱への移動を個別に許可します。四項目とも既定はオフです。有効化や制限の緩和にはリスクへの明示的な同意が必要で、管理者と時刻を記録します。この設定変更に追加の再ログインは不要です。「連携 → Google → ドライブへの書き込み」で４操作、操作ごとの承認方法、フォルダ一覧を設定します。準備状況には委任スコープ、スタッフごとのテスト、会社アカウントの同意が表示されます。メンバーは「自分の接続 → ドライブへの書き込み」で接続し、書き込み確認カードを処理します。記録は「ログ → 履歴」の「ドライブへの書き込み」で絞り込みます。

各操作は既定でメンバー本人の確認を必要とします。管理者は操作ごとに確認を無効にするか、本人の確認に加えて管理者の承認を要求できます。本人確認は五分間有効で、そのメンバーだけが判断できます。ツール許可リスト、Admin トークン、ワークフローの継続承認では代用できません。確認が必要な操作は無人実行やバックグラウンド処理では拒否されます。個人接続と所有者への委任では依頼者がアカウント本人である必要があります。会社共用接続と固定の委任では、検証済みの依頼メンバーが確認します。

| ツール | パラメーターと結果 | 操作スイッチ | 効果クラス |
|---|---|---|---|
| `drive_create_file` | `name` は必須、`parent_folder_id` は任意。`text`（最大 100,000 文字）、`attachment`（この会話の添付ファイル名）、`folder=true` のいずれか一つを指定。任意の `convert_to` は `document`／`spreadsheet`、`mime_type` は `text/plain`／`text/csv`／`text/markdown`。ファイルまたはフォルダを作成します。 | 作成 | Publish |
| `drive_copy_file` | `file_id`、`parent_folder_id` は必須、`name` は任意。読み取り可能なファイルを宛先フォルダへコピーします。 | 作成 | Publish |
| `drive_update_file` | `file_id` は必須。`name`、`description`、`text` の少なくとも一つを指定。名前、説明、テキスト内容を変更します。内容は最大 100,000 文字で、Google 形式以外のテキストファイルに限ります。 | 変更 | Modify |
| `drive_move_file` | `file_id`、`destination_folder_id` は必須。同じドライブ内で移動し、元の親フォルダは Google から取得します。 | 移動 | Publish |
| `drive_trash_file` | `file_id` は必須。ゴミ箱へ移動するだけです。 | ゴミ箱へ移動 | Delete |
| `docs_insert_text` | `document_id`、`text`（1～20,000 文字）は必須。`at="start"`、`at="end"`、`after_text`（文書内で一意の位置指定テキスト、1～1,000 文字）から一つを指定。`tab_id` は任意。 | 変更 | Modify |
| `docs_format` | `document_id`、`operations`（1～50 件）は必須。段落・文字のスタイル、表のセル背景、ヘッダー、文書タブの追加に対応。詳細は下記参照。 | 変更 | Modify |
| `sheets_add_sheet` | `spreadsheet_id`、`title` は必須。`copy_from_sheet`（既存シート名）は任意。同じスプレッドシートに空のシートを追加するか、既存シートを複製します。 | 変更 | Modify |
| `sheets_add_note` | `spreadsheet_id`、`range`（A1、最大 1,000 セル）、`note`（最大 5,000 文字、空文字で消去）は必須。セルのメモを設定し、スレッド形式のコメントは作成しません。 | 変更 | Modify |

ID の代わりに、対応する Google ファイル・フォルダ URL も使えます。作成時に `parent_folder_id` を省略するとマイドライブのルートを使用し、フォルダリストが空の場合だけ許可されます。アップロードはこの会話の現在の依頼に添付されたファイルに限り、最大 5 MB（5 MiB）です。任意のローカルパスや社員の作業フォルダは指定できません。Google ドキュメントやスプレッドシートへの変換は任意です。完全削除、ゴミ箱を空にする操作、共有や権限の変更には対応しません。

`docs_format` の各操作は `type` と、その種類に対応するフィールドだけを受け付けます。

- `paragraph_style`：一意の `text`、`named_style`（`NORMAL_TEXT`、`TITLE`、`SUBTITLE`、`HEADING_1`～`HEADING_6`）。`alignment`（`START`、`CENTER`、`END`、`JUSTIFIED`）は任意。
- `text_style`：一意の `text` と、`bold`、`italic`、`underline`、`font_size_pt`（6～96）の少なくとも一つ。
- `table_cell_style`：ゼロ始まりの `table_index`（0～99）、`row`（0～499）、`column`（0～49）、`background_hex`（`#RRGGBB`）。
- `header`：`text`（最大 1,000 文字）。
- `add_tab`：`title`（1～100 文字）。

### フォルダの規則と制限

管理者はフォルダまたは共有ドライブ全体をリストに登録できます。登録済み共有ドライブ内のファイルは許可範囲内です。それ以外は、すべての親経路が十二階層以内に登録済みフォルダへ到達する必要があります。親を読めない場合、対象や祖先がゴミ箱内の場合、確認上限を超える場合、Google の権限が足りない場合は拒否します。再確認を含め、一回の呼び出しでメタデータを取得できるのは十六回までです。

作成とコピーは宛先フォルダを確認します。コピー元は読み取り可能で、ゴミ箱内でない必要があります。変更、移動、ゴミ箱への移動は対象の場所を確認します。登録済みフォルダ自体の名前変更、移動、ゴミ箱への移動はできません。移動元と宛先の両方が範囲内である必要があり、マイドライブと共有ドライブの間や、異なる共有ドライブ間の移動はできません。ショートカットは許可フォルダや宛先に指定できません。変更はショートカットを拒否します。移動とゴミ箱への移動はショートカット自体を操作し、リンク先をたどらず自身の親で確認します。

リストが空の場合、作成と変更はそのアカウントが書き込めるすべての場所で可能ですが、移動とゴミ箱への移動は使えません。書き込み前後と承認後にフォルダを確認します。Google 自体にフォルダ許可リストはなく、確認中にも他者が場所を変えられるため、この確認はベストエフォートです。書き込み後の範囲違反は管理者に通知し、未完了の確認も記録します。拒否や結果不明も含むすべての試行を記録しますが、内容やトークンは残しません。結果不明の操作は自動再試行せず、Google で確認してから改めて依頼してください。

既存の `docs_append`、`docs_create`、`docs_replace_text`、`sheets_append`、`sheets_create`、`sheets_update` は従来の認可を維持し、この四つのスイッチやフォルダ規則の対象外です。新しい Docs／Sheets の四ツールは「変更」に分類します。Google 公式 Drive リモート MCP に完全な Drive 権限を持つトークンを渡すことはありません。この九つの書き込みツールはメイン gateway プロセス内だけで実行します。

アカウントの準備は[接続設定](google-setup.md)と[個人の書き込み同意](google-personal-connections.md)を参照してください。Google OAuth の分離・取消、委任スコープ、文書タブ、変換、共有ドライブのロール、個人メッセージでの確認配信は実アカウントでの受け入れ確認が必要です。本書はこれらの検証済みを意味しません。
