# DuDuClaw 全機能一覧

> v1.8.14 | 最終更新：2026-04-21

---

## コアアーキテクチャ

| 機能 | 説明 |
|------|------|
| マルチランタイム AI エージェントプラットフォーム | 統一 `AgentRuntime` trait — Claude / Codex / Gemini / OpenAI-compat 4 バックエンド自動検出 |
| MCP Server（JSON-RPC 2.0）| stdin/stdout 経由で AI Runtime に 80+ ツールを公開。`<agent>/.mcp.json` に登録（Claude CLI `-p` はプロジェクトレベルのみ読取）、起動時に自動生成/修復 |
| ACP/A2A Server | `duduclaw acp-server` — stdio JSON-RPC 2.0（`agent/discover` / `tasks/send` / `tasks/get` / `tasks/cancel`）、`.well-known/agent.json` AgentCard、Zed / JetBrains / Neovim IDE 統合 |
| エージェントディレクトリ構造 | `.claude/`, `.mcp.json`, `SOUL.md`, `CLAUDE.md`, `CONTRACT.toml`, `agent.toml`, `wiki/`, `SKILLS/`, `memory/`, `tasks/`, `state/` |
| サブエージェントオーケストレーション | `create_agent` / `spawn_agent` / `list_agents` + `reports_to` 階層 + D3.js OrgChart + 「## Your Team」自動注入 |
| DelegationEnvelope | 構造化受け渡しプロトコル — context / constraints / task_chain / expected_output |
| TaskSpec ワークフロー | 多段階タスク計画 — 依存認識スケジューリング、自動再試行（3x）、再計画（2x）、永続化 |
| 長返信ページング | チャネル byte budget を超えるサブエージェント返信を `channel_format::split_text` で分割、`📨 **agent** 的回報 (1/N)` ラベル |
| 孤立レスポンス復旧 | dispatcher 起動時 `reconcile_orphan_responses` が `bus_queue.jsonl` の crash/Ctrl+C/hotswap 残留 callback を原子的に再生 |
| ファイルベース IPC | `bus_queue.jsonl` によるエージェント間委任、最大 5 ホップ追跡 |
| Per-Agent Channel Token | `get_agent_channel_token` が per-agent `bot_token_enc` を優先読取（Discord スレッド間 bot の 401 を修正）|

## マルチランタイム

| 機能 | 説明 |
|------|------|
| Claude Runtime | Claude Code SDK (`claude` CLI) + JSONL ストリーミング + `--resume` ネイティブマルチターン |
| Codex Runtime | OpenAI Codex CLI + `--json` ストリーミング、`AGENTS.md` で system prompt を渡す |
| Gemini Runtime | Google Gemini CLI + `--output-format stream-json`、`GEMINI_SYSTEM_MD` env で system prompt、`--approval-mode yolo` |
| OpenAI 互換 Runtime | HTTP エンドポイント（MiniMax / DeepSeek 等）REST API |
| RuntimeRegistry | インストール済み CLI の自動検出、per-agent `[runtime]` 設定 |
| クロスプロバイダーフェイルオーバー | `FailoverManager` ヘルス追跡、クールダウン、再試行不可エラー検出 |

## セッションメモリスタック（v1.8.1 + v1.8.6）

| 機能 | 説明 |
|------|------|
| ネイティブマルチターン | Claude CLI `--resume` + SHA-256 決定論的 session ID + history-in-prompt フォールバック |
| Hermes 流ターントリミング | >800 chars → 先頭 300 + 末尾 200 + `[trimmed N chars]`、CJK セーフ文字スライス |
| プロンプトキャッシュ戦略 | Direct API "system_and_3" ブレークポイント、マルチターン ~75% ヒット率 |
| 圧縮サマリー注入 | 圧縮後サマリー（role=system）を system prompt に注入、会話ターンではない |
| Instruction Pinning | 最初のユーザーメッセージ → 非同期 Haiku 抽出 → `sessions.pinned_instructions` → system prompt 末尾（U字型注意） |
| Snowball Recap | 各ターンの user message 先頭に `<task_recap>` を付加、LLM コストゼロ |
| Clarification 累積 | エージェントの質問 + ユーザー回答 → pinned instructions に追加（≤1000 文字） |
| P2 Key-Fact Accumulator | 実質的なターン毎に 2-4 事実 → `key_facts` FTS5 テーブル → top-3 注入（~100-150 tokens vs MemGPT 6,500、−87%） |
| CLI 軽量パス | `call_claude_cli_lightweight()` — 25-40% コスト削減 |
| 安定化フラグ | `--strict-mcp-config` + `--exclude-dynamic-system-prompt-sections`（10-15% token 削減）；`--bare` は v1.8.11 で削除（OAuth キーチェーンを破壊） |
| CJK セーフ文字列スライス | `duduclaw_core::truncate_bytes` / `truncate_chars` が 31 箇所の unsafe byte-index スライスを置換 |

