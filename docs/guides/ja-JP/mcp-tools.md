# MCP ツール: エージェントに何が見え、なぜそうなるのか

DuDuClaw の MCP サーバーは標準の `tools/list` でツールを宣言します。このページでは、誤解されやすい 2 点を説明します。

1. なぜエージェントに見えるツールが、サーバーが実装しているものより少ないのか。
2. ツール説明に字数上限が入った今、長い詳細はどこへ行ったのか。

ツールを追加する場合は先に [custom-mcp-tool.md](../custom-mcp-tool.md) を読んでください。このページは実装ではなく宣言面の話です。gateway なしで MCP サーバーだけを使う場合は [mcp-standalone.md](mcp-standalone.md) を参照してください。

## ルール: 見えること ⇔ 呼べること

`tools/list` は、呼び出し元が**その時点で実際に呼べる**ツールだけを列挙します。以下のフィルタはいずれもディスパッチャが既に強制しているゲートを写したものなので、エージェントが読むリストとサーバーが受け付ける呼び出しは常に同じ集合を指します。

これは正しさの問題であると同時にコストの問題でもあります。ツールスキーマは**起動のたびに支払う固定のプロンプトコスト**です。CLI はセッションごとに `tools/list` を一度読み、その全体が最初のユーザートークンより前にモデルのコンテキストに入ります。ゲートが拒否するツールのスキーマは二重の損です。トークンを払い、さらにモデルが使えないツールを前提に計画を立てます。

ツールを隠すことは**認可の判断ではありません**。未掲載のツールを呼んでも本物のゲートに到達し、ゲート自身のメッセージで拒否されます。

### フィルタの一覧

| フィルタ | 参照元 | オフ / 空のときの効果 |
|---|---|---|
| 外部クライアント許可リスト | `principal.is_external` | 従来の許可リスト 7 ツールと、明示的に付与した scope のツールのみ。続けて次の行の scope 規則が適用されます |
| 社員でない呼び出し元の scope | `principal.scopes`。`admin` を持たず AI 社員でもないキー（gateway 内部キーでも、社員ごとのキーでも、gateway が社員用に起動したプロセスでもない）に適用 | キーが最低 scope を持つツールだけを掲載。プロセス自身のエージェントとして動くツール（`working_state_*`、`canvas_*`、`shared_wiki_delete`、`memory_search_by_layer` と三つの統合状態の参照など）は掲載せず、すべての外部キーにも掲載しません。呼び出しはディスパッチゲートが同じ規則で拒否します（`process_agent_tool`）。[mcp-standalone.md](mcp-standalone.md) を参照 |
| Google Workspace | `config.toml [integrations] google_workspace` | Google 系 19 ツールを非表示 |
| Google の身元（v1.76.0） | `agent.toml [google] identity`、なければ `config.toml [integrations.google_identity] default_identity` | `none` なら同じ 19 ツールを非表示にし、呼び出しも拒否。それ以外の値では一覧に残り、使えない呼び出しは理由を返します。`google_status` は最初に社員が誰として動いているかを示します |
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
- **自分への `agent_update`**：AI 社員は自分について `reports_to`、`db_sources`、`db_sources_add`、`db_sources_remove`、`budget_cents`、`role` を送れません（監査 `agent_authority_refused`）。部下の編集は従来どおりです。

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

メモリ・wiki・ユーザープロファイル・`code_map` のツールは `openWorldHint: false` も付きます。`memory_alias_add`・`wiki_export`・`wiki_rebuild_fts` は `idempotentHint: true` です。これらのリストは `crates/duduclaw-cli/src/mcp/tools_def/annotations.rs` にあります。

ヒントは MCP クライアント向け（呼び出し前に確認するかどうかなど）で、権限を与えるものではありません。DuDuClaw のゲートは毎回そのまま動きます。Claude Code と Codex はモデルに名前・説明・入力スキーマだけを渡すため、上の説明バイト予算には含めません。

## 説明から移した長い詳細

### `tasks_create`／`tasks_update` — ゴール費用上限

`tasks_create` は `kind="goal"` のとき `cost_cap_cents` を受け付けます。米セント単位の正の整数（最大 `9223372036854775807`）です。省略すると `[goal_loop] default_cost_cap_cents`（`0` は上限なし）を使います。通常タスクと予約のリクエストでは拒否されます。例：`{"kind":"goal","title":"報告書を確認","assigned_to":"agnes","cost_cap_cents":500}`。ゴールの説明や受け入れ基準は通常どおり記入します。

