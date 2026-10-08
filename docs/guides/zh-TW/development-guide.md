# DuDuClaw Development Guide

> Agent 開發、瀏覽器自動化調試、及本地環境設定指南。

---

## 1. 快速開始

### 1.1 本地開發環境

```bash
# 啟動伺服器（Gateway + 通道 + heartbeat + Dashboard）
duduclaw run

# 同一個伺服器，略過互動式提問
duduclaw gateway
```

沒有獨立的 `duduclaw dev` 模式。伺服器會：
- 在 `config.toml [gateway]` 的位址（`bind` / `port`，預設 `http://127.0.0.1:18789`）提供 Dashboard
- 即時把日誌串流到 Dashboard
- 不會替任何 Agent 加上瀏覽器 MCP server；`.mcp.json` 裡的瀏覽器項目怎麼來，見 2.4 節

### 1.2 Agent 目錄結構

```
~/.duduclaw/agents/my-bot/
├── agent.toml          # Agent 配置（model、budget、capabilities）
├── SOUL.md             # Agent 人格與行為指引
├── CLAUDE.md            # Claude Code 專案指引（可選）
├── CONTRACT.toml       # 行為合約（只有 [boundaries]）
├── .mcp.json           # 這個 Agent 的 MCP server（DuDuClaw 寫入自己的項目；瀏覽器 server 由你加）
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
詳見 [RFC-24](../../rfc/RFC-24-decision-continuity.md)。

### 1.4 AI Runtime 後端選擇（Multi-Runtime）

每個 Agent 可獨立選擇驅動它的 AI CLI 後端，透過 `AgentRuntime` trait 抽象。
`RuntimeRegistry` 在啟動時自動偵測各 CLI 是否安裝並註冊；`agent.toml` 以
`[runtime] provider` 指定（預設 `claude`），`fallback` 指定後端不可用時的退路。

```toml
[runtime]
provider = "antigravity"   # claude | codex | gemini（已棄用）| antigravity | openai_compat
fallback = "claude"        # 後端偵測不到時改用此後端
```

| Provider | CLI 二進位 | 認證 | 備註 |
|----------|-----------|------|------|
| `claude` | `claude`（永遠可用，核心） | OAuth / API Key 輪替 | 預設後端 |
| `codex` | `codex` | OpenAI | — |
| `gemini` | `gemini` | `GEMINI_API_KEY` / OAuth | **v1.67.0 棄用，v1.72.0 移除**（見[已棄用名稱](deprecations.md#gemini-cli-runtime)）。個人版 OAuth 於 2026-06-18 停用；付費金鑰仍可用 |
| `antigravity` | `agy`（`~/.local/bin/agy`） | Google 登入（在終端機執行 `agy`）/ `GEMINI_API_KEY` | Gemini CLI 的官方後繼者，多模型（Gemini 3.x + Claude + GPT-OSS） |
| `openai_compat` | HTTP（無 CLI） | per-provider key | Exo / llamafile / vLLM 等 OpenAI 相容端點 |

**Antigravity（`agy`）特有注意事項**（詳見
[TODO-antigravity-cli-migration.md](../../todo/TODO-antigravity-cli-migration.md)）：

- Agent 目錄會被自動加入 agy 的 `trustedWorkspaces`，避免 headless 下卡在
  「是否信任此 workspace？」互動提示。
- print 模式無 JSON 輸出 → token 使用量為 CJK-aware 啟發式估算（非精確值）。
- Gateway 呼叫前要先完成認證：在主機的終端機執行 `agy`，照提示完成 Google 登入
  （`agy` 沒有 `login` 子指令）；或改用 API key 模式：在 `config.toml` 設定
  `[antigravity] auth = "api_key"`，並提供 Gemini API key（`gemini` provider 帳號，
  或環境變數 `GEMINI_API_KEY`）。容器或沒有 keyring、瀏覽器的遠端主機請用 API key 模式。

### 1.5 活測環境（Live validation home）

驗證新版時，請在隔離的 home 對真的 gateway 測，不要動 `~/.duduclaw`：

```bash
scripts/live-test/make-home.sh /tmp/ddc-live --port 18977
HOME=/tmp/ddc-live/os-home DUDUCLAW_HOME=/tmp/ddc-live duduclaw run --yes &   # 開機一次，會寫出 .mcp.json
scripts/live-test/mcp-probe.sh /tmp/ddc-live plain
scripts/live-test/mcp-probe.sh /tmp/ddc-live prod-shaped
```

啟動 gateway 時，一律把 `HOME` 指向 `make-home.sh` 在隔離 home 裡建立的 `os-home` 目錄。gateway 與它啟動的 AI CLI 會從 `HOME` 底下找登入與設定；只換 `DUDUCLAW_HOME` 的話，被啟動的 `claude` 會用操作者自己的登入，花掉操作者自己的額度，Antigravity 的 API key 模式還會改寫操作者自己的設定檔。`mcp-probe.sh` 啟動 MCP server 時也用同一個 `os-home`。

這個 home 有兩位員工：`plain`（沒有 allowlist）與 `prod-shaped`（`allowed_tools = ["mcp__duduclaw__*", ...]`，另有 denied、核准清單、明確的權限旗標、預算與契約）。v1.67.0 起「萬用字元 allowlist 讓所有平台工具被拒」的回歸，就是因為活測員工沒有 allowlist 才漏測。規則：升級正式 home 之後，要透過每位員工自己的 MCP 註冊去呼叫真工具（`mcp-probe.sh ~/.duduclaw <agent-id>`）。隔離的 home 不會隔離操作者的 Claude 連接器（Drive、Gmail），請在測試任務裡明寫不要查外部服務。編譯快取塞滿磁碟時，先跑 `scripts/clean-build-cache.sh --dry-run`；它只清本 workspace 自己的產物、保留第三方依賴，且在 `cargo` 或 `rustc` 還在跑時拒絕執行。細節見 `scripts/live-test/README.md`。

---

## 2. 瀏覽器自動化與 Computer Use 調試

### 2.1 架構概覽

沒有路由器。Agent 看到 L1、L2 兩個 MCP 工具與選用的 L3 server，自己決定用哪一個；L5 是容器裡的 session，由 agent 透過 `computer_*` MCP 工具驅動。不會從一層自動升級到下一層（舊的「BrowserRouter」已在 2026-09 刪除，見[瀏覽器自動化](../../features/zh-TW/08-browser-automation.md)）。

```
Agent
  ├── L1: web_fetch_cached   （HTTP GET，SSRF 防護，磁碟快取）
  ├── L2: web_extract        （同一條抓取路徑 + CSS selector）
  ├── L3: 外部 headless 瀏覽器 MCP server（選用，每個 agent 的 .mcp.json）
  └── L5: 帶虛擬顯示器的容器裡的電腦操作 session，
           由 agent 透過八個 computer_* MCP 工具驅動（session 由 gateway 持有）
