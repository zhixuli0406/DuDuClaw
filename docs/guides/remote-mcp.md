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
| `POST /mcp` | Standard MCP endpoint (initialize / tools/list / tools/call / ping) |
| `GET /.well-known/oauth-protected-resource` | RFC 9728 resource metadata (a 401's `WWW-Authenticate` header points here) |
| `GET /.well-known/oauth-authorization-server` | RFC 8414 authorization server metadata |
| `POST /oauth/register` | RFC 7591 dynamic client registration (public client) |
| `GET /oauth/authorize` → `POST /oauth/decision` | Authorization code flow plus the operator consent page |
| `POST /oauth/token` | Token issuance / refresh |
| `POST /mcp/v1/call`, `GET /mcp/v1/stream` | The existing DuDuClaw REST/SSE surface (unchanged) |

Static Bearer keys (`ddc_…`) and OAuth tokens (`ddc_oauth_…`) share the same
`Authorization: Bearer` surface: scripts and your own integrations can keep using a static key,
while OAuth exists for clients that only speak OAuth, like claude.ai.

## Security notes

- The browser origin (`Origin` header) is checked against loopback plus the
  `config.toml [gateway] allowed_origins` allow-list (anchored match); anything else gets a 403.
  Non-browser clients are unaffected.
- A quick tunnel's URL changes every time it starts. For production use, put it behind a fixed
  domain (a reverse proxy or a Cloudflare named tunnel) — the OAuth flow derives its issuer from
  the request's `Host` / `X-Forwarded-Proto`.
- To revoke every issued connection, delete `~/.duduclaw/mcp_oauth_issued.json`; the next
  request fails immediately.
