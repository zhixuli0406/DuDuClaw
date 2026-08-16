# DuDuClaw アーキテクチャ概要

## アーキテクチャ概要（v1.13.1）

DuDuClawは**マルチランタイム AI エージェントプラットフォーム（Multi-Runtime AI Agent Platform）**であり、統一された `AgentRuntime` トレイトを通じて**Claude Code / Codex / Gemini** CLIをAIバックエンドとしてサポートし、自動検出とエージェントごとの設定に対応しています。DuDuClawは単体のLLM製品ではなく、1つ（または複数）のAI CLIを、チャネルルーティング・セッション記憶・自己進化・マルチアカウントローテーション・ローカルLLM推論・ブラウザ自動化・IDE統合を備えた長時間稼働エージェントへと変える配管層です。

## 主要なアーキテクチャ上の決定

### ランタイムとトランスポート
- **Multi-Runtime**（`AgentRuntime` トレイト）— Claude / Codex / Gemini / OpenAI-compat の4バックエンド、`RuntimeRegistry` による自動検出、エージェントごとの設定は `agent.toml [runtime]` に記述。
- **MCP Server（stdio）**（`duduclaw mcp-server`）— stdin/stdout上のJSON-RPC 2.0を通じて、チャネル・メモリ・エージェント・skill・task・共有wiki・autopilotの各ツールをAI Runtimeに公開します。登録はエージェントレベルの `<agent>/.mcp.json`（v1.8.5でv1.8.4のグローバル登録を取り消しました。Claude CLIの `-p --dangerously-skip-permissions` はプロジェクトレベルの `.mcp.json` しか読み込まないため）。Gateway起動時に全エージェントの `.mcp.json` を自動作成・修復します。
- **MCP Server（HTTP/SSE）**（`duduclaw http-server --bind 127.0.0.1:8765`、v1.9.4）— Bearer認証の `POST /mcp/v1/call`（単発のJSON-RPCツール呼び出し）、`GET /mcp/v1/stream`（長時間接続のSSEイベントストリーム、Bearerまたは `?api_key=`）、`POST /mcp/v1/stream/call`（非同期＋SSE結果プッシュ）、`GET /healthz`（認証不要）。トークンバケット方式のレート制限（60 req/min）。`mcp_sse_store.rs` がbroadcastチャネルでSSE接続を管理します。外部HTTPクライアント向けにstdioを補完します。
- **ACP/A2A Server**（`duduclaw acp-server`）— stdio JSON-RPC 2.0ループで、`agent/discover`、`tasks/send`、`tasks/get`、`tasks/cancel` メソッドを提供し、`.well-known/agent.json` のAgentCardを出力します。Agent Client Protocolを通じてZed / JetBrains / NeovimなどIDE統合を可能にします。
- **エージェントディレクトリ**はClaude Codeと互換性があります。各ディレクトリには `.claude/`、`.mcp.json`、`SOUL.md`、`CLAUDE.md`、`CONTRACT.toml`、`agent.toml`、`wiki/`、`SKILLS/`、`memory/`、`tasks/`、`state/` が含まれます。

