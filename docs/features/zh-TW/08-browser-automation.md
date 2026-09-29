# 瀏覽器自動化與 Computer Use

> 三組 MCP 工具，從一次純 HTTP 抓取到完整虛擬桌面。由 agent 自己選，沒有自動路由器。

---

## 歷史說明

本頁早期版本描述的是一個**五層自動路由器**，會自己從 L1 一路升級到 L5，還包含一層「L4 Sandbox Browser」。

那個路由器（`browser_router.rs`）寫於 2026-04，從未取得任何呼叫端，CHANGELOG 也從未提及，已於 2026-09 刪除。它描述的 L4 層級，本來就不曾以獨立能力存在過。

實際出貨的東西比較簡單：三組由 agent 自行選擇的 MCP 工具，外加一個可選的外部 MCP server 提供 headless 瀏覽。成本紀律來自 agent 的判斷與工具說明，不是來自路由引擎。

---

## 實際出貨的東西

### L1 — `web_fetch_cached`

一次帶 SSRF 防護、磁碟快取與速率限制的純 HTTP GET，回傳狀態碼、content type 與 body（截斷在 6 萬字元）。`ttl_seconds` 控制快取（預設 86400）。

SSRF 閘（`web_fetch::validate_url` ＋ `resolve_public_addrs`）拒絕內網主機與雲端 metadata 端點，在請求當下重新解析 DNS，要求解析出的每個位址都是公開位址，然後把答案釘住供該次請求使用。同一道閘也保護常駐感知的 `http_poll` 與 `websocket` 來源。

用途：有文件的 API、JSON 端點、只需要原始位元組的伺服器渲染頁面。

### L2 — `web_extract`

走同一條帶快取、經 SSRF 驗證的路徑抓取 URL，再用 CSS 選擇器擷取元素。輸出格式為 `text`（預設）、`html` 或 `json`（結構化，含屬性與子節點）。

用途：內容就在初始 HTML 裡的傳統伺服器渲染網站。

L1 與 L2 都不執行 JavaScript。單頁應用（SPA）只會回傳空殼。

### L3 — Playwright 或 Browserbase，以外部 MCP server 形式

要處理 JavaScript 渲染的頁面，作法是在該 agent 自己的 `<agent_dir>/.mcp.json` 註冊一個瀏覽器 MCP server（`mcp_template.rs` 會產生 Playwright 或 Browserbase 設定）。這是 per-agent 的 MCP server，與全域註冊的 DuDuClaw MCP server 無關。

它不在 DuDuClaw binary 裡，L2 不會自動降級到它，也沒有任何東西會替你安裝——由操作者加入，再由該 agent 的 `allowed_tools`／`denied_tools` 決定能不能呼叫。

### L5 — Computer Use

七個 MCP 工具驅動容器沙盒裡的虛擬顯示器：`computer_screenshot`、`computer_click`、`computer_type`、`computer_key`、`computer_scroll`、`computer_session_start`、`computer_session_stop`。

`computer_use_orchestrator` 負責整個迴圈——容器生命週期 → 截圖 → Claude 視覺分析 → 動作 → 重複——並把進度回報到原始通道。容器映像（預設 `duduclaw-computer-use:latest`）、顯示尺寸與網路模式都可設定，即使 panic 或任務取消也保證清理。

用途：任何人坐在電腦前能做的事——登入、拖放、視覺辨識。它也是差距最大的最慢、最貴選項。

---

## 安全：預設拒絕

L2 以上的每一層都需要在 `agent.toml` 明確授權：

```toml
[capabilities]
computer_use = false        # 七個 computer_* 工具
browser_via_bash = false    # 從 Bash 工具呼叫啟動瀏覽器
allowed_tools = [...]       # 白名單
denied_tools = [...]        # 黑名單
```

- `computer_use = false`（預設）讓每個 `computer_*` MCP 工具回傳拒絕，且是 fail-closed 檢查：檔案不存在、TOML 壞掉、鍵的型別不對，三者一律拒絕。
- `denied_tools` 會以 `--disallowedTools` 傳給 CLI，**同時**在 MCP 分派總門強制，所以連 PTY pool 路徑也擋得住。
- `browser_via_bash` 已不再設任何環境旗標。過去讀 `DUDUCLAW_BROWSER_VIA_BASH` 的 `bash-gate.sh` 白名單，已隨其他 shell hook 一起在 `ba015a48` 移除。這個 capability 仍然有效：它餵給 `disallowed_tools()` 與 `CapabilitiesConfig::sandbox_level()`，後者是 codex 與 gemini runtime 決定 `ReadOnly` 還是 `WorkspaceWrite` 沙盒的依據。
- 另外三組只存在於那個死路由器欄位上的限制（信任／封鎖網域、每 session 頁數上限、截圖稽核、逐動作人工核准）**沒有任何實作**。不可逆動作的核准閘走的是 `ApprovalBroker` 與 `agent.toml [capabilities] approval_required_tools`／`irreversible_tools`。

---

## 成本概略比較

| 層級 | 啟動 | 記憶體 | 執行 JS | 需要容器 |
|---|---|---|---|---|
| L1 `web_fetch_cached` | ~0 ms | ~1 MB | 否 | 否 |
| L2 `web_extract` | ~0 ms | ~5 MB | 否 | 否 |
| L3 Playwright／Browserbase MCP | 數秒 | 數百 MB | 是 | 否（外部行程或雲端） |
| L5 `computer_*` | ~10 s | 500 MB+ | 是 | 是 |

L1 就答得出來的問題卻動用 L5，是最昂貴的誤用，而且要靠 agent 自己避免：平台不會攔它。

---

## 與其他系統的互動

- **容器沙盒** — L5 跑在與 agent 任務隔離相同的容器基礎設施上（`--network=none`、tmpfs、唯讀 rootfs）。
- **安全防線** — capability 強制與稽核軌跡見 [05-security-defense.md](05-security-defense.md)。
- **常駐感知** — `http_poll`／`websocket` tick 來源共用 L1 的 SSRF 閘，見 [41-resident-sensing.md](41-resident-sensing.md)。
- **稽核日誌** — 每次 MCP 工具呼叫（含被拒絕的）都會進 `tool_calls.jsonl`，參數與結果皆經遮罩。

---

## 總結

誠實版沒有路由器故事漂亮，但比較好維運：四種碰網路的方式，各有各的成本與各自的開關，而 agent 必須自己選。路由引擎被刪掉的那天，這一頁就該停止描述它。
