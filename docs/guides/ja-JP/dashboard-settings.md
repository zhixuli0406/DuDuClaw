# ダッシュボード設定の対応表（v1.68.0）

v1.68.0 からダッシュボードで設定できる項目について、どのページにあるか、どの設定キーに書き込むか、保存後に gateway の再起動が必要かをまとめます。最後の節では、専用のコントロールがないキーを編集する「設定ファイルの詳細編集」を説明します。

他の言語：[English](../dashboard-settings.md) · [繁體中文](../zh-TW/dashboard-settings.md)

## 保存の共通ルール

- システム設定（`config.toml`）は `system.update_config` で書き込まれ、管理者だけが使えます。ページは変更した項目だけを送ります。変更がなければ何も送りません。
- 書き込み時にファイルをロックして読み直します。読み込んだ後にほかの書き込みでファイルが変わっていた場合、保存は拒否されます。再読み込みしてから保存してください。
- `config.toml` が解析できない場合、`system.update_config`、常駐センシングのデータソース編集、設定ファイルの詳細編集は書き込みを拒否します。先に設定ファイルの詳細編集で構文を直してください。`config.toml` に書き込むほかの設定ページ（チャネル、アカウント、Odoo など）も同じく書き込みを拒否します。
- 保存はファイルをその場で書き換え、変更したキーだけを書き直します。コメント、空行、キーの順序、変更していないキーの書式はそのまま残り、値を変えた行の行末コメントも残ります。新しいキーはそのセクションの末尾に、新しいセクションはファイルの末尾に追加されます。設定ファイルの詳細編集だけでなく、`config.toml` や `inference.toml` に書き込むすべての設定ページで同じです。
- 再起動後に有効になるキーは応答の `restart_required` に入ります。システム設定ページとチャネル管理ページには、gateway が再起動するまで「再起動後に有効になる設定」の案内が表示されます（手動で閉じることもできます）。推論ページと AI 社員の編集ページには表示されません。
- 次のキーを変更すると、変更前後の値を含む監査イベント `config_protected_key_changed` も記録されます：`acp.trusted`、`tick.allow_command_sources`、`container.sandbox.when_unavailable`、`container.sandbox.script_when_unavailable`、`memory.supersession_trust_guard`。

## システム設定 → 詳細設定 → 自動化エンジン

| セクション → コントロール | キー | 反映 |
|---|---|---|
| ゴールループとディスパッチ → ディスパッチ方針に「1人の社員に4つの役割」を追加 | `[dispatch] policy = "role_team"` | 即時（ディスパッチエンジンを再起動） |
| ゴールループとディスパッチ → 受け入れ判定に使う実行環境 | `[dispatch] judge_provider` | 即時 |
| ゴールループとディスパッチ → 受け入れ判定に使うモデル | `[dispatch] judge_model` | 即時 |
| ナレッジとメモリ → 夜間の整理（モデル段階） | `[night] llm_enabled` | 即時 |
| 人による引き継ぎ（独立カード） | `[takeover] enabled`、`duration_minutes`、`max_duration_minutes`（一時停止は上限を超えられません） | 即時 |
| AI 社員のメールボックス（独立カード） | `[mail] enabled`、`gmail_enabled`、`dropfolder_enabled`、`default_agent`、`auto_trigger` | 即時 |
| 1 人 4 役（全体の既定値） | `[team] enabled`、`gate`、`[team.roles.<役割>] runtime`／`model`／`effort`（統合役は `utility` として保存） | 次のゴール |
| 常駐センシング → 有効化、既定のペース、コマンド型ソースの許可、アドレス解決キャッシュ（秒） | `[tick] enabled`、`preset`、`allow_command_sources`、`dns_ttl_secs` | gateway に常駐センシングが読み込まれていれば即時にソースを再起動、そうでなければ再起動後 |
| 常駐センシング → データソース（追加・編集・削除） | `[[tick.sources]]`、RPC `tick.sources.list` / `upsert` / `remove`（管理者） | 同上。`headers` の値は返さず件数だけ返します。`command` 型ソースの書き込みは `config_protected_key_changed` も記録 |

## システム設定 → 詳細設定 → システム