### チャネル（7種類 + 汎用Webhook）
- **Telegram**（ロングポーリング）— ファイル/写真/スタンプ/音声、フォーラム/トピック、メンション限定、Whisper文字起こし。
- **LINE**（webhook）— HMAC-SHA256署名、スタンプカタログ、チャットごとの設定。
- **Discord**（Gateway WebSocket）— `tokio::select!` によるハートビート、スラッシュコマンド、自動スレッド、ボイスチャンネル（Songbird）。v1.9.2で強化：本物のop 6 RESUME（`session_id` + `resume_gateway_url` + シーケンス番号を永続化）、スタール監視（ハートビート間隔の2倍を超えて通信がなければ切断）、ハートビートチャネルの容量を1→16にし `try_send` へ変更、op 9に1〜5秒のジッターを追加、RESUMEDディスパッチの処理、backoff上限を60秒に短縮。
- **Slack**（Socket Mode）、**WhatsApp**（Cloud API）、**Feishu**（Open Platform v2）、**WebChat**（`/ws/chat` + Reactフロントエンド）。
- **汎用Webhook**：`POST /webhook/{agent_id}` + HMAC-SHA256。
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
- **Direct APIのpromptキャッシュ**（"system_and_3" ブレークポイント戦略、マルチターンで約75%のキャッシュヒット率、純粋なsystem-promptキャッシュでは95%以上）。
- **圧縮サマリー**は50kトークンの閾値でsystem prompt（会話ターンではなく）に注入されます。
- **Instruction Pinning**（v1.8.6 P0）— ユーザーの最初のターン → 非同期でHaikuがコアタスクを抽出 → `sessions.pinned_instructions` に保存 → system promptの末尾に注入（U字型の注意分布を利用）。明確化の回答は蓄積されます（≤1000文字）。
- **Snowball Recap**（v1.8.6 P0）— 各ターンでユーザーメッセージの前に `<task_recap>` を付加。LLMコストはゼロ。
- **P2 Key-Fact Accumulator**（v1.8.6）— 実質的な内容のあるターンごとに、Haikuが2〜4件の重要事実を抽出 → FTS5付きの `key_facts` テーブルに保存 → 最も関連性の高い上位3件をsystem promptに注入。約100〜150トークンで、MemGPTの6,500トークン（−87%）と比較して大幅に削減。
- **CLI軽量パス**— `call_claude_cli_lightweight()` に `--effort medium --max-turns 1 --no-session-persistence --tools ""` を付与し、メタデータタスクに使用。コストを25〜40%削減。
- **安定化フラグ**— `--strict-mcp-config`（MCP分離）+ `--exclude-dynamic-system-prompt-sections`（ターンをまたいだprompt安定性、トークンを10〜15%削減）。`--bare` はv1.8.11で削除されました（OSキーチェーンの認証情報検索を壊していたため）。

### 進化
- **予測駆動エンジン**：Active Inference + Dual Process Theory、約90%の会話でLLMコストゼロ。無視できる/中程度の誤差 → コストゼロ、有意な誤差 → GVUリフレクションを起動、重大な誤差 → 緊急GVUループを起動。
- **MetaCognition**：100回の予測ごとに誤差閾値を自己校正、Adaptive Depth（GVU 3〜7ラウンド）を駆動。
- **GVU²自己対戦ループ**（Generator→Verifier→Updater）：TextGradフィードバック、4+2層の検証（L1-Format / L2-Metrics / L2.5-MistakeRegression / L3-LLMJudge / L3.5-SandboxCanary / L4-Safety）。**Evolution v3以降は非デフォルトのlegacyパス**（`agent.toml [evolution] legacy_soul_evolution = true`）— 詳細は後述のAEEを参照。
- **Deferred GVU**：勾配の蓄積＋遅延リトライ（最大3回延期、72時間の範囲、実効9〜21ラウンドに相当）。
- **MistakeNotebook**：ループを横断するエラー記憶で退行を防止。エントリには決定的な `TrajectoryEvidence`（どのツール/アサーションが失敗したか）が付与され、リフレクションの統合が未検証の自己申告診断を鵜呑みにしなくなりました（Evolution v3）。
- **SOUL.mdバージョン管理**：24時間の観察期間＋自動ロールバック、書き込みはアトミック（一時ファイル＋rename）でSHA-256フィンガープリント付き。この仕組みは上記のlegacy GVUパスに適用されます。**Evolution v3以降 `SOUL.md` はエージェントに対してデフォルトで読み取り専用**です（オペレーター/ダッシュボードのみ書き込み可）。
- **AEE（Agentic Evolution Engine、v3のデフォルト）**：デフォルトの進化対象は `SOUL.md` から**playbook**へと変わりました。カテゴリ／シグナル／eval caseに紐づく、小さく個別に廃止可能な遺伝子状のエントリの集合で、Gate（決定的、拒否権を保持）とMeasure（スコアリング、拒否権なし）の分離、champion＋現状維持か改善のみ許可するコミットゲート（matches-or-improves）、そしてファイル全体ではなくエントリ単位の観察期間を通じて進化します。詳細は `evolution-engine.md` 第12章と `../../features/ja-JP/38-aee-playbook-evolution.md` を参照。
- **Agent-as-Evaluator**：独立したEvaluator Agent（コスト管理のためHaikuを使用）が、構造化されたJSON判定によるアドバーサリアル検証を行います。
- **ConversationOutcome**：LLMコストゼロで会話結果を検出（TaskType / Satisfaction / Completion）、zh-TW + en の両言語に対応。
- **外部要因**：ユーザーフィードバック、セキュリティイベント、チャネル指標、Odooのビジネスコンテキスト、他エージェントからのシグナルが予測エンジンとGVUリフレクションに反映されます。

