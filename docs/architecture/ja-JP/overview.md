# DuDuClaw アーキテクチャ概要

## アーキテクチャ概要（v1.67.0 時点で照合済み）

DuDuClawは**マルチランタイム AI エージェントプラットフォーム（Multi-Runtime AI Agent Platform）**であり、統一された `AgentRuntime` トレイトを通じて**Claude Code / Codex / Antigravity / Grok** CLI（および OpenAI 互換 API）をAIバックエンドとしてサポートし（Gemini CLI バックエンドは v1.67.0 で非推奨、v1.69.0 で削除、Antigravity に置き換え）、自動検出とエージェントごとの設定に対応しています。DuDuClawは単体のLLM製品ではなく、1つ（または複数）のAI CLIを、チャネルルーティング・セッション記憶・自己進化・マルチアカウントローテーション・ローカルLLM推論・ブラウザ自動化・IDE統合を備えた長時間稼働エージェントへと変える配管層です。

## 主要なアーキテクチャ上の決定

### ランタイムとトランスポート
- **Multi-Runtime**（`AgentRuntime` トレイト）— `runtime_catalog.rs` に 13 個のランタイムidがあります。12 個の CLI バックエンド（Claude、Codex、Antigravity（`agy`）、Grok、Qwen Code、Kimi Code、GitHub Copilot CLI、Kiro、Cursor、Mistral Vibe、OpenCode、そして v1.67.0 で非推奨・v1.69.0 で削除される Gemini CLI）と OpenAI-compat HTTP です。5 つの CLI と OpenAI-compat はそれぞれ専用モジュールを持ち、残り 7 つの CLI は汎用の print モードランタイム 1 つを共有します。`RuntimeRegistry` による自動検出、エージェントごとの設定は `agent.toml [runtime] provider` に記述。
- **ランタイム間フェイルオーバー時のモデル置換**（`failover.rs`）：`[runtime] fallback` が*別の* provider に呼び出しを回す場合、フォールバック先ランタイムはプライマリのモデルidを引き継がなくなりました（以前は codex エージェントの `gpt-5.4` が Claude ランタイムに対して spawn されていました）。モデルは次の4つの分岐を順に評価して決まります。① `agent.toml [model] fallbacks` のうち、モデルファミリーが確実にフォールバック先ランタイムのものである最初のエントリ（`provider/model` 形式の修飾は外します）、② そのランタイムが要求されたモデルをすでに扱える場合は要求モデルを維持（モデルファミリーを宣言しない `openai_compat` が任意のidをプロキシし続けられるのはこの分岐によります）、③ ランタイムカタログがそのバックエンドに載せている最初のモデル、④ いずれにも当てはまらなければ**spawn を拒否**し、`no model configured for fallback runtime <P>` を報告して失敗した試行として記録します。置換のたびに `agent` / `from_runtime` / `to_runtime` / `from_model` / `to_model` を含む `warn!` を出力します。
- **MCP Server（stdio）**（`duduclaw mcp-server`）— stdin/stdout上のJSON-RPC 2.0を通じて、チャネル・メモリ・エージェント・skill・task・共有wiki・autopilotの各ツールをAI Runtimeに公開します。登録はエージェントレベルの `<agent>/.mcp.json`（v1.8.5でv1.8.4のグローバル登録を取り消しました。Claude CLIの `-p --dangerously-skip-permissions` はプロジェクトレベルの `.mcp.json` しか読み込まないため）。Gateway起動時に全エージェントの `.mcp.json` を自動作成・修復します。
- **MCP Server（HTTP/SSE）**（`duduclaw http-server --bind 127.0.0.1:8765`、v1.9.4）— Bearer認証の `POST /mcp/v1/call`（単発のJSON-RPCツール呼び出し）、`GET /mcp/v1/stream`（長時間接続のSSEイベントストリーム、Bearerまたは `?api_key=`）、`POST /mcp/v1/stream/call`（非同期＋SSE結果プッシュ）、`GET /healthz`（認証不要）。トークンバケット方式のレート制限（60 req/min）。`mcp_sse_store.rs` がbroadcastチャネルでSSE接続を管理します。外部HTTPクライアント向けにstdioを補完します。
- **ACP と A2A**— `duduclaw acp`（= `duduclaw acp client`）は Zed / JetBrains / Neovim のエージェントパネル向けに、stdio 上で Agent Client Protocol v1 を実装します。`duduclaw acp server`（旧名 `acp-server`、v1.69.0 までは引き続き解釈されます）は A2A を提供します。`message/send` は `bus_queue.jsonl` に追記し、`tasks/get` は bus の観測結果を A2A の状態に対応づけ、Agent Card は `/.well-known/agent-card.json`（旧 `/agent.json` エイリアスあり）にあります。
- **エージェントディレクトリ**はClaude Codeと互換性があります。各ディレクトリには `.claude/`、`.mcp.json`、`SOUL.md`、`CLAUDE.md`、`CONTRACT.toml`、`agent.toml`、`wiki/`、`SKILLS/`、`memory/`、`tasks/`、`state/` が含まれます。

