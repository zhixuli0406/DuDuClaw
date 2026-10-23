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

- グループの「メンションのみ」（`mention_only`、グループごと）：v1.76 から、LINE グループでオンにすると、`@` で AI スタッフを呼ぶメッセージ（または @ 一覧の要求）と、呼ばれたスタッフが待っている 1 件にだけ返信します。以前はグループ全体が無反応になっていました。Telegram の共有 bot もスタッフ名を受け付けます。[@ で AI スタッフを呼び出す](../../features/ja-JP/62-calling-ai-staff.md)を参照。
- Web サイト用チャットウィジェット：公開の切り替えは `[webchat] public_widget`、ウィジェットキーは `[webchat] widget_key` に書き込みます（可視 ASCII 16〜256 文字、生成ボタンあり。公開するにはキーが必要。キーは応答に含まれません）。即時反映。ウィジェットキーは `config.toml` に平文で保存されます。Web サイトのページソースに現れる公開キーなので、シークレットとしては扱いません。
- WhatsApp、Feishu、Google Chat、Teams、WeCom、DingTalk：6 つの webhook ルートは常にマウントされています。チャネルが設定されるまでは 404 を返し、設定後はプラットフォームの署名を検証して、署名が不正なら 401 を返します。`channels.add` は再起動なしでチャネルを起動します。`channels.add` は `hot_started` と `restart_required: false` を返し、認証情報が足りないときは `not_started_reason` を付けます。従業員ごとの Slack bot も追加時に起動します。
- メッセージングアプリでの思考表示（専用の設定項目はなく、設定ファイルの詳細編集で設定。返信ごとに読み直します）：`[thinking] channel_audience` = `internal`（既定：チャネル ID が検証済みで、有効な dashboard アカウントが管理者または回答する社員に紐付いている送信者はタイマーと伏せ字化した要約、それ以外はタイマーのみ）、`all`（1 対 1 チャットのすべての送信者が要約を見られる）、`none`（何も表示しない）。`[thinking] channel_group_display` = `status`（既定：グループチャットはタイマーのみ）、`summary`、`off`。`[thinking] display` と回答する社員の `[model] show_thinking` のうち厳しい方が、引き続き両方の上限になります。`config.toml` が読めない、または不明な値のときは何も表示しません。Web サイトの訪問者（Odoo ライブチャット、WebChat ウィジェット）には思考は表示されません。詳しくは[メッセージングアプリでの思考表示](../../features/ja-JP/66-thinking-in-messaging-apps.md)。

## 連携 → Odoo → Web サイトのライブチャット

専用の保存ボタンで `odoo.livechat.set`（管理者）から `[odoo_livechat]` に書き込みます。Odoo 接続の保存は `[odoo]` 全体を書き換えるため、別セクションにしています。ボットの API キーは `bot_api_key_enc` として暗号化保存され、返されません。空欄なら変更しません。保存と「AI に戻す」は監査ログに記録されます。詳細は [features/ja-JP/61-odoo-livechat.md](../../features/ja-JP/61-odoo-livechat.md)。

| 項目 | キー | 反映 |
|---|---|---|
| Web サイトのライブチャットに応答する | `enabled` | 数秒以内（ポーリングごとに設定を再読込。オフでボットは Odoo でオフライン） |
| ボットユーザーのログイン／API キー | `bot_username`、`bot_api_key_enc` | 次のポーリング（再接続） |
| 既定の AI 社員、最初のあいさつ、確認間隔 | `default_agent`、`greeting`、`poll_interval_seconds`（2〜60） | 次のポーリング |
| 対応するライブチャットチャネル | `livechat_channel_ids` | 次のポーリング |
| 有人対応への引き継ぎ | `handoff_user_ids`、`handoff_keywords`、`handoff_message` | 次のポーリング |
| オーディエンスルール | `[[odoo_livechat.audience_rules]]`（`name`、`agent`、`enabled`、`livechat_channel_ids`、`chatbot_script_ids`、`country_codes`、`langs`、`visitor`、`partner_tags`、`keywords`）。指定した AI 社員が存在しないと保存できません | 新しいチャット（振り分け済みのチャットは変わりません） |

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
| チャネルと連携 → 外部の連絡先から @ で呼び出し（v1.76） | `[channels] public_trigger`（管理者のみ。[@ で AI スタッフを呼び出す](../../features/ja-JP/62-calling-ai-staff.md)を参照） |
| 自動化 → 記憶 → 決定の継続、決定の保持日数 | `[memory] decision_continuity`、`decision_ttl_days`（1〜3650） |
| 詳細設定 → モデル詳細設定 → 詳細キー／値 | 任意の `[セクション] キー`。各行に型（文字列、整数、小数、真偽値、文字列配列）を指定。書き込み前に `AgentConfig` 全体として解析し、失敗すると全体を拒否。`agent`、`capabilities`、`container`、`permissions`、`channels`、`odoo`、`mcp`、`runtime`、`prompt`、`os_watch`、`budget` セクションはここでは編集できません（未リリース：`prompt`、`os_watch`、`budget` を追加。`prompt.cli_bare_mode` など管理者専用のキーを含むため、管理者は設定ファイルの詳細編集で変更します） |