| セクション → コントロール | キー | 反映 |
|---|---|---|
| 一般 → ログ形式 | `[logging] format`（`json`。それ以外の値はプレーンテキスト） | 再起動後 |
| 表示名 | `[general] name` | 再起動後（LAN への告知は起動時に作られます） |
| アカウントのヘルスチェック間隔 | `[rotation] health_check_interval_seconds` | 再起動後 |
| タスクサンドボックス → イメージ、使えないとき（タスク）、使えないとき（スクリプト） | `[container.sandbox] image`、`when_unavailable`、`script_when_unavailable` | 即時（タスクごとに読み込み） |
| タスクサンドボックス → リソース上限 | `memory_bytes`、`tmp_bytes`、`workspace_bytes`、`pids`、`cpu_millis`、`max_turns`。セクション全体で検証（`tmp_bytes + workspace_bytes` は `memory_bytes` 以下） | 即時 |
| タスクサンドボックス → コンピューター操作のイメージ | `[computer_use] image` | 即時 |
| 記憶・トレース・信頼 → 記憶の信頼度チェック | `[memory] supersession_trust_guard` | 新しいセッション |
| 記憶・トレース・信頼 → OpenTelemetry トレースの送信先 | `[telemetry] otlp_endpoint`。`otel` 機能を含むビルドでだけ表示（`system.status` の `otel_compiled`） | 再起動後 |
| 記憶・トレース・信頼 → GitHub ツール | `[integrations] github`（オフにもできます） | 即時 |
| 記憶・トレース・信頼 → 外部 A2A リクエストを信頼（管理者のみ表示） | `[acp] trusted` | 即時 |
| ローカルファイルのフォルダ | `[files] allowed_roots`（完全パス、ファイルシステムのルートは不可、最大 64） | 即時 |
| シークレット管理 → 詳細設定：シークレットバックエンド | 1Password：`onepassword_host`、`onepassword_vault`、アクセストークン。Infisical：`infisical_addr`、`infisical_project_id`、`infisical_environment`、アクセストークン。トークンは `*_enc` として暗号化保存 | 再起動の報告なし |

`[general] log_level`（日常 → 一般設定 → ログレベル）は保存するとすぐ反映されます。環境変数 `RUST_LOG` が設定されている場合だけ反映できず、再起動が必要と報告されます。アカウントのローテーション戦略とレート制限のクールダウンを保存するとローテーションのキャッシュが消え、次の呼び出しから新しい値が使われます（以前は最大 30 分遅れることがありました）。

## チャネル管理

- Web サイト用チャットウィジェット：公開の切り替えは `[webchat] public_widget`、ウィジェットキーは `[webchat] widget_key` に書き込みます（可視 ASCII 16〜256 文字、生成ボタンあり。公開するにはキーが必要。キーは応答に含まれません）。即時反映。ウィジェットキーは `config.toml` に平文で保存されます。Web サイトのページソースに現れる公開キーなので、シークレットとしては扱いません。
- WhatsApp、Feishu、Google Chat、Teams、WeCom、DingTalk：6 つの webhook ルートは常にマウントされています。チャネルが設定されるまでは 404 を返し、設定後はプラットフォームの署名を検証して、署名が不正なら 401 を返します。`channels.add` は再起動なしでチャネルを起動します。`channels.add` は `hot_started` と `restart_required: false` を返し、認証情報が足りないときは `not_started_reason` を付けます。従業員ごとの Slack bot も追加時に起動します。

## 推論

保存（`inference.update`、管理者）に成功すると gateway が推論エンジンをリセットし、次の返信から新しい設定が使われます。再起動は不要です。

- 信頼度ルーター → 詳細：`[router] local_tools`、`ucci_fast_router`、`ucci_strong_router`、`ucci_observations`、`ucci_shadow_strong`、`ucci_shadow_max_inflight`（1〜16）、`ucci_drop_stop_token`、`[generation] capture_logprobs`、`capture_top_logprobs`。
- llamafile ローカルサーバー：`[llamafile] enabled`、`dir`、`default_file`、`host`、`port`、`gpu_layers`、`context_size`、`extra_args`。既知の制限：画面で項目を空にしても保存済みの値は消えません。消すときは設定ファイルの詳細編集を使ってください。

## AI 社員の編集ページ

このページは `agents.update` でその社員の `agent.toml` に書き込み、変更した項目だけを送ります。そのため、保存してもハートビートのスケジュール（`[heartbeat] cron`）が消えることはなくなりました。

| タブ → セクション | キー |
|---|---|
| 予算 → 1日の上限 | `[budget] daily_cap_cents` |
| ツールと権限 → 確認が必要なツール（常に承認待ち、不可逆として扱う、先に判定役が判断、タスク許可が必要） | `[capabilities] approval_required_tools`、`irreversible_tools`、`maybe_irreversible_tools`、`scoped_tools` |
| ツールと権限 → 並行ブランチ | `[fork] enabled` |
| ツールと権限 → 送信内容の保護 | `[guardrails] enabled`、`block_secrets`、`block_injection_echo`、`redact_pii`、`deny_phrases` |
| ツールと権限 → 権限 | `[permissions]` の 4 つのフラグ（後述） |
| 頭脳とエンジン → 推論の強さ | `[model] effort` |
| 頭脳とエンジン → プロバイダー／フォールバック | `[runtime] provider`／`fallback`。qwen、kimi、copilot、kiro、cursor、vibe、opencode を追加 |
| 頭脳とエンジン → 起動内容の簡素化 | `[runtime] minimal_context` |
| 頭脳とエンジン → 1人の社員に4つの役割 | `[team] enabled`、`[team.roles.<役割>]` |
| 自動化 → 夜間の整理 | `[night_engine] enabled` |
| 自動化 → 記憶 → 決定の継続、決定の保持日数 | `[memory] decision_continuity`、`decision_ttl_days`（1〜3650） |
| 詳細設定 → モデル詳細設定 → 詳細キー／値 | 任意の `[セクション] キー`。各行に型（文字列、整数、小数、真偽値、文字列配列）を指定。書き込み前に `AgentConfig` 全体として解析し、失敗すると全体を拒否。`agent`、`capabilities`、`container`、`permissions`、`channels`、`odoo`、`mcp`、`runtime` セクションはここでは編集できません |

