# DuDuClaw dashboard RPC API

The gateway uses JSON text `WsFrame` messages over WebSocket at
`ws://<host>:<port>/ws` (default port 18889). Member clients authenticate the
first `connect` frame with a JWT access token obtained through HTTP login.

Read the [draft member-facing compatibility contract](member-contract.md) for
the actual transport, login and refresh flow, member method inventory, key
response fields and two-minor-version compatibility promise. Its inventory
is pending confirmation against the distributor's actual calls.

```json
{"type":"req","id":"connect-1","method":"connect","params":{"jwt":"<access_token>"}}
{"type":"req","id":"me-1","method":"users.me","params":{}}
```

Responses use `type: "res"`, `ok` and either `payload` or `error`. This is not
JSON-RPC 2.0, and member login does not use Ed25519 challenge signing.
`/ws/chat` has a separate WebChat protocol described in the contract.

[openapi.yaml](openapi.yaml) is a partial historical discovery reference,
with selected sections maintained separately. Its `/rpc/<method>` paths are
**pseudo-paths**, not HTTP endpoints. It is not an exhaustive method list or
the member compatibility contract. Actual routing and permissions live in
`crates/duduclaw-gateway/src/handlers/` and the gateway-owned namespace modules.
