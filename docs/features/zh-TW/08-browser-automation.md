# 瀏覽器自動化與 Computer Use

> 從一次純 HTTP 抓取到完整虛擬桌面：兩個內建抓取工具、一個可選的外部瀏覽器 server，以及在隔離容器裡執行的電腦操作 session，由 AI 員工透過八個 `computer_*` MCP 工具驅動。沒有自動路由器。

---

## 歷史說明

本頁早期版本描述的是一個**五層自動路由器**，會自己從 L1 一路升級到 L5，還包含一層「L4 Sandbox Browser」。

那個路由器（`browser_router.rs`）寫於 2026-04，從未取得任何呼叫端，CHANGELOG 也從未提及，已於 2026-09 刪除。它描述的 L4 層級，本來就不曾以獨立能力存在過。

實際出貨的東西比較簡單：兩個由 agent 自行選擇的內建 MCP 抓取工具、一個可選的外部 MCP server 提供 headless 瀏覽，以及在容器裡執行的電腦操作 session。成本紀律來自 agent 的判斷與工具說明，不是來自路由引擎。

---

## 實際出貨的東西

### L1 — `web_fetch_cached`

一次帶 SSRF 防護、磁碟快取與速率限制的純 HTTP GET，回傳狀態碼、content type 與 body（截斷在 6 萬字元）。`ttl_seconds` 控制快取（預設 86400）。

SSRF 閘（`web_fetch::validate_url` ＋ `resolve_public_addrs`）拒絕內網主機與雲端 metadata 端點，在請求當下重新解析 DNS，要求解析出的每個位址都是公開位址，然後把答案釘住供該次請求使用。同一道閘也保護常駐感知的 `http_poll` 與 `websocket` 來源。

「公開位址」在整個 workspace 只有一個定義，即 `duduclaw_core::net_addr::is_public_ip`，使用者包括 `web_fetch_cached`、`web_extract`、媒體下載、常駐感知來源、relay URL 檢查、MCP server 匯入、skills RPC、Odoo URL 檢查、wiki 聯邦的 peer 檢查，以及電腦操作的位址釘住。拒絕的 IPv4 區段：`0.0.0.0/8`、`10.0.0.0/8`、`100.64.0.0/10`、`127.0.0.0/8`、`169.254.0.0/16`、`172.16.0.0/12`、`192.0.0.0/24`、`192.0.2.0/24`、`192.168.0.0/16`、`198.18.0.0/15`、`198.51.100.0/24`、`203.0.113.0/24`、`224.0.0.0/4`、`240.0.0.0/4`。IPv6 只有全球單播（`2000::/3`）算公開，但要扣掉 Teredo `2001::/32`，以及文件用區段 `2001:db8::/32` 與 `3fff::/20`。內嵌 IPv4 位址的形式（IPv4-mapped `::ffff:0:0/96`、NAT64 `64:ff9b::/96`、6to4 `2002::/16`）改以內嵌的那個位址判斷，所以 `http://[::ffff:127.0.0.1]/` 會被拒絕；IPv4-compatible `::/96` 與本地用途的 NAT64 `64:ff9b:1::/48` 則整類拒絕。這次變更之前，網路工具的檢查只認得私有、loopback、link-local 與 `0.0.0.0` 這些 IPv4 區段，加上 `::1` 與 `fc00::/7`，所以像 `[::ffff:127.0.0.1]` 這樣的位址會通過檢查並連到 loopback。

用途：有文件的 API、JSON 端點、只需要原始位元組的伺服器渲染頁面。

### L2 — `web_extract`

走同一條帶快取、經 SSRF 驗證的路徑抓取 URL，再用 CSS 選擇器擷取元素。輸出格式為 `text`（預設）、`html` 或 `json`（結構化，含屬性與子節點）。

用途：內容就在初始 HTML 裡的傳統伺服器渲染網站。

L1 與 L2 都不執行 JavaScript。單頁應用（SPA）只會回傳空殼。

### L3 — Playwright 或 Browserbase，以外部 MCP server 形式

要處理 JavaScript 渲染的頁面，作法是在該 agent 自己的 `<agent_dir>/.mcp.json` 註冊一個瀏覽器 MCP server。這是 per-agent 的 MCP server，與全域註冊的 DuDuClaw MCP server 無關。

沒有任何程式會自動寫入這個項目。`crates/duduclaw-agent/src/mcp_template.rs` 裡有 `playwright_mcp_config`、`browserbase_mcp_config`、`ensure_playwright_in_config`、`ensure_browserbase_in_config`，但沒有任何程式呼叫它們。操作者要嘛手寫這個項目，要嘛在 Dashboard 的 MCP marketplace（`marketplace.install`，限管理員）替該 agent 安裝 `playwright`／`browserbase`。手寫時，`playwright_mcp_config(true)` 會產生的形狀如下：

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

