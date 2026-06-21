# MCP HTTP/SSE Transport

> MCPツールボックスへの二つ目の玄関——単発呼び出しはBearer認証のREST、長時間接続のストリームはServer-Sent Eventsで、リモートクライアントがstdio transportと同じツールに到達できます。

---

## たとえ話：専用回線 vs. 交換台付きラジオ放送

元々のMCP transportは**stdio**——単一のClaude CLIプロセスとDuDuClaw MCP serverを直結する専用電話回線です。高速で親密ですが、発信者はちょうど一人だけ。二つのプロセスが`stdin`/`stdout`を共有し、他の誰もダイヤルインできません。

HTTP/SSE transportは**交換台とラジオ放送**です：

- 有効なバッジ（Bearer APIキー）を持つ誰もが*交換台にダイヤル*して単発リクエストを出せます——`POST /mcp/v1/call`。一回鳴らし、一回答え、切る。
- あるいは*ラジオにチューニング*——`GET /mcp/v1/stream`——イベント発生時にプッシュされる長時間接続の放送を聴く。
- さらに*交換台に電話して結果をラジオで告知してもらう*——`POST /mcp/v1/stream/call`——今リクエストを出し、すでに購読しているストリームから回答が返ってくるのを聴く。

交換台は専用回線を置き換えません。stdio transportは引き続きローカルCLIに奉仕します。HTTP/SSEは*同じツールボックス*を、別プロセス・別コンテナ・別マシンに住む外部クライアントに開放するだけです。

---

## なぜ二つ目のtransportか

MCP serverとその唯一の消費者が同じ場所にあり一緒に起動される場合、stdioは完璧です。しかし以下には奉仕できません：

- ネットワーク経由でツールを呼び出したいwebダッシュボードや外部サービス。
- 単発の同期呼び出しでブロックするのではなく、結果のストリームを*購読*したい長時間実行クライアント。
- 単一の実行中MCP serverを共有する複数の独立した発信者。

HTTP/SSEはstdioに触れずにそのギャップを埋めます：

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
        │  ローカル        │          │  リモートHTTP            │
        │  Claude CLI      │          │  クライアント            │
        │  プロセス        │          │  (ダッシュボード/サービス│
        │  (一発信者)      │          │   /別マシン)             │
        └─────────────────┘          └──────────────────────────┘
        「専用回線」                  「交換台 + ラジオ」
```

一つのコマンドで起動：

```
duduclaw http-server --bind 127.0.0.1:8765
```

`--no-sse`を渡すとストリーミングエンドポイントを無効化し、`POST /mcp/v1/call`と`GET /healthz`のみを公開します。

---

## 四つのエンドポイント

| エンドポイント | メソッド | 認証 | 用途 |
|----------------|----------|------|------|
| `/mcp/v1/call` | `POST` | Bearerのみ | 単発同期のJSON-RPC 2.0 `tools/call`、一回の応答。 |
| `/mcp/v1/stream` | `GET` | Bearer **または** `?api_key=` | 長時間接続のSSEイベントストリームを開く；`conn_id`を返す。 |
| `/mcp/v1/stream/call` | `POST` | Bearerのみ | 指定ストリームにツール呼び出しを注入；結果はSSEでプッシュ。 |
| `/healthz` | `GET` | なし | 死活プローブ——`200 OK`を返す、認証不要。 |

`/healthz`は唯一の認証不要ルートです。それ以外はすべてfail closed：欠落または不正な`Authorization`ヘッダは、いかなるツール実行よりも前に拒否されます。

---

## Request → Call（同期）

最もシンプルな経路は単発の往復——ダイヤルし、尋ね、答えを得て、切る：

```
client                         mcp_http_server                  dispatcher
  |                                  |                              |
  |  POST /mcp/v1/call               |                              |
  |  Authorization: Bearer <key>     |                              |
  |  { jsonrpc:"2.0",                |                              |
  |    method:"tools/call", ... } -->|                              |
  |                                  |-- Bearer認証 --------------> |
  |                                  |-- rate-limitゲート --------> |
  |                                  |-- jsonrpc=="2.0"を検証       |
  |                                  |-- method==                   |
  |                                  |     "tools/call"を検証       |
  |                                  |-- ツールをディスパッチ ----> |
  |                                  |<------- ツール結果 --------- |
  |<------ JSON-RPC 2.0 result ------|                              |
  |       (接続を閉じる)             |                              |
```

`jsonrpc`が`"2.0"`でなければ、serverはコード`-32600`のJSON-RPCエラーを返します。`method`が`tools/call`でなければ`-32601`（"Method not found"）を返します。契約は意図的に厳格です——検証されるのは実際に実行される成果物であり、リクエストの外殻だけではありません。

---

## Stream/Call → SSE Push（非同期）

長時間接続クライアントでは、フローは同じ接続上で二つのリクエストに分かれます：

```
1. 購読
   client --- GET /mcp/v1/stream (Bearer または ?api_key=) ---> server
   server --- 接続を登録、conn_idを返す --------------------> client
              (broadcast channelが開く；クライアントは聴取中)

2. 発射
   client --- POST /mcp/v1/stream/call?conn_id=<id> -------> server
              { jsonrpc:"2.0", method:"tools/call", ... }
   server --- 発信者がconn_idを所有するか検証 -------------> (ownershipチェック)
   server --- ツールをディスパッチ ------------------------> dispatcher

