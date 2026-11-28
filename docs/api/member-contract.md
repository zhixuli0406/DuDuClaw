# Member-facing RPC compatibility contract

**Draft — since 1.85.0.** This is the native member UI's verified candidate
inventory. The distributor's actual method and field inventory is pending;
this is not a claim to cover every integration. “Since” means the first
contract version, not the version that introduced a method.

The source of the table is `MEMBER_CONTRACT_METHODS` in
[`member_contract.rs`](../../crates/duduclaw-gateway/src/handlers/member_contract.rs).
Gateway tests exercise dispatch with a real Employee account, live `users.db`
and normal direct bindings, check successful read payload fields, and compare
this table's method names with the constant. These tests are added with this
draft; they have not been executed as part of its authoring.

This contract describes member dashboard RPCs. Personal API keys use a restricted
subset of this table, described in the authentication section below; existing
ACL checks still apply. Account credential operations remain browser-only.

## Transport and authentication

The dashboard connects to `ws://<host>:<port>/ws` (default port 18889), or the
corresponding `wss://` URL behind TLS. Frames are JSON text using the gateway's
[`WsFrame`](../../crates/duduclaw-gateway/src/protocol.rs) envelopes. The wire
protocol does **not** use a `jsonrpc` version field, numeric JSON-RPC error
codes or a `result` envelope.

The member connection starts with this request as its first frame:

```json
{"type":"req","id":"connect-1","method":"connect","params":{"jwt":"<access_token>"}}
```

A successful JWT handshake responds with the same id:

```json
{"type":"res","id":"connect-1","ok":true,"payload":{"status":"authenticated","boot_id":"<boot-id>","user":{"id":"<user-id>","email":"member@example.com","role":"employee"},"must_change_password":false}}
```

The server requires this initial connect frame within 10 seconds. `connect`
is a transport handshake handled in `server.rs`, outside the table below.
A compatibility dispatch handler for `connect` (and `connect.challenge`)
still exists, but it is not the member login flow. Members do not sign an
Ed25519 challenge. Legacy gateway-token and local appliance bootstrap paths
are separate from member JWT authentication; lack of a configured legacy
`auth_token` does not mean member authentication is disabled.

After the handshake, requests and responses use these shapes:

```json
{"type":"req","id":"me-1","method":"users.me","params":{}}
{"type":"res","id":"me-1","ok":true,"payload":{"user":{},"bindings":[],"email_setup":{"placeholder":false,"password_required":true}}}
{"type":"res","id":"bad-1","ok":false,"error":"permission denied"}
{"type":"res","id":"bad-2","ok":false,"error":{"code":"invalid_setting","key":"agent_id","message":"agent_id: required"}}
```

Request `id` and `method` are strings. Omitted `params` deserializes as null;
use an object for ordinary calls. Response ids correlate requests; `payload`
and `error` are omitted when absent. An error is a JSON value: many handlers
return a string; others return an object with a code and message. Clients must
handle both. There is no universal numeric error-code schema. Example account
objects above are abbreviated; protected fields are specified below.

Server pushes use `{"type":"event","event":"<name>","payload":{...}}`.
`seq`, `state_version` and `boot_id` are optional event metadata. These pushes
are separate from request responses. Refetch relevant lists after reconnect,
a changed boot id or `events_lost`; there is no replay guarantee.

`/ws/chat` is a separate WebChat protocol, not a dispatch method. Its first
frame is `{"type":"auth","token":"<access_token>"}`; do not send `/ws`
request envelopes there. Attachment assignments use `POST /api/tasks/rpc`
with `Authorization: Bearer <access_token>` and the endpoint's attachment
format, rather than an additional method named `tasks.goal_schedule`.

## Login, expiry and renewal

1. `POST /api/login` with JSON `{"email":"member@example.com","password":"..."}`.
   A completed login returns `access_token`, `refresh_token` and `user`.
   Channel-code login is also available through `/api/otp/request` and
   `/api/otp/verify`.