### チャネル（11種類）
- **Telegram**（ロングポーリング）— ファイル/写真/スタンプ/音声、フォーラム/トピック、メンション限定、OpenAI Whisper API による音声文字起こし。
- **LINE**（webhook）— HMAC-SHA256署名、スタンプカタログ、チャットごとの設定。
- **Discord**（Gateway WebSocket）— `tokio::select!` によるハートビート、スラッシュコマンド、自動スレッド。ボイスチャンネル（Songbird）はデフォルト無効の `discord-voice` feature を有効にしたビルドでのみ使え、リリースバイナリには含まれません。v1.9.2で強化：本物のop 6 RESUME（`session_id` + `resume_gateway_url` + シーケンス番号を永続化）、スタール監視（ハートビート間隔の2倍を超えて通信がなければ切断）、ハートビートチャネルの容量を1→16にし `try_send` へ変更、op 9に1〜5秒のジッターを追加、RESUMEDディスパッチの処理、backoff上限を60秒に短縮。
- **Slack**（Socket Mode）、**WhatsApp**（Cloud API webhook、署名検証は fail-closed）、**Feishu**（Open Platform v2）、**Google Chat**（webhook、JWT 検証）、**Microsoft Teams**（Azure Bot / Connector v3、JWT 検証）、**WeCom**（HMAC-SHA1 + AES-256-CBC）、**DingTalk**（HMAC-SHA256 + 時間窓）、**WebChat**（`/ws/chat` + Reactフロントエンド）。
- 汎用の `POST /webhook/{agent_id}` エンドポイントは一度もマウントされたことがなく、v1.66 で削除されました。受信 webhook は個々のチャネルと Odoo（`POST /webhook/odoo`）に属します。
- **チャネルのホットスタート/ストップ**：Dashboardの `channels.add` / `channels.remove` でgatewayを再起動せずにチャネルタスクを起動/中断できます。
- **メディアパイプライン**：画像の自動リサイズ（最大1568px）+ MIME検出 + Vision統合。

### サブエージェントオーケストレーション
- `create_agent` / `spawn_agent` / `list_agents` MCPツール、`reports_to` 階層と連動。
- System promptが「## Your Team」サブエージェント名簿を自動注入します。
- **構造化ハンドオフ**：`DelegationEnvelope`（context / constraints / task_chain / expected_output）、失敗時はRaw形式にフォールバック。
- **TaskSpecワークフロー**：依存関係を考慮したスケジューリング、自動リトライ（3回）、リプラン（2回）、永続化を備えた多段階タスク計画。
- **長文レスポンスの分割**：チャネルのバイト予算を超えるサブエージェントの返信は `channel_format::split_text` で分割され、`📨 **agent** 的回報 (1/N)` / `(續 2/N)` ラベルが付与されます（Discord 1900 / Telegram 4000 / LINE 4900 / Slack 3900）。
- **孤立レスポンスの復旧**：`reconcile_orphan_responses` が、クラッシュ/Ctrl+C/ホットスワップで取り残された `bus_queue.jsonl` のエントリをアトミックに再生します。

### セッション記憶スタック
- **ネイティブなマルチターン**：Claude CLIの `--resume <session-id>`（SHA-256による決定的なセッションID付き）。`--resume` が失敗した場合（古いハンドル、アカウントローテーション、未知のstream-jsonエラー）は履歴をプロンプトに埋め込む方式に自動フォールバックします。
- **ターントリミング**（800文字超 → 先頭300 + 末尾200 + `[trimmed N chars]`、CJK対応）。
- **Direct APIのpromptキャッシュ**（"system_and_3" ブレークポイント戦略。測定済みのヒット率は公表していません）。
- **圧縮サマリー**は50kトークンの閾値でsystem prompt（会話ターンではなく）に注入されます。
- **Instruction Pinning**（v1.8.6 P0）— ユーザーの最初のターン → 非同期でHaikuがコアタスクを抽出 → `sessions.pinned_instructions` に保存 → system promptの末尾に注入（U字型の注意分布を利用）。明確化の回答は蓄積されます（≤1000文字）。
- **Snowball Recap**（v1.8.6 P0）— 各ターンでユーザーメッセージの前に `<task_recap>` を付加。LLMコストはゼロ。
- **P2 Key-Fact Accumulator**（v1.8.6）— 実質的な内容のあるターンごとに、Haikuが2〜4件の重要事実を抽出 → FTS5付きの `key_facts` テーブルに保存 → 最も関連性の高い上位3件をsystem promptに注入。約100〜150トークンで、MemGPTの6,500トークン（−87%）と比較して大幅に削減。
- **CLI軽量パス**— `call_claude_cli_lightweight()` に `--effort medium --max-turns 1 --no-session-persistence --tools ""` を付与し、メタデータタスクに使用。コストを25〜40%削減。
- **安定化フラグ**— `--strict-mcp-config`（MCP分離）+ `--exclude-dynamic-system-prompt-sections`（ターンをまたいだprompt安定性、トークンを10〜15%削減）。`--bare` はv1.8.11で削除されました（OSキーチェーンの認証情報検索を壊していたため）。

