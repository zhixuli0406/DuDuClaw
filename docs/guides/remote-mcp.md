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
```

拿到對外網址後，在 claude.ai「設定 → 連接器 → 新增自訂連接器」貼上：

```
https://<你的網址>/mcp
```

claude.ai 會自動走 OAuth 探索（RFC 9728 → RFC 8414 → 動態註冊），把你導到
DuDuClaw 的授權頁。頁面上貼一把**內部 MCP API key**（`config.toml [mcp_keys]` 中
`is_external = false` 的 key）按「同意連線」即完成。

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

## 端點總覽

| 路徑 | 用途 |
|---|---|
| `POST /mcp` | 標準 MCP 端點（initialize／tools/list／tools/call／ping） |
| `GET /.well-known/oauth-protected-resource` | RFC 9728 資源中繼資料（401 的 `WWW-Authenticate` 會指向這裡） |
| `GET /.well-known/oauth-authorization-server` | RFC 8414 授權伺服器中繼資料 |
| `POST /oauth/register` | RFC 7591 動態客戶端註冊（public client） |
| `GET /oauth/authorize` → `POST /oauth/decision` | 授權碼流程＋操作者同意頁 |
| `POST /oauth/token` | 換發／刷新權杖 |
| `POST /mcp/v1/call`、`GET /mcp/v1/stream` | 既有的 DuDuClaw REST/SSE 面（不變） |

靜態 Bearer key（`ddc_…`）與 OAuth 權杖（`ddc_oauth_…`）走同一個
`Authorization: Bearer` 表面；腳本／自家整合繼續用靜態 key 即可，OAuth 是給
「只會說 OAuth」的客戶端（claude.ai）用的。

## 安全備註

- 瀏覽器來源（`Origin` 標頭）比對 loopback＋`config.toml [gateway] allowed_origins`
  白名單（錨定比對），其餘一律 403；非瀏覽器客戶端不受影響。
- quick tunnel 的網址每次啟動會變——正式對外請用固定網域（反向代理或 Cloudflare
  named tunnel），OAuth 流程的 issuer 由請求的 `Host`／`X-Forwarded-Proto` 導出。
- 要撤銷所有已簽發的連線，刪除 `~/.duduclaw/mcp_oauth_issued.json` 即可（下次請求
  立即失效）。
