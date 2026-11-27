# MCP ツール: エージェントに何が見え、なぜそうなるのか


## ローカルの呼び出し元を検証する

`duduclaw identity whoami --json` で gateway が発行した実行時の身分を照会できます。資格情報がない、無効、または検証できない場合は非ゼロで終了します。ローカルのサードパーティー MCP server は proxy から `DUDUCLAW_RUN_CREDENTIAL`、`DUDUCLAW_IDENTITY_ENDPOINT`、`DUDUCLAW_IDENTITY_SERVER` を継承し、従来の身分変数は引き続き除去されます。gateway はその実行の CLI プロセスと子孫プロセスにだけ応答するため、実行の外で起動した server は拒否されます。呼び出しごとに検証し、ツール独自の規則で認可してください。[開発ガイド](custom-mcp-tool.md)を参照してください。


## スキルのアーカイブ復元

未使用期間によるスキルの stale 化と自動アーカイブは削除されました。`[skill_curator]` は読み取れますが全設定が無効で、プロセスごとに一度警告します。Gateway 起動時に記録済みのアーカイブを元の場所へ戻し、Active にします。既存ファイルは上書きせず、競合したアーカイブは `skills-archive` に残します。Linux と macOS は両モードでディレクトリ FD を使ってグローバルと社員の平面形式スキルを復元します。シンボリックリンクはたどりません。他の OS はアーカイブを残し、手動復元が必要です。

| 非推奨ツール（削除予定 v1.86.0） | 動作 |
|---|---|
| `skill_curator_status` | 読み取り専用のアーカイブ一覧と復元案内。`run=true` は受け付けますが無効である旨を返します。スキル名とスコープのみ表示します。社員は自分の範囲のみ、管理者は全範囲を参照できます |
| `skill_pin` | `pinned=true` は記録済みアーカイブを上書きせず復元します。それ以外のピン操作は無効です。Linux と macOS で復元できます。社員は自分の範囲のみ復元でき、グローバルと他の社員の範囲には管理者権限が必要です |

読み込まれたスキルは `skill_list` で確認してください。同名の競合を解消してから復元を再試行し、成功までアーカイブを保管してください。既存の認可と社員スコープの制限は両ツールに引き続き適用されます。


DuDuClaw の MCP サーバーは標準の `tools/list` でツールを宣言します。このページでは、誤解されやすい 2 点を説明します。

1. なぜエージェントに見えるツールが、サーバーが実装しているものより少ないのか。
2. ツール説明に字数上限が入った今、長い詳細はどこへ行ったのか。

ツールを追加する場合は先に [custom-mcp-tool.md](../custom-mcp-tool.md) を読んでください。このページは実装ではなく宣言面の話です。gateway なしで MCP サーバーだけを使う場合は [mcp-standalone.md](mcp-standalone.md) を参照してください。


## 設定の適用範囲と起動結果

| 設定 | 読み取るランタイム | ダッシュボード |
|---|---|---|
| 従業員の `.mcp.json` | Claude、Codex、Grok、プロセス内ツールループ（OpenAI 互換、Direct API、ローカル推論） | 管理者が編集可能。`source: "mcp_json"` |
| 従業員の `agent.toml` の `[[mcp.external]]` | プロセス内ツールループのみ | 読み取り専用。`source: "agent_toml"` |

Claude・Codex・Grok は `[[mcp.external]]` を読み取りません。MCP ページは各項目の
使用先を表示し、`mcp.list` は `effective_runtimes` に `claude`、`codex`、`grok`、
`in_process` を返します。これは設定の適用範囲であり、接続成功の保証ではありません。
無効な TOML 項目は無効のままです。形式間の自動移行や認証情報の転送は行いません。
両方に同じサーバーがある場合、プロセス内ループは TOML の登録を優先します。

管理者専用の `mcp.update` で stdio 項目を追加・編集すると、保存後に前回の結果を
消し、MCP ページの「今すぐ確認」と同じ起動確認を実行します。この操作は管理者専用の
`mcp.probe`（引数 `{agent_id, server_name}`）を呼び、`{ok, tool_count, reason_code}`
を返します。保存時は `probe` に結果を返します。設定の検証と有効設定の生成はメモリー
内で行い、`.mcp.json` の修復や従業員ディレクトリへの書き込みは行いません。設定用の
唯一の出力は権限 0600 の一時 proxy 設定で、成功・失敗・タイムアウト後に削除します。
`initialize` と `tools/list` を送り、期限は 10 秒、終了時にプロセスを回収し、上流ツール
は呼びません。失敗しても保存した設定は残ります。成功は起動と可視ツールの取得だけを
確認し、呼び出し権限やリモートサービスの健全性を保証しません。gateway 全体で同時に
最大 4 件、空き待ちは最大 2 秒です。同じ従業員／サーバーの実行中や空き待ちの期限切れ
では `busy` を返すので、後でもう一度お試しください。

proxy の認証判定、検査順序、fail-closed 動作は変わりません。proxy は共有ファイルに
起動結果を書かず、自身の stdio ピアの最初の JSON-RPC リクエストに `-32001`、固定
メッセージと `error.data.reason_code` を返し、自身の stderr に固定形式の理由コードを
出力します。`identity_rejected` は認証器の拒否（認証情報の欠落・不正、ポリシーや鍵の
読み取り失敗など）を表し、再認証で細かい原因を推測しません。結果の記録は gateway
のみが行います。確認時は proxy のエラーフレームから理由を取得し、gateway が起動した
Claude の `system/init` は接続済み／未接続（`connection_failed`）だけを提供します。
観察器はその起動の凍結設定の名前だけを完全一致で受け入れ、イベント・名前・状態の
長さを制限し、最大 64 件を重複排除して、イベントごとに最大 1 件の失敗要約を記録
します。確認の失敗も gateway のロック・容量制限付き helper で `channel_failures.jsonl`
に `mcp_start` を 1 件記録できます。トークン、環境変数値、上流引数は含みません。
未接続でもターンは継続します。

`mcp.list` の `last_start` は gateway 内の容量制限付きメモリーマップだけを読み、
最古の項目から削除します。JSONL からは状態を読みません。画面は翻訳した理由と時刻を
表示します。実行時に未接続となった場合は「今すぐ確認」で詳細な理由を取得できます。
サーバーの更新・削除や従業員の削除で前回の結果を消し、gateway の再起動でも失われます。
不明な状態は推測しません。常時監視ではありません。

## MCP クライアントのプロトコルバージョン

API／openai-compat クライアントとリモート MCP bridge は **2026-07-28** を優先します。
`server/discover` で確認し、各リクエストにバージョン、クライアント機能、識別情報の
metadata を付けます。HTTP では `MCP-Protocol-Version`、`Mcp-Method`、必要に応じて
`Mcp-Name` を送信します。`x-mcp-header` 付きの引数をヘッダーへ反映し、不正な注釈の
ツールは一覧から除外します。非 ASCII の値は規定の Base64 形式で送信します。

旧サーバーには **2025-11-25** または **2025-06-18** で接続し、`initialize`、
`notifications/initialized`、合意した session ヘッダーを使用します。初回の探測で
`-32022` を受けた場合は共通の最新バージョンを選択します。接続後に bridge が
同じエラーを受けると、操作を拒否して接続を無効にします。会話を再起動し、再協商、
ツール一覧の取得、権限検査をやり直す必要があり、別バージョンで呼び出しを再送しません。
API クライアントも `tools/call` で `-32022` を受けた場合は呼び出しを終了し、そのクライアントの
後続リクエストを拒否します。host は再接続、ツール一覧の取得、権限検査をやり直す必要が
あります。初回のハンドシェイクと独立した一覧取得時のバージョン復旧は維持します。
明示的な modern の機能／ヘッダーエラーではダウングレードしません。新しい API HTTP
接続は毎回独立して探測し、URL をキーとしたグローバルなバージョンキャッシュを使いません。
bridge は自身の接続の協商状態だけを保持します。認証情報、session、ツール一覧を
別の利用者と共有しません。API クライアントは旧 initialize も
400／404／405 になると旧 HTTP+SSE を試しますが、`endpoint` は設定元と同一 origin に限定します。

API クライアントの MRTR は各回で新しい ID と元の引数を使い、`requestState` を変更せず返し、回数を制限します。
API クライアントには host input handler 接口があります。未登録の場合は対話機能を宣言せず、
予期しない入力要求を拒否します。同意画面は追加しておらず、elicitation を自動承認しません。
Tasks、sampling、roots は既定で無効です。リモート bridge は modern の対話機能を宣言せず、
状態だけの応答を含むすべての `input_required` を拒否します。下流からのリクエストに
`requestState` または `inputResponses` が含まれる場合も、送信前に拒否します。継続データ
全体の外部送信・承認検査を元の承認済み呼び出しに結び付ける保護が整うまで、bridge の
MRTR は利用できません。応答ストリームが切れた場合はエラーにし、結果が不明な操作を
黙って再実行しません。既存のツール制限と第三者ツール分類は維持されます。

リモート OAuth は事前登録済みの認証情報を優先します。CIMD を使う管理者は既存の公開
HTTPS metadata 文書を設定できます。

```toml
[remote_mcp]
client_metadata_url = "https://your-domain.example/mcp-client.json"
```

認可サーバーが CIMD をサポートする場合、文書の client ID はこの URL と一致し、既定の
callback URI を許可し、公開クライアント認証（`token_endpoint_auth_method = "none"`）を
使用する必要があります。選択した文書の検証失敗時は拒否します。未設定または非対応なら
`application_type` を付けた動的登録にフォールバックします。PKCE と resource indicator を
維持し、コード交換前に callback の `iss` を検証します。保存した認証情報は issuer に結び付けます。
探測時と保存済みの OAuth resource は、MCP エンドポイントの canonical resource
（scheme、host、port、末尾のスラッシュを除いた path）と完全に一致する必要があります。
resource の query、fragment、同一 origin の別パスは拒否し、不一致の古い認証情報は再接続が必要です。
403 の自動 scope step-up と CIMD `private_key_jwt` は未実装です。

## 会議の文字起こし

`meeting_transcribe` は呼び出す従業員自身の添付ファイルの `source_path` と、任意の
`engine`、`language_hint`（既定 `zh-TW`）、`write_wiki`（既定 `false`）を受け取り、
背景仕事の ID と状態を返し、音声は管理者が設定したプロバイダーに送信します。`meeting_transcribe_status`／`meeting_transcribe_cancel` に `job_id` を渡して自分の仕事を確認・キャンセルできます。`meeting_upload_link {}` は元の会話用の一回限りのアップロードリンクを返します。`transcribe_audio` と同じ
`messaging:send` スコープに加え、検証済みの従業員 ID と既存の能力・承認・外部送信
ゲートが必要です。外部 fallback 名前空間では使用できません。効果は保存データの
変更とネットワーク送信です。任意のエンジンは管理者の設定と一致する必要があります。

音声は Gemini または OpenAI に送信し、結果を台湾の繁体字表記に変換します。
RFC-23 が有効な場合は保存する文字列をマスクしますが、アップロード済みの音声は
マスクできません。この版の `drive_file_id` と `write_wiki: true` は未対応エラーを
返します。ゲートウェイで背景実行します。ffmpeg、サービス制限、設定、プライバシーについては
[会議の文字起こし](../../features/ja-JP/73-meeting-transcripts.md) を参照してください。

## MCP API キー（`ddc_<env>_…`、v1.82 以降）

ダッシュボードの「連携 → ツールサーバー」タブにある MCP API キー欄で作成したキーは、SHA-256 指紋として `<home>/mcp_keys.db` に保存され、`config.toml` には書かれません。stdio の `duduclaw mcp-server`（キーは `DUDUCLAW_MCP_API_KEY`）と `duduclaw http-server`（`Authorization: Bearer`）は呼び出しごとにストアを参照する（指紋でインデックス検索し、見つかった行の指紋を固定時間で比較する）ため、名前変更・期限変更・無効化・削除は再起動なしで次の呼び出しから反映されます。これらは `mcp_keys.db` を読み取り専用でのみ開き、書き込むのはゲートウェイだけです。各キーの有効期限は管理者が選びます（30／90／365 日、日付指定、無期限）。従来の固定 30 日ではありません。期限切れのキーと無効化されたキーは同じ拒否メッセージ（`API key is expired or disabled`）を受けます。サーバーのログには理由とキー ID が残り、キーそのものは決して書かれません。

最終使用時刻は別ファイル `<home>/mcp_keys_usage.db`（キー ID と時刻のみ、キーごとに 1 分に最大 1 回）に記録され、ゲートウェイがキー一覧を返すときに読み込みます。このファイルは認証に一切使われません。壊れたり書き換えられたりしても、「最終使用」欄の表示が変わるだけです。

