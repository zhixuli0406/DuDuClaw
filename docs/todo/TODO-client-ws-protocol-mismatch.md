# TODO — Client RPC over WebSocket speaks JSON-RPC 2.0, the gateway does not

**Owner:** — &nbsp; **Status:** VS Code fixed (uncommitted); Chrome + Stream Deck outstanding
**Last updated:** 2026-08-16 &nbsp; **Severity:** 🟠 HIGH (every dashboard RPC in the affected clients is dead)

## Problem

Three of the five bundled clients build their RPC frames as JSON-RPC 2.0. The
gateway speaks its own `WsFrame` protocol, so **every dashboard RPC from those
clients fails**, and the failure is reported as a misleading transport error.

Reproduced 2026-08-16 in the VS Code extension: the approvals panel renders
`審批載入失敗：Error: connection closed` against a healthy local gateway.

## Root cause

`crates/duduclaw-gateway/src/protocol.rs` defines a tagged enum:

```rust
#[serde(tag = "type")]
pub enum WsFrame {
    #[serde(rename = "req")]  Request  { id, method, params },
    #[serde(rename = "res")]  Response { id, ok, payload, error },
    #[serde(rename = "event")] Event   { event, payload, seq, state_version },
}
```

The clients send `{"jsonrpc":"2.0", method, params, id}` — no `type` field.
`serde_json::from_str::<WsFrame>` therefore fails, the handshake arm falls
through to `_ => Err(())`, and the gateway logs
`WebSocket auth failed – closing connection` and drops the socket.

The client sees only a closed socket, so it rejects all in-flight promises with
its own generic message. Nothing anywhere names the real cause.

```
client → {"jsonrpc":"2.0",…}   ──►  WsFrame deserialize fails
                                     └─► "auth failed", socket closed
client ← (close)               ──►  reject('connection closed')   ← misleading
```

The read path is wrong in the same way: replies are matched on `f.result` and
`f.error.message`, but the gateway sends `ok` plus `payload`, and `error` is a
bare JSON value (usually a plain string). Server-pushed `event` frames carry no
`id` and are not filtered out, so they can be mistaken for responses.

## Scope

| Client | RPC path | Status |
|---|---|---|
| `clients/vscode` | `src/extension.ts` | **fixed 2026-08-16**, not yet committed |
| `clients/chrome` | `sidepanel.js:104,118` | broken |
| `clients/streamdeck` | `src/plugin.ts:131` (+ built `bin/plugin.js`) | broken |
| `clients/obsidian` | chat only (`type: auth` / `user_message`) | unaffected |
| `clients/wordpress` | chat only | unaffected |

The chat channel (`/ws/chat`) uses a different, correct protocol, which is why
conversation works in every client while dashboard RPC does not. That split is
what let the bug ship unnoticed.

## Fix applied to VS Code (reference for the other two)

Send with the tag, and read the real reply shape:

```ts
// send
sock.send(JSON.stringify({ type: 'req', id, method, params }));

// receive
if (f.type !== 'res' || f.id == null) return;   // ignore `event` pushes
if (f.ok === false) {
  const msg = typeof f.error === 'string' ? f.error : JSON.stringify(f.error);
  p.reject(new Error(msg || 'request failed'));
} else {
  p.resolve(f.payload);
}
```

## Remaining work

1. Apply the same change to `clients/chrome/sidepanel.js`.
2. Apply it to `clients/streamdeck/src/plugin.ts` **and rebuild** the committed
   bundle at `com.duduclaw.deck.sdPlugin/bin/plugin.js` — editing only the
   source would leave the shipped artifact broken.
3. Commit the VS Code fix.

## Worth considering

- A tiny shared frame helper (or at minimum a documented snippet in
  `docs/api/`) so the fourth client does not reinvent this a fourth time.
- The gateway currently answers an unparseable first frame with
  `WebSocket auth failed`. Distinguishing "malformed frame" from "bad
  credentials" would have made this a five-minute diagnosis instead of a hunt.

## Acceptance

- Approvals load in VS Code, Chrome, and Stream Deck against a live gateway.
- A deliberately malformed frame produces a log line that names the parse
  failure rather than an auth failure.

## Evidence

Gateway log at the moment the VS Code panel reported `connection closed`:

```
13:16:08  INFO  New WebSocket connection established
13:16:08  WARN  WebSocket auth failed – closing connection
```

Hand-rolled probe against the same gateway — untagged frame vs tagged frame:

```json
{"type":"res","id":"","ok":false,"error":"expected connect message"}
{"type":"res","id":"1","ok":true,"payload":{"status":"authenticated", …}}
```

## Related code

- `crates/duduclaw-gateway/src/protocol.rs` — `WsFrame`
- `crates/duduclaw-gateway/src/server.rs` — WS `connect` handling
- `clients/{vscode,chrome,streamdeck}` — the three RPC implementations
