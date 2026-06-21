# MCP HTTP/SSE Transport

> 通往 MCP 工具箱的第二道大門——以 Bearer 認證的 REST 處理單次呼叫，以 Server-Sent Events 處理長連線串流，讓遠端用戶端能存取與 stdio transport 相同的工具。

---

## 比喻：私人專線 vs. 總機加廣播電台

原本的 MCP transport 是 **stdio**——一條直接接在單一 Claude CLI 行程與 DuDuClaw MCP server 之間的私人電話專線。它快速而親密，但只有一位來電者。兩個行程共用 `stdin`/`stdout`；沒有別人能撥進來。

HTTP/SSE transport 則是**總機加廣播電台**：

- 任何持有有效識別證（Bearer API key）的人都能*撥打總機*發出單次請求——`POST /mcp/v1/call`。一響、一答，掛斷。
- 或者*收聽電台*——`GET /mcp/v1/stream`——聆聽事件發生時即時推送的長連線廣播。
- 還能*撥打總機，請它把結果在電台上播報*——`POST /mcp/v1/stream/call`——現在發出請求，從已訂閱的串流上聽到回應傳回。

總機並不取代私人專線。stdio transport 仍服務本地 CLI。HTTP/SSE 只是把*同一個工具箱*開放給住在另一個行程、另一個容器，或另一台機器的外部用戶端。

---

## 為什麼需要第二種 transport

當 MCP server 與其唯一的消費者位於同處並一起啟動時，stdio 堪稱完美。但它無法服務：

- 想透過網路呼叫工具的 web 儀表板或外部服務。
- 想*訂閱*一連串結果，而非阻塞於單次同步呼叫的長時間執行用戶端。
- 共用單一執行中 MCP server 的多個獨立來電者。

HTTP/SSE 在不動 stdio 的前提下填補這個缺口：

```
            ┌──────────────────────────────────────────────┐
            │            DuDuClaw MCP Toolbox               │
            │   channel · memory · agent · skill · task ·   │
            │      shared wiki · autopilot · ...            │
            └──────────────────────────────────────────────┘
                 ▲                              ▲
                 │ stdio (JSON-RPC 2.0)         │ HTTP/SSE (JSON-RPC 2.0)
                 │                              │
        ┌────────┴────────┐          ┌──────────┴──────────────┐
        │  本地 Claude    │          │  遠端 HTTP 用戶端        │
        │  CLI 行程       │          │  (儀表板 / 服務 /        │
        │  (單一來電者)   │          │   另一台機器)            │
        └─────────────────┘          └──────────────────────────┘
        「私人專線」                  「總機 + 電台」
```

以一道指令啟動：

```
duduclaw http-server --bind 127.0.0.1:8765
```

傳入 `--no-sse` 可停用串流端點，只開放 `POST /mcp/v1/call` 與 `GET /healthz`。

---

## 四個端點

| 端點 | 方法 | 認證 | 用途 |
|------|------|------|------|
| `/mcp/v1/call` | `POST` | 僅 Bearer | 單次同步的 JSON-RPC 2.0 `tools/call`，一次回應。 |
| `/mcp/v1/stream` | `GET` | Bearer **或** `?api_key=` | 開啟長連線 SSE 事件串流；回傳一個 `conn_id`。 |
| `/mcp/v1/stream/call` | `POST` | 僅 Bearer | 把工具呼叫注入指定串流；結果經 SSE 推送。 |
| `/healthz` | `GET` | 無 | 存活探測——回傳 `200 OK`，無需認證。 |

`/healthz` 是唯一不需認證的路由。其餘一律 fail closed：缺漏或格式錯誤的 `Authorization` 標頭會在任何工具執行前被拒絕。

---

## Request → Call（同步）

最簡單的路徑是單次往返——撥號、發問、得到答案、掛斷：

```
client                         mcp_http_server                  dispatcher
  |                                  |                              |
  |  POST /mcp/v1/call               |                              |
  |  Authorization: Bearer <key>     |                              |
  |  { jsonrpc:"2.0",                |                              |
  |    method:"tools/call", ... } -->|                              |
  |                                  |-- 認證 Bearer -------------> |
  |                                  |-- rate-limit 閘門 ---------> |
  |                                  |-- 驗證 jsonrpc=="2.0"        |
  |                                  |-- 驗證 method==              |
  |                                  |     "tools/call"             |
  |                                  |-- 派送工具 ----------------> |
  |                                  |<------- 工具結果 ----------- |
  |<------ JSON-RPC 2.0 result ------|                              |
  |       (連線關閉)                 |                              |
```

若 `jsonrpc` 不是 `"2.0"`，server 回傳代碼 `-32600` 的 JSON-RPC 錯誤。若 `method` 不是 `tools/call`，回傳 `-32601`（"Method not found"）。契約刻意嚴格——驗證的是真正會執行的產物，而非僅僅請求外殼。

---

## Stream/Call → SSE Push（非同步）

對長連線用戶端而言，流程在同一條連線上分成兩個請求：

```
1. 訂閱
   client --- GET /mcp/v1/stream (Bearer 或 ?api_key=) ---> server
   server --- 註冊連線，回傳 conn_id --------------------> client
              (broadcast channel 已開；用戶端聆聽中)

2. 發射
   client --- POST /mcp/v1/stream/call?conn_id=<id> -----> server
              { jsonrpc:"2.0", method:"tools/call", ... }
   server --- 驗證來電者擁有 conn_id ---------------------> (ownership 檢查)
   server --- 派送工具 ----------------------------------> dispatcher

3. 接收
   server --- 把結果事件推上 conn_id 串流 ---------------> client
              (從步驟 1 的 SSE 連線傳達)
```