### Wikiナレッジレイヤー（v1.8.9）
- **4層アーキテクチャ**（Vault-for-LLMに着想）：L0 Identity / L1 Core / L2 Context / L3 Deep。
- **信頼度重み付け**（frontmatterの `trust`、0.0〜1.0）— 検索結果は信頼度加重スコアで順位付けされます。
- **自動注入**：`build_system_prompt()` がL0+L1ページを自動的にWIKI_CONTEXTへ注入します。CLI／チャネル返信／dispatcherの各パスに対応し、Claude / Codex / Gemini / OpenAIの各ランタイムで統一されています。
- **FTS5インデックス**（`unicode61` トークナイザー）— 書き込み/削除のたびに自動同期、`wiki_rebuild_fts` で手動再構築も可能。
- **ナレッジグラフ**：`wiki_graph` MCPツールがBFS深度を制限したMermaid図をエクスポート。ノードの形状はレイヤーごとに異なります。
- **重複検出**：`wiki_dedup` はタイトル一致＋タグのJaccard類似度（≥0.8）で重複ページを検出します。
- **逆引きbacklinkインデックス**：`related` frontmatterと本文中のmarkdownリンクをスキャンし、双方向のマッピングを構築します。
- **検索フィルター**：`wiki_search` / `shared_wiki_search` は `min_trust`、`layer`、`expand`（1ホップのbacklink展開）に対応。
- **Shared Wiki**：`~/.duduclaw/shared/wiki/` にエージェント横断のSOP・ポリシー・製品仕様を格納。可視性は `wiki_visible_to` capabilityで制御。

### メモリシステム
- **認知メモリ**（オプション）：`SqliteMemoryEngine`、エピソード記憶と意味記憶を分離し、Generative Agentsの3軸重み付け検索（Recency × Importance × Relevance）を採用。
- **メモリ減衰の日次スケジューラー**：バックグラウンドタスクが24時間ごとに `duduclaw_memory::decay::run_decay` を実行。重要度が低く30日経過 → アーカイブ。アーカイブ済みで90日経過 → 完全削除。
- **認知メモリMCPツール**：`memory_search_by_layer`（エピソード/意味フィルター）、`memory_successful_conversations`、`memory_episodic_pressure`、`memory_consolidation_status`。
- **MemGPT 3層システム**（Core Memory、Recall Memory、Archival Bridge、Budget Manager、Consolidation Pipeline、MCPツール6個）は**v1.8.1で削除**されました（−1,985行）— このプロンプト注入方式はプロンプトごとに6,500トークンも肥大化させ、「lost in the middle」による注意力の劣化を引き起こしていました。

### Worktree分離（v1.6.0）
- **Git worktree L0分離レイヤー**— タスクごとのファイルシステム分離で、コンテナサンドボックスより低コスト。
- **WorktreeManager**：create / remove / list / cleanup_staleのライフサイクル管理。
- **アトミックマージ**：dry-runによる事前チェック → abort → クリーンな場合のみ実マージ。グローバル `Mutex` で保護。
- **Snapワークフロー**：create → execute → inspect → merge/cleanup（判定ロジックは純粋関数で実装し、テスト容易性を確保）。
- **ブランチ命名**：`wt/{agent_id}/{adjective}-{noun}`、50×50の単語リストから生成。
- **copy_env_files**：パストラバーサル対策（jail）+ symlink拒否 + 1MBサイズ上限。
- **リソース上限**：エージェントごとに最大5個のworktree、全体で最大20個。

