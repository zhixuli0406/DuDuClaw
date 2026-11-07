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

# 3. Stop the first HTTP server, then restart with the tunnel HTTPS URL
duduclaw http-server --bind 127.0.0.1:8765 \
  --public-origin https://mcp.example.com --trusted-proxy 127.0.0.1
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

## Canonical origin and proxy configuration

For public OAuth, start (or restart) the server with the actual fixed HTTPS origin:

```bash
duduclaw http-server --bind 127.0.0.1:8765 \
  --public-origin https://mcp.example.com --trusted-proxy 127.0.0.1
```

Replace the domain and proxy IP with your deployment's values. With a quick tunnel, first obtain its URL, then restart `http-server` with that URL as `--public-origin`; a changed URL requires new authorization. Prefer a fixed domain.

| CLI option | Default and behavior |
| --- | --- |
| `--public-origin <https-origin>` | Unset: use `http://` plus the actual bound socket address. If set, require an HTTPS origin only (no credentials, query, fragment or path except `/`). Issuer, resource, discovery, token and revoke URLs all use this origin; `Host` and `X-Forwarded-Proto` never change it. |
| `--trusted-proxy <IP>` | Empty; repeat for each trusted proxy's exact IP. Trust `X-Forwarded-For` only when the TCP peer is listed. Walk right to left past trusted hops and use the first untrusted IP; malformed headers fall back to the TCP peer. The proxy must overwrite or append the actual connecting IP and prevent direct access that impersonates a trusted hop. |
| `--allow-insecure-lan` | Off. Explicitly permits plaintext non-loopback listening and prints a prominent warning. Testing only: operator keys and tokens can be intercepted. It does not enable TLS or trust forwarded headers. |

Non-loopback binding fails before startup unless both an HTTPS `--public-origin` and at least one trusted proxy are configured, or the insecure opt-in is present. Without `--allow-insecure-lan`, a non-loopback listener rejects every request whose TCP peer is missing or absent from the trusted-proxy list with HTTP 403, before reading the body, checking credentials or charging quotas. This includes health checks and unknown routes; forwarded headers cannot bypass the check. Loopback binds continue to support local proxy connections. The listener itself is HTTP: terminate TLS at the trusted proxy and protect the proxy-to-backend network, which remains plaintext. These are CLI flags, not `config.toml` keys or environment variables.

For insecure LAN tests, bind the actual LAN IP (for example `--bind 192.168.1.10:8765 --allow-insecure-lan`); a wildcard such as `0.0.0.0` is not a usable client-facing canonical address.

Authorization and token requests accept RFC 8707 `resource`, exactly `<canonical-origin>/mcp`. A missing value defaults only to this URI; an empty or different value returns `invalid_target`, including a conflicting repeated parameter. Consent, code exchange, refresh and access validation preserve this audience. Changing the origin requires reconnecting; old stored codes/tokens without audience binding are rejected.

Dynamic registration counts every attempt toward a 3/minute allowance per resolved client IP. The separate 10/minute global allowance is charged only after JSON/metadata validation and a registration-capacity check, when a request would create a client. Invalid requests and capacity refusals do not consume it. The 200-client cap remains: at capacity, the oldest never-authorized registration at least one hour old may be removed to make room. Clients with live codes, grants, access tokens or refresh tokens are protected; previously authorized clients and registrations within the grace period are not capacity-eviction candidates. Without an eligible candidate, registration is refused. Otherwise never-authorized clients expire after 24 hours and previously authorized clients after 90 days of inactivity. Successful consent or token use extends activity; unauthenticated requests do not. Pruning happens on store mutations. Expired or evicted clients must register again. Rate limits are process-local and reset on restart; enforce aggregate limits at the proxy if running multiple servers.

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

CIMD cache misses have a dedicated 12-attempt/minute budget per resolved source IP and a process-wide limit of four concurrent fetch workers. Waiting more than 100 ms for a slot returns HTTP 503 `temporarily_unavailable`. Failed fetches and requests exceeding the five-second fetch wait are cached by client_id URL for 30 seconds; the combined success/failure cache holds at most 64 entries. The five-second wait includes DNS resolution. Timed-out or cancelled requests leave their worker holding its slot until underlying DNS/network work finishes, preventing abandoned work from exceeding the concurrency limit. Cached documents do not consume fetch-attempt quota.

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

## Revalidate and revoke connections (v1.80.0)

