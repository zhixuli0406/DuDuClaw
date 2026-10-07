# 更多 MCP 工具：MCP 註冊表、遠端伺服器與應用整合平台

DuDuClaw 的 AI 員工透過自己 `.mcp.json` 裡列的 MCP server 使用工具。除了內建目錄（Playwright、Browserbase、Filesystem、Memory）與「從網址匯入」之外，儀表板現在可以：

1. **搜尋官方 MCP 註冊表**，把伺服器安裝給某位員工（`MCP → MCP 註冊表`）。
2. **連線遠端 MCP 伺服器**（Streamable HTTP），用 OAuth 登入、API 權杖或不需登入（`MCP → 遠端伺服器`）。Gateway 加密保存並自動更新登入資訊，員工只拿到工具。
3. **從 Marketplace 分頁連線代管的應用整合平台**（Zapier、Composio）：一個遠端端點後面有上千個第三方應用。

連線只有管理者能做；任何登入的人都能搜尋註冊表，非管理者的安裝會變成安裝申請，照原本的主管 → 管理者流程核准。

## 1. MCP 註冊表搜尋與安裝

`MCP → MCP 註冊表`：輸入名稱或主題後搜尋。結果來自 `https://registry.modelcontextprotocol.io`（Gateway 只連這個主機、結果快取約十分鐘、超過 2 MiB 的回應一律拒絕）。每一筆會顯示套件種類（`npm`、`pypi`、`oci`）、是否有代管端點（`remote`）、版本、原始碼、必填設定，以及不能安裝時的原因：

| 原因 | 意思 |
|------|------|
| 沒有 DuDuClaw 能執行的套件或遠端端點 | 只有 `nuget`、`mcpb` 等套件 |
| 只提供舊版 SSE 連線方式 | 2024-11-05 的 HTTP+SSE 協定，原生橋接不支援 |
| 需要自訂請求標頭 | 例如 `X-API-Key`；橋接只送 `Authorization` |
| 已標示停用／已移除 | 註冊表如此標示 |

**安裝**時選擇員工與伺服器名稱，並填入套件的必填環境變數（存在該員工的 `.mcp.json`，只有操作者的系統使用者讀得到）。同時有套件與代管端點時，可以選要用哪一種。

Gateway 端的處理（`mcp.registry_install`）：

- 依畫面上的版本重新抓取該伺服器的 `server.json`；
- 交給與「從網址匯入」相同的解析器（`npm → npx -y <套件>@<版本>`、`pypi → uvx <套件>==<版本>`、`oci → docker run -i --rm <映像>`，版本固定為你看到的那一版）；
- 管理者的安裝走與 `mcp.import.install` 相同、有安全掃描的安裝流程（掃描失敗即拒絕、用加鎖的 `.mcp.json` 寫入器）；其他人的安裝變成 `mcp.install_request`，核准後才安裝。

代管端點會以遠端伺服器的形式安裝（見下節），之後要由管理者完成連線，員工才用得到。

## 2. 遠端 MCP 伺服器

### 運作方式

遠端伺服器在員工 `.mcp.json` 裡的項目長這樣：

```json
"zapier": {
  "command": "/usr/local/bin/duduclaw",
  "args": ["mcp-remote-bridge", "--agent", "nova", "--server", "zapier"],
  "env": { "DUDUCLAW_HOME": "/home/me/.duduclaw" }
}
```

Claude CLI 把它當一般的 stdio MCP server 啟動。隱藏指令 `duduclaw mcp-remote-bridge` 把每一則 JSON-RPC 訊息（請求、通知與回應）以 Streamable HTTP 轉給伺服器，再把伺服器的回答（JSON 或 `text/event-stream`）寫回，記住 `Mcp-Session-Id`、送出協商好的 `MCP-Protocol-Version`，並在每個請求加上最新的 `Authorization: Bearer …`。

網址與所有登入資訊只存在 `<home>/remote_mcp/servers.json`（權限 0600、跨行程鎖），用 Gateway 的本機金鑰檔加密（AES-256-GCM，與頻道權杖、OAuth 權杖同一把）。無法加密時什麼都不存。`.mcp.json`、橋接的命令列與環境變數裡都沒有任何秘密。檔案中只有這些是明文：員工、伺服器名稱、登入方式、網址的主機、狀態、時間與是否有 refresh token。