```

沒有 L4：包住整個任務的容器是任務沙箱（2.5 節），不是瀏覽器層級。能力開關在 `agent.toml [capabilities]`：`computer_use`（預設 `false`，缺檔或格式錯誤一律拒絕；管 `computer_*` 工具）、`browser_via_bash`、`allowed_tools`、`denied_tools`。`denied_tools` 除了以 `--disallowedTools` 傳給 CLI，MCP 分派端也會強制檢查。

### 2.2 L1 — `web_fetch_cached` 調試

透過 agent 操作，或直接請模型呼叫：

```bash
claude -p "Use web_fetch_cached to fetch https://example.com"
```

**驗證重點**（`crates/duduclaw-gateway/src/web_fetch.rs`、`crates/duduclaw-cli/src/mcp/web.rs`）：
- 只接受 `http` 與 `https`，其他 scheme（`file:`、`javascript:`、`data:`）會被擋
- `localhost`、雲端 metadata 主機名，以及 `duduclaw_core::net_addr::is_public_ip` 不視為公開的每個位址都會被擋：IPv4 的 `0.0.0.0/8`、`10.0.0.0/8`、`100.64.0.0/10`、`127.0.0.0/8`、`169.254.0.0/16`、`172.16.0.0/12`、`192.0.0.0/24`、`192.0.2.0/24`、`192.168.0.0/16`、`198.18.0.0/15`、`198.51.100.0/24`、`203.0.113.0/24`、`224.0.0.0/4`、`240.0.0.0/4`；IPv6 在 `2000::/3` 之外的位址，加上 `2001::/32`、`2001:db8::/32` 與 `3fff::/20`；IPv4-mapped、NAT64（`64:ff9b::/96`）與 6to4 位址改以內嵌的 IPv4 位址判斷（試試 `http://[::ffff:127.0.0.1]/`，它必須被拒絕）。同一個分類器也供其他所有出站閘使用（常駐感知、媒體、relay、MCP 匯入、skills RPC、Odoo、wiki 聯邦、電腦操作的位址釘住）
- 同一 URL 第二次請求回傳 `cached: true`（`ttl_seconds` 控制快取時間）
- 速率限制：每個 agent 每分鐘 10 次，與 `web_extract` 共用
- 回傳內容超過 60,000 字元會被截斷

### 2.3 L2 — `web_extract` 調試

```bash
claude -p 'Use web_extract on https://example.com with selector "h1" and format "text"'
```

走同一條抓取路徑，所以上面的 SSRF、快取、速率限制檢查都適用。兩個工具都不執行 JavaScript，單頁應用程式只會拿到空殼。

