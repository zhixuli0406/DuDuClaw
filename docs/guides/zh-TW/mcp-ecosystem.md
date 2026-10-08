# 更多 MCP 工具：MCP 註冊表、遠端伺服器與應用整合平台

DuDuClaw 的 AI 員工透過自己 `.mcp.json` 裡列的 MCP server 使用工具。除了內建目錄（Playwright、Browserbase、Filesystem、Memory）與「從網址匯入」之外，儀表板現在可以：

1. **搜尋官方 MCP 註冊表**，把伺服器安裝給某位員工（`MCP → MCP 註冊表`）。
2. **連線遠端 MCP 伺服器**（Streamable HTTP），用 OAuth 登入、API 權杖或不需登入（`MCP → 遠端伺服器`）。Gateway 加密保存並自動更新登入資訊，員工只拿到工具。
3. **從 Marketplace 分頁連線代管的應用整合平台**（Zapier、Composio）：一個遠端端點後面有上千個第三方應用。
4. **依動作類型分類並管制第三方工具**（第 4 節）。
5. **接收遠端伺服器的事件**（MCP Events，第 5 節），讓自動化規則或持續責任在事情發生時開始工作。

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

**中斷連線**會刪除已保存的登入資訊但保留伺服器（之後可再連線）；**移除**連同紀錄與 `.mcp.json` 項目一起刪除。從員工的伺服器清單移除遠端伺服器也一樣。

服務商的授權伺服器中繼資料有 `revocation_endpoint`（RFC 7009）時，Gateway 也會請它撤銷 refresh token（沒有時撤銷 access token）：在本機刪除之後於背景進行，最多 10 秒，服務商緩慢或失敗都不會讓登入資訊留在磁碟上。結果記為稽核 `remote_mcp_token_revocation`（`revoked`、`http_<狀態碼>`、`failed`、`timeout`）。Bearer 權杖與沒有該端點的服務商無法從這裡撤銷，請到服務商帳號撤銷。

### 額外標頭與伺服器串流（選用）

連線對話框的「額外標頭與伺服器串流」：

- **額外標頭**：每行一個 `名稱: 值`，每次呼叫此伺服器（確認、橋接、MCP Events）都會帶上。值存在加密紀錄裡，不會出現在 `.mcp.json`、argv 或環境變數；狀態清單只顯示名稱。留空則沿用已存的標頭。拒絕：DuDuClaw 自己設定的標頭（`Authorization`、`Host`、`Content-Length`、`Content-Type`、`Accept`、`Mcp-Session-Id`、`MCP-Protocol-Version`、`Last-Event-ID`、`Cookie`、`Connection`、`Transfer-Encoding`、`Proxy-` 或 `Sec-` 開頭的名稱等）、不是 HTTP token 的名稱、含控制字元或非 ASCII 的值、超過 16 個。OAuth 中繼資料探索的請求不帶這些標頭。
- **接收伺服器主動傳來的訊息**（預設關）：送出 `notifications/initialized` 後，橋接開啟 Streamable HTTP 的選用 `GET` 串流，把收到的每則訊息交給員工。中斷後重開（1 秒起、加倍到 30 秒）並帶 `Last-Event-ID`；`405`（伺服器沒有此串流）或 `404`（工作階段已失效）就停止。固定解析的 HTTP 用戶端 10 分鐘逾時，所以安靜的串流至少每 10 分鐘重開一次。

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

其他網址（例如區網 IP 的 `http://192.168.1.20:18789`、主機名稱 `http://duduclaw.local:18789`，或不在 `allowed_origins` 的 https 網址）無法直接收到轉回，因為 OAuth 2.1 只允許 loopback 使用非 https 的轉回網址。這時登入分兩步完成：

1. 登入頁會把瀏覽器導向 `http://127.0.0.1:<儀表板連接埠>/oauth/mcp/callback`，也就是「瀏覽器所在電腦」的本機位址（RFC 8252 §7.3，所有 OAuth 2.1 伺服器都接受）。如果瀏覽器就在 Gateway 主機上，這個位址就是 Gateway 本身，登入會自動完成。
2. 在其他電腦上，這一頁會打不開（「無法連上這個網站」），這是正常的：把網址列上的完整網址複製，貼到連線對話框顯示的欄位（RPC `mcp.remote_complete`）。Gateway 只接受 `/oauth/mcp/callback` 上的本機位址，而且主機、連接埠與路徑都必須和這次登入註冊的轉回網址相同；state 只能用一次。貼上的網址只含一次性授權碼與 state，PKCE 驗證碼不會離開 Gateway。

回呼頁由 Gateway 自己在 `/oauth/mcp/callback` 提供，不需要登入（只能用一次的 state 就是防護）。

### Gateway 可以連到哪些位址