**管理者だけが変更できる項目。** `[agent] reports_to`、`department`、`name`、`[capabilities]` 全体、`[container] sandbox_enabled`、`network_access`、`readonly_project`、`[permissions]` の 5 つのフラグ（`can_modify_own_soul`、`can_create_agents`、`can_send_cross_agent`、`can_schedule_tasks`、`can_modify_own_skills`）と（未リリース）`[permissions] allowed_channels`、`[guardrails] enabled` と `block_secrets`、（v1.76）`[channels] public_trigger`、そして（未リリース）社員ごとの bot トークン `[channels.discord]`、`[channels.telegram]`、`[channels.slack]`、`[odoo]` 全体、`[runtime] provider` と `fallback`、`[prompt] cli_bare_mode`、`[os_watch]` 全体、`[model] account_pool`、`role = "main"` への変更とその解除、予算の緩和（`monthly_limit_cents` や `daily_cap_cents` の引き上げ、上限の撤廃（0 は上限なし）、`hard_stop` のオフ）。社員のオーナーは上限の追加や引き下げ、`hard_stop` のオンは引き続き行えます。管理者以外からの変更は拒否され、管理者による変更が成功すると監査イベント `agent_authority_changed` が、拒否された試みには `agent_authority_refused` が記録されます。モデルの変更で実行環境が自動的に切り替わる場合、管理者以外にはその旨が表示されます。`org.toml` は `reports_to` か `department` が実際に変わったときだけ更新されます。`agents.inspect` は対応するフォーム項目を `admin_only_fields` として返し、編集ページは管理者以外に対してそれらを無効にします。

