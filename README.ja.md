# DuDuClaw 🐾

<div align="center">

[繁體中文](README.md) · [English](README.en.md) · **日本語**

</div>

DuDuClaw は、Claude Code・Codex・Gemini などの AI コマンドラインツールを Telegram・LINE・Discord をはじめとする 9 つのメッセージングプラットフォームにつなぎ、あなたを覚えて自ら成長する 24 時間稼働の AI アシスタントに変えます。

必要なのは Rust バイナリ 1 つだけ。チャネルルーティング、会話メモリ、マルチアカウントローテーション、行動ガードレール、ローカル推論、Web ダッシュボードをすべて内蔵。AI の頭脳は Claude・Codex・Gemini・Antigravity、あるいは任意の OpenAI 互換 API へいつでも切り替えられ、設定とメモリは自分のマシンに残ります。コアは Apache 2.0 ライセンスです。

[![CI](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml/badge.svg)](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml)
[![Version](https://img.shields.io/badge/version-1.43.0-blue)](https://github.com/zhixuli0406/DuDuClaw/releases)
[![npm](https://img.shields.io/npm/v/duduclaw?logo=npm)](https://www.npmjs.com/package/duduclaw)
[![PyPI](https://img.shields.io/pypi/v/duduclaw?logo=pypi)](https://pypi.org/project/duduclaw/)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE)

https://github.com/user-attachments/assets/9f18408a-cf46-4db2-9ab0-dcc8db2486fc

## 目次

- [なぜ DuDuClaw なのか](#why)
- [アーキテクチャ概要](#architecture)
- [インストール](#install)
- [クイックスタート](#quickstart)
- [機能一覧](#features)
- [CLI コマンド](#cli)
- [信頼とセキュリティ](#trust)
- [他製品との比較](#comparison)
- [ドキュメント](#docs)
- [ライセンス](#license)

<a id="why"></a>

## なぜ DuDuClaw なのか

ターミナルでときどき `claude` や `gemini` を使うだけなら、純正 CLI で十分です。しかし LINE 公式アカウントに AI を常駐させたい、チームの Discord を任せたい、役割の違う複数エージェントを同時に動かしたい、となった瞬間、インフラ層を丸ごと自作する羽目になります。DuDuClaw はその層を最初から提供します:

| ニーズ | 純正 CLI | DuDuClaw |
|---|---|---|
| Telegram / LINE / Discord 対応 | ターミナルのみ | 9 チャネル、エージェントごとの bot token |
| マルチ LLM フェイルオーバー | 手動再起動 | 4 種のローテーション戦略 + クロスプロバイダ failover |
| LLM 切替時のコンテキスト | 消失 | 完全保持 |
| 会話メモリと知識ベース | 単発セッション | SQLite 時系列メモリ + 階層 wiki を自動注入 |
| ツールの LLM 間共有 | ベンダーごとに書き直し | 130+ MCP ツールを一度書けば 5 バックエンドで共用 |
| ガードレール / 監査 / 秘密情報管理 | 自作 | ポリシーカーネル + OS サンドボックス + AES-256-GCM 内蔵 |

<a id="architecture"></a>

## アーキテクチャ概要

AI ランタイムが頭脳、DuDuClaw が配管、その間を MCP(JSON-RPC 2.0)がつなぎます。頭脳は差し替え可能、配管はそのまま:

```
AI Runtime (brain) — Claude Code / Codex / Gemini / Antigravity / OpenAI-compat
  ↕ MCP Protocol (JSON-RPC 2.0, stdin/stdout)
DuDuClaw (plumbing)
  ├─ Channel Router — Telegram / LINE / Discord / Slack / WhatsApp / Feishu
  │                    / Google Chat / Microsoft Teams / WebChat
  ├─ Multi-Runtime — 5 バックエンド自動検出、エージェントごとに設定
  ├─ Session Memory — ネイティブ --resume + 時系列メモリ + key facts + 階層 wiki
  ├─ MCP Server — 130+ ツール(チャネル、メモリ、エージェント、スキル、タスク、wiki、ERP)
  ├─ Evolution Engine — GVU² 二重ループ進化 + 予測駆動 + MistakeNotebook
  ├─ Security — PolicyKernel reference monitor + OS サンドボックス + redaction vault
  ├─ Inference Engine — llama.cpp / mistral.rs / Exo P2P / llamafile / MLX
  ├─ Account Rotator — OAuth + API キーのローテーション、予算追跡、ヘルスチェック
  └─ Web Dashboard — React 19 SPA(32 ページ)、rust-embed でバイナリに内蔵
```

Rust ワークスペースは 20 crate 構成:基盤の `duduclaw-core`、サービス層 `duduclaw-gateway`、統一 API 層 `duduclaw-llm`、ローカル推論 `duduclaw-inference`、認知メモリ `duduclaw-memory`、セキュリティ層 `duduclaw-security` など。全体設計は [ARCHITECTURE.md](ARCHITECTURE.md) を参照してください。

<a id="install"></a>

## インストール

### npm(推奨、Windows 含む全プラットフォーム)

前提条件は [Node.js](https://nodejs.org/) 20+ のみ:

```bash
npm install -g duduclaw
```

プラットフォームに対応するビルド済みバイナリ(macOS ARM64/x64、Linux x64/ARM64、Windows x64)が自動で入ります。コンパイラも Rust も不要です。

> ⚠️ インストール中に Rust / MSVC Build Tools の導入と 1.5 時間のコンパイルを求められたら、それは間違ったルートです。「ソースからビルド」はコントリビュータ向け。通常利用は上の npm コマンドを使ってください。

### Homebrew(macOS / Linux)

```bash
brew install zhixuli0406/tap/duduclaw
```

### ワンライナーインストール

```bash
# macOS / Linux
curl -fsSL https://raw.githubusercontent.com/zhixuli0406/DuDuClaw/main/scripts/install.sh | sh
```

```powershell
# Windows (PowerShell)
irm https://raw.githubusercontent.com/zhixuli0406/DuDuClaw/main/scripts/install.ps1 | iex
```

### デスクトップアプリ

Tauri 製のネイティブデスクトップ版。起動時にローカル gateway を自動で立ち上げ、CLI と `~/.duduclaw` を共有します。[Releases](https://github.com/zhixuli0406/DuDuClaw/releases) からダウンロード:

| プラットフォーム | ファイル | 備考 |
|------|------|------|
| macOS(Apple Silicon / Intel) | `DuDuClaw_*.dmg` | 署名 + Apple 公証済み、そのまま開けます |
| Windows x64 | `DuDuClaw_*_x64_en-US.msi` | Authenticode 証明書は未購入のため SmartScreen が警告します。「詳細情報」→「実行」で起動、気になる場合は CLI 版を |
| Linux | `*_amd64.AppImage` / `.deb` | 署名不要 |

### ソースからビルド

前提条件:[Rust](https://rustup.rs/) 1.85+、[Node.js](https://nodejs.org/) 20+。

```bash
git clone https://github.com/zhixuli0406/DuDuClaw.git
cd DuDuClaw
cd web && npm ci --legacy-peer-deps && npm run build && cd ..
cargo build --release -p duduclaw-cli -p duduclaw-gateway --features duduclaw-gateway/dashboard
./target/release/duduclaw run
```

### Python SDK(任意のライブラリ)

コアの gateway / CLI は Rust バイナリで、Python は不要です。PyPI の `duduclaw` は `import duduclaw` 用の純粋なライブラリ(agents / channels / mcp / memory_eval モジュール)で、コマンドラインツールを含みません。そのため `pipx install duduclaw` が失敗するのは想定どおりです。必要な場合:

```bash
pip install duduclaw
```

<a id="quickstart"></a>

## クイックスタート

AI の頭脳が 1 つ必要です(ブラウザのセットアップウィザードで後から設定も可能)。5 つから選べます:[Claude Code](https://docs.anthropic.com/en/docs/claude-code)、[Codex](https://github.com/openai/codex)、[Gemini CLI](https://github.com/google-gemini/gemini-cli)、Antigravity のいずれかを入れてログインする、任意の OpenAI 互換プロバイダの API キーを用意する、ローカル GGUF モデルを使う。

```bash
# 1. 初回セットアップ(省略可 — ブラウザのウィザードでも完了できます)
duduclaw onboard

# 2. まとめて起動(gateway + チャネル + スケジューラ + dispatcher)
duduclaw run

# 3. ダッシュボードを開く
open http://localhost:18789
```

初回アクセスではウィザードが案内します:AI バックエンドを選ぶ → 最初のエージェントを作る → 内蔵 WebChat でそのまま会話。あとは Channels ページに bot token を貼れば、同じエージェントを再起動なしで Telegram・LINE・Discord などに接続できます。

よく使う次の一歩:

```bash
duduclaw agent create      # エージェントを追加
duduclaw wizard            # 業種テンプレートでセットアップ
duduclaw status            # システムヘルスのスナップショット
duduclaw update            # アップデートの確認とインストール
duduclaw service install   # 起動時に自動開始(launchd / systemd)
```

<a id="features"></a>

## 機能一覧

| 領域 | 内蔵機能 | 詳細 |
|------|----------|------|
| チャネル | 9 チャネル(Telegram / LINE / Discord + 音声 / Slack / WhatsApp / Feishu / Google Chat / Teams / WebChat)、エージェントごとの bot、ホット起動/停止、プラットフォーム最適レンダリング、入力中インジケータ、長時間タスクの進捗ボード | [docs/features](docs/features/README.md) |
| マルチランタイム | Claude / Codex / Gemini / Antigravity / OpenAI-compat、自動検出、エージェントごとの設定、切替時もコンテキスト保持 | [ARCHITECTURE.md](ARCHITECTURE.md) |
| 統一 LLM API 層 | `duduclaw-llm` が 4 つのネイティブプロトコル(Anthropic Messages / OpenAI Responses / Gemini / OpenAI-compat)を単一の正規化リクエストでカバー。8 つの OpenAI-compat プリセット(DeepSeek / MiniMax / Groq / Together / Mistral / OpenRouter / xAI / Qwen)+ 価格レジストリ + クロスプロバイダ fallback を内蔵 | [ARCHITECTURE.md](ARCHITECTURE.md) |
| MCP サーバー | 138 ツール:チャネル、メモリ、エージェント編成、スキルマーケット、タスクボード、共有 wiki、Odoo ERP、computer use、live forking。stdio と HTTP/SSE の両トランスポート、外部には 7 ツールのみ公開 | [docs/api](docs/api/README.md) |
| メモリ | SQLite 時系列メモリ(事実の置換チェーン)、HippoRAG-lite 知識グラフ検索(Personalized PageRank)、エビングハウス忘却曲線によるアーカイブ、エージェント横断の共有 wiki | [docs/features](docs/features/README.md) |
| 自己進化 | GVU² 二重ループ + 予測駆動(会話の約 90% は LLM コストゼロ)、SOUL.md バージョン管理 + 24 時間観察期間つき自動ロールバック、MistakeNotebook のターン横断メモリ | [evolution-engine.md](docs/architecture/evolution-engine.md) |
| セキュリティ | PolicyKernel reference monitor(LLM 不使用、fail-closed)、macOS Seatbelt / Linux Landlock ネイティブサンドボックス、Docker / Apple Container / WSL2 コンテナサンドボックス、secret redaction vault、CONTRACT.toml 行動契約 + レッドチーム CLI | [SECURITY.md](SECURITY.md) |
| アカウントとコスト | OAuth + API キーのローテーション(4 戦略)、レート制限 / 課金クールダウン、キャッシュ効率分析つきコストテレメトリ、OAuth サブスクリプションアカウントを駆動するクロスプラットフォーム PTY プール | [docs/features](docs/features/README.md) |
| ローカル推論 | llama.cpp(Metal/CUDA/Vulkan)/ mistral.rs / Exo P2P / llamafile / MLX、3 段階の信頼度ルーティング。Whisper 音声認識とベクトル埋め込みも内蔵 | [docs/features](docs/features/README.md) |
| Live Forking | RFC-26:進行中のタスクを N 個の競合ブランチに分岐し、それぞれ copy-on-write 隔離、AI ジャッジが勝者を選んでマージ(デフォルト無効) | [docs/rfc](docs/rfc) |
| 自動アップデート | ダッシュボードからワンクリック、または無人更新(`auto_update = true`)。SHA-256 + Ed25519 の二重検証後にその場で再起動、開いているタブは自動リロード | [deployment-guide.md](docs/guides/deployment-guide.md) |
| Web ダッシュボード | React 19 + TypeScript SPA 32 ページ、バイナリに内蔵で追加デプロイ不要。zh-TW / en / ja 対応 | [docs/features](docs/features/README.md) |
| ERP 連携 | Odoo ブリッジ 15 MCP ツール(CRM / 販売 / 在庫 / 会計)、CE/EE 自動検出、エージェントごとの認証分離 | [docs/rfc](docs/rfc/RFC-21-operator-guide.md) |

全機能リストは [docs/features/feature-inventory.md](docs/features/feature-inventory.md)、バージョン履歴は [CHANGELOG.md](CHANGELOG.md) を参照してください。

<a id="cli"></a>

## CLI コマンド

```
duduclaw onboard             # 初回セットアップ(--yes でプロンプトをスキップ)
duduclaw run                 # まとめて起動(gateway + channels + heartbeat + cron + dispatcher)
duduclaw agent               # ターミナルで対話。サブコマンド create / list / inspect / pause / resume / run
duduclaw wizard              # 業種テンプレートでセットアップ
duduclaw status              # システムヘルスのスナップショット
duduclaw doctor              # ヘルス診断
duduclaw test <agent>        # レッドチームセキュリティテスト(内蔵 9 シナリオ)
duduclaw eval                # エージェント行動 eval スイートを実行
duduclaw update              # アップデートの確認とインストール
duduclaw service install     # システムサービスとして登録。start / stop / status / logs / uninstall も
duduclaw export / import     # ~/.duduclaw の書き出し / 取り込み(個人データの可搬性)
duduclaw migrate-from openclaw   # OpenClaw / Hermes / paperclip からの無痛移行(既定は dry-run、--apply で反映)
duduclaw mcp-server          # MCP サーバー起動(stdio JSON-RPC 2.0)
duduclaw http-server         # MCP HTTP/SSE トランスポート起動(Bearer 認証)
duduclaw acp-server          # ACP/A2A サーバー起動(Zed / JetBrains / Neovim 連携)
duduclaw license             # ライセンス管理(activate / status / redeem / rebind / …)
```

全 26 コマンドとサブコマンドは `duduclaw --help` で確認できます。開発者向けは[開発ガイド](docs/guides/development-guide.md)へ。

<a id="trust"></a>

## 信頼とセキュリティ

インストールする中身は完全に透明です:

- **npm パッケージの中身**:小さな JS ラッパーとプラットフォームバイナリ(`@duduclaw/<platform>` optionalDependencies)。`postinstall` はプラットフォームパッケージの存在確認のみ([`install.js`](npm/duduclaw/scripts/install.js))。任意の URL からのダウンロードや実行は一切ありません
- **テレメトリなし**:phone-home 通信ゼロ。すべての秘密情報は AES-256-GCM で暗号化され、あなたのマシンに残ります
- **特権昇格なし**:完全にユーザー空間で動作
- **メンテナ**:嘟嘟數位科技有限公司(台湾登記企業、統一編号 94139082)

各リリース資産には 3 種類の検証手段が付属します:SHA-256 チェックサム、[cosign](https://github.com/sigstore/cosign) keyless 署名、minisign Ed25519 署名(内蔵オートアップデータはこの署名を必須とし、未署名・改竄されたリリースを拒否します):

```bash
# SHA-256
shasum -a 256 -c duduclaw-darwin-arm64.tar.gz.sha256

# minisign(同じ公開鍵がバイナリにも埋め込まれています)
minisign -Vm duduclaw-darwin-arm64.tar.gz \
  -P RWTh5pOpk0YmdBgm3VyB2bzxFtajNLXr7zFDhbcc75TgM8YfeV+NSzXh
```

ビルド済みバイナリを信頼しない場合は、[ソースからのビルド](#install)が 3 コマンドで済みます。脆弱性の報告は [SECURITY.md](SECURITY.md) へ。

> なぜ「新しい」パッケージがバージョン 1.3x から始まるのか?DuDuClaw は公開前にプライベートリポジトリで数か月開発されました(400+ コミット)。全履歴は [git log](https://github.com/zhixuli0406/DuDuClaw/commits/main) にあります。

<a id="comparison"></a>

## 他製品との比較

| | DuDuClaw | OpenClaw | IronClaw | Dify |
|---|---|---|---|---|
| 言語 | Rust | TypeScript | Rust | Python |
| チャネル | 9 | 25+ | 8 | 0(API)|
| マルチランタイム | 5 バックエンド | 単一 | 単一 | マルチ LLM |
| MCP サーバー | 138 ツール | なし | なし | なし |
| 自己進化エンジン | GVU² 二重ループ | なし | なし | なし |
| ローカル推論 | 5 バックエンド + 信頼度ルーティング | なし | なし | なし |
| 行動契約 | CONTRACT.toml + レッドチーム | なし | WASM サンドボックス | なし |
| ライセンス | Apache 2.0(オープンコア)| MIT | オープンソース | $59+/月 |

<a id="docs"></a>

## ドキュメント

- [ARCHITECTURE.md](ARCHITECTURE.md):システムアーキテクチャ全体
- [docs/README.md](docs/README.md):公開ドキュメント索引(アーキテクチャ / RFC / ADR / 仕様 / ガイド)
- [docs/guides/deployment-guide.md](docs/guides/deployment-guide.md):本番デプロイ(Tailscale / Docker / systemd / 自動アップデート / 監視)
- [docs/guides/development-guide.md](docs/guides/development-guide.md):開発環境とエージェント開発
- [docs/guides/custom-mcp-tool.md](docs/guides/custom-mcp-tool.md):カスタム MCP ツールの作り方
- [docs/spec](docs/spec/soul-md-spec.md):SOUL.md / CONTRACT.toml フォーマット仕様
- [CHANGELOG.md](CHANGELOG.md):バージョン履歴

<a id="license"></a>

## ライセンス

オープンコアモデル:コアは [Apache License 2.0](LICENSE) で、自由に使用・改変・再配布できます。商用アドオンモジュール(`commercial/`)はクローズドソースの有償で、業種テンプレート、エンタープライズダッシュボード、ライセンス検証を含みます。詳細は [LICENSING.md](LICENSING.md)。

<p align="center">
  🐾 Built with louis.li
</p>