### ローカル推論
- **統一 `InferenceBackend` トレイト**（`duduclaw-inference` crate）：llama.cpp（Metal/CUDA/Vulkan/CPU）、mistral.rs（ISQ + PagedAttention + Speculative Decoding）、OpenAI互換HTTP（Exo/llamafile/vLLM/SGLang）。
- **Confidence Router**：LocalFast / LocalStrong / CloudAPIの3段階ルーティング、CJKを考慮したトークン推定。
- **InferenceManager**：自動切り替えのステートマシン——Exo P2P → llamafile → Direct backend → OpenAI-compat → Cloud API。
- **Exo P2Pクラスター**（`exo_cluster.rs`）：分散推論、複数マシンをまたいで235B超のモデルを実行可能。
- **llamafile manager**：サブプロセスのライフサイクル管理、ヘルスモニタリング、localhostでOpenAI互換APIを提供。
- **MLX bridge**：Apple Silicon上でPythonサブプロセスから `mlx_lm` を呼び出し、ローカルリフレクション + LoRAに利用。
- **MCPツール**：`model_list`、`model_load`、`model_unload`、`inference_status`、`hardware_info`、`route_query`、`inference_mode`、`llamafile_start/stop/list`、`compress_text`、`decompress_text`。

### トークン圧縮
- **Meta-Token（LTSC）**— Rustネイティブの可逆・BPE類似アルゴリズム、構造化入力で27〜47%の圧縮率。
- **LLMLingua-2**— Microsoftによるトークン重要度に基づく枝刈り、2〜5倍の非可逆圧縮。
- **StreamingLLM**— attention sink + スライディングウィンドウKVキャッシュ。
- **戦略セレクター**：`compress_text` は `strategy` パラメータ（meta_token / llmlingua / streaming_llm / auto）を受け付けます。

### 音声パイプライン
- **ASR**：Whisper.cpp（ローカル）/ SenseVoice ONNX（ローカル）/ OpenAI Whisper API / Deepgram（ストリーミング）。
- **TTS**：Piper ONNX（ローカル）/ MiniMax T2A / Edge TTS / OpenAI TTS。
- **VAD**：Silero ONNX。
- **音声デコード**：symphonia（OGG Opus、MP3、AAC、WAV、FLAC → PCM）。
- **Discord Voice**（Songbird）+ **LiveKit** マルチエージェント音声ルーム。
- **ONNX Embedding**：BERT WordPieceトークナイザー + ONNX Runtimeベクトル埋め込み。

### セキュリティ
- **Claude Codeセキュリティhooks**（`.claude/hooks/`）：3段階の漸進的防御——Layer 1 決定的ブラックリスト（<50ms）、Layer 2 難読化/情報流出検出（YELLOW以上）、Layer 3 HaikuによるAI判断（RED時のみ）。
- **脅威レベルステートマシン**：GREEN → YELLOW → RED、自動エスカレーション/降格（24時間イベントなしで1段階降格）。
- **SOUL.mdドリフト検出**（SHA-256フィンガープリント）。
- **Prompt injectionスキャナー**（6種類のルールカテゴリ + XML区切り文字による保護）。
- **機密情報漏洩スキャナー**— 20種類以上のパターン（Anthropic/OpenAI/AWS/GitHub/Slack/Stripe/DB URL）。
- **CONTRACT.toml**— `must_not` / `must_always` の境界ルール、system promptに自動注入。`duduclaw test` レッドチームCLI（組み込み9シナリオ）。
- **統一マルチソース監査ログ**：`audit.unified_log` が `security_audit.jsonl` / `tool_calls.jsonl` / `channel_failures.jsonl` / `feedback.jsonl` を共通のエンベロープ（timestamp / source / event_type / agent_id / severity / summary / details）にマージし、Logsページのフィルターチップで絞り込めます。
- 保存時は**AES-256-GCM**— エージェントごとに鍵を分離。
- **Ed25519 challenge-response** によるWebSocket認証。
- **コンテナサンドボックス**（Docker / Apple Container / WSL2）— `--network=none`、tmpfs、読み取り専用rootfs、512MB上限。
- **ブラウザ自動化**（5層自動ルーティング）：L1 API Fetch → L2 Static Scrape → L3 Headless → L4 Sandbox Container → L5 Computer Use。`CapabilitiesConfig` によりデフォルト拒否。`bash-gate.sh` がLayer 1.5としてPlaywright/Puppeteerをホワイトリスト許可。
- **CJK安全なバイトスライス**：`duduclaw_core::truncate_bytes` / `truncate_chars` が31箇所の安全でない `s[..s.len().min(N)]` を置き換え（v1.8.11のマルチバイトコードポイントpanicを修正）。

