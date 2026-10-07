# DuDuClaw Development Guide

> Agent 開発、ブラウザ自動化のデバッグ、ローカル環境セットアップのガイド。

---

## 1. クイックスタート

### 1.1 ローカル開発環境

```bash
# サーバーを起動（Gateway + チャンネル + heartbeat + Dashboard）
duduclaw run

# 対話式の質問を省略して同じサーバーを起動
duduclaw gateway
```

独立した `duduclaw dev` モードはありません。サーバーは以下を行います。
- `config.toml [gateway]` のアドレス（`bind` / `port`、既定は `http://127.0.0.1:18789`）で Dashboard を提供します
- ログをリアルタイムで Dashboard にストリーミングします
- どの Agent にもブラウザ MCP server を追加しません。`.mcp.json` にブラウザの項目を入れる方法は 2.4 節を参照してください

### 1.2 Agent ディレクトリ構成

```
~/.duduclaw/agents/my-bot/
├── agent.toml          # Agent 設定（model、budget、capabilities）
├── SOUL.md             # Agent の人格と行動指針
├── CLAUDE.md            # Claude Code プロジェクト指示（任意）
├── CONTRACT.toml       # 行動契約（[boundaries] のみ）
├── .mcp.json           # この Agent の MCP server（DuDuClaw は自身の項目を書き込み、ブラウザ server は利用者が追加）
├── .claude/            # Claude Code 設定ディレクトリ
└── SKILLS/             # Agent のスキルディレクトリ
```

### 1.3 決定の継続性（Decision Continuity, RFC-24）

Agent がユーザーに列挙形式の選択肢（「案 A/B/C」「Option 1/2」）を提示した後、
ユーザーが後から（セッションの再起動や圧縮をまたいでも）「案 C で」と返信する
ことがあります。デフォルトでは、対話の圧縮によって選択肢の内容がすでに失われて
いる場合があります。この機能を有効にすると、システムはメッセージ送信時に各選択
肢を対話メモリとは独立したセマンティックメモリ層に自動保存し、以降のターンに
「未決定事項」を注入して Agent が正しく解決できるようにします。

`agent.toml` で有効化します（デフォルトは無効、Agent ごとの opt-in）。

```toml
[memory]
decision_continuity = true
```

検出は決定的でLLMコストはゼロ、かつ保守的に働きます（見逃すより余分に拾う方
針）。バックグラウンドでの取得に失敗しても返信の送信はブロックされません。詳細
は [RFC-24](../../rfc/RFC-24-decision-continuity.md) を参照してください。

### 1.4 AI Runtime バックエンドの選択（Multi-Runtime）

各 Agent は `AgentRuntime` トレイトの抽象化を通じて、自分を駆動する AI CLI バッ
クエンドを個別に選択できます。`RuntimeRegistry` は起動時に各 CLI がインストール
されているかを自動検出して登録します。`agent.toml` では `[runtime] provider` で
指定し（デフォルトは `claude`）、`fallback` はバックエンドが利用不可な場合の代替
先を指定します。

```toml
[runtime]
provider = "antigravity"   # claude | codex | gemini（非推奨）| antigravity | openai_compat
fallback = "claude"        # 検出できない場合に使うバックエンド
```

| Provider | CLI バイナリ | 認証 | 備考 |
|----------|-----------|------|------|
| `claude` | `claude`（常に利用可能、コア） | OAuth / API Key ローテーション | デフォルトバックエンド |
| `codex` | `codex` | OpenAI | — |
| `gemini` | `gemini` | `GEMINI_API_KEY` / OAuth | **v1.67.0 で非推奨、v1.72.0 で削除予定**（[非推奨となった名称](deprecations.md)参照）。個人版 OAuth は 2026-06-18 に廃止；有料 API キーは引き続き利用可 |
| `antigravity` | `agy`（`~/.local/bin/agy`） | Google サインイン（ターミナルで `agy` を実行）/ `GEMINI_API_KEY` | Gemini CLI の公式後継、マルチモデル（Gemini 3.x + Claude + GPT-OSS） |
| `openai_compat` | HTTP（CLI なし） | プロバイダーごとのキー | Exo / llamafile / vLLM などの OpenAI 互換エンドポイント |

**Antigravity（`agy`）固有の注意事項**（詳細は
[TODO-antigravity-cli-migration.md](../../todo/TODO-antigravity-cli-migration.md) を参照）。

- Agent ディレクトリは自動的に agy の `trustedWorkspaces` に追加され、headless 実
  行時に「このワークスペースを信頼しますか？」というプロンプトで止まることを回
  避します。
- print モードには JSON 出力がないため、トークン使用量は CJK 対応のヒューリステ
  ィック推定値であり、正確な値ではありません。
- Gateway が呼び出せるようにするには、事前に認証を済ませる必要があります。ホストの
  ターミナルで `agy` を実行して Google サインインの案内に従う（`agy login` サブコマンドは
  ありません）か、API キーモードを使います。`config.toml` に `[antigravity] auth = "api_key"`
  を設定し、Gemini API キー（`gemini` プロバイダーのアカウント、または環境変数
  `GEMINI_API_KEY`）を用意してください。keyring やブラウザのないコンテナ・リモートホストでは
  API キーモードを使います。

### 1.5 ライブ検証用ホーム（Live validation home）

新しいビルドは `~/.duduclaw` ではなく、隔離したホームで実際の gateway に対して検証します。

