# 專業軟體整合：Photoshop 與 AutoCAD

本指南說明如何讓 DuDuClaw 的 AI 員工操作 Adobe Photoshop 與 AutoCAD。做法是掛載社群維護的 MCP server，透過 per-agent `.mcp.json` 接上，再用 capability 治理圍住高風險工具，DuDuClaw 本身不自寫這些軟體的驅動。這樣新增一個專業軟體不必等 DuDuClaw 出新版，換一個 MCP server 就換一套能力。

> 這兩個 MCP server 都是第三方、非官方專案（皆 MIT 授權）。它們暴露的任意腳本執行介面（Photoshop 的 ExtendScript、AutoCAD 的 AutoLISP）本質上就是遠端程式碼執行（RCE）面。請務必先讀本頁的「風險聲明」與「capability 治理」兩節，不要裸裝裸用。

## 支援矩陣

| 軟體 | MCP server | 平台 | 連線方式 | 需求 | RCE 面 |
|------|-----------|------|----------|------|--------|
| Photoshop | `@alisaitteke/photoshop-mcp`（npm） | macOS + Windows | macOS AppleScript / Windows COM，統一經 ExtendScript API | 已安裝 Photoshop（2012–2025+）、Node.js（`npx`） | `photoshop_execute_script`（任意 ExtendScript） |
| AutoCAD（File IPC） | `puran-water/autocad-mcp`（Python） | **Windows-only** | temp 檔 + `PostMessageW` 注入 + AutoLISP dispatcher | Windows 10/11、AutoCAD LT 2024+、Python（`uv`） | `system` 工具內的 `execute_lisp`（任意 AutoLISP） |
| AutoCAD（ezdxf） | 同上，`AUTOCAD_MCP_BACKEND=ezdxf` | **跨平台**（Win/mac/Linux/WSL） | headless 直接讀寫 DXF，不觸 AutoCAD 進程 | Python（`uv`），**無需 AutoCAD** | **無**（不執行 AutoLISP） |

重點：跨平台、批次、無人值守、或來源不確定的 CAD 工作，一律走 **ezdxf** 後端。它不啟動 AutoCAD、不跑 AutoLISP，把 RCE 面降為零。File IPC 只在「有人在旁、需要真 AutoCAD 幾何引擎」時使用。

## 安裝步驟

### 1. 裝好目標軟體與 MCP runtime

- **Photoshop**：本機安裝 Adobe Photoshop 並可啟動。核心功能不需 UXP plugin；只有 neural filters（磨皮、上色）需選配 UXP 橋接。另需 Node.js（提供 `npx`）。
- **AutoCAD**：clone `puran-water/autocad-mcp`，`uv sync` 安裝依賴。clone 後核對 commit hash，`uv.lock` 會鎖依賴版本。File IPC 後端另需 Windows + AutoCAD LT 2024+，並依上游說明載入其 `mcp_dispatch.lsp`。

### 2. 在 agent 的 `.mcp.json` 掛上 server

MCP server 掛在**單一 agent** 的 `.mcp.json`（`~/.duduclaw/agents/<id>/.mcp.json`），而非全域。這份檔案安裝 agent 時已預寫一個 `duduclaw` server（agent 存取 DuDuClaw 自身 MCP 工具的必要項）。掛新 server 時要**併入** `mcpServers`，保留既有 `duduclaw` entry，不要整檔覆寫。

Photoshop（把遙測關掉，見風險聲明）：

```json
{
  "mcpServers": {
    "duduclaw": { "command": "…", "args": ["mcp-server"], "env": { "DUDUCLAW_AGENT_ID": "<id>" } },
    "photoshop": {
      "command": "npx",
      "args": ["-y", "@alisaitteke/photoshop-mcp"],
      "env": { "LOG_LEVEL": "2", "ANALYTICS_DISABLED": "1", "POSTHOG_DISABLED": "1" }
    }
  }
}
```

AutoCAD（`command` 要改成本機 venv 的絕對路徑）：

```json
{
  "mcpServers": {
    "duduclaw": { "command": "…", "args": ["mcp-server"], "env": { "DUDUCLAW_AGENT_ID": "<id>" } },
    "autocad-mcp": {
      "command": "C:\\path\\to\\autocad-mcp\\.venv\\Scripts\\python.exe",
      "args": ["-m", "autocad_mcp"],
      "env": { "AUTOCAD_MCP_BACKEND": "auto" }
    }
  }
}
```

`serverKey`（`photoshop` / `autocad-mcp`）會成為工具名前綴：工具在 Claude 端叫做 `mcp__<serverKey>__<tool>`。下一節的 capability 設定就用這個命名。

### 3. 設定 capability 治理

見下節。這是必要步驟，不是選配。

> 前述兩個專業軟體都有對應的付費專家包（`marketing-designer` 行銷設計師、`cad-drafter` CAD 製圖員），把 soul、`.mcp.json` 樣板、capability 設定與安全 SOP 打包好，一次安裝完成。這些單一專家包也會出現在 dashboard 專家包頁的內建目錄（依職能部門標示），安裝時可用「匯報對象」選單（或 CLI `duduclaw expert install <pack> --attach-under <agent-id>`）把專家掛到既有主管之下，直接併入組織圖與部門。

## Capability 治理

DuDuClaw 用 agent 的 `agent.toml [capabilities]` 控管工具。以下四個欄位是治理這類外部 MCP server 的主力，都是既有機制，寫對設定即生效。

### `allowed_tools`：啟用鍵（不設就靜默失效）

