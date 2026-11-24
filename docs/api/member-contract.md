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
remain effective through the existing live checks.

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

`users.me` additionally preserves `user.avatar_agent_id` (nullable, RM-212).
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
| `me.google.status` | Read | Read personal Google connection and consent status (Enterprise) | `connection`, `login_email`, `client_configured`, `chat_read`, `integration_enabled`, `requests`, `delegated`, `requires_login`, `recent_usage` | 1.85.0 |
| `me.google.connect_start` | Write | Start personal Google authorization (Enterprise) | — | 1.85.0 |
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
| `users.set_avatar`, `users.avatar_conflicts` | RM-212 Admin-only management methods, outside the member inventory |

`users.list` is also Admin-only; its RM-212 avatar field is not a member-facing
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

## Personal member API keys (RM-213 part B, 1.85.0)

An administrator must first enable `[member_api_keys] enabled = true` in
`config.toml`, or use the member-key section on **My account → External access**.
The default is off. An unreadable or malformed configuration disables both
key authentication and key management. The administrator can still change the
switch through the normal authenticated configuration UI.

A member signs in with a browser JWT session, then calls
`me.member_keys.create {"name":"My server","expires_in_days":90}`. Accounts with
MFA must also supply a fresh `code` (TOTP or unused recovery code), checked with
the existing MFA step-up verifier. The response is `{"key":{...},"secret":"..."}`.
Only this response contains the plaintext. Admin accounts cannot create keys;
use a separate member account. Creation is never available to an existing key.
Expiry is mandatory: 1–365 days; the UI defaults to 90 and offers 30/90/365.
Each account can have at most five active keys.

`me.member_keys.list` returns `{"keys":[...],"max_active":5}` and
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
cannot authenticate these member-key paths. Conversely, member keys are refused
by the status endpoint, MCP HTTP authentication, MCP key validation, legacy
Dashboard-token authentication and JWT-only account/security endpoints.

Key RPC access is the intersection of `MEMBER_CONTRACT_METHODS`, the member's
**current** role/bindings, and all existing handler gates. Credential management,
`users.change_password`, `users.change_email`, `mfa.*`, `sessions.*`, `config.*`,
`me.google.connect_*`, `me.google.disconnect`, `me.google.consents.set` and
step-up operations are excluded. Unknown methods are denied. The compatibility
table includes browser-only methods; its presence alone is not a key grant.

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
