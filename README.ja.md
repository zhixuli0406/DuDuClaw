# DuDuClaw 🐾

<div align="center">

[繁體中文](README.md) · [English](README.en.md) · **日本語**

</div>

DuDuClaw は、Claude Code・Codex・Antigravity などの AI コマンドラインツールを、Telegram・LINE・Discord をはじめとする 11 のメッセージングアプリに常駐し、納品前には独立した判定役の検証を通過し、使った費用を一円単位で記録する AI社員に変えます。

必要なのは Rust バイナリ 1 つだけ。チャネルルーティング、会話メモリ、マルチアカウントローテーション、行動ガードレール、ローカル推論、Web ダッシュボードをすべて内蔵。AI の頭脳は 12 種類の CLI バックエンド(Claude Code・Codex・Antigravity・Grok など。Gemini CLI は非推奨)と任意の OpenAI 互換 API の間でいつでも切り替えられ、設定とメモリは自分のマシンに残ります。コアは Apache 2.0 ライセンスです。

[![CI](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml/badge.svg)](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml)
[![Version](https://img.shields.io/badge/version-1.68.1-blue)](https://github.com/zhixuli0406/DuDuClaw/releases)
[![npm](https://img.shields.io/npm/v/duduclaw?logo=npm)](https://www.npmjs.com/package/duduclaw)
[![PyPI](https://img.shields.io/pypi/v/duduclaw?logo=pypi)](https://pypi.org/project/duduclaw/)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE)

https://github.com/user-attachments/assets/9f18408a-cf46-4db2-9ab0-dcc8db2486fc

## 目次

- [なぜ DuDuClaw なのか](#why)
- [アーキテクチャ概要](#architecture)
- [前提条件](#prerequisites)
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
| Telegram / LINE / Discord 対応 | ターミナルのみ | 11 チャネル、エージェントごとの bot token |
| マルチ LLM フェイルオーバー | 手動再起動 | 4 種のローテーション戦略 + クロスプロバイダ failover |
| LLM 切替時のコンテキスト | 消失 | 完全保持 |
| 会話メモリと知識ベース | 単発セッション | SQLite 時系列メモリ + 階層 wiki を自動注入 |
| ツールの LLM 間共有 | ベンダーごとに書き直し | 249 MCP ツールを一度書けば、Claude・Codex・Gemini・Antigravity・Grok・OpenAI 互換の各ランタイムから呼び出せる |
| ガードレール / 監査 / 秘密情報管理 | 自作 | ポリシーカーネル + OS サンドボックス + AES-256-GCM 内蔵 |
| 顧客に渡す一台まるごとの専用機 | Linux を自分で入れ、更新と改ざん対策も自前 | DuDuClaw OS イメージ:A/B アップデートとロールバック + 読み取り専用ルート、電源を入れるだけ;人と AI がデスクトップを共用しても日常利用の邪魔をしない |

<a id="architecture"></a>

## アーキテクチャ概要

AI ランタイムが頭脳、DuDuClaw が配管、その間を MCP(JSON-RPC 2.0)がつなぎます。頭脳は差し替え可能、配管はそのまま:

```
AI Runtime (brain) — Claude Code / Codex / Antigravity / Grok / … (12 CLIs) / OpenAI-compat
  ↕ MCP Protocol (JSON-RPC 2.0, stdin/stdout)
DuDuClaw (plumbing)
  ├─ Channel Router — Telegram / LINE / Discord / Slack / WhatsApp / Feishu
  │                    / Google Chat / Microsoft Teams / WeCom / DingTalk / WebChat
  ├─ Multi-Runtime — 13 のランタイム ID(12 CLI + OpenAI-compat)、自動検出、エージェントごとに設定
  ├─ Session Memory — ネイティブ --resume + 時系列メモリ + key facts + 階層 wiki
  ├─ MCP Server — 249 ツール(チャネル、メモリ、エージェント、スキル、タスク、wiki、ERP)
  ├─ Evolution Engine — 予測駆動 + AEE playbook ルール + MistakeNotebook
  ├─ Security — PolicyKernel reference monitor + OS サンドボックス + redaction vault
  ├─ Inference Engine — OpenAI 互換ローカルサーバー(llama-server / Ollama / vLLM)/ llamafile
  ├─ Account Rotator — OAuth + API キーのローテーション、予算追跡、ヘルスチェック
  └─ Web Dashboard — React 19 SPA、rust-embed でバイナリに内蔵
```

Rust ワークスペースは 24 crate 構成:基盤の `duduclaw-core`、サービス層 `duduclaw-gateway`、統一 API 層 `duduclaw-llm`、ローカル推論 `duduclaw-inference`、認知メモリ `duduclaw-memory`、セキュリティ層 `duduclaw-security` など。全体設計は [ARCHITECTURE.md](ARCHITECTURE.md) を参照してください。

ローカルモデルの実験的な校正ルーティングは Varun Kotte の [UCCI(arXiv:2605.18796)](https://arxiv.org/abs/2605.18796) に基づきます。設定とデータ準備は [UCCI calibrated cascade](docs/features/57-ucci-calibrated-cascade.md) を参照してください。

同じ gateway + dashboard は、マシン一台まるごとという形でも提供しています。[DuDuClaw OS](https://github.com/zhixuli0406/DuDuClaw-OS) は Yocto でビルドしたアプライアンスイメージで、Yocto レイヤーとリリースパイプラインは独立したリポジトリに置き、本リポジトリの Rust ワークスペースを剪定済みスナップショットとして取り込んでいます。詳しくは下記のインストール節を参照してください。

<a id="prerequisites"></a>

## 前提条件

DuDuClaw 自体には LLM が含まれません。まず AI の頭脳を用意してください(ブラウザのセットアップウィザードで後から設定することも可能です):

- 対応する AI CLI を 1 つ入れて API キーを設定する。例:[Claude Code](https://docs.anthropic.com/en/docs/claude-code)、[Codex](https://github.com/openai/codex)、Antigravity(全一覧は [multi-runtime](docs/features/ja-JP/13-multi-runtime.md)。[Gemini CLI](https://github.com/google-gemini/gemini-cli) は非推奨で v1.69.0 で削除)。Anthropic と Google はサードパーティ製品で使われる個人向けサブスクリプションのトークンをブロックしており、停止されたアカウントもあるため、API キーを使ってください
- 任意の OpenAI 互換プロバイダの API キーを用意する
- あるいはローカルモデルを OpenAI 互換サーバー(llama-server、Ollama、vLLM、llamafile)の背後で動かす(クラウドアカウント不要)

<a id="install"></a>

## インストール

### デスクトップアプリ(個人利用におすすめ)

Tauri 製のネイティブビルド。起動するだけでローカル gateway が自動で立ち上がり、ターミナル操作は一切不要です。CLI と `~/.duduclaw` を共有します。[Releases](https://github.com/zhixuli0406/DuDuClaw/releases) からダウンロード:

| プラットフォーム | ファイル | 備考 |
|------|------|------|
| macOS(Apple Silicon / Intel) | `DuDuClaw_*.dmg` | 署名 + Apple 公証済み、そのまま開けます |
| Windows x64 | `DuDuClaw_*_x64_en-US.msi` | Authenticode 証明書は未購入のため SmartScreen が警告します。「詳細情報」→「実行」でインストール可能。解除手順は [docs/guides/desktop-unblock.md](docs/guides/desktop-unblock.md) を参照 |
| Linux | `*_amd64.AppImage` / `.deb` | 署名不要 |

> macOS 版デスクトップアプリの最新は v1.66.1([desktop-v1.66.1](https://github.com/zhixuli0406/DuDuClaw/releases/tag/desktop-v1.66.1))です。v1.67.0 の macOS 版は Apple の公証で失敗したため公開されていません。Windows と Linux は v1.67.0 があります。

開けばそれだけで完結します——アプリ内のウィザードが AI バックエンドの選択と最初のエージェント作成まで案内してくれるので、コマンドを打つ必要はありません。

### npm(上級者 / サーバー用途、Windows 含む全プラットフォーム)

サーバーで動かしたい、スクリプトで自動化したい、あるいは単にコマンドラインが好き、という場合はこちら。前提条件は [Node.js](https://nodejs.org/) 20+ のみ:

```bash
npm install -g duduclaw
```

プラットフォームに対応するビルド済みバイナリ(macOS ARM64/x64、Linux x64/ARM64、Windows x64)が自動で入ります。コンパイラも Rust も不要です。

> ⚠️ インストール中に Rust / MSVC Build Tools の導入と 1.5 時間のコンパイルを求められたら、それは間違ったルートです。「ソースからビルド」はコントリビュータ向け。通常利用は上の npm コマンドを使ってください。

### DuDuClaw OS(アプライアンスイメージ、pre-GA)

PC を一台専有したくないなら、電源を入れるだけで動く AI スタッフ専用機という選択肢があります。[DuDuClaw OS](https://github.com/zhixuli0406/DuDuClaw-OS) は Yocto でビルドした Linux OS で、AI エージェントがネイティブに住み着いています。起動するとそのまま自前のデスクトップ(コンポジタ / シェル、ロック画面、Cmd+K の委任バー)に入り、人と AI が一台の x86-64 マシンを共用しつつ、日常利用の邪魔はしません。エージェントの GUI 作業は既定でシャドウワークスペースで実行され、あなたがキーボードやマウスに触れた瞬間、デスクトップ上で操作中のエージェントは手を止めます。デスクトップ版は A/B アトミックアップデートとロールバック、読み取り専用ルートを備え、Chromium / LibreOffice / Steam と中国語 IME をプリロード済みです。Secure Boot 署名、dm-verity、TPM2 はビルド時のオーバーレイオプションで、現行の v0.2.0 でもまだ有効化されていません。起動時は BIOS/UEFI で Secure Boot をオフにしてください。

現行バージョンは v0.2.0(プラットフォーム v1.63.0 を同梱、旧バージョンの v0.1.0 はロールバック先として保持)です。[DuDuClaw-OS Releases](https://github.com/zhixuli0406/DuDuClaw-OS/releases) からディスク全体イメージ `.wic.zst`(デスクトップ版)、デスクトップ版を書き込む `installer-desktop` インストーラー `.iso`、またはベースイメージ(アプリ層なし)を書き込む `installer` インストーラー `.iso` をダウンロードしてください。各ファイルには `.sha256` と minisign 署名が付属し、検証コマンドは[ドキュメントサイトの OS README](https://os.duduclaw.dudustudio.monster/docs/os/readme/)にあります。現在は bring-up 段階(0.x)で、QEMU での検証は済んでいますが、**実機での起動検証はまだ行っていません**。ハードウェア要件と対応機種は [docs/guides/hardware-requirements.md](docs/guides/hardware-requirements.md)、製品概要は [docs/features/50-duduclaw-os-appliance.md](docs/features/50-duduclaw-os-appliance.md) を参照してください。

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

- **デスクトップ版**:アプリを開くだけ。gateway は自動起動し、ウィザードもアプリ内にそのまま表示されます。
- **npm / ソースビルド**:

  ```bash
  duduclaw run                  # まとめて起動(gateway + チャネル + スケジューラ + dispatcher)
  open http://localhost:18789   # ダッシュボードを開く
  ```

どちらの方法でも、初回アクセスではウィザードが案内します:AI バックエンドを選ぶ → 最初のエージェントを作る → 内蔵 WebChat でそのまま会話。先にターミナルで `duduclaw onboard` を実行する必要はありません。あとは Channels ページに bot token を貼れば、同じエージェントを再起動なしで Telegram・LINE・Discord などに接続できます。

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
| チャネル | 11 チャネル(Telegram / LINE / Discord / Slack / WhatsApp / Feishu / Google Chat / Teams / WeCom / DingTalk / WebChat)、エージェントごとの bot、ホット起動/停止、プラットフォーム最適レンダリング、入力中インジケータ、長時間タスクの進捗ボード。Telegram の音声メッセージは OpenAI Whisper API で文字起こし。Discord のボイスチャンネルは既定外のビルド機能で、リリース版バイナリには含まれない | [docs/features](docs/features/README.md) |
| マルチランタイム | 13 のランタイム ID:Claude Code / Codex / Antigravity / Grok / Qwen Code / Kimi Code / GitHub Copilot CLI / Kiro / Cursor / Mistral Vibe / OpenCode / Gemini CLI(非推奨、v1.69.0 で削除)と OpenAI-compat。自動検出、エージェントごとの設定、切替時もコンテキスト保持 | [docs/features/13](docs/features/ja-JP/13-multi-runtime.md) |
| 統一 LLM API 層 | `duduclaw-llm` が 4 つのネイティブプロトコル(Anthropic Messages / OpenAI Responses / Gemini / OpenAI-compat)を単一の正規化リクエストでカバー。8 つの OpenAI-compat プリセット(DeepSeek / MiniMax / Groq / Together / Mistral / OpenRouter / xAI / Qwen)+ 価格レジストリ + クロスプロバイダ fallback を内蔵 | [ARCHITECTURE.md](ARCHITECTURE.md) |
| MCP サーバー | 249 ツール:チャネル、メモリ、エージェント編成、スキルマーケット、タスクボード、共有 wiki、Odoo ERP、computer use、live forking。stdio と HTTP/SSE の両トランスポート。外部クライアントのキーは既定で 7 つの基本ツールのみ使え、オペレーターがメモリ・wiki・メッセージ系のスコープを追加付与できる。コネクタ・実行系・管理系のツールは外部に公開しない | [docs/api](docs/api/README.md) |
| メモリ | SQLite 時系列メモリ(事実の置換チェーン)、HippoRAG-lite 知識グラフ検索(Personalized PageRank)、エビングハウス忘却曲線によるアーカイブ、エージェント横断の共有 wiki | [docs/features](docs/features/README.md) |
| 自己進化 | 予測駆動(設計上、多くの会話は LLM を呼ばずに終わる)+ AEE playbook 進化:SOUL.md はエージェントに対して読み取り専用。学習するのは各々 eval ケースに紐づく小さなルールで、現在の playbook 以上の場合のみコミットされ、24 時間後にルールごとに清算される。MistakeNotebook のターン横断メモリ | [evolution-engine.md](docs/architecture/evolution-engine.md) |
| セキュリティ | PolicyKernel reference monitor(LLM 不使用、fail-closed)、macOS Seatbelt / Linux Landlock ネイティブサンドボックス(エージェントごと、既定オフ)、コンテナサンドボックス(タスクサンドボックスは Docker のみで既定オフ、スクリプトサンドボックスは Docker で Windows では WSL2 を先に試行)、secret redaction vault、CONTRACT.toml 行動契約 + レッドチーム CLI | [SECURITY.md](SECURITY.md) |
| アカウントとコスト | OAuth + API キーのローテーション(4 戦略)、レート制限 / 課金クールダウン、キャッシュ効率分析つきコストテレメトリ。呼び出しごとに公式 CLI を新しいプロセスで実行(PTY セッションプールは 2026-09 に削除。Grok など TTY が必要な CLI には単発の疑似端末を使う)。Anthropic と Google はサードパーティ製品での個人向けサブスクリプションのトークンをブロックしているため、API キーを使ってください([multi-runtime](docs/features/ja-JP/13-multi-runtime.md)) | [docs/features](docs/features/README.md) |
| ローカル推論 | OpenAI 互換のローカルサーバー(llama-server / Ollama / vLLM / SGLang)または llamafile、3 段階の信頼度ルーティング | [docs/features](docs/features/README.md) |
| ファインチューニング | この機械の会話・仕事の成果・承認判断から SFT / DPO データセット(ShareGPT / Alpaca)を構築し、自前の GPU 機(SSH + LLaMA-Factory)または Together のクラウドで学習、GGUF / LoRA をローカルモデルディレクトリへ取り込み。ローカル学習は行わず(内蔵グラフィックスでは不可)、データが機械を出るときは明示的な同意が必要 | [docs/features/54](docs/features/54-finetune.md) |
| Live Forking | RFC-26:進行中のタスクを N 個の競合ブランチに分岐し、それぞれ copy-on-write 隔離、AI ジャッジが勝者を選んでマージ(デフォルト無効。v1.67.0 では Windows で有効にしないこと、CHANGELOG 参照) | [docs/rfc](docs/rfc) |
| 自動アップデート | ダッシュボードからワンクリック、または無人更新(`auto_update = true`)。SHA-256 + Ed25519 の二重検証後にその場で再起動、開いているタブは自動リロード | [deployment-guide.md](docs/guides/deployment-guide.md) |
| Web ダッシュボード | React 19 + TypeScript SPA、バイナリに内蔵で追加デプロイ不要。zh-TW / en / ja 対応 | [docs/features](docs/features/README.md) |
| ERP 連携 | Odoo ブリッジ 17 MCP ツール(CRM / 販売 / 在庫 / 会計)、CE/EE 自動検出、エージェントごとの認証分離 | [docs/rfc](docs/rfc/RFC-21-operator-guide.md) |
| DuDuClaw OS | Yocto アプライアンスイメージ(現行 v0.2.0、プラットフォーム v1.63.0 を同梱):自前コンポジタ / シェルとショートカット、人と AI の共同運転(エージェント専用シート、シャドウワークスペース、人の入力でエージェントを凍結、Super+Esc 緊急停止;ビルドには組み込み済みで既定オフ)、A/B アトミックアップデートとロールバック、読み取り専用ルート、初回起動時の自動プロビジョニング + LAN ダッシュボード、アプリ互換レイヤー(Flatpak / Bottles / Waydroid);Secure Boot 署名 / dm-verity / TPM2 はビルドオーバーレイオプション(v0.2.0 でもまだ未有効化);独立リポジトリ・独立バージョン、pre-GA | [docs/features/50](docs/features/50-duduclaw-os-appliance.md) · [52](docs/features/52-desktop-edition.md) |

全機能リストは [docs/features/feature-inventory.md](docs/features/feature-inventory.md)、バージョン履歴は [CHANGELOG.md](CHANGELOG.md) を参照してください。

<a id="cli"></a>

## CLI コマンド

```
duduclaw onboard             # 初回セットアップ;通常はブラウザのウィザードで完了、ヘッドレス/スクリプト向け(--yes でプロンプトをスキップ)
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
duduclaw migrate from openclaw   # OpenClaw / Hermes / paperclip からの無痛移行(既定は dry-run、--apply で反映)
duduclaw mcp-server          # MCP サーバー起動(stdio JSON-RPC 2.0)
duduclaw http-server         # MCP HTTP/SSE トランスポート起動(Bearer 認証)
duduclaw acp                 # Agent Client Protocol サーバー起動(Zed / JetBrains / Neovim agent panel)
duduclaw acp server          # A2A サーバー起動(エージェント間相互運用)
duduclaw license             # ライセンス管理(activate / status / redeem / rebind / …)
```

全コマンドとサブコマンドは `duduclaw --help` で確認できます。開発者向けは[開発ガイド](docs/guides/development-guide.md)へ。

<a id="trust"></a>

## 信頼とセキュリティ

インストールする中身は完全に透明です:

- **npm パッケージの中身**:小さな JS ラッパーとプラットフォームバイナリ(`@duduclaw/<platform>` optionalDependencies)。`postinstall` はプラットフォームパッケージの存在確認のみ([`install.js`](npm/duduclaw/scripts/install.js))。任意の URL からのダウンロードや実行は一切ありません
- **テレメトリなし**:利用データや会話内容を当社に送信することはありません。gateway は 6 時間ごとに GitHub Releases で更新を確認します。有償ライセンスを入れている場合は、ライセンスサーバーでライセンスを更新し(プランにより 3〜7 日ごと)、失効リストを毎日取得します。ライセンスファイルがなければライセンス関連の通信はありません。秘密情報は AES-256-GCM で暗号化され、あなたのマシンに残ります
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
| チャネル | 11 | 25+ | 8 | 0(API)|
| マルチランタイム | 13 のランタイム ID(12 CLI + OpenAI-compat) | 単一 | 単一 | マルチ LLM |
| MCP サーバー | 249 ツール | なし | なし | なし |
| 自己進化エンジン | AEE playbook ルール(予測駆動) | なし | なし | なし |
| ローカル推論 | OpenAI 互換ローカルサーバー / llamafile + 信頼度ルーティング | なし | なし | なし |
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
- [docs/features/50-duduclaw-os-appliance.md](docs/features/50-duduclaw-os-appliance.md):DuDuClaw OS アプライアンス(製品概要);[52-desktop-edition.md](docs/features/52-desktop-edition.md):デスクトップ版、人と AI で一台を共有;ハードウェア要件は [hardware-requirements.md](docs/guides/hardware-requirements.md);アプリ互換・イメージのビルド・リリースはいずれも [DuDuClaw-OS](https://github.com/zhixuli0406/DuDuClaw-OS) リポジトリ(互換層は同リポジトリの `docs/guides/app-compat.md`)
- [CHANGELOG.md](CHANGELOG.md):バージョン履歴

<a id="license"></a>

## ライセンス

オープンコアモデル:コアは [Apache License 2.0](LICENSE) で、自由に使用・改変・再配布できます。商用アドオン(`commercial/`、本リポジトリには含まれない)はクローズドソースの有償で、有償の業種パックなどがあり、ライセンスキーで有効になります。ライセンス検証のクライアント(`crates/duduclaw-license`)は Apache 2.0 のコアに含まれます。詳細は [LICENSING.md](LICENSING.md)。

<p align="center">
  🐾 Built with louis.li
</p>