**支援格式：**
- `text` — 純文字內容
- `html` — 內層 HTML
- `json` — 結構化 JSON（tag、屬性、子節點）

### 2.4 L3 — 外部 headless 瀏覽器 MCP server 調試

DuDuClaw 沒有內建 headless 瀏覽器，也不會替你安裝。Agent 需要渲染 JavaScript 的頁面時，在該 agent 自己的 `.mcp.json` 註冊一個瀏覽器 MCP server（和全域註冊的 DuDuClaw MCP server 分開）。`crates/duduclaw-agent/src/mcp_template.rs` 有 `playwright_mcp_config` / `browserbase_mcp_config` 可以組出這種設定，但 gateway 裡沒有程式碼會自動呼叫它們來安裝，所以請自行寫入。

```bash
# 查看該 agent 目前註冊了什麼
cat ~/.duduclaw/agents/my-bot/.mcp.json
```

**`.mcp.json` 範例（`playwright_mcp_config(true)` 產生的格式）：**
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

**前置需求：**
- 需要 Node.js 與 `npx`。`npx -y` 會在第一次啟動時下載 `@playwright/mcp`，不必全域安裝。
- 需要一個這個 server 能啟動的瀏覽器。要哪一個、怎麼安裝，請看 `@playwright/mcp` 的 README，這裡不重複。
- v1.67.1 之前這個範例寫的是 `@anthropic-ai/mcp-server-playwright`，npm 上沒有這個套件。照舊範例寫的 `.mcp.json` 仍是這個名稱，啟動時會失敗，請改成上面那一行。

這個 server 的工具能不能被該 agent 呼叫，由 `[capabilities] allowed_tools` / `denied_tools` 決定。

### 2.5 容器沙箱調試（任務沙箱）

沒有獨立的「L4 沙箱瀏覽器」層：瀏覽器相關工作走 L1、L2、選用的 L3 MCP server 或 L5（見 2.6）。包住 agent 整個任務的容器是**任務沙箱**（`agent.toml [container] sandbox_enabled = true`）。設定請看[任務沙箱指南](task-sandbox.md)；以下步驟用來查出沙箱任務為什麼失敗。

```bash
# 1. 前置條件：Docker 連得上、image 在本機、哪些員工開了沙箱
duduclaw doctor

# 2. 沙箱寫入的稽核事件
grep -E 'task_sandbox_(unavailable|bypassed|tool_violation)' ~/.duduclaw/security_audit.jsonl | tail

# 3. 還在的容器（沙箱會替容器加標籤，任務結束時移除）
docker ps -a --filter name=dudu-task-
```

- `task_sandbox_unavailable` 帶有原因代碼（`docker_unreachable`、`image_missing`、`network_disabled`、`root_user`、`no_account`、`unsupported_runtime`、`invalid_config`、`unsupported_platform`）。
- `task_sandbox_bypassed` 代表 `when_unavailable = "run_unsandboxed"` 讓任務在不隔離的情況下執行。
- `task_sandbox_tool_violation` 代表 AI 用了檔案與 shell 以外的工具，任務已被中止。

想在 image 裡用類似的限制手動看看（這不會重現任務的憑證、掛載與 supervisor，只能看 image 內容，以及 CLI 在唯讀 root 下能不能啟動）：

```bash
docker run --rm -it --read-only --user "$(id -u):$(id -g)" \
  --cap-drop ALL --security-opt no-new-privileges:true \
  --tmpfs /tmp:rw,exec,nosuid,nodev,size=256m,mode=1777 \
  --memory 4g --pids-limit 128 --cpus 1 \
  --entrypoint /bin/sh <sandbox-image>
```

image 用 `config.toml [container.sandbox] image` 的值（預設 `ghcr.io/zhixuli0406/duduclaw:v<版本>`）。沙箱需要 bridge 網路才連得到模型供應商，所以這個範例保留 Docker 預設網路。PTC 與 `secaudit` 使用的腳本沙箱是另一條路徑，確實使用 `--network=none`。

### 2.6 L5 — Computer Use 調試