### 進化
- **予測駆動エンジン**：Active Inference + Dual Process Theory。設計上、ほとんどの会話は LLM 呼び出しなしで終わります（測定済みの割合は公表していません）。中程度の誤差はエピソード記憶として保存され、有意な誤差は AEE の進化ラウンドを開始し、重大な誤差（または有意な誤差が3回連続）は緊急ラウンドを開始します。無視できる誤差のうち少数の探索枠（ε、下限 5%）もラウンドを開始します。
- **MetaCognition**：100回の予測ごとに誤差閾値を自己校正。
- **MistakeNotebook**：ループを横断するエラー記憶で退行を防止。エントリには決定的な `TrajectoryEvidence`（どのツール/アサーションが失敗したか）が付与され、リフレクションの統合が未検証の自己申告診断を鵜呑みにしなくなりました（Evolution v3）。
- **`SOUL.md` はエージェントに対して読み取り専用**です（Evolution v3 WP1.1、オペレーター/ダッシュボードのみ書き込み可）。旧来のGenerator→Verifier→Updater書き換え経路、`SOUL.md`バージョン管理、24時間の観察ウィンドウ、自動ロールバックは **2026-09-29（S11）に削除**されました：エージェントに代わってこのファイルを書ける経路がもう存在しない以上、それらが守っていた対象自体がありません。
- **AEE（Agentic Evolution Engine、唯一の進化エンジン）**：進化の対象は `SOUL.md` ではなく**playbook**です。カテゴリ／シグナル／eval caseに紐づく、小さく個別に廃止可能な遺伝子状のエントリの集合で、Gate（決定的、拒否権を保持）とMeasure（スコアリング、拒否権なし）の分離、champion＋現状維持か改善のみ許可するコミットゲート（matches-or-improves）、そしてファイル全体ではなくエントリ単位の観察期間を通じて進化します。詳細は `evolution-engine.md` 第12章と `docs/features/38-aee-playbook-evolution.md` を参照。
- **Agent-as-Evaluator**：独立したEvaluator Agent（コスト管理のためHaikuを使用）が、構造化されたJSON判定によるアドバーサリアル検証を行います。
- **ConversationOutcome**：LLMコストゼロで会話結果を検出（TaskType / Satisfaction / Completion）、zh-TW + en の両言語に対応。
- **外部要因**：ユーザーフィードバック、セキュリティイベント、チャネル指標、Odooのビジネスコンテキスト、他エージェントからのシグナルが予測エンジンと進化ラウンドに反映されます。

### Wikiナレッジレイヤー（v1.8.9）
- **4層アーキテクチャ**（Vault-for-LLMに着想）：L0 Identity / L1 Core / L2 Context / L3 Deep。
- **信頼度重み付け**（frontmatterの `trust`、0.0〜1.0）— 検索結果は信頼度加重スコアで順位付けされます。
- **自動注入**：`build_system_prompt()` がL0+L1ページを自動的にWIKI_CONTEXTへ注入します。CLI／チャネル返信／dispatcherの各パスに対応し、Claude / Codex / Antigravity / Grok / OpenAI-compat の各ランタイム（および非推奨の Gemini ランタイム）で統一されています。
- **FTS5インデックス**（`unicode61` トークナイザー）— 書き込み/削除のたびに自動同期、`wiki_rebuild_fts` で手動再構築も可能。
- **ナレッジグラフ**：`wiki_graph` MCPツールがBFS深度を制限したMermaid図をエクスポート。ノードの形状はレイヤーごとに異なります。
- **重複検出**：`wiki_dedup` はタイトル一致＋タグのJaccard類似度（≥0.8）で重複ページを検出します。
- **逆引きbacklinkインデックス**：`related` frontmatterと本文中のmarkdownリンクをスキャンし、双方向のマッピングを構築します。
- **検索フィルター**：`wiki_search`（どちらの scope でも）は `min_trust`、`layer`、`expand`（1ホップのbacklink展開）に対応。
- **Shared Wiki**：`~/.duduclaw/shared/wiki/` にエージェント横断のSOP・ポリシー・製品仕様を格納。可視性は `wiki_visible_to` capabilityで制御。

