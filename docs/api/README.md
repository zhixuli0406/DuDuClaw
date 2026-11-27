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
a proven command-free Claude CLI turn. Both built-in tools and MCP count:
any third-party MCP server makes the proof fail; every command-capable DuDuClaw
tool must be unavailable under the MCP server's actual allowed/denied selectors.
A `mcp__duduclaw__*` allowlist includes command tools. The shared inventory
`COMMAND_CAPABLE_MCP_TOOLS` in `duduclaw-core/src/types.rs` documents each tool;
it includes `execute_program`, `office_script`, desktop input/session tools,
workspace writes and `llamafile_start`. Screenshots and workspace reads cannot
issue commands. Fixed-purpose data/OS queries are not general command interfaces.

A protected-mode flag alone does not establish isolation. Non-Claude, local
inference and API execution are refused for listed non-Admin main turns unless
isolated. Both Claude spawn paths recheck the final tool arguments and the
strict frozen MCP file; every runtime fallback attempt rechecks its actual
provider. Mapped human channel turns use the host-owned principal for this gate.
Unreadable or ambiguous inputs fail closed, with `MAIN_TURN_REFUSAL`.

Only authenticated dashboard JWT sessions can obtain the list-based WebChat
conversation/assignment grant. Member API keys and other credentials cannot
inherit it from their account, even when that account appears on the list.
`users.me.main_agent_access` and list effectiveness use the in-memory roster
and strict read-only configuration checks; they never regenerate resolved
employee configuration. Live Admin turns retain their existing access.
Management and scheduling retain their separate Admin-only gate.

[openapi.yaml](openapi.yaml) is a partial historical discovery reference,
with selected sections maintained separately. Its `/rpc/<method>` paths are
**pseudo-paths**, not HTTP endpoints. It is not an exhaustive method list or
the member compatibility contract. Actual routing and permissions live in
`crates/duduclaw-gateway/src/handlers/` and the gateway-owned namespace modules.
