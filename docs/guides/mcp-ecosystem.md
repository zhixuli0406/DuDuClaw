# More MCP tools: the MCP Registry, remote servers and app aggregators

DuDuClaw employees use tools through MCP servers listed in their own
`.mcp.json`. Besides the built-in catalogue (Playwright, Browserbase,
Filesystem, Memory) and "Import from URL", the dashboard can now:

1. **Search the official MCP Registry** and install a server to one employee
   (`MCP → MCP Registry`).
2. **Connect remote MCP servers** (Streamable HTTP) with an OAuth sign-in, an
   API token, or no authentication (`MCP → Remote servers`). The gateway keeps
   the credentials encrypted and refreshes them; the employee only gets the
   tools.
3. **Connect hosted app aggregators** (Zapier, Composio) from the
   Marketplace tab: thousands of third-party apps behind one remote endpoint.
4. **Classify and gate third-party tools** by kind of action (section 4).
5. **Receive events from remote servers** (MCP Events, section 5), so an
   autopilot rule or a responsibility can start work when something happens.

All three are Admin features for connecting; anyone signed in can search the
Registry, and a non-Admin's install becomes an install request for the usual
manager → Admin approval.

## 1. MCP Registry search and install

`MCP → MCP Registry`: type a name or topic and search. Results come from
`https://registry.modelcontextprotocol.io` (the gateway only ever talks to that
host, caches answers for about ten minutes, and refuses answers over 2 MiB).
Each result shows its package kinds (`npm`, `pypi`, `oci`), whether it has a
hosted endpoint (`remote`), the version, the repository, required settings, and
— when it cannot be installed — why:

| Reason | Meaning |
|--------|---------|
| No package or remote DuDuClaw can run | only `nuget`, `mcpb` or similar packages |
| Only the legacy SSE transport | the 2024-11-05 HTTP+SSE protocol, which the native bridge does not speak |
| Needs custom request headers | e.g. `X-API-Key`; the bridge sends only `Authorization` |
| Marked deprecated / removed | the registry says so |