クライアント ID で発行するダッシュボードのキーはすべて**外部**キーです。付与できるスコープはサーバー側の 1 つのリスト（`memory:read`、`memory:write`、`wiki:read`、`wiki:write`、`messaging:send`）で決まり、それ以外はどのスコープかを示して `mcp_key_scope_not_grantable` で拒否されます。クライアント ID には AI スタッフの ID や名前、システム予約名（`dashboard`、`cron`、`default`、`gateway-internal` など）は使えません。AI スタッフ自身のキー（RFC-21 のスタッフ別キー、v1.76 の Google 専用キー）は `agent_id` で明示的に作成し、スタッフの **ID**（`agents/` 配下のフォルダー名、大文字小文字も一致）を指定する必要があります。`[agent] name`／表示名や大文字小文字の違う ID は拒否されます（`mcp_key_employee_id_required`）。キーは作成時にそのスタッフへ紐付けられ（`agent_binding`、以後変わりません）、そのスタッフとして動作する内部キーで、v1.81 のキー作成画面が提供していたスコープだけを持ち、`admin` は持てません。同僚の個人専用スタッフの場合は、その所有者か、本人確認（step-up）をやり直して監査記録を残した管理者だけが作成できます（個人専用スタッフは、まだ提供されていないネイティブ Linux のスタッフ分離に含まれる機能で、現在のインストールには存在しません）。

**内部キーを `duduclaw http-server` で使う場合。** HTTP サーバーはすべての呼び出し元に対して 1 人の固定スタッフ（デフォルトのスタッフ）として動作し、多くのツールはキーではなくそのスタッフとして読み書きします：`working_state_*`、`mail_*`、すべての `wiki_*` ツール、`canvas_*`、`send_message`、`tasks_list`、`plan_*`、`odoo_*`、コンピューター操作・フォーク・ナレッジのツール、そして `crates/duduclaw-cli/src/mcp/tools_list.rs` の `DEFAULT_AGENT_HANDLER_TOOLS` ∪ `PROCESS_AGENT_TOOLS` に含まれる残りのツールです。HTTP では「既定で拒否」です。

- これらのツールを呼び出せるのは、ゲートウェイ自身の内部キー（ゲートウェイが指紋を記録した `gateway-internal` キーで、サーバー自身のスタッフ ID が検証済みのもの）だけです。
- それ以外の外部でないキー（スタッフ専用キー、表示名で作成されたキー、移行されたキー、未知の内部クライアント ID）は、スコープにかかわらずこれらのツールをすべて拒否され（`-32003`、監査クラス `per_agent_key_process_tool`）、`tools/list` にも表示されません。この判断はキーの認証方法だけで行い、ディスク上のファイルは参照しません。
- 残りのツール（`memory_store`、`memory_search`、`file_read` など）は、常にキーが**紐付けられたスタッフ**として実行され、サーバーのデフォルトのスタッフになることはありません。`tools/list` には、そのキーのスコープで実際に呼び出せるツールだけが表示されます（`memory:read` だけのキーには `memory_store` は表示されません）。`admin` が必要なツール（`tasks_create` など）はスタッフ専用キーでは呼び出せず、表示もされません。
- 紐付けのない、または紐付け先のスタッフがもう存在しない（フォルダーの削除・名前変更、または `status = "deleted"`）外部でないキーは、**すべての**ツールを拒否され（`-32003`、理由付きの監査クラス `remote_key_unbound`）、`tools/list` は空になります。キー一覧ページでは「スタッフはもう存在しません」と表示されます。

**スタッフを削除すると、そのキーは永久に取り消されます。** ダッシュボードでスタッフを削除すると、同じ操作の中でそのスタッフに紐付いたすべてのキーが削除され（指紋を墓標として残すため、二度と認証されません）、キーごとに監査記録 `mcp_key_deleted`（理由 `employee_deleted`）が書かれます。あとでスタッフを復元してもキーは戻らず、**同じ ID で後から作成されたスタッフもそれらのキーを使えません**。新しいスタッフには新しいキーを発行してください。ゲートウェイ以外で削除されたスタッフ（MCP `agent_remove` がフォルダーを `agents/_trash` に移した場合や、フォルダーを手動で削除した場合）や `status = "deleted"` のスタッフも、ゲートウェイの次回のキー確認（起動時と 10 分ごと）で同じようにキーが取り消されます（`agents/` フォルダー自体が存在しない間は何も判断しません）。第二の防御として、紐付け先スタッフのフォルダーより前に作成されたキーは HTTP サーバーで拒否されます（理由 `employee_recreated`、キー一覧ページでは「スタッフはもう存在しません」）。これは削除されたスタッフの ID で作り直されたスタッフであり、次回の確認より前でも拒否されます。この確認には OS がフォルダーの作成日時を提供する必要があります（macOS、Windows、多くの Linux ファイルシステム）。取得できない環境では、上記の取り消しが保証になります。DuDuClaw のホーム全体をバックアップから復元したり別の場所へコピーしたりすると作成日時がリセットされるため、復元前に発行したスタッフ専用キーは拒否され、再発行が必要です。

**拒否されたキーは記録されます。** MCP キーの認証に失敗するたびに、`reason`（`missing_header`（キーなし）、`bad_scheme`（`Bearer` 以外）、`malformed_key`（形式不正）、`wrong_credential_type`（資格情報の種類が違う）、`unknown_key`（存在しない）、`key_disabled`（無効化済み）、`key_expired`（期限切れ）、`key_deleted`（削除済み）、`key_unbound`（スタッフが存在しない）、`store_unavailable`（ストアを読めない）のいずれか）と固定の経路名 `route`（`mcp`、`mcp_v1_call`、`mcp_v1_stream` など）が記録されます。無効化・期限切れ・削除済み・紐付けなしのキーにはキー ID（`mk_…`）が付きます。記録にキーそのもの、その一部、指紋、URL のパラメーター、User-Agent が含まれることはありません。記録は 2 つのファイルに分かれます。

- `<home>/security_audit.jsonl`（ハッシュチェーン付きのセキュリティ監査。サーバーログにも警告が出ます）：**既存のキー**（無効化・期限切れ・削除済み・スタッフが存在しない）の認証失敗は、キー ID 付きの `mcp_auth_failed` としてすぐに書かれます。キーと理由ごとに 1 時間 1 件で、その時間の残りの回数は正時のあとに回数（`count`）付きでもう 1 件書かれます。**それ以外の失敗**は 1 時間ごとに 1 件の `mcp_auth_failed_summary` にまとめられます（理由ごとの回数、接続元の数（1,024 まで）、書き込めなかった記録の数、明細ファイル名）。そのため、有効なキーを持たない相手がどれだけ試しても、セキュリティ監査に増えるのは 1 時間に最大 1 件です。取り消したキーで連携サービスが再試行を続けていると、キー ID 付きの記録がすぐに現れます。
- `<home>/mcp_auth_failures.jsonl`（所有者のみ読み書き可）：接続元 `source` 付きの 1 分ごとの明細です。接続元はサーバーのソケットから見た IP（`X-Forwarded-For` は使いません。リバースプロキシ経由ではプロキシのアドレス）です。`http-server` では、同じ接続元・理由・キーの 1 分間で最初の失敗をすぐに書き、その分の残りを回数付きの 1 件にまとめます。1 分あたり最大 60 件で、それを超えた分は `source: "many"` の 1 件に合算されます。4 MiB に達すると `mcp_auth_failures.jsonl.1` に名前が変わり（前のものを置き換え）、2 つ合わせて約 8 MiB 以内に収まります。`duduclaw import` はこのファイルを移行しません。

stdio の `mcp-server` は 1 プロセスにつき最大 1 件の `mcp_auth_failed` をセキュリティ監査に書き、明細は書きません。

外部キーと各スタッフ自身の stdio `mcp-server` には影響しません。今後のバージョンですべてのツールをキーのスタッフとして実行するよう変更し、この制限を解除する予定です。現在のリリースではこの点は変わりません。

**アップグレード後の `config.toml [mcp_keys]`。** ゲートウェイは v1.82 で最初に起動したとき、その時点の `config.toml` の `[mcp_keys]` 項目の指紋を 1 つのトランザクションで「移行待ち」として記録し、各キーをストアへ移して監査記録を書いてから `config.toml` から削除します。移行待ちがなくなると「移行完了」マーカーを書きます。移行したキーはクライアント・外部フラグ・スコープを引き継ぎ、v1.81 が拒否し始めていた瞬間（`created_at` + 31 日、秒単位）に期限切れになります。移行した内部キーは、クライアント ID が既存スタッフの ID と完全に一致し、**かつ**すべてのスコープがスタッフ専用キーで使えるもの（`admin` は不可）である場合だけそのスタッフに紐付けられ、それ以外は紐付けなし（上記のとおり HTTP サーバーで拒否）になります。`admin` などスタッフ専用キーで使えないスコープを持つスタッフの旧キーは、スコープをそのまま移行しますが紐付けません。キー一覧ページには「再発行が必要：スタッフ専用キーに使えないスコープがあります」と表示され、ゲートウェイのログにもヒント（`ddc_prod_…3f9a`）で記録されます。スタッフ ID で新しいキーを発行し、古いキーを削除してください。スタッフに紐付いたキーではスタッフ専用のスコープだけが有効になり、`admin` が他のスコープの代わりになることはありません。`created_at` が現在より数分以上先の項目は、受け付けも選択も移行もされません（無視された項目として表示）。すべての読み取り側（MCP サーバー、OAuth 同意ページ、Google キーの確認）は次の規則に従います。

- `mcp_keys.db` が存在しない（このホームで v1.82 のゲートウェイが一度も起動していない。例：アップグレード後のゲートウェイより先に起動した `mcp-server`）：`[mcp_keys]` を v1.81 の規則（30 日）で検証します。これが唯一の移行期間で、ゲートウェイがストアを作成した時点で永久に閉じます。`mcp_keys.db` の削除にはホームディレクトリへの書き込み権限が必要で、ストアはグループや他のユーザーが書き込めるホームを拒否します。
- `mcp_keys.db` が存在するのに読めない、テーブルがない、スキーマや `user_version` が想定と違う：gateway-internal キーを含むすべてのキーを拒否します（「ストアを利用できません」）。「まだストアがない」とは扱いません。
- ストアにあるキーはストアだけで判断します。`config.toml` に残った古いコピーは無視され、次回起動時に削除されます。
- 「移行完了」マーカーより前は、`config.toml` は移行待ちとして記録された指紋に対してのみ有効です。マーカー以降、内部キー以外で `config.toml` を参照することはありません。後から追加された項目は移行されず、認証にも使われません。ログに一度だけ「無視された旧キー項目」として記録され、キー一覧ページにヒント（`ddc_prod_…3f9a`）で表示されるので、管理者が削除してください。
- `gateway-internal`：ゲートウェイは内部キーを発行・ローテーションするときに、その指紋を `mcp_keys.db` に記録します。受け付けるのはそのキーだけで、`gateway-internal` として、`config.toml` に項目があり、30 日以内であることが条件です。このクライアント ID を名乗るだけの項目は拒否されます。**アップグレード後の初回起動時、ゲートウェイは自身用に新しい内部キーを発行します**（起動時に `DUDUCLAW_MCP_API_KEY` が指定されていればそれを使い、`config.toml` の日付を書き直します）。記録するのはその 1 つだけで、自身の MCP 設定に渡し、同じ起動の後半で各スタッフの `.mcp.json` にも書き込みます。`config.toml` に既にある `gateway-internal` 項目は日付にかかわらず採用も記録もされず、認証にも使われません。ログに一度だけ（ヒントのみで）記録され、キー一覧ページに「無視された旧キー項目」として表示されます。

`http-server` の OAuth 同意ページは、ストア内の有効な内部（外部でない）キー、または上記の規則で使える `config.toml` のキーを操作者の証明として受け付けます。ストアへ移る前にそのキーで同意した認可は引き続き有効です。

**キーの移行は一方向です。** v1.81 は `mcp_keys.db` を読みません。ダウングレード後は外部キーを v1.81 で再発行する必要があります。そのために古い `config.toml` を復元しないでください。v1.82 で削除・無効化したキーが再び有効になってしまいます。

## 社員ごとのツール一覧の整理

Admin は社員の概要にある「ツール一覧の整理」でモードを選び、未使用ツールと推定節約量を確認できます。オンのときは一覧を復元できます。モード選択は既存の社員更新処理を使います。`agents.tool_pruning_status {agent_id}` と `agents.tool_pruning_restore {agent_id}` は Admin 専用です。推定値は候補ツールのシリアライズ済み定義の長さを四で割った値で、tokenizer の実測値やプロバイダーで確認した節約量ではありません。

```toml
# agent.toml
[capabilities]
tool_pruning = "shadow" # "off" | "shadow" | "on"
```

既定の `shadow` は観測と報告のみで、`tools/list` の応答は `off` と同一です。不明または不正な値は `off` と扱い、一度警告します。`on` の場合だけ、既存の全フィルターの後で条件に合うツールを非表示にします。権限の付与や取り消しは行いません。非表示でも既存のディスパッチゲートを通して呼べます。呼び出しに成功すると `notifications/tools/list_changed` により一覧に戻ります。

