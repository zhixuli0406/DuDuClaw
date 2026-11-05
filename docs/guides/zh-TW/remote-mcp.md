# Remote MCP：讓 claude.ai／任何 MCP 客戶端直連你的 DuDuClaw

`duduclaw http-server` 提供標準的 **MCP Streamable HTTP** 端點（`POST /mcp`）與完整的
**OAuth 2.1** 授權流程。這代表 claude.ai 的自訂連接器（Custom Connector）、Claude
行動版、MCP Inspector，或任何支援 remote MCP server 的應用，都可以直接連上你自架的
DuDuClaw，使用記憶、知識庫等工具面。

## 快速開始

```bash
# 1. 啟動 HTTP server（預設只綁 loopback）
duduclaw http-server --bind 127.0.0.1:8765

# 2. 需要對外時開一條通道（或用你自己的反向代理/網域）
duduclaw tunnel          # Cloudflare quick tunnel，畫面會給你 https 網址

# 3. 停止第一步的 HTTP server，再以通道的 HTTPS 網址重新啟動
duduclaw http-server --bind 127.0.0.1:8765 \
  --public-origin https://mcp.example.com --trusted-proxy 127.0.0.1
```

拿到對外網址後，在 claude.ai「設定 → 連接器 → 新增自訂連接器」貼上：

```
https://<你的網址>/mcp
```

claude.ai 會自動走 OAuth 探索（RFC 9728 → RFC 8414 → 動態註冊），把你導到
DuDuClaw 的授權頁。頁面上貼一把**內部 MCP API key**（`config.toml [mcp_keys]` 中
`is_external = false` 的 key）按「同意連線」即完成。

## 固定 origin 與代理設定

對外使用 OAuth 時，請以實際固定的 HTTPS origin 啟動或重新啟動伺服器：

```bash
duduclaw http-server --bind 127.0.0.1:8765 \
  --public-origin https://mcp.example.com --trusted-proxy 127.0.0.1
```

請換成自己的網域與代理 IP。quick tunnel 須先取得網址，再以該網址設定 `--public-origin` 並重啟 `http-server`；網址變更後須重新授權，正式使用建議固定網域。

| CLI 參數 | 預設與行為 |
| --- | --- |
| `--public-origin <https-origin>` | 預設未設定，使用 `http://` 加實際綁定的 socket 位址。設定時只接受 HTTPS origin，不得含帳密、query、fragment 或 `/` 以外的路徑。issuer、resource、探索、token 與 revoke 網址全部使用此值，`Host`／`X-Forwarded-Proto` 不會改變它。 |
| `--trusted-proxy <IP>` | 預設空白，可重複指定每台可信代理的確切 IP。TCP 對端在清單內才採用 `X-Forwarded-For`，由右往左跳過可信代理，取第一個不在清單的 IP；格式錯誤則使用 TCP 對端。代理必須覆寫或附加實際連線 IP，並防止直接連線冒充可信節點。 |
| `--allow-insecure-lan` | 預設關閉，明確允許非 loopback 明文監聽，啟動時顯示醒目警告。僅供測試：操作者金鑰與權杖可能遭攔截。此參數不會啟用 TLS 或信任轉送標頭。 |

非 loopback 綁定必須同時設定 HTTPS `--public-origin` 及至少一個可信代理，或明確指定不安全模式，否則啟動前拒絕。未指定 `--allow-insecure-lan` 時，非 loopback 監聽器會在讀取 body、驗證憑證及扣配額前，以 HTTP 403 拒絕 TCP 對端不在可信代理清單或缺少對端資訊的所有請求，包含健康檢查與未知路由；轉送標頭無法繞過此檢查。Loopback 綁定仍支援本機代理連線。監聽器本身仍是 HTTP；由可信代理終結 TLS，代理到後端仍為明文，須保護這段網路。這些是 CLI 參數，沒有新增 `config.toml` 鍵或環境變數。

不安全的區網測試請綁定實際 LAN IP，例如 `--bind 192.168.1.10:8765 --allow-insecure-lan`；`0.0.0.0` 等萬用位址不是客戶端可使用的 canonical 位址。