### メモリシステム
- **認知メモリ**（オプション）：`SqliteMemoryEngine`、エピソード記憶と意味記憶を分離し、Generative Agentsの3軸重み付け検索（Recency × Importance × Relevance）を採用。
- **メモリ減衰の日次スケジューラー**：バックグラウンドタスクが24時間ごとに `duduclaw_memory::decay::run_decay` を実行。重要度が低く30日経過 → アーカイブ。アーカイブ済みで90日経過 → 完全削除。
- **認知メモリMCPツール**：`memory_search_by_layer`（エピソード/意味フィルター）、`memory_successful_conversations`、`memory_episodic_pressure`、`memory_consolidation_status`。
- **MemGPT 3層システム**（Core Memory、Recall Memory、Archival Bridge、Budget Manager、Consolidation Pipeline、MCPツール6個）は**v1.8.1で削除**されました（−1,985行）。このプロンプト注入方式はプロンプトごとに6,500トークンも肥大化させ、「lost in the middle」による注意力の劣化を引き起こしていました。

### ローカル推論
- **統一 `InferenceBackend` トレイト**（`duduclaw-inference` crate）：OpenAI互換HTTP（llama-server/Ollama/vLLM/SGLang/llamafile）。プロセス内の llama.cpp、mistral.rs、MLX バックエンドは 2026-09 に削除されました。どのリリースバイナリもこれらをコンパイルしていませんでした。代わりにローカルの OpenAI 互換サーバーを動かしてください。
- **Confidence Router**：LocalFast / LocalStrong / CloudAPIの3段階ルーティング、CJKを考慮したトークン推定。
- **InferenceManager**：自動切り替えのステートマシン：llamafile → Direct backend → OpenAI-compat → Cloud API。
- **llamafile manager**：サブプロセスのライフサイクル管理、ヘルスモニタリング、localhostでOpenAI互換APIを提供。
- **MCPツール**：`model_list`、`model_load`、`model_unload`、`inference_status`、`hardware_info`、`route_query`、`inference_mode`、`llamafile_start/stop/list`。

### トークン圧縮
- **返信経路の予算パイプライン**（`gateway/prompt_compression.rs`）：TurnTrim → DropOldestToolEchoes → BisectAndSummarize。コスト圧力を考慮し、CJK 安全なトークン推定を使います。直近のキャッシュ効率が 50% を超え、予算超過が 15% 未満のときはスキップします。
- 以前の Meta-Token（LTSC）/ LLMLingua-2 / StreamingLLM 圧縮器と、その `compress_text` / `decompress_text` ツールは v1.33 で削除されました。

### 音声パイプライン
- **HTTP エンドポイント**：`POST /api/stt`（OpenAI 互換の文字起こし API またはローカルのコマンドテンプレート。STT provider が未設定なら 501）、`POST /api/tts`（Piper（ローカル）/ Edge TTS / MiniMax T2A / OpenAI TTS）。
- **Telegram の音声**：OpenAI Whisper API による文字起こしと Edge TTS による返信で、どちらもハードコードされています。ダッシュボードの音声設定はこの経路には反映されません。
- **リリースバイナリに含まれないもの**：プロセス内 Whisper（`whisper` feature）、ONNX 埋め込み（`onnx` feature）、Discord ボイス（`discord-voice` feature）。
- SenseVoice、Deepgram、Silero VAD、`symphonia` によるデコード、LiveKit の音声ルームは以前ここに記載されていましたが、いずれもコードが存在しません。[`docs/features/ja-JP/14-voice-pipeline.md`](../../features/ja-JP/14-voice-pipeline.md) を参照。

