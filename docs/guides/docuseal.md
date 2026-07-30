# DocuSeal — 文件簽署工作流

[DocuSeal](https://github.com/docusealco/docuseal) 是開源的 DocuSign 替代品
（cloud 或 self-hosted）。DuDuClaw 提供 `duduclaw-docuseal-mcp`——一個開源的
MCP stdio wrapper，讓 agent 走「生成合約 → 寄送簽署 → 查狀態 → 取回簽署檔」
全流程。

## 兩條路，怎麼選

| 路徑 | 適用 | 認證 |
|---|---|---|
| **`duduclaw-docuseal-mcp`（本 wrapper）** | cloud（api.docuseal.com / .eu）**與** self-hosted 都支援；工具面較完整（歸檔、重寄、prefill 更新、簽署檔 URL） | `X-Auth-Token` API key |
| **DocuSeal 官方內建 MCP**（2026-03 起） | 僅 self-hosted；5 個工具（search/load/create template、send、search documents） | 實例 Settings → MCP Server 產生的 Bearer token，`url = "https://<host>/mcp"` 直接掛 [MCP Bridge](mcp-bridge.md) |

## wrapper 的 10 個工具

`docuseal_list_templates`、`docuseal_get_template`、
`docuseal_create_template_from_pdf`（base64 或 URL，PDF 內可用
`{{欄位;role=Signer1;type=signature}}` text tags 自動放欄位）、
`docuseal_create_submission`（寄送簽署，回每位簽署人的簽署連結 `embed_src`）、
`docuseal_get_submission`（狀態＋事件＋`audit_log_url`）、
`docuseal_list_submissions`、`docuseal_archive_submission`、
`docuseal_get_submission_documents`（完成後的簽署檔下載 URL）、
`docuseal_resend_submitter_email`、`docuseal_update_submitter`（prefill／改聯絡方式）。

## 設定

環境變數：

| 變數 | 說明 |
|---|---|
| `DOCUSEAL_API_KEY` | 必填。cloud 在 <https://console.docuseal.com/api> 取得；self-hosted 在實例 API settings |
| `DOCUSEAL_BASE_URL` | 選填。預設 `https://api.docuseal.com`；EU cloud `https://api.docuseal.eu`；self-hosted `https://<host>/api` |

`agent.toml` 掛載（stdio）：

```toml
[[mcp.external]]
name = "docuseal"
command = "duduclaw-docuseal-mcp"
env = { DOCUSEAL_API_KEY = "secret://local/docuseal_api_key" }
# self-hosted 加： DOCUSEAL_BASE_URL = "https://sign.example.com/api"
allowed_tools = [
  "docuseal_list_templates", "docuseal_get_template",
  "docuseal_create_submission", "docuseal_get_submission",
  "docuseal_get_submission_documents", "docuseal_resend_submitter_email",
]
```

寄送／歸檔屬於對外且半不可逆的動作——建議把 `docuseal_create_submission`、
`docuseal_archive_submission` 列入 `[capabilities] approval_required_tools`
走 HITL 審批。

## 簽署完成 → 自動通知（webhook）

DocuSeal 的 webhook 只能在 UI 設定（cloud: Console → Webhooks；self-hosted:
Settings → Webhooks），API 無法代設。把 `form.completed` /
`submission.completed` 指向你的自動化入口後，可串 autopilot 規則做「完成即
通知頻道／建任務」。Payload 外殼是
`{"event_type", "timestamp", "data"}`；驗簽 header `X-Docuseal-Signature`
（`<unix_ts>.<hex_hmac>`，HMAC-SHA256 對 `<ts>.<raw_body>`，容差 ±300s）。

## 本機驗證

```sh
cargo build -p duduclaw-docuseal-mcp
printf '%s\n%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  | DOCUSEAL_API_KEY=test ./target/debug/duduclaw-docuseal-mcp
```

第二行回應應列出 10 個 `docuseal_*` 工具。實際 API 呼叫（`tools/call`）需要
有效 key；HTTP 層失敗會以 `isError: true` 回給 agent，不會中斷 server。