**Install** picks the employee and a server name, and asks for the package's
required environment values (stored in that employee's `.mcp.json`, which only
the operator's OS user can read). When a server offers both a package and a
hosted endpoint you choose which one.

What happens on the gateway (`mcp.registry_install`):

- the server's `server.json` is fetched again for the exact version shown;
- it goes through the same parser as "Import from URL"
  (`npm → npx -y <pkg>@<version>`, `pypi → uvx <pkg>==<version>`,
  `oci → docker run -i --rm <image>`; the version is pinned to what you saw);
- an Admin's install uses the same scanned install as `mcp.import.install`
  (fail-closed security scan, the locked `.mcp.json` writer); anyone else's
  becomes an `mcp.install_request` and is installed only after approval.

A hosted endpoint is installed as a remote server (below) and then has to be
connected by an Admin before the employee can use it.

## 2. Remote MCP servers

### How it works

A remote server's entry in the employee's `.mcp.json` is

```json
"zapier": {
  "command": "/usr/local/bin/duduclaw",
  "args": ["mcp-remote-bridge", "--agent", "nova", "--server", "zapier"],
  "env": { "DUDUCLAW_HOME": "/home/me/.duduclaw" }
}
```

The Claude CLI starts it like any stdio MCP server. The hidden
`duduclaw mcp-remote-bridge` forwards every JSON-RPC message (requests,
notifications and responses) to the server over Streamable HTTP, writes the
server's answers (JSON or `text/event-stream`) back, keeps the
`Mcp-Session-Id`, sends the negotiated `MCP-Protocol-Version`, and adds a
fresh `Authorization: Bearer …` to every request.

The URL and every credential live only in `<home>/remote_mcp/servers.json`
(mode 0600, cross-process lock), encrypted with the gateway's per-machine
keyfile (AES-256-GCM, the key that already protects channel and OAuth tokens).
If encryption is not available nothing is stored. Nothing secret appears in
`.mcp.json`, in the bridge's command line or in its environment. The file keeps
in plain text only: employee, server name, sign-in kind, the URL's host,
status, timestamps and whether a refresh token exists.

This replaces `npx -y mcp-remote <url>`, which DuDuClaw used to write for remote
servers. That package signs in by opening a browser on the gateway machine
(impossible on a headless gateway or a DuDuClaw OS appliance) and stores its
tokens in plain text under `~/.mcp-auth`. It is still written for one case
only: a manifest entry with `"type": "sse"` (the legacy transport the native
bridge does not speak); the import preview says so.

### Connecting

`MCP → Remote servers → Connect a server` (or **Connect** after a Registry
install, or on a Marketplace aggregator card): pick the employee, a server
name, the URL, and the sign-in:

- **Sign in (OAuth)** — the gateway discovers the server's authorization
  server, registers itself as a client named "DuDuClaw", and the dashboard
  opens the sign-in page in a new tab. After you approve, the provider sends
  your browser back to `<dashboard>/oauth/mcp/callback`; the gateway exchanges
  the code, stores the tokens, writes the `.mcp.json` entry and shows a small
  "Connected" page with a link back. The dialog notices on its own.
- **API token** — pasted once, sent as `Authorization: Bearer <token>`. The
  gateway checks it with an `initialize` request before saving.
- **None** — for servers that need no sign-in (also checked first).

**Disconnect** deletes the stored credentials and keeps the server (connect it
again later); **Remove** also deletes the record and the `.mcp.json` entry.
Removing a remote server from the employee's server list does the same.

When the provider's authorization server metadata advertises a
`revocation_endpoint` (RFC 7009), the gateway also asks it to revoke the
refresh token (or the access token when there is none): after the local
delete, in the background, at most 10 seconds, so a slow or failing provider
never keeps the credentials on disk. The outcome is audited as
`remote_mcp_token_revocation` (`revoked`, `http_<status>`, `failed`,
`timeout`). Bearer tokens and providers without the endpoint cannot be revoked
from here: revoke them in your account there.

### Extra headers and the server stream (optional)

Under **Extra headers and server stream** in the connect dialog:

- **Extra headers**: one `Name: value` per line, sent on every request to the
  server (the probe, the bridge, MCP Events calls). They are stored inside the
  encrypted record, never in `.mcp.json`, argv or environment; the status list
  shows only their names. Leave the box empty to keep the stored ones.
  Refused: headers DuDuClaw sets itself (`Authorization`, `Host`,
  `Content-Length`, `Content-Type`, `Accept`, `Mcp-Session-Id`,
  `MCP-Protocol-Version`, `Last-Event-ID`, `Cookie`, `Connection`,
  `Transfer-Encoding`, anything starting with `Proxy-` or `Sec-`, …), names
  that are not HTTP tokens, values with control or non-ASCII characters, more
  than 16 headers. A server's sign-in discovery requests (OAuth metadata) do
  not carry them.
- **Listen for messages the server sends on its own** (off by default): after
  `notifications/initialized` the bridge opens the optional `GET` stream of
  the Streamable HTTP transport and passes every message on it to the
  employee. A dropped stream is reopened (1 s, doubling to 30 s) with
  `Last-Event-ID`; `405` (the server has no such stream) or `404` (session
  gone) stops it. The pinned HTTP client times out after 10 minutes, so a
  quiet stream is reopened at least that often.

Tokens are refreshed when they are within a minute of expiry, and once more
when the server answers `401`. A refresh token the provider refuses marks the
connection **Sign in again**; until then the employee's calls to that server
fail with a message telling it an administrator must reconnect.

### The OAuth flow in detail (MCP authorization spec)

1. An unauthenticated `initialize` is sent; the `401` answer's
   `WWW-Authenticate: Bearer resource_metadata="…"` names the protected
   resource metadata (RFC 9728). Without it,
   `/.well-known/oauth-protected-resource/<path>` and then
   `/.well-known/oauth-protected-resource` are tried. The metadata must be
   about the same origin as the server.
2. The authorization server's metadata is read (RFC 8414, then OpenID Connect
   discovery, path-inserted forms first). Its `issuer` must match, and it must
   list `S256` in `code_challenge_methods_supported`; otherwise the gateway
   refuses, as the spec requires.
