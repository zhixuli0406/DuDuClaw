# DuDuClaw 主要機能紹介

> DuDuClaw v1.62.0 | 最終更新：2026-09-04

本ディレクトリには、DuDuClawの注目機能に関する詳細な紹介記事を収録しています。各記事では設計思想、システム動作、運用フローを解説しており、ソースコードを読まずに「仕組み」を理解したい開発者を対象としています。

---

## 機能インデックス

| # | 記事 | 概要 |
|---|------|------|
| 1 | [予測駆動型進化エンジン](01-prediction-driven-evolution.md) | 90%の会話をLLMコストゼロで進化 |
| 3 | [信頼度ルーターとローカル推論エンジン](03-confidence-router.md) | スマートなモデル選択でAPI費用80%以上削減 |
| 4 | [ファイルベースIPCメッセージバス](04-file-based-ipc.md) | 構造化エージェント間委任 + TaskSpecワークフロー |
| 5 | [セキュリティ防御](05-security-defense.md) | 稼働中の4つのガード、どこで動くか、どれもカバーしないもの |
| 7 | [マルチアカウントローテーションとクロスプロバイダーフェイルオーバー](07-account-rotation.md) | Claude/Codex/Antigravity横断の認証情報スケジューリング |
| 8 | [ブラウザ自動化と Computer Use](08-browser-automation.md) | エージェントが選ぶ取得ツール 2 つと任意のブラウザサーバー、8 つの `computer_*` MCP ツールで駆動する Computer Use セッション。自動ルーターなし |
| 9 | [行動規約とレッドチームテスト](09-behavioral-contracts.md) | 機械的に強制可能なエージェント行動境界 |
| 10 | [認知メモリシステム](10-cognitive-memory.md) | 忘却曲線を備えた人間型記憶 |
| 11 | [プロンプト予算の強制](11-token-compression.md) | 見積もり、3段階、それでも駄目なら拒否 |
| 12 | [業種テンプレートとOdoo ERP連携](12-industry-templates.md) | すぐに使えるビジネスインテリジェンス |
| 13 | [マルチランタイムエージェント実行](13-multi-runtime.md) | Claude / Codex / Antigravity / Grok / OpenAI互換など複数バックエンドを統一（Gemini CLI は非推奨） |
| 14 | [音声パイプライン](14-voice-pipeline.md) | 別々に配線された2つの STT/TTS 経路：fail-closed な HTTP と、ハードコードされた Telegram ハンドラ |
| 15 | [スキルライフサイクルエンジン](15-skill-lifecycle.md) | 6段階の自動スキル抽出・管理 |
| 16 | [セッションメモリスタック](16-session-memory-stack.md) | Instruction Pinning + Snowball Recap + Key-Fact Accumulator |
| 17 | [Wiki 知識レイヤー](17-wiki-knowledge-layer.md) | L0-L3 信頼度加重知識の自動注入 |
| 19 | [Agent Client Protocol (ACP/A2A)](19-agent-client-protocol.md) | `duduclaw acp` は IDE パネル（Zed / JetBrains / nvim）向けに Agent Client Protocol v1 を話し、`duduclaw acp-server` は A2A の stdio インターフェースです |
| 20 | [メモリインテリジェンス](20-memory-intelligence.md) | 時系列ファクト + Reflexionループ + バッチ取得（v1.19.0） |
| 23 | [Autopilot ルールエンジン](23-autopilot-engine.md) | イベント駆動の自動化 + サーキットブレーカー |
| 24 | [タスクボードとアクティビティフィード](24-task-board.md) | チームメイトとしてのエージェントのタスク管理 |
| 25 | [アイデンティティ解決](25-identity-resolution.md) | WikiCache / Notion / Chained プロバイダー（RFC-21 §1） |
| 26 | [MCP HTTP/SSE トランスポート](26-mcp-http-sse.md) | Bearer 認証 REST + SSE エンドポイント（W20） |
| 27 | [ワンショットPTY呼び出し](27-pty-pool-runtime.md) | 本物の端末を要求するCLIにPTYを用意（プールは2026-09に削除） |
| 28 | [ライブ実行フォーク（Live Forking）](28-live-forking.md) | 並列ブランチと AI ジャッジ。コピー規則、保持ブランチの採用、テストのタイムアウト処理 |
| 29 | [進化イベント](29-evolution-events.md) | バッチ + リトライ配信のブラックボックスレコーダー |
| 30 | [カスタムダッシュボードウィジェット](30-custom-widgets.md) | AI ガイドまたは生 HTML のダッシュボードカード（サンドボックス実行） |
| 31 | [オフィス文書スイート](31-office-document-suite.md) | 実 docx/xlsx/pptx/pdf 出力：DELIVER 納品プロトコル、アーカイブとプレビュー |
| 32 | [エキスパートパック](32-expert-packs.md) | インストール可能な AI チーム：内蔵カタログ、LLM ガイド自作、部門×職級の組織配置 |
| 33 | [OS ネイティブ知覚とプロアクティブケア](33-os-native-perception.md) | ファイル監視＋前面アプリ知覚 → フットプリント記憶、ケアチェック、ワンクリック自動化 |
| 34 | [自律ゴールループ](34-goal-loop.md) | /goal → MAV 受入判定、各ラウンドの状態と費用を記録；週次生存表に標本の限界を明示 |
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
| 48 | [ゴール意図ルーター](48-goal-intent-router.md) | チャットチャネルがタスク委任を察知してゴール作成を提案。自動作成は決して行わない |
| 49 | [コードセキュリティ監査](49-code-security-audit.md) | `duduclaw secaudit`：静的スキャナ＋AI 深層監査＋敵対的レビュー＋サンドボックス PoC |
| 50 | [DuDuClaw OSアプライアンス](50-duduclaw-os-appliance.md) | 起動可能なアプライアンスイメージ：LAN ダッシュボードでの初期設定、デバイスページ、sysd 権限分離、webhook リレー |
| 51 | [DuDuClaw OS キーボードショートカット一覧](51-os-keyboard-shortcuts.md) | DuDuClaw OS の全ショートカット：コンポジタのグローバルバインド、シェル UI、初回セットアップ、ロック画面 |
| 52 | [DuDuClaw OS デスクトップ版](52-desktop-edition.md) | 人と AI で一台のマシンを共有：シャドウワークスペース、人の入力が常に優先、明示的なハンドバック、共同運転は既定オフ |
| 53 | [デバイス上のローカルモデル](53-local-models.md) | 検証済み GGUF 6 種、ワンクリックでダウンロードと有効化。既定は hybrid、速度は誇張しない |
| 54 | [ファインチューニングと事後学習](54-finetune.md) | データの整理はここで、学習は別の GPU で、GGUF／LoRA を持ち帰る——この機械は学習しない |
| 55 | [データソースとネイティブ DB コネクタ](55-data-sources.md) | 任意の `db_field` ルールが参照できるレジストリ、顧客自身の MCP server も遮蔽する MCP プロキシ、そしてファーストパーティの読み取り専用 PostgreSQL／MySQL／SQLite コネクタ |
| 56 | [チームとしての従業員（Team-as-Agent）](56-team-as-agent.md) | 一人の従業員の内部に 規劃／執行／審核／合成 の 4 つの役割を置き、それぞれが自分の runtime とモデルを持ちます。スイッチは既定でオンですが、`[team.roles]` に 2 つ目のベンダーを指定して初めてチームが組まれます。分解可能性ゲートがタスクごとに判定し、役割間ではトランスクリプトではなく TaskPacket を交換します |
| 57 | [UCCI キャリブレーション付きカスケード](57-ucci-calibrated-cascade.md) | 実験的で opt-in のローカルルーティング層：isotonic キャリブレーション済みの router が、従来の post-hoc 信頼度ゲートの代わりにトークンマージンの不確実性から LocalFast→LocalStrong→クラウドのエスカレーションを判断します。オフラインでフィットし、既定ではオフ |
| 58 | [ナイトエンジン](58-night-engine.md) | アイドル時間の記憶整理：4 つのサブパス（2 つは決定論的、2 つはユーティリティモデル）、パスごとの支出上限と日次サーキットブレーカー。独立した 2 つのスイッチで既定オフ。成果のあるパスは Activity Feed に表示 |
| 59 | [ローカルプロキシ](59-local-proxy.md) | `duduclaw proxy` — Aider / Cline / Codex がアカウントプールを借りられる OpenAI 互換の localhost エンドポイント。Bearer 必須、既定は loopback、OAuth シートを転送できないことも明記 |
| 60 | [Discovery](60-discovery.md) | 承認済み workspace の探索、予算とツリー台帳、hash 検証済み成果、ゼロ LLM held-out ポリシー比較。統合検証中 |

---

## 補足記事

| 記事 | 概要 |
|------|------|
| [Live Forking 利用シナリオ](live-forking.md) | 28 の利用シナリオ姉妹編：いつ使うべきか、いつ使うべきでないか、`duduclaw eval` との違い |
| [ERP / CRM サポートマトリクス](erp-support-matrix.md) | 営業・顧客との対話用の 1 ページ早見表 |

---

## 全機能一覧

注目機能だけでなく全機能の一覧は [feature-inventory.md](feature-inventory.md) をご覧ください。