Browserbase 方面，`browserbase_mcp_config` 與 marketplace 的 `browserbase` 卡片產生同一個項目：執行 `npx -y @browserbasehq/mcp`，`env` 放 `BROWSERBASE_API_KEY`、`BROWSERBASE_PROJECT_ID`、`GEMINI_API_KEY`（最後一個給這個 server 的預設模型用）。值在安裝卡片時填入（`marketplace.install` 以 `env: { 變數名: 值 }` 接收，缺值會拒絕安裝並列出變數名稱），跟 `claude mcp add -e` 一樣以字串原樣寫進該 AI 員工的 `.mcp.json`。檔案權限只有擁有者可讀寫（0600），`mcp.list` 對每個值只回 `set`／`not_set`／`reference`。這裡用 `${NAME}` 參照行不通：CLI 從自己的環境展開參照，而 gateway 啟動 AI 員工的 CLI 時只保留白名單環境變數，`*_API_KEY` 形式的名稱全部拿掉。改版前卡片寫的就是參照，server 啟動時沒有金鑰。v1.67.1 之前，marketplace 卡片與產生的 Playwright 項目用的是 npm 上不存在的套件名稱（`@anthropic-ai/mcp-server-playwright`、`@anthropic-ai/mcp-server-browserbase` 等 `@anthropic-ai/mcp-server-*`），產生的 Browserbase 項目則用已棄用的 `@browserbasehq/mcp-server-browserbase`；已經用這些名稱寫入的項目不會被自動改寫，CLI 啟動它時會失敗，請手動修改或重新安裝。

它不在 DuDuClaw binary 裡，L2 也不會自動降級到它。該 agent 的 `allowed_tools`／`denied_tools` 決定能不能呼叫。

### L5 — Computer Use

電腦操作只有一條路徑：AI 員工呼叫八個 `computer_*` MCP 工具，每一步都由自己決定。員工必須設 `[capabilities] computer_use = true`。只要某個 runtime 的 CLI 拿得到 DuDuClaw MCP 工具，就能使用（見 [多 Runtime](13-multi-runtime.md)）。不涉及 Anthropic API 金鑰。

| 工具 | 參數 | 作用 |
|---|---|---|
| `computer_session_start` | `task`（字串，選填，記進稽核日誌）、`width`（整數，320–1920）、`height`（整數，240–1200）；尺寸預設值來自 `[capabilities.computer_use_config]`（1280x800） | 啟動該員工的 session，並回報顯示尺寸、各項上限、高風險動作能否在聊天中確認，以及 `computer_navigate` 能開哪些網站 |
| `computer_screenshot` | 無 | 回傳遮罩後的畫面，為 MCP 圖片區塊（PNG），另附一行文字說明已用動作數與剩餘時間。整張截圖被遮掉時，這段文字會如實說明，並寫出原因與下一步動作工具不會回傳截圖，所以 agent 要自己呼叫這個工具來確認結果 |
| `computer_click` | `x`、`y`（整數，截圖像素，從 0 起算）、`button`（`left` 預設或 `right`）、`double`（布林，只限左鍵） | 點一下 |
| `computer_type` | `text`（字串，1–2,000 字元） | 在焦點元素輸入文字 |
| `computer_key` | `key`（字串，由字母、數字、`+`、`-`、`_` 組成，最長 64 字元，例如 `Return`、`ctrl+s`） | 按下按鍵或組合鍵；無效的按鍵會被拒絕，不會被替換成別的 |
| `computer_scroll` | `x`、`y`（整數）、`direction`（`up` 或 `down`，預設 `down`）、`amount`（整數 1–20，預設 3） | 把指標放在該像素上捲動 |
| `computer_navigate` | `url`（字串） | 在容器的瀏覽器開啟頁面。瀏覽器以 kiosk 模式執行、沒有網址列，所以這是唯一開啟頁面的方式 |
| `computer_session_stop` | `session_id`（字串，選填） | 結束 session 並移除它的容器 |

呼叫怎麼走：這些工具跑在 `duduclaw mcp-server`，那是每個 agent CLI 各自一個的獨立行程，本身不擁有任何容器。每次呼叫都會變成對 gateway 在 loopback 上的一個簽章 `POST`（`/api/internal/computer-use`，body 為 `{op: start | screenshot | action | stop | status, …}`），所有檢查都由 gateway 執行，容器也由 gateway 持有（`crates/duduclaw-gateway/src/computer_use_sessions/`）。因此 gateway 必須在執行中，否則工具會回傳說明這點的錯誤。只有 gateway 回報 session 已啟動、動作已執行，工具才會這樣回報。這條路由如何驗證呼叫端，見 [SECURITY.md](../../../SECURITY.md)。

Session 規則：