3. 受信
   server --- 結果イベントをconn_idストリームにプッシュ ----> client
              (ステップ1のSSE接続経由で届く)
```

`conn_id`が二つのリクエストを結びつけます。`mcp_sse_store.rs`は購読時に**所有するprincipal**を記録するため、`stream/call`は発信者がプッシュ先のストリームを実際に所有しているか検証できます——他人の放送に結果をねじ込むことはできません。

### mcp_sse_store.rsの内部

各SSE接続は`tokio::sync::broadcast`送信側と、リプレイ用の接続ごとリングバッファ（容量1024イベント）に支えられます：

```
SseEventStore
  └─ connections: HashMap<conn_id, ConnectionEntry>
                          └─ tx:    broadcast::Sender<String>   (ライブfan-out)
                          └─ ring:  直近1024件のSseEvent        (リプレイバッファ)
                          └─ owner: principal client_id         (所有権ゲート)
```

- `register_connection` / `remove_connection`がライフサイクルを管理（クライアント切断時に削除し送信側を破棄）。
- `push_event`はライブ放送；`record`は放送せずリングバッファに追記。
- `replay_after(conn_id, last_event_id)`は再接続クライアントが取りこぼしたイベントを回復させる——1024イベントウィンドウより遅れすぎていればエラーを返す。
- アイドル接続は10分のTTL後に退去させられ、放棄されたストリームが蓄積しません。

---

## 認証 + Rate-Limitゲート

認証された各リクエストは、ツール実行前に二つのチェックを通過します：

```
受信リクエスト
      |
      v
┌─────────────────────────────────────────────┐
│ 1. 認証                                       │
│    /mcp/v1/call      → Bearerヘッダのみ       │
│    /mcp/v1/stream    → Bearer または ?api_key=│
│    欠落/不正         → 拒否 (fail closed)     │
└─────────────────────────────────────────────┘
      |  Principalを解決
      v
┌─────────────────────────────────────────────┐
│ 2. RATE LIMIT  (OpType::HttpRequest)         │
│    token bucket：キーごと60 req/min          │
│    容量60、補充1.0 token/s                    │
│    Read/Write bucketsから独立                 │
└─────────────────────────────────────────────┘
      |  許可
      v
   ツールをディスパッチ → (下流でRead/Write scopeチェックは依然適用)
```

HTTPゲート（`OpType::HttpRequest`）は、既存の`Read`（100/min）・`Write`（20/min）制限とは*別個*のtoken bucketです——transport層に位置し、per-tool scopeチェックが走る前に、APIキーごとに生リクエスト量をスロットルします。`GET /mcp/v1/stream`はキーを`?api_key=`で運べます（ヘッダを設定できないブラウザの`EventSource`でも認証可能にするため）；`POST /mcp/v1/call`と`POST /mcp/v1/stream/call`はBearerヘッダのみ受け付けます。

すべての応答——認証不要の`/healthz`さえも——標準のcapabilityヘッダを携えるため、クライアントは両transport間で一貫して機能をネゴシエートできます。

---

## curlの例

HTTP経由の単発同期ツール呼び出し：

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

認証不要の死活チェック：

```bash
curl http://127.0.0.1:8765/healthz
# → 200 OK
```

ブラウザからストリームを購読（`EventSource`はヘッダを設定できないため、キーはquery stringに）：

```js
const es = new EventSource("http://127.0.0.1:8765/mcp/v1/stream?api_key=" + key);
es.onmessage = (e) => console.log("event:", e.data);
```

---

## なぜ重要か

### 一つのツールボックス、二つの扉

同じMCPツール——channel、memory、agent、skill、task、shared wiki、autopilot——は、ローカルCLIからstdio経由でも、*そして*リモートHTTPクライアントからネットワーク経由でも到達可能です。ツールの二つ目の実装はありません；HTTP層は同じdispatcherの前に置かれたtransportアダプタです。

### ポーリングではなくプッシュ

SSEはクライアントが結果を*購読*することを可能にし、同期呼び出しでブロックしたりループでポーリングしたりせずに済みます。`stream/call`を発射すれば、すでに開いている接続から答えが返ってきます——1024イベントのリプレイバッファ付きで、短い切断でもデータを失いません。

### デフォルトでfail closed、デフォルトでスロットル

`/healthz`を除き、有効なキーなしにツールを奉仕するルートはありません。60 req/minのHTTP bucketは、per-tool scopeチェックが走る前にtransport層で乱用を上限制限し、所有権検証はある発信者が他人のストリームにプッシュするのを止めます。

### ブラウザに優しい

`/mcp/v1/stream`の`?api_key=`フォールバックは、まさに`EventSource`が`Authorization`ヘッダを設定できないために存在します——ダッシュボードがproxyなしで直接購読できるように。

---

## まとめ

stdioは専用回線：一プロセス、一発信者、ネットワークなし。HTTP/SSE transportは交換台とラジオ放送です——`POST /mcp/v1/call`は単発のダイヤルと応答、`GET /mcp/v1/stream`は聴取、`POST /mcp/v1/stream/call`はリクエストを発射し結果が空中から返ってくるのを聴く。同じツールボックス、Bearer認証、60 req/minでスロットル、デフォルトでfail-closed——今やローカルCLIだけでなく、あらゆる外部HTTPクライアントが到達できます。