`conn_id` 把兩個請求綁在一起。`mcp_sse_store.rs` 在訂閱時記錄**擁有的 principal**，使 `stream/call` 能驗證來電者確實擁有它要推送的串流——你無法把結果硬塞進別人的廣播。

### mcp_sse_store.rs 內部

每條 SSE 連線以一個 `tokio::sync::broadcast` 發送端，加上每條連線的環形緩衝（容量 1024 事件）作為重播之用：

```
SseEventStore
  └─ connections: HashMap<conn_id, ConnectionEntry>
                          └─ tx:    broadcast::Sender<String>   (即時扇出)
                          └─ ring:  最近 1024 個 SseEvent       (重播緩衝)
                          └─ owner: principal client_id         (擁有權閘門)
```

- `register_connection` / `remove_connection` 管理生命週期（用戶端斷線時移除，使發送端被丟棄）。
- `push_event` 即時廣播；`record` 只附加到環形緩衝而不廣播。
- `replay_after(conn_id, last_event_id)` 讓重連的用戶端恢復遺漏的事件——若落後超過 1024 事件視窗則回傳錯誤。
- 閒置連線在 10 分鐘 TTL 後被驅逐，使被遺棄的串流不會累積。

---

## 認證 + Rate-Limit 閘門

每個經認證的請求在工具執行前都通過兩道檢查：

```
傳入請求
      |
      v
┌─────────────────────────────────────────────┐
│ 1. 認證                                       │
│    /mcp/v1/call      → 僅 Bearer 標頭         │
│    /mcp/v1/stream    → Bearer 或 ?api_key=    │
│    缺漏/格式錯誤     → 拒絕 (fail closed)     │
└─────────────────────────────────────────────┘
      |  解析出 Principal
      v
┌─────────────────────────────────────────────┐
│ 2. RATE LIMIT  (OpType::HttpRequest)         │
│    token bucket：每把 key 60 req/min         │
│    容量 60，補充 1.0 token/s                  │
│    獨立於 Read/Write buckets                  │
└─────────────────────────────────────────────┘
      |  放行
      v
   派送工具  → (下游仍套用 Read/Write scope 檢查)
```

HTTP 閘門（`OpType::HttpRequest`）是與既有 `Read`（100/min）、`Write`（20/min）限制*分開*的 token bucket——它位於 transport 層，在 per-tool scope 檢查執行之前，依每把 API key 節流原始請求量。`GET /mcp/v1/stream` 可把 key 帶在 `?api_key=`（讓無法設定標頭的瀏覽器 `EventSource` 仍能認證）；`POST /mcp/v1/call` 與 `POST /mcp/v1/stream/call` 僅接受 Bearer 標頭。

每個回應——連未認證的 `/healthz` 也是——都攜帶標準的 capability 標頭，使用戶端能在兩種 transport 間一致地協商功能。

---

## curl 範例

透過 HTTP 的單次同步工具呼叫：

```bash
curl -X POST http://127.0.0.1:8765/mcp/v1/call \
  -H "Authorization: Bearer $DUDUCLAW_MCP_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": "1",
    "method": "tools/call",
    "params": {
      "name": "memory_search",
      "arguments": { "query": "deployment target" }
    }
  }'
```

無認證存活檢查：

```bash
curl http://127.0.0.1:8765/healthz
# → 200 OK
```

從瀏覽器訂閱串流（key 放在 query string，因為 `EventSource` 無法設定標頭）：

```js
const es = new EventSource("http://127.0.0.1:8765/mcp/v1/stream?api_key=" + key);
es.onmessage = (e) => console.log("event:", e.data);
```

---

## 為什麼這很重要

### 一個工具箱，兩道門

相同的 MCP 工具——channel、memory、agent、skill、task、shared wiki、autopilot——既能由本地 CLI 透過 stdio 存取，*也*能由遠端 HTTP 用戶端透過網路存取。工具沒有第二套實作；HTTP 層是擺在同一個 dispatcher 前的 transport 轉接器。

### 推送，而不只是輪詢

SSE 讓用戶端*訂閱*結果，而非阻塞於同步呼叫或在迴圈裡輪詢。發出一個 `stream/call`，答案就從你已開啟的連線傳回——還有 1024 事件的重播緩衝，使短暫斷線不致遺失資料。

### 預設 fail closed、預設節流

除了 `/healthz`，沒有任何路由會在沒有有效 key 的情況下服務工具。60 req/min 的 HTTP bucket 在 per-tool scope 檢查執行之前，就於 transport 層封頂濫用；而擁有權驗證阻止某個來電者推送進別人的串流。

### 對瀏覽器友善

`/mcp/v1/stream` 上的 `?api_key=` 後備正是因為 `EventSource` 無法設定 `Authorization` 標頭而存在——讓儀表板無需 proxy 即可直接訂閱。

---

## 總結

stdio 是私人專線：一個行程、一位來電者、沒有網路。HTTP/SSE transport 是總機與廣播電台——`POST /mcp/v1/call` 處理單次撥號與回答，`GET /mcp/v1/stream` 讓你收聽，`POST /mcp/v1/stream/call` 讓你發出請求並從空中聽到結果傳回。同一個工具箱、以 Bearer 認證、以 60 req/min 節流、預設 fail-closed——如今任何外部 HTTP 用戶端皆可存取，不再只限本地 CLI。