```bash
scripts/live-test/make-home.sh /tmp/ddc-live --port 18977
HOME=/tmp/ddc-live/os-home DUDUCLAW_HOME=/tmp/ddc-live duduclaw run --yes &   # 一度起動すると .mcp.json が書かれる
scripts/live-test/mcp-probe.sh /tmp/ddc-live plain
scripts/live-test/mcp-probe.sh /tmp/ddc-live prod-shaped
```

gateway を起動するときは、必ず `HOME` を、`make-home.sh` が隔離ホームの中に作る `os-home` ディレクトリに向けてください。gateway と、それが起動する AI CLI は、`HOME` の下からログインと設定を探します。`DUDUCLAW_HOME` だけを変えると、起動された `claude` はオペレーター自身のログインを使い、オペレーター自身の利用枠を消費します。Antigravity の API キーモードでは、オペレーター自身の設定ファイルが書き換えられます。`mcp-probe.sh` も、MCP サーバーを起動するときに同じ `os-home` を使います。

このホームには従業員が 2 人います。`plain`（allowlist なし）と `prod-shaped`（`allowed_tools = ["mcp__duduclaw__*", ...]`、denied と承認リスト、明示的な権限フラグ、予算、契約付き）です。v1.67.0 以降のワイルドカード allowlist がすべてのプラットフォームツールを拒否していた不具合は、検証用従業員に allowlist がなかったため見逃されました。ルール: 本番ホームをアップグレードしたら、各従業員自身の MCP 登録を通して実際のツールを呼びます（`mcp-probe.sh ~/.duduclaw <agent-id>`）。隔離ホームはオペレーターの Claude コネクタ（Drive、Gmail）までは隔離しないため、テストタスクには外部サービスを照会しないよう明記してください。ビルドキャッシュでディスクが埋まったときは、まず `scripts/clean-build-cache.sh --dry-run` を実行します。サードパーティの成果物は残し、`cargo` や `rustc` が動いている間は実行を拒否します。詳細は `scripts/live-test/README.md`。

---

## 2. ブラウザ自動化と Computer Use のデバッグ

### 2.1 アーキテクチャ概要

ルーターはありません。エージェントは L1・L2 の MCP ツールと任意の L3 サーバーを見て、どれを使うかを自分で選びます。L5 はコンテナ内のセッションで、エージェントが `computer_*` MCP ツールで駆動します。ある層から次の層へ自動でエスカレーションされることはありません（旧「BrowserRouter」は 2026-09 に削除されました。[ブラウザ自動化](../../features/ja-JP/08-browser-automation.md)を参照）。

```
Agent
  ├── L1: web_fetch_cached   （HTTP GET、SSRF 防御、ディスクキャッシュ）
  ├── L2: web_extract        （同じ取得経路 + CSS セレクタ）
  ├── L3: 外部ヘッドレスブラウザ MCP サーバー（任意、エージェントごとの .mcp.json）
  └── L5: 仮想ディスプレイを持つコンテナ内の Computer Use セッション、
           エージェントが 8 つの computer_* MCP ツールで駆動（セッションは gateway が所有）
           └── gateway が駆動：チャットメッセージ + Anthropic の computer ツール
```

L4 はありません。タスク全体を包むコンテナはタスクサンドボックス（2.5 節）であり、ブラウザの階層ではありません。機能のゲートは `agent.toml [capabilities]` にあります。`computer_use`（既定 `false`、ファイル欠落や形式エラーはすべて拒否。`computer_*` ツールに効きます）、`browser_via_bash`、`allowed_tools`、`denied_tools` です。`denied_tools` は CLI に `--disallowedTools` として渡されるだけでなく、MCP ディスパッチャでも強制されます。

### 2.2 L1 — `web_fetch_cached` のデバッグ

エージェント経由で動かすか、モデルに直接呼ばせます。

```bash
claude -p "Use web_fetch_cached to fetch https://example.com"
```

**確認項目**（`crates/duduclaw-gateway/src/web_fetch.rs`、`crates/duduclaw-cli/src/mcp/web.rs`）：
- `http` と `https` のみ受け付け、それ以外のスキーム（`file:`、`javascript:`、`data:`）はブロックされる
- `localhost`、クラウドのメタデータホスト名、そして `duduclaw_core::net_addr::is_public_ip` が公開と判定しないすべてのアドレスはブロックされる：IPv4 の `0.0.0.0/8`、`10.0.0.0/8`、`100.64.0.0/10`、`127.0.0.0/8`、`169.254.0.0/16`、`172.16.0.0/12`、`192.0.0.0/24`、`192.0.2.0/24`、`192.168.0.0/16`、`198.18.0.0/15`、`198.51.100.0/24`、`203.0.113.0/24`、`224.0.0.0/4`、`240.0.0.0/4`。IPv6 は `2000::/3` の外側に加え、`2001::/32`、`2001:db8::/32`、`3fff::/20`。IPv4-mapped、NAT64（`64:ff9b::/96`）、6to4 のアドレスは、埋め込まれた IPv4 アドレスで判定される（`http://[::ffff:127.0.0.1]/` を試すこと。拒否されなければならない）。同じ分類器が、他のすべての外向きゲート（常駐センシング、メディア、relay、MCP インポート、skills RPC、Odoo、wiki フェデレーション、Computer Use のピン留め）にも使われる
- 同じ URL への 2 回目のリクエストは `cached: true` を返す（`ttl_seconds` でキャッシュ期間を指定）
- レート制限：エージェントあたり 1 分 10 リクエスト（`web_extract` と共有）
- 返される本文は 60,000 文字で切り詰められる