一個 session 會透過 `computer_use_orchestrator`（`crates/duduclaw-gateway/src/computer_use_orchestrator.rs`）啟動一個 Docker 容器，並需要 `[capabilities] computer_use = true`。Agent 呼叫八個 `computer_*` MCP 工具。MCP 端（`crates/duduclaw-cli/src/mcp/computer_use_client.rs`）是個薄客戶端：它替每次呼叫簽章，再以 `POST /api/internal/computer-use` 在 loopback 上送給 gateway，body 為 `{op: start | screenshot | action | stop | status, …}`。Gateway 端（`crates/duduclaw-gateway/src/computer_use_sessions/`）驗證呼叫端（`auth.rs`）、重新檢查工具閘與審批清單（`gates.rs`）、驗證每個動作並評估風險（`actions.rs`）、依白名單檢查 `computer_navigate` 的 URL（`navigation.rs`）、從自己記錄的進行中回合找出要做高風險確認的聊天（`turns.rs`），並回收與掃除容器（`mod.rs`、`sweep.rs`）。網路白名單是 `[capabilities.computer_use_config] allowed_domains`，規則見[瀏覽器自動化](../../features/zh-TW/08-browser-automation.md)。想知道某個 agent 現在有沒有 session，可以找它的容器（見下方），或看瀏覽器稽核日誌裡的 `session_start`／`session_end` 列（5.1 節）。

沒有其他進入方式。過去由聊天訊息觸發、由 gateway 自己執行的迴圈（Anthropic `computer_20251124` 工具、把進度貼到通道），以及 `native` 主機桌面模式都已移除；提到電腦操作關鍵字的訊息現在走一般回覆路徑。`[capabilities] computer_use_mode = "native"` 仍可解析，但工具會以 `native_unsupported` 拒絕它；`"auto"` 或沒有這個鍵時，行為等同 `"container"`。

每個 agent 的上限來自 `agent.toml [capabilities.computer_use_config]`：`max_actions`（50）、`max_session_minutes`（10）、`display_width` / `display_height`（1280x800）、`allowed_apps`、`blocked_actions`（預設 `delete_file`、`terminal`、`system_preferences`）、`auto_confirm_trusted`，以及 `allowed_domains`。Session 管理器也會擋下符合 `CONTRACT.toml` `must_not` 規則的動作，規則是從 `[must_not] rules = [...]` 這張表讀的（`computer_use_sessions/mod.rs`），和契約系統其他部分使用的 `[boundaries]` 表不同。CONTRACT.toml 沒有任何 `[browser.*]` 鍵，沒有程式碼會讀。

#### 方式 A：Container（生產環境）

```bash
# 拉取這個 gateway 版本預設使用的映像（由 .github/workflows/computer-use-image.yml
# 從 v1.66.1 之後的第一個 release tag 開始發布）
docker pull ghcr.io/zhixuli0406/duduclaw-computer-use:v<版本>

# 或在 repo 根目錄自行建置，再讓 gateway 改用本機 tag：
#   config.toml  ->  [computer_use]
#                    image = "duduclaw-computer-use:latest"
docker build -f container/Dockerfile.computer-use -t duduclaw-computer-use:latest .

# 手動啟動（和 gateway 一樣不給網路），再啟動隨需的 VNC 伺服器觀看。
# VNC 伺服器只在容器內的 unix socket 監聽；主機上用 socat 把一個本機 port
# 接到 relay（範例用本機 tag；拉取的 ghcr 映像用法相同）。
docker run -d --rm --name duduclaw-cu-debug --network=none \
  -e DISPLAY_SIZE=1280x800 \
  duduclaw-computer-use:latest
printf 'Debug123\n' | docker exec -i duduclaw-cu-debug duduclaw-vnc start viewonly
socat TCP-LISTEN:5900,bind=127.0.0.1,reuseaddr,fork \
  EXEC:'docker exec -i duduclaw-cu-debug duduclaw-vnc-relay'

# 用 VNC 客戶端連線觀看（密碼 Debug123）
# macOS: open vnc://localhost:5900
```

映像（約 1.09 GB）以 `debian:trixie-slim` 為基底，內含 Debian 的 `chromium` 套件、Xvfb、視窗管理器 `openbox`、隨需啟動的 VNC（`x11vnc`，只聽 unix socket，由 `duduclaw-vnc` 啟動）、`xdotool`、`scrot`、網域過濾器、`xdotool getactivewindow` 健康檢查，以及給 `duduclaw-eval-dom` 輔助程式用的 Python 3（見 3.5 節）。Openbox 與 Chromium 以非特權使用者 `sandbox` 執行；entrypoint 保留 root 只是為了設定 iptables。Chromium 以 kiosk 模式從 0,0 開始鋪滿整個虛擬顯示器，裝置縮放比例固定為 1，所以頁面座標等於截圖像素；它的 DevTools port 只在容器內的 127.0.0.1 監聽。瀏覽器若結束（例如 agent 關掉視窗），entrypoint 會重新啟動它。Chromium 從 `container/scripts/chromium-policy.json` 讀取受管政策（頁面不能存取本地網路或 loopback、沒有無痕或訪客視窗、沒有檔案對話框、列印與下載、彈出視窗與裝置權限被封鎖、`file://`／`chrome://`／`devtools://`／`view-source:`／`javascript://` 被封鎖；完整清單見[瀏覽器自動化](../../features/zh-TW/08-browser-automation.md)）。`DeveloperToolsAvailability` 刻意不設：設了它也會停用輔助程式需要的 loopback DevTools 協定。`duduclaw-navigate` 只從 stdin 讀 URL（`printf 'https://example.com/\n' | docker exec -i <container> duduclaw-navigate`），任何參數都是用法錯誤。截圖寫在 `/tmp/duduclaw-root/screen.png`（由 root 擁有、權限 0700、在瀏覽器啟動前建立的目錄），Chromium 的 log 也在那裡。

