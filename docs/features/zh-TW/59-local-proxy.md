# 本機 proxy — 把帳號池借給 Aider、Cline、Codex

`duduclaw proxy` 在 localhost 上開一個 OpenAI 相容的 HTTP 端點，轉發到 DuDuClaw 已經在管的帳號池。任何會講 OpenAI chat API 的工具——Aider、Cline、Continue、Codex，甚至一行 `curl`——都可以指過來，直接用你已經設定好的金鑰與配額，不必再在硬碟上多放一份憑證。

---

## 快速開始

```bash
duduclaw proxy --bind 127.0.0.1:8788
```

第一次在沒設定金鑰的情況下啟動時，它會印出一把臨時 Bearer 金鑰，並提示固定金鑰要設在哪。接著把客戶端指過來：

```bash
curl http://127.0.0.1:8788/v1/chat/completions \
  -H "Authorization: Bearer $DUDUCLAW_PROXY_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"anthropic/claude-sonnet-5","messages":[{"role":"user","content":"hi"}]}'
```

Aider：

```bash
export OPENAI_API_BASE=http://127.0.0.1:8788/v1
export OPENAI_API_KEY=$DUDUCLAW_PROXY_KEY
aider --model anthropic/claude-sonnet-5
```

---

## 端點

| 方法 | 路徑 | 認證 | 說明 |
|---|---|---|---|
| POST | `/v1/chat/completions` | Bearer | 支援串流（SSE）與一次回傳 |
| GET | `/v1/models` | Bearer | 內建的模型目錄 |
| GET | `/healthz` | 無 | 存活探測 |

---

## 模型名稱

有 `provider/model` 前綴時，前綴決定 provider：

```
anthropic/claude-sonnet-5   → anthropic
openai/gpt-5.5              → openai
gemini/gemini-3-pro         → gemini
deepseek/deepseek-chat      → openai-compat preset
```

沒有前綴的裸名稱會落到 `--default-provider`，而它本身預設是 `anthropic`。所以用 `--default-provider openai` 啟動時，`gpt-4o` 會解析成 `openai/gpt-4o`。這個值沒有對應的 `config.toml` 鍵，只能在啟動時用參數給。

---

## 認證

Bearer 金鑰**一律必要**。解析順序：

1. 命令列 `--key <值>`
2. 環境變數 `DUDUCLAW_PROXY_KEY`
3. `config.toml` 的 `[proxy] key`
4. 都沒設 ⇒ 產生一把隨機金鑰並印出來，行程結束就失效

```toml
[proxy]
key = "ddk-proxy-…"
```

比對採常數時間。預設綁 loopback；綁到可路由位址等於把整個帳號池暴露給所有打得到那個埠的人，請走 Tailscale 或 SSH tunnel，不要直接開在 0.0.0.0。

流量限制以來源 IP 為單位，用的是跟 MCP HTTP server 同一套 token bucket。

---

## 已知限制：訂閱制 OAuth 席次無法轉發

帳號輪替器裡有兩種帳號：**API key** 帳號，以及**訂閱制 OAuth 席次**（Claude Pro／Team／Max）。只有 API key 帳號能經由這個 proxy 轉發。輪替器如果選到 OAuth 席次，請求會被明確拒絕，而不是靜默回一個空完成：

> 選定帳號 `<名稱>`（OAuth）為訂閱制 OAuth seat，proxy 轉發需 API key 帳號（OAuth 轉發為 PENDING-LIVE）

所以要用這個 proxy，帳號池裡至少要有一個 API key 帳號。訂閱制轉發尚未實作，在實作之前這頁都會照實這樣寫。

---

## 失敗行為

全線 fail-closed。沒有可用帳號時回 `503` 並附繁中原因，絕不回一個空完成讓 coding agent 當成答案吃下去。上游錯誤會映射成最接近的 OpenAI 相容錯誤格式。

---

## 延伸閱讀

- [部署指南](../../guides/zh-TW/deployment-guide.md) — 設定 proxy 所借用的帳號池
- [Remote MCP](../../guides/zh-TW/remote-mcp.md) — 反方向：讓外部客戶端驅動 DuDuClaw 的工具，而不是它的模型
