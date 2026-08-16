# Dockerインストールガイド

> 対応バージョン：v1.8.23+
> 最終更新：2026-04-22

本ガイドはDocker ComposeでDuDuClawサーバーをデプロイする一連の流れをカバーします——
port設定、三大CLI（Claude / Codex / Gemini）の認証方式から、
データ永続化、channel webhook、自動更新までを扱います。

ローカルで試すだけなら、まず
[docs/deployment-guide.md §1](deployment-guide.md#1-local-development)
のネイティブインストール手順を参照してください。ネイティブインストールの方が起動が速く、デバッグも簡単です。
Dockerはサーバーデプロイ、実行環境の分離、統一環境が必要なチームに向いています。

---

## 1. 概要

DuDuClawのDockerイメージは `container/Dockerfile.server` で構築され、3段階ビルドを採用しています。

| Stage | 内容 |
|-------|------|
| 1. frontend-builder | `node:22-slim` + React/TSフロントエンドのビルド |
| 2. rust-builder | `rust:slim` + `duduclaw` リリースバイナリのコンパイル |
| 3. production | `python:3.12-slim` + 三大AI CLI + Docker CLI + 実行環境 |

最終イメージには以下が組み込まれています。

- `duduclaw` メインプログラム（Rust製、dashboard込み）
- `@anthropic-ai/claude-code`、`@openai/codex`、`@google/gemini-cli`（`npm i -g` でインストール）
- `docker.io` CLI（ホストのDocker daemonを呼び出してagent sandboxを作成するために使用）
- （任意）Python 3.12：高度なローカル推論（MLX / LLMLingua-2、`mlx_lm` / `llmlingua` が必要）にのみ必要。Skillのセキュリティスキャンとchannel返信はすでにRustネイティブ実装で、Pythonには依存しません

---

## 2. 前提条件

| 項目 | バージョン | 備考 |
|------|------|------|
| Docker Engine | ≥ 24.0 | Linuxはネイティブ版を直接インストール推奨。macOS / WindowsはDocker DesktopまたはColimaを使用 |
| Docker Compose | v2.20+ | 最近のDockerには標準搭載。コマンドは `docker compose` で、旧来の `docker-compose` ではない |
| Git | 任意 | ソースコードのclone用 |
| ディスク容量 | ≥ 4 GB | ビルド中は一時的に約3 GB使用。完成イメージは約1.2 GB |
| Port | `18789` | デフォルトのgateway port、変更可能 |

channel webhook（LINE、WhatsApp、Feishu、Generic Webhook）で外部からのメッセージを受信するには、
**公開HTTPS URL** が別途必要です。最も手軽な選択肢は
[Tailscale Funnel](#11-tailscale-funnel-公開httpsをwebhookに提供)
または
[Cloudflare Tunnel](deployment-guide.md#4-cloudflare-tunnel-long-term-stable)です。

---

## 3. クイックスタート

```bash
# 1. ソースコードをclone
git clone https://github.com/zhixuli0406/DuDuClaw.git
cd DuDuClaw

# 2. .envを作成（必要に応じて記入）
cp .env.example .env
$EDITOR .env

# 3. 起動
docker compose up -d

# 4. 起動ログを確認
docker compose logs -f duduclaw

# 5. 動作確認
curl http://localhost:18789/health
# {"status":"ok","version":"1.8.23", ...}

# 6. Dashboardを開く
open http://localhost:18789
```

初回起動時、`server-entrypoint.sh` は `~/.duduclaw/config.toml` が存在しないことを検出し、自動的に
`duduclaw onboard --yes` を実行して基本設定ファイルを生成します（API keyとchannel tokenは暗号化して保存されます）。

---

## 4. 環境変数（`.env`）

`docker-compose.yml` は定義済みの変数のみを読み込み、**未定義の変数は空文字列として渡されます**
（すべて `${VAR:-}` 構文を使用しているため）。以下のリストは用途別に分類しています。

### 4.1 Runtime認証（いずれか1つ以上を設定）

| 変数 | 用途 | 備考 |
|------|------|------|
| `CLAUDE_CODE_OAUTH_TOKEN` | Claude Codeの長期有効token | `claude setup-token` で生成、**推奨**。ブラウザのないコンテナ環境に適している |
| `ANTHROPIC_API_KEY` | Claude APIキー | Fallback、従量課金 |
| `OPENAI_API_KEY` | Codex / OpenAI-compat APIキー | Fallback。ChatGPT Plus/Pro OAuthはvolume経由 |
| `GEMINI_API_KEY` | Gemini APIキー | Fallback。Google OAuthはvolume経由 |

最低いずれか1組が利用可能である必要があります。DuDuClawは `agent.toml [runtime]` と
`[model] api_mode` に基づいてどれを使うか決定します。詳細は
[§6 三大CLIの認証設定](#6-三大cliの認証設定)を参照してください。

### 4.2 Channel Tokens

| 変数 | Channel |
|------|---------|
| `LINE_CHANNEL_TOKEN` / `LINE_CHANNEL_SECRET` | LINE Messaging API |
| `TELEGRAM_BOT_TOKEN` | Telegram Bot |
| `DISCORD_BOT_TOKEN` | Discord Bot |
| `SLACK_BOT_TOKEN` / `SLACK_APP_TOKEN` | Slack Socket Mode |
| `WHATSAPP_ACCESS_TOKEN` など | WhatsApp Cloud API（`.env.example` を参照）|
| `FEISHU_APP_ID` / `FEISHU_APP_SECRET` | Feishu |

実際に使用するchannelの分だけ記入すれば十分です。未記入の変数に対応するchannelは起動しません。

> `docker-compose.yml` は現状 `LINE_*` / `TELEGRAM_*` / `DISCORD_*` の3グループのみをデフォルトで注入します。他のchannelを環境変数経由で渡すには、compose ファイルの
> `environment:` ブロックに自分で追加するか、起動後に **Dashboard → Channels → Add** から
> ホット追加する方法があります（推奨。gatewayの再起動が不要で、tokenも即座に暗号化されてconfigに書き込まれます）。

### 4.3 Bind / Port関連

| 変数 | デフォルト | 説明 |
|------|------|------|
| `DUDUCLAW_BIND` | `0.0.0.0` | Gatewayのlistenアドレス。コンテナ内では必ず `0.0.0.0` でなければ外部からアクセスできません |
| `DUDUCLAW_ALLOWED_ORIGINS` | （空） | Dashboard WebSocket/CORSの追加許可 `Origin`、カンマ区切り。tailnetやリバースプロキシ経由でdashboardを開き、WSが403になる場合に設定します（[§13 トラブルシューティング](#dashboardがくるくる回り続ける-websocket-403)を参照）。config.tomlの `[gateway] allowed_origins` とマージされます |

Portはenv varではなくcompose ファイル内で設定します。詳細は[§5 Port設定の詳細](#5-port設定の詳細)を参照してください。

### 4.4 その他

| 変数 | 用途 |
|------|------|
| `TS_AUTHKEY` | Tailscale Funnelの起動時に自動でtailnetに参加させる（`--profile tailscale` と併用）|

---

## 5. Port設定の詳細

### 5.1 デフォルト

`docker-compose.yml`：

```yaml
services:
  duduclaw:
    ports:
      - "18789:18789"   # HOST:CONTAINER
    environment:
      - DUDUCLAW_BIND=0.0.0.0
```

- **コンテナ内部**：gatewayは常に `0.0.0.0:18789` にbindされます。これは意図的な設計で、
  コンテナ自体が隔離されたネットワーク空間であるため、localhostにbindすると外部からアクセスできなくなります。
- **ホスト**：`18789:18789` によりホストの18789番portがコンテナの18789番portへ転送されます。

### 5.2 ホストportの変更

最もよくあるケースは18789が既に使用中、または複数インスタンスが必要な場合です。左側の数字だけを変更します。

```yaml
    ports:
      - "28789:18789"   # 外部からは28789でアクセス
```

その後、dashboardとwebhook URLも新しいportに変更する必要があります。

```bash
curl http://localhost:28789/health
```

> 右側の `18789`（コンテナ内部port）は**変更しないでください**。変更する場合は
> `~/.duduclaw/config.toml` の `[gateway] port` も同期して変更する必要があり、
> entrypointは `--yes` で自動生成しているため、メンテナンスコストが上がります。

### 5.3 ローカルのみにbind（LANへの露出を避ける）

デフォルトの `ports: "18789:18789"` は `0.0.0.0` にbindされるため、同一ネットワーク内の他のマシンからも接続できます。
ローカル専用にしたい場合（リバースプロキシと組み合わせる場合など）：

```yaml
    ports:
      - "127.0.0.1:18789:18789"
```

### 5.4 同一マシンでの複数インスタンス

同じマシンで2つのDuDuClawを動かす場合：

```bash
# ディレクトリA — 本番
cd /srv/duduclaw-prod
# ports: "18789:18789"
docker compose up -d

# ディレクトリB — ステージング
cd /srv/duduclaw-staging
# ports: "28789:18789"
# container_nameも変更し、衝突を避ける
docker compose up -d
```

`container_name`（デフォルト `duduclaw-server`）も忘れずに変更し、名前の衝突を避けてください。

---

## 6. 三大CLIの認証設定

3つのruntimeにはそれぞれ2種類の認証経路があります。**OAuth**（追加費用なし、サブスクリプションプランを使用）
と **APIキー**（従量課金のfallback）です。実務上は「まずOAuthを設定し、
保険としてAPIキーも1つ用意しておく」ことをお勧めします。

コンテナ内の3つのOAuth状態ディレクトリには、それぞれ独立したnamed volumeがあります。

| パス | Volume | CLI |
|------|--------|-----|
| `/home/duduclaw/.claude` | `duduclaw-claude` | Claude Code |
| `/home/duduclaw/.codex`  | `duduclaw-codex`  | Codex |
| `/home/duduclaw/.gemini` | `duduclaw-gemini` | Gemini |

つまり、**一度ログインすればコンテナを再構築してもログインし直す必要はありません**。

### 6.1 Claude Code CLI

#### 方法A：Setup Token（推奨）

Claude Codeは自動化シナリオ向けに、ブラウザのコールバックを必要としない長期有効tokenを提供しており、
コンテナ / CI / headless serverに適しています。

```bash
# ブラウザのある自分のマシンで実行する（コンテナ内ではない）
npm install -g @anthropic-ai/claude-code
claude setup-token
# 画面の指示に従いブラウザでOAuthを完了する
# 最後に CLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat01-... が表示される
```

生成されたtokenを `.env` に書き込みます。

```bash
# .env
CLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat01-xxxxxxxx...
```

そして：

```bash
docker compose up -d
```

DuDuClawは起動時に環境変数を読み込み、`claude` CLIがこのtokenを認識して自動的にログインします。コンテナ内で何か操作する必要はありません。Tokenの有効期限は30日で、
期限の7日前からDuDuClawがdashboardに警告を表示します。

#### 方法B：コンテナ内でのインタラクティブログイン

Pro / Team / Maxのサブスクリプションがあり、そのquotaを使いたい場合：

```bash
# コンテナに入る
docker compose exec duduclaw bash

# コンテナ内で実行
claude auth login
# URLが表示されるので、ローカルのブラウザで開いて認可を完了する
# 状態は /home/duduclaw/.claude/ に書き込まれる（duduclaw-claude volume）

# 確認
claude auth status
exit
```

volumeに保存されているため、コンテナを再起動しても状態は保持されます。

#### 方法C：APIキーFallback

`.env`：

```bash
ANTHROPIC_API_KEY=sk-ant-...
```

この経路は**従量課金**で、サブスクリプションを使い切ったりアカウントがrate-limitされたりした場合の保険として適しています。
DuDuClawのAccountRotatorが複数アカウント間で自動的にローテーションします。詳細は
[features/07-account-rotation.md](../../features/ja-JP/07-account-rotation.md)を参照してください。

### 6.2 Codex（OpenAI）CLI

#### 方法A：ChatGPT Plus/Pro OAuth

```bash
docker compose exec duduclaw bash
codex login
# ChatGPTアカウントのパスワードを入力するか、ブラウザの案内に従う
# 状態は /home/duduclaw/.codex/ に書き込まれる（duduclaw-codex volume）
exit
```

この方法はChatGPTのサブスクリプションquotaを使用し、追加のAPI費用は発生しません。

#### 方法B：APIキーFallback

`.env`：

```bash
OPENAI_API_KEY=sk-proj-...
```

従量課金（OpenAI APIの料金体系）です。

### 6.3 Gemini（Google）CLI

#### 方法A：Google OAuth

```bash
docker compose exec duduclaw bash
gemini auth
# 画面の指示に従いローカルのブラウザでGoogleログインを完了する
# 状態は /home/duduclaw/.gemini/ に書き込まれる（duduclaw-gemini volume）
exit
```

GoogleはGemini CLIに対して現在無料枠を提供しており、日常利用に適しています。

#### 方法B：APIキーFallback

`.env`：

```bash
GEMINI_API_KEY=AIza...
```

Google AI Studioの料金体系で課金されます。

### 6.4 Agent単位でのRuntime指定

各agentは自身の `agent.toml` で使用するruntimeを指定できます。

```toml
# ~/.duduclaw/agents/my-agent/agent.toml

[runtime]
preferred = "claude"      # メインruntime：claude / codex / gemini / openai-compat
fallback = "gemini"       # Claudeが利用不可の際に自動切り替え
```

完全な例とfailover戦略については
[features/13-multi-runtime.md](../../features/ja-JP/13-multi-runtime.md)を参照してください。

指定がない場合、`RuntimeRegistry` は起動時にPATHをスキャンし、最初に見つかった利用可能なruntimeを選択します。

### 6.5 3つのCLIがすべて認識されているか確認

```bash
docker compose exec duduclaw bash

# 3つともバージョンが表示されるはず
claude --version
codex --version
gemini --version

# Claudeのログイン状態
claude auth status
```

---

## 7. データ永続化（Volumes）

`docker-compose.yml` は4つのnamed volumeを宣言しています。

| Volume | 内容 | バックアップの推奨度 |
|--------|------|---------|
| `duduclaw-data` | メインデータ：config、agents、memory SQLite、logs、bus_queue.jsonl | **最重要**。毎日バックアップ推奨 |
| `duduclaw-claude` | Claude CLI OAuth状態 | 中程度。失っても `claude auth login` をやり直せばよい |
| `duduclaw-codex`  | Codex OAuth状態 | 中程度 |
| `duduclaw-gemini` | Gemini OAuth状態 | 中程度 |

さらにホスト側からマウントするパスがあります。

| パス | 用途 | 必要性 |
|------|------|-------|
| `/var/run/docker.sock` | コンテナがホストのDocker daemonを呼び出しagent sandboxを作成するために使用 | **必須**。container sandboxによる隔離実行に使用 |

### 7.1 バックアップ

```bash
# メインデータをtar.gzにバックアップ
docker run --rm \
  -v duduclaw_duduclaw-data:/source:ro \
  -v $(pwd):/backup \
  alpine tar czf /backup/duduclaw-data-$(date +%F).tar.gz -C /source .
```

### 7.2 復元 / 移行

バックアップをターゲットマシンに配置し、同名のvolumeに復元します。

```bash
docker volume create duduclaw_duduclaw-data
docker run --rm \
  -v duduclaw_duduclaw-data:/target \
  -v $(pwd):/backup \
  alpine tar xzf /backup/duduclaw-data-2026-04-22.tar.gz -C /target
```

### 7.3 ホストディレクトリで置き換える（上級者向け）

データを直接ブラウズしたい場合は、named volumeをbind mountに置き換えられます。

```yaml
    volumes:
      - ./data/duduclaw:/home/duduclaw/.duduclaw
      - ./data/claude:/home/duduclaw/.claude
      - ./data/codex:/home/duduclaw/.codex
      - ./data/gemini:/home/duduclaw/.gemini
      - /var/run/docker.sock:/var/run/docker.sock
```

ホスト側ディレクトリの**ownerはUID 1000でなければならない**点に注意してください（コンテナ内の `duduclaw` ユーザー）。

```bash
mkdir -p data/{duduclaw,claude,codex,gemini}
sudo chown -R 1000:1000 data/
```

---

## 8. ヘルスチェックと可観測性

### 8.1 3つのhealth endpoint

| パス | 用途 | HTTP 200の条件 |
|------|------|---------------|
| `/health` | 完全な状態（JSON） | 常に200と構造化された内容を返す |
| `/health/ready` | Readiness probe | agentsがすべてロード済み |
| `/health/live`  | Liveness probe | processがまだ生きている |

Composeはデフォルトで `/health` をhealthcheckに使用します（30秒ごと、3回連続失敗でunhealthyと判定）。

### 8.2 状態の確認

```bash
# Containerの状態
docker compose ps

# リアルタイムlog
docker compose logs -f duduclaw

# 直近100行
docker compose logs --tail=100 duduclaw

# Healthcheckの履歴
docker inspect duduclaw-server --format '{{json .State.Health}}' | jq
```

### 8.3 Prometheus Metrics

Gatewayは `GET /metrics` エンドポイントを提供しています。指標一覧は
[docs/deployment-guide.md §8](deployment-guide.md#8-prometheus--grafana-monitoring)を参照してください。

---

## 9. Watchtowerによる自動更新

`docker-compose.yml` にはWatchtowerサービスが組み込まれており、1時間ごとにイメージの更新を確認します。

> **注意**：現在のcomposeファイルの `WATCHTOWER_SCOPE=duduclaw` 設定は
> **対応するlabelを持つコンテナのみ**を更新対象としますが、`duduclaw` サービス自体には
> そのlabelが設定されていません。自動更新を有効にするには、`services.duduclaw` に以下を追加してください。
>
> ```yaml
>     labels:
>       - "com.centurylinklabs.watchtower.scope=duduclaw"
> ```
>
> または `WATCHTOWER_SCOPE` を削除してください（すべてのコンテナが更新対象になり、範囲が広がります）。

自動更新を望まない場合は、watchtowerサービスのブロックをそのまま削除してください。

---

## 10. Channel Webhookには公開HTTPSが必要

LINE / WhatsApp / Feishu / Generic Webhookはいずれも**インターネットからアクセス可能なHTTPS URL**
がなければメッセージを受信できません。DuDuClawが家庭用ネットワークやグローバルIPのないサーバーで動作している場合、
よく使われる方法は以下の通りです。

| 方法 | 適したシーン | コスト | ガイド |
|------|---------|------|------|
| Tailscale Funnel | 家庭 / 開発 / 一時的な利用 | 無料 | [§11](#11-tailscale-funnel-公開httpsをwebhookに提供) |
| Cloudflare Tunnel | 長期的な本番運用 | 無料（独自ドメインが必要） | [deployment-guide §4](deployment-guide.md#4-cloudflare-tunnel-long-term-stable) |
| ngrok | クイックデモ | 無料版はURLが変わる | [deployment-guide §3](deployment-guide.md#3-ngrok-alternative) |
| リバースプロキシ（Caddy / Nginx） | 独自のグローバルIP＋ドメイン | 自前でメンテナンスが必要 | [deployment-guide §5](deployment-guide.md#5-reverse-proxy-caddy--nginx) |

---

## 11. Tailscale Funnel（公開HTTPSをWebhookに提供）

Composeには既にTailscaleサービスが同梱されていますが、デフォルトでは `tailscale` profile配下にあり自動起動しません。

### 11.1 有効化

```bash
# 1. Tailscale admin consoleでauth keyを生成する
#    https://login.tailscale.com/admin/settings/keys
#    "Reusable" + "Ephemeral" にチェックすることを推奨

# 2. .envに書き込む
echo "TS_AUTHKEY=tskey-auth-xxxxxxx" >> .env

# 3. tailscale profileを起動
docker compose --profile tailscale up -d

# 4. Tailscale admin consoleでこのマシンのFunnelを有効化
#    Machines → duduclaw → Enable Funnel

# 5. https URLを取得
docker compose exec tailscale tailscale funnel status
# https://duduclaw.your-tailnet.ts.net/  ← これがwebhookのprefixになる
```

LINE webhook URLには `https://duduclaw.your-tailnet.ts.net/webhook/line` を設定します。

> Tailscaleコンテナは現在 `image: tailscale/tailscale:latest` を使用しています。
> 本番環境では特定のdigestにpinすることを推奨します（composeファイルに既にTODOとして記載済みです）。

---

## 12. よく使うコマンド早見表

```bash
# 起動 / 停止 / 再起動
docker compose up -d
docker compose down           # container停止＋削除（volumeは削除されない）
docker compose restart duduclaw

# イメージの再ビルド（コードやDockerfileの変更後）
docker compose build --no-cache duduclaw
docker compose up -d

# コンテナshellに入る
docker compose exec duduclaw bash

# 単発コマンドの実行
docker compose exec duduclaw duduclaw agent list
docker compose exec duduclaw duduclaw cost summary

# Log
docker compose logs -f duduclaw
docker compose logs --since 1h duduclaw

# クリーンアップ
docker compose down -v        # ⚠️ volumeも一緒に削除される。データは全て失われる
docker system prune -af       # ⚠️ 未使用のimage / containerをすべて削除する
```

---

## 13. トラブルシューティング

### Containerは起動しているが `curl localhost:18789/health` がConnection refusedを返す

**原因**：Gatewayがコンテナ内部のlocalhostにbindされており、ホストから到達できません。
**確認方法**：

```bash
docker compose exec duduclaw env | grep DUDUCLAW_BIND
# DUDUCLAW_BIND=0.0.0.0 になっているはず
```

もし空文字列であれば、composeファイルの `environment:` ブロックに追加してください。

### `docker compose up` が `Compiling duduclaw-gateway` で止まる

Rustのコンパイルは CPU/RAM を多く消費し、一般的なノートPCでの**初回**ビルドは10〜20分ほどかかります。
以降の変更はincremental cacheが効くため、通常1〜2分程度です。

メモリが不足している（4 GB未満）とOOM killedになる可能性があります。以下を検討してください。

- pre-buildイメージが提供されていればそれを使用する
- またはホスト側で事前に `cargo build --release` を実行してバイナリを作り、Dockerfileを書き換えて
  直接COPYする（上級者向け）

### Claude CLIが "not logged in" と表示される

**確認の順序**：

```bash
# 1. Env varが渡っているか
docker compose exec duduclaw env | grep -E "CLAUDE_CODE_OAUTH_TOKEN|ANTHROPIC_API_KEY"

# 2. Volumeが正しくマウントされているか
docker compose exec duduclaw ls -la ~/.claude/

# 3. CLI自体には何が見えているか
docker compose exec duduclaw claude auth status
```

よくある間違いは、`.env` を編集した後に `docker compose up -d` で container を**再起動**するのを忘れることです。
env varは起動時にのみ注入されるため、ホット変更は反映されません。

### Dashboardには接続できるがagentが「まず `claude auth status` を実行してください」と返信する

これはv1.8.x以前のバージョンの残存メッセージです。v1.8.22以降は
`FailureReason` で分類されたzh-TWメッセージに変更されています。まだ表示される場合は：

```bash
docker compose exec duduclaw duduclaw --version
# 1.8.22以上であることを確認。そうでなければ最新のmainをpullして再buildする
```

### Dashboardがくるくる回り続ける、WebSocket 403

症状：tailnet（`*.ts.net`）またはリバースプロキシのドメイン経由でdashboardを開くとHTTPページ自体は正常に読み込まれるが、
リアルタイムデータがくるくる回り続ける。ブラウザDevToolsのNetworkパネルでは `/ws` のアップグレードが**403**を返している。

原因：WebSocketの `Origin` allowlistはデフォルトでloopbackのみを含んでおり、外部ドメインが含まれていません。allowlistに
追加すれば解決します。

```bash
# composeのenvironment:、または.env
DUDUCLAW_ALLOWED_ORIGINS=duduclaw.your-tailnet.ts.net,duduclaw.yourdomain.com
```

または `~/.duduclaw/config.toml` に記述します。

```toml
[gateway]
allowed_origins = ["duduclaw.your-tailnet.ts.net"]
```

再起動後、起動ログに有効になった追加originsが出力されます。詳細は
[deployment-guide §5 WebSocket Origin 許可リスト](deployment-guide.md#websocket-origin-許可リストリバースプロキシtailnet-利用者は必読)を参照してください。

### Channel webhookがagentをトリガーしない

- LINE / WhatsApp / FeishuはHTTPSが必要です。[§10](#10-channel-webhookには公開httpsが必要)の設定を確認してください
- Dashboard → Channelsで該当channelの接続状態インジケーターを確認してください
- logを確認する：`docker compose logs -f duduclaw | grep -i webhook`

### Container sandboxがsub-agentを起動できない

DuDuClawは `docker.sock` を通じてホストのDocker daemonを呼び出し、agent tasksを実行する隔離コンテナを作成します。失敗する場合：

```bash
# コンテナ内からsocketが使えるかテストする
docker compose exec duduclaw docker ps
# ホスト上のcontainer一覧が見えるはず
```

Permission deniedになる場合、多くはSELinux / AppArmorがブロックしています。Linuxでは以下を試してください。

```yaml
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock:rw
    group_add:
      - "${DOCKER_GID:-999}"   # ホストのdocker group GIDに合わせる
```

### コンテナ再構築後にOAuth状態が消える

volumeが実際に存在するか確認してください。

```bash
docker volume ls | grep duduclaw
# duduclaw_duduclaw-data
# duduclaw_duduclaw-claude
# duduclaw_duduclaw-codex
# duduclaw_duduclaw-gemini
```

もしどれかが欠けていれば、composeファイルが変更されたか、過去に `docker compose down -v` が実行されたことを意味します。
`claude auth login` / `codex login` / `gemini auth` を再度実行してください。

---

## 14. 関連ドキュメント

- デプロイ方式の概要：[docs/deployment-guide.md](deployment-guide.md)
- Multi-Runtimeアーキテクチャ：[docs/features/13-multi-runtime.md](../../features/ja-JP/13-multi-runtime.md)
- Account Rotation：[docs/features/07-account-rotation.md](../../features/ja-JP/07-account-rotation.md)
- 開発ガイド：[docs/development-guide.md](development-guide.md)
