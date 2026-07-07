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

### 1.3 決策連續性 (Decision Continuity, RFC-24)

當 Agent 向使用者提出列舉式選項（「方案 A/B/C」、「Option 1/2」）後，若使用者
稍後（甚至跨 session / 重啟 / 壓縮後）回覆「用方案 C」，預設情況下選項內容可能已
隨對話壓縮而遺失。啟用後，系統會在訊息送出時自動把每個選項存進獨立於對話記憶的
語意記憶層，並在後續回合注入「待決事項」讓 Agent 正確解析。

`agent.toml` 啟用（預設關閉，per-agent opt-in）：

```toml
[memory]
decision_continuity = true
```

偵測為確定性、零 LLM 成本，且偏保守（寧漏勿錯）；背景擷取失敗不影響回覆送出。
詳見 [RFC-24](../rfc/RFC-24-decision-continuity.md)。

### 1.4 AI Runtime 後端選擇（Multi-Runtime）

每個 Agent 可獨立選擇驅動它的 AI CLI 後端，透過 `AgentRuntime` trait 抽象。
`RuntimeRegistry` 在啟動時自動偵測各 CLI 是否安裝並註冊；`agent.toml` 以
`[runtime] provider` 指定（預設 `claude`），`fallback` 指定後端不可用時的退路。

```toml
[runtime]
provider = "antigravity"   # claude | codex | gemini | antigravity | openai_compat
fallback = "claude"        # 後端偵測不到時改用此後端
```

| Provider | CLI 二進位 | 認證 | 備註 |
|----------|-----------|------|------|
| `claude` | `claude`（永遠可用，核心） | OAuth / API Key 輪替 | 預設後端 |
| `codex` | `codex` | OpenAI | — |
| `gemini` | `gemini` | `GEMINI_API_KEY` / OAuth | 個人版 OAuth 於 2026-06-18 停用；付費金鑰仍可用 |
| `antigravity` | `agy`（`~/.local/bin/agy`） | `ANTIGRAVITY_API_KEY` / OAuth | Gemini CLI 的官方後繼者，多模型（Gemini 3.x + Claude + GPT-OSS） |
| `openai_compat` | HTTP（無 CLI） | per-provider key | Exo / llamafile / vLLM 等 OpenAI 相容端點 |

**Antigravity（`agy`）特有注意事項**（詳見
[TODO-antigravity-cli-migration.md](../todo/TODO-antigravity-cli-migration.md)）：

- Agent 目錄會被自動加入 agy 的 `trustedWorkspaces`，避免 headless 下卡在
  「是否信任此 workspace？」互動提示。
- print 模式無 JSON 輸出 → token 使用量為 CJK-aware 啟發式估算（非精確值）。
- 需先在裝有 `agy` 的機器上完成一次互動式登入（OAuth）或設定
  `ANTIGRAVITY_API_KEY`，Gateway 才能成功呼叫。

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
claude -p 'Use web_extract on https://example.com with selector "h1" and format "text"'
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

### 3.1 Input Guard（注入掃描）

進入 Agent 的使用者輸入會經過 `duduclaw-security` 的 `input_guard` 掃描——採**風險評分制**（0-100），6 條規則加權累計，超過門檻即封鎖並寫入 `security_audit.jsonl`：

| 規則 | 權重 | 偵測範例 |
|------|------|---------|
| instruction_override | 40 | "ignore previous instructions" |
| role_hijack | 35 | "act as", "your new role" |
| system_prompt_extraction | 30 | "reveal your instructions" |
| tool_abuse | 30 | 誘導濫用工具呼叫 |
| encoding_bypass | 25 | Base64 / 編碼繞過 |
| data_exfiltration | 25 | "send to" + URL |

另有 Unicode 正規化（零寬字元、同形字）防繞過。

> 注意：L1/L2 爬取的網頁內容目前**未經**獨立的內容分類掃描；web_fetch 層的防護為 SSRF 驗證（scheme / 內部 IP / metadata 端點 / DNS rebinding / redirect 逐跳重驗）＋ 5MB 上限 ＋ 速率限制。

### 3.2 Emergency Stop

- 頻道內安全詞：`!STOP` / `!停止`（單一 scope）、`!STOP ALL` / `!全部停止`（全域），`!RESUME` / `!恢復` 復原——由 failsafe 系統處理，需管理員權限
- Dashboard Header 提供一鍵 E-Stop / Resume

### 3.3 Tool Approval（HITL ApprovalBroker）

高風險操作走統一的 ApprovalBroker（`approvals.db`，TTL 過期即拒絕、fail-closed）：
- `agent.toml [capabilities] approval_required_tools` 宣告需審批的工具
- autopilot `require_approval` 動作同樣經過此 broker
- 詳見 observability / capabilities 相關文件

### 3.4 使用者配對（Pairing）

頻道層級的使用者存取控制，設定存於 `channel_settings`（global scope，per channel type）：
- `require_pairing = "true"`：未核准的使用者需先配對才能對話
- `allowed_users` / `blocked_users`：JSON 陣列白名單／黑名單
- 流程：管理員以 MCP tool `pairing_manage`（action=generate）產生 6 位數配對碼（5 分鐘有效）→ 使用者在頻道輸入 `/pair <配對碼>` → 核准並持久化於 `~/.duduclaw/access_control.json`
- 防暴力破解：單碼 5 次失敗鎖定、跨重生累計 15 次上限、常數時間比對、碼以 SHA-256 存放

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
