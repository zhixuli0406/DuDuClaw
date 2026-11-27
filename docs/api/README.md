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

Enterprise Admin dashboard sessions use `agents.main_access.get` and
`agents.main_access.set` to read and replace the initially empty list. Saving
accepts `user_ids`; adding people also requires `acknowledged: true`. Responses
include `users`, `risk_notice` and `effective`. For a non-empty list,
`effective` is true only in protected mode with isolated employee execution;
otherwise `ineffective_reason` explains that only Admins can use the main
employee. An empty list reports `effective: true` without granting anyone access.
Saving an inactive list preserves it for later; removing command tools does
not activate it.

Active listed Enterprise members may converse and assign ordinary work only
under protected execution, with access checked again before execution.
Personal API keys can never use the main AI employee, including keys held by
Admins. `users.me.main_agent_access` reports dashboard-session availability.
Scheduling and employee management stay Admin-only. Work for a listed
non-Admin person cannot use administrator tools. See
[Department managers](../features/79-department-managers.md) for memory exposure,
delegation and dashboard integration limits.

[openapi.yaml](openapi.yaml) is a partial historical discovery reference,
with selected sections maintained separately. Its `/rpc/<method>` paths are
**pseudo-paths**, not HTTP endpoints. It is not an exhaustive method list or
the member compatibility contract. Actual routing and permissions live in
`crates/duduclaw-gateway/src/handlers/` and the gateway-owned namespace modules.
