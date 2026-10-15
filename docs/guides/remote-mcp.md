# Remote MCP: connect claude.ai, or any MCP client, straight to your DuDuClaw

`duduclaw http-server` exposes a standard **MCP Streamable HTTP** endpoint (`POST /mcp`) along
with a full **OAuth 2.1** authorization flow. That means claude.ai's Custom Connector, the
Claude mobile app, MCP Inspector, or any app that supports a remote MCP server can connect
directly to your self-hosted DuDuClaw and use its memory, knowledge base, and other tools.

## Quick start

```bash
# 1. Start the HTTP server (binds to loopback only by default)
duduclaw http-server --bind 127.0.0.1:8765

# 2. Open a tunnel when you need external access (or use your own reverse proxy / domain)
duduclaw tunnel          # Cloudflare quick tunnel; the terminal prints an https URL
```

Once you have a public URL, paste it into claude.ai's "Settings → Connectors → Add custom
connector" (設定 → 連接器 → 新增自訂連接器):

```
https://<your-url>/mcp
```

claude.ai runs OAuth discovery automatically (RFC 9728 → RFC 8414 → dynamic registration) and
takes you to DuDuClaw's authorization page. Paste in an **internal MCP API key** (a key under
`config.toml [mcp_keys]` with `is_external = false`) and click "Agree to connect" (同意連線) to
finish.

## Use it from ChatGPT

ChatGPT can add DuDuClaw as a custom connector (an "app" in developer mode).
Nothing is published to any store; the connector exists only in your ChatGPT
account.

1. Expose `duduclaw http-server` at a fixed **https** address (a reverse proxy
   or a named tunnel; ChatGPT reaches it from OpenAI's servers, so loopback or
   a LAN address does not work).
2. In ChatGPT, turn on developer mode (Settings → Apps & Connectors →
   Advanced settings; which plans offer it depends on OpenAI), then create a
   connector with the server URL `https://<your-address>/mcp` and OAuth as
   the authentication.
3. ChatGPT discovers the authorization server (RFC 9728 → RFC 8414) and
   registers itself, either through dynamic client registration
   (`POST /oauth/register`) or with a Client ID Metadata Document (its
   `client_id` is an https URL that DuDuClaw fetches; supported since
   2026-10-08, see below). Its redirect URI, as given in third-party guides,
   is `https://chatgpt.com/connector_platform_oauth_redirect`; DuDuClaw
   accepts any https redirect URI, so nothing has to be configured for it.
4. DuDuClaw's consent page opens: paste an **internal** MCP API key and
   approve.

What ChatGPT gets: the same external-client tier as every OAuth client
(below) — the base tools plus at most `memory:read`, `memory:write`,
`wiki:read`, `wiki:write` and `messaging:send`. Connector, execution, roster
and Admin tools are never exposed over OAuth.

Client ID Metadata Documents: when a `client_id` is an `https://` URL with a
path and is not a registered id, DuDuClaw fetches it (public addresses only,
pinned, no redirects, 5 s, 5 KiB, cached for an hour). The document's
`client_id` must equal the URL, its `redirect_uris` must be https (or
loopback http) URLs and include the one used, and only public clients
(`token_endpoint_auth_method` absent or `none`) are accepted. The
authorization server metadata advertises
`client_id_metadata_document_supported: true`.

Not verified: no real ChatGPT account was used. The redirect URI and the
registration methods come from third-party connector guides (OpenAI's own
pages could not be fetched while writing this); the flow is tested against
local requests only.

## Authorization model (read this part)

Access tokens issued through OAuth are **always treated as "external client" tier**, sharing
the exact same scope policy as any external-facing tool surface. There is no second rule set:

- The base tool surface (7 core tools) is always available.
- Scopes a connector requests get narrowed to the **allow-list that can be granted externally**
  (`memory:read` / `memory:write` / `wiki:read` / `wiki:write` / `messaging:send`). Connector
  tools (Odoo/Google/Notion), execution tools, the personnel roster, and Admin are **never**
  exposed over OAuth, no matter what a client asks for.
- The key pasted into the consent page must be an internal key (an external key cannot
  self-upgrade its own tier).

Token details: access tokens last 1 hour, refresh tokens last 30 days and rotate on every use,
authorization codes are single-use and valid for 10 minutes, and PKCE S256 is required. Every
token is stored on disk only as a SHA-256 hash (`~/.duduclaw/mcp_oauth_issued.json`, mode 0600).

## Endpoint reference

| Path | Purpose |
|---|---|
| `POST /mcp` | Standard MCP endpoint: `server/discover` / `tools/list` / `tools/call` / `subscriptions/listen` (2026-07-28), `initialize` / `ping` (legacy). See [Protocol versions](#protocol-versions) |
| `GET /.well-known/oauth-protected-resource` | RFC 9728 resource metadata (a 401's `WWW-Authenticate` header points here) |
| `GET /.well-known/oauth-authorization-server` | RFC 8414 authorization server metadata |
| `POST /oauth/register` | RFC 7591 dynamic client registration (public client); an unregistered https `client_id` is read as a Client ID Metadata Document |
| `GET /oauth/authorize` → `POST /oauth/decision` | Authorization code flow plus the operator consent page |
| `POST /oauth/token` | Token issuance / refresh |
| `POST /mcp/v1/call`, `GET /mcp/v1/stream` | The existing DuDuClaw REST/SSE surface (unchanged) |

Static Bearer keys (`ddc_…`) and OAuth tokens (`ddc_oauth_…`) share the same
`Authorization: Bearer` surface: scripts and your own integrations can keep using a static key,
while OAuth exists for clients that only speak OAuth, like claude.ai.

## Protocol versions

DuDuClaw serves both MCP eras, on `POST /mcp` and on the stdio `duduclaw mcp-server`:

- **Modern, 2026-07-28.** No `initialize`. Each request names its revision and client capabilities in `params._meta` (`io.modelcontextprotocol/protocolVersion`, `io.modelcontextprotocol/clientCapabilities`). Supported: `server/discover`, `tools/list`, `tools/call` and `subscriptions/listen`. Over HTTP every request also needs `MCP-Protocol-Version` and `Mcp-Method` headers, and `tools/call` needs `Mcp-Name`. A missing or mismatched header returns `400` with `-32020`. An unsupported revision returns `-32022` with the supported list.
- **Legacy, 2024-11-05 to 2025-11-25.** `initialize` negotiates: you get the revision you asked for, or 2025-11-25 if it is unknown. `ping` is still answered.

Tool-list changes: over stdio, a legacy client gets `notifications/tools/list_changed` as before, and a modern client gets it on a `subscriptions/listen` stream with `toolsListChanged: true`. The HTTP endpoint is stateless and cannot tell that a caller's tools changed, so it acknowledges a subscription with no notification types and ends it straight away. HTTP clients re-read `tools/list` when its `ttlMs` (5 seconds) runs out.

Verified on 2026-10-16 with the official TypeScript client `@modelcontextprotocol/client` 2.3.1, in pinned-2026-07-28, auto and legacy modes, over both stdio and HTTP.

## Security notes

- The browser origin (`Origin` header) is checked against loopback plus the
  `config.toml [gateway] allowed_origins` allow-list (anchored match); anything else gets a 403.
  Non-browser clients are unaffected.
- A quick tunnel's URL changes every time it starts. For production use, put it behind a fixed
  domain (a reverse proxy or a Cloudflare named tunnel) — the OAuth flow derives its issuer from
  the request's `Host` / `X-Forwarded-Proto`.
- To revoke every issued connection, delete `~/.duduclaw/mcp_oauth_issued.json`; the next
  request fails immediately.