這取代了 DuDuClaw 以前替遠端伺服器寫入的 `npx -y mcp-remote <url>`。那個套件要在 Gateway 主機上開瀏覽器登入（無螢幕的 Gateway 與 DuDuClaw OS 設備做不到），並把權杖以明文存在 `~/.mcp-auth`。現在只剩一種情況還會寫它：清單裡 `"type": "sse"` 的項目（原生橋接不支援的舊版連線方式），匯入預覽會註明。

### 連線

`MCP → 遠端伺服器 → 連線伺服器`（或註冊表安裝後的**連線**、Marketplace 整合平台卡片上的**連線**）：選擇員工、伺服器名稱、網址與登入方式：

- **登入（OAuth）**——Gateway 找出伺服器的授權伺服器、以「DuDuClaw」名稱註冊用戶端，儀表板在新分頁開啟登入頁面。核准後服務商把瀏覽器送回 `<儀表板網址>/oauth/mcp/callback`，Gateway 換取權杖、保存、寫入 `.mcp.json` 項目，顯示一個「Connected」小頁面與返回連結。對話框會自己偵測到完成。
- **API 權杖**——貼上一次，以 `Authorization: Bearer <權杖>` 送出。儲存前 Gateway 會先用 `initialize` 請求確認可用。
- **不需要**——用於不需登入的伺服器（同樣先確認）。

**中斷連線**會刪除已保存的登入資訊但保留伺服器（之後可再連線）；**移除**連同紀錄與 `.mcp.json` 項目一起刪除。從員工的伺服器清單移除遠端伺服器也一樣。兩者都不會向服務商送出撤銷請求：請也到服務商帳號裡撤銷 DuDuClaw 的存取權。

權杖在到期前一分鐘內會自動更新，伺服器回 `401` 時再更新一次。服務商拒絕 refresh token 時，連線會標示**需要重新登入**；在那之前員工對該伺服器的呼叫會失敗，訊息會告訴它需要管理者重新連線。

### OAuth 流程細節（依 MCP 授權規格）

1. 先送一個未帶權杖的 `initialize`；`401` 回應的 `WWW-Authenticate: Bearer resource_metadata="…"` 指向受保護資源中繼資料（RFC 9728）。沒有這個參數時，依序嘗試 `/.well-known/oauth-protected-resource/<路徑>` 與 `/.well-known/oauth-protected-resource`。中繼資料必須屬於同一個來源。
2. 讀取授權伺服器中繼資料（RFC 8414，接著 OpenID Connect discovery，路徑插入形式優先）。`issuer` 必須相符，且 `code_challenge_methods_supported` 必須列出 `S256`，否則依規格拒絕。
3. 動態用戶端註冊（RFC 7591）：公開用戶端、`token_endpoint_auth_method = none`、轉回網址 `<儀表板來源>/oauth/mcp/callback`。服務商沒有註冊端點時，在對話框打開**使用自己的 OAuth 用戶端**，填入你在那裡以該轉回網址註冊的 client id（與 secret）。
4. 授權碼搭配 PKCE S256，並在授權、換取權杖與更新請求都帶 RFC 8707 的 `resource` 參數（伺服器的標準網址）。
5. `state` 是 32 個隨機位元組，只存在 Gateway 記憶體，綁定該員工、伺服器、verifier 與儀表板來源，十分鐘內有效、只能用一次。
6. refresh token 會輪替：服務商給的新 token 取代舊的；同一連線的更新以鎖檔排隊，兩個橋接行程不會用掉同一個 refresh token。

Client ID Metadata Documents（CIMD）沒有實作：它需要一個公開網址上的 `client.json`，自架的 Gateway 沒有。

### 哪些儀表板網址可以接收登入結果

轉回目標是你正在使用的儀表板網址（`window.location.origin`）。Gateway 接受：

- loopback 網址（`localhost`、`127.0.0.1`、`[::1]`），http 或 https；
- 列在 `config.toml [gateway] allowed_origins`（或 `DUDUCLAW_ALLOWED_ORIGINS`）裡的 `https` 網址，主機與連接埠需完全相符。