授權與權杖請求接受 RFC 8707 `resource`，必須完全等於 `<canonical-origin>/mcp`。省略時只預設為此 URI；空字串、其他位址或互相衝突的重複參數都回傳 `invalid_target`。同意、授權碼交換、刷新及存取驗證均維持同一 audience。變更 origin 後須重新連線；舊儲存資料缺少 audience 的授權碼及權杖會遭拒絕。

動態註冊的每 IP 配額會計算所有嘗試，每個解析後的來源 IP 每分鐘 3 次。獨立的全域配額為每分鐘 10 次，只在 JSON／metadata 驗證及容量檢查通過、即將建立客戶端時扣除；無效請求與容量不足不會消耗全域配額。客戶端上限維持 200 個：滿額時可移除建立至少一小時、從未授權且最舊的註冊紀錄。仍有有效授權碼、授權、存取或刷新權杖的客戶端會保留；曾經授權或仍在寬限期內的紀錄也不會因滿額而被移除。找不到候選者就拒絕註冊。一般到期時間仍為未授權者 24 小時、曾授權者閒置 90 天。成功同意或使用權杖會延長活動時間，未認證請求不會。每次儲存變更都會清理；到期或被回收後須重新註冊。限流計數屬於單一程序，重啟歸零，多伺服器部署須在代理另設合計限制。

## 從 ChatGPT 使用

ChatGPT 可以把 DuDuClaw 加成自訂連接器（開發者模式下的「應用程式」）。不會發布到任何商店，連接器只存在你自己的 ChatGPT 帳號裡。

1. 讓 `duduclaw http-server` 有一個固定的 **https** 位址（反向代理或具名通道；ChatGPT 從 OpenAI 的伺服器連線，loopback 或區網位址不行）。
2. 在 ChatGPT 開啟開發者模式（設定 → 應用程式與連接器 → 進階設定；哪些方案提供由 OpenAI 決定），以伺服器網址 `https://<你的位址>/mcp`、驗證方式 OAuth 建立連接器。
3. ChatGPT 會找到授權伺服器（RFC 9728 → RFC 8414）並自行註冊：透過動態用戶端註冊（`POST /oauth/register`），或用 Client ID Metadata Document（`client_id` 是 DuDuClaw 會去讀取的 https 網址；2026-10-08 起支援，見下方）。依第三方指南，它的轉回網址是 `https://chatgpt.com/connector_platform_oauth_redirect`；DuDuClaw 接受任何 https 轉回網址，不必另外設定。
4. DuDuClaw 的授權頁出現後，貼上一把**內部** MCP API key 並同意。

ChatGPT 拿到的權限與所有 OAuth 用戶端相同（見下方「外部用戶端」等級）：基礎工具，加上最多 `memory:read`、`memory:write`、`wiki:read`、`wiki:write`、`messaging:send`。連接器、執行、人員名冊與 Admin 工具永遠不會經 OAuth 開放。

Client ID Metadata Document：`client_id` 是帶路徑的 `https://` 網址且不是已註冊的 id 時，DuDuClaw 會去讀取它（只連公開位址、固定解析、不跟隨轉址、5 秒、5 KiB、快取一小時）。文件中的 `client_id` 必須與網址相同，`redirect_uris` 必須是 https（或 loopback http）網址且包含實際使用的那個，只接受公開用戶端（`token_endpoint_auth_method` 不存在或為 `none`）。授權伺服器中繼資料會宣告 `client_id_metadata_document_supported: true`。

CIMD 未命中快取時，每個解析後的來源 IP 每分鐘最多嘗試 12 次，全程序同時最多 4 個抓取工作。等待名額超過 100 毫秒會回傳 HTTP 503 `temporarily_unavailable`。抓取失敗或等待超過 5 秒的結果，按 client_id 網址快取 30 秒；成功與失敗共用最多 64 筆的快取。5 秒等待包含 DNS 解析。請求逾時或取消後，背景工作仍占用名額，直到實際 DNS／網路工作結束，避免遺留工作突破併發上限。命中快取不消耗抓取嘗試配額。

未驗證：沒有用真的 ChatGPT 帳號測試。轉回網址與註冊方式取自第三方連接器指南（撰寫時無法取得 OpenAI 自己的頁面）；流程只用本機請求測試過。

## 授權模型（重要）

OAuth 簽發的存取權杖**永遠是「外部客戶端」等級**，與對外工具面的 scope 政策共用同
一條規則，沒有第二套：