計算では `tool_calls.jsonl` の末尾 5 MB だけを読みます。ログがない、読めない、または記録済みターンが十未満なら整理しません。十ターン以上かつ二セッション以上、または五セッション以上にわたって未使用のツールが候補です。記憶、タスク、作業状態、メッセージ、承認、wiki 読み取り、ツール探索、Admin／不明な効果のツール、有効なタスク限定許可のあるツールは残ります。ターンはホストが記録する `(session_id, turn_id)` の一意な組であり、ログの行数ではありません。チャネル呼び出しは会話の session と turn、ディスパッチ作業は自身の dispatch session と run を使い、一回の run を一ターンと数えます。これらの項目がない古い行はターンのサンプルにはなりませんが、成功した使用はそのツールの未使用期間をリセットします。各ツールの最後の成功後のターンとセッションで条件を判定します。各プロセスで五分間キャッシュします。復元は社員ごとのリセットマーカーを書き込み、それ以前の使用履歴を計算から外すため、既存の権限で利用できる一覧に戻ります。権限フィルターは引き続き適用されます。

Claude CLI の `minimal_context` は `--tools` で内蔵 CLI ツールを制御し、MCP ツール定義には影響しません。この経路は変更していません。

## ルール: 一覧は認可に従い、整理は探索だけを変更する

`tools/list` は、呼び出し元が**その時点で実際に呼べる**ツールだけを列挙します。認可フィルターはディスパッチャのゲートと対応します。任意の社員別整理は探索一覧だけをさらに絞り込みます。認可済みのツールは、非表示でも呼び出せます。

これは正しさの問題であると同時にコストの問題でもあります。ツールスキーマは**起動のたびに支払う固定のプロンプトコスト**です。CLI はセッションごとに `tools/list` を一度読み、その全体が最初のユーザートークンより前にモデルのコンテキストに入ります。ゲートが拒否するツールのスキーマは二重の損です。トークンを払い、さらにモデルが使えないツールを前提に計画を立てます。

ツールを隠すことは**認可の判断ではありません**。未掲載のツールを呼んでも本物のゲートに到達し、既存の規則により受け付けられるか拒否されます。

### フィルタの一覧