其他主機的 `http` 一律拒絕（OAuth 2.1 只允許 loopback 使用非 https 的轉回網址）。若你是用區網 IP 開儀表板，登入時請在 Gateway 主機上以 `http://localhost:<埠>` 開啟，或改用 https 並把主機加進 `allowed_origins`。回呼頁由 Gateway 自己在 `/oauth/mcp/callback` 提供，不需要登入（只能用一次的 state 就是防護）。

### Gateway 可以連到哪些位址

所有相關網址（伺服器、它的中繼資料、授權伺服器的各端點）都必須是 `https`，且只能解析到公開網際網路位址（`duduclaw_core::net_addr::is_public_ip`）；解析出的位址在該次連線中固定，POST 不跟隨轉址，中繼資料的 GET 轉址會重新檢查。只有 Gateway 本機上的伺服器（`localhost`、`127.0.0.0/8`、`::1`）可以用 `http`，也只有這時它的中繼資料可以指向 loopback。私有網段（`10.x`、`192.168.x` 等）的伺服器一律拒絕。

### 遮蔽

RFC-23 遮蔽啟用時，橋接項目和其他 stdio server 一樣會被 `duduclaw mcp-proxy` 包起來，遠端工具結果也會遮蔽。

## 3. 代管的應用整合平台：Zapier 與 Composio

Marketplace 分頁有兩張遠端卡片：

| 卡片 | 預設端點（取自 MCP 註冊表） | 登入 |
|------|----------------------------|------|
| Zapier | `https://mcp.zapier.com/api/v1/connect` | OAuth |
| Composio | `https://connect.composio.dev/mcp` | OAuth |

**連線**會開啟已填好端點的遠端對話框；若你的帳號顯示不同的端點請換掉，或服務商給了 API 權杖就改用權杖。DuDuClaw 不附任何帳號或金鑰。

員工透過這些工具送出的所有內容都會經過服務商的雲端，服務商也能操作你在那裡連接的每個應用。請在 Zapier 或 Composio 帳號中只開啟這位員工需要的應用與動作，並盡量每位員工使用各自的連線。

## RPC 一覽

| 方法 | 誰可以用 | 用途 |
|------|---------|------|
| `mcp.registry_search { query, cursor? }` | 已登入 | 搜尋（固定主機、快取） |
| `mcp.registry_install { name, version?, agent_id, remote?, server_name?, env? }` | 已登入（非管理者 ⇒ 安裝申請） | 走有掃描的安裝流程 |
| `mcp.remote_connect { agent_id, name, url?, auth, bearer?, redirect_origin?, client_id?, client_secret? }` | 管理者 | 連線；`oauth` 回傳 `authorize_url` |
| `mcp.remote_status { agent_id? }` | 管理者 | 不含秘密的紀錄 |
| `mcp.remote_disconnect { agent_id, name, forget? }` | 管理者 | 刪除登入資訊（`forget` 連項目一起移除） |

稽核事件（`security_audit.jsonl`）：`remote_mcp_connect_started`、`remote_mcp_connected`、`remote_mcp_connect_failed`、`remote_mcp_disconnected`（員工、伺服器、主機、登入方式；不含網址路徑或權杖）。`duduclaw doctor` 的「員工 MCP 設定中的其他伺服器」一列會標出橋接項目的主機與連線狀態。

## 未涵蓋／未驗證

- 沒有用真的 Zapier、Composio 帳號或真的第三方 OAuth 服務商測試；流程只對本機的假授權伺服器與假 MCP 伺服器驗證過。
- 橋接不會開啟請求以外、由伺服器主動推送訊息的 `GET` 串流，也不會續接中斷的事件串流。
- 舊版 HTTP+SSE 伺服器仍走 `npx mcp-remote`（見上方）；需要自訂標頭的遠端端點不支援。
- 中斷連線不會在服務商端撤銷權杖。
- 桌面版的儀表板來源（`tauri://…`）不是可接受的轉回來源；請用瀏覽器登入。
- 員工與 Gateway 是同一個系統使用者：擁有不受限 Bash 的員工可以讀取金鑰檔與紀錄檔，或自行以自己的 `--agent` 啟動橋接。橋接會拒絕與行程 `DUDUCLAW_AGENT_ID` 不同的 `--agent`，但真正的隔離是不要開放 Bash。
- 進行中的登入只存在 Gateway 記憶體：登入途中重啟 Gateway 就要重新開始。