`tasks_update` はゴールに対して同じフィールドを受け付けます。社員は上限の追加・引き下げだけが可能で、有効な上限の引き上げや削除はできません。引き上げは操作者が行います。上限の保存だけでは再開しません。ダッシュボードの「上限を引き上げて再試行」は保存後に既存の再試行決定を使います。タスクツリーの測定費用が上限以上なら `budget_exhausted` で停止し、費用を読み取れない場合はディスパッチを見送ります。[ゴールループ](../../features/ja-JP/34-goal-loop.md)を参照してください。

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

`field` は `gvu_enabled`（真偽値）、`max_silence_hours`、`skill_token_budget`、`max_active_skills`（数値）を受け付けます。旧 `stagnation_*` フィールドはこのツールでは変更できません。[evolution-switches.md](../evolution-switches.md) を参照してください。

AI スタッフは、自分または部下の設定を変更できます。`agent_update` と同じ身元・組織権限チェックを使い、委任ポリシーが `open` の場合はチームをまたぐ変更も許可されます。人間のオペレーターはこの制限を受けません。身元または組織権限のチェックを通過しない場合は、対象の設定ファイルを読む前に拒否します。これらのフィールドはツール・データ・組織の権限を付与せず、進化のマスタースイッチも迂回しないため、自分の設定変更は引き続き可能です。

### `execute_program` — スクリプトの実行場所

スクリプトはスクリプトサンドボックスで実行されます。`config.toml [container.sandbox] image` のイメージから作るコンテナで、[タスクサンドボックス](task-sandbox.md)と同じイメージです（自動ではダウンロードされません）。コンテナはホストのユーザーで動きます（ホストのプロセスが root のときは `1000:1000`、WSL2 では常にこちら）。すべての capability を破棄し、`no-new-privileges`、読み取り専用のルートファイルシステム、ネットワークなし、2 GiB のメモリ（swap なし）、256 プロセス、1 CPU、小さな `/tmp` tmpfs が適用されます。マウントされるのはスクリプトを置いた専用ディレクトリだけで、`/workspace` に読み取り専用でマウントされます。`timeout_seconds`（既定 30、最大 300）に加えて 600 秒の絶対上限があり、stdout と stderr は 1 つの出力として返ります（読み取り上限 2 MiB、返信上限 1 MiB）。呼び出しがキャンセルされるとコンテナは強制削除されます。macOS と Linux では Docker を使い、Windows ではまず WSL2、次に Docker を試します。

サンドボックスが使えないとき（Docker がない、イメージがない、`[container.sandbox]` が無効など）、スクリプトは**実行されません**。ツールは `docker pull <image>` のコマンドを含む `Script sandbox unavailable (<コード>): …` を返し、監査イベント `script_sandbox_unavailable` を書きます。以前のバージョンでは、この場合に黙ってホスト上で実行していました。その動作に戻すには `[container.sandbox] script_when_unavailable = "run_unsandboxed"` を設定します（タスクサンドボックスの `when_unavailable` とは別のキーです）。ホストでの実行は毎回 `script_sandbox_bypassed` として監査されます。

スクリプトからプラットフォームのツールを呼び返すことはできません。コンテナ内に RPC ソケットはありません。

### `computer_*` — gateway が実行するセッション

8 つのツールは、スタッフごとに 1 つの Computer Use セッションを駆動します。仮想ディスプレイとキオスクブラウザを持つ、隔離されたコンテナです。MCP サーバーは各呼び出しを loopback 経由で gateway に転送するだけで（`POST /api/internal/computer-use`、リクエストごとに署名）、コンテナを所有してすべてのチェックを行うのは gateway なので、gateway が動いている必要があります。`agent.toml [capabilities] computer_use = true` でない限り非表示です。

| ツール | パラメータ | 備考 |
|---|---|---|
| `computer_session_start` | `task` 文字列、任意。`width` 整数 320–1920。`height` 整数 240–1200 | スタッフごとにセッションは 1 つ。結果には、上限、高リスクのアクションをチャットで確認できるか、`computer_navigate` が開けるサイトが載る |
| `computer_screenshot` | なし | MCP の画像ブロック（PNG、マスク済み）に続いて、使用済みアクション数と残り時間のテキストブロック。全体がマスクされた画像は、その旨が理由（複数のウィンドウ、機密性のある、または読み取れない前面ウィンドウ、検出の失敗）と次の手順とともにテキストで報告される |
| `computer_click` | `x`、`y` 整数（必須）。`button` 文字列 `left`/`right`。`double` 真偽値 | `double` は左ボタンのみ |
| `computer_type` | `text` 文字列（必須）、1–2,000 文字 | 監査には文字数のみ記録される |
| `computer_key` | `key` 文字列（必須）：英字、数字、`+`、`-`、`_` | 例：`Return`、`ctrl+s` |
| `computer_scroll` | `x`、`y` 整数（必須）。`direction` 文字列 `up`/`down`（既定 `down`）。`amount` 整数 1–20（既定 3） | |
| `computer_navigate` | `url` 文字列（必須） | `https://` のみ。ホストはスタッフの `[capabilities.computer_use_config] allowed_domains` に完全一致し、かつセッション開始時に解決できたもの。ポートはないか 443、ユーザー名・パスワードなし、2,000 バイト以下。許可リストがなければセッションにネットワークはなく、呼び出しは拒否される |
| `computer_session_stop` | `session_id` 文字列、任意 | コンテナを削除する |