Session 使用哪個映像：預設是 `ghcr.io/zhixuli0406/duduclaw-computer-use:v<gateway 版本>`，由 `.github/workflows/computer-use-image.yml` 在 git tag `v*`（或手動觸發）時發布。這個 workflow 在原生 runner 上分別建置 `linux/amd64` 與 `linux/arm64`，推送 `:<tag>` 與 `:latest` 之前，先用 gateway 自己的容器參數對每個建置做 smoke test（視窗管理器起來、拍到一張截圖、`duduclaw-eval-dom` 回傳 `[]`）。它從 v1.66.1 之後的第一個 release tag 才開始執行，所以 v1.66.1 以前沒有已發布的映像，`scripts/release.sh verify` 也不會檢查它。唯一的覆寫方式是全域的 `config.toml [computer_use] image = "<ref>"`（可用 digest 參照），沒有逐員工的映像鍵。`[computer_use]` 區段無效時，電腦操作會停用並附上說明，不會退回預設值（`crates/duduclaw-gateway/src/computer_use_image.rs`）。映像永遠不會自動下載：`docker run` 帶 `--pull never`，每個 session 開始前也會先做存在檢查（`docker image inspect`）。只有本機自建 `duduclaw-computer-use:latest`（舊預設值）的機器，必須拉取帶版本號的映像，或設定覆寫鍵。

Orchestrator 自己啟動的容器使用 `--read-only`、256 MB tmpfs `/tmp`、1 CPU、512 MB 記憶體、512 個行程上限（Chromium 的執行緒也算在這個上限內；先前的 100 在一般含 web worker 的頁面就會用完），並且一律 `--network=none`，除非 session 有在啟動時解析成功的白名單主機。每個容器另外都帶 `--security-opt no-new-privileges`。有白名單主機時，gateway 會明確加上 `--network bridge`、每個主機一個 `--add-host <host>:<address>`、把位址以 `ALLOWED_IPS` 傳給網域過濾器，並加上 `--cap-add=NET_ADMIN`，過濾器需要它來安裝預設拒絕的出站規則（只放行連到那些位址的 TCP 443、沒有 DNS、loopback 只限 `127.0.0.1`／`::1`、Docker 解析器 `127.0.0.11` 被拒絕；容器有網路但沒有白名單時，同樣的 loopback 規則也適用）；沒有這個權限時，有網路路由的容器會拒絕啟動。容器名稱以 `duduclaw-cu-` 開頭，可用 `docker ps -a --filter name=duduclaw-cu-` 找殘留。每個容器帶有標籤 `com.duduclaw.computer-use.home`（哪個 DuDuClaw home 擁有它）與 `com.duduclaw.computer-use.deadline`（unix 秒）；gateway 啟動時與之後每 10 分鐘會掃除本 home 已結束、或超過該期限 600 秒以上的容器。

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

進入 Agent 的使用者輸入會經過 `duduclaw-security` 的 `input_guard` 掃描，採**風險評分制**（0-100，有上限）：7 條規則加權累計，達到門檻（預設 60）即封鎖並寫入 `security_audit.jsonl`。`instruction_override`、`role_hijack`、`tool_abuse`、`data_exfiltration` 只要命中一次就直接封鎖，不看總分（`crates/duduclaw-security/src/input_guard.rs`）：

| 規則 | 權重 | 偵測範例 |
|------|------|---------|
| instruction_override | 40 | "ignore previous instructions" |
| role_hijack | 35 | "act as", "your new role" |
| system_prompt_extraction | 30 | "reveal your instructions" |
| tool_abuse | 30 | 誘導濫用工具呼叫 |
| encoding_bypass | 25 | Base64 / 編碼繞過 |
| data_exfiltration | 25 | "send to" + URL |
| termination_manipulation | 30 | "the task is never complete" |

另有 Unicode 正規化（零寬字元、同形字）防繞過；原文含超過 3 個零寬字元時分數再加 20。

> 注意：L1/L2 爬取的網頁內容目前**未經**獨立的內容分類掃描；web_fetch 層的防護為 SSRF 驗證（scheme / 內部 IP / metadata 端點 / DNS rebinding / redirect 逐跳重驗）＋ 5MB 上限 ＋ 速率限制。