The original internal MCP key must remain valid. Authorization-code exchange, refresh, and each access-token check revalidate it. Removing or expiring the key, changing its metadata (including scopes), or failing to read its configuration invalidates the grant. A refresh then returns `invalid_grant` and removes the entire token family, including its access tokens. Consent cannot grant scopes beyond the internal key; refresh can only retain or reduce the token's scopes. Reusing an already rotated refresh token also invalidates its family.

In **Manage → Integrations → MCP API keys**, Admin accounts can view active OAuth grants, application names, scopes, authorization times and last use, and revoke a grant. These operations use the Admin-only `mcp_oauth_grants.list` and `mcp_oauth_grants.revoke` RPCs. The list never returns credentials or their hashes. Revocation prevents subsequent requests; it does not cancel a tool call already running.

Clients can `POST /oauth/revoke` over HTTPS with an `application/x-www-form-urlencoded` body containing `client_id` and `token`, plus optional `token_type_hint`. The server supports public clients only (`none`, the same policy as the token endpoint) and checks the token's client binding. Both access and refresh tokens revoke the whole family. Unknown tokens and tokens belonging to another client return HTTP 200 without changing another client's grant; a missing client ID or token is rejected. The hint may be ignored. Discovery advertises `revocation_endpoint` and `revocation_endpoint_auth_methods_supported`. See [RFC 7009](https://www.rfc-editor.org/rfc/rfc7009).

**Upgrade:** older OAuth codes and tokens without consent/family lineage are rejected. Reconnect each application to obtain new authorization. The shared store locks the complete read/update/write operation; concurrent refresh, registration and dashboard revocation cannot restore a revoked family. Unreadable or malformed storage fails closed.

`duduclaw mcp issue-refresh-token` issues a separate **stdio-only** `ddc_refresh_*` credential for `duduclaw mcp-server`. The command warns at issuance. HTTP Bearer and SSE query authentication reject it with a stdio-only explanation. For `http-server`, use a static MCP API key or OAuth. This keeps HTTP on its existing authentication paths; a CLI refresh credential cannot be exchanged at `/oauth/token`. Stdio continues checking its scopes, expiry and revocation on each call.

Validation: live browser acceptance has not been performed.

OAuth issuance is bounded to 2,048 access/refresh records per family (including spent refresh digests), 16,384 token records and 1,024 grants overall, with a 16 MiB store limit. At capacity, issuance fails without consuming the current refresh token; spent digests are retained until expiry so replay and rotated-token revocation still work. Anonymous traffic has a 120/minute IP allowance. Valid Bearer requests use a separate bounded principal allowance (60/minute) plus the existing per-client gate, so exhausting an anonymous bucket does not consume their quota. IP resolution follows the trusted-proxy policy above; without it, anonymous clients behind a proxy share its TCP peer allowance. Concurrent authentication work is also bounded. Quota classification uses a bounded digest snapshot refreshed at most once per second; it never grants access. New credentials may take up to one second to receive their independent allowance. Protected requests still revalidate live state, so stale hints do not bypass revocation. Static-key hints cover at most 4,096 entries in a configuration of up to 1 MiB; keys outside those bounds use the anonymous admission allowance before normal authentication. Token exchange has a 12/minute allowance per (client_id, resolved source IP) pair, plus a 120/minute per-IP bound regardless of claimed client IDs or bearer hints. Invalid exchanges from one source cannot consume another source’s allowance for the same public client_id. Admission tables and blocking storage workers are bounded. Access validation is read-only; admitted use updates last-use telemetry at most once per minute in a blocking worker. Grant-list configuration errors are reported as unavailable, not an empty list. Dashboard revocation audits the actor, family, application, scopes and outcome without credentials or hashes; an audit-write failure is reported even if revocation already took effect.

## Endpoint reference

| Path | Purpose |
|---|---|
| `POST /mcp` | Standard MCP endpoint: `server/discover` / `tools/list` / `tools/call` / `subscriptions/listen` (2026-07-28), `initialize` / `ping` (legacy). See [Protocol versions](#protocol-versions) |
| `GET /.well-known/oauth-protected-resource` | RFC 9728 resource metadata (a 401's `WWW-Authenticate` header points here) |
| `GET /.well-known/oauth-authorization-server` | RFC 8414 authorization server metadata |
| `POST /oauth/register` | RFC 7591 dynamic client registration (public client); an unregistered https `client_id` is read as a Client ID Metadata Document |
| `GET /oauth/authorize` → `POST /oauth/decision` | Authorization code flow plus the operator consent page |
| `POST /oauth/token` | Token issuance / refresh |
| `POST /oauth/revoke` | Revoke the token family (RFC 7009, public client) |
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
  domain (a reverse proxy or a Cloudflare named tunnel), and configure it with `--public-origin`.
