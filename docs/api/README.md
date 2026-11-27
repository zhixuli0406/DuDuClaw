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
`effective: true`. A non-empty list takes effect only in protected mode with
isolated employee execution. Otherwise, it has `effective: false` and an
`ineffective_reason` explaining that only Admins can use the main AI employee.
Saving still succeeds and keeps the list for when protected execution is enabled.
Removing shell tools or changing the employee's runtime does not activate the list.

In protected mode, active listed Enterprise members can talk to the main AI
employee and assign it work. A protected-mode flag alone is insufficient: the
host must also have the employee isolation architecture installed. Each final
execution checks the person's current access and the protected execution state.
Without protected execution, the list grants no access; only Admins can use the
main AI employee through these conversation and assignment entry points.

For WebChat, list access requires an authenticated dashboard session. Member API
keys never grant access to the main AI employee, even if their account is listed
or is an Admin. Other credentials cannot inherit the dashboard session grant.
`users.me.main_agent_access` reports this availability: non-Admins must be active,
listed Enterprise members using a dashboard session while protected execution
is enabled. Live Admin turns retain their existing access. Management and
scheduling remain Admin-only.

[openapi.yaml](openapi.yaml) is a partial historical discovery reference,
with selected sections maintained separately. Its `/rpc/<method>` paths are
**pseudo-paths**, not HTTP endpoints. It is not an exhaustive method list or
the member compatibility contract. Actual routing and permissions live in
`crates/duduclaw-gateway/src/handlers/` and the gateway-owned namespace modules.