### アカウントとコスト
- **エージェントごとのモデルルーティング**（SDKファースト）：`agent.toml [model]`— `preferred`（Claude SDKモデル）、`local.model`、`local.use_router`、`api_mode`（cli/direct/auto）、`account_pool`（後述）。
- **マルチOAuthアカウントローテーション**：OAuthセッション（Claude Pro/Team/Max、`claude auth status` 経由。`setup-token` アカウントは `CLAUDE_CODE_OAUTH_TOKEN`）+ APIキー。4種類の戦略（Priority/LeastCost/Failover/RoundRobin）。レート制限クールダウン（2分）、課金枯渇クールダウン（24時間）、予算強制、トークン有効期限追跡（30日/7日前の警告）。
- **エージェントごとのアカウントプール**（`agent.toml [model] account_pool`）：そのエージェントが使用できるローテーション対象アカウントを制限します。適用対象は**候補集合**——provider/health/cooldown/budgetのフィルターの後、戦略実行の前——なので、4つの戦略はすべて絞り込んだ集合上でも意味論をそのまま保ちます。エントリはアカウントの `id` **または**dashboardの `label`（完全一致、トリム済み、ASCII大文字小文字を区別しない。部分一致は不可）と照合されます。**Fail-open**：プールが*利用可能な*アカウントに一件も一致しない場合（idが古い、全アカウントがクールダウン中など）は `warn` を記録し、フルセットにフォールバックします。古くなったプールがエージェントをアカウントなしの状態にすることは絶対にありません。未設定/空 ⇒ ローテーションの挙動は変わりません。エントリポイント：`AccountRotator::select_with_pool` / `select_for_provider_with_pool`。
- **二重ディスパッチパス**：サブエージェントディスパッチャー（`claude_runner::call_with_rotation`）とユーザー向けチャネル返信（`channel_reply::call_claude_cli_rotated` → `rotate_cli_spawn_with_pool`）の両方がrotatorを経由し、それぞれ応答するエージェントの `account_pool` を引き継ぎます。
- **`FailureReason` 分類**— RateLimited / Billing / Timeout / BinaryMissing / SpawnError / EmptyResponse / NoAccounts / Unknown。カテゴリごとに専用のzh-TWユーザー向けメッセージを表示し、`channel_failures.jsonl` に監査記録を残します。
- **バイナリ検出**：`which_claude()` / `which_claude_in_home()` がHomebrew（Intel + Apple Silicon）、Bun、Volta、npm-global、`.claude/bin`、`.local/bin`、asdf shims、NVMバージョンディレクトリを探索します。`PATH` が空の状態でlaunchdから起動されたgatewayがバイナリを発見できない問題を修正。
- **CostTelemetry**：SQLiteベースのトークン使用量トラッキングとキャッシュ効率分析（`cache_read / (input + cache_read + cache_creation)`）、200Kの価格崖警告、適応的ルーティング（キャッシュ効率<30% → ローカルへ）。MCPツール：`cost_summary`、`cost_agents`、`cost_recent`。
- **Direct APIクライアント**（`direct_api.rs`）：純粋なチャットではClaude CLIを迂回し、system promptに `cache_control: ephemeral` を付与 → キャッシュヒット率95%以上。単一の `reqwest::Client`（タイムアウト120秒）を使用。全OAuthアカウントがクールダウン中のフォールバックとして利用。

