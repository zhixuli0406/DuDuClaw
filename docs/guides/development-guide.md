# DuDuClaw Development Guide

> Agent 開發、瀏覽器自動化調試、及本地環境設定指南。

---

## 1. 快速開始

### 1.1 本地開發環境

```bash
# 啟動開發模式（自動配置 Playwright MCP）
duduclaw dev --port 18789

# 或指定特定 Agent
duduclaw dev --agent my-bot --port 18789
```

開發模式會：
- 啟動 Gateway + Dashboard（`http://localhost:18789`）
- 自動為啟用 `browser_via_bash` 的 Agent 生成 `.mcp.json`（Playwright MCP）
- 即時日誌串流到 Dashboard

### 1.2 Agent 目錄結構

```
~/.duduclaw/agents/my-bot/
├── agent.toml          # Agent 配置（model、budget、capabilities）
├── SOUL.md             # Agent 人格與行為指引
├── CLAUDE.md           # Claude Code 專案指引（可選）
├── CONTRACT.toml       # 行為合約（boundaries、browser restrictions）
├── .mcp.json           # MCP server 設定（自動生成）
├── .claude/            # Claude Code 設定目錄
└── SKILLS/             # Agent 技能目錄
```

---

## 2. 瀏覽器自動化調試（L1-L5）

### 2.1 架構概覽

```
Agent Request → BrowserRouter (<1ms)
  ├── L1: API Fetch        (reqwest — 零成本)
  ├── L2: Static Scrape    (CSS selector — 零成本)
  ├── L3: Headless Browser (Playwright MCP — 低成本)
  ├── L4: Sandbox Browser  (Container + Playwright — 中成本)
  └── L5: Computer Use     (Virtual display + Claude vision — 高成本)
```

核心原則：**能用 API 就不開瀏覽器，能無頭就不用 computer_use。**

### 2.2 L1 — API Fetch 調試

```bash
# 執行 SSRF 防禦測試
duduclaw test --browser

# 手動測試 MCP tool
claude -p "Use web_fetch_cached to fetch https://example.com"
```

**驗證項目：**
- `file://`、`javascript:`、`data:` scheme 被封鎖
- 內部 IP（127.0.0.0/8、10.0.0.0/8、172.16.0.0/12、192.168.0.0/16）被封鎖
- IPv6 loopback `[::1]` 被封鎖
- 快取命中（同一 URL 第二次請求應返回 `cached: true`）
- 速率限制（每 Agent 每分鐘 10 次）

### 2.3 L2 — CSS Extraction 調試

```bash
# 測試 CSS 選擇器萃取
claude -p 'Use web_extract on https://example.com with selectors [{"name":"title","selector":"h1","format":"text"}]'
```

**支援格式：**
- `text` — 純文字內容
- `html` — 內部 HTML
- `json` — 結構化 JSON（含 tag、attributes、children）

### 2.4 L3 — Playwright MCP 調試

```bash
# 確認 Playwright MCP 已設定
cat ~/.duduclaw/agents/my-bot/.mcp.json

# 手動生成（如果不存在）
# 在 agent.toml 設定 browser_via_bash = true，然後重啟 Gateway
```

**`.mcp.json` 範例：**
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

**前置條件：**
- `npm install -g @anthropic-ai/mcp-server-playwright`
- Playwright Chromium：`npx playwright install chromium`

### 2.5 L4 — Container Sandbox 調試

```bash
# 建構 sandbox 映像
docker build -f container/Dockerfile.browser-sandbox -t duduclaw/browser-sandbox .

# 手動啟動（測試用）
docker run --rm --read-only --tmpfs /tmp:size=256m \
  -e ALLOWED_DOMAINS="example.com,httpbin.org" \
  duduclaw/browser-sandbox

# 無域名 = 無網路
docker run --rm --read-only --tmpfs /tmp:size=256m --network=none \
  duduclaw/browser-sandbox
```

**`CONTRACT.toml` 範例：**
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

### 2.6 L5 — Computer Use 調試

#### 方式 A：Container（生產環境）

```bash
# 建構 computer-use 映像
docker build -f container/Dockerfile.computer-use -t duduclaw/computer-use .

# 啟動（含 VNC）
docker run --rm -p 5900:5900 \
  -e DISPLAY_SIZE=1280x800 \
  -e VNC_ENABLED=true \
  -e VNC_PASSWORD=debug123 \
  duduclaw/computer-use

# 用 VNC client 連線觀察
# macOS: open vnc://localhost:5900
```

**`CONTRACT.toml` L5 設定：**
```toml
[browser.computer_use]
enabled = true
max_actions = 50
container_required = true
display_size = "1280x800"
blur_patterns = ["input[type=password]", ".credit-card", "[data-sensitive]"]
```

