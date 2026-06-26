# DuDuClaw 🐾

<div align="center">

[繁體中文](README.md) · [English](README.en.md) · **日本語**

</div>

> **Multi-Runtime AI Agent Platform** — Claude / Codex / Gemini の三大 CLI を統一し、あなたのマルチチャネル AI アシスタントを構築

[![CI](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml/badge.svg)](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/Rust-2024_edition-orange?logo=rust)](https://www.rust-lang.org/)
[![Python](https://img.shields.io/badge/Python-3.9+-blue?logo=python)](https://www.python.org/)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE)
[![Version](https://img.shields.io/badge/version-1.26.0-blue)](https://github.com/zhixuli0406/DuDuClaw/releases)
[![npm](https://img.shields.io/npm/v/duduclaw?logo=npm)](https://www.npmjs.com/package/duduclaw)
[![PyPI](https://img.shields.io/pypi/v/duduclaw?logo=pypi)](https://pypi.org/project/duduclaw/)

---

## 🔒 信頼性とセキュリティ（Trust & Security）

これはオープンソースプロジェクトです — インストールする内容を完全に透明化します。

### なぜ「新しい」npm パッケージがすでに version 1.21+ なのか？

DuDuClaw は公開前に、プライベートリポジトリで数か月の集中開発（400+ commits）を経ています。
完全な履歴は [git log](https://github.com/zhixuli0406/DuDuClaw/commits/main) を参照してください。

### npm パッケージの中身は？

- 対応プラットフォームの Rust バイナリを呼び出すだけの小さな JS ラッパー
- プラットフォームバイナリは npm `optionalDependencies`（`@duduclaw/<platform>`）で配布 —
  **任意の URL から外部コードをダウンロードして実行する postinstall は存在しません**
- `postinstall` はプラットフォームパッケージの有無を確認するだけ
  （[`npm/duduclaw/scripts/install.js`](npm/duduclaw/scripts/install.js) 参照）— 何もダウンロード・実行しません
- GitHub Releases のバイナリはすべて SHA-256 チェックサム付き

### プリビルドバイナリを信頼しない場合はソースからビルド

```bash
git clone https://github.com/zhixuli0406/DuDuClaw
cd DuDuClaw
cargo build --release
```

### バイナリの検証

各リリースには SHA-256 チェックサムと [cosign](https://github.com/sigstore/cosign) keyless 署名が付属します：

```bash
# Releases からダウンロード
wget https://github.com/zhixuli0406/DuDuClaw/releases/download/v1.21.1/duduclaw-darwin-arm64.tar.gz

# SHA-256 を検証（リリース内の .sha256 ファイルと照合）
shasum -a 256 -c duduclaw-darwin-arm64.tar.gz.sha256

# cosign 署名を検証
cosign verify-blob \
  --certificate duduclaw-darwin-arm64.tar.gz.pem \
  --signature duduclaw-darwin-arm64.tar.gz.sig \
  --certificate-identity-regexp "https://github.com/zhixuli0406/DuDuClaw" \
  --certificate-oidc-issuer "https://token.actions.githubusercontent.com" \
  duduclaw-darwin-arm64.tar.gz
```

### サプライチェーンの透明性

- **ライセンス**：Apache 2.0
- **メンテナー**：嘟嘟數位科技有限公司（台湾登記法人、統一番号 94139082）
- **公開 commit 履歴**：github.com/zhixuli0406/DuDuClaw
- **CI/CD**：すべてのリリースは GitHub Actions でビルド
- **テレメトリなし**：phone-home 通信はゼロ
- **API キーを収集しない**：すべての秘密情報は AES-256-GCM で自分のマシンに保管
- **権限昇格なし**：完全に user space で動作

脆弱性の報告は [SECURITY.md](SECURITY.md) を参照してください。

---

## なぜネイティブの Claude / GPT / Gemini CLI ではなく DuDuClaw なのか？

ネイティブ CLI は、個人として単一の LLM をたまに使う分には十分優秀です。
しかし本番環境へ投入する段階になると、DuDuClaw がすでに提供しているものを自分で作り直す羽目に
なります：

| 必要なもの | ネイティブ CLI | DuDuClaw |
|---|---|---|
| マルチ LLM 自動フォールバック | 手動で再起動 | 標準搭載（4 戦略） |
| LLM 切り替え時のコンテキスト保持 | 失われる | 保持される |
| LLM 横断のツール共有 | LLM ごとに書き直し | 一度書けば共有 |
| 本番ハードニング（DLQ／リトライ／可観測性） | 自前で構築 | 標準搭載 |
| マルチチャネル（Telegram／LINE／Discord／…） | CLI のみ | 7 チャネル |
| シークレット／監査／PII マスキング | 自前で構築 | 標準搭載 |

`claude` や `gemini` を一人でたまに使うだけなら — ネイティブのままで十分です。
**本番のマルチ LLM Agent システム**を構築するなら — DuDuClaw が 3 か月分のインフラ構築を肩代わり
します。

---

> 🎉 **v1.26.0 — 個人版 / エンタープライズ版 + ワンクリック CLI ログイン**（[Release](https://github.com/zhixuli0406/DuDuClaw/releases/tag/v1.26.0)）
>
> ライセンス階層と直交する「プロダクト形態」軸（個人版 / エンタープライズ版）を追加。**デフォルト値と UI のみを制御し、コア機能を一切ゲートしません**。さらに全 CLI 向けのワンクリックログインを追加。
>
> - **個人版 / エンタープライズ版**（`EditionProfile`）— 個人版（既定）は単一所有者・設定ゼロ、エンタープライズ版はマルチシート / コンプライアンス管理面。優先順位 `DUDUCLAW_EDITION` env > `agent.toml [edition]` > ライセンス階層 > 個人版。dashboard は個人版でエンタープライズ nav（org / users / governance / partner / wiki-trust）を隠し、EditionBadge + 非ブロッキングのソフト上限バナーを表示
> - **Dashboard ワンクリック CLI ログイン**（`auth.cli_login.*`）— 各 CLI（Claude / Codex / Gemini / Antigravity）のネイティブログインを gateway 上の PTY で駆動し、dashboard ターミナルにストリーム、貼り付けたコードを返送；CLI ごとに `remote_safe`（貼り付け式 vs localhost コールバック）を提示；Claude `setup-token` はエンドツーエンドで検証済み
> - **Antigravity CLI（`agy`）を内蔵** — server Docker イメージに claude / codex / gemini と並べて同梱し、Antigravity ランタイムをすぐ利用可能
> - **個人版データ可搬性** — `duduclaw export` / `duduclaw import` が `~/.duduclaw/` を可搬な `.tar.gz`（agents / メモリ / 設定 / ライセンス）にまとめ、マシン間移行やセルフホスト ↔ マネージドの切替に対応
> - 新ライセンス階層 `PersonalProSelfHost`（NT$490/月）：エンタープライズモジュールなしで業種テンプレート + 優先パッチを解放



https://github.com/user-attachments/assets/30406ad1-4595-43ce-8c08-dba8f0ca9683



<details>
<summary><strong>v1.9.4 → v1.25.x 累積ハイライト</strong></summary>

- **v1.25.0** — ブラウザ優先のオンボーディング：`WelcomePage`（3 ステップ、5 つの AI バックエンド経路）+ `FirstRunGate`（Agent ゼロの誘導）+ ガイド付きツアー `GuidedTour`（依存ゼロのスポットライト）+ `runtime.detect` RPC による設定ゼロ起動
- **v1.24.0** — Antigravity CLI（`agy`）ランタイム対応 · PtyPool の Claude 固定を解除：`RuntimeType::Antigravity` を追加（ワンショット `agy -p`、バイナリ自動解決、システムプロンプト + 履歴の埋め込み、CJK セーフな切り詰め、`trustedWorkspaces` 事前登録）；`CliKind::Antigravity` を PtyPool / worker spawn に接続、`cli_kind_for_provider()` が `[runtime] provider` から種別を導出；対話型 REPL は Claude 専用のまま（設計上）；旧 `gemini` バックエンドは有料 `GEMINI_API_KEY` / エンタープライズ向けに維持
- **v1.23.0** — Decision Continuity（RFC-24）：エージェントが列挙式の選択肢（案 A/B/C）を提示した際、各選択肢を Temporal Memory の **semantic** 層に永続化（会話圧縮から独立）し、未決事項をターンごとに再注入；後から「案 C で」（別ターン / セッション / プロセス）と言われても、推測ではなく永続状態から解決。検出は決定論的でゼロ LLM；`[memory] decision_continuity = true` でエージェント単位の opt-in
- **v1.22.0** — RFC-26 Live Forking（ラウンド 1–4）：実行中のタスクを N 個の競合ブランチに分岐し、それぞれ copy-on-write の隔離ワークスペースで異なる戦略を試行、AI judge が勝者を選定（`duduclaw-fork` + 6 つの MCP ツール + クロスプロセス `ForkStore` + `RotatingBranchExecutor` + `LiveAggregate` 予算プリエンプション）；スキル合成スケジューラ（W19-P1）；共有コンポーネントライブラリで再構築した Calm Glass ダッシュボード。いずれもデフォルト無効
- **v1.21.0** — RFC-25 §5 フォローアップ：非 Claude（Codex / Gemini / OpenAI-compat）パスが 11 件の欠落をすべて解消して一級市民に（マルチターン文脈、コスト計測、keepalive、per-(home,provider) フェイルオーバー退避）；`release.sh` のマルチプラットフォーム版数同期 + ドリフト監査 + bump 後アサート + `verify`、`skip-existing` で PyPI が黙って凍結する問題を修正
- **v1.20.0** — RFC-25 マルチランタイム解放 + A2A：「Multi-Runtime 四バックエンド」はこれまでコンパイルされない孤立ソースで、すべての実行パスが Claude をハードコードしていました。v1.20.0 はこれを配線し、LLM を呼ぶサブシステムを単一の provider-agnostic な choke-point（`runtime_dispatch::run_agent_prompt` + 遅延自動検出する `RuntimeRegistry`）に通します。channel reply / GVU / サブ agent 委譲は非 Claude provider で choke-point を通り（Claude は OAuth ローテーション / PTY パスを維持、リグレッションなし）；ACP `tasks/send` がターゲット agent を実際に実行し Failed / Completed を報告；Phase 0 で GVU 進化モデルのハードロックを撤廃（reject → warn）

- **v1.19.0** — Memory Intelligence：W18/W19 で設計された記憶層を、現行の Rust `SqliteMemoryEngine` に非侵襲的に実装。**Temporal Memory**（`memories` に時態 / ナレッジグラフ列 + `store_temporal` の自動 supersession チェーン + `get_history`/`get_at`、検索は既定で有効な記憶のみ返す）；**Reflexion Loop**（既存の `MistakeNotebook` をブリッジ：回答プロンプトへのリコール注入 + 同カテゴリ ≥3 件を semantic ルールに統合）；**`memory_fetch_batch`** MCP ツール（ID 一括取得 ≤100、namespace/ownership 隔離）。`MemoryEntry` は不変、影響ゼロ
- **v1.18.0** — Dashboard 予算／使用量の正確化：永続化された `CostTelemetry` 台帳から読む（再構築でゼロに戻るメモリカウンタを置換）、`cost_millicents` の単位誤称を修正、`marketplace.install` を実装、設定永続化の穴埋め、フロントエンドの runtime バグ掃除 + 88 個の i18n キー

- **v1.17.0** — RFC-24 License v2.0（Open Core 基盤）：新 crate `duduclaw-license`（verification-only クライアント、署名鍵は `commercial/duduclaw-license` に保持）、7 つの tier 継承チェーン `OpenSource` / `Hobby` / `Solo` / `Studio` / `Business` / `SelfHostPro` / `Oem`、Ed25519 trust registry は `DUDUCLAW_LICENSE_PUBKEY_<ID>` env でシード化（空の registry は fail-safe で OpenSource に退避）。Apache 2.0 コアは**無制限で利用可能**、有料サブスクリプションが `commercial/*` 付加価値モジュールをアンロック
- **v1.16.0** — MCP Refresh Tokens + GVU `SoulPatchOp::Consolidate`：新モジュール `mcp_refresh` は `~/.duduclaw/mcp_tokens.db` を後ろ盾とする長寿命クレデンシャル（`ddc_refresh_<env>_<64hex>`、90 日、失効可能、hash のみ保存）で、Claude Desktop が auth-fail 後にサイレント切断してリトライしない問題を解決；GVU には「縮小不変式」を伴う `SoulPatchOp::Consolidate` バリアントを追加し、SOUL.md が 150 行／8KB のハードリミットに近づいたとき自己トリガーで統合できるようにした
- **v1.15.2** — `agent_update_soul` 信頼バックドアの封鎖：従来は SOUL.md を書き込んだ後に `soul_guard::accept_soul_change` を呼んで完全性 hash を更新しておらず、正当な呼び出しのたびに永続的な stored-vs-current drift が残っていました；さらに呼び出しチェーン全体が `tool_calls.jsonl` に書かず、バックドアが事後分析から完全に不可視でした。v1.15.2 は audit row を補完し（成功 + 4 種類の拒否パスすべてを記録、hash プレフィックス 16 文字）、書き込みのたびに fingerprint を同期します
- **v1.15.1** — GVU SOUL.md 無制限成長の修正：agnes/SOUL.md が 5 回の GVU cycle で 61 行から 592 行に膨張。3 層防御：(1) `strip_proposal_meta` が legacy パスで `## 診斷` / `## rationale` / `## expected_improvement` などの meta セクションを除去；(2) `SOUL_MAX_LINES = 150` / `SOUL_MAX_BYTES = 8KB` のハードリミットを ASI コンテンツ重み閾値とは独立に設定；(3) structured な `SoulPatch { section, op, content }` と `apply_patch_to_soul` を追加し、Generator→Verifier→Updater 全チェーンを通した
- **v1.15.0** — Cross-Platform PTY Pool + Worker：Anthropic が OAuth サブスクリプションアカウントの `claude -p` をブロックした後の公式代替パス。新 crate `duduclaw-cli-runtime`（`portable-pty` ConPTY/openpty クロスプラットフォーム + sentinel-framed in-band プロトコル + `PtyPool` semaphore + idle eviction + supervisor + restart policy）と `duduclaw-cli-worker`（localhost JSON-RPC + Bearer + `/healthz`、gateway は in-process または out-of-process を選択可能）；`channel_reply` は OAuth を REPL へ / API-key を `oneshot_pty_invoke + claude -p` へ；Phase 8 `pty_pool_*` Prometheus メトリクス；すべての失敗は legacy `tokio::process::Command` に fallback。デフォルトはオフ、`agent.toml [runtime] pty_pool_enabled = true` で有効化
- **v1.14.0** — RFC-23 Sensitive Data Redaction：新 crate `duduclaw-redaction`、内部データ（Odoo / shared wiki / file tools）を `<REDACT:CATEGORY:hash8>` token に置換してから LLM に送り、信頼境界（user channel reply、whitelist ツール egress）で自動復元；AES-256-GCM 暗号化 SQLite vault（per-agent 32-byte key、0o600 権限）+ TTL 7d 二段階 GC（mark→30 日後 purge）+ 5 個の組み込み profile + 5 層 enable/disable resolver + JSONL audit 10MB rotation
- **v1.13.1** — Odoo Test-Before-Save：`odoo.test` RPC は inline params を受け取り、Dashboard の「接続テスト」はフォームの現在値で直接 Odoo を叩き、先に保存する必要がない；inline credential を空にすると保存済みの鍵に fallback；同じ SSRF / HTTPS / db-name 検証チェーン、`scrub_odoo_error()` は 240 文字で切り詰めて HTML エラーページの漏洩を防ぐ
- **v1.13.0** — Runtime-health overhaul（16 件の issue / 2 ラウンドの修正）：GVU/SOUL 自己進化の復旧、`[prompt] mode = "minimal"` Anthropic Skills スタイルのシステムプロンプト追加、`[budget] max_input_tokens` 圧縮パイプライン、async session summarizer、TF-IDF wiki 関連性ランキング、`duduclaw lifecycle flush` 四半期コールド／ホット分離 CLI
- **v1.12.x** — W22-P0 ADR-002 `x-duduclaw` capability negotiation（HTTP 422 早期失敗）+ ADR-004 Secret Manager + RFC-22 マルチ agent 協調修正（agnes が偽の子 agent 応答を捏造 / autopilot の大量誤トリガー / channel パスの token 未記録）+ `duduclaw weekly-report` サブコマンド
- **v1.11.0** — RFC-21（[Issue #21](https://github.com/zhixuli0406/DuDuClaw/issues/21)）：`duduclaw-identity` crate（IdentityProvider trait + Wiki/Notion/Chained の 3 実装）+ Odoo per-agent 認証分離（`OdooConnectorPool` がグローバル admin シングルトンを置換）+ shared wiki `.scope.toml` SoT 名前空間ポリシー
- **v1.10.0** — Wiki RL Trust Feedback：`WikiTrustStore` per-agent SQLite trust、`CitationTracker` 二段 LRU + bounded-time eviction で DoS 防止、`WikiJanitor` 毎日 pass（自動で corrected / archive マーキング / frontmatter 同期）+ sub-agent turn_id の貫通 + multi-process flock + atomic batch upsert
- **v1.9.4** — `duduclaw-durability` 五大持続性機構（idempotency / retry / circuit breaker / checkpoint / DLQ）+ `duduclaw-governance` PolicyRegistry + MCP HTTP/SSE Transport + LOCOMO 記憶評価システム（毎日 03:00 UTC 評価 + 200 件の golden QA）+ LLM Fallback + Discord RESUME + Web ReliabilityPage

</details>

---

## 目次

- [DuDuClaw とは？](#what)
- [主要機能](#features)
- [競合比較](#comparison)
- [エージェントディレクトリ構造](#directory)
- [Security Hooks](#security)
- [インストール](#install)
- [CLI コマンド](#cli)
- [プロジェクト構造](#structure)
- [技術選定](#tech)
- [テスト](#testing)
- [ドキュメント](#docs)
- [ライセンス](#license)

---

<a id="what"></a>

## DuDuClaw とは？

DuDuClaw は **Multi-Runtime AI Agent プラットフォーム**です——**Claude Code / Codex / Gemini** の三大 CLI を同時に AI バックエンドとしてサポートし、統一された `AgentRuntime` trait によりシームレスな切り替えと自動検出を実現します。

特定の単一 AI プロバイダーに縛られず、あなたの AI Agent に通信チャネル、記憶、自己進化、ローカル推論、アカウント管理といった完全なインフラを接続します。

コアコンセプト：

- **Multi-Runtime** — `AgentRuntime` trait が Claude / Codex / Gemini / OpenAI-compat の 4 バックエンドを統一、`RuntimeRegistry` が自動検出、per-agent 設定
- **Plumbing = DuDuClaw** — チャネルルーティング、session 管理、記憶検索、アカウントローテーション、ローカル推論などのインフラを担当
- **ブリッジ = MCP Protocol** — `duduclaw mcp-server` が MCP Server として、チャネルと記憶ツールを AI Runtime に公開

```
AI Runtime (brain) — Claude CLI / Codex CLI / Gemini CLI / OpenAI-compat
  ↕ MCP Protocol (JSON-RPC 2.0, stdin/stdout)
DuDuClaw (plumbing)
  ├─ Channel Router — Telegram / LINE / Discord / Slack / WhatsApp / Feishu / WebChat
  ├─ Multi-Runtime — Claude / Codex / Gemini / OpenAI-compat 自動検出 + per-agent 設定
  ├─ Session Memory Stack — ネイティブ --resume + Instruction Pinning + Snowball Recap + Key-Fact Accumulator
  ├─ MCP Server — 80+ ツール（通信、記憶、Agent、Skill、推論、タスク、ナレッジベース、ERP）、per-agent 登録
  ├─ Evolution Engine — GVU² デュアルループ進化 + 予測駆動 + MistakeNotebook
  ├─ Inference Engine — llama.cpp / mistral.rs / Exo P2P / llamafile / MLX / ONNX
  ├─ Voice Pipeline — ASR (SenseVoice / Whisper) + TTS (Piper / MiniMax) + VAD (Silero)
  ├─ Account Rotator — 複数 OAuth + API Key ローテーション、予算追跡、ヘルスチェック、Cross-Provider Failover
  ├─ Browser Automation — 5 層自動ルーティング（API Fetch → Scrape → Headless → Sandbox → Computer Use）
  ├─ Worktree Isolation — Git worktree L0 サンドボックス、アトミックマージ、Agent ごと 5 個上限
  ├─ Wiki Knowledge Layer — L0-L3 四層ナレッジ構造 + 信頼重み + FTS5 + 自動注入
  ├─ ACP/A2A Server — `duduclaw acp-server` stdio JSON-RPC 2.0、Zed/JetBrains/Neovim 統合
  └─ Web Dashboard — React 19 SPA（23 ページ）、rust-embed で binary に組み込み
```

---

<a id="features"></a>

## 主要機能

### チャネルとメッセージング

| 機能 | 説明 |
|------|------|
| **七チャネル対応** | Telegram（long polling）、LINE（webhook）、Discord（Gateway WebSocket、op 6 RESUME + stall watchdog + 1-5s jitter）、Slack（Socket Mode）、WhatsApp（Cloud API）、Feishu（Open Platform v2）、WebChat（WebSocket）|
| **Per-Agent Bot** | 各 Agent が独立した Bot Token を持つことができ、同一プラットフォーム上で複数 Agent を並行運用 |
| **チャネルのホット起動／停止** | Dashboard でのチャネル追加／削除が即座に反映され、gateway の再起動不要 |
| **WebChat** | 組み込みの `/ws/chat` WebSocket エンドポイント、React フロントエンドでリアルタイム対話 |
| **Generic Webhook** | `POST /webhook/{agent_id}` + HMAC-SHA256 署名検証 |
| **Media Pipeline** | 画像の自動リサイズ（max 1568px）+ MIME 検出 + Vision 統合 |
| **Sticker システム** | LINE スタンプカタログ + 感情検出 + Discord emoji への等価マッピング |

### AI 実行と推論

| 機能 | 説明 |
|------|------|
| **MCP Server アーキテクチャ** | `duduclaw mcp-server` は 80+ ツールを提供し、通信、記憶、Agent 管理、推論、スケジューリング、Skill マーケット、タスクボード、共有ナレッジベース、Odoo ERP をカバー。各 agent ディレクトリの `.mcp.json` に登録（Claude CLI `-p --dangerously-skip-permissions` はプロジェクトレベルの設定のみ読み取る）、gateway 起動時に自動で作成／修復 |
| **MCP Refresh Tokens**（v1.16.0）| `~/.duduclaw/mcp_tokens.db` を後ろ盾とする長寿命クレデンシャル — token 形式 `ddc_refresh_<env>_<64hex>`、寿命 90 日、個別失効可能、hash のみ保存（元の token は決して残らない）；`authenticate_from_env` は prefix でクレデンシャルをルーティングし、旧版 `ddc_<env>_<32hex>` を完全保持；新 CLI `duduclaw mcp { issue-refresh-token \| revoke-token \| list-tokens }` で Claude Desktop が auth-fail 後にサイレント切断してリトライしない痛点を解決 |
| **Multi-Runtime** | `AgentRuntime` trait — Claude / Codex / Gemini / OpenAI-compat の 4 バックエンド、`RuntimeRegistry` 自動検出、per-agent 設定 |
| **ローカル推論エンジン** | 統一 `InferenceBackend` trait — llama.cpp（Metal/CUDA/Vulkan）/ mistral.rs（ISQ + PagedAttention）/ Exo P2P クラスタ / llamafile / MLX（Apple Silicon）/ OpenAI-compat HTTP |
| **三層信頼度ルーティング** | LocalFast → LocalStrong → CloudAPI、ヒューリスティックな信頼度スコアに基づき自動分流、CJK-aware token estimation |
| **InferenceManager** | マルチモード自動切替：Exo P2P → llamafile → Direct backend → OpenAI-compat → Cloud API、周期的ヘルスチェック + 自動 failover |
| **ネイティブ多輪 Session** | Claude CLI `--resume` と SHA-256 決定論的 session ID + history-in-prompt fallback（アカウントローテーション／stale session で自動リトライ）；Hermes スタイルの turn trimming（>800 chars, CJK-safe）；Direct API "system_and_3" ブレークポイントキャッシュ戦略 |
| **Session 記憶スタック** | Instruction Pinning（初回メッセージで Haiku がコアタスクを抽出 → session prompt 末尾に注入）+ Snowball Recap（毎輪 `<task_recap>` を前置するゼロコストの振り返り）+ P2 Key-Fact Accumulator（毎輪 2-4 件の事実 → FTS5 索引 → top-3 を注入、わずか 100-150 tokens vs MemGPT 6,500 tokens、−87%）|
| **Claude CLI 軽量パス** | `call_claude_cli_lightweight()` は `--effort medium --max-turns 1 --no-session-persistence --tools ""` で metadata タスク（圧縮、instruction/key-fact 抽出）を処理、25-40% のコスト削減 |
| **Claude CLI 安定化フラグ** | `--strict-mcp-config`（MCP 分離）+ `--exclude-dynamic-system-prompt-sections`（輪をまたぐ prompt 安定化、10-15% token 削減）、`--bare` は OAuth キーチェーンを壊すため v1.8.11 で削除 |
| **Direct API** | CLI を迂回して Anthropic Messages API を直接呼び出し、`cache_control: ephemeral` で 95%+ のキャッシュヒット率 |
| **Token 圧縮** | Meta-Token（BPE-like 27-47%）、LLMLingua-2（2-5x 有損）、StreamingLLM（無限長対話）|
| **Cross-Provider Failover** | `FailoverManager` ヘルス追跡、クールダウン、リトライ不能エラー検出 |
| **Cross-Platform PTY Pool**（v1.15.0）| OAuth アカウント専用のインタラクティブ REPL チャネル — クロスプラットフォーム `portable-pty`（ConPTY on Win 10 1809+、openpty on Unix）+ sentinel-framed in-band 応答プロトコル（scrollback scraping なし / sidecar なし）+ per-agent semaphore + idle eviction + health-check supervisor + restart policy。デフォルトはオフ、per-agent `agent.toml [runtime] pty_pool_enabled = true` で有効化；out-of-process モード（`worker_managed = true`）を選択して pool を `duduclaw-cli-worker` 子プロセスに移し localhost JSON-RPC で通信することも可能 |
| **PTY Pool Observability** | Phase 8 production-rollout メトリクス — `pty_pool_*` Prometheus counters（acquires / cache-hit / spawn / 3 種類の eviction 理由 / 4 種類の invoke outcome / duration histogram）+ `worker_health_misses_total` + `worker_restarts_total` + `pty_pool_managed_worker_active` モード gauge + `GET /api/runtime/status` JSON エンドポイント（loopback-only） |
| **Browser 自動化** | 5 層ルーティング（API Fetch → Static Scrape → Headless Playwright → Sandbox Container → Computer Use）、deny-by-default |

### 音声とマルチメディア

| 機能 | 説明 |
|------|------|
| **ASR 音声認識** | ONNX SenseVoice（ローカル）+ Whisper.cpp（ローカル）+ OpenAI Whisper API |
| **TTS 音声合成** | ONNX Piper（ローカル）+ MiniMax T2A |
| **VAD 音声活動検出** | ONNX Silero VAD |
| **Discord 音声チャンネル** | Songbird 統合、Discord 音声対話 |
| **LiveKit 音声ルーム** | WebRTC マルチ Agent 音声会議 |
| **ONNX 埋め込み** | BERT WordPiece tokenizer + ONNX Runtime ベクトル埋め込み |

### エージェントのオーケストレーションと進化

| 機能 | 説明 |
|------|------|
| **Sub-Agent オーケストレーション** | `create_agent` / `spawn_agent` / `list_agents` MCP ツール + `reports_to` 組織階層 + D3.js アーキテクチャ図；system prompt に "## Your Team" 子 Agent 名簿を自動注入 + 長い報告メッセージの自動ページ分割（Discord 1900 / Telegram 4000 / LINE 4900 / Slack 3900 byte budget、ラベル `📨 **agent** 的回報 (1/N)`）|
| **クロスシステム prompt 注入** | CLAUDE.md + CONTRACT.toml（must_not/must_always）+ SOUL.md + Wiki L0+L1 + key_facts top-3 + pinned_instructions を CLI/channel/dispatcher の 3 パスで一貫注入、Claude/Codex/Gemini/OpenAI の 4 runtime で挙動を整合 |
| **孤児レスポンスの復旧** | dispatcher 起動時に `reconcile_orphan_responses` が `bus_queue.jsonl` をスキャンし、crash/Ctrl+C/hotswap 後に残留した `agent_response` callback をアトミックに再生 |
| **GVU² デュアルループ進化** | 外ループ（Behavioral GVU — SOUL.md 進化）+ 内ループ（Task GVU — 即時タスクリトライ）、MistakeNotebook がループ間で記憶を共有 |
| **予測駆動進化** | Active Inference + Dual Process Theory、~90% の対話がゼロ LLM コスト；MetaCognition が 100 予測ごとに閾値を自己校正 |
| **4+2 層検証** | L1-Format / L2-Metrics / **L2.5-MistakeRegression** / L3-LLMJudge / **L3.5-SandboxCanary** / L4-Safety、最初の 4 層はゼロコスト |
| **Adaptive Depth** | MetaCognition 駆動の GVU 反復深度（3-7 輪）、過去の成功率に応じて自動調整 |
| **Deferred GVU** | gradient 蓄積 + 遅延リトライ（最大 3 回 deferral、72h スパンで 9-21 輪の有効反復）|
| **ConversationOutcome** | ゼロ LLM の対話結果検出（TaskType / Satisfaction / Completion）、zh-TW + en バイリンガル |
| **SOUL.md バージョン管理** | 24h 観察期間 + 自動ロールバック、atomic write（SHA-256 fingerprint）|
| **`SoulPatchOp::Consolidate`**（v1.16.0）| structured patch path に「縮小不変式」バリアントを追加 — 意味的には `Replace` と等価だが `apply_patch_to_soul` は新コンテンツが既存本文より短くない場合に拒否し、LLM は SOUL.md が 150 行 / 8KB のハードリミットに近づいたとき自己トリガーで統合できる |
| **`agent_update_soul` 信頼チェーン**（v1.15.2）| 書き込み後に自動で `soul_guard::accept_soul_change` を呼び完全性 fingerprint を同期 + 成功/4 種類の拒否パスすべてを `tool_calls.jsonl` に記録（hash プレフィックス 16 文字）、stored-vs-current drift とバックドアの不可視性問題を封鎖 |
| **Agent-as-Evaluator** | 独立した Evaluator Agent（Haiku でコスト制御）が敵対的検証を行い、構造化 JSON verdict を返す |
| **DelegationEnvelope** | 構造化された引き継ぎプロトコル — context / constraints / task_chain / expected_output、Raw payload と後方互換 |
| **TaskSpec ワークフロー** | 多段階タスク計画 — dependency-aware scheduling / auto-retry（3x）/ replan（最大 2 回）/ persistence |
| **Orchestrator テンプレート** | 5 ステップ計画戦略（Analyze → Decompose → Delegate → Evaluate → Synthesize）+ 複雑度ルーティング |
| **Skill ライフサイクル** | 7 段階管理 — Activation → Compression → Extraction → Reconstruction → Distillation → Diagnostician → Gap Analysis |
| **Skill 自動合成** | 繰り返される領域ギャップを検出 → 状況記憶から新 Skill を合成 → サンドボックス試用（TTL 管理）→ Agent 間で卒業昇格 |
| **Task Board** | SQLite タスク管理 — ステータス/優先度/割り当て追跡 + リアルタイム Activity Feed（WebSocket プッシュ）|
| **Autopilot ルールエンジン** | タスク委譲、通知、Skill トリガーの自動化 — タスク作成/ステータス変更/チャネルメッセージ/アイドル検出/Cron スケジュールに対応 |
| **共有ナレッジベース** | `~/.duduclaw/shared/wiki/` で Agent 間の知識（SOP、ポリシー、製品仕様）を共有 + 作者帰属 |
| **Wiki ナレッジ階層** | Vault-for-LLM に着想 — L0 Identity / L1 Core（毎回の対話に自動注入）/ L2 Context（毎日更新）/ L3 Deep（オンデマンド検索）、各ページに `trust` (0.0-1.0) 重みを付与；FTS5 unicode61 tokenizer が CJK 全文検索をサポート；`wiki_dedup` が重複ページを検出、`wiki_graph` が Mermaid ナレッジグラフを出力 |
| **Wiki 自動注入** | `build_system_prompt()` が L0+L1 ページを WIKI_CONTEXT に自動注入；CLI 対話、チャネル返信、dispatcher/cron の 3 つのシステム prompt 組み立てパスをカバー、Claude/Codex/Gemini/OpenAI の 4 runtime で一貫 |
| **Git Worktree L0 分離** | タスクごとに独立した worktree 作業領域（コンテナサンドボックスより安価）、atomic merge（dry-run pre-check + global `Mutex`）、`wt/{agent_id}/{adjective}-{noun}` のフレンドリーなブランチ名；agent ごと上限 5 個、グローバル 20 個；Snap workflow：create → execute → inspect → merge/cleanup |
| **ACP/A2A Protocol Server** | `duduclaw acp-server` が stdio JSON-RPC 2.0 サーバー（`agent/discover` / `tasks/send` / `tasks/get` / `tasks/cancel`）を提供、Agent Client Protocol と互換、Zed / JetBrains / Neovim IDE 統合をサポート；`.well-known/agent.json` AgentCard を出力 |
| **Reminder スケジューリング** | 一回限りのリマインダー（相対時間 `5m`/`2h`/`1d` または ISO 8601 絶対時間）、`direct` 静的メッセージまたは `agent_callback` 起床モード |

### 信頼性とガバナンス（v1.9.x で追加）

| 機能 | 説明 |
|------|------|
| **`duduclaw-durability` crate** | 五大持続性機構 — idempotency key 管理、指数バックオフリトライ（jitter）、三態サーキットブレーカー（Closed/Open/HalfOpen）、checkpoint レジューム、Dead Letter Queue 終態失敗メッセージ処理 |
| **`duduclaw-governance` crate** | PolicyRegistry + 4 種類の PolicyType（Rate/Permission/Quota/Lifecycle）+ quota_manager（soft/hard クォータ）+ error_codes（QUOTA_EXCEEDED / POLICY_DENIED の標準化）+ YAML ホットリロード + audit log |
| **LLM Fallback** | 主モデルが timeout/503/429/overloaded のとき自動で fallback モデルに切替、`is_llm_fallback_error` / `should_attempt_model_fallback` は純関数、hard deadline は統一して hard timeout エラーを返し fallback をトリガー |
| **Evolution Events システム** | 30+ event schema、async emitter（batch + retry）、query インターフェース、reliability 機構；HTTP endpoint を gateway に公開、Web ReliabilityPage で可視化 |
| **MCP HTTP/SSE Transport**（W20-P1/P2）| `duduclaw http-server --bind 127.0.0.1:8765` — `POST /mcp/v1/call`（単発 JSON-RPC ツール呼び出し）+ `GET /mcp/v1/stream`（SSE ロングコネクションイベントストリーム）+ `POST /mcp/v1/stream/call`（async + SSE push）+ Bearer 認証 + token bucket rate limit |
| **記憶 MCP scope 強制検証** | `memory:read` / `memory:write` scope を `store/read/search` の execute() エントリポイントでチェックし、v1.9.3 以前は任意の有効な API Key が scope を回避できた認証欠陥を修正 |
| **LOCOMO 記憶評価** | `memory_eval/` — retrieval_accuracy / retention_rate / locomo_integrity_check + cron_runner（毎日 03:00 UTC）+ 5 分の smoke_test P0 + 200 件の golden QA ゴールデンセット |

### セキュリティ

| 機能 | 説明 |
|------|------|
| **Claude Code Security Hooks** | 三層プログレッシブ防御 — Layer 1 ブラックリスト（<50ms）→ Layer 2 難読化検出（YELLOW+）→ Layer 3 Haiku AI 判定（RED only）|
| **脅威レベル状態機械** | GREEN → YELLOW → RED の自動昇降格、24h イベントなしで 1 レベル降格 |
| **SOUL.md ドリフト検出** | SHA-256 fingerprint のリアルタイム比較 |
| **Prompt Injection スキャン** | 6 カテゴリのルール、XML 区切りタグで注入防止 |
| **Secret 漏洩スキャン** | 20+ パターン（Anthropic/OpenAI/AWS/GitHub/Slack/Stripe/DB URL など）|
| **機微ファイル保護** | Read/Write/Edit の 3 方向で `secret.key`、`.env*`、`SOUL.md`、`CONTRACT.toml` を保護 |
| **行動契約** | `CONTRACT.toml` が `must_not` / `must_always` 境界を定義 + `duduclaw test` レッドチームテスト（9 シナリオ）|
| **統一マルチソース監査ログ** | `audit.unified_log` が 4 つの JSONL（`security_audit.jsonl` / `tool_calls.jsonl` / `channel_failures.jsonl` / `feedback.jsonl`）を統一エンベロープ（timestamp / source / event_type / agent_id / severity / summary / details）にマージ、Logs ページはソースフィルタ、重大度ドロップダウン、リアルタイムと履歴のタブ分割をサポート |
| **JSONL 監査ログ** | async 書き込み、Rust `AuditEvent` schema と互換のフォーマット |
| **CJK-Safe 文字列スライス** | `truncate_bytes` / `truncate_chars` の新モジュールが 31 箇所の `s[..s.len().min(N)]` byte-index スライスを置換（v1.8.11 のマルチバイト codepoint panic を修正）|
| **Per-Agent 鍵分離** | AES-256-GCM 暗号化保存、agent 間で鍵は互いに不可視 |
| **コンテナサンドボックス** | Docker / Apple Container（`--network=none`、tmpfs、read-only rootfs、512MB limit）|
| **Browser 自動化** | 5 層ルーティング（API Fetch → Static Scrape → Headless → Sandbox → Computer Use）、deny-by-default |

### アカウントとコスト

| 機能 | 説明 |
|------|------|
| **デュアルモードアカウントローテーション** | OAuth サブスクリプション（Pro/Team/Max）+ API Key ハイブリッド — 4 戦略（Priority/LeastCost/Failover/RoundRobin）|
| **ヘルス追跡** | Rate limit クールダウン（2min）、課金枯渇クールダウン（24h）、Token 失効追跡（30d/7d 事前警告）|
| **コストテレメトリ** | SQLite token 追跡、キャッシュ効率分析、200K 価格クリフ警告、適応的ルーティング（キャッシュ効率 <30% で自動的にローカルへ切替）|
| **Claude CLI バイナリ探索** | `which_claude()` / `which_claude_in_home()` が Homebrew（Intel + Apple Silicon）/ Bun / Volta / npm-global / `.claude/bin` / `.local/bin` / asdf shims / NVM バージョンディレクトリをスキャンし、launchd 起動時に binary が見つからない問題を修正 |
| **構造化失敗分類** | `FailureReason` 列挙（RateLimited / Billing / Timeout / BinaryMissing / SpawnError / EmptyResponse / NoAccounts / Unknown）+ 分類された zh-TW メッセージ + `channel_failures.jsonl` 監査記録 |

### 統合と拡張

| 機能 | 説明 |
|------|------|
| **Odoo ERP 統合** | `duduclaw-odoo` ミドルウェア — 15 個の MCP ツール（CRM/販売/在庫/会計/汎用検索レポート）、CE/EE をサポート、EditionGate 自動検出。Dashboard 設定ページは**テストしてから保存**をサポート（v1.13.1、credential を空にすると保存済みの鍵に fallback）+ **per-agent 認証分離**（v1.11.0、`OdooConnectorPool` がグローバル admin シングルトンを置換）|
| **Skill マーケット** | GitHub Search API リアルタイム索引 + 24h ローカルキャッシュ + セキュリティスキャン + Dashboard マーケットページ |
| **Prometheus メトリクス** | `GET /metrics` — requests、tokens、duration histogram、channel status |
| **CronScheduler** | `cron_tasks.jsonl` + cron 式、定時タスクの自動トリガー |
| **ONNX 埋め込み** | BERT WordPiece tokenizer + ONNX Runtime ベクトル埋め込み、セマンティック検索対応 |
| **Experiment Logger** | Trajectory recording、RL/RLHF オフライン分析をサポート |
| **Memory Decay スケジューリング** | 24h ごとにバックグラウンドで `run_decay` を実行：低重要度 + 30 日以上をアーカイブ → 封存 90 日以上を永久削除 |
| **RL Trajectory Collector** | チャネル対話中に `~/.duduclaw/rl_trajectories.jsonl` へ書き込み、`duduclaw rl` CLI が export/stats/reward 機能を提供、複合報酬（outcome×0.7 + efficiency×0.2 + overlong×0.1）|
| **Marketplace RPC** | `marketplace.list` が実在の MCP カタログ（Playwright, Browserbase, Filesystem, GitHub, Slack, Postgres, SQLite, Memory, Fetch, Brave Search）を提供、`~/.duduclaw/marketplace.json` でユーザー定義をマージ可能 |
| **Partner Portal** | SQLite `PartnerStore`（`~/.duduclaw/partner.db`）+ 7 RPCs（profile/stats/customers CRUD）+ 販売統計 |

### Web ダッシュボード

| 機能 | 説明 |
|------|------|
| **技術スタック** | React 19 + TypeScript + Tailwind CSS 4 + shadcn/ui、温かみのある amber カラー |
| **24 個のページ** | Dashboard / Agents / Channels / Accounts / Memory / Security / Settings / OrgChart / SkillMarket / Logs / WebChat / OnboardWizard / Billing / License / Report / PartnerPortal / Marketplace / KnowledgeHub / Odoo / Login / Users / Analytics / Export / **Reliability**（v1.9.4 で追加）|
| **Reliability ダッシュボード** | circuit breaker 状態 / retry 統計 / DLQ キュー深度 / evolution events リアルタイムデータ；`/reliability` ルート、`getEvolutionEvents` / `getReliabilityStats` / `getDlqItems` API を統合 |
| **リアルタイムログ** | BroadcastLayer tracing → WebSocket プッシュ、WS ハートビート ping/pong（server 30s / client 25s）+ 60s アイドルでクローズ |
| **Logs 履歴ページの書き直し** | ソースフィルタ chips（すべて / セキュリティ / ツール呼び出し / チャネル失敗 / フィードバック）+ リアルタイム件数カウント + 重大度ドロップダウン + 重大度で着色した左枠（emerald/amber/rose）+ クリックで JSON 詳細を展開 |
| **Memory ページ Key Insights** | 4 番目のタブが P2 Key-Fact Accumulator の蓄積した構造化インサイト（`key_facts` テーブル）+ `access_count` badge + タイムスタンプ + ソース metadata を表示 |
| **Memory ページ進化履歴** | SOUL.md バージョン履歴 + 前/後メトリクスの差分（positive feedback / prediction error / user corrections）+ ステータスバッジ（Confirmed / RolledBack / Observing）|
| **Toast 通知システム** | モジュールスコープのイベントバス、max-5 queue、自動クローズ、暖色系 stone/amber/emerald/rose バリアント、`prefers-reduced-motion` を尊重 |
| **組織アーキテクチャ図** | D3.js インタラクティブな Agent 階層の可視化 |
| **ダーク／ライト切替** | システム設定に追従、手動切替もサポート |
| **国際化** | zh-TW / en / ja-JP 三言語対応（600+ 翻訳キー）|
| **Skill Market 三タブ** | Marketplace / Shared Skills / My Skills の三タブ構成 + Skill 採用フロー |
| **Autopilot 設定** | 自動化ルールの作成/管理/監視 + 履歴記録の閲覧 |
| **Session Replay** | 対話リプレイコンポーネント、タイムライン表示をサポート |

---

<a id="comparison"></a>

## 競合比較

| | **DuDuClaw** | **OpenClaw** | **IronClaw** | **Moltis** | **Dify** |
|---|---|---|---|---|---|
| 言語 | Rust | TypeScript | Rust | Rust | Python |
| チャネル | 7 | 25+ | 8 | 5 | 0 (API) |
| Multi-Runtime | **4 バックエンド（Claude/Codex/Gemini/OpenAI）** | - | - | - | 複数 LLM |
| MCP Server | **80+ ツール** | - | - | - | - |
| 自己進化エンジン | **GVU² デュアルループ** | - | - | - | - |
| ローカル推論 | **6 バックエンド + 三層信頼度ルーティング** | - | - | - | - |
| 音声 (ASR/TTS) | **4 ASR + 4 TTS provider** | - | - | - | - |
| Token 圧縮 | **3 種類の戦略** | - | - | - | - |
| Browser 自動化 | **5 層ルーティング** | - | - | - | - |
| コストテレメトリ | **キャッシュ効率分析** | - | 基本 | 基本 | 基本 |
| 行動契約 | **CONTRACT.toml + レッドチーム** | - | WASM サンドボックス | - | - |
| ERP 統合 | **Odoo 15 ツール** | - | - | - | - |
| セキュリティ監査 | **三層防御 + Hooks** | CVE-2026-25253 | WASM | 基本 | 中程度 |
| ライセンス | **Apache 2.0 (Open Core)** | MIT | オープンソース | オープンソース | $59+/月 |

---

<a id="directory"></a>

## エージェントディレクトリ構造

各 Agent は 1 つのフォルダであり、その構造は Claude Code と完全に互換です：

```
~/.duduclaw/agents/
├── dudu/                    # メイン Agent
│   ├── .claude/             # Claude Code 設定
│   │   └── settings.local.json
│   ├── .mcp.json            # MCP Server 設定（DuDuClaw platform tools + Playwright などの agent 専用 MCP）
│   │                        # gateway 起動時に自動で作成/修復；Claude CLI `-p` モードはこのファイルのみ読む
│   ├── SOUL.md              # 人格定義（SHA-256 保護）
│   ├── CLAUDE.md            # Claude Code ガイドライン（CLAUDE_WIKI テンプレートを含む）
│   ├── CONTRACT.toml        # 行動契約（must_not / must_always）、system prompt に自動注入
│   ├── agent.toml           # DuDuClaw 設定（モデル、予算、ハートビート、runtime、capabilities）
│   ├── SKILLS/              # スキルセット（進化エンジンが自動生成可能）
│   ├── wiki/                # Wiki ナレッジベース（L0-L3 階層 + trust 重み + FTS5）
│   ├── memory/              # 日次ノート + memory.db（予測偏差）+ key_facts テーブル
│   ├── tasks/               # TaskSpec ワークフロー永続化（JSON）
│   └── state/               # ランタイム状態（SQLite：sessions.pinned_instructions など）
│
└── coder/                   # 別の Agent
    └── ...
```

`duduclaw migrate` を使うと、旧版の `agent.toml` を Claude Code 互換フォーマットに自動変換できます。

---

<a id="security"></a>

## Security Hooks

DuDuClaw は Claude Code の Hook システムの上に三層プログレッシブ防御を構築しています：

```
                    ┌─────────────────────────────────────┐
  SessionStart ──→  │ session-init.sh                     │  鍵権限検証 + 環境初期化
                    └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  UserPrompt   ──→  │ inject-contract.sh                  │  CONTRACT.toml ルール注入
                    └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  PreToolUse   ──→  │ bash-gate.sh (Bash)                 │  Layer 1: ブラックリスト (<50ms)
     (Bash)         │   ├─ Layer 2: 混淆偵測 (YELLOW+)    │  Layer 2: base64/eval/外滲
                    │   └─ Layer 3: Haiku AI (RED only)   │  Layer 3: AI セキュリティ判定
                    └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  PreToolUse   ──→  │ file-protect.sh → ai-review.sh     │  機微ファイル保護 + AI 審査
  (Write|Edit|Read) └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  PostToolUse  ──→  │ secret-scanner.sh → audit-logger.sh │  Secret スキャン → async 監査
                    └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  Stop         ──→  │ threat-eval.sh                      │  脅威レベルの再評価
                    └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  ConfigChange ──→  │ config-guard.sh                     │  設定改竄検出
                    └─────────────────────────────────────┘
```

### 脅威レベル状態機械

| レベル | トリガー条件 | 防御動作 |
|------|---------|---------|
| **GREEN** (デフォルト) | 通常操作 | Layer 1 ブラックリスト + ファイル保護 + Secret スキャン |
| **YELLOW** | 1 時間以内に ≥ 2 回の遮断 | +Layer 2 難読化検出 + 外部ネットワーク制限 |
| **RED** | 注入/eval 攻撃を検出 | +Layer 3 Haiku AI が全コマンドを判定 + AI ファイル審査 |

降格：24 時間イベントなしで自動的に 1 レベル降格（RED→YELLOW→GREEN）。

---

<a id="install"></a>

## インストール

### npm（推奨・Windows を含む全プラットフォーム）

```bash
npm install -g duduclaw
```

インストール完了後、対応プラットフォームの**プリコンパイル済み binary** が自動でダウンロードされます（macOS ARM64/x64、Linux x64/ARM64、Windows x64 をサポート）。**コンパイラ・Rust・MSVC Build Tools は一切不要**です。Windows ユーザーは事前に [Node.js](https://nodejs.org/) を入れるだけで、これが唯一の前提条件です。

> **⚠️ インストール中に Rust / MSVC Build Tools（~2GB）のインストールとコンパイル（約 1.5 時間）を求められたら、それは間違った経路です。**
> それは「[ソースからビルド](#ソースからビルド)」経路で、コードを変更する開発者のみが必要とします。通常利用では必ず上記の `npm install -g duduclaw`（または下記の Homebrew / ワンライナー）を使ってください。公式のプリビルド binary を直接ダウンロードします。

### Homebrew（macOS / Linux）

```bash
brew install zhixuli0406/tap/duduclaw
```

### ワンライナーインストール

**macOS / Linux：**

```bash
curl -fsSL https://raw.githubusercontent.com/zhixuli0406/DuDuClaw/main/scripts/install.sh | sh
```

**Windows（PowerShell）：**

```powershell
irm https://raw.githubusercontent.com/zhixuli0406/DuDuClaw/main/scripts/install.ps1 | iex
```

> ワンライナーインストーラーは**最新リリース**を自動検出して対応プラットフォームのプリビルド binary をダウンロードします（こちらもコンパイル不要）。GitHub のダウンロードに失敗した場合のみソースビルドを尋ねます（その際は `npm install -g duduclaw` を推奨）。`DUDUCLAW_VERSION` 環境変数で特定バージョンに固定できます。

### Python SDK（任意のライブラリ、CLI ではありません）

> **重要**：コアの gateway / CLI（`duduclaw` コマンド）は **Rust バイナリ**で、上記の **npm** または **Homebrew** でインストールすれば**フル機能**が使えます。Skill セキュリティスキャンとチャネル応答はすべて Rust ネイティブの経路で処理されるため、**Python 依存は不要**です。
> PyPI 上の `duduclaw` は**純粋な Python ライブラリ**（`import duduclaw` 用）であり、**コマンドラインツールは含まれません**。したがって `pipx install duduclaw` は失敗します（No apps associated with package）。これは想定された動作です。

`pip install duduclaw` は**コア機能には任意**で、次の場合のみ必要です：

- 自身の Python コードで `duduclaw` を `import` したいとき（agents / channels / mcp / memory_eval モジュール）。
- スタンドアロンのメモリ評価ツール（LOCOMO）を実行するとき。

> **高度なローカル推論（MLX リフレクション / LLMLingua-2 圧縮）** は別の opt-in 機能で、`mlx_lm` や `llmlingua` などの ML パッケージに依存します。`duduclaw` の PyPI パッケージ**ではありません**。必要に応じて `inference.toml` に従って個別にインストールしてください。

任意のライブラリをインストールするには：

```bash
pip install duduclaw
```

このコマンドは以下の依存をインストールします：

| パッケージ | 最低バージョン | 用途 |
|------|---------|------|
| `anthropic` | ≥ 0.40 | 自身の Python コードからの Claude API 直接呼び出し |
| `httpx` | ≥ 0.27 | 非同期 HTTP クライアント（アカウントローテーション、ヘルスチェック）|
| `pyyaml` | ≥ 6.0 | 設定ファイルのパース |

#### macOS（Homebrew Python）／その他の externally-managed 環境

`error: externally-managed-environment`（[PEP 668](https://peps.python.org/pep-0668/)）と表示される場合、システム Python への直接インストールは禁止されています。仮想環境を使用してください：

```bash
# venv
python3 -m venv .venv && source .venv/bin/activate
pip install --upgrade duduclaw

# または uv を使用（本プロジェクトで採用済み、より高速）
uv venv && uv pip install --upgrade duduclaw
```

インストール済みバージョンの確認：

```python
import duduclaw
print(duduclaw.__version__)   # 実際にインストールされた PyPI バージョンを反映
```

> `__version__` は `importlib.metadata` 経由でインストール済みパッケージのメタデータ（`pyproject.toml`）から動的に読み込まれます。ソースチェックアウト（pip 未インストール）では組み込み文字列にフォールバックし、`scripts/release.sh` のドリフトガードによって他プラットフォームのバージョンと同期されます。

開発環境では追加でインストール：

```bash
pip install duduclaw[dev]
# 含む：pytest>=8, pytest-asyncio>=0.24, ruff>=0.8
```

### ソースからビルド

```bash
git clone https://github.com/zhixuli0406/DuDuClaw.git
cd DuDuClaw

# （任意）import duduclaw ライブラリやメモリ評価ツールが必要な場合のみ。コアのビルドには不要
# pip install duduclaw

# Dashboard をビルド
cd web && npm ci --legacy-peer-deps && npm run build && cd ..

# Rust binary をビルド（Dashboard を含む）
cargo build --release -p duduclaw-cli -p duduclaw-gateway --features duduclaw-gateway/dashboard

# 初回設定
./target/release/duduclaw onboard

# 起動
./target/release/duduclaw run
```

> **前提要件**：[Rust](https://rustup.rs/) 1.85+、[Python](https://www.python.org/) 3.9+、[Node.js](https://nodejs.org/) 20+、および少なくとも 1 つの AI CLI：[Claude Code](https://docs.anthropic.com/en/docs/claude-code)、[Codex](https://github.com/openai/codex)、[Gemini CLI](https://github.com/google-gemini/gemini-cli)（いずれか 1 つまたは複数）

---

<a id="cli"></a>

## CLI コマンド

```
duduclaw onboard             # インタラクティブな初回設定
duduclaw run                 # ワンクリック起動（gateway + channels + heartbeat + cron + dispatcher）
duduclaw migrate             # agent.toml を Claude Code フォーマットに変換
duduclaw mcp-server          # MCP Server を起動（AI Runtime 用、stdio JSON-RPC 2.0）
duduclaw http-server         # MCP HTTP/SSE Transport を起動（Bearer 認証、デフォルト 127.0.0.1:8765）
duduclaw acp-server          # ACP/A2A Server を起動（IDE 統合：Zed/JetBrains/Neovim）
duduclaw gateway             # WebSocket gateway server のみ起動

duduclaw agent               # CLI インタラクティブ対話
duduclaw agent list          # すべての Agent を一覧表示
duduclaw agent create        # 新規 Agent を作成（業種テンプレート指定可能）
duduclaw agent inspect       # Agent 詳細を表示
duduclaw agent pause         # Agent を一時停止
duduclaw agent resume        # Agent を再開
duduclaw agent edit          # Agent 設定を編集
duduclaw agent remove        # Agent を削除

duduclaw test <agent>        # レッドチームセキュリティテスト（9 個の組み込みシナリオ + JSON レポート）
duduclaw status              # システムヘルススナップショット
duduclaw doctor              # ヘルス診断
duduclaw wizard              # 業種テンプレートのインタラクティブ設定
duduclaw evolution finalize  # 期限切れの SOUL.md 観察ウィンドウを一括回収（--dry-run / --agent <id>）

duduclaw rl export           # RL trajectory をエクスポート（~/.duduclaw/rl_trajectories.jsonl）
duduclaw rl stats            # Agent ごとの trajectory 統計
duduclaw rl reward           # 複合報酬を計算（outcome×0.7 + efficiency×0.2 + overlong×0.1）

duduclaw service install     # システムサービスとしてインストール
duduclaw service start/stop  # システムサービスの起動／停止
duduclaw service status      # サービス状態
duduclaw service logs        # サービスログ
duduclaw service uninstall   # システムサービスを削除

duduclaw license activate    # ライセンスを有効化
duduclaw license status      # ライセンス状態
duduclaw license verify      # ライセンスを検証
duduclaw update              # 更新の確認とインストール
duduclaw version             # バージョン情報
```

---

<a id="structure"></a>

## プロジェクト構造

```
DuDuClaw/
├── crates/                         # Rust crates (20 個)
│   ├── duduclaw-core/              # 共有型、traits (Channel, MemoryEngine)、エラー定義
│   ├── duduclaw-agent/             # Agent 登録、ハートビート、予算、契約、skill loader/registry
│   ├── duduclaw-auth/              # マルチユーザー認証（Argon2 パスワード、JWT、ACL ロール権限）
│   ├── duduclaw-security/          # AES-256-GCM、SOUL guard、input guard、audit、key vault
│   ├── duduclaw-container/         # Docker / Apple Container / WSL2 サンドボックス実行
│   ├── duduclaw-memory/            # SQLite + FTS5 全文検索 + ベクトル埋め込み + 評価 batch query API
│   ├── duduclaw-inference/         # ローカル推論エンジン（llama.cpp / mistral.rs / ONNX / Exo / llamafile）
│   ├── duduclaw-gateway/           # Axum サーバー、7 チャネル、session、GVU²、prediction、cron、dispatcher、LLM fallback、evolution events、PTY pool 統合
│   ├── duduclaw-bus/               # tokio broadcast + mpsc メッセージルーティング
│   ├── duduclaw-bridge/            # PyO3 Rust↔Python ブリッジ層
│   ├── duduclaw-odoo/              # Odoo ERP ミドルウェア (JSON-RPC, CE/EE, 15 MCP tools)
│   ├── duduclaw-cli/               # clap CLI エントリ + MCP server (stdio + HTTP/SSE) + migrate + test
│   ├── duduclaw-dashboard/         # rust-embed で React SPA を組み込み
│   ├── duduclaw-desktop/           # デスクトップ wrapper（macOS/Windows/Linux）
│   ├── duduclaw-durability/        # 持続性フレームワーク（idempotency / retry / circuit breaker / checkpoint / DLQ）— v1.9.4 で追加
│   ├── duduclaw-governance/        # PolicyRegistry / quota_manager / error_codes / audit / approval — v1.9.4 で追加
│   ├── duduclaw-identity/          # IdentityProvider trait + Wiki/Notion/Chained の 3 実装 — v1.11.0 で追加
│   ├── duduclaw-redaction/         # ソース感知 redaction + 復元可能 vault（AES-256-GCM）+ 5 profile + JSONL audit — v1.14.0 で追加
│   ├── duduclaw-cli-runtime/       # クロスプラットフォーム PTY pool runtime（portable-pty / sentinel-framed）— v1.15.0 で追加
│   └── duduclaw-cli-worker/        # standalone PTY pool worker subprocess（localhost JSON-RPC + Bearer token）— v1.15.0 で追加
│
├── python/duduclaw/                # Python 拡張層
│   ├── channels/                   # LINE / Telegram / Discord チャネルプラグイン
│   ├── sdk/                        # Claude Code SDK chat + マルチアカウントローテーション
│   ├── evolution/                  # Skill Vetter セキュリティスキャン
│   ├── tools/                      # Agent 動的管理ツール
│   ├── agents/                     # capability manifest + capability-based router + memory_resolver（v1.9.4）
│   ├── mcp/                        # MCP API Key auth（key masking を含む）+ memory tools（store/read/search/namespace/quota）
│   └── memory_eval/                # LOCOMO 記憶評価（retrieval/retention + cron + 200 件の golden QA）— v1.9.4 で追加
│
├── npm/                            # npm 配布パッケージ
│   ├── duduclaw/                   # メインパッケージ（プラットフォーム非依存 wrapper + postinstall binary ダウンロード）
│   ├── darwin-arm64/               # macOS Apple Silicon プリコンパイル binary
│   ├── darwin-x64/                 # macOS Intel プリコンパイル binary
│   ├── linux-x64/                  # Linux x86-64 プリコンパイル binary
│   ├── linux-arm64/                # Linux ARM64 プリコンパイル binary
│   └── win32-x64/                  # Windows x64 プリコンパイル binary
│
├── web/                            # React Dashboard
│   └── src/
│       ├── components/             # UI コンポーネント (OrgChart, ApprovalModal, SessionReplay)
│       ├── pages/                  # 24 個のページ（ReliabilityPage v1.9.4 で追加を含む）
│       ├── stores/                 # Zustand 状態管理 (8 stores)
│       ├── lib/                    # API client (WebSocket JSON-RPC + evolution events / reliability HTTP)
│       └── i18n/                   # zh-TW / en / ja-JP
│
├── templates/                      # 業種テンプレート + Agent 役割テンプレート
│   ├── restaurant/                 # 飲食業（カスタマーサポート、予約、FAQ、能動的プッシュ）
│   ├── manufacturing/              # 製造業（設備監視、SOP、異常アラート）
│   ├── trading/                    # 貿易業（見積、注文、在庫、価格表）
│   ├── evaluator/                  # Evaluator Agent（敵対的検証）
│   ├── orchestrator/               # Orchestrator Agent（タスクオーケストレーション）
│   └── wiki/                       # Wiki ナレッジベーステンプレート
│
├── .claude/                        # Claude Code Hook セキュリティシステム
│   ├── settings.local.json         # Hook 設定（6 イベント × 10 スクリプト）
│   └── hooks/                      # 三層プログレッシブ防御スクリプト
│
├── docs/                           # 公開ドキュメント
│   ├── spec/                       # フォーマット仕様（SOUL.md / CONTRACT.toml）
│   ├── api/                        # WebSocket RPC + OpenAPI spec
│   ├── guides/                     # 開発ガイド（カスタム MCP ツールなど）
│   └── *.md                        # アーキテクチャ、デプロイ、進化エンジンなど
│
├── ARCHITECTURE.md                 # 完全なアーキテクチャ設計ドキュメント
└── CLAUDE.md                       # AI コラボレーション設計コンテキスト
```

---

<a id="tech"></a>

## 技術選定

| 項目 | 選択 | 理由 |
|------|------|------|
| AI 対話 | **Multi-Runtime（Claude / Codex / Gemini CLI）** | 単一プロバイダーに縛られない、自動検出 + per-agent 設定 |
| コア言語 | **Rust** | メモリ安全、高性能、単一 binary デプロイ |
| 拡張言語 | **Python (PyO3)** | Claude Code SDK 統合、チャネルプラグインの柔軟性 |
| フロントエンドフレームワーク | **React 19 + TypeScript** | リアルタイムデータ更新、成熟したエコシステム |
| UI スタイル | **shadcn/ui + Tailwind CSS 4** | 温かみがありカスタマイズ可能、性能良好 |
| データベース | **SQLite + FTS5** | 依存ゼロ、組み込み型、全文検索 |
| ツールプロトコル | **MCP (Model Context Protocol)** | Claude Code ネイティブサポート、stdin/stdout JSON-RPC |
| ローカル推論 | **ONNX Runtime + llama.cpp** | クロスプラットフォーム、Metal/CUDA/Vulkan GPU 加速 |
| 音声認識 | **SenseVoice + Whisper.cpp** | 多言語、ローカルオフライン、ゼロ API コスト |
| リアルタイム通信 | **WebRTC (LiveKit)** | 低遅延音声、多人数会議 |

---

<a id="testing"></a>

## テスト

```bash
# Rust テスト
cargo test --workspace --exclude duduclaw-bridge

# Python テスト
pip install pytest pytest-asyncio ruff
ruff check python/
pytest tests/python/ -v

# フロントエンド型チェック
cd web && npx tsc --noEmit
```

---

<a id="docs"></a>

## ドキュメント

- [ARCHITECTURE.md](ARCHITECTURE.md) — 完全なシステムアーキテクチャ設計
- [CLAUDE.md](CLAUDE.md) — AI コラボレーション設計コンテキストと原則
- [CHANGELOG.md](CHANGELOG.md) — バージョン変更履歴
- [docs/features/README.md](docs/features/README.md) — 機能の詳細解説（19 篇、zh-TW / ja-JP 翻訳を含む）
- [docs/features/feature-inventory.md](docs/features/feature-inventory.md) — 完全な機能一覧
- [docs/spec/soul-md-spec.md](docs/spec/soul-md-spec.md) — SOUL.md フォーマット仕様 v1.0
- [docs/spec/contract-toml-spec.md](docs/spec/contract-toml-spec.md) — CONTRACT.toml フォーマット仕様 v1.0
- [docs/api/README.md](docs/api/README.md) — WebSocket RPC プロトコル + JSON-RPC 2.0 インターフェース
- [docs/architecture/evolution-engine.md](docs/architecture/evolution-engine.md) — Evolution Engine v2 設計ドキュメント
- [docs/guides/deployment-guide.md](docs/guides/deployment-guide.md) — 本番環境デプロイガイド
- [docs/guides/development-guide.md](docs/guides/development-guide.md) — 開発者設定と Agent 開発
- [docs/guides/custom-mcp-tool.md](docs/guides/custom-mcp-tool.md) — カスタム MCP ツールのチュートリアル

---

<a id="license"></a>

## ライセンス

**Open Core モデル** — コアコードは [Apache License 2.0](LICENSE) を採用し、完全に自由に使用、修正、配布できます。

商業付加価値モジュール（`commercial/` ディレクトリ）はクローズドソースの有料で、以下を含みます：業種テンプレート、進化パラメータセット、エンタープライズダッシュボード、ライセンス検証。

詳細は [LICENSING.md](LICENSING.md) を参照。

---

<p align="center">
  🐾 Built with louis.li
</p>