所有相關網址（伺服器、它的中繼資料、授權伺服器的各端點）都必須是 `https`，且只能解析到公開網際網路位址（`duduclaw_core::net_addr::is_public_ip`）；解析出的位址在該次連線中固定，POST 不跟隨轉址，中繼資料的 GET 轉址會重新檢查。只有 Gateway 本機上的伺服器（`localhost`、`127.0.0.0/8`、`::1`）可以用 `http`，也只有這時它的中繼資料可以指向 loopback。私有網段（`10.x`、`192.168.x` 等）的伺服器一律拒絕。

### 遮蔽

RFC-23 遮蔽啟用時，橋接項目和其他 stdio server 一樣會被 `duduclaw mcp-proxy` 包起來，遠端工具結果也會遮蔽。（此時 proxy 把第 4 節的工具管制留給橋接，不會問兩次。）

## 3. 代管的應用整合平台：Zapier 與 Composio

Marketplace 分頁有兩張遠端卡片：

| 卡片 | 預設端點（取自 MCP 註冊表） | 登入 |
|------|----------------------------|------|
| Zapier | `https://mcp.zapier.com/api/v1/connect` | OAuth |
| Composio | `https://connect.composio.dev/mcp` | OAuth |

**連線**會開啟已填好端點的遠端對話框；若你的帳號顯示不同的端點請換掉，或服務商給了 API 權杖就改用權杖。DuDuClaw 不附任何帳號或金鑰。

員工透過這些工具送出的所有內容都會經過服務商的雲端，服務商也能操作你在那裡連接的每個應用。請在 Zapier 或 Composio 帳號中只開啟這位員工需要的應用與動作，並盡量每位員工使用各自的連線。

## 4. 第三方工具的動作類型

DuDuClaw 自己的每個工具都有動作類型（`read`、`draft`、`send`、`publish`、`purchase`、`delete`、`modify`、`admin`），員工的 `[capabilities] action_rules` 可依類型或工具允許、詢問或阻擋（見 `docs/features/05-security-defense.md`）。員工其他 MCP 伺服器的工具，在 DuDuClaw 位於其路徑上的地方也會分類：`.mcp.json` 啟動的伺服器經過 `duduclaw mcp-proxy`，遠端伺服器經過遠端橋接。

**分類**依伺服器在 `tools/list` 為每個工具宣告的 `annotations`：

| 標記 | 類型 |
|------|------|
| `destructiveHint: true` | `delete` |
| `readOnlyHint: true`，伺服器列在 `trusted_read_hint_servers` | `read` |
| `readOnlyHint: true`，伺服器未列入 | `modify` |
| 其他情況，包括沒有標記 | `modify` |

標記是伺服器對自己的說法，沒有人查核。`destructiveHint` 來自任何伺服器都採信（只會讓工具更嚴格）；`readOnlyHint` 只對管理員列出的伺服器採信：

```toml
[capabilities]
trusted_read_hint_servers = ["github"]   # .mcp.json 的伺服器名稱
action_rules = [
  { effect = "modify", verdict = "ask" },
  { tool = "github.delete_repository", verdict = "block" },   # 或 "mcp__github__delete_repository"
]
```

預設的代價：惡意伺服器可以把會刪資料的工具標成 `readOnlyHint: true`；若採信，它會通過所有 `read` 規則與唯讀通道。不採信時，未列入的伺服器中真正唯讀的工具會被當成修改（在 `modify` 會被詢問、阻擋或隱藏的地方一樣處理）。員工在伺服器列出工具前就呼叫的工具沒有標記，算 `modify`。

**執行**在 proxy 與橋接中，依員工的 `action_rules`（每次列出與呼叫都重讀，與 DuDuClaw 工具同一套規則；`tool` 規則寫 `<伺服器>.<工具>` 或 `mcp__<伺服器>__<工具>`，`<伺服器>.*` 代表該伺服器所有工具）：

- `block`：從員工看到的 `tools/list` 移除，呼叫以 JSON-RPC 錯誤 `-32003` 回覆，不會送到伺服器（稽核 `third_party_tool_refused`）。
- `ask`：呼叫等待 ApprovalBroker 決定（`mcp_call` 卡片，5 分鐘）；拒絕、逾時或核准系統無法使用時一律拒絕（稽核 `third_party_tool_approval`）。
- **唯讀通道**（`DUDUCLAW_LANE=explore`：心跳主動檢查與 MCP Events 啟動的工作）：只列出、只能呼叫類型為 `read` 的工具，因此未列入伺服器的工具全部隱藏。

沒有 `action_rules` 鍵且在一般通道：一切照舊。遮蔽啟用、員工有 `action_rules` 鍵、或在唯讀通道啟動時，`.mcp.json` 的 stdio 伺服器會經過 proxy；遠端伺服器一律經過橋接。