外部 per-agent MCP 工具**不在** Claude CLI 的預設 allow-list 內。在 `-p` 子行程模式下，凡不在 `allowed_tools` 的工具會需要互動確認，而子行程無從確認，於是**靜默 no-op**。所以要讓 Photoshop/AutoCAD 工具能用，`allowed_tools` 必須顯式含 `mcp__photoshop__*` 或 `mcp__autocad-mcp__*`，並補回 agent 仍需要的其他工具：

```toml
[capabilities]
allowed_tools = [
  "mcp__duduclaw__*", "mcp__photoshop__*",
  "WebSearch", "WebFetch", "Read", "Write", "Edit", "Glob", "Grep", "TodoWrite",
]
```

一旦設了 `allowed_tools`，它就成為**唯一**的自動核准集合（allowlist 模式），沒列到的工具一律不放行——這對收窄攻擊面有利（例如刻意不列 `Bash`）。

`allowed_tools`／`denied_tools` 現在有兩層獨立強制：上面說的 Claude CLI `-p` 子行程 allow-list 只管**這個 agent 用 CLI spawn 出去的呼叫**；MCP 分派總門（`McpDispatcher`）另外會對**任何直接對 MCP server 說話的呼叫**（stdio／HTTP／SSE，或非 Claude runtime 的 openai-compat tool-loop）再檢查一次同一份設定，精確比對工具基底名稱（自動剝除 `mcp__<server>__` 前綴）。兩層設定同一份、判定邏輯一致（`denied_tools` 恆贏），不需要為了涵蓋非 CLI 呼叫路徑另外設定第二份。

### `denied_tools`：硬擋 RCE 工具（求值必勝）

`denied_tools` 在 `allowed_tools` 之後求值，永遠贏。Photoshop 的任意腳本工具用此硬擋：

```toml
denied_tools = ["mcp__photoshop__photoshop_execute_script"]
```

### `scoped_tools`：高風險工具走人核 grant 閘

列進 `scoped_tools` 的工具，若無 active 授權（grant），會被折進 Claude CLI 的 `--disallowedTools`；要用得逐任務經 `capability_request` → ApprovalBroker 由人核可（PORTICO task-scoped grant），任務結束即撤銷。用於覆寫存檔類，或無法用 `denied_tools` 精確隔離的 RCE。

AutoCAD 的 `execute_lisp`（RCE）被上游綁在 `system` 工具內，與 undo/redo/screenshot 同一工具，無法只擋子操作，所以整個 `system` 走 grant 閘：

```toml
scoped_tools = ["mcp__autocad-mcp__system"]
```

Photoshop 的覆寫存檔類同樣走 grant 閘（工具實名以 server 首次 introspection 校對為準）：

```toml
scoped_tools = ["mcp__photoshop__photoshop_save_document", "mcp__photoshop__photoshop_close_document"]
```

### `maybe_irreversible_tools`：ActionGuard 覆寫提示

標記「可能不可逆」的工具，走 goal-loop / duduclaw-dispatch 路徑時交 LLM judge 或人核。這是互補宣告；純 `-p` CLI 回合內的外部 MCP 工具，主要仍靠 `scoped_tools` 折進 `--disallowedTools` 圍住。

### 粒度限制（誠實聲明）

capability 治理的粒度到「工具」為止。當上游把危險操作與安全操作綁在同一個工具（AutoCAD 的 `system` 同時含 `execute_lisp`、undo、screenshot），就無法只擋危險子操作，只能整個工具走 grant 閘（代價：安全子操作也需授權）。要更乾淨的隔離，改用不含 RCE 的後端（AutoCAD 用 ezdxf）。

## 風險聲明

- **任意腳本執行（RCE）**：ExtendScript 與 AutoLISP 都能讀寫任意檔案、啟動外部程式，等同桌面權限的 shell。這是這兩個自動化生態的設計，不是 bug。務必依上節硬擋（Photoshop）或走 grant 閘（AutoCAD），不要對 agent 全開。
- **遙測（Photoshop）**：`@alisaitteke/photoshop-mcp` 預設開啟第三方遙測（Mixpanel / PostHog），會送出使用事件。掛載時在 `.mcp.json env` 帶 `ANALYTICS_DISABLED=1`（本頁範例已含）。
- **供應鏈**：`npx -y @alisaitteke/photoshop-mcp` 每次拉 npm latest，`-y` 自動同意。正式部署先在乾淨環境驗過某版行為，再把 args 釘成 `@alisaitteke/photoshop-mcp@<版本>`。AutoCAD 端 clone 後核對 commit、倚賴 `uv.lock`。導入前建議跑 `npm audit` / `pip-audit`。
- **未官方**：兩者皆非 Adobe / Autodesk 官方專案，與原廠無隸屬。
- **未逐行審源碼**：以上依兩 repo README（一手來源）審查。README 未揭露之處（如 Photoshop「API key 不離開本機」的實際傳輸行為）屬未驗證，正式導入前宜補源碼級審查與依賴掃描。

## 疑難排解

| 症狀 | 可能原因 | 處置 |
|------|----------|------|
| Photoshop/AutoCAD 工具「叫了沒反應」 | `allowed_tools` 未含該 server 的 `mcp__<key>__*` | 補上（見「啟用鍵」）；外部 MCP 工具不在預設 allow-list |
| 存檔工具總是被擋 | 在 `scoped_tools` 且無 active grant | 正常行為；要覆寫先經 ApprovalBroker 核可 |
| AutoCAD 工具全無反應 | `.mcp.json` 的 `command` 仍是樣板路徑 | 改成本機 venv python 的絕對路徑 |
| 跨平台跑不起 File IPC | File IPC 僅 Windows + AutoCAD LT 2024+ | 改用 `AUTOCAD_MCP_BACKEND=ezdxf` |