整数と真偽値のパラメータは、数値の文字列と `"true"`/`"false"` の文字列も受け付けます。クリック、入力、キー、スクロール、ナビゲートはそれぞれ 1 アクションとして `max_actions`（既定 50）に数えられます。上限、承認と確認のルール、ネットワーク許可リストとその残存リスクは[ブラウザ自動化](../../features/ja-JP/08-browser-automation.md)にあります。

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

### `responsibility_get` / `responsibility_followup` / `responsibility_ask` — 継続タスクのツール

[継続タスク](continuous-responsibilities.md)用の 3 つのツールです。`config.toml [responsibilities] enabled` が true で、呼び出し元が AI従業員の身元であるときだけ掲載されます（外部クライアントには見えません）。機能がオフの間はすべての呼び出しが拒否されます。`tasks_*` と同じく Admin スコープが必要で、ゲートウェイ自身のキーはこのスコープを持っています。

| ツール | 呼べる人 | ルール |
|---|---|---|
| `responsibility_get` | どの AI従業員も可。運用者キーはどの継続タスクも読める | `responsibility_id` なし：呼び出し元自身のものを一覧。id あり：概要（スケジュール、購読、今期の費用と回数、実行中の 1 回、`cost_not_counted`）。他の従業員の継続タスクは、委譲ポリシー上で呼び出し元と持ち主に関係がない限り「見つからない」になる |
| `responsibility_followup` | その継続タスクを持つ AI従業員。自分の実行が開いていて、継続タスクが active のときだけ | `due_at`（RFC 3339）に 1 回限りの起床を予約。現在から `min_wake_interval_secs` 以上先、`stop_at` 以前。今期の回数上限に数えられ、使い切っていれば拒否。黙って別の時刻にずらすことはない |
| `responsibility_ask` | `responsibility_followup` と同じ | 質問 1 つ（最大 1000 文字）、選択肢は最大 5 つ（各 1000 文字まで）。`ttl_secs` は既定 1 日で、60 秒から `stop_at` までに収める。質問はインジェクション検査を受け、担当者には 200 文字に切った引用として見せ、該当があれば警告を付ける。プッシュは継続タスクの通知条件と期間ごとの上限に従う。回答は次の実行にデータとして渡され、権限は与えない。その実行を停止すると質問も取り下げられる |

1 つの継続タスクで AI従業員が予約した待機中の起床は同時に 1 つだけなので、待機中の `responsibility_followup` と `responsibility_ask` は両立しません。運用者キーはこの 2 つの起床ツールを使えません（従業員になりすますことになるため）。固定のエラーコード：`not_found`、`not_active`、`no_open_occurrence`、`invalid_due_at`、`period_occurrence_limit`、`agent_followup_limit`、`epoch_changed`、`invalid_question`。

継続タスクを作成・変更・再開・再有効化するツールはなく、指示を送るツールもありません。

### `knowledge_*`：ドキュメントナレッジベース

`knowledge_search`、`knowledge_get_passage`、`knowledge_list_collections` は[ドキュメントナレッジベース](../../features/ja-JP/63-knowledge-base.md)を読みます。スコープは `knowledge:read` で、外部 MCP キーには付与できません。呼び出しはすべて署名付きローカルルート `POST /api/internal/knowledge` でゲートウェイに渡され、ゲートウェイが読めるコレクション（社員本人と部門への付与を `agent.toml [knowledge] collections` で絞り込み、ターンの相手と機密度ルールとの共通部分）を決め、アクセスログを 1 行書きます。ゲートウェイが動いていないときはその旨を返し、データベースを直接読むことはありません。

