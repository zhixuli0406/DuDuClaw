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

## Main AI employee access list (1.86.0)

Enterprise Admin dashboard sessions can read `agents.main_access.get` and save
`agents.main_access.set` (`user_ids`, plus `acknowledged: true` when adding people).
Both responses contain `users`, `risk_notice` and `effective`. An empty list has
`effective: true`. A non-empty list has `effective: false` and `ineffective_reason`
when any main-role or configured default employee can run shell commands, or its
configuration cannot be determined. Saving still succeeds; the warning does not
discard the list.

The safeguard accepts isolated employee execution (`isolation_required()`), or
a proven shell-free effective Claude CLI tool list. A protected-mode flag alone
does not establish isolation. Other runtimes, cross-runtime fallback and API
routes conservatively remain ineffective without isolation. Malformed tool
lists fail closed. `users.me.main_agent_access` reflects this safeguard for
active listed users; live Admin accounts retain access. Only dashboard sessions
receive this availability flag.

This package exposes target-aware admission helpers for later conversation and
assignment integration; existing entry points retain their Admin-only gate.

[openapi.yaml](openapi.yaml) is a partial historical discovery reference,
with selected sections maintained separately. Its `/rpc/<method>` paths are
**pseudo-paths**, not HTTP endpoints. It is not an exhaustive method list or
the member compatibility contract. Actual routing and permissions live in
`crates/duduclaw-gateway/src/handlers/` and the gateway-owned namespace modules.
