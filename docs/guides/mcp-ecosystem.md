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
Neither sends a revocation request to the provider: revoke DuDuClaw's access
in your account there as well.

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

Plain `http` on any other host is refused (OAuth 2.1 allows non-https redirect
URIs only for loopback). If you reach the dashboard over the LAN by IP, either
open it as `http://localhost:<port>` on the gateway machine for the sign-in, or
serve it over https and add the host to `allowed_origins`. The callback page is
served by the gateway itself at `/oauth/mcp/callback`; it needs no login (the
single-use state is the guard).

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
redacted too.

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

## RPC reference

| Method | Who | Purpose |
|--------|-----|---------|
| `mcp.registry_search { query, cursor? }` | signed in | Search (fixed host, cached) |
| `mcp.registry_install { name, version?, agent_id, remote?, server_name?, env? }` | signed in (non-Admin ⇒ install request) | Install through the scanned path |
| `mcp.remote_connect { agent_id, name, url?, auth, bearer?, redirect_origin?, client_id?, client_secret? }` | Admin | Connect; `oauth` returns `authorize_url` |
| `mcp.remote_status { agent_id? }` | Admin | Records without secrets |
| `mcp.remote_disconnect { agent_id, name, forget? }` | Admin | Delete credentials (`forget` also removes the entry) |

Audit events (`security_audit.jsonl`): `remote_mcp_connect_started`,
`remote_mcp_connected`, `remote_mcp_connect_failed`,
`remote_mcp_disconnected` (employee, server, host, sign-in kind; never a URL
path or token). `duduclaw doctor`'s "員工 MCP 設定中的其他伺服器" row names
bridge entries with their host and connection state.

## Not covered / not verified

- No real Zapier or Composio account, and no real third-party OAuth provider,
  was used to test this; the flow is verified against a local fake
  authorization server and MCP server only.
- The bridge does not open the optional `GET` stream for server-initiated
  messages outside a request, and does not resume a broken event stream.
- Legacy HTTP+SSE servers still go through `npx mcp-remote` (see above);
  remotes that need custom headers are not supported.
- Disconnecting does not revoke the token at the provider.
- The desktop app's dashboard origin (`tauri://…`) is not an accepted redirect
  origin; sign in from a browser.
- The employee runs as the same OS user as the gateway: an employee with
  unrestricted Bash could read the keyfile and the store, or start the bridge
  for its own `--agent` by hand. The bridge refuses an `--agent` different
  from the process's `DUDUCLAW_AGENT_ID`, but real isolation is not granting
  Bash.
- Pending sign-ins live in gateway memory: a gateway restart during a sign-in
  means starting it again.