2. If login instead returns `mfa_required` and `challenge_id`, complete
   `/api/mfa/verify`. If it returns `mfa_enrollment_required` and
   `enrollment_token`, complete `/api/mfa/enroll/*`. These are intermediate
   states, not a completed login. See [MFA compatibility](../guides/deprecations.md#mfa).
3. The default access-token lifetime is **30 minutes**; the default
   refresh-token lifetime is **7 days** (`duduclaw-auth/src/jwt.rs`).
4. Before access expires, `POST /api/refresh` with JSON
   `{"refresh_token":"<refresh_token>"}`. Success returns only
   `{"access_token":"<new_access_token>"}`. Keep the existing refresh token:
   this call does not rotate it or extend its seven-day lifetime.
5. Use the new access token for subsequent HTTP calls and a new WebSocket
   connection's `connect.params.jwt`. There is no in-place token replacement
   RPC. A refused or expired refresh requires login again; a 429 is a throttle
   response, not proof that the credential has expired.

Existing `/ws` connections are checked against live account state. Access-token
expiry closes JWT sockets when MFA is enforced, or for MFA-enrolled Enterprise
accounts; it does not universally close every socket at 30 minutes. Clients
should renew tokens regardless. Role changes, suspension and binding changes
remain effective through the existing live checks. A session `jti`, including
one preserved by refresh, is not fresh authentication proof: creating a member
key additionally requires the current password (without MFA) or a fresh MFA factor.

The native same-site dashboard also supports remembered-session cookies via
`/api/session/refresh`. This is distinct from the seven-day refresh-token API;
a cross-origin or server-side client must not assume it has that cookie.
The default remembered-session retention is 180 days, configurable from 1 to
365 days. None of these lifetimes grants additional method permissions.

## Draft method inventory

Read/write classifies intent, not an HTTP verb or a grant. Employee is the
account role; Viewer, Operator and Owner are direct AI-employee binding levels.
Owner is an ordinary binding, not an Admin account. Record ownership, task
privacy, deployment edition, feature switches, live bindings and confirmation
requirements still apply. In particular, routine execution requires an eligible
AI employee with task sandbox disabled. Google self-service requires Enterprise
and its existing signed-in browser connection flow.

Key fields are required **top-level `payload` field names on the documented
read shape**, including fields whose value may be null. Nested values and
optional/additive fields are not exhaustively enumerated. Writes currently have
no response-field snapshot (`—`). The scoped `skills.list {agent_id}` shape is
listed; its unscoped aggregate shape has `global_skills`, `agents`, `scanned`,
`usage_error` and `usage_truncated`. `about.get` uses its default full response,
not the `brand_revision_only` projection.

`users.me` additionally preserves `user.avatar_agent_id` (nullable).
Setting an avatar remains an Admin operation. A member with an assigned avatar
cannot use `users.set_primary_agent` to replace it.

<!-- member-contract-methods:start -->
| Method | Kind | Purpose | Key response fields | Since |
|---|---|---|---|---|
| `users.me` | Read | Read the signed-in account and direct bindings | `user`, `bindings`, `email_setup` | 1.85.0 |
| `about.get` | Read | Read product and vendor information | `product_name`, `vendor`, `version`, `tier`, `brand_revision` | 1.85.0 |
| `onboarding.get` | Read | Read personal first-login progress | `enabled`, `dismissed`, `password_done`, `steps`, `examples` | 1.85.0 |
| `onboarding.update` | Write | Update personal first-login progress | — | 1.85.0 |
| `users.set_primary_agent` | Write | Choose a primary AI with an Operator or Owner binding; an assigned avatar locks this preference | — | 1.85.0 |
| `agents.list` | Read | List visible AI employees | `agents` | 1.85.0 |
| `agents.callable` | Read | List AI employees the caller can call | `agents`, `default_agent`, `unrestricted` | 1.85.0 |
| `agents.org_chart` | Read | Read the visible AI organization chart | `agents` | 1.85.0 |
| `agents.update` | Write | Update an AI employee with a direct Owner binding | — | 1.85.0 |
| `chat.sessions.list` | Read | List conversations for a bound AI employee | `sessions` | 1.85.0 |
| `chat.sessions.history` | Read | Read an authorized conversation | `session_id`, `agent_id`, `messages` | 1.85.0 |
| `chat.sessions.restore` | Write | Restore an owned archived conversation | — | 1.85.0 |
| `knowledge.citations.for_session` | Read | Read citations for an owned WebChat conversation | `citations` | 1.85.0 |
| `skills.recent` | Read | Read personal recently used skills | `skills` | 1.85.0 |
| `decisions.list` | Read | Read open decisions for a bound AI employee | `decisions` | 1.85.0 |
| `tasks.list` | Read | List tasks for a bound AI employee | `tasks` | 1.85.0 |
| `tasks.create` | Write | Create or schedule a task for a callable AI employee | — | 1.85.0 |
| `tasks.update` | Write | Update an authorized task | — | 1.85.0 |
| `tasks.comment` | Write | Comment on an authorized task | — | 1.85.0 |
| `tasks.comments` | Read | Read comments on an authorized task | `comments` | 1.85.0 |
| `tasks.assign` | Write | Assign an authorized task to a callable AI employee | — | 1.85.0 |
| `tasks.goal_decide` | Write | Decide an authorized goal with Operator access | — | 1.85.0 |
| `tasks.goal_create` | Write | Create or schedule a goal for a callable AI employee | — | 1.85.0 |
| `tasks.skill_assign` | Write | Assign a skill using an authorized runner | — | 1.85.0 |
| `cron.list` | Read | List visible routines | `tasks` | 1.85.0 |
| `cron.templates` | Read | Read routine templates | `templates` | 1.85.0 |
| `cron.add` | Write | Create a prompt-only routine for an Operator-bound AI employee | — | 1.85.0 |
| `cron.update` | Write | Update a manageable routine | — | 1.85.0 |
| `cron.pause` | Write | Pause a manageable routine | — | 1.85.0 |
| `cron.resume` | Write | Resume an eligible manageable routine | — | 1.85.0 |
| `cron.remove` | Write | Remove a manageable routine | — | 1.85.0 |
| `cron.run_now` | Write | Run an eligible manageable routine now | — | 1.85.0 |
| `reminders.list` | Read | List visible reminders | `reminders` | 1.85.0 |
| `reminders.cancel` | Write | Cancel an owned or Operator-authorized reminder | — | 1.85.0 |
| `assign_schedules.list` | Read | List personal scheduled assignments | `schedules`, `max_active`, `min_interval_minutes` | 1.85.0 |
| `assign_schedules.pause` | Write | Pause a personal scheduled assignment | — | 1.85.0 |
| `assign_schedules.resume` | Write | Resume a personal scheduled assignment | — | 1.85.0 |
| `assign_schedules.remove` | Write | Remove a personal scheduled assignment | — | 1.85.0 |
| `skills.list` | Read | List skills for a Viewer-bound AI employee | `agent_id`, `skills`, `scanned`, `skill_state`, `can_manage`, `usage_error`, `usage_truncated` | 1.85.0 |
| `skills.search` | Read | Search local and configured market skills | `skills`, `source`, `total_indexed`, `hub_errors` | 1.85.0 |
| `skills.install_request` | Write | Request a skill installation through the approval chain | — | 1.85.0 |
| `skills.share` | Write | Share a skill from an Owner-bound AI employee | — | 1.85.0 |
| `skills.shared` | Read | List shared skills | `skills` | 1.85.0 |
| `skills.adopt` | Write | Adopt a shared skill into an Owner-bound AI employee | — | 1.85.0 |
| `skills.leaderboard` | Read | Read the visible skill ranking | `leaderboard`, `metric`, `note` | 1.85.0 |
| `memory.search` | Read | Search memory for a Viewer-bound AI employee | `entries`, `signals` | 1.85.0 |
| `memory.browse` | Read | Browse memory for a Viewer-bound AI employee | `entries`, `signals` | 1.85.0 |
| `memory.key_facts` | Read | Read key facts for a Viewer-bound AI employee | `entries` | 1.85.0 |
| `memory.history` | Read | Read the temporal history of a memory fact | `subject`, `predicate`, `chain`, `current_id` | 1.85.0 |
| `wiki.pages` | Read | List wiki pages for a Viewer-bound AI employee | `pages`, `exists` | 1.85.0 |
| `wiki.read` | Read | Read a wiki page for a Viewer-bound AI employee | `content`, `path` | 1.85.0 |
| `wiki.search` | Read | Search a wiki for a Viewer-bound AI employee | `hits` | 1.85.0 |
| `shared_wiki.pages` | Read | List shared wiki pages visible to the caller | `pages`, `exists` | 1.85.0 |
| `me.memory.list` | Read | List memories about the signed-in person | `items`, `next_cursor` | 1.85.0 |
| `me.memory.forget` | Write | Forget a memory about the signed-in person | — | 1.85.0 |
| `dashboard.layout.get` | Read | Read the personal dashboard layout | `layout` | 1.85.0 |
| `dashboard.layout.set` | Write | Save the personal dashboard layout | — | 1.85.0 |
| `activity.list` | Read | Read activity for a bound AI employee | `events`, `total` | 1.85.0 |
| `users.change_password` | Write | Change the signed-in account password | — | 1.85.0 |
| `users.change_email` | Write | Change the signed-in account email | — | 1.85.0 |
| `me.status_tokens.list` | Read | List personal external-status credentials | `enabled`, `max_active`, `endpoint_path`, `tokens` | 1.85.0 |
| `me.status_tokens.create` | Write | Create a personal status-only credential when enabled | — | 1.85.0 |
| `me.status_tokens.revoke` | Write | Revoke a personal status-only credential | — | 1.85.0 |
| `me.google.write_confirmations` | Read | Read only your pending Google Drive write confirmations (display projection only) | `requests` | 1.86.0 |
| `me.google.write_confirm` | Write | Confirm or cancel your single Google Drive write (`id`, `approve`: boolean) | `decided` | 1.86.0 |
| `me.google.chat_confirmations` | Read | Read only your pending Google Chat confirmations | `requests` | 1.85.0 |
| `me.google.chat_confirm` | Write | Confirm or cancel your single Google Chat send | `decided` | 1.85.0 |
| `me.google.status` | Read | Read personal Google connection and consent status (Enterprise) | `connection`, `login_email`, `client_configured`, `chat_read`, `chat_send`, `integration_enabled`, `requests`, `delegated`, `requires_login`, `recent_usage` | 1.85.0 |
| `me.google.connect_start` | Write | Start personal Google authorization (Enterprise) | — | 1.85.0 |
| `me.google.write_connect_start` | Write | Start separate personal Google Drive write authorization (browser session only) | — | 1.86.0 |
| `me.google.write_disconnect` | Write | Remove the local Google Drive write connection (browser session only) | — | 1.86.0 |
| `me.google.connect_finish` | Write | Finish personal Google authorization (Enterprise) | — | 1.85.0 |
| `me.google.connect_paste` | Write | Finish Google authorization using a pasted callback (Enterprise) | — | 1.85.0 |
| `me.google.disconnect` | Write | Disconnect the personal Google account (Enterprise) | — | 1.85.0 |
| `me.google.consents.set` | Write | Set personal Google data-use consent for an AI employee (Enterprise) | — | 1.85.0 |
<!-- member-contract-methods:end -->

## Excluded candidates

These exclusions reflect existing routing and permissions; this work does not
change them.

| Candidate | Reason |
|---|---|
| `approvals.list`, `approvals.decide` | Manager minimum in `dispatch_org.rs` |
| `install_requests.list`, `install_requests.decide` | Manager minimum in `dispatch_org.rs` |
| `budget.incidents` | Manager minimum in `dispatch_org.rs` |
| `tasks.goal_schedule` | No such dispatched method; use `schedule` on `tasks.create` or `tasks.goal_create` |
| `connect`, `/ws/chat` | Transport entry points documented above, outside the dispatched compatibility inventory |
| `users.set_avatar`, `users.avatar_conflicts` | Admin-only management methods, outside the member inventory |

`users.list` is also Admin-only; its avatar field is not a member-facing
read contract. The presence of a method call in shared React code does not
establish Employee permission.

## Compatibility promise

Starting with 1.85.0, listed method names, their supported member operations and
listed key fields stay compatible. Additive fields are allowed; clients should
ignore fields they do not recognize. Existing authorization and privacy checks
remain in force, and a listing never promises access to somebody else's data.
The draft label means the distributor's coverage is pending, not that an
already listed method may silently break.

Any rename or removal of a listed method or key field must be announced in
`CHANGELOG.md` under **Deprecated**, with its replacement and the earliest
removal version. The old name/field must continue working for **two minor
versions**. For example, a deprecation in 1.85 keeps the old form through 1.86;
1.87 is the earliest removal. A replacement must coexist during this window;
changing the table or its test expectations alone is not a migration policy.
See [RPC deprecation policy](../guides/deprecations.md#rpc-methods).

## Evidence and test limits

The tests live in
[`member_contract_tests.rs`](../../crates/duduclaw-gateway/src/handlers/tests/member_contract_tests.rs).
They call `MethodHandler::handle` using an Employee from `users.db`, an Owner
binding to a fixture employee, scoped tasks/routines/schedules and an owned
WebChat session. Every read must return success with its required fields;
write probes may return validation, missing-resource or unavailable-service
errors, but must not return an unknown-method or permission/role refusal.
They do not run AI work, external OAuth exchanges or a native browser journey.
Transport/login details above were checked against `server.rs`, `protocol.rs`,
`webchat.rs`, `server_mfa.rs` and `duduclaw-auth/src/jwt.rs`; socket authentication
itself is outside these dispatch tests. Build and test execution remains pending.

## Personal member API keys (1.85.0)

An administrator must first enable `[member_api_keys] enabled = true` in
`config.toml`, or use the member-key section on **My account → External access**.
The default is off. An unreadable or malformed configuration disables both
key authentication and key management. The administrator can still change the
switch through the normal authenticated configuration UI.

A member signs in with a browser JWT session, then calls
`me.member_keys.create {"name":"My server","expires_in_days":90,"password":"<current password>"}`.
Every account must freshly re-authenticate. Accounts without MFA supply their
current `password`, verified by the existing timing-safe password checker;
MFA accounts supply a fresh `code` (TOTP or unused recovery code) instead, using
the existing step-up verifier and its replay/lockout checks. Attempts share the
login limiter for the actual socket peer IP and account email (five attempts
per 15 minutes); request parameters cannot select that peer. Holding an access
JWT or refresh token alone is insufficient.

The gateway issues an internal, single-use proof bound to `(user, jti,
credential_epoch, "me.member_keys.create")`, valid for at most five minutes.
It consumes the proof before attempting creation, including attempts refused
by a key cap. The proof is never returned to the client or accepted as an RPC
parameter. Account epoch and remembered-session validity are rechecked at the
write. The response is `{"key":{...},"secret":"..."}`.
Only this response contains the plaintext. Admin accounts cannot create keys;
use a separate member account. Creation is never available to an existing key.
Expiry is mandatory: 1–365 days; the UI defaults to 90 and offers 30/90/365.
Each account can have at most five active keys and 50 total retained rows.
On creation, expired or old-epoch rows are marked revoked; revoked rows older
than 90 days are purged. Recent history is retained even when the total cap
refuses creation (`member_key_history_limit_reached`).

`me.member_keys.list {"limit":25,"cursor":"<next_cursor>"}` returns
`{"keys":[...],"next_cursor":null,"max_active":5,"max_total":50,"reauthentication":"password","policy":{"allow_conversation_content":false,"allow_mcp_scopes":[]}}`
(`reauthentication` is `mfa` when enrolled). Omit the cursor for the first page.
Both this method and `member_keys.admin_list` accept `limit` (default 25,
1–100) and an opaque cursor; `next_cursor: null` ends the list. Ordering is
newest creation first, then id, so deleting a previous page's row does not shift
later pages. Invalid limits/cursors return `member_key_invalid_page`. The
Dashboard shows one page at a time; **Next page** advances and **Refresh**
returns to the first page. The create dialog requires the corresponding password
or MFA input and clears it after submission or cancellation.

Admin dashboard-browser RPCs `member_keys.policy_get {}` and `member_keys.policy_set {allow_conversation_content?: bool, allow_mcp_scopes?: string[], acknowledge_risk?: bool}` work even while member keys are disabled. Both return `{readable:true, enabled, allow_conversation_content, allow_mcp_scopes, available_mcp_scopes, risk:{conversation,mcp_scopes}}`. With unreadable or malformed configuration, get returns `{readable:false, message}` and set refuses. A non-table `[member_api_keys]` is unreadable and disables keys. Unknown fields and explicit `null` values are refused.

Enabling conversation content or adding a scope requires `acknowledge_risk:true`; narrowing does not. The `member_keys_policy_changed` audit is written before saving, and an audit failure blocks the write. Generic writers, including `config.raw.set`, `system.update_config`, `google.integration.set` and all ordinary configuration commits, refuse widening these two settings. Backup restore and CLI import strip widening and show a notice directing the administrator to confirm it through the dedicated settings.

`me.member_keys.list {limit?, cursor?}` also returns `policy:{allow_conversation_content,allow_mcp_scopes}`. Conversation permission controls creation only. MCP permissions are checked on every call as the intersection of stored key scopes and current `allow_mcp_scopes`; re-adding a scope restores it for existing keys that were created with that scope.

`me.member_keys.revoke {"id":"..."}` revokes an owned key. Administrators use
`member_keys.admin_list` and `member_keys.admin_revoke {"id":"..."}`; the latter
can revoke any member's key. Lists contain id, name, created/expires/last-used
and revocation timestamps, and active status; the admin list adds the owner.
Neither list includes hashes. Management requires a browser login and is
refused while the master switch is off.

**Do not put this key in browser front-end code; call from your server.** Keep it
in a server-side secret manager. The format is `ddc_member_` followed by the
unpadded base64url encoding of 32 OS-random bytes. The database stores only its
SHA-256 digest. Never put the key in a URL, query string, log or source repository.

Authenticate `/ws` using exactly one credential in the connect frame:

```json
{"type":"req","id":"connect-1","method":"connect","params":{"member_key":"<member-key>"}}
```

For `/ws/chat`, the first frame is:

```json
{"type":"auth","member_key":"<member-key>"}
```

For member HTTP operations, use `Authorization: Bearer <member-key>`: `/api/me`,
`POST /api/tasks/rpc`, file list/download/preview, chat attachment downloads and
knowledge-source upload/original downloads, STT/TTS and AG-UI runs. Task RPC still accepts only its documented
two methods (`tasks.goal_create`, `tasks.skill_assign`) and runs normal dispatch.
The attachment and original-file routes retain their existing ownership and
AI-employee checks. Other HTTP surfaces retain their JWT-only authentication;
this does not authorize arbitrary endpoints. Status credentials and MCP keys
cannot authenticate these member-key paths.

MCP transports accept member keys through the HTTP `Authorization: Bearer`
header or the stdio `DUDUCLAW_MCP_API_KEY` environment variable, for person
tools only. Query-string credentials remain refused. The transports use the
loopback-only `/api/internal/person-key` bridge for live member-key authority
and gateway person-tool execution. Member keys are refused by OAuth consent,
the status endpoint, MCP key validation, legacy Dashboard-token authentication
and JWT-only account/security endpoints.

The person tool `my_tasks_list` accepts a `limit` from 1 to 200 (default 50);
out-of-range values return HTTP 400. It queries at most 50 visible employees
in employee-id order and stops collecting at `5 * limit` rows (at most 1000).
The collected rows are sorted newest-first before applying the result limit.
The result includes `truncated: true` when employees or rows were omitted.
The bridge passes a bounded `limit` to each employee's `tasks.list` query;
SQL ownership checks and a scan cap bound collection before enrichment.
`more_may_exist` contributes to `truncated`. `tasks.list_page` is not exposed
to member keys. Member stdio sessions do not poll for tool-list changes;
the next real request observes current scopes.

Key RPC access is the intersection of `MEMBER_CONTRACT_METHODS`, the member's
**current** role/bindings, and all existing handler gates. Credential management,
`users.change_password`, `users.change_email`, `mfa.*`, `sessions.*`, `config.*`,
`me.google.connect_*`, `me.google.disconnect`, `me.google.write_connect_start`,
`me.google.write_disconnect`, `me.google.consents.set`, `me.google.chat_confirm`,
`me.google.write_confirmations`, `me.google.write_confirm` and
step-up operations are excluded. Unknown methods are denied. The compatibility
table includes browser-only methods; its presence alone is not a key grant.

For `agents.update`, key sessions may send only the explicit non-secret leaf
paths in `MEMBER_KEY_AGENT_UPDATE_FIELDS` beside the contract. The whitelist
covers reviewed identity, model, budget, heartbeat, permission, language and
selected nested settings; normal Owner/admin checks still apply. Unknown
fields, dotted/legacy aliases, arbitrary `advanced_kv`, credential sections
(including all `odoo` and channel objects), and token/secret/password/key/credential
fields are refused with `member_key_forbidden_field` before any settings or
channel side effects. Mixed safe/forbidden requests are rejected as a whole,
including empty strings and null values used to clear credentials.

Every key call and socket heartbeat rechecks the key row, expiration, active
account, role and credential epoch. A key can never gain Admin authority: a
promotion to Admin refuses it. Revocation, offboarding, expiration, password
change, MFA reset or disabling the feature stops further calls; already-open
WebSocket connections close on their next call/heartbeat. Chat streaming also
rechecks identity before delivery. Keys do not use the browser JWT's 30-minute
access-token expiry.

`[member_api_keys] rate_per_minute` defaults to 120 (valid range 1–60000), shared
per key across connections and transports, with a bounded limiter. An exceeded
limit returns code `member_key_rate_limited` (HTTP 429 on direct HTTP operations).
The first use in each ten-minute window creates an `auth_audit_log` row; later
uses increment an aggregate. Last-used time is updated once per window. Write
attempts record the method and key id individually, without parameters, hashes
or plaintext. Creation and revocation are also audited.

The tests added for part B have not been executed in the implementation session;
compilation, target tests and independent security review remain release gates.

Drive write confirmations are browser-session operations. `me.google.write_confirmations` returns
`{ "requests": [...] }` with escaped display fields and an expiry, never raw arguments or grants.
`me.google.write_confirm` accepts `{ "id": "...", "approve": true }` (or `false` to cancel)
and returns `{ "decided": true }`. Only the authenticated requesting member may decide;
managerial roles and chat-admin authority cannot substitute for that member.

### Internal person-key MCP bridge (v1.86)

`POST /api/internal/person-key` accepts only a loopback peer and exactly one
`Authorization: Bearer ddc_member_…` header, with a 64 KiB request-body limit.
The gateway derives the holder from that key on every request. JWTs, MCP keys
and status credentials cannot authenticate this route. This is the gateway
contract for the separate MCP process; MCP transport support is a separate
delivery on the MCP side.

`{"op":"authenticate"}` returns HTTP 200
`{"ok":true,"key_id":"…","user_id":"…","scopes":[…]}`. Scopes are sorted,
deduplicated, and intersect the key's live `mcp_scopes` with the current
`[member_api_keys] allow_mcp_scopes`. This operation records exactly one
`mcp.authenticate` call.

`{"op":"call","tool":"<name>","args":{…}}` returns HTTP 200
`{"ok":true,"result":<value>}`. Only gateway-hosted person tools are accepted:

| Tool | Required scope | Arguments and result |
| --- | --- | --- |
| `my_tasks_list` | `tasks:read` | Optional string `status` and positive integer `limit` (default 50); returns `{"tasks":[…]}`. |
| `my_agents_status` | `agents:status` | Empty `{}`; returns `{"agents":[{"id":"…","name":"…","status":"…","can_call":true}]}`. |

`my_tasks_list` calls the ordinary `tasks.list` handler for each employee
permitted by the holder's live direct/team ACL, passing a gateway-derived
`agent_id` and the optional `status`. Existing privacy projections remain in
effect. Results are merged by descending `created_at` (id breaks ties) and
then truncated to `limit`. The current `tasks.list` RPC does not accept a
limit, and `tasks.list_page` is not in the member-key RPC contract. With no
eligible employee, an ordinary `agents.list` call supplies the member-key
gate and the result is an empty task list.

`my_agents_status` calls `agents.list` and `agents.callable` with `{}`. Only
employees returned by `agents.list` are included; `can_call` is membership
in `agents.callable`. `id` is the RPC's `name`, while the output `name` is
`display_name` (or the id when empty). Neither tool executes as a default
employee. Each underlying RPC retains its normal rate/audit charge; the
wrapper adds no charge. Each parsed tool call also writes a
`person_key_mcp_call` security event with key id, holder id, canonical tool
name and outcome, without credentials or arguments. Unauthenticated calls
have null identity fields, and unknown tool names are recorded as `unknown`.

Refusals have the shape `{"ok":false,"code":"…","message":"…"}`:

| HTTP | Code | Meaning |
| --- | --- | --- |
| 400 | `person_key_bad_request` | Malformed/oversized body, unknown operation/tool, extra field, or invalid arguments. |
| 401 | `person_key_invalid` | Wrong, expired, revoked or invalidated credential; inactive holder; feature disabled. |
| 403 | `person_key_loopback` | Non-loopback peer. |
| 403 | `person_key_scope` | Scope missing from the live key/config intersection. |
| 403 | `person_key_forbidden` | Normal handler refused; its message is preserved. |
| 429 | `member_key_rate_limited` | Existing per-key quota exhausted. |
| 503 | `person_key_unavailable` | Authority/config/database unavailable or invalid internal response. |

Both request variants reject unknown fields, including top-level `user_id`
and `agent_id`; tool arguments cannot select either identity. Null optional
arguments, non-integer/zero/negative limits, and non-object `args` are invalid.
Revocation, password/epoch changes, offboarding and scope narrowing take
effect on the next call; composite operations also recheck scope between RPCs
and before releasing a result. The route tests were added without execution
in the P6a implementation session; gateway compilation and tests remain to
be run by the integration/review session.

## Personal-key scopes and own Odoo connections (v1.86.0)

`me.member_keys.create` additionally accepts `include_conversations` (boolean, default `false`) and `mcp_scopes` (array of strings, default `[]`). Existing password/MFA re-verification is unchanged. Scopes are sorted/deduplicated and must be a subset of `tasks:read`, `agents:status`, `odoo:read`, `odoo:write` and the administrator's `[member_api_keys] allow_mcp_scopes`. Conversation opt-in requires `[member_api_keys] allow_conversation_content=true` at creation only. Key/list metadata includes `include_conversations`, `mcp_scopes`, and `conversations_migrated`. Pre-upgrade keys migrate with conversations enabled and `conversations_migrated=true`; they still obey the own-web-conversation boundary.

A member key sees only its holder's own web conversations. Owner access to an AI employee never opens other members' conversations or external-channel conversations. New keys default to `include_conversations=false`; without it they cannot read conversation methods, resume or subscribe to existing WebChat conversations, or download conversation attachments or conversation-derived files. `activity.list` exposes conversation text only for the holder's own conversations with this option enabled. Memory reads are restricted to the holder's assigned avatar or their own personal employee under active isolation; ownership of a shared employee is insufficient.

`[member_api_keys] allow_conversation_content` (default `false`) governs creation only. Turning it off does not strip existing grants. Keys created before the upgrade retain conversation access and carry `conversations_migrated=true`, within the same own-web-conversation boundary. Browser-only `me.member_keys.narrow` and Admin `member_keys.admin_narrow` can remove conversation access or MCP scopes, never add them. Creation still requires the existing password/MFA re-verification.

| RPC | Parameters / result |
| --- | --- |
| `me.member_keys.narrow` | `{id, include_conversations?, mcp_scopes?}` → `{narrowed:true}`; browser-only owner operation. |
| `member_keys.admin_narrow` | Same parameters/result; live browser Admin may narrow any member key. |

Omitted fields stay unchanged. These operations can turn conversations off or remove scopes; they cannot enable conversations or add a scope, even if the administrator currently permits it. Changes are audited and take effect at live authority checks. Create a new key with fresh re-verification to obtain wider grants.

| Error code | Meaning |
| --- | --- |
| `member_key_conversation_scope` | Key lacks the conversation option for a conversation method. |
| `member_key_memory_scope` | Requested employee is neither the holder's assigned avatar nor their personal employee under active isolation. |
| `member_key_conversation_not_allowed` | Creation requested conversations while the administrator disallows it. |
| `member_key_scope_not_allowed` | Creation requested MCP scopes outside the administrator allowlist. |
| `member_key_invalid_scopes` | Scopes are not an array of supported strings. |
| `member_key_invalid_conversations` | Conversation option is not a boolean. |
| `member_key_cannot_widen` | Narrowing would add authority. |

The conversation option gates `chat.sessions.list`, `chat.sessions.history`, `chat.sessions.restore`, `knowledge.citations.for_session`, `memory.search`, `memory.browse`, `memory.key_facts`, `memory.history`, and `me.memory.list`. It does not gate `me.memory.forget`, which deletes the holder's own memory rather than reading it.

### MCP credentials

Use `Authorization: Bearer <member-key>` on MCP HTTP or `DUDUCLAW_MCP_API_KEY` for stdio. Never use URL query credentials. Effective scopes are the stored grant intersected with current `allow_mcp_scopes`. Only `my_tasks_list`, `my_agents_status` and the documented personal Odoo subset are available; see [MCP tools](../guides/mcp-tools.md#person-tools-and-odoo-access-v1860). Personal Odoo calls always use the key holder's own connected account and have no employee/company fallback. Personal writes require acknowledged `follow_odoo_account`; otherwise only reads are available. OAuth consent refuses personal keys in this release. `/api/internal/person-key` is a loopback-only internal bridge, not a client API.

### Browser-only personal Odoo RPCs

These methods require a live authenticated browser session and cannot be called with a member key. `[odoo] member_credentials` defaults to `false`.

| RPC | Parameters / result |
| --- | --- |
| `me.odoo.status` | `{}` → `member_credentials_enabled`, `connected`, `state`, `login`, `odoo_name`, `server`, `database`, `verified_at`, `last_used_at`; no secret. |
| `me.odoo.connect` | `{login, api_key}`; verifies the Odoo identity against the company's configured URL/database and stores an owner/epoch-bound sealed credential. |
| `me.odoo.test` | `{}`; tests the stored credential. After a server change, read status in the same browser session before testing against the new destination. |
| `me.odoo.disconnect` | `{}` → `{disconnected:true}`; removes the saved credential, including while the feature is off. |
| `odoo.member_credentials.list` | Admin, `{}` → `{credentials:[...], enabled}`; metadata only. |
| `odoo.member_credentials.revoke` | Admin, `{user_id}` → `{revoked:boolean}`. |
| `odoo.member_credentials.set_enabled` | Admin, `{enabled, acknowledge_risk?}` → `{enabled}`; enabling requires `acknowledge_risk=true` and is audited. |

Read status first to see the current server/database; the same session must have read the matching destination within ten minutes before connect or a server-change test. A server fingerprint mismatch marks `needs_reverify`. An epoch change (password change, sign-out-everywhere, suspension) requires key re-entry with connect; test cannot revive that credential. Connect/test share five attempts per minute per member, including malformed and failed attempts. Offboarding removes the credential. Admins cannot set another member's key.

Admin browser RPCs `odoo.access_get {}` and `odoo.access_set {expected_hash, permission_mode?, company_key_agents?, always_refuse?, attachment_max_mb?, dismiss_upgrade_notice?, acknowledge_risk?}` manage the audited policy. Normal get/set responses include `hash`; set requires that get snapshot as the string `expected_hash` and atomically refuses stale saves. An unreadable get has no hash and cannot be saved. Selecting Follow, adding company-account AIs or removing refusal rules requires risk acknowledgement. See [settings](../guides/dashboard-settings.md#member-key-and-odoo-access-settings-v1860) for defaults and downgrade behavior.

Per-employee changes use the admin browser RPC `odoo.agent_access_set {agent_id, permission_override?, act_as_requester?, acknowledge_risk?}`: only `inherit`, `engine`, and `read_only` are accepted; enabling requester identity or loosening an override requires `acknowledge_risk=true`. It edits only those two fields, rechecks the live admin session, and audits before writing; an audit failure blocks the change. `odoo.agent_config_get` returns both fields. Raw/profile writers still refuse widening. The controls will be included in the Odoo settings page.
