# MCP HTTP/SSE Transport

> A second front door to the MCP toolbox — Bearer-authenticated REST for single calls, Server-Sent Events for long-lived streams, so remote clients can reach the same tools the stdio transport serves.

---

## The Metaphor: A Private Line vs. a Switchboard with Radio

The original MCP transport is **stdio** — a private phone line wired directly between one Claude CLI process and the DuDuClaw MCP server. It's fast and intimate, but it has exactly one caller. The two processes share `stdin`/`stdout`; nothing else can dial in.

The HTTP/SSE transport is a **switchboard plus a radio broadcast**:

- Anyone with a valid badge (a Bearer API key) can *dial the switchboard* and place a single request — `POST /mcp/v1/call`. One ring, one answer, hang up.
- Or they can *tune into the radio* — `GET /mcp/v1/stream` — and listen to a long-lived broadcast of events pushed as they happen.
- And they can *call the switchboard and ask it to announce the result on the radio* — `POST /mcp/v1/stream/call` — fire a request now, hear the answer come back over the stream they're already subscribed to.

The switchboard doesn't replace the private line. The stdio transport still serves the local CLI. HTTP/SSE simply opens the *same toolbox* to external clients that live in another process, another container, or another machine.

---

## Why a Second Transport

stdio is perfect when the MCP server and its one consumer are co-located and spawned together. But it can't serve:

- A web dashboard or external service that wants to invoke a tool over the network.
- A long-running client that wants to *subscribe* to a stream of results rather than block on one synchronous call.
- Multiple independent callers sharing a single running MCP server.

HTTP/SSE fills that gap without touching stdio:

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
        │  local Claude   │          │  remote HTTP clients     │
        │  CLI process    │          │  (dashboard / service /  │
        │  (one caller)   │          │   another machine)       │
        └─────────────────┘          └──────────────────────────┘
        "the private line"           "the switchboard + radio"
```

Start it with one command:

```
duduclaw http-server --bind 127.0.0.1:8765
```

Pass `--no-sse` to disable the streaming endpoints and expose only `POST /mcp/v1/call` and `GET /healthz`.

---

## The Four Endpoints

| Endpoint | Method | Auth | Purpose |
|----------|--------|------|---------|
| `/mcp/v1/call` | `POST` | Bearer only | One synchronous JSON-RPC 2.0 `tools/call`, one response. |
| `/mcp/v1/stream` | `GET` | Bearer **or** `?api_key=` | Open a long-lived SSE event stream; returns a `conn_id`. |
| `/mcp/v1/stream/call` | `POST` | Bearer only | Inject a tool call into a named stream; result is pushed over SSE. |
| `/healthz` | `GET` | none | Liveness probe — returns `200 OK`, no auth required. |

`/healthz` is the only unauthenticated route. Everything else fails closed: a missing or malformed `Authorization` header is rejected before any tool runs.

---

## Request → Call (synchronous)

The simplest path is a single round-trip — dial, ask, get the answer, hang up:

```
client                         mcp_http_server                  dispatcher
  |                                  |                              |
  |  POST /mcp/v1/call               |                              |
  |  Authorization: Bearer <key>     |                              |
  |  { jsonrpc:"2.0",                |                              |
  |    method:"tools/call", ... } -->|                              |
  |                                  |-- authenticate Bearer -----> |
  |                                  |-- rate-limit gate ---------> |
  |                                  |-- validate jsonrpc=="2.0"    |
  |                                  |-- validate method==          |
  |                                  |     "tools/call"             |
  |                                  |-- dispatch tool -----------> |
  |                                  |<------- tool result -------- |
  |<------ JSON-RPC 2.0 result ------|                              |
  |       (connection closes)        |                              |
```

If `jsonrpc` isn't `"2.0"`, the server returns a JSON-RPC error with code `-32600`. If `method` isn't `tools/call`, it returns `-32601` ("Method not found"). The contract is strict on purpose — the artifact that actually executes is validated, not just the request envelope.

---

## Stream/Call → SSE Push (asynchronous)

For long-lived clients, the flow splits across two requests on the same connection:

```
1. SUBSCRIBE
   client --- GET /mcp/v1/stream (Bearer or ?api_key=) ---> server
   server --- registers connection, returns conn_id -----> client
              (broadcast channel now open; client listening)

2. FIRE
   client --- POST /mcp/v1/stream/call?conn_id=<id> -----> server
              { jsonrpc:"2.0", method:"tools/call", ... }
   server --- verify caller owns conn_id ----------------> (ownership check)
   server --- dispatch tool ------------------------------> dispatcher