## 通信チャネル（7種）

| チャネル | プロトコル |
|----------|------------|
| Telegram | ロングポーリング、ファイル/写真/スタンプ/音声、forums/topics、mention-only、音声転記 |
| LINE | Webhook + HMAC-SHA256、スタンプ、per-chat 設定 |
| Discord | Gateway WebSocket、スラッシュコマンド、ボイスチャンネル（Songbird）、auto-thread（v1.8.14 で thread session id 漂流修正）、embed 返信 |
| Slack | Socket Mode、mention-only、thread 返信 |
| WhatsApp | Cloud API |
| Feishu | Open Platform v2 |
| WebChat | 組み込み `/ws/chat` WebSocket + React フロントエンド |
| チャネルホットスタート/ストップ | Dashboard 駆動の動的起動/停止 |
| Generic Webhook | `POST /webhook/{agent_id}` + HMAC-SHA256 署名検証 |
| メディアパイプライン | 自動リサイズ（max 1568px）+ MIME 検出 + Vision 統合 |
| スタンプシステム | LINE スタンプカタログ + 感情検出 + Discord emoji 対応 |
| チャネル失敗追跡 | `channel_failures.jsonl` + `FailureReason` 列挙 |

## 進化システム

| 機能 | 説明 |
|------|------|
| 予測駆動エンジン | Active Inference + Dual Process Theory、約 90% LLM コストゼロ |
| デュアルプロセスルーター | System 1（ルール）/ System 2（LLM リフレクション） |
| GVU² デュアルループ | 外側ループ（Behavioral GVU — SOUL.md）+ 内側ループ（Task GVU — 即時再試行） |
| 4+2 層検証 | L1-Format / L2-Metrics / L2.5-MistakeRegression / L3-LLMJudge / L3.5-SandboxCanary / L4-Safety |
| MistakeNotebook | ループ間エラー記憶 — 失敗パターン記録、退行防止 |
| SOUL.md バージョン管理 | 24h 観察期間 + アトミックロールバック + SHA-256 フィンガープリント |
| MetaCognition | 100 予測毎に誤差閾値を自己校正 |
| Adaptive Depth | MetaCognition 駆動の GVU 反復深度（3-7 ラウンド） |
| Deferred GVU | 勾配累積 + 遅延再試行（最大 3 deferral、72h、9-21 実効ラウンド） |
| ConversationOutcome | LLM ゼロの会話結果検出、zh-TW + en |
| Agent-as-Evaluator | 独立 Evaluator Agent（Haiku コスト管理）による対抗的検証 |
| Orchestrator テンプレート | 5 ステップ計画（Analyze → Decompose → Delegate → Evaluate → Synthesize）|

## Wiki 知識レイヤー（v1.8.9）

| 機能 | 説明 |
|------|------|
| 4 層アーキテクチャ | L0 Identity / L1 Core / L2 Context / L3 Deep — Vault-for-LLM 由来 |
| 信頼度加重 | `trust` (0.0-1.0) frontmatter；検索は trust-weighted score でランク |
| 自動注入 | `build_system_prompt()` が CLI / channel / dispatcher 3 パスで L0+L1 を WIKI_CONTEXT に注入 |
| FTS5 全文索引 | SQLite `unicode61` tokenizer（CJK 対応）、書込/削除で自動同期、`wiki_rebuild_fts` で手動再構築 |
| 知識グラフ | `wiki_graph` MCP ツールが BFS 制限付き Mermaid 図を出力、レイヤー別ノード形状 |
| Dedup 検出 | `wiki_dedup` — タイトルマッチ + タグ Jaccard 類似度（≥0.8） |
| 逆 backlink 索引 | `related` frontmatter + body markdown リンクをスキャン |
| 検索フィルタ | `min_trust` / `layer` / `expand`（1-hop related/backlink 拡張） |
| 共有 Wiki | `~/.duduclaw/shared/wiki/` — 組織横断 SOP/ポリシー/仕様、`wiki_visible_to` 可視性制御 |
| CLAUDE_WIKI テンプレート | 新規エージェント作成時に CLAUDE.md へ同梱、LLM に wiki MCP ツールの使い方を教示 |

