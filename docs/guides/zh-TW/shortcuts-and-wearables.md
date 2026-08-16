# 手腕與穿戴：Shortcuts 捷徑、AI 錄音吊墜接入

> 前置：`duduclaw http-server --bind 127.0.0.1:8765` 已啟動，並用 `duduclaw mcp issue-refresh-token` 或 `~/.duduclaw/mcp_keys.toml` 取得 Bearer key。手機/穿戴要打得到這個位址（家內網 IP 或 Tailscale）。

## 1. iPhone／Apple Watch 捷徑（零上架、零審查）

Shortcuts 的「取得 URL 內容」就能打 DuDuClaw 的 HTTP API；捷徑開啟「顯示在 Apple Watch」後可放上錶面 complication 一鍵執行。

**捷徑 A：問 AI 員工**（語音聽寫 → 回覆通知）
1. 「聽寫文字」
2. 「取得 URL 內容」：POST `http://<gateway>:8765/mcp/v1/call`
   - Headers：`Authorization: Bearer <key>`、`Content-Type: application/json`
   - Body（JSON）：
     ```json
     {"jsonrpc":"2.0","id":"1","method":"tools/call",
      "params":{"name":"send_message","arguments":{"content":"聽寫文字"}}}
     ```
3. 「顯示通知」帶回應內容。

**捷徑 B：存一則記憶**：同上，`params.name` 換 `memory_store`、`arguments.content` 帶聽寫文字。

> 審批裁決在手腕上更簡單：不用捷徑，直接**回覆** LINE/Telegram 的審批卡打「同意」或「拒絕」即可（四通道支援，見 CHANGELOG）。

## 2. Bee 手環（Amazon）：純設定接入

Bee 官方 CLI 本身就是 MCP server，掛進 agent 的外部 MCP 清單即可，agent 直接查你的對話摘要：

```toml
# agent.toml
[[mcp.external]]
name = "bee"
command = "bee"          # Bee CLI（照官方文件安裝並登入）
args = ["mcp"]
# 工具面照預設 deny-by-default，需要哪些再開
allow = ["get_conversations", "get_todos"]
```

詳見 [mcp-bridge.md](mcp-bridge.md) 的外部 MCP 掛載說明。

## 3. Omi／Plaud：webhook 直灌記憶

gateway 的 `POST /ingest/transcript` 是為穿戴廠商 webhook 設計的薄轉接：同一把 Bearer key、同一條 `memory_store` 寫入管線（scope 檢查與來源綁定照常生效）。接受的欄位：`text`／`transcript`／`summary`／`segments[].text`。

- **Omi**：App → Developer → Integration webhook 填
  `https://<你的公網入口>/ingest/transcript`（需帶 `Authorization: Bearer <key>` 的話用其 header 設定；Omi 版本若不支援自訂 header，前面加一層 Cloudflare Worker 補 header）
- **Plaud**：Developer Platform → Webhooks 指向同一端點（signed webhook 的驗簽在你的反代層做，或直接依 Bearer key 信任）
- 手動測試：
  ```bash
  curl -X POST http://127.0.0.1:8765/ingest/transcript \
    -H "Authorization: Bearer <key>" -H "Content-Type: application/json" \
    -d '{"source":"plaud","summary":"客戶說週五前要報價"}'
  # → {"stored":true,...}
  ```

存進去的記憶會標 `wearable,<source>` 標籤與外部來源（信任上限照 v1.41 origin binding，穿戴逐字稿不會被當成高信任事實）。

## 安全備註

- key 只授權 MCP 對外白名單工具（memory/wiki/send_message 共 7 個）；掉了就 `duduclaw mcp` 撤銷重發。
- 公網暴露 `/ingest/transcript` 前，先確認 rate limit（60 req/min/key）與反代層的來源限制。