結果は文書の内容であることを明示します。各段落（およびコレクション名と説明）は印の付いたブロックに入り、各行は `│ ` で始まります。タイトルと位置は 1 行にまとめ `[` `]` を置き換えるため、文書が結果の見出しを偽造したりブロックを閉じたりできません。`knowledge_search` は段落を `[n] タイトル · 位置（コレクション） chunk_id=…` と番号付けし、返信で引用できます。取り込み時に指示らしい文と判定された段落には警告行が付きます。`collections` は絞り込みのみです。`knowledge_get_passage` は毎回権限を再確認し、存在しない段落と権限のない段落に同じ応答（`permission denied`）を返します。チャネルや WebChat の返信中は、番号がそのターンの引用番号の続きになります（すでにプロンプトに入った段落は元の番号のまま）。そのため回答の `[n]` は読み手が開く段落と一致します。

`knowledge_add_source`（スコープ `knowledge:write`、こちらも外部キーには付与不可）は、社員側のコレクションに文書を提案します。`collection` と、`path`（社員自身の `attachments/` 内のファイル）・`url`・`text` のいずれか 1 つ、任意の `title` を渡します。ツール自体は何も追加せず、`knowledge_add_source` 承認カードを作成してその ID を返します。カードはダッシュボードで管理者またはそのコレクションの編集者だけが決定でき、公開訪問者・未紐付けの送信者・未登録のターンでは拒否し、その社員の保留中カードが多すぎる場合も拒否します。どの呼び出し元にも免除はありません。

`knowledge_search` と `knowledge_list_collections` は、応答の先頭に検索モード（ハイブリッド＝キーワードと意味ベクトル、またはキーワードのみ）を示し、ベクトルを使わなかった場合はその理由（モデル未インストール、クラウド提供元が未承認、匿名化が無効、クラウドのエラーなど）を添えます。`knowledge_list_collections` は各コレクションのベクトル作成率も返します。ベクトルが段落を並べ替えるのは、その呼び出しが読めるコレクションの中だけです。

### `odoo_schema_overview`：保存済みの Odoo 構造

- `odoo_schema_overview` は **`odoo:read`** が必要です。引数なしではスキャン済みのモデル、ラベル、カスタムフィールドの印を一覧表示します。`model` で 1 モデルのフィールドを取得し、`query` でモデル／フィールド名とラベルを検索できます。社員の Odoo モデル／アクション設定と機能モジュールのスイッチで結果を絞ります。構造を見られても、既定でブロックされた業務レコードへのアクセスは許可されません。
- 構造スキャンは dashboard の管理者が「Odoo の構造を再スキャン」（`odoo.schema_scan`）で実行します。保存済み profile ごとに 60 秒に 1 回まで、失敗も計上し再起動でも保持します。概要は信頼できないメタデータと明示し、ラベルの検査と長さ制限、不正な技術名の除外を行います。
- `odoo_schema_fields {model}` は引き続き、許可された 1 モデルの最新メタデータを読みます。組み込みの読み取りと明示的なフィールド一覧を持つ汎用読み取りは、最新の `fields_get` で存在しない項目を除きます。メタデータ取得失敗や残るフィールドがゼロの場合は読み取りを拒否します。
- `odoo_status` にスナップショットの経過時間、取得時のサーバーバージョン、直近のスキャン失敗を表示します。バージョンが異なると再スキャンを勧めます。接続成功後、スナップショットがない、またはバージョンが変わった場合はバックグラウンドスキャンを開始し、自動試行は有効な profile ごとに 1 時間に 1 回までです。同じバージョンでのカスタマイズは手動で再スキャンしてください。

上限は 400 モデル、各 500 フィールド、90 秒です。モジュール・モデル探索一覧はそれぞれ 5,000 件までです。打ち切りを明示し、不完全な差分で削除済みとは判断しません。保存先と権限の詳細は [Odoo の構造スキャンと互換性](../../features/ja-JP/12-industry-templates.md#構造スキャンとフィールドの互換性)を参照してください。

## 非推奨エイリアスは引き続き掲載されます

非推奨のツール名は説明に `[deprecated → …]` の接頭辞を付けたまま `tools/list` に残ります。隠せば呼べなくなり、それは非推奨期間の目的と正反対だからです。旧 → 新の完全な対照表は [deprecations.md](../deprecations.md) にあります。現在、非推奨の MCP ツールはありません。v1.66.0 の期間のエイリアスは v1.69.0 で削除されました。

## 関連文書

- [custom-mcp-tool.md](../custom-mcp-tool.md) — ツールの追加
- [mcp-bridge.md](../mcp-bridge.md) — 外部 MCP サーバーのマウント
- [remote-mcp.md](../remote-mcp.md) — HTTP/OAuth トランスポート
- [../../spec/task-packet.md](../../spec/task-packet.md) — TaskPacket 仕様
- [../../spec/reversible-context-ccr.md](../../spec/reversible-context-ccr.md) — `duduclaw_ccr_*` ツール。MCP サーバーではなく直接 API のツールループが注入します