**儀表板**：`MCP → 遠端伺服器 →「第三方工具與其動作類型」` 依員工顯示每個伺服器最近一次經 proxy 或橋接列出的工具（`<home>/mcp_tool_effects/<員工>/<伺服器>.json`），類型與決定依目前設定重新計算。伺服器要在某次工作階段經 DuDuClaw 列出工具後才會出現。這份快照只用於顯示；管制一律依當下的清單。

**涵蓋的 runtime**：Claude CLI（它啟動 `.mcp.json` 的伺服器，spawn 交給它改寫後的設定）。Codex、Gemini、Antigravity、Grok 員工只登記 DuDuClaw 自己的伺服器，openai-compat 工具迴圈也只啟動 `duduclaw mcp-server`，沒有第三方伺服器可管。`.mcp.json` 中的 `url`／`type` 項目（CLI 直接連線）不涵蓋，請改以遠端伺服器連線。

## 5. 遠端伺服器的事件（MCP Events）

支援草案 MCP Events 擴充的遠端伺服器，可以在事情發生時（新事故、新郵件⋯）通知 Gateway。實作的是 Triggers & Events 工作小組設計草稿（`modelcontextprotocol/experimental-ext-triggers-events` 的 `docs/design-sketch-proposal.md`，2026-02-19 草案）中的 webhook 模式。

**設定**

1. 給 Gateway 一個伺服器連得到的位址：`config.toml [mcp_events] public_base_url = "https://hooks.example.com"`（反向代理或通道接到 Gateway 埠；只接受 `https`，測試時 loopback 位址可用 `http`）。單一訂閱的回呼位址是 `<public_base_url>/webhook/mcp-events/<id>`。
2. 在**遠端伺服器**連線該伺服器（任何登入方式）。
3. 在「遠端伺服器的事件」選擇伺服器、輸入事件名稱後**訂閱**。Gateway 會詢問伺服器（`initialize` 必須宣告 `capabilities.events`）、產生 `whsec_` 簽章金鑰、每個事件名稱呼叫一次 `events/subscribe`（`delivery: { mode: "webhook", url, secret }`，`ttlMs` 一天），並回應伺服器的驗證挑戰。排程（開機後每 10 分鐘）會在每份授權的 `refreshBefore` 之前重新訂閱。

**接收**（`POST /webhook/mcp-events/{id}`，一律掛載）：不認得的 id ⇒ `404`；超過 256 KiB ⇒ `413`；每個訂閱每分鐘超過 120 次 ⇒ `429`；必須有 Standard Webhooks 簽章（`webhook-id`、`webhook-timestamp`、`webhook-signature`，對 `id.timestamp.body` 的 HMAC-SHA256，常數時間比對，接受多個簽章）且時間戳記在 5 分鐘內，否則 `401`；有送 `X-MCP-Subscription-Id` 時必須是伺服器回傳過的 id；重複的 `webhook-id` 回覆成功但丟棄。控制訊息：`verification` 回傳挑戰值、`gap` 記入稽核、`terminated` 結束訂閱（之後的投遞回 `410`）。訂閱沒要求的事件名稱回 `410`（不會重送）。

接受的事件會成為 `events.db` 的 `mcp.event` 列，含訂閱、員工、伺服器、事件名稱與 id、通道、時間與事件的 `data`——經 `input_guard` 掃描（命中時標 `suspicious: true`，不會丟棄），超過 16 KiB 改為截斷文字。它是資料，不是指示：

- **自動化規則**：觸發 `mcp_event`（欄位 `server`、`name`、`agent_id`、`lane`、`suspicious` 與 `data.*`），由它產生的提示開頭有固定的安全提醒。
- **持續責任**：事件來源 `mcp.event`，屬於訂閱的員工。

**預設唯讀。** 事件啟動的工作（自動化規則的 `delegate` 或 `run_skill`）在唯讀通道執行：佇列訊息帶 `lane = "explore"`，Claude CLI 啟動時帶 `DUDUCLAW_LANE=explore`（DuDuClaw 工具只剩 `read`、`draft`）、內建工具只有 `Read`、`Glob`、`Grep`、`WebFetch`、`WebSearch`，第三方伺服器經過管制的 proxy（第 4 節）。這與[唯讀持續任務](continuous-responsibilities.md)是同一條通道、同一個旗標，規則也相同：OpenAI 相容執行環境的員工可以執行，DuDuClaw 工具同樣受限，且不掛載 `agent.toml [mcp.external]` 伺服器；使用其他 runtime 或任務沙箱的員工在開始前就被拒絕（`explore_lane_unsupported`），MoA 模型與純本機推論也被拒絕（混合模式的本機分流略過）。訂閱時開啟「允許一般模式」，事件就能以員工平常的權限啟動工作；只有這種訂閱會喚醒持續責任（occurrence 是一般的目標任務），唯讀訂閱的事件在那裡記為 `dropped(explore_lane)`。