### スケジューリング
- **HeartbeatScheduler**：エージェントごとの統一スケジューリング——busポーリング + GVUサイレンスブレーカー + cron、`max_concurrent_runs` セマフォで制御。
- **CronScheduler**：`cron_tasks.jsonl`（v1.8.12以降は `cron_tasks.db` も）を読み込み、cron式に従ってタスクを発火します。`list_cron_tasks` は全タスクを返します（v1.8.3以降、default_agentによる絞り込みは行いません）。`schedule_task` MCPツールのスキーマはv1.8.12で修正され、`agent_id` と `name` フィールドが追加されました。
- **ReminderScheduler**：一回限りのリマインダー（相対時間 `5m`/`2h`/`1d` またはISO 8601）、`direct` 静的メッセージまたは `agent_callback` ウェイクアップモード。

### Skillエコシステム
- **7段階のライフサイクル**：Activation → Compression → Extraction → Reconstruction → Distillation → Diagnostician → Gap Analysis。
- **GitHubライブインデックス**— Search API + 24時間のローカルキャッシュ + 加重検索。
- **Skill自動合成**（Phase 3-4）：gap accumulatorが繰り返し発生するドメインギャップを検出 → エピソード記憶からskillを合成（Voyagerに着想）→ TTL付きサンドボックス試行 → エージェント横断の卒業判定。MCPツール：`skill_security_scan`、`skill_graduate`、`skill_synthesis_status`。
- **Rustネイティブ Skillセキュリティスキャナー**（`skill_lifecycle::security_scanner`）— Pythonサブプロセス不要。dashboardの審査、MCPの `skill_security_scan` ツール、サンドボックス試行ゲートを支えます。

### タスクとナレッジ
- **Task Board**：SQLiteベースのタスク管理（状態/優先度/割り当てを追跡）+ リアルタイムActivity Feed WebSocket。MCPツール：`tasks.list/create/update/assign`、`activity.list/subscribe`。
- **共有ナレッジベース**：`~/.duduclaw/shared/wiki/`、Wikiの対象分類（agent/shared/both）に対応。MCPツール：`shared_wiki_ls/read/write/search/delete/stats`、`wiki_share`。
- **Autopilotルールエンジン**：委任/通知/skill実行の自動化。トリガー：タスク作成、状態変化、チャネルメッセージ、アイドル検出、cronスケジュール。

### インテグレーション
- **Odoo ERPブリッジ**（`duduclaw-odoo` crate）：CE/EEに対応するJSON-RPCミドルウェア、15個のMCPツール（CRM/Sales/Inventory/Accounting）、EditionGate自動検出、イベントポーリング + webhook。`OdooConnectorPool` によるエージェントごとの認証情報分離（RFC-21 §2、v1.11.0）。Dashboardの保存前テスト：`odoo.test` RPCがインラインパラメータを受け付け（v1.13.1）— 認証情報を省略すると保存済みシークレットにフォールバック。`odoo.configure` と同じSSRF/HTTPS/DB名バリデーターを使用。`scrub_odoo_error()` が接続エラーを240文字に切り詰め、HTML/URLの漏洩を防ぎます。
- **Prometheusメトリクス**：gateway HTTPの `GET /metrics`— リクエスト数、トークン数、所要時間ヒストグラム、チャネル状態。
- **RLトラジェクトリコレクター**：チャネルとのやり取り中、エージェントごとの軌跡を `~/.duduclaw/rl_trajectories.jsonl` に書き込みます。`duduclaw rl export|stats|reward` CLI（複合報酬：outcome × 0.7 + efficiency × 0.2 + overlong × 0.1）。
- **BroadcastLayer** tracing layerがリアルタイムログをWebSocket購読者にストリーミングします。
- **Dashboard WebSocketハートビート**：サーバーは30秒ごとにPingを送信し、Pongが60秒間なければアイドルソケットを切断します。クライアント側は25秒ごとにアプリケーションレベルの `ping` RPCを送信します（ブラウザは制御フレームを送出できないため）。