### セキュリティ
- **Claude Code PreToolUse hooks**（`agent_hook_installer` がエージェントごとに `<agent_dir>/.claude/settings.json` へ導入）：`duduclaw hook agent-file-guard`（Rust サブコマンド、matcher `Write|Edit|MultiEdit|Bash`。正規ツリー外のエージェント構造ファイル、自分の SOUL.md への書き込み、他エージェントへの書き込みをブロックし、`reports_to` / `department` / `name` / `[capabilities]` / `[delegation]` / `[acp]` に対する `org_field_guard` のフィールド単位凍結を含む）と `duduclaw hook data-file-guard`（RFC-23 §14.4、matcher `Read|Bash`、匿名化が有効なときだけ武装。サンドボックスではなく `Bash` のファイル名ヒューリスティック。H10 2026-09 が Windows で無効だったシェルスクリプトを置き換え）。2026-04 の3段階シェルスクリプト防御と GREEN/YELLOW/RED の脅威レベルステートマシンは `ba015a48` で削除済みです。[`docs/features/ja-JP/05-security-defense.md`](../../features/ja-JP/05-security-defense.md) を参照。
- **SOUL.mdドリフト検出**（SHA-256フィンガープリント、`.soul_history/` に最大10世代のバックアップ）。
- **Prompt injectionスキャナー**（`input_guard`、11種類のルールカテゴリ、ブロック閾値60、NFKC正規化、英語＋zh-TW パターン、XML区切り文字による保護）。
- **機密情報漏洩スキャナー**— 19 種類の機密パターン（Anthropic / OpenAI / AWS / GitHub / GitLab / Slack / Stripe / Google / SendGrid / JWT / PEM 鍵、および鍵やパスワードの代入）と高エントロピー検査。skill セキュリティスキャナーが使用します。
- **CONTRACT.toml**— `must_not` / `must_always` の境界ルール、system promptに自動注入。`duduclaw test` レッドチームCLI（組み込み9シナリオ）。
- **統一マルチソース監査ログ**：`audit.unified_log` が `security_audit.jsonl` / `tool_calls.jsonl` / `channel_failures.jsonl` / `feedback.jsonl` を共通のエンベロープ（timestamp / source / event_type / agent_id / severity / summary / details）にマージし、Logsページのフィルターチップで絞り込めます。
- 保存時は**AES-256-GCM**— エージェントごとに鍵を分離。
- **ダッシュボード／WebSocket 認証**：JWT アカウントログイン（パスワードは Argon2id でハッシュ化して `users.db` に保存）、または gateway の管理者トークンです。以前の Ed25519 challenge-response の経路は gateway から削除されました。どの設定からも有効にできないものでした。Ed25519 はライセンス署名、更新の検証、relay のデバイスプロトコルで引き続き使われます。
- **コンテナサンドボックス**：独立した 2 つのパスがあります。エージェント単位の*タスクサンドボックス*（`agent.toml [container] sandbox_enabled`）は、委任されたタスクの AI CLI を読み取り専用・非 root・リソース制限付きの Docker コンテナで実行します（Docker のみ、`network_access = true` が必要、実行できないときは fail-closed。[タスクサンドボックスガイド](../../guides/ja-JP/task-sandbox.md)を参照）。PTC `execute_program` と `secaudit` の PoC ステップが使う*スクリプトサンドボックス*は Docker 上（Windows ではまず WSL2）で動き、`--network=none`、読み取り専用ルートで、マウントは読み取り専用の専用スクリプトディレクトリだけです。使えないとき PTC はスクリプトを実行せず（`[container.sandbox] script_when_unavailable = "run_unsandboxed"` の場合を除く）、PoC はホスト上で実行されません。
- **ブラウザ自動化と Computer Use**：エージェントが選ぶ取得ツール 2 つと任意のブラウザサーバーがあり、自動ルーターはありません。L1 `web_fetch_cached`（SSRF ゲート付きキャッシュ HTTP）、L2 `web_extract`（CSS セレクタスクレイプ）。L5 Computer Use は `computer_use_orchestrator` が起動するコンテナ内で動き（イメージ `ghcr.io/zhixuli0406/duduclaw-computer-use:v<version>`、自動 pull なし、アクションは `xdotool`）、エージェントが 8 つの `computer_*` MCP ツールで駆動します（`duduclaw mcp-server` プロセスが署名付き loopback ルート `POST /api/internal/computer-use`、`computer_use_sessions/` を通じて gateway 所有のセッションに転送。ネットワークはセッション開始時にピン留めされた、エージェントごとの `allowed_domains` のホストにのみ）。チャットで起動するループと `native` のホストデスクトップモードは削除されました。L3 ヘッドレスはエージェントごとの任意の Playwright/Browserbase MCP サーバー（`.mcp.json`）。`CapabilitiesConfig`（`computer_use` / `browser_via_bash` / `allowed_tools` / `denied_tools`）によりデフォルト拒否。死コードだった `browser_router.rs` の5層ルーターとその「L4 Sandbox Browser」層は 2026-09 に削除。
- **CJK安全なバイトスライス**：`duduclaw_core::truncate_bytes` / `truncate_chars` が31箇所の安全でない `s[..s.len().min(N)]` を置き換え（v1.8.11のマルチバイトコードポイントpanicを修正）。