### 2.3 L2 — `web_extract` のデバッグ

```bash
claude -p 'Use web_extract on https://example.com with selector "h1" and format "text"'
```

同じ取得経路を使うため、上記の SSRF、キャッシュ、レート制限のチェックがそのまま適用されます。どちらのツールも JavaScript を実行しないので、シングルページアプリは空の殻しか返しません。

**対応フォーマット：**
- `text` — プレーンテキスト
- `html` — 内側の HTML
- `json` — 構造化 JSON（タグ、属性、子要素）

### 2.4 L3 — 外部ヘッドレスブラウザ MCP サーバーのデバッグ

DuDuClaw はヘッドレスブラウザを同梱しておらず、自動で追加もしません。エージェントが JavaScript でレンダリングされるページを必要とする場合は、そのエージェント自身の `.mcp.json` にブラウザ MCP サーバーを登録します（グローバル登録の DuDuClaw MCP サーバーとは別）。`crates/duduclaw-agent/src/mcp_template.rs` に `playwright_mcp_config` / `browserbase_mcp_config` というエントリを組み立てるヘルパーがありますが、gateway にこれを呼んで自動インストールするコードはないため、自分で書き込んでください。

```bash
# エージェントに登録されている内容を確認
cat ~/.duduclaw/agents/my-bot/.mcp.json
```

**`.mcp.json` の例（`playwright_mcp_config(true)` が生成する形）：**
```json
{
  "mcpServers": {
    "playwright": {
      "command": "npx",
      "args": ["-y", "@playwright/mcp", "--headless"],
      "env": {}
    }
  }
}
```

**前提条件：**
- Node.js と `npx` が必要です。`npx -y` が初回起動時に `@playwright/mcp` をダウンロードするため、グローバルインストールは不要です。
- サーバーが起動できるブラウザが必要です。どのブラウザをどうインストールするかは `@playwright/mcp` の README を参照してください（ここでは繰り返しません）。
- v1.67.1 より前、この例は npm に存在しない `@anthropic-ai/mcp-server-playwright` を指定していました。古い例から書いた `.mcp.json` はその名前のままで起動に失敗するため、上の行に書き換えてください。

このサーバーのツールをエージェントが呼べるかどうかは、`[capabilities] allowed_tools` / `denied_tools` で決まります。

### 2.5 コンテナサンドボックスのデバッグ（タスクサンドボックス）

独立した「L4 サンドボックスブラウザ」層はありません。ブラウザ関連の作業は L1、L2、任意の L3 MCP サーバー、または L5（2.6 を参照）を通ります。エージェントのタスク全体を包むコンテナは**タスクサンドボックス**（`agent.toml [container] sandbox_enabled = true`）です。設定は[タスクサンドボックスガイド](task-sandbox.md)を参照してください。以下の手順は、サンドボックスのタスクがなぜ失敗したかを調べるためのものです。

```bash
# 1. 前提条件：Docker に接続できるか、イメージがあるか、どのエージェントがサンドボックス有効か
duduclaw doctor

# 2. サンドボックスが書く監査イベント
grep -E 'task_sandbox_(unavailable|bypassed|tool_violation)' ~/.duduclaw/security_audit.jsonl | tail

# 3. 残っているコンテナ（サンドボックスはコンテナにラベルを付け、タスク終了時に削除します）
docker ps -a --filter name=dudu-task-
```

- `task_sandbox_unavailable` には理由コード（`docker_unreachable`、`image_missing`、`network_disabled`、`root_user`、`no_account`、`unsupported_runtime`、`invalid_config`、`unsupported_platform`）が付きます。
- `task_sandbox_bypassed` は、`when_unavailable = "run_unsandboxed"` により隔離なしでタスクが実行されたことを示します。
- `task_sandbox_tool_violation` は、AI がファイルとシェル以外のツールを使い、タスクが停止されたことを示します。

イメージの中を似た制限で手動確認するには次のようにします（タスクの認証情報、マウント、supervisor は再現されません。イメージの中身と、読み取り専用 root で CLI が起動するかを見るだけです）。

```bash
docker run --rm -it --read-only --user "$(id -u):$(id -g)" \
  --cap-drop ALL --security-opt no-new-privileges:true \
  --tmpfs /tmp:rw,exec,nosuid,nodev,size=256m,mode=1777 \
  --memory 4g --pids-limit 128 --cpus 1 \
  --entrypoint /bin/sh <sandbox-image>
```

イメージは `config.toml [container.sandbox] image` の値（デフォルト `ghcr.io/zhixuli0406/duduclaw:v<バージョン>`）を使います。サンドボックスはモデルプロバイダーに届くよう bridge ネットワークが必要なため、この例では Docker のデフォルトネットワークのままにしています。PTC と `secaudit` が使うスクリプトサンドボックスは別のパスで、こちらは `--network=none` を使います。

### 2.6 L5 — Computer Use のデバッグ

