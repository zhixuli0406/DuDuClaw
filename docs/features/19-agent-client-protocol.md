# ACP and A2A interfaces

DuDuClaw has separate interfaces for IDE conversations and external task submission. Their transports, credentials and task lifetimes differ.

| Interface | Entry point | Purpose |
|---|---|---|
| Agent Client Protocol v1 | `duduclaw acp`, stdio | IDE sessions and prompt turns |
| Legacy A2A command | `duduclaw acp server`, stdio | Existing local scripts using `agent/discover`, `message/send`, `tasks/send` / `tasks/get` / `tasks/cancel` |
| A2A v1.0 JSON-RPC, since v1.80.0 | Gateway `POST /a2a` | Authenticated external task-board goals |
| MCP | `duduclaw mcp-server` or the separate HTTP server | Tools for AI runtimes and MCP clients |

## Gateway A2A v1.0 (since v1.80.0)

The gateway publishes a signed Agent Card at `/.well-known/agent-card.json`. It advertises the JSON-RPC interface, protocol version `1.0`, the actual enabled skills of employees explicitly exposed by the operator, and Dashboard Bearer JWT authentication. Signing protects card integrity; it does not authenticate a caller or grant access to a skill. The card advertises no streaming or push notification support. Only enabled skills with nonempty names and descriptions are published, tagged `employee:<id>` for their actual employee. If no employee is exposed or no publishable skill exists, the public card returns HTTP 503; an authenticated caller with no available skills receives `-32007` from `GetExtendedAgentCard`. `SendMessage` requires an exposed employee with at least one publishable skill. If the signing key is unavailable, discovery returns HTTP 503 rather than an unsigned card.

No employees are exposed by default. Configure the existing employee ids in `config.toml`:

```toml
[a2a]
exposed_agents = ["researcher"]
public_url = "https://host.example/a2a"
trusted_proxy_cidrs = []
```

`public_url` is the externally reachable HTTPS URL for `/a2a`; configure TLS at the gateway's reverse proxy. Exposing an employee only narrows the existing authorization checks. It does not grant a Dashboard account access to that employee, enable a disabled skill, or change tool capabilities. An empty list, unreadable configuration or invalid A2A configuration refuses task access. Removing an employee from the list also removes A2A access to that employee's earlier tasks.

`trusted_proxy_cidrs` defaults to `[]` and accepts at most 32 valid IPv4/IPv6 CIDRs. Configure only the immediate proxies you control. A matching proxy must overwrite `X-Forwarded-For` with one client IP: a missing, duplicate, chained or malformed value is rejected. Headers from other peers are ignored. Without this configuration, clients behind one proxy share its peer-IP bucket within each traffic class. For example, use `trusted_proxy_cidrs = ["192.0.2.10/32"]` only when that address is your immediate proxy; it does not grant request authorization.

Every RPC requires `Authorization: Bearer <Dashboard access JWT>` and `A2A-Version: 1.0`. Obtain the JWT through the existing Dashboard account login. This endpoint does not accept the legacy shared admin token or MCP HTTP-server API keys. The public card is discoverable without authentication; `GetExtendedAgentCard` requires an authenticated caller and preserves all access restrictions.

An absent or empty version is treated as `0.3`, which this HTTP interface does not support: the response is `VersionNotSupportedError` (`-32009`). Use the PascalCase v1.0 methods. The existing stdio names `message/send` and `tasks/get` are HTTP aliases, but still require the `1.0` header and v1.0 request objects. They do not enable a 0.3 transport.

### Submit and poll

The following request example has not been verified against a live gateway. Replace the host and employee id, and supply a valid Dashboard JWT in `DUDUCLAW_DASHBOARD_JWT`.

```sh
curl --fail-with-body 'https://host.example/a2a' \
  -H "Authorization: Bearer $DUDUCLAW_DASHBOARD_JWT" \
  -H 'A2A-Version: 1.0' \
  -H 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{"metadata":{"agentId":"researcher"},"message":{"messageId":"request-001","role":"ROLE_USER","parts":[{"text":"Summarize the public project documentation."}]},"configuration":{"returnImmediately":true}}}'
```

`metadata.agentId` selects the exposed employee. `message.messageId` is required; `role` must be `ROLE_USER`, with plain-text `parts`. Set `configuration.returnImmediately` to `true`: this implementation creates an asynchronous goal through the existing task-board goal creation path and returns `result.task`. Execution uses the employee's configured runtime and existing goal-loop gates. Submission requires direct Operator access or Admin; a team-only grant is insufficient. Each message is limited to 4,000 characters and the HTTP body to 64 KiB. Only new tasks are accepted: omit `message.contextId` and `message.taskId`; the returned `contextId` equals the task id. `historyLength` must be omitted or `0`. The authenticated account and `messageId` form a durable unique key. Resending the same full `params` object returns the original task; reusing the id with different parameters is rejected. JSON object key order does not matter. Deleting the task does not make the id reusable or recreate the task. Retries still require current authorization.

The duplicate check compares the full canonical `params`: object key order is ignored, array order is retained, and omitted fields differ from explicitly supplied defaults. A conflicting payload returns `-32602` (`INVALID_ARGUMENT`); a retry after deletion returns `-32001` (`TASK_NOT_FOUND`). Pagination tokens bind the owner and the context, status, status-timestamp and artifact filters; `pageSize` may change between pages.

Use the returned task id for subsequent requests to the same endpoint, with the same authentication and version headers:

