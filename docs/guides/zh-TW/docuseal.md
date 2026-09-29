# DocuSeal：文件簽署工作流

[DocuSeal](https://github.com/docusealco/docuseal) 是開源的 DocuSign 替代品（cloud 或 self-hosted）。DuDuClaw 透過 DocuSeal **官方自己的 MCP server** 連上它，用 `[[mcp.external]]` 掛載，不經過 DuDuClaw 這側的 wrapper。

> **2026-09 變更。** DuDuClaw 原本內建一個第一方 stdio wrapper crate（`duduclaw-docuseal-mcp`，10 個工具）。它從來沒被 `scripts/release.sh` 建進出貨包，使用者得自己 `cargo build`；而 DocuSeal 在 2026-03 已經有官方 MCP server。該 wrapper 已移除，改用下面的官方 server。

## 掛載官方 server

DocuSeal self-hosted 的 MCP 端點是 `https://<host>/mcp`。在實例的 **Settings → MCP Server** 產生 bearer token，再透過 [MCP Bridge](../mcp-bridge.md) 掛載：

```toml
[[mcp.external]]
name = "docuseal"
url = "https://sign.example.com/mcp"
headers = { Authorization = "Bearer secret://local/docuseal_mcp_token" }
allowed_tools = [
  "search_templates", "load_template", "create_template",
  "send_document", "search_documents",
]
```

寄送簽署是對外、半不可逆的動作——建議把送出的工具放進 `[capabilities] approval_required_tools`，走 HITL 審批。

## 官方 server 的涵蓋範圍

五個工具：搜尋板模、載入板模、建立板模、寄送簽署、搜尋文件。它**只支援 self-hosted**——cloud 租戶（`api.docuseal.com` / `.eu`）沒有 MCP 端點。

如果你用的是 DocuSeal cloud，或需要更完整的 REST 面（歸檔、重寄、prefill 更新、簽署檔下載 URL），直接帶 `X-Auth-Token` header 呼叫 [DocuSeal REST API](https://www.docuseal.com/docs/api)——自己寫一個小 MCP server，或用 agent 的 HTTP 工具。

## 簽署完成 → 自動通知（webhook）

DocuSeal 的 webhook 只能在它的 UI 設定（cloud：Console → Webhooks；self-hosted：Settings → Webhooks），API 設不了。把 `form.completed` / `submission.completed` 指向你的自動化入口，就能接一條 autopilot 規則做「完成時通知通道／建任務」。payload 外層是 `{"event_type", "timestamp", "data"}`；簽章 header 是 `X-Docuseal-Signature`（`<unix_ts>.<hex_hmac>`，對 `<ts>.<raw_body>` 做 HMAC-SHA256，±300 秒容許）。
