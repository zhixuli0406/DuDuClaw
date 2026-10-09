# Registry 上架物（官方 MCP Registry＋ACP Agent Registry）

兩張「零審核 metadata 票」的送件產物與 runbook。schema 皆於 2026-08-13 對照官方 repo 現行文件撰寫（MCP：`modelcontextprotocol/registry` docs；ACP：`agentclientprotocol/registry` FORMAT.md／CONTRIBUTING.md／AUTHENTICATION.md）。

## MCP Registry（`mcp/server.json`）

上架的是**獨立模式**（standalone profile）：只裝 npm 套件、不跑 gateway，就能在 Claude Code／Codex／Cursor 用記憶與 wiki 工具。使用者端的說明與工具清單在 [`docs/guides/mcp-standalone.md`](../../docs/guides/mcp-standalone.md)。schema 於 2026-10-07 對照官方文件（quickstart、package-types）與 `2025-12-11` JSON schema 重新驗證過。

`server.json` 重點：
- `packages[0]`：npm 套件 `duduclaw`，stdio，positional 參數 `mcp-server`（等於 `npx duduclaw mcp-server`）。
- `environmentVariables`：`DUDUCLAW_MCP_API_KEY`，必填、secret，說明指向 `duduclaw mcp init`。
- `description` 上限 100 字元（schema 硬規則；2026-08 版超過上限，送件會被擋）。
- 兩個 `version` 由 `scripts/release.sh` 跟著其他平台一起改（`mcp_registry` 類別），`release.sh audit` 與改版後的檢查兩個欄位都會讀，兩者不一致或任一缺漏都算落差。

前置（順序不能反）：
1. `npm/duduclaw/package.json` 有 `"mcpName": "io.github.zhixuli0406/duduclaw"`（2026-08-13 起都有）。**registry 驗證的是「已發佈」到 npm 的那一版**，所以 `server.json` 的版本必須是 npm 上已經有的版本。
2. 用 GitHub 帳號 `zhixuli0406` 登入，namespace `io.github.zhixuli0406/*` 才會通過。
3. **第一次送件要等到含有 `duduclaw mcp init` 的那一版發佈到 npm 之後。** `server.json` 的說明叫使用者跑 `npx duduclaw mcp init`，1.70.1 以前的版本沒有這個指令；目前檔案裡的 `1.70.1` 只是讓 `release.sh` 有東西可以改。送件前用 `npx -y duduclaw@$(jq -r .version distribution/registries/mcp/server.json) mcp init --help` 確認那一版有這個指令。
4. PyPI 套件不上架：它是 Python SDK，不含 `duduclaw` 執行檔也沒有 `mcp-server` 入口，所以不需要 `mcp-name:` 那一行。

送件（👤，每次 release 之後）：
```bash
# 安裝（擇一）
brew install mcp-publisher
curl -L "https://github.com/modelcontextprotocol/registry/releases/latest/download/mcp-publisher_$(uname -s | tr '[:upper:]' '[:lower:]')_$(uname -m | sed 's/x86_64/amd64/;s/aarch64/arm64/').tar.gz" | tar xz mcp-publisher && sudo mv mcp-publisher /usr/local/bin/

# 確認 npm 已有這一版且帶 mcpName
npm view duduclaw@$(jq -r .version distribution/registries/mcp/server.json) mcpName

cd distribution/registries/mcp
mcp-publisher login github       # 裝置碼登入
mcp-publisher publish            # 讀當前目錄 server.json

# 驗證
curl "https://registry.modelcontextprotocol.io/v0.1/servers?search=io.github.zhixuli0406/duduclaw"
```

## Glama（`glama.json`＋`distribution/glama/`）

已收錄：https://glama.ai/mcp/servers/zhixuli0406/DuDuClaw 。Glama 會在沙箱裡建置並啟動 server，再呼叫 `tools/list` 打分數（Server Coherence、TDQS）。

- repo 根目錄的 `glama.json`（schema `https://glama.ai/mcp/schemas/server.json`，必填欄位 `maintainers`）另外宣告 `build.dockerfile`（repo 根的 `Dockerfile`）與 `command`。後台若依 `command` 重生 Admin Dockerfile 的 `CMD`，必須跟根目錄 `Dockerfile` 的 `CMD` 同一條。
- 根目錄 `Dockerfile` 是 Glama Admin 建置用的：安裝 `mcp-proxy@6.7.16`，前面掛獨立 MCP server。`mcp-proxy` 預設只聽 IPv6 `::` 的 8080，也不讀平台注入的 `PORT`。`CMD` 改成 shell，綁 `0.0.0.0`，連接埠依序用 `MCP_PROXY_PORT`、`PORT`、8080。
- `distribution/glama/Dockerfile` 從 npm 安裝最新的 `duduclaw`，只跑獨立模式的 MCP server（stdio），不含 gateway，也不加 `mcp-proxy`。建置要以 repo 根目錄為 context：`docker build -f distribution/glama/Dockerfile .`。
- `distribution/glama/entrypoint.sh`：沒有 `DUDUCLAW_MCP_API_KEY` 時先在容器自己的資料目錄跑 `duduclaw mcp init --client print` 發一把金鑰（只有記憶與 wiki 範圍），再啟動 `duduclaw mcp-server`。預設目錄（`DUDUCLAW_HOME`，否則 `$HOME/.duduclaw`）寫不進去時改用 `/tmp/duduclaw-home`，失敗時印出抹掉金鑰後的錯誤。2026-10-08 在本機 Docker 實測列出 24 個工具。
- Glama 後台（👤，Admin → Dockerfile）若沒有自動用根目錄這個檔，把內容貼進去；建置成功後在 Releases 建一個 release，分數項目才會開始評。

送件前若改過 `server.json`，可以先用官方 schema 驗證（例如 `python -m jsonschema` 或任何 JSON Schema 驗證器，schema 網址見檔案的 `$schema`）。Glama 與 awesome-mcp-servers 不讀這個檔案，收錄流程依各自網站（本 repo 未驗證）。

## ACP Agent Registry（`acp/duduclaw/`）

送件（👤）：fork `agentclientprotocol/registry` → 複製 `acp/duduclaw/`（agent.json＋icon.svg，16×16 currentColor 已符規格）到 repo 根 `duduclaw/` → PR。

**✅ gap 已解（2026-08-13 同日，WP0.13）**：真正的 Agent Client Protocol server 已實作為獨立指令 **`duduclaw acp`**（ACP v1：initialize／session/new／session/prompt 串流／session/cancel；未設定 home 回 `AUTH_REQUIRED` -32000 並宣告 `duduclaw onboard` 認證方法），協定測試腳本全迴路活測通過。agent.json 的 `args` 已改指 `["acp"]`。（歷史紀錄：`duduclaw acp-server` 是 A2A 協定、與 ACP 撞名，兩者刻意分開在不同指令；功能文件 docs/features/19 三語已同步。）

**⚠ 送件時機**：`duduclaw acp` 指令**首次隨下一個 release 出貨**——本目錄 agent.json 目前的 binary archive/sha256 指向 v1.56.0 資產（不含 `acp` 指令）。送件前先等含此指令的 release 發佈，並照下方「每版維護」把 `version`＋五平台 URL/sha256 換成該版，npx 車道則自動吃 npm latest（同樣需 npm publish 新版後才有效）。

每版維護：agent.json 的 `version`＋binary 五平台 archive URL/sha256 隨 release 更新（sha256 來源＝release 的 `.sha256` 資產）。