### 3.2 Emergency Stop

- 頻道內安全詞：`!STOP` / `!停止`（單一 scope）、`!STOP ALL` / `!全部停止`（全域），`!RESUME` / `!恢復` 復原，由 failsafe 系統處理，需管理員權限
- 這個版本的 Dashboard 沒有可用的 E-Stop 控制：它呼叫的 `browser.emergency_stop` RPC 一律回傳錯誤「Browser automation features require the Pro edition」。停止狀態存在 failsafe manager 的記憶體裡（`crates/duduclaw-security/src/failsafe.rs`），所以重啟 gateway 也會清掉

### 3.3 Tool Approval（HITL ApprovalBroker）

高風險操作走統一的 ApprovalBroker（`approvals.db`，TTL 過期即拒絕、fail-closed）：
- `agent.toml [capabilities] approval_required_tools` 宣告需審批的工具；`irreversible_tools` 一律詢問，`maybe_irreversible_tools` 在模型判官認為該呼叫可能不可逆時詢問（fail-closed）。這些清單是針對發出呼叫的 agent 讀取。在 v1.67.0 之前，agent 透過自己的 MCP server 發出的呼叫，是拿 gateway 的內部金鑰名稱去檢查，所以這三個清單在那條路徑上沒有任何效果。
- 一般工具的請求以 `mcp_call` 類型建立，文字寫成工具呼叫；安裝類工具維持 `mcp_install` 類型。八個 `computer_*` 工具改由 gateway 的電腦操作路由詢問（三個清單中任何一個列到都一律詢問），所以不會有人被問兩次。
- autopilot `require_approval` 動作同樣經過此 broker
- 詳見 observability / capabilities 相關文件

### 3.4 使用者配對（Pairing）

頻道層級的使用者存取控制，設定存於 `channel_settings`（global scope，per channel type）：
- `require_pairing = "true"`：未核准的使用者需先配對才能對話
- `allowed_users` / `blocked_users`：JSON 陣列白名單／黑名單
- 流程：管理員以 MCP tool `pairing_manage`（action=generate）產生 6 位數配對碼（5 分鐘有效）→ 使用者在頻道輸入 `/pair <配對碼>` → 核准並持久化於 `~/.duduclaw/access_control.json`
- 防暴力破解：單碼 5 次失敗鎖定、跨重生累計 15 次上限、常數時間比對、碼以 SHA-256 存放

### 3.5 Screenshot Masking

L5 orchestrator 拍的每一張截圖，在送給模型或存進稽核資料夾之前，都會先經過 `capture_masked_screenshot`（`crates/duduclaw-gateway/src/computer_use_orchestrator.rs`）：

- 它向容器查詢符合三個固定 CSS selector 的元素位置：`input[type=password]`、`.credit-card`、`[data-sensitive]`（`crates/duduclaw-gateway/src/computer_use.rs` 的 `MaskingConfig::default()`），再把這些區塊塗黑。
- 偵測是執行 `docker exec <容器> duduclaw-eval-dom '<js>'`，gateway 端有 10 秒逾時。輔助程式（`container/scripts/duduclaw-eval-dom`，只用 Python 標準函式庫）連到容器內 127.0.0.1 上的 Chromium DevTools port，在唯一可見的頁面裡執行這段運算式，本身另有 5 秒上限。運算式、幾何讀回與 `document.visibilityState` 檢查都在隔離的 world（`Page.createIsolatedWorld`，`container/scripts/duduclaw_cdp.py`）裡執行，所以頁面就算重新定義這些函式，也移不動矩形。多個頁面可見時輔助程式以結束碼 3 離開，其他失敗以 1 離開；gateway 依結束碼（不看文字）對應成 `mask_reason` 的 `several_pages` 或 `helper_failed`。可見頁面不只一個時（例如用 `ctrl+n` 開了視窗，沒有任何瀏覽器政策能防這點），輔助程式會失敗，截圖整張被遮掉，直到下一次 `computer_navigate` 關掉多出來的頁面。它依裝置像素比把矩形從 CSS 像素換算成截圖像素（因此瀏覽器縮放也涵蓋在內），向外取整、四邊各加 1 px、裁到螢幕範圍內，並丟掉沒有可見面積的矩形。在測試頁實測：沒有任何敏感像素外露，每邊最多多蓋 2 px，瀏覽器縮放後同樣如此。
- 以下情況輔助程式會以非零碼結束，gateway 隨即把整張截圖塗黑（fail closed）：瀏覽器沒在執行、沒有可見頁面或同時有一個以上可見頁面、瀏覽器視窗不在 0,0 或沒有鋪滿顯示器、visual viewport 被雙指縮放或捲動、JavaScript 拋出錯誤、超過 5 秒上限。
- 偵測不到的範圍：不屬於頁面的瀏覽器介面（縮放提示泡泡、權限詢問、自動填入下拉選單、alert 對話框）、跨來源 iframe 內的內容，以及 shadow DOM 內的內容。敏感欄位若出現在這些地方，截圖裡仍然看得到。
- DOM 遮罩之後，gateway 會讀取前景視窗標題。標題含有憑證相關字樣（`1password`、`bitwarden`、`lastpass`、`keepass`、`keychain`、`密碼`、`password`、`credential`、`ssh`、`gpg`、`pgp`）時，整張截圖塗黑。標題讀不到（指令錯誤、逾時、非零結束碼、輸出不是 UTF-8）時，整張截圖同樣塗黑（fail closed）。成功讀到但內容為空的標題不會觸發遮罩。