**保存できる人（未リリース、動作の変更）。** `agents.update` は保存のたびに呼び出し元のロールと社員への紐付けをアカウントデータベースから読み直し、書き込み直前にファイルロック内でもう一度確認します。紐付けの解除、アカウントの停止、パスワード変更の要求は、セッションの期限を待たず次の保存から有効です。どの経路からの `agent.toml` の書き込みも（このページ、チャットの `/model`、設定ファイルの詳細編集、自動の変更）コメントとキーの順序を保ち、版を残し（[設定ファイル](#設定ファイル未リリース)を参照）、`agent_file_changed` を記録します。保存後に社員一覧を再読み込みできなかったときのメッセージは「已儲存，但暫時無法立即套用；請稍後重新整理，仍未生效請重新啟動服務」です（定期的な再読み込みはありません）。

**権限フラグが有効に（動作の変更）。** `can_create_agents`、`can_send_cross_agent`、`can_modify_own_skills`、`can_schedule_tasks` には以前は読み取り側がありませんでした。v1.68.0 からは `false` と書かれたフラグが、MCP のディスパッチゲートで対応するツールを止めます（`create_agent`。`send_to_agent`、`spawn_agent`。`create_reminder`、`schedule` 付きの `tasks_create`。`skill_hub_install`、`shared_skill_adopt`、`skill_graduate`、`skill_pin`、`skill_from_recording`）。未記載や型の誤りは許可扱いです。古いテンプレートはこれらを `false` と書いていることが多いため、アップグレード後の最初の起動で全社員に一度だけ移行を行います。`permissions_enforced_since` の印がない `[permissions]` では、4 つのフラグのうち `false` のものを `true` に変え、`permissions_enforced_since = "1.68.0"` を追加し、リセットごとに監査イベント `permission_flags_reset` を記録します。その後このページや設定ファイルの詳細編集で書いた `false` はオペレーターの判断として扱われます。一時的な役割メンバー（`agents/.ephemeral/`）は移行の対象外で、最小権限のままです。

## AI 社員ページ → データソース（未リリース）

AI 社員に紐づいている人（閲覧者以上）なら誰でも見られます。ファイルの追加と削除には、その社員のオーナーとしての紐づけか管理者であること（毎回アカウントデータベースから読み直します）に加え、社員専用コレクションの編集者かオーナーであることが必要です。

| 操作 | 書き込み先 | 反映 |
|---|---|---|
| ファイルを追加（初回）：機密度、「今この AI 社員と一緒に働いている同僚にも読ませる」 | `knowledge.db` の新しいコレクション `<表示名> 的資料`、その共有、社員設定に既にある `[knowledge] collections` の一覧 | 即時。共有先：作成者（オーナー）、その AI 社員（閲覧者）、チェック時は社員に紐づく有効なアカウント全員（オーナー → オーナー、オペレーター／閲覧者 → 閲覧者）。 |
| ファイルを追加 | 社員専用コレクションのソース | バックグラウンドで読み込み、読み込み中は一覧を 3 秒ごとに更新。 |
| 削除（「忘れさせる」も選べる） | ソースを削除し、チェック時は管理者宛ての忘却申請を出す | すぐに検索されなくなり、忘却申請は承認待ち。 |
| ドキュメントライブラリで管理 | — | コレクションを開く（権限、検索テスト、アクセス記録、設定）。 |

社員をアーカイブまたは削除すると専用コレクションもアーカイブされ（オーナーである人だけがアクセスを残します）、社員を復元するとアーカイブで外した共有が戻ります。最初のアップロードでは既存の `[knowledge] collections` の一覧にもコレクションを追加し、その後のアップロードでは一覧に触れません。詳しくは[ドキュメントナレッジベース](../../features/ja-JP/63-knowledge-base.md#ダッシュボードでの操作)。

## システム設定 → 詳細設定 → ナレッジベース検索

管理者のみ（1.76.0 で追加）。ドキュメントナレッジベースが意味で検索しているか、キーワード一致のみかを表示します。キーワードのみの場合はその理由と、すでにベクトルを持つ段落の割合も表示します。

| コントロール | 書き込み | 効果 |
|---|---|---|
| 計算方法：このコンピューター／OpenAI／Google Gemini／オフ | `[knowledge.embedding] provider`（クラウドは `cloud_model`、`cloud_account` も） | 次の検索から反映。クラウドプロバイダーを選ぶと、段落を受け取るベンダーとホストを示すダイアログが開きます。チェックボックスにチェックするまで **{vendor} を使う** は無効のままで、同意の記録は `cloud_disclosure_ack` として保存されます。匿名化が無効のときはダイアログで警告します（その間は何も送信されません）。それ以外を選ぶと同意の記録は削除されます。 |
| インストール（サイズ） | `models/embedding/<model>/` と `lib/onnxruntime/` にダウンロード | 進捗付きのバックグラウンドダウンロード。**キャンセル**後に再開でき、完了するとすべての段落のベクトル作成が待ち行列に入ります。 |
| 再確認（オフラインインストール） | — | オフラインパッケージ（`.tar` と `.tar.minisig`、展開せずに `models/embedding/` に置く）の署名をリリース鍵で確認してから展開し、各ファイルのハッシュを照合してモデルをインストール済みにします。 |
| このモデルを使う | `local_model` | ローカルモデルを切り替えます。段落にはバックグラウンドで新しいモデルのベクトルが作られます（前のモデルのベクトルは残りますが比較には使われません）。 |
| 削除 | モデルファイルを削除 | 理由を表示したうえで検索はキーワードのみになります。ランタイムライブラリは残ります。 |
| 未作成の検索データを補う | — | まだベクトルのないすべてのソースを待ち行列に入れます。 |

詳細：[ドキュメントナレッジベース](../../features/ja-JP/63-knowledge-base.md#ベクトルモデル)。

## 設定ファイル（未リリース）

RPC `agent_files.list / get / diff / history / save / rollback / withdraw / pending_get`。社員編集ページの「行動の境界」も同じ仕組みを使います（`contract.update`）。対象：人格設定（`SOUL.md`）、自己紹介（`IDENTITY.md`）、作業ルール（`INSTRUCTIONS.md`）、行動の境界（`CONTRACT.toml`）、基本設定（`agent.toml`。ここでは閲覧のみでシークレットはマスク。変更は編集ページ、管理者は設定ファイルの詳細編集でも可）、この社員専用のスキル（`SKILLS/` 配下に既にある `.md`。ファイルキーは `skill:<パス>`、例 `skill:quote/SKILL.md`）。スキルはここでは編集のみで、追加・削除はできません（追加は自作スキルの流れを使います）。スキルの隣のスクリプトには触れません。パスは各階層が単一の名前で `..` や見えない文字（ゼロ幅・方向制御など）を含まず、最大 4 階層。シンボリックリンクとハードリンクは拒否し、保存時はファイルロック内でフォルダーが差し替えられていないことを確認してから書き込みます（ロックファイルは社員フォルダーの外）。ほかのスキル書き込み経路（自作スキル、社員自身のスキルツール、スキルの卒業）はここを通らず、版も残らず、このロックも使いません。上限：Markdown とスキルは 1 ファイル 64 KB、`CONTRACT.toml` は 32 KB、`must_not` と `must_always` はそれぞれ最大 100 項目・各 500 文字。

- **設定ファイルのタブ：** 社員詳細ページの「設定ファイル」タブ（`/agents/<社員>/files`）。左は分かりやすい名前のファイル一覧で、実際のファイル名は小さな灰色の文字、バッジは「確認待ち」「ダッシュボード外で変更」「閲覧のみ」。右は最初は閲覧表示で、オーナーと管理者は「編集」で変更でき、下にサイズと上限、ボタンは「差分を表示」「保存してチェック」「変更を破棄」。チェック結果は行動の境界と同じ確認ダイアログを使います。保存時にほかの人が先に保存していた場合は、自分の内容と現在の内容を並べて表示し、「自分の内容をコピー」と「再読み込み」を選べます。「変更履歴」（Operator 以上）は時刻・変更者・変更元を一覧し、閲覧・現在の内容との比較・復元ができます。未保存の変更があるときは、ファイルやタブの切り替え、ページを閉じる前に確認します（サイドバーなどほかのリンクで移動する場合はまだ確認しません）。閲覧だけの人には「閲覧できます。変更できるのは、このスタッフの担当者か管理者だけです。」と表示されます。
- **権限：** 社員に紐付いた人は閲覧でき、Operator 以上は版と差分を、社員のオーナーと管理者は保存できます。呼び出しのたびにアカウントデータベースを読み直し、セッションの情報は使いません。審査の判断は管理者だけです。
- **保存のたびのチェック：** まず固定ルール（サイズ、制御文字、`CONTRACT.toml` の厳密な解析、隠れた・見えない内容、今回追加された行や新しい `must_always` のインジェクションの型、キーのような文字列、人格設定の大部分の削除、`must_not`／`must_always` の削除・書き換えやツール回数上限の引き上げ・撤廃。スキルはさらに、先頭の `---` ブロックがスキルローダーと同じ形式で読めること、`name` が単一の名前であること、スキルスキャナーが今回追加された行で見つけたコマンド実行・データの外部送信・インジェクションの型は高リスク（実際に追加された行で判定するので、同じ種類の 2 行目も新規扱い）、むき出しの URL や禁止事項への言及は要注意。元からある高リスクの内容は「このスキルには元から疑わしい内容があります」として表示。前回ここで保存した後にシステム外で変更されたスキルは、全体をその版と比べてチェック）、次にデプロイのユーティリティモデルによる差分の確認（`[runtime] utility_provider`／`utility_model`、`[agent_files] review_provider`／`review_model` で上書き可。費用はデプロイの `agent-file-review` に計上され、社員の予算からは引かれません）。モデルは結果を厳しくする方向にしか変えられません。結果：`pass` はそのまま適用、`warn` は理由（4 文字以上）を書いて適用するか管理者に送る、`block` は管理者以外は送るだけで、管理者は理由と「リスクを理解しました」で適用（監査は Critical）。確認が完了しないとき（ユーティリティモデルがない、タイムアウト、使えない回答、上限に到達）や、内容が長すぎて一部しか確認できなかったときは少なくとも `warn`（`[agent_files] on_analysis_failure = "block"` なら `block`）で、管理者以外は管理者に送るだけ、管理者は「リスクを理解しました」で確定します。黙って通ることはなく、無効にもできません。1 日の上限はデプロイ全体 200 回、1 人 40 回、社員 1 人あたり 60 回（`review_daily_limit`、`review_daily_limit_per_user`、`review_daily_limit_per_agent`）、さらに 1 人 1 分あたり 10 回まで（`checks_per_minute`）。同じ変更を 15 分以内に再チェックすると前回の結果を使います。確定時にモデルは再実行しません。確定は同じ人・同じファイル・同じ元の版・同じ内容で 15 分以内に限り、固定ルールはその時点の権限で再実行し（厳しくなっていれば再チェックが必要）、高リスクの確定では書き込み直前にファイルロック内で管理者であることを再確認します。ダッシュボード外で変更された SOUL.md・INSTRUCTIONS.md は全体をチェックし、最後に承認された版と比較します（理由「ダッシュボード外で変更」）。
- **保存：** エディターが読んだハッシュ（`base_hash`）をファイルロック内で照合し、その間にほかの人が変更していれば競合として拒否します。新しい内容は社員の次の対話から有効になり、チャネルでは次の返信に 1 行の案内が付きます。
- **版：** 社員フォルダーの外の `<home>/agent_file_history/<社員>/<ファイル>/`（所有者のみ読み取り可）に、既定でファイルごとに 20 版（`[agent_files] history_keep`、5〜100）。最初の変更では変更前の内容も残します。版の復元にも同じチェックが走ります。社員を削除すると版は `agent_file_history/_trash/` に移り、同じ ID の新しい社員は版なしで始まります。
- **承認済みの基準：** SOUL.md と INSTRUCTIONS.md を保存すると承認済みの指紋が更新されます（INSTRUCTIONS.md にも指紋ができ、ハートビートが `_instructions_integrity_drift` として報告）。システム外で変更されたファイルは表示で分かり、承認済みの版に戻すか、確認して現在の内容を受け入れられます（そのまま保存すると全体をチェックし、理由を書いて確定するか審査を依頼します。確定してもファイルは書き換えず、現在の内容を版として残して承認済みの基準にします。システム外で変更されていないファイルをそのまま保存した場合は「変更なし」のままです）。社員の名前を変えると SOUL.md／IDENTITY.md 内の名前も更新され、承認済みの SOUL はそのまま承認済み、すでにシステム外で変更されていた SOUL は表示されたままです。名前はプレーンテキスト（1〜64 文字、改行・見えない文字・`< > [ ] { }`・バッククォートなし）で、名前を置き換えた結果が固定ルールに引っかかる場合は同期しません（`identity_files_skipped` に表示）。
- **審査依頼：** 種類 `agent_file_change`。現在も管理者である人がダッシュボードでのみ判断でき、期限は 3 日（期限切れは拒否扱い）。チャネルにはボタンのない通知だけが届きます。提案された内容は依頼が終わるまで `<home>/agent_file_pending/` に保存されます。承認時はまず適用し、待っている間にファイルが変わっていた場合は拒否され、依頼者が送り直す必要があります。
- **監査：** `agent_file_changed`、`agent_file_change_refused`、`agent_file_review_failed`、`agent_file_change_requested`（記録するのはハッシュだけで、内容は記録しません）。

## 思考の表示

専用の項目はまだないため、下の設定ファイルの詳細編集で設定します。どちらのファイルも毎ターン読み直すので、変更は次の返信から有効です。

| ファイル | キー | 値 |
|---|---|---|
| `config.toml` | `[thinking] display` | `summary`（既定：WebChat でタイマー、トークン、伏せ字処理済みの思考の文章）、`status`（タイマーとトークンのみ）、`off`（思考フレームを送らない。WebChat 自身のタイマーは残る） |
| `config.toml` | `[thinking] max_chars` | 1 ターンの思考の文章の上限。既定 4000、200–20000 に収める |
| `config.toml` | `[thinking] anthropic_display` | `summarized`（既定）または `updates`（ベータ、一部の Claude モデルのみ） |
| `agent.toml` | `[model] show_thinking` | `summary`／`status`／`off`。未設定なら `[thinking] display` に従う。厳しい方が適用される |

読めない・解析できない `config.toml`、または解釈できない `display` の値は `status` として扱います。思考の文章が見えるのは、ダッシュボードにサインインした WebChat 利用者と、メッセージアプリでは `[thinking] channel_audience` が許可する送信者だけです（「チャネル管理」の項を参照）。詳細は[思考の表示](../../features/ja-JP/64-thinking-display.md)。

## 設定ファイルの詳細編集

場所：システム設定 → 詳細設定 → 設定ファイルの詳細編集。管理者だけに表示され、RPC は `config.raw.get` / `config.raw.set` です。

- **対象ファイル：**システム設定（`config.toml`）、推論設定（`inference.toml`）、各 AI 社員の `agent.toml`（`agents/` 配下の実ディレクトリで、シンボリックリンクは不可）。
- **シークレットの伏せ字：**キー名が `_enc` で終わる、`key` そのものか `_key` で終わる、token・secret・password・passwd・api_key・apikey・widget_key・private_key・credential・service_account_json を含む値、`headers`・`otlp_headers`・`env` テーブルのすべての値、パスワード付き URL は `«set»` と表示されます。`«set»` のまま保存すると元の値が保たれ、値がないのに `«set»` と書くと拒否されます。`[mcp_keys]` は表示されません。
- **検証：**まず TOML 構文を確認し、エラーは行と列で示します。次に型の確認（`config.toml` は各セクションの検証、`InferenceConfig`、`AgentConfig`）を行い、失敗すると書き込みません。社員の `[agent] name` はここでは変更できません。
- **競合：**開いたときに内容のハッシュを受け取り、保存時に送り返します。その間にファイルが変わっていれば拒否されるので、再読み込みしてから編集してください。
- **バックアップ：**書き込み前に `<ファイル名>.bak-<Unix 時刻>`（権限 0600）へコピーし、新しい 5 つを残します。
- **監査：**書き込みごとに `config_raw_edited` を記録します。`[delegation]`、`[acp]`、または社員の `[agent]`、`[capabilities]`、`sandbox_enabled`、`network_access`、`can_modify_own_soul` に触れた場合は Warning です。
- **ドキュメントナレッジベース：**コレクション、資料、共有、閲覧記録はサイドバーの「会社 → ドキュメントライブラリ」で管理します。全体の上限 `[knowledge]` にはまだ設定ページがなく、各キー（`enabled`、`max_source_bytes`、`max_source_chars`、`max_sources_per_collection`、`max_collections`、`max_chunks_total`、`extract_timeout_secs`、`extract_memory_mb`、`access_log_retention_days`、`max_collection_bytes`、`max_total_bytes`、`extract_when_unavailable` = `refuse`／`run_unsandboxed`、`auto_retrieve`、`inject_budget_chars_max`）はここで型を検査します。`[knowledge.embedding]` の各キー（`provider` = `local`／`openai`／`gemini`／`none`、`local_model`、`cloud_model`、`cloud_account`、`cloud_base_url`、`allow_private_endpoint`、`batch_size`、`threads`、`max_vector_rows`、`idle_unload_minutes`）も同様です。開示の確認記録 `cloud_disclosure_ack` はここでは変更できず（保存は拒否されます）、「ナレッジベース検索」の開示ダイアログからのみ書き込まれます。社員の `[knowledge] collections` は「Agent 管理 → 設定を編集 → ツールと権限 → ドキュメントライブラリ」で設定します（共有されたコレクションだけ選べます）。その社員の `agent.toml` を直接編集することもできます。返信に関する `auto_retrieve`、`answer_mode` = `assist`／`kb_only`、`top_k`、`inject_budget_chars` もそこにあります。詳しくは[ドキュメントナレッジベース](../../features/ja-JP/63-knowledge-base.md)。
- **再起動の一覧：**保存後に再起動が必要な部分を表示します。`config.toml` の `[gateway]`、`[server]`、`[telemetry]`、`[logging]`、`[channels]`、`[wiki]`、`[relay]`、`[decision]` は起動時にだけ読まれます。`general.name`、`rotation.health_check_interval_seconds`、`goal_loop.resume_on_restart`、`knowledge.max_source_bytes`（アップロードルートのサイズ上限を決める）も再起動が必要です。ログレベル、常駐センシング、秘匿化はその場で反映できた場合は一覧に出ません。`inference.toml` の保存は常にその場で推論エンジンをリセットします。社員ファイルで再起動が必要なのは `heartbeat.max_concurrent_runs` だけです。

## メンテナンス SSH 画面（未リリース）

「デバイス → メンテナンス」で管理者が期間と機能を選択し、
パスワードを再入力して `MAINTENANCE` と入力します。SSH を選ぶ場合は、
技術担当者の OpenSSH 公開鍵が 1 つ必要です。画面は形式を確認し、
秘密鍵を貼り付けると送信前に消去します。ゲートウェイとデバイス側も
独立して検証します。RSA は 3072–8192 ビットです。対応状況が不明、
旧版、一部だけ更新された OS では SSH を選べません。診断の詳細は
利用できます。パスワードを 5 回連続で間違えると 15 分間ロックされます。

開始後は `ssh duduclaw-maint@duduclaw.local`、利用可能なプライベート
IPv4 アドレス、担当者の鍵とデバイスの指紋、終了日時、接続数、
ログイン失敗数を表示します。管理者への通知と両方の指紋を照合してください。
通知先がなければデバイス本体の画面で確認します。この画面だけでは
デバイスの身元を確認できません。通知先の設定は配信成功を意味しません。
ポート 22 はプライベート・リンクローカルと VPN CGNAT `100.64.0.0/10`
のみ許可し、グローバル IPv6 は拒否します。範囲内でも信頼できる
接続元とは限りません。メニューは指定の診断とモデル・ゲートウェイの
再起動に限定され、ゲートウェイの再起動で終了・切断します。

終了を確認できない場合は警告を表示し続けます。履歴には最近の
鍵の指紋と接続イベントを表示します。状態を 30 秒ごとに取得するため、
期限切れやデバイス側の終了表示は次の取得まで遅れる場合があります。
完全な OS イメージ、実際の SSH、通知配信はまだ検証していません。
未リリースの画面であり、組み立てたデバイスの受け入れ完了を意味しません。

## セキュリティ → 認証情報の保護（未リリース）

「セキュリティ」ページの「認証情報の保護」カード（管理者、両エディション）は 1 行で
「認証情報の保護：ファイル」または「認証情報の保護：システムのキーチェーン（…）」と表示し、その意味を説明
します。対象は AI 社員が特定の人として動くための認証情報だけで、カードの表示に
かかわらずほかの設定はこれまでどおり動きます。

- **システムのキーチェーンへ移す**（`security.credential_protection_enable`）は
  macOS のキーチェーンまたは Windows 資格情報マネージャーに保護キーを作ります。
  Linux と Docker では「このプラットフォームはシステムのキーチェーンに未対応」と表示され、ボタンはありません。
- **再承認が必要**：macOS でアップグレード後やキーチェーンがロック中に表示されます。
  その Mac のターミナルで `duduclaw credentials authorize` を実行し、ダイアログを
  一度許可してください。
- **保護キーが見つからない／置き換えられた／設定エラー**：個人アカウントは停止します。
  **再度有効にする**（`reset: true`）と新しいキーが作られ、保存済みの個人の認証情報は
  無効になり、各自が再接続します。
- **キーを新しくする**（同じく `reset: true`）は問題がないときも表示され、キーと記録が
  一緒に置き換えられた疑いがあるときに使います。効果は再度有効にするのと同じです。
  hardened runtime のない macOS の実行ファイルでは、DuDuClaw に注入したコードがキーを
  読めるため「同じユーザーアカウントのプログラムは読み取り可能」と表示します。
- **無効にする**（`security.credential_protection_disable`、`confirm: true`、確認文字の
  入力が必要）はキーを削除し、保存済みの個人の認証情報は無効になります。

ターミナルでは `duduclaw credentials status`（スクリプト用に `--json`）で同じ状態を
確認できます。詳細は[セキュリティ防御](../../features/ja-JP/05-security-defense.md#特定の人の認証情報をシステムのキーチェーンに置く未リリース)。