**金鑰**：以 Gateway 金鑰檔加密存在 `<home>/mcp_events/subscriptions.json`（0600）。「更換簽章金鑰」會在更新訂閱時把新的送給伺服器，舊的 15 分鐘內仍接受。「取消訂閱」先刪除本機訂閱（回呼位址立即回 `404`），再盡力呼叫 `events/unsubscribe`。稽核：`mcp_event_subscription_created`／`_rotated`／`_revoked`／`_refresh_failed`、`mcp_event_delivered`、`mcp_event_delivery_rejected`、`mcp_event_control`（只有 id、名稱與數量）。

已驗證與假設：只讀得到設計草稿（OpenAI 關於 ChatGPT 支援的頁面無法取得）。未實作：poll 與 push 投遞、cursor 與重播（訂閱一律從「現在」開始）、`deliveryStatus`、`maxAgeMs`、`events/list`、訂閱 `arguments`（一律 `{}`）與選用的 `v1a` 伺服器簽章。只對本機假伺服器測試過。

## RPC 一覽

| 方法 | 誰可以用 | 用途 |
|------|---------|------|
| `mcp.registry_search { query, cursor? }` | 已登入 | 搜尋（固定主機、快取） |
| `mcp.registry_install { name, version?, agent_id, remote?, server_name?, env? }` | 已登入（非管理者 ⇒ 安裝申請） | 走有掃描的安裝流程 |
| `mcp.remote_connect { agent_id, name, url?, auth, bearer?, redirect_origin?, client_id?, client_secret?, headers?, server_stream? }` | 管理者 | 連線；`oauth` 回傳 `authorize_url`；`headers` `{名稱: 值}` 取代已存的標頭 |
| `mcp.remote_status { agent_id? }` | 管理者 | 不含秘密的紀錄（`header_names`、`server_stream`） |
| `mcp.remote_disconnect { agent_id, name, forget? }` | 管理者 | 刪除登入資訊（`forget` 連項目一起移除），可行時再到服務商撤銷 |
| `mcp.tool_effects { agent_id }` | 管理者 | 最近列出的第三方工具與類型、決定 |
| `mcp.events_subscribe { agent_id, server, event_types, mode? }` | 管理者 | 訂閱（`mode` 預設 `explore`，或 `normal`） |
| `mcp.events_list { agent_id? }` | 管理者 | 不含秘密的訂閱 |
| `mcp.events_unsubscribe { id }` | 管理者 | 刪除後向伺服器取消訂閱 |
| `mcp.events_rotate { id }` | 管理者 | 更換簽章金鑰 |

稽核事件（`security_audit.jsonl`）：`remote_mcp_connect_started`、`remote_mcp_connected`、`remote_mcp_connect_failed`、`remote_mcp_disconnected`、`remote_mcp_token_revocation`（員工、伺服器、主機、登入方式；不含網址路徑或權杖），以及第 4、5 節的事件。`duduclaw doctor` 的「員工 MCP 設定中的其他伺服器」一列會標出橋接項目的主機與連線狀態。

## 未涵蓋／未驗證

- 沒有用真的 Zapier、Composio 帳號或真的第三方 OAuth 服務商測試；流程只對本機的假授權伺服器與假 MCP 伺服器驗證過。
- 橋接不會續接回應 POST 的中斷事件串流；伺服器主動推送的 `GET` 串流需逐一開啟，只對本機假伺服器測試過。
- 舊版 HTTP+SSE 伺服器仍走 `npx mcp-remote`（見上方）。註冊表中宣告必填標頭的項目仍顯示為無法安裝，請改用網址連線並填「額外標頭」。
- 只有授權伺服器宣告 `revocation_endpoint` 時才會到服務商撤銷，且為盡力而為。
- 第三方工具分類依第 4 節所述採信伺服器的標記；儀表板讀的快照檔由員工自己的行程寫入。
- MCP Events：見第 5 節結尾；Gateway 必須能以伺服器接受的 https 位址連到。
- 桌面版的儀表板來源（`tauri://…`）不是可接受的轉回來源；請用瀏覽器登入。
- 員工與 Gateway 是同一個系統使用者：擁有不受限 Bash 的員工可以讀取金鑰檔與紀錄檔，或自行以自己的 `--agent` 啟動橋接。橋接會拒絕與行程 `DUDUCLAW_AGENT_ID` 不同的 `--agent`，但真正的隔離是不要開放 Bash。
- 進行中的登入只存在 Gateway 記憶體：登入途中重啟 Gateway 就要重新開始。