## スキルエコシステム

| 機能 | 説明 |
|------|------|
| 7 段階ライフサイクル | Activation → Compression → Extraction → Reconstruction → Distillation → Diagnostician → Gap Analysis |
| GitHub ライブインデックス | Search API + 24h ローカルキャッシュ + 加重検索 |
| スキルマーケット | Web ダッシュボード、インストール、セキュリティスキャン |
| スキル自動合成 | ギャップアキュムレーター → エピソード記憶から合成（Voyager 由来）→ サンドボックス試用（TTL） → クロスエージェント卒業 |
| Python Skill Vetter | サブプロセスでの候補スキルのセキュリティスキャン |

## ローカル推論エンジン

| 機能 | 説明 |
|------|------|
| llama.cpp | Metal/CUDA/Vulkan/CPU（`llama-cpp-2` crate） |
| mistral.rs | Rust ネイティブ、ISQ、PagedAttention、Speculative Decoding |
| OpenAI 互換 HTTP | Exo/llamafile/vLLM/SGLang |
| 信頼度ルーター | LocalFast / LocalStrong / CloudAPI 3 層 + CJK-aware トークン推定 |
| InferenceManager | マルチモード自動切替：Exo P2P → llamafile → Direct → OpenAI-compat → Cloud API |
| llamafile マネージャ | サブプロセスライフサイクル、6 OS でゼロインストール |
| Exo P2P クラスタ | 分散推論、235B+ モデルを複数マシンで実行 |
| MLX Bridge | Apple Silicon の `mlx_lm` + LoRA ローカルリフレクション |
| モデル管理 | `model_search`（HuggingFace）/ `model_download`（resume + mirror）/ `model_recommend`（ハードウェア認識） |

## 圧縮エンジン

| 機能 | 説明 |
|------|------|
| Meta-Token（LTSC） | Rust ネイティブ無損失 BPE-like、構造化入力で 27-47% 圧縮率 |
| LLMLingua-2 | Microsoft トークン重要度剪定、2-5x 損失圧縮 |
| StreamingLLM | Attention sink + スライディングウィンドウ KV-cache、無限長会話 |
| 戦略セレクタ | `compress_text` の `strategy` パラメータ — `meta_token` / `llmlingua` / `streaming_llm` / `auto` |

## 音声パイプライン

| 機能 | 説明 |
|------|------|
| ASR | Whisper.cpp（ローカル）/ SenseVoice ONNX（ローカル）/ OpenAI Whisper API / Deepgram（ストリーミング） |
| TTS | Piper ONNX（ローカル）/ MiniMax T2A / Edge TTS / OpenAI TTS |
| VAD | Silero ONNX |
| オーディオデコード | symphonia：OGG Opus / MP3 / AAC / WAV / FLAC → PCM |
| Discord Voice | Songbird 統合 |
| LiveKit | WebRTC マルチエージェント音声ルーム |
| ONNX Embedding | BERT WordPiece tokenizer + ONNX Runtime |

## セキュリティ

