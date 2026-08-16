# DuDuClaw 主要機能紹介

> DuDuClaw v1.61.0 | 最終更新：2026-08-16

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
| 16 | [セッションメモリスタック](16-session-memory-stack.md) | Instruction Pinning + Snowball Recap + Key-Fact Accumulator |
| 17 | [Wiki 知識レイヤー](17-wiki-knowledge-layer.md) | L0-L3 信頼度加重知識の自動注入 |
| 18 | [Git Worktree L0 分離](18-worktree-isolation.md) | タスク毎の軽量ワークスペース + アトミックマージ |
| 19 | [Agent Client Protocol (ACP/A2A)](19-agent-client-protocol.md) | stdio JSON-RPC 2.0 — Zed/JetBrains/Neovim 統合 |
| 20 | [メモリインテリジェンス](20-memory-intelligence.md) | 時系列ファクト + Reflexionループ + バッチ取得（v1.19.0） |
| 21 | [ガバナンス層](21-governance-layer.md) | ポリシーレジストリ + エージェント別クォータ（duduclaw-governance） |
| 22 | [永続化フレームワーク](22-durability-framework.md) | 冪等性 / リトライ / サーキットブレーカー / チェックポイント / DLQ |
| 23 | [Autopilot ルールエンジン](23-autopilot-engine.md) | イベント駆動の自動化 + サーキットブレーカー |
| 24 | [タスクボードとアクティビティフィード](24-task-board.md) | チームメイトとしてのエージェントのタスク管理 |
| 25 | [アイデンティティ解決](25-identity-resolution.md) | WikiCache / Notion / Chained プロバイダー（RFC-21 §1） |
| 26 | [MCP HTTP/SSE トランスポート](26-mcp-http-sse.md) | Bearer 認証 REST + SSE エンドポイント（W20） |
| 27 | [クロスプラットフォーム PTY プール + ワーカー](27-pty-pool-runtime.md) | 対話型 `claude` REPL を駆動（v1.15.0） |
| 28 | [ライブ実行フォーク（Live Forking）](28-live-forking.md) | 並列ブランチ + AI ジャッジ（duduclaw-fork, RFC-26） |
| 29 | [進化イベント](29-evolution-events.md) | バッチ + リトライ配信のブラックボックスレコーダー |
| 30 | [カスタムダッシュボードウィジェット](30-custom-widgets.md) | AI ガイドまたは生 HTML のダッシュボードカード（サンドボックス実行） |
| 31 | [オフィス文書スイート](31-office-document-suite.md) | 実 docx/xlsx/pptx/pdf 出力：DELIVER 納品プロトコル、アーカイブとプレビュー |
| 32 | [エキスパートパック](32-expert-packs.md) | インストール可能な AI チーム：内蔵カタログ、LLM ガイド自作、部門×職級の組織配置 |
| 33 | [OS ネイティブ知覚とプロアクティブケア](33-os-native-perception.md) | ファイル監視＋前面アプリ知覚 → フットプリント記憶、ケアチェック、ワンクリック自動化 |
| 34 | [自律ゴールループ](34-goal-loop.md) | /goal → 完了までループ、MAV 受入ジャッジ；行き詰まりは人間へ |
| 35 | [写真 → デスクトップペット](35-photo-desktop-pet.md) | ローカル写真→ピクセルペット：Codex Pets スプライトシート＋徘徊エンジン |
| 36 | [録画 → スキル](36-recording-to-skill.md) | ブラウザ/デスクトップ録画を承認制 SKILL.md 草稿へ蒸留 |
| 37 | [部門と職級の分離](37-delegation-isolation.md) | 組織境界委譲ポリシー：階級 / 部門 / ホワイトリスト強制 |
| 38 | [自主進化 v3：AEE + Playbook](38-aee-playbook-evolution.md) | Agentic Evolution Engine — ゲート/測定分割のプレイブックルール |
| 39 | [キャリブレーション予測 + held-out 学習ゲート](39-calibrated-forward-model.md) | 正当なスコアリング校準＋サンプル外ルール昇格 |
| 40 | [通知ガバナンス](40-notification-governance.md) | チャネル横断の通知ガバナンス |
| 41 | [常駐センシング＋シグナル起動](41-resident-sensing.md) | 外部データストリーム：ルール命中時のみエージェント起動 |
| 42 | [人間による引き継ぎ](42-human-takeover.md) | `/takeover` ライフサイクルによる人間の引き継ぎ（オプト イン） |
| 43 | [Telegram ミニアプリ 承認詳細カード](43-telegram-miniapp.md) | Telegram 内の承認詳細カード（プレビュー、デフォルト オフ） |
| 44 | [ワークステート（Working State）](44-working-state.md) | エージェント別の唯一の権威ある横断起動状態 — ゴーストメモリ修正 |
| 45 | [ローカルモデルマーケットプレイス](45-local-model-marketplace.md) | 用途別セレクター + ハードウェア適合 HF ピッカー + ワンクリックインストール |
| 46 | [信念ループ（Belief Loop）](46-belief-loop.md) | 外部世界についての構造化予測、実現対実測でスコア化 |
| 47 | [Agent Mail（メール箱）](47-agent-mail.md) | エージェント別メール受信トレイ、送信下書きは人間承認で送出 |

---

## 補足記事

| 記事 | 概要 |
|------|------|
| [Live Forking 利用シナリオ](live-forking.md) | 28 の利用シナリオ姉妹編：いつ使うべきか、いつ使うべきでないか、`duduclaw eval` との違い |
| [ERP / CRM サポートマトリクス](erp-support-matrix.md) | 営業・顧客との対話用の 1 ページ早見表 |

---

## 全機能一覧

注目機能だけでなく全機能の一覧は [feature-inventory.md](feature-inventory.md) をご覧ください。