```json
{"jsonrpc":"2.0","id":2,"method":"GetTask","params":{"id":"<task-id>"}}
{"jsonrpc":"2.0","id":3,"method":"ListTasks","params":{"pageSize":20}}
{"jsonrpc":"2.0","id":4,"method":"CancelTask","params":{"id":"<task-id>"}}
{"jsonrpc":"2.0","id":5,"method":"GetExtendedAgentCard","params":{}}
```

`ListTasks` accepts `contextId`, `status`, `statusTimestampAfter` and `includeArtifacts`, with `pageSize` from 1 to 100 and opaque `pageToken` cursors. Pass `nextPageToken` for the next page. Tasks are ordered by a monotonic creation sequence. The first page fixes an upper sequence bound that subsequent tokens retain; tasks created later appear only in a new listing without a token. Permissions and status filters are checked again on every page, so this is an inventory boundary, not a frozen copy of task state. `totalSize` counts only currently readable tasks; more than 10,000 stored tasks for one caller returns `RESOURCE_EXHAUSTED` instead of a partial count. Reads and cancellation are limited to tasks created by the same authenticated account through A2A. They also recheck employee exposure, live account/employee permissions and the task's audience. Ownership is stored on the task and inherited by child tasks. The same owner restriction applies to ordinary Dashboard task lists, reads, edits, deletion, stopping, comments, attachments, activity and push events. Admin has no bypass, and no separate administrative override is provided. Ownership records retained after deletion continue protecting remaining evidence. External MCP callers without the Dashboard owner principal are refused; the authenticated employee assigned to execute the task retains necessary task access.

Task status uses `TASK_STATE_*`, including `TASK_STATE_SUBMITTED`, `TASK_STATE_WORKING`, `TASK_STATE_COMPLETED`, `TASK_STATE_FAILED`, `TASK_STATE_INPUT_REQUIRED` and `TASK_STATE_CANCELED`. Approval or human-input waits are not completion. Cancellation applies the existing task-tree stop fence. A pending or uncertain stop must be polled; it is not proof that an external process or side effect has physically stopped. Canceling a terminal task returns `TaskNotCancelableError` (`-32002`).

### Supported scope and errors

The retained owner check also applies to `forward.recent` and `forward.chain`, including deleted A2A tasks; Admin cannot bypass another caller's ownership. `tasks.flow_metrics` excludes other A2A owners' tasks before calculating counts, timing and round statistics. Non-A2A task access remains unchanged.

Before any task-store schema migration, an existing A2A schema or receipt requires a readable `task_access_principals` table. If that table is missing or unreadable, opening the store fails; it is never rebuilt empty from surviving tasks. Restore the intact registry from a consistent backup. Fresh databases and legacy databases without A2A traces still migrate.

Existing `keys/a2a-signing.ed25519` keys must be regular files, not symbolic links. On Unix they must belong to the gateway's effective user and have mode exactly `0600`; Windows skips Unix ownership/mode checks. Invalid files are refused without replacing them or changing their permissions. Peer-IP admission is recorded before the global bucket: requests rejected by the peer limit do not consume global capacity.

| Operation | Support |
|---|---|
| `SendMessage` | New asynchronous plain-text goal |
| `GetTask`, `ListTasks` | Own A2A tasks, subject to current access |
| `CancelTask` | Own task-tree stop request |
| `GetExtendedAgentCard` | Authenticated card; no extra authority |
| `SendStreamingMessage`, `SubscribeToTask` | Unsupported; no SSE |
| Blocking responses, task continuation, push notification configuration | Unsupported |

JSON-RPC protocol errors use the standard error codes. A2A errors use `-32001` through `-32009` where applicable, with `error.data` as an array of typed `google.rpc.ErrorInfo` details. Unauthenticated and unauthorized requests use HTTP 401/403. Input screening, method authorization and security audit apply before task operations. Public discovery, rejected authentication and authenticated RPC each have separate 120 requests/minute IP buckets. Their separate global limits are 600, 600 and 1,200 requests/minute respectively; RPC also retains the 60/minute caller limit. Anonymous traffic cannot spend the authenticated RPC budget. Each IP/caller limiter holds at most 10,000 keys; saturation refuses new keys, and stale-key cleanup is throttled. This interface is HTTP-only; WebSocket `a2a.*` calls are refused. See [Security defense](05-security-defense.md).

## IDE Agent Client Protocol

For an IDE agent panel, use `duduclaw acp`. It implements `initialize`, `session/new`, `session/prompt`, `session/update` and `session/cancel`. An unconfigured home reports `AUTH_REQUIRED` and points to terminal onboarding. Prompt turns use the gateway reply pipeline and the Main-role employee.

Zed configuration:

```json
{"agent_servers":{"DuDuClaw":{"command":"duduclaw","args":["acp"]}}}
```

The gateway A2A HTTP methods are not IDE session methods. Do not configure an IDE panel with `duduclaw acp server`.

## Legacy A2A over stdio

`duduclaw acp server` retains its existing local protocol. Its `message/send` path writes to the gateway's delegation bus as `a2a-client`; the existing `[acp] trusted` policy applies. Its process-local task index and best-effort bus observations are separate from the durable HTTP task-board goals. Restarting that stdio process loses its indexed task ids; a missing bus response does not prove completion. The legacy in-memory `tasks/send` manager is also distinct from the HTTP task board.

The HTTP exposure list and JWT ownership rules described above belong to `/a2a`; they do not redefine the stdio command's trust model. The earlier versions of this page incorrectly described HTTP as a future discovery-only extension and advertised streaming/multi-turn task behavior without an implemented transport. The supported HTTP operations are listed above.