selector 與填色都不能設定：`CONTRACT.toml` 與 `agent.toml` 裡沒有任何遮罩設定鍵會被讀取。

---

## 4. Browser Test Suite

沒有瀏覽器測試指令：`duduclaw test` 只吃一個 Agent 名稱和可選的 `--bank` 檔，對 Agent 的合約與輸入掃描器做紅隊測試（見 [CONTRACT.toml 規格](../../spec/contract-toml-spec.md)），沒有 `--browser` 旗標。瀏覽器相關程式碼由原始碼裡的單元測試涵蓋：

```bash
# L1 / L2 抓取路徑：URL 驗證、SSRF 閘、快取
cargo test -p duduclaw-gateway web_fetch

# L5 截圖遮罩、動作解析
cargo test -p duduclaw-gateway computer_use

# L5 稽核紀錄與截圖儲存
cargo test -p duduclaw-gateway screenshot_audit
```

---

## 5. 審計與監控

### 5.1 瀏覽器與 computer use 的活動記在哪裡

| 紀錄 | 寫入者 | 內容 |
|---|---|---|
| `~/.duduclaw/tool_calls.jsonl` | MCP dispatcher（`crates/duduclaw-cli/src/mcp/dispatch.rs`） | 每次呼叫會改變狀態的工具寫一列，含遮罩過的輸入與結果文字。涵蓋全部八個 `computer_*` 工具，輸入經過精簡：`computer_type` 記成 `chars=<n>`，`computer_navigate` 記成主機與路徑長度（不含路徑、不含查詢字串），`computer_screenshot` 不含圖片。這裡的一列代表一次呼叫；有沒有真的執行要看下一列。`web_fetch_cached`、`web_extract` 屬唯讀，不寫在這裡。 |
| `~/.duduclaw/audit/browser/audit.jsonl` | 電腦操作 session 管理器（`crates/duduclaw-gateway/src/screenshot_audit.rs`） | 帶雜湊鏈欄位 `_prev_hash` 的列，tier 為 `L5a`。Session 會寫 `session_start`、`screenshot`（帶 `fully_masked` 與 `mask_reason`：`several_pages`、`title_sensitive`、`title_unreadable`、`helper_failed`，或 null）、每個執行過的動作一列（`left_click`、`type`、`key`、`scroll` 等）並附風險評級、`navigate`（`url` 為不含查詢字串與 fragment 的 `https://<host><path>`，另有 `domain`）、`action_refused` 與 `session_end`。輸入的文字只記字元數。超過 16 MiB 後輪替為 `audit.jsonl.old`，雜湊鏈延續。L1、L2 不寫這裡。 |
| `~/.duduclaw/audit/browser/screenshots/<agent_id>/<UTC 時間戳>.png` | 同一個管理器 | 每次 `computer_screenshot` 呼叫的遮罩後圖片。 |
| `~/.duduclaw/security_audit.jsonl` | input guard、任務沙箱與其他安全事件 | 被封鎖的輸入與 `task_sandbox_*` 事件（2.5 節）。 |

```bash
# 最近的 L5 動作
tail -20 ~/.duduclaw/audit/browser/audit.jsonl | jq .

# 最近的 computer_* 工具呼叫
grep '"tool_name":"computer_' ~/.duduclaw/tool_calls.jsonl | tail
```

在通道裡輸入 `/replay [n]`（預設 5）會列出這個 Agent 在 `audit/browser/audit.jsonl` 的最後 `n` 列。沒有 `browser_audit_log` 這個 MCP 工具；Dashboard 的 `browser.audit_log` RPC 回傳的也是和 `browser.emergency_stop` 相同的「Pro edition」錯誤。

### 5.2 截圖保留

存下的截圖保留 7 天：電腦操作的清掃（gateway 啟動時，之後每 10 分鐘）會刪掉更舊的檔案。每位員工的資料夾另外最多保留 500 個檔案與 200 MiB；存入新截圖時，超過任一上限就刪掉最舊的。Dashboard 沒有頁面顯示這些截圖。