- 基礎工具面（7 個基礎工具）恆可用。
- 連接器請求的 scope 會被收斂到**可對外授與的白名單**（`memory:read` /
  `memory:write` / `wiki:read` / `wiki:write` / `messaging:send`）；
  連接器類（Odoo/Google/Notion）、執行類、人員名冊與 Admin **永不透過 OAuth 開放**，
  就算客戶端聲稱要求也一樣。
- 同意頁貼的必須是內部 key（外部 key 不能自我升級）。

權杖細節：存取權杖 1 小時、refresh token 30 天且每次使用即輪替；授權碼單次有效
10 分鐘；PKCE S256 必須。所有權杖落盤只存 SHA-256 雜湊（`~/.duduclaw/mcp_oauth_issued.json`，0600）。

## 重新驗證與撤銷連線（v1.80.0）

授權時使用的內部 MCP 金鑰必須持續有效。交換授權碼、刷新權杖及每次驗證 access token 都會重查該金鑰。金鑰被移除、過期、設定變更（包含 scopes），或設定無法讀取時，授權失效；刷新回傳 `invalid_grant`，並清除該次授權的整個權杖家族，包含 access token。同意授權不能超出內部金鑰權限；刷新只能維持或縮小原有 scopes。重複使用已輪替的 refresh token 也會使整個家族失效。

管理員可在「管理 → 整合 → MCP API 金鑰」查看有效 OAuth 授權的應用程式名稱、權限、授權時間及最後使用時間，並撤銷指定授權。對應的 `mcp_oauth_grants.list`、`mcp_oauth_grants.revoke` RPC 僅限 Admin。清單不回傳憑證或其雜湊。撤銷會阻止後續請求，不會取消已在執行中的工具呼叫。

