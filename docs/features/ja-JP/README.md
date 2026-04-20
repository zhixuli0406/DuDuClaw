# DuDuClaw 主要機能紹介

> DuDuClaw v1.8.14 | 最終更新：2026-04-21

本ディレクトリには、DuDuClawの注目機能に関する詳細な紹介記事を収録しています。各記事では設計思想、システム動作、運用フローを解説しており、ソースコードを読まずに「仕組み」を理解したい開発者を対象としています。

---

## 機能インデックス

| # | 記事 | 概要 |
|---|------|------|
| 1 | [予測駆動型進化エンジン](01-prediction-driven-evolution.md) | 90%の会話をLLMコストゼロで進化 |
| 2 | [GVU² デュアルループ](02-gvu-self-play-loop.md) | 二重ループ進化 + 4+2層検証 |
| 3 | [信頼度ルーターとローカル推論エンジン](03-confidence-router.md) | スマートなモデル選択でAPI費用80%以上削減 |
| 4 | [ファイルベースIPCメッセージバス](04-file-based-ipc.md) | 構造化エージェント間委任 + TaskSpecワークフロー |
| 5 | [3段階プログレッシブセキュリティ防御](05-security-defense.md) | 階層型脅威フィルタリングで最小コスト |
| 6 | [SOUL.md バージョン管理とロールバック](06-soul-versioning.md) | アトミックなパーソナリティ更新と自動ロールバック |
| 7 | [マルチアカウントローテーションとクロスプロバイダーフェイルオーバー](07-account-rotation.md) | Claude/Codex/Gemini横断の認証情報スケジューリング |
| 8 | [5層ブラウザ自動化ルーター](08-browser-automation.md) | 段階的リソースエスカレーション |
| 9 | [行動規約とレッドチームテスト](09-behavioral-contracts.md) | 機械的に強制可能なエージェント行動境界 |
| 10 | [認知メモリシステム](10-cognitive-memory.md) | 忘却曲線を備えた人間型記憶 |
| 11 | [トークン圧縮トライアド](11-token-compression.md) | 3つの戦略でより多くを、より少なく |
| 12 | [業種テンプレートとOdoo ERP連携](12-industry-templates.md) | すぐに使えるビジネスインテリジェンス |
| 13 | [マルチランタイムエージェント実行](13-multi-runtime.md) | Claude / Codex / Gemini / OpenAI互換統一バックエンド |
| 14 | [音声パイプライン](14-voice-pipeline.md) | ASR / TTS / VAD / LiveKit — ローカル優先音声インテリジェンス |
| 15 | [スキルライフサイクルエンジン](15-skill-lifecycle.md) | 7段階の自動スキル抽出・管理 |
| 16 | [セッションメモリスタック](../16-session-memory-stack.md) | Instruction Pinning + Snowball Recap + Key-Fact Accumulator |
| 17 | [Wiki 知識レイヤー](../17-wiki-knowledge-layer.md) | L0-L3 信頼度加重知識の自動注入 |
| 18 | [Git Worktree L0 分離](../18-worktree-isolation.md) | タスク毎の軽量ワークスペース + アトミックマージ |
| 19 | [Agent Client Protocol (ACP/A2A)](../19-agent-client-protocol.md) | stdio JSON-RPC 2.0 — Zed/JetBrains/Neovim 統合 |

> 注：16-19 は現在英語版のみ。翻訳の PR を歓迎します。

---

## 全機能一覧

注目機能だけでなく全機能の一覧は [feature-inventory.md](feature-inventory.md) をご覧ください。