- **每位員工一個 session。** 已有 session 在執行時再呼叫 `computer_session_start` 會被拒絕，並指出現有的 session。只有該員工能看到或驅動它。
- **整個 gateway 同時最多五個 session。**
- **上限。** Session 在 `max_session_minutes`（預設 10）之後、連續 2 分鐘沒有任何操作之後（等待審批、儀表板有人觀看或有人接手的時間不算閒置；設了 `keep_alive_minutes` 時改為暫停容器，見[閒置保留、即時畫面與接手](#閒置保留即時畫面與接手)），或呼叫 `computer_session_stop` 時結束。每次點擊、輸入、按鍵、捲動與導覽都計入 `max_actions`（預設 50），截圖不計。額度用完後，session 仍可截圖與停止。
- **被拒絕的呼叫端與模式。** 臨時（ephemeral）agent 不能啟動 session，包含 Team-as-Agent 的角色成員（它們會複製父員工的 capabilities）。`[capabilities] computer_use_mode` 為 `"native"` 的員工會被拒絕，錯誤碼為 `native_unsupported`，訊息說明這個模式已移除，並請刪掉該鍵或設為 `"container"`；不會悄悄退回容器。`"auto"` 或沒有這個鍵時，行為等同 `"container"`。
- **威脅等級。** 只有在 `~/.duduclaw/threat_level` 為 GREEN（或不存在）時才能啟動新 session。YELLOW 只允許截圖與停止；RED 會結束所有 session。內容恰好是緊急停止詞（`停止`、`stop`、`abort`、`やめて` 等）的聊天訊息，會結束所有電腦操作 session。
- **清理。** 清掃程式每 15 秒檢查一次是否有 session 超過期限或閒置上限。容器帶有一個標示所屬 DuDuClaw home 的標籤與一個期限標籤；gateway 啟動時與之後每 10 分鐘，會掃除本 home 已結束、或超過期限 10 分鐘以上的容器，Docker 無法列出時則什麼都不移除。
- **請求限制。** 每位員工每分鐘最多 120 個請求，請求 body 最大 64 KiB。

gateway 自己套用的閘，因為這條路由不經過 MCP 分派器也能到達：

- `[capabilities] denied_tools`／`allowed_tools` 與 `scoped_tools`（列在其中的工具需要有效的任務範圍授權）會在每個操作上以該工具自己的名稱檢查。
- 列在 `approval_required_tools`、`irreversible_tools` 或 `maybe_irreversible_tools` 的工具，會透過 ApprovalBroker 等待人工決定（最長 300 秒，沒有回應視同拒絕）。屬於 maybe-irreversible 的 `computer_*` 工具一律會詢問，這裡沒有模型判官。審批文字會寫出工具名稱；`computer_type` 只給字元數，`computer_navigate` 只給驗證過的主機名稱（不含路徑與查詢字串）。`computer_navigate` 的 URL 會在詢問之前先驗證，所以會被拒絕的 URL 不會產生審批請求。MCP 端的審批閘會略過這八個工具，避免重複詢問。
- 每個動作都依固定規則（`risk_detector.rs`）評估風險。當焦點視窗的標題符合 `blocked_actions`（預設 `delete_file`、`terminal`、`system_preferences`）、符合 `CONTRACT.toml` 的 `[must_not] rules` 項目，或焦點視窗標題讀不到時，動作會被拒絕。高風險動作（輸入對準敏感欄位、輸入看起來像憑證的文字，或視窗在非空的 `allowed_apps` 之外）需要有人在 60 秒內於該對話的通道確認，前提是這次工具呼叫來自正在回覆某個通道對話的回合；gateway 是從自己記錄的該員工進行中回合找出通道，不會採用請求裡帶的值。沒有通道可問時（排程執行、被委派的任務），高風險動作會被拒絕。`auto_confirm_trusted = true` 會略過確認。

#### 聊天觸發的迴圈後來怎麼了

早期版本還有第二條路徑：當通道訊息看起來是電腦操作請求（比對一份關鍵字清單），gateway 會自己呼叫 Anthropic Messages API 的 `computer_20251124` 工具，執行回傳的動作，並把進度與截圖貼到通道；另有一個 `computer_use_mode = "native"` 的變體，原本打算驅動主機桌面。兩者都已移除。在任何已發布的版本裡，兩者都沒有完成過一個 session：v1.66.1 的容器用的映像沒有任何東西建置或發布過，它的 Dockerfile 也無法啟動（snap 空殼的瀏覽器、在 `--network=none` 下會中止的 entrypoint、沒有視窗管理器），而 native 模式在任何主機動作之前，就先呼叫了同一個容器啟動。

現在，提到點擊或截圖的通道訊息走一般回覆路徑，要不要呼叫這些工具由 AI 員工決定。電腦操作不讀取任何 Anthropic API 金鑰（直接 API 的回覆 fallback 仍然會讀）。

**映像內容。** `container/Dockerfile.computer-use` 建出一個以 Debian（`debian:trixie-slim`）為基底、約 1.09 GB 的映像：Xvfb、視窗管理器 `openbox`、以 kiosk 模式執行的 Chromium（Debian 真正的 `chromium` 套件）、給儀表板即時畫面用的隨需 VNC 伺服器（只聽 unix socket）、負責動作與截圖的 `xdotool` 和 `scrot`、網域過濾器、遮罩輔助程式 `duduclaw-eval-dom`，以及 `computer_navigate` 使用的 `duduclaw-navigate` 輔助程式。瀏覽器與視窗管理器以非特權使用者 `sandbox` 執行。這個映像在 2026-10-01 之前從來沒有正常運作過（Ubuntu 的瀏覽器套件只是 snap 過渡用的空殼，在容器裡無法啟動；entrypoint 在 `--network=none` 下會中止；也沒有視窗管理器與遮罩輔助程式），當天修好並完成驗證。

發布由 release workflow `.github/workflows/computer-use-image.yml` 負責。它在 git tag `v*` 或手動觸發時執行，在原生 runner 上分別建置 `linux/amd64` 與 `linux/arm64`，推送前先用 gateway 自己的容器參數做 smoke test（視窗管理器起來、拍到一張截圖、DOM 輔助程式回傳 `[]`、`duduclaw-navigate` 在沒有網路時能乾淨地失敗），再發布 `ghcr.io/zhixuli0406/duduclaw-computer-use:<tag>` 與 `:latest`。這個 workflow 從 v1.66.1 之後的第一個 release tag 才開始執行，所以 v1.66.1 以前的版本都沒有已發布的映像。`scripts/release.sh verify` 不會檢查這個映像。

**實際使用哪個映像。** 預設是 `ghcr.io/zhixuli0406/duduclaw-computer-use:v<gateway 版本>`。只有一個全域鍵可以覆寫：`config.toml [computer_use] image = "<ref>"`（可用 digest 參照）。沒有逐員工的映像設定。`[computer_use]` 區段無效（未知的鍵、不可用的參照、`config.toml` 無法解析）時，電腦操作會停用並附上說明，不會退回預設值。映像永遠不會自動下載：`docker run` 帶 `--pull never`，啟動 session 前也會先確認映像在本機。映像不在時，訊息會寫出映像名稱與兩種處理方式：`docker pull <image>`，或在 repo 根目錄自行建置，再把覆寫鍵指向本機 tag：

```bash
docker build -f container/Dockerfile.computer-use -t duduclaw-computer-use:latest .
```

```toml
[computer_use]
image = "duduclaw-computer-use:latest"
```

升級注意：只有本機自建 `duduclaw-computer-use:latest`（舊預設值）的機器，必須拉取帶版本號的映像，或設定覆寫鍵。

**`duduclaw doctor`。** 電腦操作這一列會顯示將使用的映像、是否已在本機，以及哪些 AI 員工設了 `[capabilities] computer_use = true`。沒有任何員工使用電腦操作時為 Pass；有員工使用，且映像不在本機、Docker 無法連線或設定無效時為 Warn。只要有任何員工仍設為 `computer_use_mode = "native"`，它也會單獨發出 Warn 並列出名單。它會逐員工列出可用的白名單主機有幾個、被忽略的項目有幾個。

**截圖遮罩。** 截圖送給模型或存進稽核資料夾之前，gateway 會向輔助程式查詢 `input[type=password]`、`.credit-card`、`[data-sensitive]` 元素在螢幕上的矩形（固定預設值，不可設定），再塗黑。輔助程式透過 Chromium 的 DevTools port 在唯一可見的頁面裡執行查詢，這個 port 只在容器內的 127.0.0.1 監聽。輔助程式所有的 JavaScript 都在隔離的 world（`Page.createIsolatedWorld`）裡執行，所以頁面就算重新定義 `document.visibilityState`、`querySelectorAll` 或 `getBoundingClientRect`，也改不了矩形；這點已用這樣的頁面檢查過。Chromium 從 0,0 鋪滿整個虛擬顯示器，裝置縮放比例為 1，所以頁面座標等於截圖像素；矩形依裝置像素比換算、向外取整、四邊各加 1 px，再裁到螢幕範圍。在測試頁實測，沒有任何敏感像素外露，每邊最多多蓋 2 px，瀏覽器縮放後同樣如此。輔助程式因任何原因失敗（瀏覽器沒在執行、沒有可見頁面或有多個可見頁面、視窗被移動或沒鋪滿顯示器、visual viewport 被縮放、JavaScript 錯誤、5 秒上限，或 gateway 的 10 秒逾時），整張截圖都會被遮掉。多個頁面可見時輔助程式以結束碼 3 離開，其他任何失敗以 1 離開，gateway 依結束碼判斷原因，不看輔助程式輸出的文字。偵測不到的範圍：不屬於頁面的瀏覽器介面（縮放提示泡泡、權限詢問、自動填入下拉選單、alert 對話框）、跨來源 iframe，以及 shadow DOM 內的內容。截圖本身寫在 `/tmp/duduclaw-root/screen.png`；`/tmp/duduclaw-root` 由 root 擁有、權限為 0700，在任何瀏覽器行程啟動之前由 entrypoint 建立，所以瀏覽器使用者無法預先建立或替換這個檔案。Chromium 的 log 也放在那裡。

DOM 遮罩之後，gateway 會讀取目前焦點視窗的標題。標題含有憑證相關標記時，整張截圖會被遮掉。標題讀不到（指令錯誤、逾時、非零結束碼、輸出不是 UTF-8）時，整張截圖同樣會被遮掉。成功讀到但內容為空的標題不會觸發遮罩。

整張遮罩的截圖會明說這件事。gateway 的回應帶有 `fully_masked`（true/false）與 `mask_reason`（`several_pages`、`title_sensitive`、`title_unreadable`、`helper_failed`、`injection_suspected`、`text_unscanned`，或無；最後兩個見[疑似注入時暫停](#疑似注入時暫停)）；輔助程式失敗時不會再去查標題。這時工具文字會告訴 AI 整張畫面因安全考量被隱藏，並說明該怎麼做：有多個視窗時，再呼叫一次 `computer_navigate`，它會關掉多出來的視窗；焦點視窗敏感或標題讀不到時，只要該視窗還在最前面，畫面就無法顯示；其他情況則再截一次圖，若一直發生就停止 session 並開新的。部分遮罩與未遮罩的截圖維持原本的文字。

**網路與網站白名單。** Session 沒有網路，除非該員工在 `agent.toml` 設了網站白名單：

```toml
[capabilities.computer_use_config]
allowed_domains = ["example.com", "docs.example.com"]
```

- 項目必須是完整的主機名稱，去除前後空白並轉小寫後比對。萬用字元（`*.example.com`）、IP 位址、`0x7f000001` 這類數字形式、連接埠、路徑、URL 與 `user@host` 都會被忽略，前 20 個不重複主機之後的項目也一樣。子網域不會被上層網域涵蓋：`example.com` 不代表允許 `docs.example.com`。被忽略的項目會寫進 log、計入 `computer_session_start` 的結果，並由 `duduclaw doctor` 回報。不是陣列的值會被當成空清單。
- 沒有可用項目時，容器以 `--network=none` 執行，`computer_navigate` 會被拒絕，並說明操作者要加哪個設定。
- Session 啟動時，gateway 自己解析每個主機（每個主機 5 秒上限）。主機解析不到、答案中任何位址不是公開位址，或答案裡沒有 IPv4 位址時，這個主機在本次 session 會被略過。啟動結果會列出可連到與被略過的主機，不含位址。
- 對解析成功的主機，容器明確接上 Docker 預設的 bridge（`--network bridge`），並為每個主機加上 `--add-host <host>:<address>`，再加上 `NET_ADMIN`，讓 entrypoint 能在放棄權限之前裝好出站過濾器。過濾器（`container/scripts/domain-filter.sh`，pinned-address 模式）預設丟棄所有出站流量，只允許連到被釘住位址的 TCP 443，並拒絕 DNS，所以名稱只能透過被釘住的項目解析。Loopback 只限 `127.0.0.1` 與 `::1`；Docker 內建解析器 `127.0.0.11` 會被拒絕（在使用者自訂的 Docker 網路上它會回答任何名稱，等於可以透過 DNS 查詢把資料送出去）。被擋下的連線會立即被拒絕，不會拖到逾時：過濾器讓自己的 TCP reset 與 ICMP unreachable 回應在 loopback 上放行，所以連到 443 埠上不在清單內的位址，或連到 80 埠上被釘住的位址，都會立刻被拒絕，不在清單內的名稱也會立刻解析失敗。
- `computer_navigate` 只接受符合以下條件的 URL：`https://`、沒有使用者名稱或密碼、沒有連接埠或連接埠為 443、主機剛好是本次 session 可連到的主機之一（IP 位址會被拒絕）、不超過 2,000 位元組，且不含空白或控制字元。URL 在詢問任何審批之前就先檢查。
- 頁面上通往白名單以外網站的連結不會載入，瀏覽器會顯示它自己的錯誤頁。重新導向到其他網域的網站也一樣會失敗。

在實際 session 上驗證過的項目：開啟 `https://example.com/` 並在截圖中看到它；對未列入的主機、長得像的主機（`example.com.evil.test`）、`http://`、非 443 連接埠、帶使用者名稱的 URL 與 `file://` 都會被拒絕；容器內白名單上的名稱解析到被釘住的位址，其他名稱解析不到，其他位址與連接埠都被拒絕；點擊通往白名單以外網站的連結時，顯示瀏覽器的 DNS 錯誤頁；停止時容器被移除。

白名單防不了的事：

- 白名單上的網站會收到 AI 在那裡輸入或送出的任何內容。白名單決定資料能送到哪裡，不決定送出什麼。
- 透過自己網域做代理或重新導向的網站（翻譯代理、同一主機上的短網址服務、搜尋引擎的快取）可以轉送來自或送往其他地方的內容。
- 位址是在 session 啟動時釘住的。位址在 session 期間改變的網站會停止運作，直到開新 session。
- 白名單是逐員工的。

**瀏覽器政策。** Chromium 以受管政策執行（`container/scripts/chromium-policy.json`，安裝為 `/etc/chromium/policies/managed/duduclaw.json`）；每一項都已在目前映像的 Chromium 上於 `chrome://policy` 檢查過為已接受。頁面不能要求本地網路或 loopback 存取，也不會顯示權限提示（頁面對 `127.0.0.1:9222` 的 fetch 或 WebSocket 會立即失敗）。名稱解析只走 `/etc/hosts`（內建 DNS 用戶端與 DNS-over-HTTPS 都關閉）。無痕、訪客與新增使用者視窗關閉，檔案對話框與列印關閉，下載與彈出視窗被封鎖，通知、地理位置、USB、serial、HID、檔案系統、direct-sockets 與視窗管理權限被封鎖，音訊、視訊與螢幕擷取關閉，密碼管理員與自動填入關閉，書籤列與書籤編輯關閉，新分頁頁面為 `about:blank`。URL 封鎖清單涵蓋 `file://`、`chrome://`、`chrome-untrusted://`、`devtools://`、`view-source:` 與 `javascript://`。`DeveloperToolsAvailability` 刻意不設，因為它同時會停用遮罩與導覽輔助程式使用的 loopback DevTools 協定；DevTools 前端改由 `devtools://` 那一項封鎖。

已知限制：沒有任何政策能擋住 `ctrl+t` 或 `ctrl+n`，而 `ctrl+n` 會開出帶網址列的一般視窗（它能連到的網路仍然只有被釘住的主機）。有多個頁面可見時，截圖會被整張遮掉，`mask_reason` 為 `several_pages`，工具文字會告訴 AI 再呼叫一次 `computer_navigate`，它會關掉多出來的頁面，只留一個。

**其他出站模式。** orchestrator 仍保留一個較舊的出站模式，會在容器內解析允許網域清單（`ALLOWED_DOMAINS`，只有這種情況才加上 `NET_ADMIN`）；gateway 裡沒有任何程式會啟用它。無論哪種模式，只要過濾器裝不上預設拒絕的規則，而容器又有非 loopback 的路由，容器就會拒絕啟動，不會在出站未過濾的狀態下執行。

用途：任何人坐在電腦前能做的事，例如登入、拖放、視覺辨識。它也是差距最大的最慢、最貴選項。

**尚未實作，也尚未驗證。**

- 沒有拖曳、滑鼠移動或 hover 工具，也沒有檔案上傳或下載工具。
- 電腦操作從不驅動主機桌面；native 模式已移除。
- 已用真實模型（Claude Sonnet 5.5，透過真實的 MCP server）驗證過：它啟動了 session、開啟一個白名單內的頁面、從截圖讀出頁面文字、依截圖上的座標點擊連結、認出白名單以外的錯誤頁，並停止 session。尚未驗證：從 gateway 啟動的 agent 回合內（正在回覆真實通道時）啟動 session。
- 聊天中的高風險確認尚未用真實通道操作過。在 WebChat 上，確認提示可能不會傳到使用者手上（未驗證）。
- 含有電腦操作關鍵字的通道訊息改走一般回覆路徑，這一點尚未用真實通道操作過。
- image workflow 還沒有帶著這些變更在 GitHub Actions 上跑過。
- 出站過濾器的 IPv6 規則已安裝，但沒有用實際流量操作過（預設 bridge 沒有 IPv6 路由）。
- 只在 macOS（arm64）的 Docker Desktop 上操作過；沒有 amd64 主機，也沒有原生 Linux Docker Engine。Windows（WSL2）主機未驗證。
- 下載與列印的封鎖，是以政策已被接受、以及對應的快捷鍵沒有開出任何東西來確認，並未實際觸發一次真正的下載。

---

## 安全：預設拒絕

L2 以上的每一層都需要在 `agent.toml` 明確授權：

```toml
[capabilities]
computer_use = false        # 電腦操作 session 與八個 computer_* 工具
browser_via_bash = false    # 從 Bash 工具呼叫啟動瀏覽器
allowed_tools = [...]       # 白名單
denied_tools = [...]        # 黑名單

[capabilities.computer_use_config]
allowed_domains = []        # computer_navigate 可開啟的網站；空白 = 沒有網路
```

- `computer_use = false`（預設）時，gateway 不會替該員工啟動電腦操作 session，八個 `computer_*` 工具會從 `tools/list` 隱藏，直接呼叫也會被拒絕，且是 fail-closed 檢查：檔案不存在、TOML 壞掉、鍵的型別不對，三者一律拒絕。gateway 在 session 期間會重新讀取這個設定；關閉後，session 在下一個操作時結束。
- `denied_tools` 會以 `--disallowedTools` 傳給 CLI，**同時**在 MCP 分派總門強制。
- `browser_via_bash` 已不再設任何環境旗標。過去讀 `DUDUCLAW_BROWSER_VIA_BASH` 的 `bash-gate.sh` 白名單，已隨其他 shell hook 一起在 `ba015a48` 移除。這個 capability 仍然有效：它餵給 `disallowed_tools()` 與 `CapabilitiesConfig::sandbox_level()`，後者是 codex 與 gemini runtime 決定 `ReadOnly` 還是 `WorkspaceWrite` 沙盒的依據。
- 只存在於那個死路由器欄位上的限制（信任／封鎖網域、每 session 頁數上限）從未以那種形式實作。現在有的是：逐員工網站白名單（上面的 `allowed_domains`）、session 動作額度、聊天中的高風險確認，以及透過 `ApprovalBroker` 與 `agent.toml [capabilities] approval_required_tools`／`irreversible_tools`／`maybe_irreversible_tools` 的審批閘，這個審批閘對 `computer_*` 工具和其他工具一視同仁。截圖稽核不是開關：一律會寫（見下方稽核日誌一項）。

---

## 成本概略比較

| 層級 | 啟動 | 記憶體 | 執行 JS | 需要容器 |
|---|---|---|---|---|
| L1 `web_fetch_cached` | ~0 ms | ~1 MB | 否 | 否 |
| L2 `web_extract` | ~0 ms | ~5 MB | 否 | 否 |
| L3 Playwright／Browserbase MCP | 數秒 | 數百 MB | 是 | 否（外部行程或雲端） |
| L5 電腦操作 | ~10 s | 500 MB+ | 是 | 是 |

L1 就答得出來的問題卻動用 L5，是最昂貴的誤用，而且要靠 agent 自己避免：平台不會攔它。

---

## 與其他系統的互動

- **容器隔離** — L5 跑在自己的 Docker 容器裡（預設映像 `ghcr.io/zhixuli0406/duduclaw-computer-use:v<gateway 版本>`，不會自動下載）：唯讀根檔案系統、256 MB 的 tmpfs `/tmp`、1 個 CPU、512 MB 記憶體、512 個行程（Chromium 的執行緒也算在這個上限內）；使用 `--network=none`，除非 session 有解析成功的白名單主機，這時會改加 `--network bridge`、被釘住的主機與給網域過濾器用的 `NET_ADMIN`（見上方網路段落）。每個電腦操作容器都帶 `--security-opt no-new-privileges`。這個容器由 `computer_use_orchestrator` 啟動，與逐員工的任務沙箱是分開的（見[任務沙箱指南](../../guides/zh-TW/task-sandbox.md)）。
- **安全防線** — capability 強制與稽核軌跡見 [05-security-defense.md](05-security-defense.md)。
- **常駐感知** — `http_poll`／`websocket` tick 來源共用 L1 的 SSRF 閘，見 [41-resident-sensing.md](41-resident-sensing.md)。
- **稽核日誌** — 八個 `computer_*` 工具都會記進 `tool_calls.jsonl`，輸入經過精簡：`computer_type` 只記字元數，`computer_navigate` 只記主機與路徑長度（不含查詢字串），`computer_screenshot` 不含圖片。`web_fetch_cached` 與 `web_extract` 屬唯讀，不會記。Session 還會在 `~/.duduclaw/audit/browser/audit.jsonl` 追加帶雜湊鏈的紀錄（tier `L5a`；session 開始與結束、每次截圖（含 `fully_masked` 與 `mask_reason`）、每個動作與其風險評級、每次導覽的主機與路徑但不含查詢字串、每次拒絕；輸入的文字同樣只記字元數），並把遮罩後的截圖存到 `~/.duduclaw/audit/browser/screenshots/<agent_id>/`。截圖每位員工保留 7 天，且最多 500 個檔案或 200 MiB，超過時先刪最舊的；`audit.jsonl` 超過 16 MiB 後輪替為 `audit.jsonl.old`。細節見[開發指南第 5 節](../../guides/zh-TW/development-guide.md#5-審計與監控)。

---

## 總結

誠實版沒有路由器故事漂亮，但比較好維運：四種碰網路的方式，各有各的成本與各自的開關，而 agent 必須自己選。路由引擎被刪掉的那天，這一頁就該停止描述它。

## 頻道核准的保存與核對

高風險 Computer Use 使用原帳號及原對話或討論串送出確認，回覆 `確認 <完整 UUID>` 或 `取消 <完整 UUID>`；問題使用 `回答 <完整 UUID> <答案>`，不授權工具。單獨的是、A/B 不會選取請求。執行前重驗畫面、視窗、政策與取消狀態，重啟會讓舊畫面核准失效。沒有收據的執行成為 `uncertain`，需 Admin 核對。支援入口及限制見[操作指南](../../guides/durable-channel-decisions.md)。

## 電腦操作工作區（session 結束後留下來的檔案）

電腦操作的容器在 session 結束時就刪除。要留下員工整理的內容，可以在 session 掛上一個**電腦操作工作區**：`computer_session_start` 帶 `workspace = "new"` 或既有的 `ws-…` id。只有 gateway 會寫入（`computer_workspace_write`，寫進員工這次 session 掛上的工作區）；`computer_workspace_list` 與 `computer_workspace_read` 不需要開著 session。容器裡的檔案在 `/workspace/files`，唯讀，放在只有 root 能進的 tmpfs 底下，瀏覽器的帳號進不去。預設關閉：要打開 `config.toml [computer_use.workspaces] enabled = true`，以及該員工的 `[capabilities.computer_use_config] workspace = true`。只支援 macOS 與 Linux。

配額、保留期限、同時只能一個 session 的租約、操作者指令（`duduclaw ops computer-workspaces`，指令列上所有會改變狀態的動作都要先由管理員在儀表板核准；緊急處置用儀表板或總開關），以及已知限制（包括擁有者隔離只對三個工作區工具成立，對有 `Read` 或 Bash 的員工不成立），見[電腦操作工作區指南](../../guides/zh-TW/computer-workspaces.md)。

## 閒置保留、即時畫面與接手

這三項讓人可以看著員工的電腦、必要時接手，也讓 session 在工作中斷時不必重來。三項都只會收緊員工能做的事。程式碼：`crates/duduclaw-gateway/src/computer_use_sessions/`（`keepalive.rs`、`live_view.rs`、`live_ops.rs`、`view_ws.rs`、`rfb.rs`），儀表板 `web/src/components/agent/ComputerSessionPanel.tsx`。

### 閒置保留

```toml
[capabilities.computer_use_config]
keep_alive_minutes = 30      # 預設 0 = 和以前一樣閒置 2 分鐘就結束；最多 240
takeover_idle_minutes = 10   # 預設 10；最多 60
```

`keep_alive_minutes` 大於 0 時，閒置 2 分鐘的 session 不會結束，而是由清掃程式暫停它的容器（`docker pause`）。員工下一次呼叫 `computer_*`，或儀表板有人要觀看時，會恢復容器（`docker unpause`）再繼續。暫停超過 `keep_alive_minutes` 就結束。暫停期間其他規則照常：`max_session_minutes` 期限繼續倒數、`max_actions` 不變，威脅等級 RED、聊天緊急停止、權限被關、工作區失去控制權與 `computer_session_stop` 都會結束它。暫停中的 session 保留名額，所以仍計入同時 5 個的上限。調低或移除 `keep_alive_minutes` 會對進行中的 session 生效，調高則不會。容器恢復不了時 session 結束（`resume_failed`）。持有 gateway 實例鎖的 gateway 會把超過期限標籤的暫停容器先恢復再移除，不等平常的 10 分鐘寬限；每個 gateway 仍會在期限過後 10 分鐘移除它。暫停與恢復都寫入稽核（`session_pause`、`session_resume`）。

### 即時畫面

員工頁有一個「電腦」分頁（員工開了 `computer_use` 時顯示），每 5 秒更新：session 是執行中、已暫停、暫停待確認還是有人接手中，已用動作、剩餘時間、閒置保留時間與正在觀看的人數。按「觀看」打開畫面。

誰能做什麼（每次請求與每條觀看連線都重新從 `users.db` 讀取角色與綁定）：

| | 管理員 | 綁定該員工的主管 | 以 Operator 以上綁定該員工的帳號 |
|---|---|---|---|
| 狀態、觀看、結束 | 可以 | 可以（任何綁定層級） | 可以 |
| 接手、交還、疑似注入後恢復 | 可以 | 需以 Operator 以上綁定 | 不行 |

畫面怎麼送到儀表板：有人觀看時，gateway 在容器裡啟動 `x11vnc`（`docker exec … duduclaw-vnc start viewonly`，新的 8 字元隨機密碼從 stdin 傳入）。它只在 root 專用的 `/tmp/duduclaw-root` 裡的 unix socket 監聽，不開任何 TCP port（若 5900–5999 有任何監聽，輔助程式會拒絕繼續執行），所以網路與瀏覽器的非特權帳號都碰不到它，主機上也沒有公開任何 port。儀表板向 `computer_sessions.view` RPC 取得一張 30 秒內有效、只能用一次的票，再開啟 gateway 的 `/ws/computer-view?ticket=…`；gateway 像儀表板 socket 一樣檢查 Origin、用掉這張票、重新讀取並判斷帳號權限、每個 session 最多 4 位觀看者，然後把 WebSocket 接到 `docker exec -i … duduclaw-vnc-relay`（unix socket 與 stdio 之間的轉送）。Session 結束或帳號失去權限（每 30 秒檢查）時連線就關閉。儀表板用 noVNC 畫出畫面。

另一個選項是在 127.0.0.1 公開 port 再由 gateway 轉送；沒有採用，因為主機上任何使用者的任何程式都能連那個 port，而 `docker exec` 轉送需要 Docker daemon 的權限。

gateway 會解析觀看端送來的協定內容（RFB 3.7/3.8）：鍵盤、滑鼠、剪貼簿與延伸按鍵訊息只放行持有接手權的帳號；調整畫面大小（`SetDesktopSize`，會破壞截圖遮罩）與 `xvp` 關機／重開機請求一律丟棄；認不得的內容直接關閉連線。沒有人接手時 VNC 伺服器另外以 `-viewonly` 執行，而且一律帶 `-noremote -nocmds -nosel`（雙向都不交換剪貼簿）。每條觀看連線都在瀏覽器稽核紀錄寫入 `view_start` 與 `view_stop`（誰、多久、轉送了幾則輸入），不記錄畫面與按鍵內容。

### 接手與交還

按「接手」會讓這個帳號取得 session 的接手權。持有期間：

- 員工的 `computer_click`、`computer_type`、`computer_key`、`computer_scroll`、`computer_navigate` 與 `computer_workspace_write` 一律以 `human_has_control` 拒絕，並請它等待；截圖、狀態與結束照常，截圖照樣遮罩；
- 串流以輸入模式、新密碼重新啟動，只有持有者的鍵盤滑鼠能到達畫面；
- session 不會因閒置而結束或暫停。

接手權在以下情況結束：按「交還」（持有者或管理員）、持有者 `takeover_idle_minutes` 分鐘沒有輸入、串流啟動失敗、session 結束。之後串流回到唯讀。員工會透過工作狀態（與 `working_state_handoff` 同一個儲存）收到一則結構化的 `continue` 交接：誰接手、多久、送出幾次輸入、為什麼結束、對方選填的留言（經過 `input_guard`，可疑的留言改成一句提示）、接手前的交接（縮短），以及下一步「先截圖，不要沿用舊座標」。稽核：`takeover_start`、`takeover_end`。接手時已通過最後一道檢查的動作會做完，之後的都會被擋。

### 疑似注入時暫停

每次 `computer_screenshot` 也會透過 `duduclaw-eval-dom`（isolated world）讀出可見頁面的文字（`document.body.innerText`，最多 32,768 字元），以封鎖門檻交給 `input_guard`。命中封鎖等級時 session 會被暫停：

- 這張與之後的每張截圖都整張遮罩，`mask_reason` 為 `injection_suspected`，直到有人恢復；
- 員工的每個動作與工作區寫入都以 `injection_suspected` 拒絕；
- Activity Feed 與員工通知頻道的 L3 推播會寫出命中的規則類別；
- 稽核 `injection_suspected` 只記規則類別，從不記頁面文字。

在「電腦」分頁按「恢復」解除（稽核 `injection_resume`）。讀不到頁面文字時，截圖整張遮罩、`mask_reason` 為 `text_unscanned`（原本已整張遮罩則維持原因），session 不會暫停。文字在截圖後立刻讀取，所以中間變動的頁面會以較新的內容掃描。`input_guard` 本身的限制照舊：它比對句型，一般頁面也可能誤判，偽裝得好的文字也可能過關。

### 未涵蓋與未驗證

- 開發環境沒有 Docker：暫停／恢復、VNC 輔助程式、轉送與串流只用替身與單元測試驗證。轉送腳本與 TCP port 檢查在一般 Linux 上跑過；映像 workflow 的 smoke test 已加上透過轉送讀 RFB 開頭、瀏覽器帳號連不到 socket、沒有 VNC TCP port 的檢查，但還沒跑過。Debian 映像裡的 `x11vnc` 是否完全照用法接受 `-rfbport 0 -unixsock` 未驗證；不接受時串流會拒絕啟動，不會暴露任何東西。
- 真實瀏覽器中的 noVNC 畫面、真實容器的 `docker pause`／`docker unpause` 以及對暫停中容器的 `docker rm --force` 都未驗證。
- VNC 認證是 8 字元密碼與 DES；真正的保護是 root 專用的 unix socket 與 gateway 經過驗證、過濾的轉送。密碼會送到有權觀看者的瀏覽器。
- 綁定該員工的主管或 Operator 看得到整個畫面（遮罩只套用在員工拿到的截圖，不套用在即時串流），包括網站顯示的內容。
- 在唯讀與輸入模式之間切換會重啟 VNC 伺服器，正在觀看的人會斷線後重連。
- 閒置保留、接手與觀看人數都存在 gateway 行程裡：gateway 重新啟動時 session 一樣會結束。
- 兩個設定都在員工編輯頁的電腦操作區塊（僅管理員，經 `agents.update`）：閒置保留 0–240 分鐘、接手閒置上限 1–60 分鐘；其他值或型別伺服器一律拒絕。