セッションは `computer_use_orchestrator`（`crates/duduclaw-gateway/src/computer_use_orchestrator.rs`）を通じて Docker コンテナを 1 つ起動し、`[capabilities] computer_use = true` が必要です。エージェントが 8 つの `computer_*` MCP ツールを呼び出します。MCP 側（`crates/duduclaw-cli/src/mcp/computer_use_client.rs`）は薄いクライアントで、各呼び出しに署名し、loopback 上の gateway に `POST /api/internal/computer-use`、本文 `{op: start | screenshot | action | stop | status, …}` として送ります。gateway 側（`crates/duduclaw-gateway/src/computer_use_sessions/`）は、呼び出し元を認証し（`auth.rs`）、ツールのゲートと承認リストを再チェックし（`gates.rs`）、各アクションを検証してリスク評価し（`actions.rs`）、`computer_navigate` の URL を許可リストと照合し（`navigation.rs`）、高リスクのアクションの確認先となるチャットを、実行中ターンについての自身の記録から特定し（`turns.rs`）、コンテナの回収とスイープを行います（`mod.rs`、`sweep.rs`）。ネットワークの許可リストは `[capabilities.computer_use_config] allowed_domains` です。ルールは[ブラウザ自動化](../../features/ja-JP/08-browser-automation.md)を参照してください。エージェントが現在セッションを持っているかを見るには、そのコンテナ（下記）か、ブラウザ監査ログの `session_start` / `session_end` の行（5.1 節）を探します。

これ以外の入口はありません。チャットメッセージが以前に起動していた gateway 実行のループ（Anthropic の `computer_20251124` ツール、進捗をチャネルに投稿）と `native` のホストデスクトップモードは削除されました。Computer Use のキーワードを含むメッセージは、現在は通常の返信経路に進みます。`[capabilities] computer_use_mode = "native"` は今も解析されますが、ツールは `native_unsupported` でこれを拒否します。`"auto"` またはキーなしは `"container"` と同じ動作です。

エージェントごとの上限は `agent.toml [capabilities.computer_use_config]` から来ます：`max_actions`（50）、`max_session_minutes`（10）、`display_width` / `display_height`（1280x800）、`allowed_apps`、`blocked_actions`（既定は `delete_file`、`terminal`、`system_preferences`）、`auto_confirm_trusted`、`allowed_domains`。セッションマネージャは `CONTRACT.toml` の `must_not` ルールに一致するアクションもブロックしますが、そのルールは `[must_not] rules = [...]` というテーブルから読まれ（`computer_use_sessions/mod.rs`）、契約システムの他の部分が使う `[boundaries]` テーブルとは異なります。CONTRACT.toml に `[browser.*]` キーはなく、読み取るコードもありません。

#### 方式 A：Container（本番環境）

```bash
# この gateway バージョンが既定で使うイメージを pull
# （.github/workflows/computer-use-image.yml が v1.66.1 の次のリリースタグから公開）
docker pull ghcr.io/zhixuli0406/duduclaw-computer-use:v<バージョン>

# またはリポジトリのルートでローカルビルドし、gateway をローカルタグに向ける：
#   config.toml  ->  [computer_use]
#                    image = "duduclaw-computer-use:latest"
docker build -f container/Dockerfile.computer-use -t duduclaw-computer-use:latest .

# 手動で起動し、VNC で仮想ディスプレイを確認
# （例はローカルタグ。pull した ghcr イメージでも同じです）。
# ポートの公開にはネットワークが必要なので、ドメインフィルタには NET_ADMIN が必要です。
# また ALLOWED_DOMAINS が空でない場合だけ VNC の応答パケットが外に出られます。
docker run --rm -p 5900:5900 \
  --cap-add=NET_ADMIN \
  -e ALLOWED_DOMAINS=example.com \
  -e DISPLAY_SIZE=1280x800 \
  -e VNC_ENABLED=true \
  -e VNC_PASSWORD=debug123 \
  duduclaw-computer-use:latest

# VNC クライアントで接続して確認
# macOS: open vnc://localhost:5900
```

イメージ（約 1.09 GB）は `debian:trixie-slim` をベースに、Debian の `chromium` パッケージ、Xvfb、ウィンドウマネージャ `openbox`、任意の VNC（`x11vnc`）、`xdotool`、`scrot`、ドメインフィルタ、`xdotool getactivewindow` によるヘルスチェック、`duduclaw-eval-dom` ヘルパー用の Python 3（3.5 節を参照）を含みます。Openbox と Chromium は非特権ユーザー `sandbox` で動きます。entrypoint が root のままなのは iptables を設定するためだけです。Chromium はキオスクモードで 0,0 から仮想ディスプレイ全体を覆い、デバイススケールファクタは 1 に固定されるため、ページ座標がそのままスクリーンショットのピクセルになります。DevTools ポートはコンテナ内の 127.0.0.1 だけで待ち受けます。ブラウザが終了した場合（エージェントがウィンドウを閉じた場合など）、entrypoint が再起動します。Chromium は `container/scripts/chromium-policy.json` のマネージドポリシーを読み込みます（ページからのローカルネットワークや loopback へのアクセスなし、シークレット／ゲストウィンドウなし、ファイルダイアログ・印刷・ダウンロードなし、ポップアップとデバイス権限はブロック、`file://` / `chrome://` / `devtools://` / `view-source:` / `javascript://` はブロック。一覧は[ブラウザ自動化](../../features/ja-JP/08-browser-automation.md)を参照）。`DeveloperToolsAvailability` は意図的に未設定にしています。設定すると、ヘルパーが必要とする loopback の DevTools プロトコルまで無効になるためです。`duduclaw-navigate` は URL を stdin からのみ読みます（`printf 'https://example.com/\n' | docker exec -i <container> duduclaw-navigate`）。引数を渡すと使い方エラーになります。スクリーンショットは `/tmp/duduclaw-root/screen.png`（root 所有でモード 0700、ブラウザの起動前に作成されるディレクトリ）に保存され、Chromium のログもそこにあります。