| 機能 | 説明 |
|------|------|
| 3 段階防御 | 決定論的ブラックリスト（<50ms）/ 難読化検出（YELLOW+）/ Haiku AI 判定（RED のみ） |
| 脅威レベル状態機械 | GREEN → YELLOW → RED 自動エスカレート、24h 無イベントで -1 |
| Ed25519 認証 | チャレンジレスポンス WebSocket 認証 |
| AES-256-GCM | API キーの保存時暗号化、per-agent 隔離 |
| Prompt Injection スキャナ | 6 ルールカテゴリ + XML 区切りタグ保護 |
| SOUL.md ドリフト検出 | SHA-256 フィンガープリント比較 |
| CONTRACT.toml | 行動境界 + `duduclaw test` レッドチーム CLI（9 シナリオ）；全ランタイムの system prompt へ自動注入 |
| RBAC | 役割ベースアクセス制御マトリクス |
| 統一監査ログ | `audit.unified_log` が `security_audit.jsonl` / `tool_calls.jsonl` / `channel_failures.jsonl` / `feedback.jsonl` を統合 |
| JSONL 監査ログ | 非同期書込、Rust `AuditEvent` スキーマ互換 |
| Unicode 正規化 | NFKC で同形異字攻撃を検出 |
| Action Claim Verifier | ツール実行クレームの署名検証 |
| コンテナサンドボックス | Docker (Bollard) / Apple Container / WSL2 — `--network=none`、tmpfs、read-only rootfs、512MB 上限 |
| シークレット漏洩スキャナ | 20+ パターン（Anthropic/OpenAI/AWS/GitHub/Slack/Stripe/DB URL 等） |

## メモリシステム

| 機能 | 説明 |
|------|------|
| エピソード/意味分離 | Generative Agents 3D 加重検索（Recency × Importance × Relevance） |
| FTS5 全文検索 | SQLite 標準搭載 |
| ベクトル索引 | Embedding セマンティック検索（ONNX BERT / Qwen3-Embedding） |
| メモリ減衰スケジューラ | 日次バックグラウンド — 低重要度 + 30 日以上アーカイブ、アーカイブ + 90 日以上完全削除 |
| 認知メモリ MCP ツール | `memory_search_by_layer` / `memory_successful_conversations` / `memory_episodic_pressure` / `memory_consolidation_status` |
| フェデレーションメモリ | エージェント横断知識共有（Private / Team / Public） |
| Key-Fact Accumulator | `key_facts` + FTS5 — セッション横断の軽量メモリ（セッションメモリスタック参照） |

## Git Worktree 分離（v1.6.0）

| 機能 | 説明 |
|------|------|
| L0 分離レイヤー | タスク毎の git worktree — コンテナサンドボックスより軽量、並行エージェントのファイル衝突防止 |
| アトミックマージ | dry-run 事前チェック → abort → クリーンなら実マージ；グローバル `Mutex` 保護 |
| Snap ワークフロー | create → execute → inspect → merge/cleanup；純粋関数の意思決定ロジック |
| フレンドリーブランチ名 | `wt/{agent_id}/{adjective}-{noun}`、50×50 ワードリスト |
| copy_env_files | パス走査 jail + symlink 拒否 + 1MB サイズ上限 |
| AgentExitCode | 構造化終了コード — Success / Error / Retry / KeepAlive |
| リソース上限 | エージェント毎 5 個、全体 20 個 |

## アカウントとコスト管理

| 機能 | 説明 |
|------|------|
| マルチアカウントローテーション | OAuth + API Key、4 戦略（Priority/LeastCost/RoundRobin/Failover） |
| 双方の dispatch 経路 | サブエージェント dispatcher もチャネル返信もローテーターを経由 |
| CostTelemetry | SQLite トークン追跡 + キャッシュ効率分析 + 200K 価格クリフ警告 |
| 予算マネージャ | アカウント毎月上限 + クールダウン + 適応ルーティング（cache_eff <30% → ローカル） |
| Direct API | CLI バイパス、`cache_control: ephemeral`、95%+ キャッシュヒット率 |
| 失敗分類 | `FailureReason` 列挙 + カテゴリ別 zh-TW メッセージ + `channel_failures.jsonl` |
| バイナリ探索 | `which_claude()` / `which_claude_in_home()` が Homebrew / Bun / Volta / npm-global / `.claude/bin` / `.local/bin` / asdf / NVM を探索 |

## ブラウザ自動化

| 機能 | 説明 |
|------|------|
| 5 層ルーター | API Fetch / 静的スクレイプ / ヘッドレス Playwright / サンドボックスコンテナ / Computer Use |
| 能力ゲーティング | `agent.toml [capabilities]` はデフォルト拒否 |
| Browserbase | クラウドブラウザ（L5 代替） |
| bash-gate.sh | Layer 1.5 allowlist（`DUDUCLAW_BROWSER_VIA_BASH=1` 必須） |