---

## 6. 常見問題

### Agent 沒有 headless 瀏覽器工具
```bash
# 確認這個 Agent 有登記瀏覽器 server
cat ~/.duduclaw/agents/my-bot/.mcp.json
```
沒有任何機制會自動加上。請自己寫入項目（2.4 節），或在 Dashboard 的 MCP marketplace（`marketplace.install`）替這個 Agent 安裝 `playwright` / `browserbase`。接著確認 `[capabilities] denied_tools` 沒有擋掉它的工具。

### 任務沙箱無法啟動
```bash
# 確認 Docker 運行中
docker info

# 任務沙箱的前置條件（Docker、image、開了沙箱的員工）
duduclaw doctor

# 確認沙箱 image 在本機（不會自動下載）
docker image inspect ghcr.io/zhixuli0406/duduclaw:v<版本>
```
錯誤對照表見[任務沙箱指南](task-sandbox.md)。

### 電腦操作無法啟動
`computer_session_start` 回傳以「電腦操作無法啟動」開頭的錯誤，並寫出原因：
- 映像不在本機：錯誤會寫出映像名稱與兩種處理方式，`docker pull <image>`，或在 repo 根目錄自行建置（`docker build -f container/Dockerfile.computer-use -t duduclaw-computer-use:latest .`）再設定 `config.toml [computer_use] image = "duduclaw-computer-use:latest"`。
- Docker 沒有回應存在檢查：啟動 Docker 後再試。
- `config.toml` 的 `[computer_use]` 無效（未知的鍵、不可用的映像參照、檔案無法解析）：請修正，系統不會退回預設映像。

```bash
# 電腦操作這一列：使用中的映像、是否在本機、哪些 AI 員工設了 computer_use = true
duduclaw doctor
```
沒有任何 AI 員工使用電腦操作時這一列為 Pass；有人使用，且映像不在本機、Docker 無法連線或設定無效時為 Warn。只要有任何 agent 仍設為 `computer_use_mode = "native"`，它也會單獨發出 Warn 並列出名單：請刪掉該鍵或設為 `"container"`。這一列也會逐 agent 列出可用的白名單主機與被忽略的項目。

其他常見的工具回應：
- 工具說無法連到 `127.0.0.1:<port>` 的 gateway：這些工具需要 gateway 在執行中，它們自己不會啟動容器。
- 工具說存取被拒（`unauthorized`）：MCP 行程不是由 gateway 啟動的（環境裡沒有內部金鑰或 agent token）、兩邊時鐘差超過 60 秒，或 `~/.duduclaw/identity.key` 不存在。gateway 只在 debug 層級的 log 寫出確切原因。
- `computer_navigate` 被拒絕：讀訊息內容。沒有 `allowed_domains` 時，訊息會指出要加哪個設定；否則會列出這個 session 能開的主機。session 啟動之後才加進白名單的主機，需要開新的 session。

### Computer-use 容器一啟動就結束
```bash
# 若 session 容器還留著，讀它的啟動紀錄
docker logs <容器>

# 或手動以「開網路＋允許清單」重現啟動過程
docker run --rm -e ALLOWED_DOMAINS=example.com <computer-use 映像>
```
出現 `[domain-filter] FATAL: cannot install the default-deny egress policy` 代表容器有網路路由卻沒有 `NET_ADMIN` 權限，過濾器拒絕在出站未過濾的狀態下執行。Gateway 只有在帶 `ALLOWED_IPS`（有解析成功白名單主機的工具 session），或帶非空的 `ALLOWED_DOMAINS` 時才會加上這個權限；兩者同時設定會被過濾器拒絕。手動啟動且帶網路的容器需要 `--cap-add=NET_ADMIN`；用 `--network=none` 時不需要這個權限也能啟動。

### Computer-use 截圖整張全黑
遮罩輔助程式失敗，所以整張截圖被遮掉（見 3.5 節）。對該 session 的容器手動執行一次：
```bash
docker exec <容器> duduclaw-eval-dom 'JSON.stringify([])'
```
正常時印出 `[]`。否則 stderr 會寫出原因（瀏覽器沒在執行、沒有可見頁面、多個可見頁面、視窗不在 0,0、visual viewport 被縮放、逾時）。沒有這支程式的舊映像會回報 "executable file not found"：請拉取目前的映像或重建（見 2.6 節）。

### Emergency Stop 無法恢復
用管理員帳號在通道裡送出 `!RESUME`（或 `!恢復`）。停止狀態存在記憶體裡，所以重啟 gateway 也會清掉。沒有可刪除的信號檔，也沒有 `emergency_stop` 這個 MCP 工具。
