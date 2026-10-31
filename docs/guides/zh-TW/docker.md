# Docker 安裝指南

> 適用版本：v1.8.23+
> 最後更新：2026-04-22

本指南涵蓋 Docker Compose 部署、Codex 裝置碼登入、API key、憑證 volume、channel webhook 與更新。

若只是想在本機試跑，請先參考
[docs/deployment-guide.md §1](deployment-guide.md#1-本機開發)
的原生安裝方式；原生安裝通常啟動更快、除錯更簡單。
Docker 適合伺服器部署、隔離執行環境、或需要統一環境的團隊。

---

## 1. 概述

DuDuClaw 的 Docker 映像建置於 `container/Dockerfile.server`，採三階段建置：

| Stage | 內容 |
|-------|------|
| 1. frontend-builder | `node:22-slim` + 建置 React/TS 前端 |
| 2. rust-builder | `rust:slim` + 編譯 `duduclaw` release 二進位 |
| 3. production | `python:3.12-slim` + 三大 AI CLI + Docker CLI + 執行期 |

最終映像內建：

- `duduclaw` 主程式（Rust，含 dashboard）
- `@anthropic-ai/claude-code`、`@openai/codex`（透過 `npm i -g`）
- `docker.io` CLI（掛載 socket 後面那個 Docker daemon 的用戶端；啟用任務沙箱時才會用到）
- （選用）Python 3.12：僅 LLMLingua-2 提示壓縮器（需 `llmlingua`）才需要；Skill 安全掃描與通道回覆已是 Rust-native，不再依賴 Python

---

## 2. 前置需求

| 項目 | 版本 | 備註 |
|------|------|------|
| Docker Engine | ≥ 24.0 | Linux 建議直接裝原生版；macOS / Windows 用 Docker Desktop 或 Colima |
| Docker Compose | v2.20+ | 內建於現代 Docker；指令為 `docker compose`，非舊的 `docker-compose` |
| Git | 任意 | Clone 原始碼用 |
| 磁碟空間 | ≥ 4 GB | 建置期間會暫用 ~3 GB；成品映像約 ~1.2 GB |
| Port | `18789` | 預設 gateway port，可改 |

若要讓 channel webhook（LINE、WhatsApp、Feishu 等走 webhook 的通道）能收到外部訊息，
還需要一個 **公開 HTTPS URL**。最簡單的方案是
[Tailscale Funnel](#11-tailscale-funnel公開-https-給-webhook) 或
[Cloudflare Tunnel](deployment-guide.md#4-cloudflare-tunnel長期穩定)。

---

## 3. 快速開始

```bash
# 1. Clone 原始碼
git clone https://github.com/zhixuli0406/DuDuClaw.git
cd DuDuClaw

# 2. 建立 .env（依需求填入）
cp .env.example .env
$EDITOR .env

# 3. 啟動
docker compose up -d

# 4. 檢視啟動日誌
docker compose logs -f duduclaw

# 5. 驗證
curl http://localhost:18789/health
# {"status":"ok","version":"1.8.23", ...}

# 6. 開啟 Dashboard
open http://localhost:18789
```

首次啟動時 `server-entrypoint.sh` 會偵測到
`~/.duduclaw/config.toml` 不存在，自動執行 `duduclaw onboard --yes`
產生基本設定檔（加密儲存 API key 與 channel token）。

---

## 4. 環境變數（`.env`）

`docker-compose.yml` 只會讀取已定義的變數，**缺少的變數會傳入空字串**
（因為都用 `${VAR:-}` 語法）。下方清單按用途分組。

### 4.1 Runtime 認證（任選其一以上）

| 變數 | 用途 | 備註 |
|------|------|------|
| `CLAUDE_CODE_OAUTH_TOKEN` | Claude Code 長效 token | `claude setup-token` 產生，適合無瀏覽器的容器環境。屬於訂閱憑證，使用前請先看第 6 節的風險說明 |
| `ANTHROPIC_API_KEY` | Claude API key | Fallback，按量計費 |
| `OPENAI_API_KEY` | Codex / OpenAI-compat API key | Fallback；ChatGPT Plus/Pro OAuth 走 volume |
| `GEMINI_API_KEY` | Gemini API key | Fallback；Google OAuth 走 volume |

至少需要其中一組可用。DuDuClaw 會依 `agent.toml [runtime]` 與
`[model] api_mode` 決定使用哪一個。詳見
[§6 CLI 認證設定](#6-cli-認證設定)。

### 4.2 Channel Tokens

| 變數 | Channel |
|------|---------|
| `LINE_CHANNEL_TOKEN` / `LINE_CHANNEL_SECRET` | LINE Messaging API |
| `TELEGRAM_BOT_TOKEN` | Telegram Bot |
| `DISCORD_BOT_TOKEN` | Discord Bot |
| `SLACK_BOT_TOKEN` / `SLACK_APP_TOKEN` | Slack Socket Mode |
| `WHATSAPP_ACCESS_TOKEN` 等 | WhatsApp Cloud API（見 `.env.example`）|
| `FEISHU_APP_ID` / `FEISHU_APP_SECRET` | 飛書 |

只需要填入你實際會用到的 channel。沒填的變數不會啟動對應 channel。

> `docker-compose.yml` 目前只預設注入 `LINE_*` / `TELEGRAM_*` / `DISCORD_*`
> 三組 env var。其他 channel 若要透過 env var 傳入，需自行在 compose 檔案的
> `environment:` 區塊新增；或啟動後改走 **Dashboard → Channels → Add**
> 做熱新增（推薦，無須重啟 gateway，token 也會立即加密寫入 config）。

### 4.3 Bind / Port 相關

| 變數 | 預設 | 說明 |
|------|------|------|
| `DUDUCLAW_BIND` | `0.0.0.0` | Gateway 監聽位址。容器內必須 `0.0.0.0` 外部才打得到 |
| `DUDUCLAW_ALLOWED_ORIGINS` | （空） | Dashboard WebSocket/CORS 的額外允許 `Origin`，逗號分隔。透過 tailnet／反向代理網域開 dashboard 且 WS 被 403 時填這裡（見 [§13 疑難排解](#dashboard-一直轉圈-websocket-403)）。與 config.toml `[gateway] allowed_origins` 合併 |

Port 在 compose 檔內而非 env var，詳見 [§5 Port 設定詳解](#5-port-設定詳解)。

### 4.4 其他

| 變數 | 用途 |
|------|------|
| `TS_AUTHKEY` | Tailscale Funnel 開機自動加入 tailnet（搭配 `--profile tailscale`）|

---

## 5. Port 設定詳解

### 5.1 預設

`docker-compose.yml`：

```yaml
services:
  duduclaw:
    ports:
      - "18789:18789"   # HOST:CONTAINER
    environment:
      - DUDUCLAW_BIND=0.0.0.0
```

- **容器內部**：gateway 永遠綁在 `0.0.0.0:18789`，這是寫死的設計，
  因為容器本身就是隔離網路空間，綁 localhost 會變成外部無法存取。
- **宿主機**：`18789:18789` 把宿主機 18789 轉送到容器 18789。

### 5.2 改宿主機 port

最常見情境是 18789 被佔用或需要多實例。只改左側數字即可：

```yaml
    ports:
      - "28789:18789"   # 外部改走 28789
```

之後 dashboard 與 webhook URL 都要改用新 port：

```bash
curl http://localhost:28789/health
```

> 右側 `18789`（容器內部 port）**不要動**。改它需要同步改
> `~/.duduclaw/config.toml` 的 `[gateway] port`，
> 而 entrypoint 用的是 `--yes` 自動生成，維護成本較高。

### 5.3 只綁本機（避免曝露給 LAN）

預設 `ports: "18789:18789"` 會綁 `0.0.0.0`，區網內其他機器也能連。
若只想本機用（例如搭配反向代理）：

```yaml
    ports:
      - "127.0.0.1:18789:18789"
```

### 5.4 多實例同機

兩份 DuDuClaw 跑在同一台機器：

```bash
# 目錄 A — 正式
cd /srv/duduclaw-prod
# ports: "18789:18789"
docker compose up -d

# 目錄 B — 測試
cd /srv/duduclaw-staging
# ports: "28789:18789"
# container_name 也要改，避免衝突
docker compose up -d
```

記得同步改 `container_name`（預設 `duduclaw-server`）避免名稱碰撞。

---

## 6. CLI 認證設定

image 包含 Claude、Codex 與 Grok，登入方法各有不同。Codex 使用裝置碼，瀏覽器不必連到容器內的 callback listener。建議備妥 API key 作為計費與備援方式。

標準 Compose 檔掛載下列憑證與設定目錄：

| 路徑 | Volume | CLI |
|------|--------|-----|
| `/home/duduclaw/.claude` | `duduclaw-claude` | Claude Code |
| `/home/duduclaw/.codex`  | `duduclaw-codex`  | Codex |
| `/home/duduclaw/.grok` | `duduclaw-grok` | Grok |
| `/home/duduclaw/.kimi-code` | `duduclaw-kimi` | Kimi Code |
| `/home/duduclaw/.copilot` | `duduclaw-copilot` | GitHub Copilot CLI |
| `/home/duduclaw/.kiro` | `duduclaw-kiro` | Kiro CLI |
| `/home/duduclaw/.cursor` | `duduclaw-cursor` | Cursor CLI |
| `/home/duduclaw/.local/share/opencode` | `duduclaw-opencode` | OpenCode |

持久化 volume 可讓 CLI 狀態在容器重建後保留。自訂 Compose 檔也要掛載對應目錄。

新增的五個 volume 只負責保留憑證；這些 CLI 不隨此 image 安裝，須另行安裝後 Dashboard 才能啟動登入。Cursor 的瀏覽器登入仍需要 CLI 可收到本機 callback；遠端主機若無法接收，請改用 API key。

> **訂閱登入有帳號風險。** Anthropic 的[法律與合規說明](https://code.claude.com/docs/en/legal-and-compliance)寫明：第三方開發者不得在自己的產品裡提供 Claude.ai 登入，也不得用 Free、Pro、Max 方案的憑證替使用者轉送請求，更不得收集、儲存或中介 Claude.ai 的憑證或 session token；Anthropic 可以不事先通知就執行，也有使用者回報帳號被停權。讓一份訂閱回覆頻道上的客戶、輪替多個訂閱帳號來避開用量上限、把 token 存在 DuDuClaw，都和這份規範衝突。下面的 setup token、容器內互動登入與 ChatGPT 登入都屬於訂閱登入，功能照常可用，但建議使用 API key；dashboard 的登入流程會先要求你勾選同意這段說明。詳見 [features/13-multi-runtime.md](../../features/zh-TW/13-multi-runtime.md)。

### 6.1 Claude Code CLI

#### 方法 A：Setup Token（不需瀏覽器）

Claude Code 專為自動化場景提供一個長效 token，不需要瀏覽器回呼，
適合容器 / CI / headless server：

```bash
# 在你「有瀏覽器」的本機電腦上執行（不是容器內）
npm install -g @anthropic-ai/claude-code
claude setup-token
# 依提示完成瀏覽器 OAuth
# 最後會顯示 CLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat01-...
```

把產生的 token 寫入 `.env`：

```bash
# .env
CLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat01-xxxxxxxx...
```

然後：

```bash
docker compose up -d
```

DuDuClaw 啟動時會讀取環境變數，`claude` CLI 認得這個 token
就會自動登入，不需要進入容器做任何事。Token 有效期 30 天，
過期前 7 天 DuDuClaw 會在 dashboard 顯示警示。

#### 方法 B：容器內互動登入

若你有 Pro / Team / Max 訂閱且想用訂閱 quota：

```bash
# 進入容器
docker compose exec duduclaw bash

# 在容器內執行
claude auth login
# 會顯示一個 URL，在本機瀏覽器打開並完成授權
# 狀態會寫入 /home/duduclaw/.claude/ ── 即 duduclaw-claude volume

# 驗證
claude auth status
exit
```

容器重啟後狀態仍在（因為 volume）。

#### 方法 C：API Key Fallback

`.env`：

```bash
ANTHROPIC_API_KEY=sk-ant-...
```

這條路徑會**按量計費**，適合當訂閱用盡或帳號被 rate-limit 時的保險。
DuDuClaw 的 AccountRotator 會自動在多個帳號間輪替，詳見
[features/07-account-rotation.md](../../features/zh-TW/07-account-rotation.md)。

### 6.2 Codex（OpenAI）CLI

#### 方法 A：ChatGPT Plus/Pro OAuth

```bash
docker compose exec duduclaw bash
codex login --device-auth
# 在瀏覽器開啟 Codex 顯示的驗證網址，輸入裝置碼。
# 等待 CLI 顯示完成；不需將 callback URL 貼回 Dashboard。
# 狀態寫入 /home/duduclaw/.codex/ ── 即 duduclaw-codex volume
exit
```

此方法使用 ChatGPT 訂閱 quota，無額外 API 費用。token 會存在容器的 volume 裡。OpenAI 的 [Sign in with ChatGPT 條款](https://openai.com/policies/sign-in-with-chatgpt-terms/)要求長期保存的驗證 token 只能放在使用者自己掌控的本機，不能放在遠端或託管環境，所以部署在伺服器上時請優先用方法 B（見 [features/13-multi-runtime.md](../../features/zh-TW/13-multi-runtime.md)）。

#### 方法 B：API Key Fallback

`.env`：

```bash
OPENAI_API_KEY=sk-proj-...
```

按量計費（OpenAI API 定價）。

### 6.4 Per-Agent 指定 Runtime

每個 agent 可以在自己的 `agent.toml` 指定要用哪個 runtime：

```toml
# ~/.duduclaw/agents/my-agent/agent.toml

[runtime]
provider = "claude"       # 主 runtime：claude / codex / openai-compat …
fallback = "codex"  # Claude 不可用時自動切換
```

完整範例與 failover 策略請見
[features/13-multi-runtime.md](../../features/zh-TW/13-multi-runtime.md)。

沒有設定 `provider` 時，agent 使用 Claude。

### 6.5 驗證已安裝的 CLI

```bash
docker compose exec duduclaw bash

# 已安裝的 CLI 應印出版本
claude --version
codex --version

# Claude 登入狀態
claude auth status
```

---

## 7. 資料持久化（Volumes）

`docker-compose.yml` 宣告了十個 named volume：

| Volume | 內容 | 備份建議 |
|--------|------|---------|
| `duduclaw-data` | 主程式資料：config、agents、memory SQLite、logs、bus_queue.jsonl | **最重要**，建議每日備份 |
| `duduclaw-claude` | Claude CLI OAuth state | 中等，遺失需重新 `claude auth login` |
| `duduclaw-codex`  | Codex OAuth state | 中等 |
| `duduclaw-grok` | Grok CLI 憑證 | 中等 |
| `duduclaw-kimi` | Kimi Code 憑證 | 中等 |
| `duduclaw-copilot` | Copilot CLI 憑證 | 中等 |
| `duduclaw-kiro` | Kiro CLI 憑證 | 中等 |
| `duduclaw-cursor` | Cursor CLI 憑證 | 中等 |
| `duduclaw-opencode` | OpenCode 憑證 | 中等 |

以及掛入的宿主機路徑：

| 路徑 | 用途 | 必要性 |
|------|------|-------|

### 7.1 備份

```bash
# 備份主資料到 tar.gz
docker run --rm \
  -v duduclaw_duduclaw-data:/source:ro \
  -v $(pwd):/backup \
  alpine tar czf /backup/duduclaw-data-$(date +%F).tar.gz -C /source .
```

### 7.2 還原 / 轉移

把備份放到目標機，還原到同名 volume：

```bash
docker volume create duduclaw_duduclaw-data
docker run --rm \
  -v duduclaw_duduclaw-data:/target \
  -v $(pwd):/backup \
  alpine tar xzf /backup/duduclaw-data-2026-04-22.tar.gz -C /target
```

### 7.3 用宿主機目錄取代（進階）

若想方便直接瀏覽資料，可把 named volume 換成 bind mount：

```yaml
    volumes:
      - ./data/duduclaw:/home/duduclaw/.duduclaw
      - ./data/claude:/home/duduclaw/.claude
      - ./data/codex:/home/duduclaw/.codex
      - ./data/gemini:/home/duduclaw/.gemini
      - ./data/grok:/home/duduclaw/.grok
      - ./data/kimi:/home/duduclaw/.kimi-code
      - ./data/copilot:/home/duduclaw/.copilot
      - ./data/kiro:/home/duduclaw/.kiro
      - ./data/cursor:/home/duduclaw/.cursor
      - ./data/opencode:/home/duduclaw/.local/share/opencode
```

注意宿主機目錄的 **owner 必須是 UID 1000**（容器內 `duduclaw` 使用者）：

```bash
mkdir -p data/{duduclaw,claude,codex,gemini,grok,kimi,copilot,kiro,cursor,opencode}
sudo chown -R 1000:1000 data/
```

---

## 8. 健康檢查與觀測

### 8.1 三條 health endpoint

| 路徑 | 用途 | HTTP 200 條件 |
|------|------|---------------|
| `/health` | 完整狀態（JSON） | 永遠回 200 + 結構化內容 |
| `/health/ready` | Readiness probe | agents 已全部載入 |
| `/health/live`  | Liveness probe | process 還活著 |

Compose 預設用 `/health` 做 healthcheck（每 30s，3 次失敗標 unhealthy）。

### 8.2 查看狀態

```bash
# Container 狀態
docker compose ps

# 即時 log
docker compose logs -f duduclaw

# 最近 100 行
docker compose logs --tail=100 duduclaw

# Healthcheck 歷史
docker inspect duduclaw-server --format '{{json .State.Health}}' | jq
```

### 8.3 Prometheus Metrics

Gateway 提供 `GET /metrics` 端點，指標清單見
[docs/deployment-guide.md §9](deployment-guide.md#9-prometheus--grafana-監控)。

---

## 9. Watchtower 自動更新

`docker-compose.yml` 已內建 Watchtower 服務，每小時檢查一次映像更新。

> **注意**：目前 compose 檔的 `WATCHTOWER_SCOPE=duduclaw` 設定
> 只會更新**帶有對應 label 的容器**，但 `duduclaw` 服務本身
> 未設定該 label。若要啟用自動更新，請於 `services.duduclaw` 加上：
>
> ```yaml
>     labels:
>       - "com.centurylinklabs.watchtower.scope=duduclaw"
> ```
>
> 或移除 `WATCHTOWER_SCOPE`（會更新所有容器，範圍較大）。

若不想自動更新，直接移除 watchtower 服務區塊即可。

---

## 10. Channel Webhook 需要公開 HTTPS

LINE / WhatsApp / Feishu 等走 webhook 的通道都需要**可從網際網路存取的 HTTPS URL**
才能收到訊息。若你的 DuDuClaw 跑在家用網路或沒有公網 IP 的伺服器，
常用方案：

| 方案 | 適用情境 | 成本 | 指引 |
|------|---------|------|------|
| Tailscale Funnel | 家用 / 開發 / 臨時 | 免費 | [§11](#11-tailscale-funnel公開-https-給-webhook) |
| Cloudflare Tunnel | 長期生產 | 免費（需自有網域） | [deployment-guide §4](deployment-guide.md#4-cloudflare-tunnel長期穩定) |
| ngrok | 快速 demo | 免費版 URL 會變 | [deployment-guide §3](deployment-guide.md#3-ngrok替代方案) |
| 反向代理（Caddy / Nginx） | 自有公網 IP + 網域 | 需自行維護 | [deployment-guide §5](deployment-guide.md#5-反向代理caddy--nginx) |

---

## 11. Tailscale Funnel（公開 HTTPS 給 webhook）

Compose 已附帶 Tailscale 服務，預設在 `tailscale` profile 下，不自動啟動。

### 11.1 啟用

```bash
# 1. 從 Tailscale admin console 產生 auth key
#    https://login.tailscale.com/admin/settings/keys
#    建議勾選 "Reusable" + "Ephemeral"

# 2. 寫入 .env
echo "TS_AUTHKEY=tskey-auth-xxxxxxx" >> .env

# 3. 啟動 tailscale profile
docker compose --profile tailscale up -d

# 4. 在 Tailscale admin console 對此機器啟用 Funnel
#    Machines → duduclaw → Enable Funnel

# 5. 取得 https URL
docker compose exec tailscale tailscale funnel status
# https://duduclaw.your-tailnet.ts.net/  ← 這就是 webhook 前綴
```

LINE webhook URL 填 `https://duduclaw.your-tailnet.ts.net/webhook/line`。

> Tailscale container 目前使用 `image: tailscale/tailscale:latest`，
> 生產環境建議改 pin 到特定 digest（compose 檔已標註 TODO）。

---

## 12. 常用指令速查

```bash
# 啟動 / 停止 / 重啟
docker compose up -d
docker compose down           # 停止並移除 container（volume 不會刪）
docker compose restart duduclaw

# 重新建置映像（程式碼或 Dockerfile 變更後）
docker compose build --no-cache duduclaw
docker compose up -d

# 進入容器 shell
docker compose exec duduclaw bash

# 跑一次性指令
docker compose exec duduclaw duduclaw agent list
docker compose exec duduclaw duduclaw doctor

# Log
docker compose logs -f duduclaw
docker compose logs --since 1h duduclaw

# 清理
docker compose down -v        # ⚠️ 連 volume 一起刪 ── 資料全失
docker system prune -af       # ⚠️ 清掉所有未使用 image / container
```

---

## 13. 疑難排解

### 容器起來但 `curl localhost:18789/health` 回 Connection refused

**成因**：Gateway 綁在容器內部 localhost，宿主機打不到。
**檢查**：

```bash
docker compose exec duduclaw env | grep DUDUCLAW_BIND
# 必須是 DUDUCLAW_BIND=0.0.0.0
```

若是空字串，把 compose 檔的 `environment:` 區塊補上即可。

### `docker compose up` 卡在 `Compiling duduclaw-gateway`

Rust 編譯很吃 CPU/RAM，**首次**建置在一般筆電上約需 10-20 分鐘。
後續變更會走 incremental cache，通常 1-2 分鐘。

若記憶體不足（< 4 GB）可能會 OOM killed，建議：

- 改用 pre-built image（若有提供）
- 或在宿主機預先 `cargo build --release` 產出二進位，改寫 Dockerfile
  直接 COPY（進階）

### Claude CLI 顯示 "not logged in"

**檢查順序**：

```bash
# 1. Env var 有沒有傳進去
docker compose exec duduclaw env | grep -E "CLAUDE_CODE_OAUTH_TOKEN|ANTHROPIC_API_KEY"

# 2. Volume 有沒有掛對
docker compose exec duduclaw ls -la ~/.claude/

# 3. CLI 本身看得到什麼
docker compose exec duduclaw claude auth status
```

常見錯誤是忘了 `docker compose up -d` 在修改 `.env` 後**重啟** container，
env var 只在啟動時注入，熱改無效。

> **第 3 步不能證明 token 還活著。**只要環境裡有 `CLAUDE_CODE_OAUTH_TOKEN`，
> `claude auth status` 就會回報 `loggedIn: true`——它從來不檢查這個 token
> 是否還能實際認證。一顆已被撤銷或組織停用的 token，一樣會通過這個檢查。
> 要確認憑證是不是真的有效，看儀表板「帳號」頁該帳號卡片上的憑證狀態徽章
> （未驗證／憑證損壞／token 無效／組織停用——健康帳號不顯示任何徽章），
> 或在 container 內直接跑一次真探測：
>
> ```bash
> docker compose exec duduclaw claude -p "reply ok" --model haiku --max-turns 1 --output-format json
> ```
>
> 看回傳 JSON 裡的 `is_error` / `api_error_status` 欄位。

### Dashboard 打得到但 agent 回訊「請先執行 `claude auth status`」

這是 v1.8.x 前版本的殘留訊息；v1.8.22+ 已改成依
`FailureReason` 分類的 zh-TW 訊息。若仍看到：

```bash
docker compose exec duduclaw duduclaw --version
# 確認 1.8.22 以上；若非，拉最新 main + 重 build
```

### Dashboard 一直轉圈 WebSocket 403

症狀：透過 tailnet（`*.ts.net`）或反向代理網域開 dashboard，HTTP 頁面正常載入，
但即時資料一直轉圈圈；瀏覽器 DevTools 的 Network 面板可見 `/ws` 升級回 **403**。

原因：WebSocket 的 `Origin` 白名單預設只含 loopback，你的對外網域不在其中。把它
加進白名單即可：

```bash
# compose 的 environment: 或 .env
DUDUCLAW_ALLOWED_ORIGINS=duduclaw.your-tailnet.ts.net,duduclaw.yourdomain.com
```

或寫進 `~/.duduclaw/config.toml`：

```toml
[gateway]
allowed_origins = ["duduclaw.your-tailnet.ts.net"]
```

重啟後啟動 log 會印出生效的額外 origins。詳見
[deployment-guide §5 WebSocket Origin 白名單](deployment-guide.md#websocket-origin-白名單反向代理--tailnet-必讀)。

### Channel webhook 沒有觸發 agent

- LINE / WhatsApp / Feishu 需要 HTTPS，確認 [§10](#10-channel-webhook-需要公開-https) 已完成設定
- 到 Dashboard → Channels 檢查該 channel 的連線狀態指示燈
- 看 log：`docker compose logs -f duduclaw | grep -i webhook`

### 任務沙箱無法啟動

這只適用於 `agent.toml [container] sandbox_enabled = true` 的員工。官方映像不支援在容器裡的 gateway 使用任務沙箱：映像沒有 Docker client，預設 compose 也不掛載 `docker.sock`。需要任務沙箱時，請把 gateway 裝在宿主機上（見 [task-sandbox.md](task-sandbox.md)）。

### Codex 員工的 shell 指令無法執行

Codex 員工的 shell 在 bubblewrap 沙箱裡執行，需要容器允許 user namespace。沙箱起不來時，員工仍能對話和使用 MCP 工具，但 shell 指令全部失敗，檔案不會被改動。`duduclaw doctor` 與儀表板的檢查頁會列出「Codex 員工 shell」並說明下一步。

先把 compose 檔更新到目前版本，重建容器：

```bash
git pull
docker compose up -d
```

目前的 compose 檔會套用 `container/seccomp-bwrap.json`、`apparmor=unconfined`、`no-new-privileges` 與 `cap_drop: ALL`。Watchtower 只更新映像，不會更新 compose 檔，這一步要手動做。

Ubuntu 24.04 預設限制非特權 user namespace，還要在宿主機載入 AppArmor 設定檔：

```bash
sudo scripts/docker-apparmor-setup.sh
# 把印出的這行加進 .env
# DUDUCLAW_APPARMOR_PROFILE=duduclaw-userns
docker compose up -d
```

| 宿主機 | 要做的事 | 驗證狀態 |
|---|---|---|
| Docker Desktop | 更新 compose | 已測 arm64 |
| Ubuntu 22.04 | 更新 compose | 已測 x64 |
| Ubuntu 24.04 | 更新 compose，再執行上面的腳本 | 已測 x64 |
| 其他 Linux、NAS | 依 `duduclaw doctor` 的結果處理 | 未測 |

在 Docker 中，Claude、Grok 等 Codex 以外的員工不能建立 namespace，也不能使用 `strace`、`gdb`。Docker Desktop 的核心沒有 Yama，doctor 的「容器行程隔離」會顯示警告；單一租戶可接受，多租戶請使用開啟 Yama 的 Linux 主機。

`distribution/nas/docker-compose.yml` 只使用映像，沒有帶 seccomp 檔，所以這份 compose 還沒有這些設定。若更新後 doctor 仍回報相同問題，代表宿主機停用了 user namespace。

### OAuth 狀態在容器重建後消失

確認 volume 確實存在：

```bash
docker volume ls | grep duduclaw
# duduclaw_duduclaw-data
# duduclaw_duduclaw-claude
# duduclaw_duduclaw-codex
# duduclaw_duduclaw-gemini
```

若缺了某個，表示 compose 檔被改過或曾 `docker compose down -v`，
重新 `claude auth login` / `codex login --device-auth` 即可。

---

## 14. 延伸閱讀

- 部署方式總覽：[docs/deployment-guide.md](deployment-guide.md)
- Multi-Runtime 架構：[docs/features/13-multi-runtime.md](../../features/zh-TW/13-multi-runtime.md)
- Account Rotation：[docs/features/07-account-rotation.md](../../features/zh-TW/07-account-rotation.md)
- 開發指南：[docs/development-guide.md](development-guide.md)