## コンテナサンドボックス

| 機能 | 説明 |
|------|------|
| Docker | Bollard API、全プラットフォーム |
| Apple Container | macOS 15+ ネイティブ |
| WSL2 | Windows Linux サブシステム |

## スケジューリング

| 機能 | 説明 |
|------|------|
| CronScheduler | `cron_tasks.jsonl` + `cron_tasks.db` 永続化（v1.8.12）、`schedule_task` MCP スキーマ修正（`agent_id` + `name` を含む） |
| ReminderScheduler | 一度限りのリマインダー（相対 `5m`/`2h`/`1d` または ISO 8601）、`direct` / `agent_callback` モード |
| HeartbeatScheduler | エージェント毎統一スケジューリング — バスポーリング + GVU サイレンスブレイカー + cron |

## ERP 連携

| 機能 | 説明 |
|------|------|
| Odoo Bridge | 15 MCP ツール（CRM/販売/在庫/会計）、JSON-RPC ミドルウェア |
| Edition Gate | CE/EE 自動検出、機能ゲート |
| イベントポーリング | Odoo 状態変化をエージェントに能動通知 |

## RL と可観測性

| 機能 | 説明 |
|------|------|
| RL Trajectory Collector | チャネル対話中に `~/.duduclaw/rl_trajectories.jsonl` へ書込 |
| `duduclaw rl` CLI | `export` / `stats` / `reward` — 複合報酬（outcome × 0.7 + efficiency × 0.2 + overlong × 0.1） |
| Prometheus メトリクス | `GET /metrics` — requests / tokens / duration histogram / channel status |
| Dashboard WS ハートビート | サーバー Ping 30s + 60s アイドルクローズ；クライアント `ping` RPC 25s |
| BroadcastLayer | tracing レイヤー、リアルタイムログを WebSocket 購読者にストリーミング |

## Web ダッシュボード

| 機能 | 説明 |
|------|------|
| 23 ページ | Dashboard / Agents / Channels / Accounts / Memory / Security / Settings / OrgChart / SkillMarket / Logs / WebChat / OnboardWizard / Billing / License / Report / PartnerPortal / Marketplace / KnowledgeHub / Odoo / Login / Users / Analytics / Export |
| 技術スタック | React 19 + TypeScript + Tailwind CSS 4 + shadcn/ui、暖色 amber テーマ |
| リアルタイムログストリーミング | BroadcastLayer tracing → WebSocket push |
| Memory Key Insights | `key_facts` カード + access_count バッジ + タイムスタンプ + メタデータ |
| Memory Evolution | SOUL.md バージョン履歴 + 前後メトリクス差分 + 状態バッジ |
| Logs 履歴リライト | ソースフィルタチップ + ソース別カウント + 重要度ドロップダウン + 重要度着色左枠 + JSON 詳細展開 |
| Toast 通知 | モジュールスコープイベントバス、max-5 キュー、暖色 stone/amber/emerald/rose バリアント |
| OrgChart | D3.js インタラクティブエージェント階層 |
| Session Replay | 会話再生コンポーネント + タイムライン |
| WikiGraph | インタラクティブ知識グラフ |
| 国際化 | zh-TW / en / ja-JP（600+ 翻訳キー） |
| ダーク/ライトテーマ | システム設定 + 手動トグル |
| Experiment Logger | RL/RLHF オフライン分析用のトラジェクトリ記録 |
| Marketplace RPC | `marketplace.list` が実 MCP カタログを提供（Playwright / Browserbase / Filesystem / GitHub / Slack / Postgres / SQLite / Memory / Fetch / Brave Search） |
| Partner Portal | SQLite `PartnerStore` + 7 RPCs（profile/stats/customers CRUD） |

## 商用機能

| 機能 | 説明 |
|------|------|
| ライセンスティア | Free / Pro / Enterprise |
| ハードウェアフィンガープリント | ライセンスバインディング |
| 業種テンプレート | 製造業 / 飲食業 / 貿易業 |
| CLI ツール | 12+ サブコマンド |
| Partner Portal | マルチテナント販売代理店インターフェース |
