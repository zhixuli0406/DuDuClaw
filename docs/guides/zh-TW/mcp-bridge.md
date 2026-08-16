# MCP Bridge——掛載外部 MCP 伺服器

DuDuClaw 可以把第三方 [Model Context Protocol](https://modelcontextprotocol.io) 伺服器掛載在自己內建的 MCP server 旁邊，讓 agent 的工具迴圈直接拿到外部伺服器的工具，不必手刻一支 Rust 連接器。Plane、Chatwoot、Invoice Ninja、Gmail/Calendar、WooCommerce，以及其他任何 MCP server，都是這樣接進 agent 的。

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
- 寫入型／不可逆的工具（計費、刪除）還要另外列進 agent 的 `[capabilities] approval_required_tools`，讓它們走 HITL 的 `ApprovalBroker`。MCP Bridge 控制的是「看不看得到」，審批機制控制的是「能不能執行」。

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
九個通道的收件匣統一交給一個 agent 處理；草稿回覆走 ApprovalBroker（如果想在寄出前經人工審閱，把 `chatwoot_create_message` 標 ⚠）。準備：一組 Chatwoot API access token。

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

### DocuSeal（尚無伺服器，需要自建 `duduclaw-docuseal-mcp`）

DocuSeal 目前還沒有現成的 MCP server；可行的路是自己刻一支小的（REST + webhook：生成 → 寄送 → webhook 完成通知），掛在這裡使用，之後再回饋上游。追蹤於 IMPL-PLAN §D，工作量估 M。

### Monica（個人 PRM，走輕量 MCP 或 IdentityProvider）

目前沒有現成的 MCP server。可以自己刻一支包 `/api/contacts` 的輕量 MCP（生日、互動紀錄），或者直接接成一個 `IdentityProvider`（見 `duduclaw-identity`）。追蹤於 IMPL-PLAN §D。

## 未來規劃

- 把每台伺服器的呼叫都記進 `tool_calls.jsonl` 稽核（目前內建工具有歸屬，外部掛載目前只在連線時留 log）。