### アカウントとコスト
- **エージェントごとのモデルルーティング**（SDKファースト）：`agent.toml [model]`— `preferred`（Claude SDKモデル）、`local.model`、`local.use_router`、`api_mode`（cli/direct/auto）、`account_pool`（後述）。
- **マルチOAuthアカウントローテーション**：OAuthセッション（Claude Pro/Team/Max、`claude auth status` 経由。`setup-token` アカウントは `CLAUDE_CODE_OAUTH_TOKEN`）+ APIキー。4種類の戦略（Priority/LeastCost/Failover/RoundRobin）。レート制限クールダウン（2分）、課金枯渇クールダウン（24時間）、予算強制、トークン有効期限追跡（30日/7日前の警告）。
- **エージェントごとのアカウントプール**（`agent.toml [model] account_pool`）：そのエージェントが使用できるローテーション対象アカウントを制限します。適用対象は**候補集合**（provider/health/cooldown/budgetのフィルターの後、戦略実行の前）なので、4つの戦略はすべて絞り込んだ集合上でも意味論をそのまま保ちます。エントリはアカウントの `id` **または**dashboardの `label`（完全一致、トリム済み、ASCII大文字小文字を区別しない。部分一致は不可）と照合されます。**Fail-open**：プールが*利用可能な*アカウントに一件も一致しない場合（idが古い、全アカウントがクールダウン中など）は `warn` を記録し、フルセットにフォールバックします。古くなったプールがエージェントをアカウントなしの状態にすることは絶対にありません。未設定/空 ⇒ ローテーションの挙動は変わりません。エントリポイント：`AccountRotator::select_with_pool` / `select_for_provider_with_pool`。
- **二重ディスパッチパス**：サブエージェントディスパッチャー（`claude_runner::call_with_rotation`）とユーザー向けチャネル返信（`channel_reply::call_claude_cli_rotated` → `rotate_cli_spawn_with_pool`）の両方がrotatorを経由し、それぞれ応答するエージェントの `account_pool` を引き継ぎます。
- **`FailureReason` 分類**— RateLimited / Billing / Timeout / BinaryMissing / SpawnError / EmptyResponse / NoAccounts / Unknown。カテゴリごとに専用のzh-TWユーザー向けメッセージを表示し、`channel_failures.jsonl` に監査記録を残します。
- **バイナリ検出**：`which_claude()` / `which_claude_in_home()` がHomebrew（Intel + Apple Silicon）、Bun、Volta、npm-global、`.claude/bin`、`.local/bin`、asdf shims、NVMバージョンディレクトリを探索します。`PATH` が空の状態でlaunchdから起動されたgatewayがバイナリを発見できない問題を修正。
- **CostTelemetry**：SQLiteベースのトークン使用量トラッキングとキャッシュ効率分析（`cache_read / (input + cache_read + cache_creation)`）、200Kの価格崖警告、適応的ルーティング（キャッシュ効率<30% → ローカルへ）。MCPツール：`cost_summary`、`cost_agents`、`cost_recent`。
- **モデル別コスト集計**（`CostTelemetry::summary_by_model`）：`token_usage` には最初のスキーマから `model` 列がありましたが、集計はすべて agent / user / day 単位で、「どのモデルにお金が使われているか」には答えられませんでした。`summary_by_model(agent_id: Option<&str>, since_unix)` はモデル別にグループ化し（コストの高い順。モデルidが記録されていない行は推測せず `"(unknown)"` にまとめます）、`requests` / `input_tokens` / `output_tokens` / `cache_read_tokens` / `cache_creation_tokens` / `cost_millicents` + `cost_usd` / `cache_efficiency` を返します。コストは各行に保存済みの `cost_millicents` の合計で、他の集計と同じ単一の価格計算経路（`cost_for`、記録時に1回だけ適用）を使い、再計算はしません。`cost_usd` は単位換算のみです。公開は追加的に行います。MCP の `cost_summary` と `cost_agents` のレスポンスに同じ期間の `by_model` 配列が加わり（`cost_agents` の agent 行は、トップレベルのJSON配列には名前付きの兄弟フィールドを持たせられないため `agents` キーの下に移ります）、ダッシュボードRPC `cost.by_model`（パラメータ `agent_id?`、`days?`、デフォルト7、1〜365に制限）は他の `cost.*` と同じ admin ゲートの下で集計を返します。`cost_summary` / `cost_agents` 内で集計に失敗した場合は、呼び出し側が実際に行った呼び出しを失敗させず、空の `by_model` に縮退します。
- **Direct APIクライアント**（`direct_api.rs`）：純粋なチャットではClaude CLIを迂回し、system promptに `cache_control: ephemeral` を付与します（測定済みのヒット率は公表していません）。単一の `reqwest::Client`（タイムアウト120秒）を使用。全OAuthアカウントがクールダウン中のフォールバックとして利用。