セッションが使うイメージ：既定は `ghcr.io/zhixuli0406/duduclaw-computer-use:v<gateway のバージョン>` で、`.github/workflows/computer-use-image.yml` が git タグ `v*`（または手動実行）で公開します。このワークフローはネイティブランナー上で `linux/amd64` と `linux/arm64` をそれぞれビルドし、`:<tag>` と `:latest` を push する前に gateway 自身のコンテナフラグで各ビルドをスモークテスト（ウィンドウマネージャの起動、スクリーンショット 1 枚、`duduclaw-eval-dom` が `[]` を返すこと）します。初めて動くのは v1.66.1 の次のリリースタグなので、v1.66.1 以前には公開済みイメージがなく、`scripts/release.sh verify` もこれを確認しません。上書きはグローバルな `config.toml [computer_use] image = "<ref>"`（digest 参照も可）だけで、エージェントごとのイメージキーはありません。`[computer_use]` セクションが無効な場合、Computer Use はメッセージ付きで利用不可になり、既定値には戻りません（`crates/duduclaw-gateway/src/computer_use_image.rs`）。イメージは自動で pull されません：`docker run` は `--pull never` を付け、各セッションの前に存在確認（`docker image inspect`）を行います。ローカルビルドの `duduclaw-computer-use:latest`（旧既定値）しかないマシンでは、バージョン付きイメージを pull するか上書きキーを設定する必要があります。

オーケストレータ自身が起動するコンテナは `--read-only`、256 MB の tmpfs `/tmp`、1 CPU、512 MB メモリ、プロセス上限 512（Chromium のスレッドもこの上限に数えられます。以前の 100 は web worker を使う普通のページで使い切られました）で動き、セッションが開始時に解決できた許可リストのホストを持つ場合を除き `--network=none` になります。すべてのコンテナには `--security-opt no-new-privileges` も付きます。許可リストのホストがある場合、gateway は `--network bridge` を明示的に付け、ホストごとに `--add-host <host>:<address>` を 1 つ付け、アドレスを `ALLOWED_IPS` としてドメインフィルタに渡し、`--cap-add=NET_ADMIN` を付けます。フィルタはこの権限でデフォルト拒否の送信ルールを設定します（それらのアドレスへの TCP 443 のみ、DNS なし、loopback は `127.0.0.1` / `::1` のみ、Docker のリゾルバ `127.0.0.11` は拒否。ネットワークはあるが許可リストのないコンテナにも同じ loopback ルールが適用されます）。権限がないと、ネットワーク経路を持つコンテナは起動を拒否します。コンテナ名は `duduclaw-cu-` で始まるため、`docker ps -a --filter name=duduclaw-cu-` で確認できます。各コンテナにはラベル `com.duduclaw.computer-use.home`（所有する DuDuClaw ホスト）と `com.duduclaw.computer-use.deadline`（unix 秒）が付き、gateway 起動時とその後 10 分ごとのスイープが、このホストのコンテナのうち終了済みのもの、またはその期限を 600 秒以上過ぎたものを削除します。

#### 方式 B：Claude Code Computer Use MCP（ローカルデバッグ限定）

> **制約**：macOS 限定、Pro/Max プラン、対話セッションのみ、マシン単位のロック

**前提条件：**
- macOS
- Claude Code v2.1.85 以降
- Claude Pro または Max のサブスクリプション

**有効化の手順：**

1. Claude Code 内で `/mcp` を実行
2. `computer-use` server を見つけて **Enable** を選択
3. 初回利用時、macOS が以下の許可を求めます。
   - **アクセシビリティ**（System Settings → Privacy & Security → Accessibility）
   - **画面収録**（System Settings → Privacy & Security → Screen Recording）

**使い方：**
```bash
# Claude Code の対話セッション内で
claude

# Claude が computer-use ツールを使ってデスクトップを直接操作します
> Safari を開いて example.com を閲覧してください
```

**注意事項：**
- 本番環境では利用不可（デバッグ専用）
- 非対話の `-p` モードは非対応
- マシン単位のロック — 同時に利用できる Claude Code セッションは 1 つのみ
- トークン消費量が非常に高い（操作のたびに画面全体のスクリーンショットが必要）
- 座標精度に限界がある（視覚的な誤認識のリスク）
- セットアップコストはゼロ（Claude Code に組み込み済み）
- ブラウザに限らず、任意の macOS アプリケーションを操作できる

---

## 3. セキュリティ機構

### 3.1 Input Guard（インジェクションスキャン）

Agent に入るユーザー入力は `duduclaw-security` の `input_guard` スキャナーを通過
します。これはリスクスコアリング方式（0-100、上限あり）を採用しており、7 つの重
み付きルールでスコアを積算し、しきい値（既定 60）に達すると入力をブロックして
`security_audit.jsonl` に記録します。`instruction_override`、`role_hijack`、
`tool_abuse`、`data_exfiltration` は、スコアに関係なく 1 回一致しただけでブロック
します（`crates/duduclaw-security/src/input_guard.rs`）。