3. Dynamic client registration (RFC 7591): public client,
   `token_endpoint_auth_method = none`, redirect URI
   `<dashboard origin>/oauth/mcp/callback`. If the provider has no
   registration endpoint, open **Use my own OAuth client** in the dialog and
   enter a client id (and secret) you registered there with that redirect URI.
4. Authorization code with PKCE S256 and the RFC 8707 `resource` parameter
   (the server's canonical URL) on the authorize, token and refresh requests.
5. The `state` is 32 random bytes, kept only in gateway memory, bound to that
   employee, server, verifier and dashboard origin, valid ten minutes and
   usable once.
6. Refresh tokens are rotated: a new one from the provider replaces the old
   one; refreshes are serialized per connection with a lock file, so two
   bridge processes never spend the same refresh token.

Client ID Metadata Documents (CIMD) are not implemented: they need a
`client.json` at a public URL, which a self-hosted gateway does not have.

### Which dashboard addresses can receive the sign-in

The redirect goes to the dashboard address you are using
(`window.location.origin`). The gateway accepts:

- a loopback address (`localhost`, `127.0.0.1`, `[::1]`), http or https;
- an `https` address listed in `config.toml [gateway] allowed_origins` (or
  `DUDUCLAW_ALLOWED_ORIGINS`), compared by exact host and port.

Any other address (plain `http` on a LAN IP such as `http://192.168.1.20:18789`
or a host name such as `http://duduclaw.local:18789`, or an https address not
in `allowed_origins`) cannot receive the redirect, because OAuth 2.1 allows
non-https redirect URIs only for loopback. For those, the sign-in still works
in two steps:

1. The provider is sent to `http://127.0.0.1:<dashboard port>/oauth/mcp/callback`,
   the loopback address of the computer your browser runs on (RFC 8252 §7.3,
   accepted by every OAuth 2.1 server). If the browser runs on the gateway
   machine, that is the gateway itself and the sign-in finishes as usual.
2. On another computer the page fails to load ("can't be reached"). That is
   expected: copy the whole address from the address bar and paste it into
   the box the connect dialog shows (RPC `mcp.remote_complete`). The gateway
   accepts only a loopback address on `/oauth/mcp/callback` whose host, port
   and path match the redirect this sign-in registered, and the state is used
   once. The pasted address carries the one-time code and the state; the PKCE
   verifier never leaves the gateway.

The callback page is served by the gateway itself at `/oauth/mcp/callback`; it
needs no login (the single-use state is the guard).

### What addresses the gateway may reach

Every URL involved (the server, its metadata, the authorization server's
endpoints) must be `https` and resolve only to public internet addresses
(`duduclaw_core::net_addr::is_public_ip`); the resolved addresses are pinned
for the connection, redirects are not followed on POSTs and are re-checked on
metadata GETs. Plain `http` is allowed only for a server on the gateway machine
itself (`localhost`, `127.0.0.0/8`, `::1`), and only then may its metadata point
at loopback too. Servers on a private network (`10.x`, `192.168.x`, …) are
refused.

### Redaction

When RFC-23 redaction is active, the bridge entry is wrapped by
`duduclaw mcp-proxy` like any other stdio server, so remote tool results are
redacted too. (The proxy then leaves the tool gate of section 4 to the
bridge, so nothing is asked twice.)

## 3. Hosted app aggregators: Zapier and Composio

The Marketplace tab has two remote cards:

| Card | Default endpoint (from the MCP Registry) | Sign-in |
|------|------------------------------------------|---------|
| Zapier | `https://mcp.zapier.com/api/v1/connect` | OAuth |
| Composio | `https://connect.composio.dev/mcp` | OAuth |

**Connect** opens the remote dialog with that endpoint filled in; replace it
with the one your account shows if it differs, or switch to an API token if the
provider gave you one. No account or key ships with DuDuClaw.

Everything the employee sends through these tools passes through the
provider's cloud, and the provider can act in every app you connect there. In
your Zapier or Composio account, enable only the apps and actions this employee
needs, and prefer a separate connection per employee.

## 4. Kinds of action for third-party tools

DuDuClaw's own tools each have a kind of action (`read`, `draft`, `send`,
`publish`, `purchase`, `delete`, `modify`, `admin`) and the employee's
`[capabilities] action_rules` can allow, ask or block by kind or by tool
(`docs/features/05-security-defense.md`). Tools of the employee's other MCP
servers are classified too, where DuDuClaw sits in their path:
`duduclaw mcp-proxy` for servers started from `.mcp.json`, and the remote
bridge for remote servers.