### スケジューリング
- **HeartbeatScheduler**：エージェントごとの統一スケジューリング。busポーリング + GVUサイレンスブレーカー + cron、`max_concurrent_runs` セマフォで制御。
- **CronScheduler**：`cron_tasks.jsonl`（v1.8.12以降は `cron_tasks.db` も）を読み込み、cron式に従ってタスクを発火します。`list_cron_tasks` は全タスクを返します（v1.8.3以降、default_agentによる絞り込みは行いません）。スケジュールは `tasks_create` + `schedule` で作成します。旧来の `schedule_task` MCPツールは非推奨のエイリアスで、v1.69.0 で削除されます。
- **ReminderScheduler**：一回限りのリマインダー（相対時間 `5m`/`2h`/`1d` またはISO 8601）、`direct` 静的メッセージまたは `agent_callback` ウェイクアップモード。

### Skillエコシステム
- **6段階のライフサイクル**：Activation → Compression（3層の段階的ロード）→ Extraction → Distillation → Diagnosis → Gap Analysis。以前の Reconstruction 段階は呼び出し元がなく、2026-09 に削除されました。
- **GitHubライブインデックス**— Search API + 24時間のローカルキャッシュ + 加重検索。
- **Skill自動合成**（Phase 3-4）：gap accumulatorが繰り返し発生するドメインギャップを検出 → エピソード記憶からskillを合成（Voyagerに着想）→ TTL付きサンドボックス試行 → エージェント横断の卒業判定。デフォルトは無効（`agent.toml [evolution] skill_synthesis_enabled`）。MCPツール：`skill_security_scan`、`skill_graduate`、`skill_synthesis_status`。
- **Rustネイティブ Skillセキュリティスキャナー**（`skill_lifecycle::security_scanner`）— Pythonサブプロセス不要。dashboardの審査、MCPの `skill_security_scan` ツール、サンドボックス試行ゲートを支えます。

### タスクとナレッジ
- **Task Board**：SQLiteベースのタスク管理（状態/優先度/割り当てを追跡）+ リアルタイムActivity Feed WebSocket。ダッシュボード RPC：`tasks.list/create/update/remove/assign`、`activity.list`。エージェント向け MCP ツール：`tasks_list`、`tasks_create`、`tasks_update`、`tasks_claim`、`tasks_complete`、`tasks_block`、`activity_list`、`activity_post`。
- **共有ナレッジベース**：`~/.duduclaw/shared/wiki/`、Wikiの対象分類（agent/shared/both）に対応。MCPツール：`scope="shared"` を付けた `wiki_ls/read/write/search/stats/lint`（`shared_wiki_*` の表記は非推奨のエイリアスで、v1.69.0 で削除）、および `shared_wiki_delete` と `wiki_share`。
- **Autopilotルールエンジン**：委任/通知/skill実行の自動化。新しいルールが使えるトリガー（12種類）：`task_created`、`task_updated`、`task_status_changed`、`activity_new`、`channel_message`、`agent_idle`、`run_at_risk`、`os_file`、`os_frontmost`、`tick`、`security_event`、`odoo_event`。`cron_tick` は送出されず、v1.67.1 から作成時に拒否（[23-autopilot-engine](../../features/ja-JP/23-autopilot-engine.md)）。

### インテグレーション
- **Odoo ERPブリッジ**（`duduclaw-odoo` crate）：CE/EEに対応するJSON-RPCミドルウェア、17個のMCPツール（CRM/Sales/Inventory/Accounting）、EditionGate自動検出、イベントポーリング + `POST /webhook/odoo`（どちらもデフォルト無効）が `odoo_event` の autopilot ルールに流れます。`OdooConnectorPool` によるエージェントごとの認証情報分離（RFC-21 §2、v1.11.0）。Dashboardの保存前テスト：`odoo.test` RPCがインラインパラメータを受け付け（v1.13.1）、認証情報を省略すると保存済みシークレットにフォールバック。`odoo.configure` と同じSSRF/HTTPS/DB名バリデーターを使用。`scrub_odoo_error()` が接続エラーを240文字に切り詰め、HTML/URLの漏洩を防ぎます。
- **Prometheusメトリクス**：gateway HTTPの `GET /metrics`。フェイルオーバー、wiki の信頼度、意思決定の継続性、プロンプト圧縮、常駐センシング（`tick_*`）、goal loop、ライブフォークのカウンター。リクエスト／トークン／所要時間／セッション／チャネル／予算の系列は一度もインクリメントされておらず、v1.66 で削除されました。
- **RLトラジェクトリコレクター**：チャネルとのやり取り中、エージェントごとの軌跡を `~/.duduclaw/rl_trajectories.jsonl` に書き込みます。これをエクスポートしていた `duduclaw rl` CLI は 2026-09 に削除されました。
- **BroadcastLayer** tracing layerがリアルタイムログをWebSocket購読者にストリーミングします。
- **Dashboard WebSocketハートビート**：サーバーは30秒ごとにPingを送信し、Pongが60秒間なければアイドルソケットを切断します。クライアント側は25秒ごとにアプリケーションレベルの `ping` RPCを送信します（ブラウザは制御フレームを送出できないため）。