### 信頼性とガバナンス（v1.9.4）
- **`duduclaw-durability` crate**— 5本柱の耐久性：
  - `idempotency.rs`：キーベースの重複排除で二重実行を防止。
  - `retry.rs`：ジッター付き指数バックオフ戦略。
  - `circuit_breaker.rs`：Closed / Open / HalfOpenの3状態、`probe_inflight` によるカウント（v1.9.4での修正：OPEN→HALF_OPEN遷移時に `probe_inflight` をインクリメントし、ゴーストプローブの超過を防止）。
  - `checkpoint.rs`：再開可能なタスク進捗。
  - `dlq.rs`：最終的に失敗したメッセージ用のDead Letter Queue。
- **`duduclaw-governance` crate**（W19-P1 M1-A）— `PolicyRegistry`（YAML + ホットリロード + エージェント優先度マージ + fail-safe + 並行upsertの安全性）、4種類の `PolicyType`（Rate / Permission / Quota / Lifecycle）、`quota_manager.rs`（エージェントごと/ポリシーごとのソフト・ハードクォータ）、`error_codes.rs`（QUOTA_EXCEEDED / POLICY_DENIED / ...）、承認ワークフロー + 監査ログ。デフォルトのポリシーセットは `policies/global.yaml`。
- **LLM fallbackチェーン**（`gateway/llm_fallback.rs`）— プライマリのタイムアウト/503/429/overloadedで自動的にフォールバックモデルへ切り替え。`is_llm_fallback_error` / `should_attempt_model_fallback` はユニットテスト付きの純粋関数です。`char_indices` によるUTF-8安全な切り詰め。
- **Evolution Eventsシステム**（`gateway/evolution_events/`）— 30種類以上のイベントスキーマ、非同期バッチ+リトライのエミッター、クエリインターフェース、信頼性の保証。gateway上でHTTPエンドポイントとして公開され、Webの `ReliabilityPage` に表示されます。

### メモリ評価（v1.9.4 / W21）
- **LOCOMO評価**（`python/duduclaw/memory_eval/`）— `retrieval_accuracy`、`retention_rate`、`locomo_integrity_check`。`cron_runner` が毎日UTC 03:00にトリガーされます。5分間の `smoke_test` P0が基本的なメモリ機能を検証。`build_golden_qa.py` がゴールドQAセットを構築し、`data/golden_qa_set.jsonl` に最初の200件を収録。`duduclaw-memory` エンジンに評価用のバッチクエリAPIを追加。
- **Python `agents/` + `mcp/` モジュール**— `agents/capabilities/`（manifest + matcher）、`agents/routing/`（router + resolution + memory_resolver）。`mcp/auth/`（キーマスキング付きAPI Key）、`mcp/tools/memory/`（store / read / search / namespace / quota、`execute()` の入口で厳格なscope強制——v1.9.3の認証ギャップ（有効なAPI Keyであればscope制限を回避できていた問題）を修正）。

### Webダッシュボード（24ページ）
- 技術スタック：React 19 + TypeScript + Tailwind CSS 4 + shadcn/ui、暖色系アンバーテーマ。
- リアルタイムログストリーミング（BroadcastLayer → WebSocket）。
- OrgChart（D3.jsによるインタラクティブなエージェント階層図）。
- Memoryページ：Key Insightsタブ（access_countバッジ付き `key_facts` カード）+ Evolutionタブ（前後の差分付きSOUL.mdバージョン履歴）。
- Logsページ：ソースフィルターチップ + severityドロップダウン + severityで色分けした左ボーダー + JSON詳細展開。
- Toast通知システム（モジュールスコープのevent bus、最大5件キュー、暖色系バリアント）。
- Skill Market 3タブ（Marketplace / Shared Skills / My Skills）。
- Autopilot設定 + Session Replay + WikiGraph。
- **ReliabilityPage**（v1.9.4、`/reliability` ルート）— circuit breaker状態、リトライ統計、DLQ深度ダッシュボード。`getEvolutionEvents` / `getReliabilityStats` / `getDlqItems` を呼び出します。
- i18n：zh-TW / en / ja-JP（600+ 翻訳キー）。
- Dark/Lightテーマ（システム追従 + 手動切り替え）。