| フィルタ | 参照元 | オフ / 空のときの効果 |
|---|---|---|
| 外部クライアント許可リスト | `principal.is_external` | 従来の許可リスト 7 ツールと、明示的に付与した scope のツールのみ。続けて次の行の scope 規則が適用されます |
| 社員でない呼び出し元の scope | `principal.scopes`。`admin` を持たず AI 社員でもないキー（gateway 内部キーでも、社員ごとのキーでも、gateway が社員用に起動したプロセスでもない）に適用 | キーが最低 scope を持つツールだけを掲載。プロセス自身のエージェントとして動くツール（`working_state_*`、`canvas_*`、`shared_wiki_delete`、`memory_search_by_layer` と三つの統合状態の参照など）は掲載せず、すべての外部キーにも掲載しません。呼び出しはディスパッチゲートが同じ規則で拒否します（`process_agent_tool`）。[mcp-standalone.md](mcp-standalone.md) を参照 |
| Google Workspace | `config.toml [integrations] google_workspace` | オフ時は `google_status` を含む全 38 ツールが非表示。ダッシュボードの設定・テストは可能。[接続手順](google-setup.md)。 |
| Google の身元（v1.76.0） | `agent.toml [google] identity`、なければ `config.toml [integrations.google_identity] default_identity` | `none` なら同じ 29 ツールを非表示にし、呼び出しも拒否。それ以外の値では一覧に残り、使えない呼び出しは理由を返します。`google_status` は最初に社員が誰として動いているかを示します |
| GitHub | `config.toml [integrations] github` | `github_*` 5 ツールを非表示 |
| `denied_tools` / `allowed_tools` | `agent.toml [capabilities]` | 拒否分を非表示。許可リストが非空なら他を全て非表示。項目は完全一致、または末尾の `*` で照合：`*`、`mcp__duduclaw__*`（DuDuClaw の全ツール）、`mcp__duduclaw__odoo_*`／`memory_*`（先頭からの前方一致）。`mcp__<他のサーバー>__…` は DuDuClaw のツールに一致しない。それ以外の位置の `*` は通常の文字。3 つの承認リストと `scoped_tools` も同じ規則 |
| `os_native` | `agent.toml [capabilities]` | `os_*` 自動化 6 ツールを非表示 |
| `recording` | `agent.toml [capabilities]` | 録画系 5 ツールを非表示 |
| `system_operator` | `agent.toml [capabilities]` | アプライアンス操作 19 ツールを非表示 |
| `codrive` | `agent.toml [capabilities]` | `codrive_run` / `codrive_status` を非表示 |
| `computer_use` | `agent.toml [capabilities]` | `computer_*` 8 ツールを非表示（下の [`computer_*`](#computer_--gateway-が実行するセッション) を参照） |
| `db_sources` | `agent.toml [capabilities]` | `db_*` 4 ツールを非表示 |
| `[fork] enabled` | `agent.toml` | 分岐系 6 ツールを非表示 |
| `[knowledge] enabled` とコレクションの付与 | `config.toml`／`knowledge.db`／`agent.toml [knowledge] collections` | ナレッジベースが有効で、社員またはその部門に（`[knowledge] collections` で絞り込んだ後も）1 つ以上のコレクションが付与されていない限り `knowledge_*` 4 ツールを非表示。付与の変更で `list_changed` を送信（下の [`knowledge_*`](#knowledge_ドキュメントナレッジベース) を参照） |
| `[responsibilities] enabled` | `config.toml` | `responsibility_*` 3 ツールを非表示（下の[継続タスクのツール](#responsibility_get--responsibility_followup--responsibility_ask--継続タスクのツール)を参照） |
| `scoped_tools` | `agent.toml [capabilities]` ＋ 有効な付与 | タスクスコープの付与が有効になるまで非表示 |
| チームのロールメンバー（v1.82） | `agents/.ephemeral/eph-*` のロールメンバーで、`[team_member]` と `ephemeral.toml` の上位社員が一致し、ID がトークンで検証済み（`DUDUCLAW_AGENT_TOKEN`） | どの委任モードでも、それ以外の呼び出し元には `team_handoff` を表示しません。そのため通常の社員のツール一覧は以前より 1 つ減ります。ハンドラーも同じ呼び出し元を `not_a_team_member` で拒否します |

機器操作コンソール（`/console`）は管理者だけが利用できます。`system_operator` スタッフが機器操作ツールを実行するときも、サーバーで依頼者が有効な管理者かを確認します。他の AI スタッフとの通常の会話は、従来のアクセス規則で利用できます。

新規作成したエージェントは上記のいずれも有効ではありません。それが狙いで、既定の構成は使えるツールの分だけを支払います。

### セッション途中の変更がどう届くか

かつてツールを隠すことは恒久的に到達不能にすることを意味しました。MCP クライアントはセッション開始時に一度だけ `tools/list` を読むからです。現在サーバーは `initialize` 応答で `tools.listChanged` を宣言し、呼び出し元に見える集合が**実際に変わったとき**に `notifications/tools/list_changed` を送ります。比較は集合単位なので、設定ファイルに触れただけで内容が同じなら何も送られません。

したがって次の流れは再起動なしで動きます。

1. エージェントが `scoped_tools` の拒否に当たる、または運用者が権限を付与したい。
2. `capability_request` が承認される（またはダッシュボードが `agent_update` を保存、あるいは誰かが `agent.toml` を編集）。
3. 数秒でサーバーが変更を検知しクライアントへ通知。
4. クライアントが `tools/list` を読み直し、ツールが現れる。

タスクスコープの付与はタスクが終端状態に入るたび失効し、同じ仕組みでツールも再び見えなくなります。

### 注意: エージェントは `scoped_tools` の名前を自力で発見できません

`scoped_tools` に載ったツールは付与が有効になるまで非表示なので、エージェントが `tools/list` から名前を読んで申請することはできません。別の手段で名前を伝えてください——`SOUL.md` に書く、playbook ルールにする、あるいはゴール開始時に `grant:<tool>` タグで付与を発行する。剪定が代償を伴う唯一の箇所であり、意図的なものです。今この瞬間に拒否されるツールを宣言することこそ、上のルールが取り除こうとしている失敗モードだからです。

### v1.68.0 からの例外：一覧に出るが拒否される

新しい2つのゲートは、ツールを `tools/list` から外さずに呼び出しを拒否します。

- `agent.toml [permissions]`：`false` と書かれたフラグは、`create_agent`（`can_create_agents`）、`send_to_agent`・`spawn_agent`（`can_send_cross_agent`）、`create_reminder`・`schedule` 付きの `tasks_create`（`can_schedule_tasks`）、`skill_hub_install`・`shared_skill_adopt`・`skill_graduate`・`skill_pin`・`skill_from_recording`（`can_modify_own_skills`）を拒否します。拒否は JSON-RPC エラー -32003 と監査イベント `permission_denied` になります。`agent.toml` が存在するのに読めない・解析できない場合はこれらのツールを拒否し、ファイルがなければ許可します。`schedule` なしの `tasks_create` は許可されるため、これらのフラグは呼び出しごとに確認されます。一時的な役割メンバーは `can_create_agents`、`can_modify_own_skills`、`can_schedule_tasks` が `false` で作られます。
- `config.toml [odoo] features_*`：Odoo ツールは一覧に残り、無効になったモジュールのモデルへの呼び出しは呼び出しごとに拒否されます（project と hr は既定でオフ）。

### レコードの関係チェック：一覧に出るが拒否される

AI 社員のものになっているレコードを変更・起動するツールがあります。どの呼び出し元にも一覧には出し、呼び出しごとに関係を確認します。

- **タスク**：`tasks_update` と `task_id` 付きの `activity_post` は、タスクの割り当て先・引き受け者・作成者ならそのまま通ります。`tasks_complete` と `tasks_block` は割り当て先と引き受け者だけ、`tasks_claim` は未割り当てか自分に割り当て済みのタスクなら通ります。それ以外は割り当て先との委任関係が必要です（同じ部署、`reports_to` の上下、またはホワイトリストのペア。`[delegation] policy` に従う）。未割り当て・未引き受けのタスクは先に引き受けます。`tasks_update` で他人のタスクを自分に割り当て直すには、常にその関係が必要です。
- **タスクのフィールド**：AI 社員はゴールモードのタスクの `title` と `description` を変更できず（`acceptance_criteria` はすべての MCP 呼び出し元が変更不可）、`tasks_update` でも `tasks_create` の `tags` でも、`outcome:`・`grant:` で始まるタグと `auto-research` タグを追加・削除・並べ替えできません。
- **新しいタスクの親**：ゲートウェイは、いまのラウンドが扱っているタスクを MCP サーバーに伝えます（`DUDUCLAW_TASK_ID`。承認カードが持つ値と同じ）。そのタスクが呼び出し元自身のもので、[継続タスク](continuous-responsibilities.md)のある 1 回の実行に属する（実行そのもの、またはその配下のタスク。タスクボードから起動されたサブタスクを含む）場合、`parent_task_id` を指定しない `tasks_create`（`kind="goal"` を含む）はその下に置かれ、社員が指定する `parent_task_id` はそのタスクかその子タスクでなければ拒否されます。値があるのに空や形式違いの場合は拒否され、「ラウンドなし」とは扱われません。それ以外（通常の goal ラウンド、実行の外にあるタスクのタスクボード起動、ラウンドの情報がない場合）は親を既定にしません。このリリースですべての呼び出し元に新しく加わる規則：指定した `parent_task_id` には親タスクとの関係（担当、引き受け、作成者、または委任ポリシー）が必要です（v1.69 は確認せずに書き込んでいました）。`kind="goal"` も `parent_task_id` を受け付けます（v1.69 は無視していました）。AI 社員が 1 つのタスクの下に作れる未完了の子タスクは最大 200 件です（`schedule` で作る定期業務やリマインダーは子タスクではありません）。Bash から起動したサーバーと、Grok ランタイムにはラウンドの情報が渡りません。そこで作られたタスクは、社員がその実行を親に指定しない限り実行の配下に置かれず、その費用にも含まれません。正しく配下に置かれるには、社員が自分の `.mcp.json` の `duduclaw` 項目を書き換えられないことも必要で、これは先に合併すべき別のプラットフォーム修正です。
- **定期業務**：`update_cron_task`、`delete_cron_task`、`pause_cron_task`、`run_cron_task` は、呼び出し元がその定期業務を実行する社員本人か、その社員と関係がある場合に通ります。`name` で指定すると 1 件だけに作用し、同名が複数あれば候補 id を示して拒否します。
- **リマインダー**：`create_reminder` の `agent_id` が呼び出し元以外なら、その社員との関係が必要です。
- **自分への `agent_update`**：AI 社員は自分について `reports_to`、`db_sources`、`db_sources_add`、`db_sources_remove`、`budget_cents`、`reply_language`、`memory_language`（v1.84.0）、`runtime`、`timeout_secs`、`max_turns`、`role` を送れません（監査 `agent_authority_refused`）。
- **`agent_update` の権限変更**：身元、モデル、予算、ハートビート、コンテナ、データベースソースの各フィールドに対応し、アトミック書き込みで保存します。AI 呼び出し元は権限フィールドを縮小する変更だけが可能で、部下の編集にも適用されます。予算の増額、能力やデータベースソースの範囲拡大は、管理者がダッシュボードで行う必要があります。自分への編集制限と組織権限チェックも引き続き適用されます。管理者の操作が必要な場合は、依頼した人に伝えてください。

オペレーター（どの AI 社員にも対応しない MCP キーで、プロセスも社員用に起動されたものではない）は制限されません。社員身分のないプロセスの内部共有キーはどのレコードも所有しないため、誰のレコードでも拒否されます。身分がシステム送信者名（`dashboard`、`cron` など）のプロセスは信頼できない身分として扱われます。拒否はすべて `tool_calls.jsonl` に記録されます。詳細は[タスクボード](../../features/ja-JP/24-task-board.md)、[委任の隔離](../../features/ja-JP/37-delegation-isolation.md)を参照。

## 説明のバイト予算

各ツールの `description` は **200 バイト**、各パラメータの説明も **200 バイト**が上限です（明記された例外が 1 件だけあります）。この上限は慣習ではなくテストで強制されます。

ツールを追加・編集する人にとっての帰結は 2 つです。

- **何をするか、何が拒否されるかを書く。** 安全上重要な一文——「これは送信しない」「これは公開される」「超過分は丸ごと拒否、決して切り詰めない」——は説明に残します。設計理由・実例・内部ラベルは残しません。
- **長い版はこのページに書く。** 説明からこのページの節、あるいはそのツール自身の仕様ページへリンクします。

### 唯一の例外

`team_handoff` の `packet` パラメータは TaskPacket の完全な形を保持します（上限 1,024 バイト）。`build_tool_schema` は `PARAM_TYPES` の短いリストにある整数・数値・真偽値・文字列配列を除き、パラメータを素の JSON-Schema 文字列として宣言するため、TaskPacket のようなオブジェクトの実際の形を書ける場所はパラメータ説明だけです。さらに TaskPacket は切り詰めずに丸ごと拒否されるので、形が見えないエージェントは有効なパケットを作れません。完全な仕様は [../../spec/task-packet.md](../../spec/task-packet.md) です。

例外は 1 つのリスト（`PARAM_CAP_EXEMPTIONS`）にまとめられ、例外が不要になったときにテストが落ちます。

## ツールの annotations

`tools/list` の各ツールには MCP の `annotations` が付きます。ディスパッチゲートと `action_rules` が使うのと同じ効果分類表（`duduclaw_core::effect_of`）から導出します。

| 効果分類 | `readOnlyHint` | `destructiveHint` |
|---|---|---|
| `read` | `true` | （付けない） |
| `draft`・`send` | `false` | `false` |
| `publish` | `false` | `true`（公開は他者が読んでいる内容を置き換えうる） |
| `modify` | `false` | `true`。ただし追加・再生成だけの `memory_store`・`user_profile_record`・`wiki_rebuild_fts` は `false`。`wiki_export` は前回のエクスポート（人が編集したかもしれない Obsidian vault を含む）を上書きするため `true` のまま |
| `delete`・`purchase`・`admin` と表にない名前 | `false` | `true` |

メモリ・wiki・ユーザープロファイル・`code_map` のツールは `openWorldHint: false` も付きます。`memory_alias_add`・`wiki_export`・`wiki_rebuild_fts` は `idempotentHint: true` です。これらのリストは `crates/duduclaw-core/src/mcp_tool_defs/annotations.rs` にあります。

ヒントは MCP クライアント向け（呼び出し前に確認するかどうかなど）で、権限を与えるものではありません。DuDuClaw のゲートは毎回そのまま動きます。Claude Code と Codex はモデルに名前・説明・入力スキーマだけを渡すため、上の説明バイト予算には含めません。

## 説明から移した長い詳細

### `tasks_create` のスケジュール

cron `schedule` で `cron_timezone` を省略すると会社タイムゾーンを使います。未設定または無効の場合は UTC を使い、確認に明記します。会社タイムゾーンに夏時間がある場合、メンバー作成のルーティンは UTC で保存し、その旨を通知します。単発の `schedule` 時刻には UTC オフセットが必要です。例：`2026-11-28T09:00:00+08:00`。



### `tasks_create`／`tasks_update` — ゴール費用上限

`tasks_create` は `kind="goal"` のとき `cost_cap_cents` を受け付けます。米セント単位の正の整数（最大 `9223372036854775807`）です。省略すると `[goal_loop] default_cost_cap_cents`（`0` は上限なし）を使います。通常タスクと予約のリクエストでは拒否されます。例：`{"kind":"goal","title":"報告書を確認","assigned_to":"agnes","cost_cap_cents":500}`。ゴールの説明や受け入れ基準は通常どおり記入します。

`tasks_update` はゴールに対して同じフィールドを受け付けます。社員は上限の追加・引き下げだけが可能で、有効な上限の引き上げや削除はできません。引き上げは操作者が行います。上限の保存だけでは再開しません。ダッシュボードの「上限を引き上げて再試行」は保存後に既存の再試行決定を使います。タスクツリーの測定費用が上限以上なら `budget_exhausted` で停止し、費用を読み取れない場合はディスパッチを見送ります。[ゴールループ](../../features/ja-JP/34-goal-loop.md)を参照してください。

### `tasks_create` — ゴールごとの実行時間の上限

`tasks_create` は `kind="goal"`（予約なし）のとき `timeout_secs`（30～3600 の整数秒）を受け付けます。そのゴールの各ラウンドの CLI 実行／API リクエストごとの上限で、担当従業員自身の上限に代わります。MCP からは従業員自身の上限以下しか指定できません。ゴールにより長い時間を与えるのは、ダッシュボードでの管理者の判断です。Claude の従業員にも適用され、実行時は現在のグローバル `[runtime] max_timeout_secs` が上限になります。`tasks_update` はこのフィールドを拒否します。例：`{"kind":"goal","title":"帳簿を照合","assigned_to":"anna","timeout_secs":180}`。

### `office_script` — 同梱 Office スクリプト

`office_script` はバイナリに同梱され manifest で検証された `docx`、`xlsx`、
`pptx`、`pdf` 用スクリプトを実行します。`create`、`extract`、`to_pdf` を選択します。
`.py` 拡張子は省略でき、スクリプト名にパスは指定できません。社員のスキルフォルダーは
プロンプト／指示だけを変更でき、実行コードは置き換えられません。Linux と macOS に対応します。

`args` は JSON 文字列配列で渡します（例：`["outline.md", "--out",
"attachments/deck.pptx"]`）。入力は社員ディレクトリまたは共有添付ファイルから指定できます。
出力は社員ディレクトリ内の新規ファイルとし、親ディレクトリが存在し、拡張子が一致する必要があります。
フラグと値は別々の要素にします。`--key=value`、シンボリックリンク、パストラバーサルは拒否されます。
生成したファイルを渡すには、返信の最後に `DELIVER:<absolute path>` を記載します。

### `codrive_run` — 3 段の実行ラダー

各ステップは次の順に 3 段を試し、当てはまる最上段を優先します。

- **C-L2 — `api_action`。** GUI に触れる前に、`target_app` の登録済みサードパーティネイティブ API/CLI/D-Bus アクションを呼びます。必要な app とアクションが登録済みなら常にこちらを優先します。`action` は短い登録識別子（chromium の `open_url`、networkmanager の `state` など）、`params` はそのアクションのペイロードで、ディスパッチ時にアクション自身のスキーマで検証されます。登録なしや実行失敗はステップの `action` にフォールバックするため、`api_action` を設定していても `action` は必須です。
- **C-L3 — `locate`。** `move`/`click` の座標を、手書きのピクセルではなく `target_app` の AT-SPI2 アクセシビリティツリー上の `(role, name)` 参照で解決します。レイアウト・解像度・テーマの変更にはるかに強い方法です。`text` / `key_name` / `wait` / `take_over` では無視されます。見つからない場合は素の `x`/`y` にフォールバックします。
- **C-L1 — 素の `x`/`y`。** 上の 2 段が無いか失敗したときの最終手段。

その他のスクリプトレベルの規則:

- `target_app` は**スクリプト単位の単一フィールド**で、ステップ単位ではありません。1 スクリプトは 1 app を操作します。別の app を操作するには `codrive_run` をもう一度呼びます。
- 結果を伴うステップ（`send` / `submit` / `delete` / `purchase` / `other`）は、どの段がディスパッチする前にも人間の承認で停止します。拒否リスト該当（銀行ページ、CAPTCHA 回避など）は接続を試みる前に即座に拒否されます。
- ログイン／パスワード／支払いのステップ（`take_over`、または `credential` クラス）は共有デスクトップを人間に渡します。資格情報のテキストをどの段からも自分で送ることはありません。人が操作を返した時点でスクリプトが再開します。
- 共有デスクトップで人間の入力があれば、エージェントの席は即座に凍結します。落ちたステップは人が操作を返した後に一度だけ再試行されます。
- `watch_mode: true` で、実行の残り時間にわたりアイドル監視が有効になります。
- 最大 50 ステップ。

### `working_state_handoff` — 構造化モード

2 つのモードがあります。

- **素のノート。** `status` を省略すると従来どおりの挙動で、約 1,200 文字で黙って切り詰められます。
- **構造化（Ralph ループ方式）。** `status` を渡すと `next_steps` / `evidence` / `blocker` と併せて検証されます。
  - `continue` — `next_steps` が必須、`blocker` は不可。
  - `complete` — `evidence` が必須、`blocker` と `next_steps` はいずれも不可。証拠のない自己申告の完了や、次の手順が残ったままの完了は拒否されます。「終わりました」は証拠ではありません。
  - `blocked` — 具体的な `blocker` が必須。

  結合後のペイロードは `config.toml [memory] working_state_handoff_max_bytes`（既定 16384、CJK 安全なバイト数）で制限されます。超過は**呼び出し全体を拒否**し、決して黙って切り詰めません。切り詰めれば、この引き継ぎを権威たらしめている証拠そのものを消しかねないからです。

### `skill_search` — source の選び方

そのスキルがどこにあるか既に分かっている場合を除き、`source` は触らないでください。

- `all`（既定）— 設定済みハブとこのエージェントが学習したスキルバンクを検索し、名前で重複排除して出典を付けます。ハブ結果は関連度 × 信頼 × インストール数 × 鮮度で順位付けされ、公式ファーストパーティのスキルが上位に底上げされます。
- `github` — 公開 GitHub リポジトリにあると分かっているスキル。
- `hub` — キュレーション済みレジストリ（`anthropic-skills`、`github`、`clawhub`、`lobehub`、`skills-sh`）。`hub` パラメータで 1 つに絞れます。
- `bank` — この構成が自力で学習したものだけ。この source では `hub` パラメータは受け付けません。

### `evolution_toggle` — フィールドと権限

`field` は `gvu_enabled`、`skill_budget_unlimited`（真偽値、管理者専用）、`max_silence_hours`、`skill_token_budget`、`max_active_skills`（数値）を受け付けます。旧 `stagnation_*` フィールドはこのツールでは変更できません。[evolution-switches.md](../evolution-switches.md) を参照してください。

AI スタッフは元の 4 フィールドについて、自分または部下の設定を変更できます。`agent_update` と同じ身元・組織権限チェックを使い、委任ポリシーが `open` の場合はチームをまたぐ変更も許可されます。人間のオペレーターはこの制限を受けません。`skill_budget_unlimited` はスキル内容の注入量上限をなくすため、管理者だけが変更できます。身元または組織権限のチェックを通過しない場合は、対象の設定ファイルを読む前に拒否します。

### `execute_program` — スクリプトの実行場所

スクリプトはスクリプトサンドボックスで実行されます。`config.toml [container.sandbox] image` のイメージから作るコンテナで、[タスクサンドボックス](task-sandbox.md)と同じイメージです（自動ではダウンロードされません）。コンテナはホストのユーザーで動きます（ホストのプロセスが root のときは `1000:1000`、WSL2 では常にこちら）。すべての capability を破棄し、`no-new-privileges`、読み取り専用のルートファイルシステム、ネットワークなし、2 GiB のメモリ（swap なし）、256 プロセス、1 CPU、小さな `/tmp` tmpfs が適用されます。マウントされるのはスクリプトを置いた専用ディレクトリだけで、`/workspace` に読み取り専用でマウントされます。`timeout_seconds`（既定 30、最大 300）に加えて 600 秒の絶対上限があり、stdout と stderr は 1 つの出力として返ります（読み取り上限 2 MiB、返信上限 1 MiB）。呼び出しがキャンセルされるとコンテナは強制削除されます。macOS と Linux では Docker を使い、Windows ではまず WSL2、次に Docker を試します。

サンドボックスが使えないとき（Docker がない、イメージがない、`[container.sandbox]` が無効など）、スクリプトは**実行されません**。ツールは `docker pull <image>` のコマンドを含む `Script sandbox unavailable (<コード>): …` を返し、監査イベント `script_sandbox_unavailable` を書きます。以前のバージョンでは、この場合に黙ってホスト上で実行していました。その動作に戻すには `[container.sandbox] script_when_unavailable = "run_unsandboxed"` を設定します（タスクサンドボックスの `when_unavailable` とは別のキーです）。ホストでの実行は毎回 `script_sandbox_bypassed` として監査されます。

スクリプトからプラットフォームのツールを呼び返すことはできません。コンテナ内に RPC ソケットはありません。

### `computer_*` — gateway が実行するセッション

9 つのツールは、スタッフごとに 1 つの Computer Use セッションを駆動します。仮想ディスプレイとキオスクブラウザを持つ、隔離されたコンテナです。MCP サーバーは各呼び出しを loopback 経由で gateway に転送するだけで（`POST /api/internal/computer-use`、リクエストごとに署名）、コンテナを所有してすべてのチェックを行うのは gateway なので、gateway が動いている必要があります。`agent.toml [capabilities] computer_use = true` でない限り非表示です。

| ツール | パラメータ | 備考 |
|---|---|---|
| `computer_session_start` | `task` 文字列、任意。`width` 整数 320–1920。`height` 整数 240–1200 | スタッフごとにセッションは 1 つ。結果には、上限、高リスクのアクションをチャットで確認できるか、`computer_navigate` が開けるサイトが載る |
| `computer_screenshot` | なし | MCP の画像ブロック（PNG、マスク済み）に続いて、使用済みアクション数と残り時間のテキストブロック。全体がマスクされた画像は、その旨が理由（複数のウィンドウ、機密性のある、または読み取れない前面ウィンドウ、検出の失敗）と次の手順とともにテキストで報告される |
| `computer_click` | `x`、`y` 整数（必須）。`button` 文字列 `left`/`right`。`double` 真偽値 | `double` は左ボタンのみ |
| `computer_type` | `text` 文字列（必須）、1–2,000 文字 | 監査には文字数のみ記録される |
| `computer_login` | `credential`（必須、認証情報のラベル文字列） | 許可されたスタッフのログインを gateway が入力・送信し、状態と説明だけを返す |
| `computer_key` | `key` 文字列（必須）：英字、数字、`+`、`-`、`_` | 例：`Return`、`ctrl+s` |
| `computer_scroll` | `x`、`y` 整数（必須）。`direction` 文字列 `up`/`down`（既定 `down`）。`amount` 整数 1–20（既定 3） | |
| `computer_navigate` | `url` 文字列（必須） | `https://` のみ。ホストはスタッフの `[capabilities.computer_use_config] allowed_domains` に完全一致し、かつセッション開始時に解決できたもの。ポートはないか 443、ユーザー名・パスワードなし、2,000 バイト以下。許可リストがなければセッションにネットワークはなく、呼び出しは拒否される |
| `computer_session_stop` | `session_id` 文字列、任意 | コンテナを削除する |

`computer_login` は 9 番目のセッションツールです。管理者が **システム → セキュリティ → Web サイトのログイン情報** で HTTPS `login_url`、ラベルと 1 人以上の許可スタッフを保存します。パスワードはそのフォームだけに入力し、チャットには貼りません。任意の `allowed_paths` は既定で空で、ヘルパーが最初のフォームを送信した後に到達した、パスワード欄が表示される二段階目の同一オリジンの正確なパスだけに使えます。Gateway は既存の検査で保存済み URL を最初に開き、ナビゲーションとログインはそれぞれ 1 アクションを消費します。ヘルパーは正確なオリジンとパス、有効な POST を検査します。ネイティブの `requestSubmit` と代替経路の送信ボタンの `formmethod` も対象とし、submit ハンドラー後も再確認します。各リクエストとリダイレクトの別オリジンを拒否し、完全な認証値をクエリに含む非 POST リクエストも拒否して、サービスワーカーを迂回します。MCP リクエストに含むのはラベルだけです。セッションの開始・状態レスポンスには呼び出し元が使えるラベルのみを表示し、ユーザー名は含めません。`needs_takeover` なら人に既存の引き継ぎ機能を使ってもらいます。OTP 入力ツールはありません。`logged_in` はパスワード欄が消えたことのみを意味します。消去を確認できない場合（`failed_uncleared`）、セッションとコンテナを停止・削除し、新しいセッションを求めるエラーを返します。ナビゲーションで復帰できません。インターセプトを解除する前に、ヘルパーは `Network.requestWillBeSent`、`Network.loadingFinished`、`Network.loadingFailed` を追跡し、リクエストの終了を確認します。最大 3 秒待機し、保留リクエストも新しい活動もないコマンド往復が 2 回連続することを要求します。完了を確認できない場合は `failed_uncleared` とし、Fetch の制限を維持したまま `Browser.close` を呼び出して、その後 Gateway がセッションとコンテナを削除します。処理中もスクリーンショットを抑止します。

ログイン先は保存したオリジン（スキーム、ホスト、ポート）に限定され、フォームの送信先も同一オリジンでなければなりません。localhost を含め HTTPS が必要です。同一オリジンの JavaScript は入力済みフィールドを読み取れます。二要素認証には人の操作が必要です。使用前に Computer Use コンテナイメージを再ビルドして新しいヘルパーを含めてください。

整数と真偽値のパラメータは、数値の文字列と `"true"`/`"false"` の文字列も受け付けます。クリック、入力、ログイン、キー、スクロール、ナビゲートはそれぞれ 1 アクションとして `max_actions`（既定 50）に数えられます。上限、承認と確認のルール、ネットワーク許可リストとその残存リスクは[ブラウザ自動化](../../features/ja-JP/08-browser-automation.md)にあります。

### `os_*` — DuDuClaw OS でのデバイス操作

root が必要な `os_*` ツール（電源、更新の状態、sysd が再検証する更新のインストール、起動評価、ロールバック、工場出荷時リセット）は、MCP サーバー内でこれまでのチェック（Admin スコープ、`[permissions]`、確認、工場出荷時リセットの承認）をすべて通したうえで、ループバック経由でリクエストを gateway に渡します（`POST /api/internal/sysd`、`computer_*` と同じくリクエストごとに署名）。gateway は自分の接続で `duduclaw-sysd` に転送するため、sysd が gateway プロセスしか受け付けない構成でもツールは使えます。このルートが受け付けるのは上記のコマンドだけで、SSH、ネットワーク、ホスト名、タイムゾーン、ローカルモデルの操作は拒否されます。これにはデバイス上のプラットフォームが v1.73.4 以降である必要があるため、v1.73.4 を内蔵する DuDuClaw OS v0.4.0 から適用されます（v0.3.0 は v1.73.1 を内蔵しており、ツールは sysd を直接呼び出します）。gateway が動いていない場合や、MCP サーバーが gateway から起動されていない（社員の身元がない）場合は理由付きで失敗し、sysd に直接接続することはありません。DuDuClaw OS 以外では変わりません。

### `belief_stats` / `belief_settle` — 検証済みと自己申告の決済

キャリブレーションに数えるのは、プラットフォームの価格と照合した決済だけです。現時点では本番でこの照合を行う経路はなく、`belief_settle` はすべての決済を社員自身の申告（`settle_source = "agent_unverified"`）として記録します。そのため既存のどの環境でも、キャリブレーションは「検証済みの決済なし」になります。

`belief_settle` は決済後の行に `counts_toward_calibration`（真偽値）を加えて返し、`false` のときは、記録はされたがキャリブレーションには数えない旨の `note` を付けます。

`belief_stats` が返す内容（ダッシュボードの `belief.summary` の `stats` と同じものに `note` を追加）：

| フィールド | 意味 |
|-----------|------|
| `n_submitted` | 提出されたすべての信念（決済済みかどうかを問わない） |
| `n_settled_all` | 決済済みのすべての信念（`verified.n + self_reported.n`） |
| `calibration_status` | `no_verified_settlements`、`insufficient_samples`（検証済み 1〜29 件）、`calibrated`（30 件以上） |
| `verified` | `n`、`hits`、および `hit_rate`、`hit_rate_wilson_low`、`mean_brier`、`overconfidence`（`calibrated` 以外ではすべて `null`） |
| `self_reported` | `n` と記述用の `hit_rate`（`n` が 0 なら `null`）。キャリブレーションではない |
| `per_subject[]` | `subject`、`verified`（`n`、`hits`、`mean_brier`）、`self_reported`（`n`） |

以前のフラットなフィールド（`n_total`、`n_settled`、`insufficient_samples`、トップレベルの `hit_rate` など）は削除されました。[信念ループ](../../features/ja-JP/46-belief-loop.md)を参照。

### `create_agent` / `agent_remove` — 削除された名前は予約される

`agent_remove` は社員を `~/.duduclaw/agents/_trash/` に移動し、社員が削除されたこと、管理者が復元できること、名前が予約されていることを答えます。パスは返しません。その後 `create_agent` は、ゴミ箱にエントリがある間、`org.toml` がディレクトリのない id を記録している間、またはゴミ箱を一覧できない間、すべての MCP caller についてその名前を拒否します。別の名前なら動作します。運用者はダッシュボードまたはターミナルから名前を再利用できます。非内部キーで HTTP 越しに呼ぶと、どちらのツールもそのキー自身の client id として動作します。詳細：[委譲の隔離](../../features/ja-JP/37-delegation-isolation.md#削除された社員の名前は予約されたままになる)。

### `create_agent` — ペルソナと枠（v1.82.0）

ペルソナは `soul_md` に入れます（`soul` は旧名。空文字は渡していないものとして扱い、両方に異なる内容がある場合のみエラー）。テンプレート作成と同じ SOUL.md の安全チェックを通り、チェックに通らない場合や SOUL.md を書き込めない場合は呼び出し全体が失敗し、ディスクには何も残りません。拒否メッセージは理由の種類（HTML コメント、不可視文字、隠しコンテンツ、プロンプトインジェクションの言い回し）を示しますが、内容は引用しません。ツールが読まない引数は拒否されず、返答の「Ignored arguments (no effect)」に並びます（最大 20 個、各 64 文字まで、残りは「… and N more」）。ライセンスの枠は削除済み社員と、有効なメンバー本人が Owner として所有する検証済み Personal 分身を除いて数え（１人１体。アーカイブした AI は枠を使います）、枠がいっぱいで拒否するときは上限と使用中の枠数だけを返し、枠を使っている社員は一覧しません（管理者はダッシュボードで確認できます）。枠の確認と作成は一つの手順で、ダッシュボードやエキスパートパックのインポートと同じロックを使うため、同時に作成しても上限を超えません。社員は自分の `[agent] status` をファイル操作ツールで変更できず、`agent_update` もアーカイブ済み・削除済みの社員の `status` は変更できません。アーカイブ・復元・削除はダッシュボードで行います。あとからペルソナを変えるには[作成後に社員のペルソナを変更する](dashboard-settings.md#作成後に社員のペルソナを変更するv1820)を参照してください。

### `responsibility_get` / `responsibility_followup` / `responsibility_ask` — 継続タスクのツール

[継続タスク](continuous-responsibilities.md)用の 3 つのツールです。`config.toml [responsibilities] enabled` が true で、呼び出し元が AI従業員の身元であるときだけ掲載されます（外部クライアントには見えません）。機能がオフの間はすべての呼び出しが拒否されます。`tasks_*` と同じく Admin スコープが必要で、ゲートウェイ自身のキーはこのスコープを持っています。

| ツール | 呼べる人 | ルール |
|---|---|---|
| `responsibility_get` | どの AI従業員も可。運用者キーはどの継続タスクも読める | `responsibility_id` なし：呼び出し元自身のものを一覧。id あり：概要（スケジュール、購読、今期の費用と回数、実行中の 1 回、`cost_not_counted`）。他の従業員の継続タスクは、委譲ポリシー上で呼び出し元と持ち主に関係がない限り「見つからない」になる |
| `responsibility_followup` | その継続タスクを持つ AI従業員。自分の実行が開いていて、継続タスクが active のときだけ | `due_at`（RFC 3339）に 1 回限りの起床を予約。現在から `min_wake_interval_secs` 以上先、`stop_at` 以前。今期の回数上限に数えられ、使い切っていれば拒否。黙って別の時刻にずらすことはない |
| `responsibility_ask` | `responsibility_followup` と同じ | 質問 1 つ（最大 1000 文字）、選択肢は最大 5 つ（各 1000 文字まで）。`ttl_secs` は既定 1 日で、60 秒から `stop_at` までに収める。質問はインジェクション検査を受け、担当者には 200 文字に切った引用として見せ、該当があれば警告を付ける。プッシュは継続タスクの通知条件と期間ごとの上限に従う。回答は次の実行にデータとして渡され、権限は与えない。その実行を停止すると質問も取り下げられる |

1 つの継続タスクで AI従業員が予約した待機中の起床は同時に 1 つだけなので、待機中の `responsibility_followup` と `responsibility_ask` は両立しません。運用者キーはこの 2 つの起床ツールを使えません（従業員になりすますことになるため）。固定のエラーコード：`not_found`、`not_active`、`no_open_occurrence`、`invalid_due_at`、`period_occurrence_limit`、`agent_followup_limit`、`epoch_changed`、`invalid_question`。

継続タスクを作成・変更・再開・再有効化するツールはなく、指示を送るツールもありません。

### `memory_search`／`memory_read`／`decision_*`：チャット会話での読み取り範囲（v1.83.0）

社員プロセスがチャット会話の1ターンを処理しているとき、`memory_search`、`memory_read`、`memory_fetch_batch`、`memory_search_by_layer`、`memory_get_history`、`memory_get_at`、`memory_improve` が返すのは、社員自身が明示的に保存した記憶、管理者がインポートした記憶や旧システムから移行した記憶、およびシステムが同じ会話から整理した記憶（記録された出所で判定）だけです。システムが他の会話から自動整理した記憶（重要な事実、ユーザーの好みとその要約、会話の要約、統合された記憶、記録された未決事項）は返さず、出所が記録されていない自動整理の記憶も返しません。その ID を `memory_read` で読むと、存在しない ID と同じ応答になります。`user_profile_get` と `user_code_profile` も同じ規則に従い、残るデータがすべて他の会話のものなら、プロフィールのないユーザーと同じ応答になります。`decision_list` と `decision_resolve` もこの会話で記録された未決事項だけを扱い、他の会話の未決事項 ID には「見つからない」と応答します。`memory_successful_conversations` は同じ規則で見える記録だけを返し（規則を適用してから `limit` 件を取り、会話が不明なら何も返しません）、`group_history_read` はこの会話が属するグループまたはスレッドだけを読め（1対1のチャットはどのグループも読めず、ほかの対象には通常の「利用できません」の応答を返します）、`memory_invalidate_by_origin`、`memory_episodic_pressure`、`memory_consolidation_status` は社員のすべての会話にまたがるため固定のエラーで拒否されます。スケジュール実行・依頼による実行と外部 MCP クライアントの動作は変わりません。

**v1.84.0 のチャットターン。** `memory_search`、`memory_search_by_layer`、`memory_get_at`、`memory_improve` は検索そのものに会話のルールを適用するようになりました（全文検索、グラフのランキング、ベクトル検索はいずれもこの会話から見える行だけを数えます）。そのため、`limit` の前にほかの会話の記憶がこの会話自身の結果を押し出すことはなくなりました。`memory_get_at` は、別の会話がより新しい値を記録していても、この会話のその時点で最新の値を返します。`memory_get_history` は v1.83.0 のクエリ後の絞り込みのままです。`memory_store` の拒否からほかの会話の情報は漏れません。重複チェックはこの会話から見える記憶だけと比べて "Not stored: this is already remembered (a near-identical memory exists)." と返し、信頼度の保護は "Not stored: a more trusted value already exists, so it was not changed." と返します。どちらも ID を含みません（ID はサーバーログにだけ記録します）。社員が明示的に保存する記憶については、置き換えと信頼度の保護は社員全体を範囲とするため、別の会話の行が 2 つ目の拒否を引き起こすことはあります。ユーザープロフィールはプラットフォームとユーザー ID をキーにします（`user:<channel>:<id>`。WebChat は従来どおり `user:webchat:<owner>`）。プラットフォームは常にそのターンから取り、引数からは取りません。ターンの会話を特定できない場合、`user_profile_record` は拒否します。ある会話で記録したプロフィールの値は、社員がチャットのターンで自分で `user_profile_record` を使って書いた値も含め、別の会話の値を置き換えなくなりました。スケジュールや委任の作業が書いた値は `user:<id>` をキーにし、社員全体を範囲として判断します。一方、社員が `memory_store` で保存したエントリーは、すべての会話の同じ事実の古い値を置き換えます。アップグレード後の最初の起動時に、出どころがすべて同じプラットフォームの会話である既存のプロフィール行を一度だけ新しいキーに書き換えます。複数のプラットフォームが混ざる行、会話以外の出どころを持つ行、出どころのない行は `user:<id>` のまま残り、LINE と Discord でだけ、新しい行が見えないときの予備として読みます。ID の形式が正しい場合に限ります（LINE：`U` と 16 進 32 文字、Discord：17〜20 桁の数字）。スケジュールや委任の実行と外部 MCP クライアントは、渡された値のままプロフィールを扱います（チャネルのプロフィールには `telegram:alice` の形式で指定します）。チャットターンでの wiki ツールは [Wiki ナレッジレイヤー](../../features/ja-JP/17-wiki-knowledge-layer.md)を参照してください。

### `knowledge_*`：ドキュメントナレッジベース

`knowledge_search`、`knowledge_get_passage`、`knowledge_list_collections` は[ドキュメントナレッジベース](../../features/ja-JP/63-knowledge-base.md)を読みます。スコープは `knowledge:read` で、外部 MCP キーには付与できません。呼び出しはすべて署名付きローカルルート `POST /api/internal/knowledge` でゲートウェイに渡され、ゲートウェイが読めるコレクション（社員本人と部門への付与を `agent.toml [knowledge] collections` で絞り込み、ターンの相手と機密度ルールとの共通部分）を決め、アクセスログを 1 行書きます。ゲートウェイが動いていないときはその旨を返し、データベースを直接読むことはありません。

結果は文書の内容であることを明示します。各段落（およびコレクション名と説明）は印の付いたブロックに入り、各行は `│ ` で始まります。タイトルと位置は 1 行にまとめ `[` `]` を置き換えるため、文書が結果の見出しを偽造したりブロックを閉じたりできません。`knowledge_search` は段落を `[n] タイトル · 位置（コレクション） chunk_id=…` と番号付けし、返信で引用できます。取り込み時に指示らしい文と判定された段落には警告行が付きます。`collections` は絞り込みのみです。`knowledge_get_passage` は毎回権限を再確認し、存在しない段落と権限のない段落に同じ応答（`permission denied`）を返します。チャネルや WebChat の返信中は、番号がそのターンの引用番号の続きになります（すでにプロンプトに入った段落は元の番号のまま）。そのため回答の `[n]` は読み手が開く段落と一致します。

`knowledge_add_source`（スコープ `knowledge:write`、こちらも外部キーには付与不可）は、社員側のコレクションに文書を提案します。`collection` と、`path`（社員自身の `attachments/` 内のファイル）・`url`・`text` のいずれか 1 つ、任意の `title` を渡します。ツール自体は何も追加せず、`knowledge_add_source` 承認カードを作成してその ID を返します。カードはダッシュボードで管理者またはそのコレクションの編集者だけが決定でき、公開訪問者・未紐付けの送信者・未登録のターンでは拒否し、その社員の保留中カードが多すぎる場合も拒否します。どの呼び出し元にも免除はありません。

`knowledge_search` と `knowledge_list_collections` は、応答の先頭に検索モード（ハイブリッド＝キーワードと意味ベクトル、またはキーワードのみ）を示し、ベクトルを使わなかった場合はその理由（モデル未インストール、クラウド提供元が未承認、匿名化が無効、クラウドのエラーなど）を添えます。`knowledge_list_collections` は各コレクションのベクトル作成率も返します。ベクトルが段落を並べ替えるのは、その呼び出しが読めるコレクションの中だけです。

### `odoo_schema_overview`：保存済みの Odoo 構造

- `odoo_schema_overview` は **`odoo:read`** が必要です。引数なしではスキャン済みのモデル名と短いラベルを一覧表示します。`model` で 1 モデルのフィールドを取得し、`query` でモデル／フィールド名とラベルを検索できます。社員の Odoo モデル／アクション設定、機能モジュールのスイッチ、およびその社員の有効なブロック解除リストを適用したブロックリストで結果を絞り、ブロック中のモデルの名前・フィールド・リレーションは返しません。構造を見られても、既定でブロックされた業務レコードへのアクセスは許可されません。
- 構造スキャンは dashboard の管理者が「Odoo の構造を再スキャン」（`odoo.schema_scan`）で実行します。保存済み profile ごとに 60 秒に 1 回まで、失敗も計上し再起動でも保持します。概要は信頼できないメタデータと明示し、ラベルの検査と長さ制限、不正な技術名の除外を行います。
- `odoo_schema_fields {model}` は引き続き、許可された 1 モデルの最新メタデータを読みます。ブロック中のモデルは Odoo に接続する前に拒否し、社員から見えないモデルを指すリレーションは省略します。組み込みの読み取りと明示的なフィールド一覧を持つ汎用読み取りは、最新の `fields_get` で存在しない項目を除きます。メタデータ取得失敗や残るフィールドがゼロの場合は読み取りを拒否します。
- 実行インスタンスのロックを保持する gateway が起動時から 30 分ごとにすべての有効な接続を確認します。スナップショットの欠落・打ち切り、サーバーバージョンの変更、インストール済みモジュール名／バージョンの変更、権限情報の欠落があればバックグラウンドでスキャンします。成功または確定した失敗の完了後に、1 時間に 1 回の自動スキャン枠を記録します。中断は枠を消費せず、MCP の会話プロセスはスキャンを起動しません。準備中は `odoo_schema_fields` で個別のモデルを確認できます。バージョンとモジュール一覧が変わらないフィールドだけの変更には手動スキャンが必要です。接続中の社員は `odoo_connect` でフィールドキャッシュを更新できます。

上限は **400 モデル、各 500 フィールド**です。メタデータとフィールド探索の上限は **5 分**、`fields_get` は最大 4 件同時実行です。アカウント権限の確認には別途 30 秒の全体上限を設けます。組み込みモデル、インストール済み業務モジュール（CRM、販売、購買、在庫、会計、プロジェクト、人事、製造、ヘルプデスク、Web）のモデル、残りの名前順で優先します。時間切れは打ち切りと表示し、接続・バージョン・モジュールが同じなら次回は取得済みフィールドを再利用して続行します。モジュール／モデル一覧は各 5,000 件までです。不完全な差分を削除の証拠にはしません。

以下の既存の汎用書き込み・構造照会に関する社員権限と機密モデル制限は Engine モードに適用します。Follow は本人の Odoo 権限を使い、会社の機能スイッチと共通の拒否は維持します。

### `odoo_create` / `odoo_write`：汎用の作成と更新（v1.81.0）

- `odoo_create {model, values}` はレコードを 1 件作成して ID を返し、`odoo_write {model, ids, values}` は 1〜100 件のレコードを更新して（重複 ID は 1 件として扱います）`ok` を返します。どちらも **`odoo:write`** が必要で、効果クラスは `modify` です。`action_rules`、承認リスト、探索レーンは `odoo_crm_create_lead` と同じように適用されます。
- **明示的な許可に加えて、別のオプトインが必要。** 社員に自分の Odoo 設定（`agent.toml [odoo]`）があり、`allowed_models` にモデル名が完全一致で含まれ、`allowed_actions` に `create`／`write`（または `create:<model>`／`write:<model>`）があり、**さらに** `allowed_actions` にテーブルごとのオプトイン `generic_create:<model>`／`generic_write:<model>` がある場合だけ表示・呼び出しできます。オプトインは既定でオフで、全テーブルや `*` の形はありません。**既存の設定は変わりません**：すでに「作成」／「更新」を付けている設定でも、オプトインをオンにするまで汎用ツールは増えません。自分の設定がない、リストが空、`*` のいずれも許可になりません。リストが空なら制限しない他の Odoo ツールより厳格で、他のツールの動作は変わらず、オプトインも無視します。ダッシュボードの 社員 → 編集 → 連携 → Odoo でテーブルを選び、そのテーブルのルールで「作成」または「更新」を付け、隣の「汎用の作成／更新を許可」をオンにします。オンにすると AI はそのテーブルの書き込み可能な任意の項目を設定できます。許可されたテーブルのいずれかが書き込み不可リスト、機能モジュールのスイッチ、ブロックリストも通過する場合にだけツールが一覧に表示されます。
- **汎用書き込みの対象外**（許可・オプトイン・ブロック解除の設定にかかわらず「この機能は提供していません」と返します）：`res.users`、`res.groups`、`res.company`、`res.users.apikeys`（およびその他の `res.users.*`／`res.groups.*`）、すべての `ir.*`（`ir.actions.*`、`ir.ui.*` を含む）、`mail.mail`、`mail.followers`、`mail.message`、`mail.template`、`mail.compose.message`、`discuss.channel`、`mail.channel`（およびその `.*` サブテーブル）。x2many コマンドで到達するテーブルにも同じリストを適用します。
- レコードを変更する前に次の順で確認します：対象外リスト、許可とオプトイン、機能モジュールのスイッチ、ブロックリスト、続いてローカルで `values`（オブジェクト、最大 64 KiB。x2many のコマンドリストは 0 作成・1 更新・4 リンクのみ。2 削除・3 リンク解除・5 全解除・6 設定は拒否。入れ子は最大 3 階層。`active` は `true` にのみ設定でき、アーカイブは拒否）、接続後にスキーマスナップショットのアカウント権限（`create`／`write` が許可と確認済みであること。不明も拒否）と `values` の各キー：保存され、読み取り専用でなく、関連フィールド（related／`_inherits`。別のレコードに書き込まれます）でないこと。one2many／many2many の値は空でないコマンドリストである必要があり、`false`、`null`、`[]`、単なる ID リスト（セットを全解除または置き換えます）は拒否します。コマンドが到達するすべてのテーブルは同じ対象外リスト、機能モジュールのスイッチ、ブロックリストを通過する必要があります。コマンド 0 と 1（および one2many の 4。レコードを元の親から移動します）にはそのテーブル自身の許可、オプトイン、アカウント権限も必要で、入れ子の値はそのテーブルのスナップショットで確認します。リンク（many2many の 4）にはそのテーブル自身の許可は不要です。この接続のスナップショットがまだない場合は、バックグラウンドの構造準備の完了を待つよう返します。v1.81.0 より前にスキャンしたスナップショットには関連フィールドの記録がないため、再スキャンするまでそのようなフィールドは拒否します。レコードの削除とアーカイブは提供しません。
- 監査ログにはモデル、フィールド名、件数だけを記録し、値は記録しません。失敗した呼び出しはエラーの種類と長さだけを記録し、Odoo のエラー本文は記録しません。

絞り込みのない概要はモデル名と短いラベルだけを返し、上限は 16 KiB です。特定の `model`、または許可されたモデルが最大 5 件の検索に限りフィールドを返し、上限は 32 KiB です。一致したモデルの総数と打ち切りを表示します。共有 Wiki の要約は 16 KiB、`odoo/schema-fields` は検索用の `deep` 参照ページとして 32 KiB に制限し、社員のコンテキストには自動挿入しません。

再スキャンは保存済みの全体接続と、会社範囲を含む各社員の有効な `[odoo]` 設定を対象にします。同じ認証済み接続はまとめて処理し、同時スキャンは最大 2 接続です。画面に社員ごとの準備完了または失敗理由を表示します。異なる Odoo ユーザーのスナップショットは共有しません。2 つの dashboard 操作は永続化された 60 秒の手動クールダウンを共有します。準備成功後、すべての有効な接続を確認できた場合だけ古いデータを削除します。一覧のロックを保持して待機中のロックファイルを削除し、実行中のスキャンとロックは保持します。

## 非推奨エイリアスは引き続き掲載されます

非推奨のツール名は説明に `[deprecated → …]` の接頭辞を付けたまま `tools/list` に残ります。隠せば呼べなくなり、それは非推奨期間の目的と正反対だからです。旧 → 新の完全な対照表は [deprecations.md](../deprecations.md) にあります。現在、非推奨の MCP ツールはありません。v1.66.0 の期間のエイリアスは v1.69.0 で削除されました。

## 関連文書

- [custom-mcp-tool.md](../custom-mcp-tool.md) — ツールの追加
- [mcp-bridge.md](../mcp-bridge.md) — 外部 MCP サーバーのマウント
- [remote-mcp.md](../remote-mcp.md) — HTTP/OAuth トランスポート
- [../../spec/task-packet.md](../../spec/task-packet.md) — TaskPacket 仕様
- [../../spec/reversible-context-ccr.md](../../spec/reversible-context-ccr.md) — `duduclaw_ccr_*` ツール。MCP サーバーではなく直接 API のツールループが注入します

## 認証情報の用途を宣言する

`credential_declare` は `credential_id` と `service` だけを受け取り、社員のメモを拒否します。サービスは `^[a-z0-9][a-z0-9._-]{0,63}$` に一致し、認証情報の検出を通過する必要があります。認証済みでその認証情報を保持する社員だけが、署名付き gateway loopback ルート経由で宣言できます。Admin が設定したサービスを社員が上書きすることはできません。このツールは既存の監査メタデータへの注記であり、秘密の取得、新たな保持者への権限付与、ツール呼び出しへの実値代入は行いません。Admin は「セキュリティ → 認証情報の監査」で確認・修正できます。[認証情報監査](../../features/ja-JP/70-credential-audit.md)を参照してください。


## 外部 MCP の環境変数資格情報

外部 MCP のキーは社員の設定ファイルから移されます。`.mcp.json` には `secret://mcp/<server>/<ENV_NAME>` 参照だけを残し、値は既存の `.keyfile` で暗号化して `<home>/secrets/mcp/<agent_id>.enc` に保存します。外部サーバーのすべての `env` が対象で、例外は `PATH`、`HOME`、`NODE_ENV`、`LOG_LEVEL` と既存の `secret://` 参照だけです。DuDuClaw 自身のサーバー項目と、その `DUDUCLAW_*` 変数は変更しません。

組み込み枠は正確な名前 `duduclaw`、`duduclaw-pro`、現在の instance key だけです。`duduclaw-evil` は第三者として扱います。有効な設定を生成する際は command、args、env 全体をホストの最新 canonical 定義に置き換えるため、鍵の更新や実行ファイル変更でも古い値を使いません。raw `mcp-proxy` 呼び出しは名前にかかわらず拒否し、第三者の proxy 専用環境変数も拒否します。Claude ではプロキシ用途がない場合や組み込みだけの場合、既存の起動準備による修復を保ち、追加フラグを渡しません。Fork のコピー元は社員向け準備を通らない一般プロジェクトの場合があるため、コピーでは gate が無効でも組み込み枠を再生成します。秘密の読み出しには保管先の社員に対する有効な `DUDUCLAW_AGENT_TOKEN` HMAC が必要で、緩い delegation 設定や既定の社員へのフォールバックでは許可しません。プロキシはホストが自分のエントリに入れた `DUDUCLAW_AGENT_TOKEN` で社員を証明します。Claude では起動ごとの MCP 設定（所有者だけが読める一時ファイルで、CLI の終了時に削除）、Codex・Grok・openai-compat では起動時の環境に入ります。社員の `.mcp.json` がプロキシ用にこのトークンを持つことはなく、上流サーバーにも渡りません。

移行は鍵、暗号化バックアップ、保管庫、設定の順に、各段階で親ディレクトリの fsync 完了を待ちます。失敗した社員の移行は中止し、設定を置換した後の失敗では元の内容を復元します。復元も失敗した場合はエラーを報告し、永続化済みの暗号化バックアップを残します。

Proxy は自分の社員に属する参照だけを、上流サーバーの起動直前に解決します。参照が存在しない、不正、または復号できない場合、そのサーバーは起動しません。エラーにはサーバー名と変数名だけを含め、値は含めません。秘密の解決が必要な設定を proxy 用に書き換えられない場合、Claude／Grok は元のファイルを直接読み込む経路へ戻りません。エクスポートでは変数名と空のプレースホルダーだけを残し、インポートする人が値を再入力します。

Gateway の起動時に既存の平文 env を移行します。まず元のファイルを `<home>/backups/mcp-migrate/<agent>-<timestamp>.enc` に暗号化して保存し、資格情報ストアへの書き込みと読み戻し検証を終えてから、設定をアトミックに置き換えます。ある社員の移行が失敗した場合は処理を中止し、ほかの社員を処理します。設定の復元も失敗した場合の回復方法は上記のとおりです。クリーンアップ後の移行バックアップは社員ごとに最大三つです。I/O エラーで削除できなかった場合は記録し、次回の走査で再試行します。サーバーや社員の削除時にはストアの該当項目を削除します。削除した社員を復元した場合はキーの再入力が必要です。

Read ガードは `<home>/secrets` への読み取りを拒否します。これは Bash に対する隔離ではありません。同じ OS ユーザーで動く社員が Bash を持つ場合、`.keyfile` を含む gateway のファイルを読めます。実際の隔離には Bash を与えないか、タスクサンドボックスを使う必要があります。対象は `env` のみで、`args`／`url` 内のキーや、キーを返すツール結果は対象外です。既存の平文エクスポートやバックアップを遡って消すことはありません。

## Google Chat 読み取りツール

`google_chat_spaces` は接続したユーザーが参加するスペース、グループチャット、DM を一覧表示します。リソース名、表示名、種類、Google が返す場合は最終活動時刻を含みます。`limit` は既定 20（1–50）、`filter` は `SPACE`、`GROUP_CHAT`、`DIRECT_MESSAGE` のいずれかです。

`google_chat_messages` は厳密に検証した `spaces/...` の最近のメッセージを新しい順に読みます。`limit` は既定 30（1–100）、任意の `since` は RFC3339、`thread` は同じスペース内のスレッドリソース名です。送信者、作成時刻、本文（1件最大 2,000 文字）、スレッド、添付ファイル名を返し、添付はダウンロードしません。表示名がない送信者は `users/...` で示し、People API や追加のメンバー権限は使用しません。応答全体は最大 30,000 文字です。

1. 管理者がダッシュボードの「管理 → 連携 → Google」タブで **Google Chat 読み取り**を有効にします（`[google] chat_read = true`、既定 false）。
2. ユーザーは「自分の接続」から自分の Google アカウントを再接続し、Chat の読み取り権限を両方選択します。追加スコープは `https://www.googleapis.com/auth/chat.spaces.readonly` と `https://www.googleapis.com/auth/chat.messages.readonly`。無効時は既存ユーザーに追加同意を求めません。
3. OAuth クライアントの Google Cloud プロジェクトで **Google Chat API** を有効化し、**Chat アプリ**を構成します。ユーザー認証でも必要です。Workspace 管理者によるアプリ許可が必要な場合もあります。

Gmail 読み取りと同じ社員・アカウントの紐付け、スコープ、承認、私有データの送信制御を使用します。既存の身分ポリシーが選択したアカウントを使い、任意のユーザー指定は受け付けません。Apps Script はこの Chat 読み取りに対応しません。DM とメンションを受信する Chat ボットとは別の機能です。無効化すると既存トークンでの新たな読み取りも拒否しますが、Google 側の同意を取り消すものではありません。

応答は信頼しない参照データとして包み、グループ履歴と同じ注入検査を行います。1件でも検出すると応答全体を `not available` として拒否します。監査にはスペース ID と件数のみを記録し、本文は残しません。固定データ・ローカルテストは実アカウントでの成功を保証しないため、納品前に実際の同意・読み取りを検証してください。

## `mail_send` — 依頼者による元の会話でのメール確認

`mail_send` は既存の宛先・データ送信ガードを使用します。確認済みで現在もそのスタッフへの Operator 権限を持つ依頼者は、元の会話で完全な宛先、件名、本文要約を確認できます。対象外の場合はダッシュボードの Admin が決定します。一回限りの承認は最終引数に結び付き、期限切れは拒否します。`[security] mail_requester_approval = false` で Admin のみに戻せます。既定値は `true` ですが、設定の読み取り失敗や不正な形式では例外を拒否します。[資格と制限](../../features/ja-JP/05-security-defense.md#依頼者による元の会話でのメール確認)を参照してください。 対象はチャネルが証明した一対一の会話または認証済みダッシュボード WebChat のみです。グループ会話と種類不明の会話には引き続きダッシュボード Admin の承認が必要で、公開 WebChat ウィジェットの訪問者は対象外です。対象外のチャネル通知に宛先、件名、本文は含めず、完全な内容はダッシュボードにのみ表示します。

この確認は外部送信のゲートのみを承認します。ネイティブのメールツールは引き続き既存の送信トレイの下書き確認を作成します。このゲートの承認はメール送信完了の証明ではありません。

### MCP のスケジュールとリマインダーの通知先

`tasks_create` に `schedule` を指定する場合、`notify_channel` はチャネル種別、`notify_chat_id` はチャット・チャネル・ルームの ID です。`create_reminder` では `channel`（例：`telegram`、`line`）と `chat_id` を使用します。検証済みの作成元へ返すには、組の**両方を省略**してください。同じ社員が実行する認証済み WebChat のターンも対象です。チャネルに `webchat` を直接指定してはいけません。その会話を選べるのは gateway だけです。単発実行には、検証済みの作成元か明示的な組による通知先が必要です。

Google Chat の社員専用アプリのセクションは共通アプリより優先されます。そのセクションが不完全または読み取り不能なら事前確認で拒否します。`bot_token` だけでは Chat アプリの設定とみなされず、共通認証情報にも切り替わりません。

対応するチャネルの会話から定期実行（`tasks_create` の cron `schedule`）、単発実行、またはリマインダー（`create_reminder`）を作成するとき、実行する AI 社員が作成した社員と同じで、会話の相手がそのターンで gateway が確認した紐付け済みアカウントであり、通知先の両方の項目を省略した場合、結果は**作成元の会話**に戻ります。会話は gateway が進行中のターンから解決して記録し、MCP ツールには AI 社員が会話を選ぶパラメーターがないため、Bash を持たない AI 社員はスケジュールの既定の通知先を別の会話に向けられません。Bash を付与された AI 社員は gateway と同じ OS ユーザーで動作し、gateway のローカルデータベースを読み書きできるため、その AI 社員に対しては減速帯であって隔離ではありません。現在の対策は、Bash が不要な AI 社員に Bash を付与しないこと、タスク sandbox、またはネイティブ Linux の従業員隔離（まだ提供されていません）です。[これらのガードがカバーしないもの](../../features/ja-JP/05-security-defense.md#これらのガードがカバーしないもの)を参照してください。Google Chat、Telegram、LINE、Discord、Slack、WhatsApp、Feishu、Microsoft Teams、WeCom、DingTalk に対応します。WebChat は従来どおり会話履歴に保存され、会話を開き直すと確認できます。

スケジュールの `notify_channel` / `notify_chat_id`、リマインダーの `channel` / `chat_id` を指定すると、その通知先が優先されます。作成元を確認できない場合、定期実行は実行記録のみを残し、単発実行とリマインダーには通知先が必要です。`tasks_create kind=goal` で作成した目標も、同じ条件で進捗と人の対応が必要な通知を作成元の会話へ送ります。作成元の会話がない目標は社員の通知設定を使用します。

作成元の会話への通知は、送信のたびに送信時点で再確認されます（通知を控える時間帯や人による引き継ぎで保留された通知も、実際に送るときに再確認します）。作成者のアカウントが有効でこの AI 社員を引き続き利用できること、チャネル ID の紐付けが残っていること、会話が消去・再作成されていないことを確認し、いずれかが満たされないか読み取れない場合は送信せず、別の会話へ送り直すこともありません。目標は Activity Feed の記録のみ残ります。公開訪問者の会話（Web サイトのウィジェット、Live Chat）はこれらの通知を受け取りません。アップグレード前に WebChat で作成されたスケジュールとリマインダーは、アップグレード後も元の会話に戻ります。gateway は、アップグレード後にこの確認を最後まで完了できた最初の起動で、各件を一度だけ確認します。完了できなかった起動は完了として記録されず、次回の起動で再試行されます。確認する内容は次のとおりです（作成者のアカウントが有効で AI 社員を引き続き利用でき、会話が作成者の所有で消去されていないこと）。確認後は新規作成分と同様に送信前に毎回再確認し、確認できないものは `origin_unproven` で拒否して、別の会話へ転送しません。保留中の目標の判断依頼は、目標が削除されていれば破棄されます。

Google Chat は既存の選択順序に従い、社員専用の Chat アプリ、スペースに記録されたアプリ、または共通アプリを使用します。選ばれたアプリには、そのスペースへの送信権限が必要です。受信者保護は継続され、保存済みの検証された会話は作成元として扱います。別の通知先を指定しても、この扱いは適用されません。ユーザーに紐付けられていない送信者は、スケジュールを作成しただけでは検証済みの作成元として扱われません。

定期実行の一覧と詳細は、実行結果 `last_status` と通知結果 `last_delivery_status` を別々に返します。通知結果は `delivered`、`delivery_failed`、`delivery_refused`（上記の送信時確認で拒否）、または未送信の空値です。失敗時の `last_delivery_reason` は固定コード `notification_delivery_failed`、拒否時は `origin_unproven`、`origin_proof_invalid`、`origin_public_visitor`、`origin_state_unreadable`、`origin_account_inactive`、`origin_access_revoked`、`origin_identity_unbound`、`origin_conversation_changed` のいずれかです。これらは直近の実行についての情報です。リマインダーは従来の `delivered` / `failed` を使い、一覧に `error` も表示します（拒否されたリマインダーは `failed` で、`error` に同じコードが入ります）。送信失敗を配信済みとして記録することはありません。

### 毎回確認する Gmail 送信と Calendar 削除

`gmail_send_draft {"draft_id":"r…"}` と `calendar_delete_event {"event_id":"…","calendar_id":"primary","notify_attendees":false}` は `google:write` と gateway のネイティブ REST 実行が必要です。Apps Script は両ツールを非表示にし、直接呼び出しも拒否します。bearer だけを渡す従来の Home 経路も非表示・拒否となります。社員専用 Google 接続か、gateway のアカウントに紐づくコネクターを使用してください。

呼び出すたびに新しい確認カードが必要です。`allow`、Admin 権限、永続ワークフロー権限でも省略できず、`block` は引き続き拒否します。送信カードは Google から取得した送信元、To／Cc／Bcc、件名、本文プレビュー、添付ファイル名とサイズを表示します。承認はアカウント、社員の世代、下書き ID、操作、MIME ハッシュに紐づきます。gateway は再確認して承認済み MIME を送信し、内容やアカウントの変更時は再承認が必要です。宛先ポリシーは全 To／Cc／Bcc を確認し、曖昧なアドレス構文を拒否します。

削除カードは予定名、タイムゾーン付き開始・終了、参加者、カレンダー、繰り返し全体の削除、通知設定を表示します。承認したバージョンを `If-Match` で確認し、変更時は再承認を要求します。`calendar_id` の既定は `primary`、通知は明示的に `sendUpdates=none` です。`notify_attendees=true` のときだけ `all` を使用します。空の HTTP 204 は成功です。削除時は既存の `calendar.calendarlist.readonly`（または上位の Calendar スコープ）で、プライマリカレンダーのアカウントと選択したカレンダーの名前・タイムゾーンも取得します。

MIME 全体は gateway のメモリに保持し、承認には必要なプレビューとハッシュを保存します。拒否・期限切れ・使用済み・不一致の承認では変更しません。書き込みのタイムアウトや結果不明時は `result_unconfirmed` を返します。自動再送は行わず、再試行の前に Google で結果を確認してください。確認期限は 90 秒です。

通信契約は [Gmail 下書き](https://developers.google.com/workspace/gmail/api/guides/drafts)と [Calendar の条件付き変更](https://developers.google.com/workspace/calendar/api/guides/version-resources)に従います。

## 本人として Google Chat を送信する

`google_chat_send {space, text, thread?}` は、依頼した認証済みメンバー自身の Google アカウントで既存スペースにテキストを送信します。`google:write` が必要で、管理者が `[google] chat_send = true` を有効にするまでは使用できません。共有アカウント、他のメンバー、Apps Script、Chat アプリの `chat.bot` トークンへの切り替えは行いません。

個人 OAuth では、先にアカウントを接続し、「自分の接続 → Google Chat の送信」で独立した `chat_send` 用途を許可します。追加同意では、同意済みの通常サービスと `https://www.googleapis.com/auth/chat.messages.create` を明示的に要求します。以前の Google 認可の自動統合は要求しません。Google の `sub` が以前と同じで、実際にスコープが付与されている必要があります。ドメイン全体の委任でも送信用スコープを別に要求し、Workspace 管理者の許可が必要です。他の Google ツールの要求には追加しません。

毎回本人の確認が必要です。確認は５分間有効で１回限りです。カードにはアカウント、スペースのリソース名、スレッド、送信する全文と個人情報の送信に関する注意が表示されます。短文は認証済みの個別チャットで確認でき、グループからの依頼や長文は本人だけが開ける「自分の接続」ページで確認します。管理者の承認、`allow` ルール、Admin MCP scope、宛先許可リストでは代替できません。組織の外部送信承認も別途適用されます。本文、宛先、アカウント、認証情報、ポリシーが変わると再確認が必要です。

Gateway は操作ごとに固定の request ID を保存します。結果が不明な場合は `result_unconfirmed` を返し、自動再送しません。新たに送信を依頼する前に Google Chat を確認してください。添付ファイルやスペースの作成には対応していません。指定スレッドは同じスペース内のものに限ります。Google では本人が送信者として表示され、アプリ名も付きます。[Google Chat API](https://developers.google.com/workspace/chat/api/reference/rest/v1/spaces.messages/create)。

## 管理者 AI の設定

`admin_settings_read` は管理者 AI 機能で公開される日常設定を読み取ります。`admin_settings_propose` は確認カードを作成するだけで、変更を直接適用しません。利用資格のある管理者本人が、自分専用の AI アバターを通じて参加している会話でのみ利用できます。変更提案はプライベートチャットまたはサインイン済み WebChat に限られます。グループでの読み取りではスケジュールのプロンプト本文とスキル説明を省略します。機能を有効にするには、管理者がリスク説明を読み確認する必要があります。1 件の提案でも、提案者本人が 10 分以内にダッシュボード受信箱で確認します。

通常のプロフィール、モデル、実行上限、定期起動、プロンプトのみのスケジュール、導入済みスキルの有効／無効を変更できます。権限、チャネルと認証情報、隔離、実行エンジン、予算引き上げ、組織と委任権限、外部ツール接続、人格と業務契約、従業員削除、外部スキルの導入は引き続きダッシュボード限定です。

AI 呼び出し元が `agent_update` を使う場合、予算は引き下げのみ可能です。予算引き上げやその他の保護対象のセキュリティ設定変更は拒否されます。データベースソースの追加は、管理者だけがダッシュボードで判断できる承認カードを作成します。削除は権限縮小として引き続き可能です。これらの制限は `open` を含むすべての委任ポリシーで適用されます。

## クラウドドライブへの書き込みツール

この九つのネイティブツールには `google:write` が必要です。管理者は作成、変更、移動、ゴミ箱への移動を個別に許可します。四項目とも既定はオフです。有効化や制限の緩和にはリスクへの明示的な同意が必要で、管理者と時刻を記録します。この設定変更に追加の再ログインは不要です。ダッシュボードの操作機能は本リリースの設定ページとともに提供予定です。Google 連携と個人の書き込み接続のブロックは、現時点では未提供です。

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

## 本人用ツールと Odoo 権限（v1.86.0）

個人キーは次のツールのみを提供し、キーのスコープと管理者の現在の `allow_mcp_scopes` の共通部分を適用します。

| スコープ | ツール |
| --- | --- |
| `tasks:read` | `my_tasks_list {status?, limit?}`：参照可能なタスク。既定 50 件、範囲 1–200。上限や省略は `truncated` で示します。 |
| `agents:status` | `my_agents_status {}`：参照可能な社員の ID、名前、状態、`can_call`。 |
| `odoo:read` | `odoo_search`、`odoo_crm_leads`、`odoo_sale_orders`、`odoo_inventory_products`、`odoo_inventory_check`、`odoo_invoice_list`、`odoo_payment_status`、`odoo_partner_search`、`odoo_schema_overview`、`odoo_schema_fields`、`odoo_status`。 |
| `odoo:write` | `odoo_create`、`odoo_write`、`odoo_activity_create`、`odoo_note_post`。確認済みの `follow_odoo_account` が必要です。 |

所有者本人の現在の権限で実行します。Odoo は本人の接続済みアカウントのみを使い、社員・会社の認証情報を借用しません。`odoo_execute`、`odoo_report`、`odoo_connect`、`odoo_message_send`、`odoo_attachment_add` と CRM／販売の専用変更ツールは対象外です。2 つの `my_*` ツールは個人キー専用で、社員用ツールではありません。

### Odoo の専用書き込みツール 4 種

すべて `odoo:write` と既存の読み取り可能な対象レコードが必要です。テキストはプレーンテキストとして HTML エスケープし、メンションや任意の宛先リストは受け付けません。

| ツール／引数 | 動作 |
| --- | --- |
| `odoo_activity_create {model, res_id, summary, note?, date_deadline?, activity_type_xmlid?, user_id?}` | 活動を作成。各テキストは最大 10,000 文字、期限は `YYYY-MM-DD`、既定の種類は `mail.mail_activity_data_todo`。Engine は接続アカウントに割り当て、`user_id` は Follow のみ受け付けます。 |
| `odoo_note_post {model, res_id, body}` | 内部メモを追加。本文は最大 10,000 文字。 |
| `odoo_message_send {model, res_id, body}` | レコードのフォロワーに通知。本文は最大 10,000 文字。内容に結び付いた人の承認が毎回必要で、社員の認証主体が必要です。 |
| `odoo_attachment_add {model, res_id, file, name?}` | 呼び出し元社員自身の添付フォルダーから非公開添付を作成。インライン／base64、絶対パス、親への移動、シンボリックリンク、共有フォルダーは不可。内容と形式が一致する PDF、PNG、JPEG、WebP、UTF-8 TXT／CSV、OOXML XLSX／DOCX／PPTX のみ。元ファイルと再エンコード後の両方に `attachment_max_mb`（既定 10 MiB、範囲 1–25）を適用します。社員の認証主体が必要です。 |

Engine は `allowed_models` の正確な対象と `allowed_actions` の `activity:<model>`、`note:<model>`、`message:<model>`、`attach:<model>` を必要とし、空リストでは許可しません。Follow は社員のモデル／操作権限と Engine の機密モデルブロックリストに代えて本人の Odoo 権限を使用します。会社の機能スイッチ、`always_refuse`、固定拒否、入力確認、承認、Odoo ACL は残ります。会社／AI 自身のアカウントは Engine のままです。社員の `permission_override=engine` は Engine を維持し、`read_only` は書き込みを除きます。有効な管理者確認がないグローバル Follow は Engine に戻ります。

`always_refuse` の既定値：`unlink:*`, `create:ir.*`, `write:ir.*`, `execute:ir.*`, `create:res.users`, `write:res.users`, `create:res.groups`, `write:res.groups`, `method:execute_import`, `method:signal_workflow`。モデル完全一致、`*`、`ir.*` のようなドット区切りの子モデル、正確なメソッド名の `method:<name>` を使用します。専用操作は対象モデルと `activity:`、`note:`、`message:`、`attach:` で照合します。

| ルール | 常に拒否する操作 |
| --- | --- |
| F1 | `ir.attachment` の更新・削除、汎用ツールによる作成。ファイルからの作成には `odoo_attachment_add` を使います。 |
| F2 | `ir.model`、すべての `ir.model.*` 子モデル、`ir.rule` の変更。 |
| F3 | `res.users.apikeys` とドット区切りの子モデルの変更。 |
| F4 | `mail` とすべての `mail.*` 子モデルの変更。汎用ツールでの `message_post`、`message_notify`、`message_subscribe`、`send_mail`、`action_send_mail`。メモ・メッセージには専用ツールを使います。 |
| F5 | `_` で始まるメソッド、および `init`、`uninstall_hook`。 |

変更を行う `odoo_execute` とすべての `action_*`、`button_*`、`toggle_*` は、全モードで人の承認が必要です。`odoo_message_send` も常に承認が必要です。Allow ルールで承認や拒否を回避できません。承認はツールと正確な引数に結び付き、一度だけ使えます。ほかの書き込みも既存の承認・データ送信チェックを受けます。管理者は監査付き `odoo.access_get/set` で管理し、Follow・会社アカウント一覧・拒否リストの権限拡大には `acknowledge_risk` が必要です。
