# MCP Bridge——掛載外部 MCP 伺服器

DuDuClaw 可以把第三方 [Model Context Protocol](https://modelcontextprotocol.io) 伺服器掛載在自己內建的 MCP server 旁邊，讓 agent 的工具迴圈直接拿到外部伺服器的工具，不必手刻一支 Rust 連接器。Plane、Chatwoot、Invoice Ninja、Gmail/Calendar、WooCommerce，以及其他任何 MCP server，都是這樣接進 agent 的。

## 設定生效範圍與啟動結果

| 設定 | 讀取它的執行環境 | 儀表板 |
|---|---|---|
| 員工的 `.mcp.json` | Claude、Codex、Grok，以及內建工具迴圈（OpenAI 相容、Direct API、本機推論） | 管理員可編輯；`source: "mcp_json"` |
| 員工 `agent.toml` 的 `[[mcp.external]]` | 僅內建工具迴圈 | 唯讀；`source: "agent_toml"` |

Claude、Codex、Grok 不讀取 `[[mcp.external]]`。MCP 頁面會在每筆項目顯示適用環境；
`mcp.list` 以 `effective_runtimes` 回傳 `claude`、`codex`、`grok`、`in_process`。
這表示設定的適用範圍，不保證已連線。停用的 TOML 項目仍維持停用；兩種格式之間不會
自動搬移設定或憑證。同一伺服器出現在兩種格式時，內建工具迴圈優先使用 TOML 項目。

管理員透過 `mcp.update` 新增或編輯 stdio 項目後，系統先儲存、清除前次啟動結果，
再執行與 MCP 頁面「立即檢查」相同的檢查。該操作呼叫僅限管理員的 `mcp.probe`，
參數為 `{agent_id, server_name}`，回傳 `{ok, tool_count, reason_code}`；儲存的回應則
將結果放在 `probe`。檢查在記憶體中驗證並產生有效設定，不修補 `.mcp.json`，也不在
員工目錄下寫入任何檔案。唯一的設定檔是權限 0600 的暫存 proxy 設定，成功、失敗或
逾時後都會移除。檢查送出 `initialize` 與 `tools/list`，期限 10 秒，結束後清理程序，
不呼叫上游工具。失敗仍保留儲存的設定；成功只代表能啟動並取得可見工具，不保證工具
呼叫權限或遠端服務健康。整個 gateway 同時最多執行 4 個檢查，等候名額最多 2 秒；
同一員工／伺服器已有檢查，或等候名額逾時，回傳 `busy`，請稍後重試。

proxy 的身分驗證、檢查順序與拒絕優先行為不變。proxy 不將啟動結果寫入共用檔案；
只對自己的 stdio 對端第一個 JSON-RPC 請求回傳 `-32001`、固定短訊息與
`error.data.reason_code`，並向自己的 stderr 印出固定格式的原因碼。
`identity_rejected` 包含既有驗證器拒絕的情況，例如缺少／無效憑證或無法讀取政策／
金鑰；診斷不會再次驗證來猜測更細的原因。啟動結果僅由 gateway 記錄：檢查從代理的
錯誤框取得原因碼；gateway 啟動的 Claude `system/init` 事件只提供已連線／未連線
（`connection_failed`）。觀察器僅接受該次凍結設定中的精確伺服器名稱，限制事件、
名稱與狀態長度，最多處理 64 筆、去除重複，每個事件最多寫一筆失敗摘要。檢查失敗
也可由 gateway 透過有鎖且有容量限制的 helper 寫入一筆 `channel_failures.jsonl`
`mcp_start` 異常紀錄，不含權杖、環境值或上游參數。未連線不會中斷該回合。

`mcp.list` 的 `last_start` 僅讀取 gateway 的有限記憶體 map，滿額時淘汰最舊項目，
不從 JSONL 取得狀態。頁面顯示翻譯後的原因與時間；執行時顯示「未連線」後，管理員
可按「立即檢查」取得精確原因。更新／移除伺服器或刪除員工會清除前次結果；gateway
重啟後不保留狀態。沒有結果時不推測狀態，這不是持續健康監控。

## 第三方工具檢查（v1.80.0 起）

`[[mcp.external]]` 的 stdio 與 Streamable HTTP 掛載都採用與受管理 MCP 代理相同的 `ThirdPartyGate`，依工具宣告判定影響類別。伺服器的工具允許／拒絕清單仍會限縮工具，員工的名稱限制、動作規則與審批要求也會套用到清單與呼叫。annotations 只能將預設 Modify 提高為 Delete，唯讀宣稱即使搭配舊信任設定也不得降低風險；遭拒的工具不會提供給模型。

員工 `.mcp.json` 中直接指定 URL/type 的項目會由受管理的啟動路徑拒絕，請改用受管理的遠端 MCP 連線。`[[mcp.external]]` 本身的 HTTP `url` 掛載仍受支援。詳見[擴充功能邊界](../../architecture/extension-gates.md)。

## MCP Bridge 還是原生連接器

- **MCP Bridge**（本頁）：這套 SaaS 本身（或社群）已經有現成的 MCP server，你只要用設定檔掛上去，不必寫任何程式碼。
- **原生連接器**（例如 `duduclaw-odoo`、`duduclaw-erpnext`）：沒有可用的 MCP server，或者你需要更深的憑證隔離、版本閘控、稽核歸屬，這些是通用掛載給不了的。

## 設定方式

在 agent 的 `agent.toml` 裡加入一個或多個 `[[mcp.external]]` 表格：

```toml
[[mcp.external]]
name = "chatwoot"
command = "npx"
args = ["-y", "@chatwoot/mcp-server-chatwoot"]
enabled = true                       # 選填，預設 true
# env 的值可以是字面值；`env://VAR` 表示從 gateway 行程的環境變數讀取；
# 或 `secret://<backend>/<name>` 表示在啟動子行程時從設定好的密鑰管理器讀取
# （密鑰不要直接寫進 agent.toml）。
env = { CHATWOOT_BASE_URL = "https://app.chatwoot.com", CHATWOOT_API_TOKEN = "secret://vault/chatwoot_token" }
# 工具可見度（皆選填）：
allowed_tools = ["chatwoot_list_conversations", "chatwoot_get_conversation"]  # 允許清單 = deny-by-default
denied_tools  = ["chatwoot_delete_conversation"]                             # 一律移除
```

講 **MCP Streamable HTTP**（不需要本機行程）的遠端伺服器，用 `url` 取代 `command` 掛載。若是 DuDuClaw 已經認得的廠商，直接用 **`preset`**：端點與憑證來源都幫你填好了（端點網址放在 DuDuClaw 裡，廠商改名只要改一處程式碼，不必動使用者的設定）：

```toml
[[mcp.external]]
preset = "google:gmail"    # gmail|calendar|drive|docs|sheets|slides|chat
allowed_tools = ["search_threads", "get_thread", "create_draft"]
```

`preset = "google:<svc>"` 會展開成 Google Workspace 官方 MCP 端點，並帶上 `bearer_token = "oauth://google"`（也就是 dashboard 已連接的 Google 帳號，token 會自動更新）。

> **Google Workspace 建議優先用原生工具。** 八個服務（Gmail／Calendar／Sheets／Drive／Docs／Slides／Forms／Tasks）都已經用 GA API 做成原生 MCP 工具，詳見 [google-workspace.md](../google-workspace.md)。官方 MCP server 目前仍是 Developer Preview，條款規定不得把 Pre-GA API 開放給自家網域以外的使用者，所以只適合當進階的自架選項，不是可出貨的路徑（[google-mcp.md](google-mcp.md) 說明了這個取捨）。

不用 preset、手動展開的話長這樣，這也是掛載任何沒有 preset 的伺服器的寫法（例如自架 DocuSeal 實例內建的 `/mcp` 端點）：

```toml
[[mcp.external]]
name = "gmail"
url = "https://gmailmcp.googleapis.com/mcp/v1"
# bearer_token：可以是字面值、env://VAR、secret://<backend>/<name>，
# 或 oauth://google（沿用 dashboard 已連接的 Google 帳號 token，
# 會自動更新）。會放進 `Authorization: Bearer <token>` 標頭送出。
bearer_token = "oauth://google"
# headers = { X-Custom = "env://MY_HEADER" }   # 選填，額外的標頭
allowed_tools = ["search_threads", "get_thread", "create_draft"]
```

欄位參考：

| 欄位 | 是否必填 | 說明 |
|---|---|---|
| `name` | 建議填 | 記錄在 log 裡的標籤（`preset` 會自帶標籤） |
| `preset` | 否 | 內建的廠商簡寫（`google:gmail`、`google:calendar`、`google:drive`、`google:docs`、`google:sheets`、`google:slides`、`google:chat`），會提供 `url` 加上預設的 `bearer_token`。未知的 preset ⇒ 該伺服器被跳過；`preset` 與 `url` 同時給 ⇒ 語意不明，一樣跳過 |
| `command` | command/url/preset 三選一 | stdio 傳輸：要啟動的執行檔（`npx`、`node`、`python`、絕對路徑……） |
| `url` | command/url/preset 三選一 | Streamable-HTTP 傳輸：遠端 MCP 端點（只接受 `https://`；同時給或都沒給 `command`/`url` 的條目會被跳過） |
| `args` | 否 | 參數陣列（僅 stdio） |
| `env` | 否 | 子行程環境變數（僅 stdio）。`env://VAR` 從 gateway 的環境變數讀取；`secret://<backend>/<name>` 從密鑰管理器讀取（見下文）；`env://`／`secret://` 憑證缺失或解析不出來時，**整台伺服器都會被停用**（fail-safe：沒有 token 的伺服器可能行為異常，乾脆不啟動） |
| `bearer_token` | 否 | HTTP 認證：字面值、`env://VAR`、`secret://…`，或 `oauth://google`；會放進 `Authorization: Bearer …` 標頭送出。解析不出來 ⇒ 該伺服器被跳過 |
| `headers` | 否 | 額外的 HTTP 請求標頭；值支援 `env://` 與 `secret://` |
| `enabled` | 否（預設 true） | 設為 false 可以保留設定但不掛載 |
| `allowed_tools` | 否 | 設定後只會開放清單內的工具（deny-by-default） |
| `denied_tools` | 否 | 一律移除，即使在允許清單內也一樣 |

## 運作語意與安全機制

- **內建的 duduclaw MCP server 永遠是 client 0**，工具名稱撞名時內建工具優先（外部重複的那個會被丟棄並留下 log）。
- 每個外部伺服器都是獨立啟動的；哪一個**連不上就跳過那個**，內建伺服器與其他外部伺服器照常運作。如果合併後的 `tools/list` 整組失敗，工具註冊表會降級成**只剩內建工具**，不會整組工具都不見。
- `allowed_tools` 是 **deny-by-default**：設了允許清單，沒列進去的工具就會被隱藏。可以搭配 `denied_tools` 再點名擋掉特定危險工具。
- 寫入型／不可逆的工具（計費、刪除）還要另外列進 agent 的 `[capabilities] approval_required_tools`，讓它們走 HITL 的 `ApprovalBroker`。工具註冊表的閘門會同時檢查清單與執行；隱藏工具並非唯一的授權檢查。

## 活體驗證流程

設定檔解析器與工具過濾器都有單元測試覆蓋
（`crates/duduclaw-gateway/src/mcp_external.rs`、
`crates/duduclaw-llm/src/mcp_client.rs`）。但要驗證一次真正的端到端掛載，
需要一台連得到的 MCP server：

1. 挑一台可以本機執行的伺服器，例如官方的參考實作 everything-server：
   ```bash
   # 在一個暫用目錄裡，確認它真的用 stdio 講 MCP
   npx -y @modelcontextprotocol/server-everything
   ```
2. 把它加進測試用 agent 的 `agent.toml`：
   ```toml
   [[mcp.external]]
   name = "everything"
   command = "npx"
   args = ["-y", "@modelcontextprotocol/server-everything"]
   allowed_tools = ["echo"]   # 證明允許清單把其他工具都藏起來了
   ```
3. 啟動 gateway，傳一則會用到 `echo` 工具的訊息給 agent。到 log 裡確認：
   - 出現 `external MCP server mounted`，`server=everything`
   - agent 叫得動 `echo`，但叫不動該伺服器的其他工具（allowlist 生效）。
4. 暫時把某個 `env://` 憑證指向一個沒設定的環境變數再重啟，確認伺服器被跳過，
   log 出現 `external MCP env credential unresolved … skipping server`。

預期結果：agent 只拿到允許清單內的外部工具；壞掉的外部伺服器不會拖垮內建工具面。

## 用 `secret://` 解析憑證

`env` 的值除了字面值，或是 `env://` 這種讀取行程環境變數的寫法之外，也可以指向密鑰管理器。DuDuClaw 在啟動子行程時，會把 `secret://<backend>/<name>` 拿去比對 `~/.duduclaw/config.toml` 裡的 `[secret_manager]` 設定；解析不出來的話，整台伺服器就不掛載（fail-safe，跟 `env://` 缺失時的行為一樣）。

支援的 backend：`local`（AES 加密儲存）、`vault`（HashiCorp Vault KV v2）、`env`、`onepassword`（1Password Connect）、`infisical`。範例：

```toml
# config.toml
[secret_manager]
backend = "vault"
vault_addr  = "https://vault.internal:8200"
vault_token_enc = "…"          # keyfile 加密；正式環境絕不可用明文

# agent.toml
[[mcp.external]]
name = "chatwoot"
command = "npx"
args = ["-y", "@chatwoot/mcp-server-chatwoot"]
env = { CHATWOOT_BASE_URL = "https://app.chatwoot.com", CHATWOOT_API_TOKEN = "secret://vault/chatwoot_token" }
```

完整的 `[secret_manager]` 欄位清單（含 1Password／Infisical）請看 `crates/duduclaw-security/src/secret_manager/mod.rs` 的模組文件。

## 常見 SaaS 伺服器範例

每個範例都是一段 `agent.toml` 設定，加上要準備的憑證。掛上去、重啟 agent，照著[活體驗證流程](#活體驗證流程)走一遍即可。
**寫入型／不可逆的工具標了 ⚠，記得列進 agent 的 `[capabilities] approval_required_tools`，讓它們走 HITL 審批。**

> 狀態：以下都是 **PENDING-LIVE**（待活測）：設定格式與解析邏輯都測過了，但要真正端到端掛上去，還需要對應的 SaaS 帳號。伺服器名稱是 2026-07 當下的生態現況，使用前請自行確認套件名稱／端點是否還正確。

### Gmail／Google Calendar（Google 官方 remote MCP）

```toml
[[mcp.external]]
name = "gmail"
command = "npx"
args = ["-y", "@google/gmail-mcp"]     # 請確認目前的官方套件名稱
env = { GOOGLE_OAUTH_TOKEN = "secret://vault/google_oauth" }
allowed_tools = ["gmail_search", "gmail_get_thread", "gmail_create_draft"]  # 只開讀取與草稿
denied_tools  = ["gmail_send"]         # ⚠ 寄信要卡審批，不能自動寄
```
準備：一個 Google Cloud OAuth app；跑一次 OAuth flow 換出 token。
`gmail_send` ⚠ → 放進 `approval_required_tools`。

### Plane（官方 `plane-mcp-server`，成熟穩定）

```toml
[[mcp.external]]
name = "plane"
command = "npx"
args = ["-y", "@makeplane/plane-mcp-server"]
env = { PLANE_API_KEY = "secret://vault/plane_api_key", PLANE_WORKSPACE_SLUG = "my-workspace" }
allowed_tools = ["plane_list_issues", "plane_get_issue", "plane_create_issue"]
denied_tools  = ["plane_delete_issue"]  # ⚠
```
選用：可以另外接一支單向同步 worker，把 Plane 的 issue 拉進 Task Board（見 IMPL-PLAN §E）。準備：一組 Plane API key，加上 workspace slug。

### Invoice Ninja（社群版 `Fuciuss/invoice-ninja-mcp`）

```toml
[[mcp.external]]
name = "invoice-ninja"
command = "npx"
args = ["-y", "invoice-ninja-mcp"]      # 請確認套件名稱
env = { INVOICE_NINJA_URL = "https://invoicing.example.com", INVOICE_NINJA_TOKEN = "secret://vault/invoiceninja_token" }
allowed_tools = ["in_list_invoices", "in_get_invoice", "in_create_invoice", "in_record_payment"]
```
**錢的事情不可逆**，每一個寫入型工具（`in_create_invoice`、`in_record_payment` 等）都要放進 `approval_required_tools`。
準備：一組 Invoice Ninja API token。

### Chatwoot（官方 `@chatwoot/mcp-server-chatwoot`）

```toml
[[mcp.external]]
name = "chatwoot"
command = "npx"
args = ["-y", "@chatwoot/mcp-server-chatwoot"]
env = { CHATWOOT_BASE_URL = "https://app.chatwoot.com", CHATWOOT_API_TOKEN = "secret://vault/chatwoot_token" }
allowed_tools = ["chatwoot_list_conversations", "chatwoot_get_conversation", "chatwoot_create_message"]
```
十一個通道的收件匣統一交給一個 agent 處理；草稿回覆走 ApprovalBroker（如果想在寄出前經人工審閱，把 `chatwoot_create_message` 標 ⚠）。準備：一組 Chatwoot API access token。

### WooCommerce（官方原生 MCP，開發預覽版）

```toml
[[mcp.external]]
name = "woocommerce"
command = "npx"
args = ["-y", "@woocommerce/mcp-adapter"]   # WordPress MCP Adapter
env = { WP_SITE_URL = "https://shop.example.com", WP_MCP_OAUTH_TOKEN = "secret://vault/woo_oauth" }
allowed_tools = ["wc_list_products", "wc_get_order", "wc_list_orders"]
```
**請走 WordPress MCP Adapter 的 OAuth 2.1，舊版 `X-MCP-API-Key` 已在 2026-06-23 廢止。** 準備：WP MCP Adapter 外掛，加上一個 OAuth client。

### DocuSeal（官方 MCP server，self-hosted）

DocuSeal self-hosted 在 `https://<host>/mcp` 有自己的 MCP 端點，用 Settings → MCP Server 產生的 bearer token 掛載即可。cloud 租戶沒有 MCP 端點，那邊走 REST API。見 [docuseal.md](docuseal.md)。

### Monica（個人 PRM，走輕量 MCP 或 IdentityProvider）

目前沒有現成的 MCP server。可以自己刻一支包 `/api/contacts` 的輕量 MCP（生日、互動紀錄），或者直接接成一個 `IdentityProvider`（見 `duduclaw-identity`）。追蹤於 IMPL-PLAN §D。

## 未來規劃

- 把每台伺服器的呼叫都記進 `tool_calls.jsonl` 稽核（目前內建工具有歸屬，外部掛載目前只在連線時留 log）。