**Classification** comes from the `annotations` the server declares for each
tool in `tools/list`:

| Annotation | Kind |
|------------|------|
| `destructiveHint: true` | `delete` |
| `readOnlyHint: true`, server listed in `trusted_read_hint_servers` | `read` |
| `readOnlyHint: true`, server not listed | `modify` |
| anything else, including no annotations | `modify` |

Annotations are what the server says about itself; nothing checks them. A
`destructiveHint` is believed from every server (it can only make a tool
stricter). A `readOnlyHint` is believed only for servers an administrator
lists:

```toml
[capabilities]
trusted_read_hint_servers = ["github"]   # .mcp.json server names
action_rules = [
  { effect = "modify", verdict = "ask" },
  { tool = "github.delete_repository", verdict = "block" },   # or "mcp__github__delete_repository"
]
```

The cost of the default: a malicious server could label a tool that deletes
things `readOnlyHint: true`; believed, it would pass every `read` rule and the
read-only lane. Not believed, a genuinely read-only tool of an unlisted server
is treated like a change (asked, blocked or hidden wherever `modify` is). A
tool the employee calls before the server listed it has no annotations and is
`modify`.

**Enforcement** in the proxy and the bridge, with the employee's
`action_rules` (re-read for every listing and call, same rules as for
DuDuClaw's tools; a `tool` rule names `<server>.<tool>` or
`mcp__<server>__<tool>`, `<server>.*` names every tool of a server):

- `block` — the tool is removed from the `tools/list` the employee sees, and a
  call is answered with JSON-RPC error `-32003` without reaching the server
  (audit `third_party_tool_refused`).
- `ask` — the call waits for an ApprovalBroker decision (`mcp_call` card,
  5 minutes); a denial, an expiry or an unavailable approval store refuses it
  (audit `third_party_tool_approval`).
- **Read-only lane** (`DUDUCLAW_LANE=explore`, the heartbeat proactive check
  and work started by MCP Events): only tools classed `read` are listed and
  callable, so every tool of an unlisted server is hidden.

No `action_rules` key and the normal lane: nothing changes. The proxy is put
in front of `.mcp.json` stdio servers when redaction is active, when the
employee has an `action_rules` key, or when the spawn runs in the read-only
lane; remote servers always go through the bridge.

**Dashboard**: `MCP → Remote servers → Third-party tools and what they do`
shows, per employee, each server's tools as last listed through the proxy or
bridge (`<home>/mcp_tool_effects/<employee>/<server>.json`), with the kind and
verdict recomputed from the current settings. A server appears only after a
session listed its tools through DuDuClaw. The snapshot is display only; the
gate always uses the live listing.

**Runtimes covered**: the Claude CLI (it starts `.mcp.json` servers; the
spawn hands it the rewritten config). Codex, Gemini, Antigravity and Grok
employees are registered with DuDuClaw's own server only, and the
openai-compat tool loop starts only `duduclaw mcp-server`, so they have no
third-party servers to gate. `url` / `type` entries in `.mcp.json` (the CLI
talks to them directly) are not covered; connect them as remote servers
instead.

