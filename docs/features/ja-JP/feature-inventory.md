# DuDuClaw 全機能一覧

> v1.1.0 | 最終更新：2026-04-07

---

## コアアーキテクチャ

| 機能 | 説明 |
|------|------|
| Claude Code 拡張レイヤー | 独立AIではなく、チャネルルーティング・セッション管理・メモリ・進化のためのプラミング |
| MCP Server（JSON-RPC 2.0） | stdin/stdout経由でClaude Codeに52+ツールを公開 |
| エージェントディレクトリ構造 | 各エージェントに`.claude/`、`SOUL.md`、`CLAUDE.md`、`.mcp.json`を含む |
| サブエージェント編成 | `create_agent` / `spawn_agent` / `list_agents`、`reports_to`階層対応 |
| Session Manager | SQLite永続化、50kトークン自動圧縮（CJK対応） |
| ファイルベースIPC | `bus_queue.jsonl`によるエージェント間タスク委譲 |

## 通信チャネル（7種）

| チャネル | プロトコル |
|----------|------------|
| Telegram | ロングポーリング、ファイル/写真/スタンプ対応 |
| LINE | Webhook、スタンプ対応 |
| Discord | Gateway WebSocket、スラッシュコマンド、ボイスチャンネル |
| Slack | Socket Mode |
| WhatsApp | Cloud API |
| Feishu | Open Platform v2 |
| WebChat | 組み込み`/ws/chat` WebSocket + Reactフロントエンド |
| チャネルホットスタート/ストップ | ダッシュボードからの即時起動/停止 |

## 進化システム

| 機能 | 説明 |
|------|------|
| 予測駆動エンジン | Active Inference、約90% LLMコストゼロ |
| デュアルプロセスルーター | System 1（ルール）/ System 2（LLMリフレクション） |
| GVUセルフプレイループ | Generator-Verifier-Updater、TextGradフィードバック、最大3ラウンド |
| SOUL.mdバージョン管理 | 24時間観察期間、アトミックロールバック、SHA-256フィンガープリント |
| MetaCognition | 100回の予測ごとにエラー閾値を自動校正 |

## スキルエコシステム

| 機能 | 説明 |
|------|------|
| 7段階ライフサイクル | 起動、圧縮、抽出、再構成、蒸留、診断、ギャップ分析 |
| GitHubライブインデックス | Search API + 24時間ローカルキャッシュ |
| スキルマーケットプレイス | Webダッシュボードでの閲覧とインストール |

## ローカル推論エンジン

| 機能 | 説明 |
|------|------|
| llama.cpp | Metal/CUDA/Vulkan/CPU |
| mistral.rs | Rustネイティブ、ISQ、PagedAttention、Speculative Decoding |
| OpenAI互換HTTP | Exo/llamafile/vLLM/SGLang |
| 信頼度ルーター | LocalFast / LocalStrong / CloudAPI 3層ルーティング |
| InferenceManager | マルチモード自動切替ステートマシン + ヘルスチェック |
| llamafileマネージャー | サブプロセスライフサイクル管理、ゼロインストールポータブル推論 |
| Exo P2Pクラスター | 分散推論、235B+モデルをマルチマシンで実行 |
| MLX Bridge | Apple Siliconローカルリフレクション + LoRA |

## 圧縮エンジン

| 機能 | 説明 |
|------|------|
| Meta-Token（LTSC） | Rustネイティブ無損失BPE風、27-47%圧縮率 |
| LLMLingua-2 | Microsoftトークン重要度プルーニング、2-5x非可逆圧縮 |
| StreamingLLM | Attention sink + スライディングウィンドウKV-cache |

## 音声パイプライン

| 機能 | 説明 |
|------|------|
| ASR | Whisper.cpp / SenseVoice ONNX / Deepgram |
| TTS | Piper（ローカルONNX）/ MiniMax（リモート） |
| VAD | Silero（ONNX） |
| LiveKit Voice | WebRTCボイスルーム |

## セキュリティレイヤー

| 機能 | 説明 |
|------|------|
| 3段階防御 | 決定的ブラックリスト / 難読化検出 / AI判定 |
| Ed25519認証 | チャレンジ-レスポンスWebSocket認証 |
| AES-256-GCM | APIキーの静的暗号化 |
| Prompt Injectionスキャナー | 6ルールカテゴリ |
| SOUL.mdドリフト検出 | SHA-256フィンガープリント照合 |
| CONTRACT.toml | 行動境界 + `duduclaw test`レッドチームテスト |
| RBAC | ロールベースアクセス制御 |
| JSONL監査ログ | 全ツール呼び出し記録 |

## メモリシステム

| 機能 | 説明 |
|------|------|
| エピソード/セマンティック分離 | Generative Agents 3D加重検索 |
| 全文検索（FTS5） | SQLite内蔵 |
| ベクトルインデックス | Embeddingセマンティック検索 |
| メモリ減衰 | 間隔反復忘却曲線 |
| フェデレーテッドメモリ | エージェント間ナレッジ共有 |
| Wikiナレッジベース | 全文検索 + ナレッジグラフ可視化 |

## アカウント・コスト管理

| 機能 | 説明 |
|------|------|
| マルチアカウントローテーション | OAuth + APIキー、4戦略 |
| CostTelemetry | トークン使用量追跡 + キャッシュ効率分析 |
| 予算管理 | アカウント別月次上限 + クールダウン |
| Direct API | CLIバイパス、95%+ キャッシュヒット率 |

## ブラウザ自動化

| 機能 | 説明 |
|------|------|
| 5層ルーター | API Fetch / 静的スクレイプ / ヘッドレス / サンドボックス / Computer Use |
| 能力ゲーティング | `agent.toml [capabilities]` デフォルト拒否 |

## コンテナサンドボックス

| 機能 | 説明 |
|------|------|
| Docker | Bollard API、全プラットフォーム |
| Apple Container | macOS 15+ネイティブ |
| WSL2 | Windows Linuxサブシステム |

## スケジューリング

| 機能 | 説明 |
|------|------|
| CronScheduler | `cron_tasks.jsonl` cron式スケジューリング |
| ReminderScheduler | ワンショットリマインダー（相対/絶対時間） |
| HeartbeatScheduler | エージェント別統合スケジューリング |

## ERP連携

| 機能 | 説明 |
|------|------|
| Odoo Bridge | 15 MCPツール（CRM/販売/在庫/会計） |
| Edition Gate | CE/EE自動検出 |

## Webダッシュボード

| 機能 | 説明 |
|------|------|
| 23ページ | ダッシュボード、エージェント管理、チャネル、メモリ、セキュリティ、請求など |
| リアルタイムログストリーミング | BroadcastLayer tracing → WebSocket |
| WikiGraph | インタラクティブナレッジグラフ |
| OrgChart | エージェント階層可視化 |

## コマーシャル

| 機能 | 説明 |
|------|------|
| ライセンス階層 | Free / Pro / Enterprise |
| ハードウェアフィンガープリント | ライセンスバインド |
| 業種テンプレート | 製造業 / レストラン / 貿易業 |
| CLIツール | 12サブコマンド |