#### 方式 B：Claude Code Computer Use MCP（僅限本地調試）

> **限制**：macOS only、Pro/Max 方案、互動式會話、機器級鎖

**前置條件：**
- macOS
- Claude Code v2.1.85 或更新版本
- Claude Pro 或 Max 訂閱方案

**啟用步驟：**

1. 在 Claude Code 中執行 `/mcp`
2. 找到 `computer-use` server，選擇 **Enable**
3. 首次使用時，macOS 會要求授權：
   - **無障礙存取**（System Settings → Privacy & Security → Accessibility）
   - **螢幕錄製**（System Settings → Privacy & Security → Screen Recording）

**使用方式：**
```bash
# 在 Claude Code 互動式會話中
claude

# Claude 會自動使用 computer-use 工具操作桌面
> 請打開 Safari 並瀏覽 example.com
```

**注意事項：**
- 不可用於生產環境（僅限開發調試）
- 不支援 `-p` 非互動模式
- 機器級鎖 — 同一時間只能一個 Claude Code 會話使用
- Token 消耗極高（每次操作都需完整螢幕截圖）
- 座標精度有限（視覺幻覺風險）
- 零設定成本（內建於 Claude Code）
- 可操作任何 macOS 應用程式（非僅瀏覽器）

---

## 3. 安全機制

### 3.1 Input Guard（內容注入掃描）

所有透過 L1/L2 爬取的網頁內容都會經過 `input_guard.rs` 掃描，偵測 6 類威脅：

| 類別 | 偵測範例 |
|------|---------|
| Direct Injection | "ignore previous instructions", "system prompt:" |
| Role Manipulation | "act as", "pretend to be", "your new role" |
| Data Exfiltration | "send to" + URL, "POST to" + IP |
| Prompt Leaking | "show me your prompt", "reveal your instructions" |
| Encoded Payload | Base64 > 50 chars, percent-encoded suspicious patterns |
| Delimiter Injection | `</system>`, `</instructions>`, `[INST]`, `<<SYS>>` |

### 3.2 Emergency Stop

Dashboard Header 提供一鍵緊急停止按鈕：
- **E-Stop**：立即中止所有 Agent 操作
- **Resume**：恢復正常運作
- MCP tool `emergency_stop` 也可透過 CLI 觸發

### 3.3 Tool Approval

高風險工具（browser、computer_use）需要明確授權：
- Dashboard Security 頁面管理授權
- 支援 session-scoped（會話結束自動撤銷）
- 支援時間限制（指定分鐘數後過期）

### 3.4 Screenshot Masking

L5 Computer Use 自動偵測並遮罩敏感區域：
- `input[type=password]` — 密碼欄位
- `.credit-card` — 信用卡表單
- `[data-sensitive]` — 自定義敏感區域

遮罩規則在 `CONTRACT.toml [browser.computer_use] blur_patterns` 中定義。

---

## 4. Browser Test Suite

```bash
# 執行完整瀏覽器測試
duduclaw test --browser

# 測試項目：
# [L1] SSRF prevention (4 URLs)
# [L1] HTTP fetch (httpbin.org)
# [L2] CSS extraction
# [Guard] Content injection scanner
```

---

## 5. 審計與監控

### 5.1 Browser Audit Log

所有瀏覽器操作記錄在 `~/.duduclaw/audit/browser/audit.jsonl`：

```bash
# 查看最近操作
tail -20 ~/.duduclaw/audit/browser/audit.jsonl | jq .

# 透過 MCP tool 查詢
claude -p "Use browser_audit_log to show last 10 entries"
```

### 5.2 Screenshot 審計

截圖儲存在 `~/.duduclaw/audit/browser/screenshots/{agent_id}/`：
- 格式：`{timestamp}.png`
- 預設保留 7 天
- 透過 Dashboard Security 頁面瀏覽

---

## 6. 常見問題

### Playwright MCP 連線失敗
```bash
# 確認已安裝
npx @anthropic-ai/mcp-server-playwright --version

# 確認 .mcp.json 正確
cat ~/.duduclaw/agents/my-bot/.mcp.json
```

### Docker sandbox 無法啟動
```bash
# 確認 Docker 運行中
docker info

# 確認映像已建構
docker images | grep duduclaw

# 手動測試
docker run --rm duduclaw/browser-sandbox echo "OK"
```

### Emergency Stop 無法恢復
```bash
# 手動清除信號檔
rm ~/.duduclaw/emergency_stop

# 或透過 MCP tool
claude -p 'Use emergency_stop with action "resume"'
```