## 5. Events from remote servers (MCP Events)

A remote server that supports the draft MCP Events extension can tell the
gateway when something happens (a new incident, a new mail…). Implemented:
the webhook mode of the Triggers & Events working group's design sketch
(`modelcontextprotocol/experimental-ext-triggers-events`,
`docs/design-sketch-proposal.md`, draft dated 2026-02-19).

**Set up**

1. Give the gateway an address the server can reach:
   `config.toml [mcp_events] public_base_url = "https://hooks.example.com"`
   (a reverse proxy or tunnel to the gateway port; `https` only, plain `http`
   only for a loopback address when testing). The callback for one
   subscription is `<public_base_url>/webhook/mcp-events/<id>`.
2. Connect the server under **Remote servers** (any sign-in).
3. Under **Events from remote servers**, pick the server, type the event
   names, and **Subscribe**. The gateway asks the server
   (`initialize` must declare `capabilities.events`), creates a `whsec_`
   signing secret, calls `events/subscribe` once per event name with
   `delivery: { mode: "webhook", url, secret }` and a one-day `ttlMs`, and
   answers the server's verification challenge. A sweep (boot, then every 10
   minutes) subscribes again before each grant's `refreshBefore`.

**Receiving** (`POST /webhook/mcp-events/{id}`, always mounted): unknown id ⇒
`404`; body over 256 KiB ⇒ `413`; more than 120 deliveries a minute per
subscription ⇒ `429`; the Standard Webhooks signature (`webhook-id`,
`webhook-timestamp`, `webhook-signature`, HMAC-SHA256 over
`id.timestamp.body`, constant-time, several signatures accepted) and a
timestamp within 5 minutes are required ⇒ otherwise `401`;
`X-MCP-Subscription-Id`, when sent, must be one the server returned; a
repeated `webhook-id` is acknowledged and dropped. Control bodies:
`verification` is echoed, `gap` is audited, `terminated` ends the
subscription (later deliveries `410`). An event name the subscription did not
ask for is answered `410` (no retries).

An accepted event becomes an `events.db` row `mcp.event` with the
subscription, employee, server, event name and id, lane, timestamp and the
event's `data` — scanned by `input_guard` (a hit sets `suspicious: true`; it
does not drop the event) and replaced by a truncated text over 16 KiB. It is
data, never instructions:

- **Autopilot**: trigger `mcp_event` (fields `server`, `name`, `agent_id`,
  `lane`, `suspicious`, and `data.*`). Prompts built from it start with a
  fixed security notice.
- **Responsibilities**: event source `mcp.event`, owned by the subscription's
  employee.

**Read-only by default.** Work an event starts (an autopilot `delegate` or
`run_skill`) runs in the read-only lane: the bus message carries
`lane = "explore"`, and the Claude CLI spawn gets `DUDUCLAW_LANE=explore`
(DuDuClaw tools: `read` and `draft` only), the built-in tools `Read`, `Glob`,
`Grep`, `WebFetch`, `WebSearch`, and third-party servers through the gated
proxy (section 4). This is the same lane, set by the same flag, as a
[read-only responsibility](continuous-responsibilities.md) and with the same
rules: an OpenAI-compatible employee runs the task with DuDuClaw tools limited
the same way and no `agent.toml [mcp.external]` server mounted; an employee on
another runtime or in the task sandbox is refused before the task starts
(`explore_lane_unsupported`), and a MoA model or local-only inference is
refused (the hybrid local offload is skipped). Turn on **Allow normal mode** when subscribing to let events start
work with the employee's usual permissions; only such subscriptions wake a
responsibility (an occurrence is an ordinary goal task), events of read-only
subscriptions are recorded there as `dropped(explore_lane)`.