| ルール | 重み | 検出例 |
|------|------|---------|
| instruction_override | 40 | "ignore previous instructions" |
| role_hijack | 35 | "act as", "your new role" |
| system_prompt_extraction | 30 | "reveal your instructions" |
| tool_abuse | 30 | ツールの誤用を誘導するプロンプト |
| encoding_bypass | 25 | Base64 などのエンコーディングによるバイパス |
| data_exfiltration | 25 | "send to" + URL |
| termination_manipulation | 30 | "the task is never complete" |

さらに Unicode 正規化（ゼロ幅文字、同形文字）によりバイパスを防ぎます。元の文字列にゼロ幅文字が 4 つ以上あると、スコアに 20 が加算されます。

> 注意：L1/L2 でスクレイピングされた Web コンテンツは、現時点では独立したコン
> テンツ分類スキャンを経由していません。`web_fetch` 層の防御は SSRF 検証
> （scheme / 内部 IP / metadata エンドポイント / DNS rebinding / リダイレクトの
> 都度再検証）＋ 5MB 上限 ＋ レート制限です。

### 3.2 Emergency Stop

- チャンネル内のセーフワード：`!STOP` / `!停止`（単一 scope）、`!STOP ALL` /
  `!全部停止`（全域）で発動、`!RESUME` / `!恢復` で復帰。failsafe システムが処理
  し、管理者権限が必要です。
- このビルドの Dashboard には動作する E-Stop 操作がありません。呼び出し先の
  `browser.emergency_stop` RPC は常に「Browser automation features require the Pro
  edition」というエラーを返します。停止状態は failsafe manager のメモリ上にある
  ため（`crates/duduclaw-security/src/failsafe.rs`）、gateway を再起動しても解除さ
  れます。

### 3.3 Tool Approval（HITL ApprovalBroker）

高リスクな操作は統一された ApprovalBroker を経由します（`approvals.db`、TTL 失
効は拒否扱い、fail-closed）。
- `agent.toml [capabilities] approval_required_tools` で承認が必要なツールを宣言します。`irreversible_tools` は常に確認し、`maybe_irreversible_tools` はモデルの判定が「取り消せない可能性がある」としたときに確認します（fail-closed）。リストは、呼び出しを行うエージェントについて読み込まれます。v1.67.0 より前は、エージェントが自身の MCP サーバー経由で行った呼び出しが、gateway の内部キー名に対してチェックされていたため、この 3 つのリストはそこでは効果がありませんでした。
- 通常のツールの要求は種別 `mcp_call` として登録され、ツール呼び出しとして表現されます。インストール系のツールは種別 `mcp_install` のままです。8 つの `computer_*` ツールは、代わりに gateway の Computer Use ルートが確認します（3 つのどのリストでも常に）。そのため、人に二重に尋ねることはありません。
- autopilot の `require_approval` アクションも同じ broker を経由
- 詳細は observability / capabilities 関連ドキュメントを参照

### 3.4 ユーザーペアリング（Pairing）

チャンネルレベルのユーザーアクセス制御で、`channel_settings`（global scope、チ
ャンネル種別ごと）に保存されます。
- `require_pairing = "true"`：未承認のユーザーはペアリングしないと会話できない
- `allowed_users` / `blocked_users`：JSON 配列のホワイトリスト／ブラックリスト
- 流れ：管理者が MCP tool `pairing_manage`（action=generate）で 6 桁のペアリング
  コード（有効期限 5 分）を生成 → ユーザーがチャンネルで `/pair <コード>` を送信
  → 承認され `~/.duduclaw/access_control.json` に永続化される
- ブルートフォース対策：単一コード 5 回失敗でロック、再生成をまたいで累計 15
  回が上限、定数時間比較、コードは SHA-256 で保存

### 3.5 Screenshot Masking

L5 オーケストレータが撮影するスクリーンショットは、モデルや監査フォルダーに渡る前にすべて `capture_masked_screenshot`
（`crates/duduclaw-gateway/src/computer_use_orchestrator.rs`）を通ります。

- 固定の 3 つの CSS セレクタ `input[type=password]`、`.credit-card`、`[data-sensitive]`
  （`crates/duduclaw-gateway/src/computer_use.rs` の `MaskingConfig::default()`）に一致する
  要素の位置をコンテナに問い合わせ、その領域を黒で塗りつぶします。
- 検出は `docker exec <コンテナ> duduclaw-eval-dom '<js>'` を実行し、gateway 側
  に 10 秒のタイムアウトがあります。ヘルパー（`container/scripts/duduclaw-eval-dom`、
  Python 標準ライブラリのみ）はコンテナ内 127.0.0.1 の Chromium DevTools ポートに
  接続し、表示中の唯一のページで式を評価します。ヘルパー自身にも 5 秒の上限があり
  ます。式、ジオメトリの読み戻し、`document.visibilityState` のチェックはすべて分離
  ワールド（`Page.createIsolatedWorld`、`container/scripts/duduclaw_cdp.py`）で動くため、
  それらの関数を再定義するページでも矩形を動かせません。ヘルパーは、複数のページが表示されている場合は終了ステータス 3、それ以外の失敗では 1 で終了し、gateway はこのステータス（テキストではなく）を `mask_reason` の `several_pages` または `helper_failed` に対応づけます。表示中のページが 2 つ以上ある
  場合（たとえば `ctrl+n` で開いたウィンドウ。これを防ぐブラウザポリシーはありません）、
  ヘルパーは失敗し、次の `computer_navigate` が余分なページを閉じるまでスクリーンショッ
  トは全体がマスクされます。矩形はデバイスピクセル比で CSS ピクセルからスクリーンショッ
  トのピクセルに換算し（ブラウザのズームにも対応します）、外側に丸め、各辺を 1 px 広げ、
  画面内に切り詰め、見える面積のない矩形は捨てます。テストページでの実測では、覆われな
  かった機密ピクセルはなく、余分に覆うのは各辺最大 2 px で、ブラウザのズーム後も同様で
  した。
