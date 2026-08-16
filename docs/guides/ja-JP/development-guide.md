# DuDuClaw Development Guide

> Agent 開発、ブラウザ自動化のデバッグ、ローカル環境セットアップのガイド。

---

## 1. クイックスタート

### 1.1 ローカル開発環境

```bash
# 開発モードを起動（Playwright MCP を自動設定）
duduclaw dev --port 18789

# または特定の Agent を指定
duduclaw dev --agent my-bot --port 18789
```

開発モードは以下を行います。
- Gateway + Dashboard を起動（`http://localhost:18789`）
- `browser_via_bash` を有効化した Agent に対して `.mcp.json`（Playwright MCP）を自動生成
- ログをリアルタイムで Dashboard にストリーミング

### 1.2 Agent ディレクトリ構成

```
~/.duduclaw/agents/my-bot/
├── agent.toml          # Agent 設定（model、budget、capabilities）
├── SOUL.md             # Agent の人格と行動指針
├── CLAUDE.md            # Claude Code プロジェクト指示（任意）
├── CONTRACT.toml       # 行動契約（boundaries、browser restrictions）
├── .mcp.json           # MCP server 設定（自動生成）
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
provider = "antigravity"   # claude | codex | gemini | antigravity | openai_compat
fallback = "claude"        # 検出できない場合に使うバックエンド
```

| Provider | CLI バイナリ | 認証 | 備考 |
|----------|-----------|------|------|
| `claude` | `claude`（常に利用可能、コア） | OAuth / API Key ローテーション | デフォルトバックエンド |
| `codex` | `codex` | OpenAI | — |
| `gemini` | `gemini` | `GEMINI_API_KEY` / OAuth | 個人版 OAuth は 2026-06-18 に廃止；有料 API キーは引き続き利用可 |
| `antigravity` | `agy`（`~/.local/bin/agy`） | `ANTIGRAVITY_API_KEY` / OAuth | Gemini CLI の公式後継、マルチモデル（Gemini 3.x + Claude + GPT-OSS） |
| `openai_compat` | HTTP（CLI なし） | プロバイダーごとのキー | Exo / llamafile / vLLM などの OpenAI 互換エンドポイント |

**Antigravity（`agy`）固有の注意事項**（詳細は
[TODO-antigravity-cli-migration.md](../../todo/TODO-antigravity-cli-migration.md) を参照）。

- Agent ディレクトリは自動的に agy の `trustedWorkspaces` に追加され、headless 実
  行時に「このワークスペースを信頼しますか？」というプロンプトで止まることを回
  避します。
- print モードには JSON 出力がないため、トークン使用量は CJK 対応のヒューリステ
  ィック推定値であり、正確な値ではありません。
- Gateway が正常に呼び出せるようにするには、`agy` がインストールされたマシンで
  一度対話的ログイン（OAuth）を完了するか、`ANTIGRAVITY_API_KEY` を設定しておく
  必要があります。

---

## 2. ブラウザ自動化のデバッグ（L1-L5）

### 2.1 アーキテクチャ概要

```
Agent Request → BrowserRouter (<1ms)
  ├── L1: API Fetch        (reqwest — ゼロコスト)
  ├── L2: Static Scrape    (CSS selector — ゼロコスト)
  ├── L3: Headless Browser (Playwright MCP — 低コスト)
  ├── L4: Sandbox Browser  (Container + Playwright — 中コスト)
  └── L5: Computer Use     (Virtual display + Claude vision — 高コスト)
```

基本原則：**API で済むならブラウザを開かず、ヘッドレスで済むなら computer_use を使わない。**

### 2.2 L1 — API Fetch のデバッグ

```bash
# SSRF 防御テストを実行
duduclaw test --browser

# MCP tool を手動でテスト
claude -p "Use web_fetch_cached to fetch https://example.com"
```

**確認項目：**
- `file://`、`javascript:`、`data:` スキームがブロックされること
- 内部 IP（127.0.0.0/8、10.0.0.0/8、172.16.0.0/12、192.168.0.0/16）がブロックされること
- IPv6 loopback `[::1]` がブロックされること
- キャッシュヒットが機能すること（同一 URL への 2 回目のリクエストは `cached: true` を返す）
- レート制限が機能すること（Agent ごとに毎分 10 回）

### 2.3 L2 — CSS 抽出のデバッグ

```bash
# CSS セレクタでの抽出をテスト
claude -p 'Use web_extract on https://example.com with selector "h1" and format "text"'
```

**対応フォーマット：**
- `text` — プレーンテキストのコンテンツ
- `html` — 内部 HTML
- `json` — 構造化 JSON（tag、attributes、children を含む）

### 2.4 L3 — Playwright MCP のデバッグ

```bash
# Playwright MCP が設定済みか確認
cat ~/.duduclaw/agents/my-bot/.mcp.json

# 存在しない場合は手動生成
# agent.toml で browser_via_bash = true を設定し、Gateway を再起動
```

**`.mcp.json` の例：**
```json
{
  "mcpServers": {
    "playwright": {
      "command": "npx",
      "args": ["@anthropic-ai/mcp-server-playwright", "--headless"],
      "env": {}
    }
  }
}
```

**前提条件：**
- `npm install -g @anthropic-ai/mcp-server-playwright`
- Playwright Chromium：`npx playwright install chromium`

### 2.5 L4 — Container Sandbox のデバッグ