3. RECEIVE
   server --- push result event onto the conn_id stream --> client
              (arrives over the SSE connection from step 1)
```

The `conn_id` ties the two requests together. `mcp_sse_store.rs` records the **owning principal** at subscribe time, so `stream/call` can verify the caller actually owns the stream it's pushing into — you can't shove a result into someone else's broadcast.

### Inside mcp_sse_store.rs

Each SSE connection is backed by a `tokio::sync::broadcast` sender plus a per-connection ring buffer (capacity 1024 events) for replay:

```
SseEventStore
  └─ connections: HashMap<conn_id, ConnectionEntry>
                          └─ tx:    broadcast::Sender<String>   (live fan-out)
                          └─ ring:  last 1024 SseEvent          (replay buffer)
                          └─ owner: principal client_id         (ownership gate)
```

- `register_connection` / `remove_connection` manage lifecycle (remove on client disconnect drops the sender).
- `push_event` broadcasts live; `record` appends to the ring without broadcasting.
- `replay_after(conn_id, last_event_id)` lets a reconnecting client recover missed events — or returns an error if it fell too far behind the 1024-event window.
- Idle connections are evicted after a 10-minute TTL so abandoned streams don't accumulate.

---

## The Auth + Rate-Limit Gate

Every authenticated request passes through two checks before a tool runs:

```
incoming request
      |
      v
┌─────────────────────────────────────────────┐
│ 1. AUTHENTICATE                              │
│    /mcp/v1/call      → Bearer header only    │
│    /mcp/v1/stream    → Bearer OR ?api_key=   │
│    missing/malformed → reject (fail closed)  │
└─────────────────────────────────────────────┘
      |  resolves Principal
      v
┌─────────────────────────────────────────────┐
│ 2. RATE LIMIT  (OpType::HttpRequest)         │
│    token bucket: 60 req/min per key          │
│    capacity 60, refill 1.0 token/s           │
│    independent of the Read/Write buckets     │
└─────────────────────────────────────────────┘
      |  allowed
      v
   dispatch tool  → (Read/Write scope checks still apply downstream)
```

The HTTP gate (`OpType::HttpRequest`) is a *separate* token bucket from the existing `Read` (100/min) and `Write` (20/min) limits — it sits at the transport layer and throttles raw request volume per API key before the per-tool scope checks ever run. `GET /mcp/v1/stream` may carry its key as `?api_key=` (so a browser `EventSource`, which can't set headers, can still authenticate); `POST /mcp/v1/call` and `POST /mcp/v1/stream/call` accept the Bearer header only.

Every response — even unauthenticated `/healthz` — carries the standard capability headers, so clients can negotiate features consistently across both transports.

---

## A curl Example

A single synchronous tool call over HTTP:

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

A no-auth liveness check:

```bash
curl http://127.0.0.1:8765/healthz
# → 200 OK
```

Subscribing to the stream from a browser (key in the query string, since `EventSource` can't set headers):

```js
const es = new EventSource("http://127.0.0.1:8765/mcp/v1/stream?api_key=" + key);
es.onmessage = (e) => console.log("event:", e.data);
```

---

## Why This Matters

### One toolbox, two doors

The same MCP tools — channel, memory, agent, skill, task, shared wiki, autopilot — are reachable from a local CLI over stdio *and* from a remote HTTP client over the network. There's no second implementation of the tools; the HTTP layer is a transport adapter in front of the same dispatcher.

### Push, not just poll

SSE lets a client *subscribe* to results instead of blocking on a synchronous call or polling in a loop. Fire a `stream/call` and the answer arrives over a connection you already hold open — with a 1024-event replay buffer so a brief disconnect doesn't lose data.

### Fails closed, throttled by default

`/healthz` aside, no route serves a tool without a valid key. The 60 req/min HTTP bucket caps abuse at the transport layer before the per-tool scope checks even run, and ownership verification stops one caller from pushing into another's stream.

### Browser-friendly

The `?api_key=` fallback on `/mcp/v1/stream` exists precisely because `EventSource` can't set an `Authorization` header — so a dashboard can subscribe directly without a proxy.

---

## The Takeaway

stdio is the private line: one process, one caller, no network. The HTTP/SSE transport is the switchboard and the radio broadcast — `POST /mcp/v1/call` for a single dial-and-answer, `GET /mcp/v1/stream` to tune in, `POST /mcp/v1/stream/call` to fire a request and hear the result come back over the air. Same toolbox, Bearer-authenticated, rate-limited at 60 req/min, fail-closed by default — now reachable by any external HTTP client, not just the local CLI.