- 次の場合、ヘルパーは 0 以外の終了コードで終わり、gateway はスクリーンショット全
  体を黒で塗りつぶします（fail closed）：ブラウザが動いていない、表示中のページが
  ない、または 2 つ以上ある、ブラウザウィンドウが 0,0 にないかディスプレイ全体を
  覆っていない、visual viewport がピンチズームまたはスクロールされている、
  JavaScript が例外を投げた、5 秒の上限を超えた。
- 検出できない範囲：ページの一部ではないブラウザ UI（ズームの吹き出し、権限の確認、
  自動入力のドロップダウン、alert ダイアログ）、クロスオリジン iframe 内の内容、
  shadow DOM 内の内容。これらに機密フィールドが表示された場合、スクリーンショット
  に写ったままになります。
- DOM マスクの後に前面ウィンドウのタイトルを読み取ります。タイト
  ルに認証情報を示す語（`1password`、`bitwarden`、`lastpass`、`keepass`、
  `keychain`、`密碼`、`password`、`credential`、`ssh`、`gpg`、`pgp`）が含まれて
  いれば、全体をマスクします。タイトルを読み取れない場合（コマンドエラー、タイム
  アウト、0 以外の終了コード、UTF-8 でない出力）も全体をマスクします（fail
  closed）。正常に読み取れた空のタイトルではマスクしません。

セレクタと塗りつぶし色は設定できません。`CONTRACT.toml` と `agent.toml` のどちら
にも、読み込まれるマスク設定キーはありません。

---

## 4. Browser Test Suite

ブラウザ用のテストコマンドはありません。`duduclaw test` が受け取るのは Agent 名と
任意の `--bank` ファイルだけで、Agent の契約と入力スキャナーに対するレッドチーム
テストを行います（[CONTRACT.toml 仕様](../../spec/contract-toml-spec.md)を参照）。
`--browser` フラグはありません。ブラウザ関連のコードはソースツリー内の単体テストで
カバーされています。

```bash
# L1 / L2 の取得経路：URL 検証、SSRF ゲート、キャッシュ
cargo test -p duduclaw-gateway web_fetch

# L5 のスクリーンショットマスク、アクション解析
cargo test -p duduclaw-gateway computer_use

# L5 の監査ログとスクリーンショット保存
cargo test -p duduclaw-gateway screenshot_audit
```

---

## 5. 監査とモニタリング

### 5.1 ブラウザと computer use の操作の記録先

| 記録 | 書き込み元 | 内容 |
|---|---|---|
| `~/.duduclaw/tool_calls.jsonl` | MCP ディスパッチャ（`crates/duduclaw-cli/src/mcp/dispatch.rs`） | 状態を変更するツールの呼び出しごとに 1 行。マスク済みの入力と結果テキストを含みます。8 つの `computer_*` ツールすべてが対象で、入力は縮約されます：`computer_type` は `chars=<n>`、`computer_navigate` はホストとパスの長さ（パスもクエリもなし）、`computer_screenshot` は画像なし。ここの 1 行は呼び出し 1 回を表し、実行されたかどうかは次の行にあります。`web_fetch_cached` と `web_extract` は読み取り専用のため、ここには書き込まれません。 |
| `~/.duduclaw/audit/browser/audit.jsonl` | Computer Use セッションマネージャ（`crates/duduclaw-gateway/src/screenshot_audit.rs`） | ハッシュチェーン用の `_prev_hash` を持つ行。tier は `L5a`。セッションは `session_start`、`screenshot`（`fully_masked` と `mask_reason` を持つ。値は `several_pages`、`title_sensitive`、`title_unreadable`、`helper_failed`、または null）、実行した各アクション（`left_click`、`type`、`key`、`scroll`、…）ごとのリスク評価付きの 1 行、`navigate`（`url` はクエリとフラグメントを除いた `https://<host><path>`、および `domain`）、`action_refused`、`session_end` を書きます。入力したテキストは文字数のみ記録されます。16 MiB を超えるとチェーンを保ったまま `audit.jsonl.old` にローテーションされます。L1 と L2 はここに書き込みません。 |
| `~/.duduclaw/audit/browser/screenshots/<agent_id>/<UTC タイムスタンプ>.png` | 同じマネージャ | `computer_screenshot` の呼び出しごとのマスク済み画像。 |
| `~/.duduclaw/security_audit.jsonl` | input guard、タスクサンドボックスなどのセキュリティイベント | ブロックされた入力と `task_sandbox_*` イベント（2.5 節）。 |

```bash
# 直近の L5 アクション
tail -20 ~/.duduclaw/audit/browser/audit.jsonl | jq .

# 直近の computer_* ツール呼び出し
grep '"tool_name":"computer_' ~/.duduclaw/tool_calls.jsonl | tail
```

チャンネルで `/replay [n]`（既定 5）を送ると、その Agent の
`audit/browser/audit.jsonl` の最後の `n` 行が表示されます。`browser_audit_log` と
いう MCP ツールはありません。Dashboard の `browser.audit_log` RPC も、
`browser.emergency_stop` と同じ「Pro edition」エラーを返します。