```bash
# サンドボックスイメージをビルド
docker build -f container/Dockerfile.browser-sandbox -t duduclaw/browser-sandbox .

# 手動起動（テスト用）
docker run --rm --read-only --tmpfs /tmp:size=256m \
  -e ALLOWED_DOMAINS="example.com,httpbin.org" \
  duduclaw/browser-sandbox

# ドメイン指定なし = ネットワークなし
docker run --rm --read-only --tmpfs /tmp:size=256m --network=none \
  duduclaw/browser-sandbox
```

**`CONTRACT.toml` の例：**
```toml
[browser]
enabled = true
max_tier = "sandbox_browser"
trusted_domains = ["example.com", "*.gov.tw"]
blocked_domains = ["*.onion", "localhost"]

[browser.restrictions]
allow_form_submit = false
max_pages_per_session = 20
max_session_minutes = 10
screenshot_audit = true
require_human_approval_for = ["form_submit", "login", "payment_*"]
```

### 2.6 L5 — Computer Use のデバッグ

#### 方式 A：Container（本番環境）

```bash
# computer-use イメージをビルド
docker build -f container/Dockerfile.computer-use -t duduclaw/computer-use .

# 起動（VNC 付き）
docker run --rm -p 5900:5900 \
  -e DISPLAY_SIZE=1280x800 \
  -e VNC_ENABLED=true \
  -e VNC_PASSWORD=debug123 \
  duduclaw/computer-use

# VNC クライアントで接続して観察
# macOS: open vnc://localhost:5900
```

**`CONTRACT.toml` の L5 設定：**
```toml
[browser.computer_use]
enabled = true
max_actions = 50
container_required = true
display_size = "1280x800"
blur_patterns = ["input[type=password]", ".credit-card", "[data-sensitive]"]
```

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
します。これはリスクスコアリング方式（0-100）を採用しており、6 つの重み付きル
ールでスコアを積算し、しきい値を超えると入力をブロックして `security_audit.jsonl`
に記録します。

| ルール | 重み | 検出例 |
|------|------|---------|
| instruction_override | 40 | "ignore previous instructions" |
| role_hijack | 35 | "act as", "your new role" |
| system_prompt_extraction | 30 | "reveal your instructions" |
| tool_abuse | 30 | ツールの誤用を誘導するプロンプト |
| encoding_bypass | 25 | Base64 などのエンコーディングによるバイパス |
| data_exfiltration | 25 | "send to" + URL |

さらに Unicode 正規化（ゼロ幅文字、同形文字）によりバイパスを防ぎます。

> 注意：L1/L2 でスクレイピングされた Web コンテンツは、現時点では独立したコン
> テンツ分類スキャンを経由していません。`web_fetch` 層の防御は SSRF 検証
> （scheme / 内部 IP / metadata エンドポイント / DNS rebinding / リダイレクトの
> 都度再検証）＋ 5MB 上限 ＋ レート制限です。

### 3.2 Emergency Stop

- チャンネル内のセーフワード：`!STOP` / `!停止`（単一 scope）、`!STOP ALL` /
  `!全部停止`（全域）で発動、`!RESUME` / `!恢復` で復帰。failsafe システムが処理
  し、管理者権限が必要です。
- Dashboard のヘッダーからワンクリックで E-Stop / Resume できます。

### 3.3 Tool Approval（HITL ApprovalBroker）

高リスクな操作は統一された ApprovalBroker を経由します（`approvals.db`、TTL 失
効は拒否扱い、fail-closed）。
- `agent.toml [capabilities] approval_required_tools` で承認が必要なツールを宣言
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

L5 Computer Use は機微な領域を自動検出してマスクします。
- `input[type=password]` — パスワード欄
- `.credit-card` — クレジットカードフォーム
- `[data-sensitive]` — カスタムの機微領域

マスクのルールは `CONTRACT.toml [browser.computer_use] blur_patterns` で定義しま
す。

---

## 4. Browser Test Suite

```bash
# ブラウザテストを一式実行
duduclaw test --browser

# テスト対象：
# [L1] SSRF prevention (4 URLs)
# [L1] HTTP fetch (httpbin.org)
# [L2] CSS extraction
# [Guard] Content injection scanner
```

---

## 5. 監査とモニタリング

### 5.1 Browser Audit Log

すべてのブラウザ操作は `~/.duduclaw/audit/browser/audit.jsonl` に記録されます。

```bash
# 直近の操作を確認
tail -20 ~/.duduclaw/audit/browser/audit.jsonl | jq .

# MCP tool 経由で照会
claude -p "Use browser_audit_log to show last 10 entries"
```

### 5.2 Screenshot の監査

スクリーンショットは `~/.duduclaw/audit/browser/screenshots/{agent_id}/` に保存
されます。
- フォーマット：`{timestamp}.png`
- デフォルトで 7 日間保持
- Dashboard の Security ページから閲覧可能

---

## 6. よくある質問

### Playwright MCP の接続に失敗する
```bash
# インストール済みか確認
npx @anthropic-ai/mcp-server-playwright --version

# .mcp.json が正しいか確認
cat ~/.duduclaw/agents/my-bot/.mcp.json
```

### Docker sandbox が起動しない
```bash
# Docker が起動しているか確認
docker info

# イメージがビルド済みか確認
docker images | grep duduclaw

# 手動でテスト
docker run --rm duduclaw/browser-sandbox echo "OK"
```

### Emergency Stop が解除できない
```bash
# シグナルファイルを手動でクリア
rm ~/.duduclaw/emergency_stop

# または MCP tool 経由で
claude -p 'Use emergency_stop with action "resume"'
```