**管理者だけが変更できる項目。** `[agent] reports_to`、`department`、`name`、`[capabilities]` 全体、`[container] sandbox_enabled`、`network_access`、`[permissions] can_modify_own_soul`。管理者以外からの変更は拒否され、管理者による変更が成功すると監査イベント `agent_authority_changed` が、拒否された試みには `agent_authority_refused` が記録されます。`org.toml` は `reports_to` か `department` が実際に変わったときだけ更新されます。

**権限フラグが有効に（動作の変更）。** `can_create_agents`、`can_send_cross_agent`、`can_modify_own_skills`、`can_schedule_tasks` には以前は読み取り側がありませんでした。v1.68.0 からは `false` と書かれたフラグが、MCP のディスパッチゲートで対応するツールを止めます（`create_agent`。`send_to_agent`、`spawn_agent`。`create_reminder`、`schedule` 付きの `tasks_create`。`skill_hub_install`、`shared_skill_adopt`、`skill_graduate`、`skill_pin`、`skill_from_recording`）。未記載や型の誤りは許可扱いです。古いテンプレートはこれらを `false` と書いていることが多いため、アップグレード後の最初の起動で全社員に一度だけ移行を行います。`permissions_enforced_since` の印がない `[permissions]` では、4 つのフラグのうち `false` のものを `true` に変え、`permissions_enforced_since = "1.68.0"` を追加し、リセットごとに監査イベント `permission_flags_reset` を記録します。その後このページや設定ファイルの詳細編集で書いた `false` はオペレーターの判断として扱われます。一時的な役割メンバー（`agents/.ephemeral/`）は移行の対象外で、最小権限のままです。

## 設定ファイルの詳細編集

場所：システム設定 → 詳細設定 → 設定ファイルの詳細編集。管理者だけに表示され、RPC は `config.raw.get` / `config.raw.set` です。

- **対象ファイル：**システム設定（`config.toml`）、推論設定（`inference.toml`）、各 AI 社員の `agent.toml`（`agents/` 配下の実ディレクトリで、シンボリックリンクは不可）。
- **シークレットの伏せ字：**キー名が `_enc` で終わる、`key` そのものか `_key` で終わる、token・secret・password・passwd・api_key・apikey・widget_key・private_key・credential・service_account_json を含む値、`headers`・`otlp_headers`・`env` テーブルのすべての値、パスワード付き URL は `«set»` と表示されます。`«set»` のまま保存すると元の値が保たれ、値がないのに `«set»` と書くと拒否されます。`[mcp_keys]` は表示されません。
- **検証：**まず TOML 構文を確認し、エラーは行と列で示します。次に型の確認（`config.toml` は各セクションの検証、`InferenceConfig`、`AgentConfig`）を行い、失敗すると書き込みません。社員の `[agent] name` はここでは変更できません。
- **競合：**開いたときに内容のハッシュを受け取り、保存時に送り返します。その間にファイルが変わっていれば拒否されるので、再読み込みしてから編集してください。
- **バックアップ：**書き込み前に `<ファイル名>.bak-<Unix 時刻>`（権限 0600）へコピーし、新しい 5 つを残します。
- **監査：**書き込みごとに `config_raw_edited` を記録します。`[delegation]`、`[acp]`、または社員の `[agent]`、`[capabilities]`、`sandbox_enabled`、`network_access`、`can_modify_own_soul` に触れた場合は Warning です。
- **再起動の一覧：**保存後に再起動が必要な部分を表示します。`config.toml` の `[gateway]`、`[server]`、`[telemetry]`、`[logging]`、`[channels]`、`[wiki]`、`[relay]`、`[decision]` は起動時にだけ読まれます。`general.name`、`rotation.health_check_interval_seconds`、`goal_loop.resume_on_restart` も再起動が必要です。ログレベル、常駐センシング、秘匿化はその場で反映できた場合は一覧に出ません。`inference.toml` の保存は常にその場で推論エンジンをリセットします。社員ファイルで再起動が必要なのは `heartbeat.max_concurrent_runs` だけです。