**Secrets**: stored encrypted with the gateway keyfile in
`<home>/mcp_events/subscriptions.json` (0600). **Replace signing secret**
sends a new one to the server with a refresh; the old one is accepted for 15
minutes. **Unsubscribe** deletes the subscription first (the callback answers
`404` at once) and then calls `events/unsubscribe`, best effort. Audit:
`mcp_event_subscription_created` / `_rotated` / `_revoked` /
`_refresh_failed`, `mcp_event_delivered`, `mcp_event_delivery_rejected`,
`mcp_event_control` (ids, names and counts only).

What was verified and what was assumed: only the design sketch could be read
(OpenAI's page about ChatGPT's support could not be fetched). Poll and push
delivery, cursors and replay (subscriptions always start from "now"),
`deliveryStatus`, `maxAgeMs`, `events/list`, subscription `arguments` (always
`{}`) and the optional `v1a` server signature are not implemented. Tested
against a loopback fake server only.

## RPC reference

| Method | Who | Purpose |
|--------|-----|---------|
| `mcp.registry_search { query, cursor? }` | signed in | Search (fixed host, cached) |
| `mcp.registry_install { name, version?, agent_id, remote?, server_name?, env? }` | signed in (non-Admin ⇒ install request) | Install through the scanned path |
| `mcp.remote_connect { agent_id, name, url?, auth, bearer?, redirect_origin?, client_id?, client_secret?, headers?, server_stream? }` | Admin | Connect; `oauth` returns `authorize_url`; `headers` `{name: value}` replaces the stored ones |
| `mcp.remote_status { agent_id? }` | Admin | Records without secrets (`header_names`, `server_stream`) |
| `mcp.remote_disconnect { agent_id, name, forget? }` | Admin | Delete credentials (`forget` also removes the entry), then revoke at the provider when possible |
| `mcp.tool_effects { agent_id }` | Admin | Third-party tools last listed, with kind and verdict |
| `mcp.events_subscribe { agent_id, server, event_types, mode? }` | Admin | Subscribe (`mode` `explore` default, or `normal`) |
| `mcp.events_list { agent_id? }` | Admin | Subscriptions without secrets |
| `mcp.events_unsubscribe { id }` | Admin | Delete, then unsubscribe upstream |
| `mcp.events_rotate { id }` | Admin | New signing secret |

Audit events (`security_audit.jsonl`): `remote_mcp_connect_started`,
`remote_mcp_connected`, `remote_mcp_connect_failed`,
`remote_mcp_disconnected`, `remote_mcp_token_revocation` (employee, server,
host, sign-in kind; never a URL path or token), plus the section 4 and 5
events. `duduclaw doctor`'s "員工 MCP 設定中的其他伺服器" row names
bridge entries with their host and connection state.

## Not covered / not verified

- No real Zapier or Composio account, and no real third-party OAuth provider,
  was used to test this; the flow is verified against a local fake
  authorization server and MCP server only.
- The bridge does not resume a broken event stream that answers a POST; the
  server-initiated `GET` stream is opt-in per server and tested against a
  loopback fake only.
- Legacy HTTP+SSE servers still go through `npx mcp-remote` (see above).
  Registry entries that declare required headers are still shown as not
  installable; connect them by URL with **Extra headers** instead.
- Revocation at the provider runs only when the authorization server
  advertises `revocation_endpoint`, and is best effort.
- Third-party tool classification trusts the server's annotations as
  described in section 4; the snapshot file the dashboard reads is written by
  the employee's own process tree.
- MCP Events: see the end of section 5; the gateway must be reachable at an
  https address the server accepts.
- The desktop app's dashboard origin (`tauri://…`) is not an accepted redirect
  origin; sign in from a browser.
- The employee runs as the same OS user as the gateway: an employee with
  unrestricted Bash could read the keyfile and the store, or start the bridge
  for its own `--agent` by hand. The bridge refuses an `--agent` different
  from the process's `DUDUCLAW_AGENT_ID`, but real isolation is not granting
  Bash.
- Pending sign-ins live in gateway memory: a gateway restart during a sign-in
  means starting it again.