客戶端可透過 HTTPS 呼叫 `POST /oauth/revoke`，以 `application/x-www-form-urlencoded` 傳入 `client_id`、`token` 及選填的 `token_type_hint`。僅支援 public client（驗證方式 `none`，與 token endpoint 相同），並核對權杖所屬客戶端。撤銷 access token 或 refresh token 都會撤銷整個家族。不認識的權杖或其他客戶端的權杖回傳 HTTP 200，不會更動他人的授權；未提供 client ID 或 token 則拒絕。伺服器可忽略 hint。探索文件會公布 `revocation_endpoint` 與 `revocation_endpoint_auth_methods_supported`。規格見 [RFC 7009](https://www.rfc-editor.org/rfc/rfc7009)。

**升級注意：**舊版缺少同意金鑰與家族關聯的 OAuth 授權碼、權杖一律拒絕，應用程式須重新連線授權。共用儲存層會鎖住完整的讀取、修改與寫入操作，並行刷新、註冊及儀表板撤銷不會把已撤銷的授權寫回；儲存檔損壞或無法讀取時拒絕操作。

`duduclaw mcp issue-refresh-token` 簽發的 `ddc_refresh_*` 是另一種**僅供 stdio 使用**的憑證，搭配 `duduclaw mcp-server`。發行指令會顯示警告，HTTP Bearer 與 SSE 查詢參數驗證會拒絕並說明用途。`http-server` 請使用靜態 MCP API 金鑰或 OAuth，沿用現有驗證入口。CLI 刷新憑證不能送到 `/oauth/token` 交換；stdio 仍會逐次檢查權限、到期與撤銷狀態。

驗證範圍：尚未做實際瀏覽器驗收。

OAuth 每組授權最多保留 2,048 筆存取／刷新紀錄（含已使用的刷新權杖摘要），全域最多 16,384 筆權杖、1,024 組授權，儲存上限為 16 MiB。達上限時拒絕發行，不消耗目前的刷新權杖；已使用摘要保留至到期，維持重播偵測與舊權杖撤銷能力。匿名請求依 IP 每分鐘限制 120 次。有效 Bearer 請求使用獨立且有容量上限的身分額度（每分鐘 60 次），再套用原有客戶端限制；匿名額度耗盡不會消耗已認證身分的額度。IP 依上方可信代理政策解析；未設定時，代理後方匿名客戶端共用 TCP 對端額度。同時進行的認證工作也有數量上限。 配額分類使用每秒最多更新一次的有界摘要快取，不作授權依據。新憑證最多等待一秒才會取得獨立額度；受保護請求仍即時重驗，舊快取不能繞過撤銷。靜態金鑰分類快取最多讀取 1 MiB 設定及 4,096 筆金鑰，超過時仍可經匿名額度進入正常認證。權杖交換依「client_id、解析後的來源 IP」組合，每分鐘限制 12 次，另有每 IP 每分鐘 120 次上限，不因宣告不同 client_id 或 Bearer 分類而跳過。同一公開 client_id 的其他來源不會因攻擊者送出無效交換而被扣除配額。限流表與背景儲存工作數均有上限。存取驗證只讀取資料；通過限流後，背景儲存工作每分鐘最多更新一次最後使用時間。授權清單遇到設定錯誤會回報無法使用，不會顯示為空清單。儀表板撤銷會記錄操作者、授權組別、應用程式、權限及結果，不記錄憑證或摘要；稽核寫入失敗會明確回報，即使撤銷已生效。

## 端點總覽

| 路徑 | 用途 |
|---|---|
| `POST /mcp` | 標準 MCP 端點：`server/discover`／`tools/list`／`tools/call`／`subscriptions/listen`（2026-07-28），`initialize`／`ping`（舊世代）。見[協定版本](#協定版本) |
| `GET /.well-known/oauth-protected-resource` | RFC 9728 資源中繼資料（401 的 `WWW-Authenticate` 會指向這裡） |
| `GET /.well-known/oauth-authorization-server` | RFC 8414 授權伺服器中繼資料 |
| `POST /oauth/register` | RFC 7591 動態客戶端註冊（public client） |
| `GET /oauth/authorize` → `POST /oauth/decision` | 授權碼流程＋操作者同意頁 |
| `POST /oauth/token` | 換發／刷新權杖 |
| `POST /oauth/revoke` | 撤銷權杖家族（RFC 7009、public client） |
| `POST /mcp/v1/call`、`GET /mcp/v1/stream` | 既有的 DuDuClaw REST/SSE 面（不變） |

靜態 Bearer key（`ddc_…`）與 OAuth 權杖（`ddc_oauth_…`）走同一個
`Authorization: Bearer` 表面；腳本／自家整合繼續用靜態 key 即可，OAuth 是給
「只會說 OAuth」的客戶端（claude.ai）用的。

## 協定版本

`POST /mcp` 與 stdio 的 `duduclaw mcp-server` 都同時支援 MCP 的兩個世代：

- **新世代 2026-07-28**：沒有 `initialize`，每個請求在 `params._meta` 帶版本與用戶端能力（`io.modelcontextprotocol/protocolVersion`、`io.modelcontextprotocol/clientCapabilities`）。支援 `server/discover`、`tools/list`、`tools/call`、`subscriptions/listen`。走 HTTP 時每個請求還要帶 `MCP-Protocol-Version` 與 `Mcp-Method` 標頭，`tools/call` 另外要 `Mcp-Name`；標頭缺漏或與內容不符回 `400` 加 `-32020`，不支援的版本回 `-32022` 並列出支援的版本。
- **舊世代 2024-11-05 到 2025-11-25**：用 `initialize` 協商，要哪個版本就回哪個，不認得的版本回 2025-11-25。`ping` 照常回應。

工具清單變動時：stdio 的舊世代用戶端照舊收到 `notifications/tools/list_changed`；新世代用戶端要開 `subscriptions/listen` 並帶 `toolsListChanged: true` 才會收到。HTTP 端點沒有狀態，無從得知呼叫者的工具變了，所以收到訂閱時回確認但不承諾任何通知類型，接著直接結束訂閱；HTTP 用戶端在 `tools/list` 的 `ttlMs`（5 秒）到期後重新讀取即可。

2026-10-16 用官方 TypeScript 用戶端 `@modelcontextprotocol/client` 2.3.1 實測：固定 2026-07-28、自動、舊世代三種模式，stdio 與 HTTP 都通過。

## 安全備註

- 瀏覽器來源（`Origin` 標頭）比對 loopback＋`config.toml [gateway] allowed_origins`
  白名單（錨定比對），其餘一律 403；非瀏覽器客戶端不受影響。
- quick tunnel 的網址每次啟動會變——正式對外請用固定網域（反向代理或 Cloudflare
  named tunnel），並以 `--public-origin` 設定該固定網址。