### 5.2 スクリーンショットの保持

保存されたスクリーンショットは 7 日間保持されます。Computer Use のスイープ（gateway 起動時、その後 10 分ごと）が、それより古いファイルを削除します。各スタッフのフォルダーには最大 500 ファイルかつ 200 MiB までしか置かれず、新しいスクリーンショットを保存すると、どちらかの上限を超えた分が古いものから削除されます。これらを表示する Dashboard ページはありません。

---

## 6. よくある質問

### Agent にヘッドレスブラウザのツールがない
```bash
# この Agent にブラウザ server が登録されているか確認
cat ~/.duduclaw/agents/my-bot/.mcp.json
```
自動で追加する仕組みはありません。項目を自分で書き込む（2.4 節）か、Dashboard の
MCP marketplace（`marketplace.install`）からその Agent に `playwright` /
`browserbase` をインストールしてください。その後、`[capabilities] denied_tools` が
そのツールをブロックしていないか確認してください。

### タスクサンドボックスが起動しない
```bash
# Docker が起動しているか確認
docker info

# タスクサンドボックスの前提条件（Docker、イメージ、サンドボックス有効のエージェント）
duduclaw doctor

# サンドボックスイメージがこのマシンにあるか確認（自動ダウンロードされません）
docker image inspect ghcr.io/zhixuli0406/duduclaw:v<バージョン>
```
エラーの対応表は[タスクサンドボックスガイド](task-sandbox.md)を参照してください。

### Computer Use を起動できない
`computer_session_start` は「電腦操作無法啟動」で始まるエラーを返し、原因を示します。
- イメージがローカルにない：イメージ名と 2 つの対処方法を示します。`docker pull <image>`、
  またはリポジトリのルートでローカルビルド（`docker build -f container/Dockerfile.computer-use -t duduclaw-computer-use:latest .`）
  してから `config.toml [computer_use] image = "duduclaw-computer-use:latest"` を設定します。
- Docker が存在確認に応答しない：Docker を起動してから再試行します。
- `config.toml` の `[computer_use]` が無効（未知のキー、使えないイメージ参照、解析できないファイル）：
  修正してください。既定イメージには戻りません。

```bash
# 「電腦操作」の行：使用するイメージ、ローカルにあるか、computer_use = true のエージェント
duduclaw doctor
```
誰も Computer Use を使っていなければこの行は Pass、使っているエージェントがいてイメージ
がない、Docker に接続できない、設定が無効のいずれかなら Warn です。いずれかのエージェントがまだ
`computer_use_mode = "native"` のままの場合も Warn になり、該当するエージェントを挙げます：そのキーを削除するか
`"container"` に設定してください。この行には、エージェントごとの使える許可リストのホストと無視された項目も表示されます。

その他のよくあるツールの答え：
- ツールが `127.0.0.1:<port>` の gateway に接続できないと言う：ツールには gateway が動いている必要があります。ツール自身がコンテナを起動することはありません。
- ツールがアクセスを拒否されたと言う（`unauthorized`）：MCP プロセスが gateway によって起動されていない（環境に内部キーもエージェントトークンもない）、時計が 60 秒を超えてずれている、または `~/.duduclaw/identity.key` がありません。gateway は正確な理由を debug レベルでのみログに出します。
- `computer_navigate` が拒否された：メッセージを読んでください。`allowed_domains` がない場合は追加すべき設定が示され、ある場合はこのセッションが開けるホストが列挙されます。セッション開始後に許可リストへ追加したホストには、新しいセッションが必要です。

### Computer-use コンテナが起動直後に終了する
```bash
# セッションのコンテナが残っていれば、起動ログを読む
docker logs <コンテナ>

# または、ネットワークあり＋許可リストありの起動を手動で再現する
docker run --rm -e ALLOWED_DOMAINS=example.com <computer-use イメージ>
```
`[domain-filter] FATAL: cannot install the default-deny egress policy` は、コンテナに
ネットワーク経路があるのに `NET_ADMIN` 権限がないため、フィルタが送信を無制限のまま
動くことを拒否したことを示します。gateway がこの権限を付けるのは、`ALLOWED_IPS`（許可リス
トのホストが解決できたツールセッション）または空でない `ALLOWED_DOMAINS` がある場合だけです。
両方を設定するとフィルタに拒否されます。ネットワーク付きで手動起動するコンテナには
`--cap-add=NET_ADMIN` が必要です。`--network=none` ならこの権限なしで起動します。

### Computer-use のスクリーンショットが全面真っ黒になる
マスク用ヘルパーが失敗したため、スクリーンショット全体がマスクされています（3.5 節）。
セッションのコンテナに対して手動で実行してください。
```bash
docker exec <コンテナ> duduclaw-eval-dom 'JSON.stringify([])'
```
正常なら `[]` を出力します。そうでなければ stderr に理由（ブラウザが動いていない、
表示中のページがない、表示中のページが複数ある、ウィンドウが 0,0 にない、visual
viewport がズームされている、タイムアウト）が出ます。ヘルパーを含まない古いイメー
ジでは "executable file not found" エラーになるので、現在のイメージを pull するか
再ビルドしてください（2.6 節）。

### Emergency Stop が解除できない
管理者アカウントでチャンネルに `!RESUME`（または `!恢復`）を送ってください。停止状
態はメモリ上にあるため、gateway を再起動しても解除されます。削除するシグナルファイ
ルはなく、`emergency_stop` という MCP ツールもありません。