### 信頼性とガバナンス（v1.9.4）
- **`duduclaw-durability` crate**（🗑️ **2026-07-04 のコミット `b0639b96` で削除**）— このcrateは5本柱の耐久性フレームワーク（`idempotency`、`retry`、`circuit_breaker`、`checkpoint`、`dlq`）を含んでいました。本体コードベース全体で呼び出し元がないことが確認されたため、削除されました。gatewayのLLM fallbackチェーンは他のメカニズムを使用しています（`gateway/failover.rs`を参照）。チェックポイント保存/巻き戻し/フォークに関する過去の記述は、現在のコードベースでは利用できません。
- **`duduclaw-governance` crate**（🗑️ **`b0639b96` で削除**）— rate / permission / quota / lifecycle の各ポリシーには強制する側が存在せず、ダッシュボードの Governance ページとその RPC は v1.66 で削除されました。レート制限、委任ポリシー + MCP scope、ライセンスのクォータはそれぞれ別の仕組みで強制されます。
- **LLM fallbackチェーン**（`gateway/failover.rs`、モジュール `failover::model`）— 3層フェイルオーバー（アカウント → モデル → ランタイム、2026-09-29 より同一モジュールツリー）の第2層：プライマリのタイムアウト/503/429/overloadedで、より軽いフォールバックモデルへ自動的に切り替えます（課金エラーでは発動しません）。`is_llm_fallback_error` / `should_attempt_model_fallback` はユニットテスト付きの純粋関数で、`FailoverManager::model_fallback_for` はすべてのディスパッチ経路が呼ぶ統合判定です。`char_indices` によるUTF-8安全な切り詰め。
- **Evolution Eventsシステム**（`gateway/evolution_events/`）— 30種類以上のイベントスキーマ、非同期バッチ+リトライのエミッター、クエリインターフェース、信頼性の保証。gateway上でHTTPエンドポイントとして公開され、Webの `ReliabilityPage` に表示されます。

### メモリ評価（v1.9.4 / W21）
- **LOCOMO評価**（`python/duduclaw/memory_eval/`）— `retrieval_accuracy`、`retention_rate`、`locomo_integrity_check`。`cron_runner` は手動の CLI エントリポイント（`python -m memory_eval.cron_runner smoke_test|weekly_kpis|monthly_locomo`）で、リポジトリ内にこれをスケジュール実行するものはありません。5分間の `smoke_test` P0が基本的なメモリ機能を検証。`build_golden_qa.py` がゴールドQAセットを構築し、`data/golden_qa_set.jsonl` に最初の200件を収録。`duduclaw-memory` エンジンに評価用のバッチクエリAPIを追加。
- **Python `agents/` + `mcp/` モジュール**— `agents/capabilities/`（manifest + matcher）、`agents/routing/`（router + resolution + memory_resolver）。`mcp/auth/`（キーマスキング付きAPI Key）、`mcp/tools/memory/`（store / read / search / namespace / quota、`execute()` の入口で厳格なscope強制。v1.9.3の認証ギャップ（有効なAPI Keyであればscope制限を回避できていた問題）を修正）。

### Webダッシュボード
- 技術スタック：React 19 + TypeScript + Tailwind CSS 4 + Base UI + 共有の `mds` コンポーネントライブラリ。
- リアルタイムログストリーミング（BroadcastLayer → WebSocket）。
- OrgChart（D3.jsによるインタラクティブなエージェント階層図）。
- Memoryページ：Key Insightsタブ（access_countバッジ付き `key_facts` カード）+ 自己進化タブ（停滞の警告、却下の統計、playbook ルールカード）。
- Logsページ：ソースフィルターチップ + severityドロップダウン + severityで色分けした左ボーダー + JSON詳細展開。
- Toast通知システム（モジュールスコープのevent bus、最大5件キュー、暖色系バリアント）。
- Skill Market 3タブ（Marketplace / Shared Skills / My Skills）。
- Autopilot設定 + Session Replay + WikiGraph。
- **Reliability ページ**— evolution イベントの照会とエージェントごとの信頼性サマリー（`audit.evolutionQuery`、`audit.reliabilitySummary`）。
- i18n：zh-TW / en / ja-JP（600+ 翻訳キー）。
- Dark/Lightテーマ（システム追従 + 手動切り替え）。
