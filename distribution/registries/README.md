# Registry 上架物（官方 MCP Registry＋ACP Agent Registry）

兩張「零審核 metadata 票」的送件產物與 runbook。schema 皆於 2026-08-13 對照官方 repo 現行文件撰寫（MCP：`modelcontextprotocol/registry` docs；ACP：`agentclientprotocol/registry` FORMAT.md／CONTRIBUTING.md／AUTHENTICATION.md）。

## MCP Registry（`mcp/server.json`）

前置（順序不能反）：
1. `npm/duduclaw/package.json` 已加 `"mcpName": "io.github.zhixuli0406/duduclaw"`（✅ 2026-08-13）——**registry 會驗證「已發佈」的 npm 包內含此欄位**，所以要等下一次 `npm publish` 之後才能送 registry。
2. GitHub 帳號驗證 namespace `io.github.zhixuli0406/*`。

送件（👤）：
```bash
brew install mcp-publisher       # 或照官方 quickstart 安裝
cd distribution/registries/mcp
mcp-publisher login github       # 瀏覽器 OAuth
mcp-publisher publish            # 讀當前目錄 server.json
```

每版維護：改 `version`（頂層＋packages[0].version）再 publish；可依官方 `github-actions.mdx` 掛進 CI。
備註：`packageArguments` 的 positional `mcp-server` 對應 `npx duduclaw mcp-server`；送件時如 schema 驗證器另有意見以官方 schema 為準。

## ACP Agent Registry（`acp/duduclaw/`）

送件（👤）：fork `agentclientprotocol/registry` → 複製 `acp/duduclaw/`（agent.json＋icon.svg，16×16 currentColor 已符規格）到 repo 根 `duduclaw/` → PR。

**✅ gap 已解（2026-08-13 同日，WP0.13）**：真正的 Agent Client Protocol server 已實作為獨立指令 **`duduclaw acp`**（ACP v1：initialize／session/new／session/prompt 串流／session/cancel；未設定 home 回 `AUTH_REQUIRED` -32000 並宣告 `duduclaw onboard` 認證方法），協定測試腳本全迴路活測通過。agent.json 的 `args` 已改指 `["acp"]`。（歷史紀錄：`duduclaw acp-server` 是 A2A 協定、與 ACP 撞名，兩者刻意分開在不同指令；功能文件 docs/features/19 三語已同步。）

**⚠ 送件時機**：`duduclaw acp` 指令**首次隨下一個 release 出貨**——本目錄 agent.json 目前的 binary archive/sha256 指向 v1.56.0 資產（不含 `acp` 指令）。送件前先等含此指令的 release 發佈，並照下方「每版維護」把 `version`＋五平台 URL/sha256 換成該版，npx 車道則自動吃 npm latest（同樣需 npm publish 新版後才有效）。

每版維護：agent.json 的 `version`＋binary 五平台 archive URL/sha256 隨 release 更新（sha256 來源＝release 的 `.sha256` 資產）。
