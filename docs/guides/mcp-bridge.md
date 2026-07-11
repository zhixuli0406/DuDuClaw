# MCP Bridge — mounting external MCP servers

DuDuClaw can mount third-party [Model Context Protocol](https://modelcontextprotocol.io)
servers next to its own internal MCP server, so an agent's tool loop gains the
external server's tools without a hand-written Rust connector. This is how you
wire Plane, Chatwoot, Invoice Ninja, Gmail/Calendar, WooCommerce, and any other
MCP server into an agent.

## When to use this vs a native connector

- **MCP Bridge** (this page): the SaaS already ships (or the community ships) an
  MCP server. You mount it by config. No code.
- **Native connector** (e.g. `duduclaw-odoo`, `duduclaw-erpnext`): no usable MCP
  server exists, or you need deep credential isolation / edition gating / audit
  attribution that a generic mount can't give.

## Configuration

Add one or more `[[mcp.external]]` tables to the agent's `agent.toml`:

```toml
[[mcp.external]]
name = "chatwoot"
command = "npx"
args = ["-y", "@chatwoot/mcp-server-chatwoot"]
enabled = true                       # optional, default true
# env values: a plain literal; `env://VAR` to pull from the gateway process
# environment; or `secret://<backend>/<name>` to pull from the configured secret
# manager at spawn time (keep secrets OUT of agent.toml).
env = { CHATWOOT_BASE_URL = "https://app.chatwoot.com", CHATWOOT_API_TOKEN = "secret://vault/chatwoot_token" }
# Tool visibility (both optional):
allowed_tools = ["chatwoot_list_conversations", "chatwoot_get_conversation"]  # allowlist = deny-by-default
denied_tools  = ["chatwoot_delete_conversation"]                             # always removed
```

Field reference:

| Field | Required | Meaning |
|---|---|---|
| `name` | recommended | Label for logs |
| `command` | **yes** | Executable to spawn (`npx`, `node`, `python`, an absolute path…) |
| `args` | no | Argument vector |
| `env` | no | Child environment. `env://VAR` pulls from the gateway's env; `secret://<backend>/<name>` pulls from the secret manager (see below); a missing/unresolvable `env://`/`secret://` credential **disables the whole server** (fail-safe — a server without its token would misbehave) |
| `enabled` | no (default true) | Set false to keep the config but not mount |
| `allowed_tools` | no | If set, ONLY these tools are exposed (deny-by-default) |
| `denied_tools` | no | Always removed, even if allow-listed |

## Semantics & safety

- The **internal duduclaw MCP server is always client 0**, so on a tool-name
  collision the internal tool wins (external duplicates are dropped with a log).
- Each external server is spawned independently; if one **fails to connect it is
  skipped** — the internal server and any other externals still serve. If the
  combined `tools/list` fails, the registry degrades to **internal-only** rather
  than losing all tools.
- `allowed_tools` is **deny-by-default**: an allowlist means unlisted tools are
  hidden. Combine with `denied_tools` to blocklist specific dangerous tools.
- For write/irreversible tools (billing, deletes), also list them in the agent's
  `[capabilities] approval_required_tools` so they route through the HITL
  `ApprovalBroker` — the MCP Bridge controls *visibility*, approvals control
  *execution*.

## Live-verification runbook

The config parser and tool filter are covered by unit tests
(`crates/duduclaw-gateway/src/mcp_external.rs`,
`crates/duduclaw-llm/src/mcp_client.rs`). To verify an actual mount end-to-end
you need a reachable MCP server:

1. Pick a server you can run locally, e.g. the reference everything-server:
   ```bash
   # in a scratch dir, confirm it speaks MCP over stdio
   npx -y @modelcontextprotocol/server-everything
   ```
2. Add it to a test agent's `agent.toml`:
   ```toml
   [[mcp.external]]
   name = "everything"
   command = "npx"
   args = ["-y", "@modelcontextprotocol/server-everything"]
   allowed_tools = ["echo"]   # prove the allowlist hides the rest
   ```
3. Start the gateway and send the agent a message that would use the `echo`
   tool. Confirm in the logs:
   - `external MCP server mounted` with `server=everything`
   - the agent can call `echo` but NOT the server's other tools (allowlist).
4. Temporarily point an `env://` credential at an unset var and restart —
   confirm the server is skipped with
   `external MCP env credential unresolved … skipping server`.

Expected: the agent gains exactly the allow-listed external tools; a broken
external server never takes down the internal tool surface.

## Resolving credentials with `secret://`

`env` values may reference the secret manager instead of holding a literal or an
`env://` process-env pull. At spawn time DuDuClaw resolves
`secret://<backend>/<name>` against the `[secret_manager]` config in
`~/.duduclaw/config.toml`; an unresolvable ref drops the whole server (fail-safe,
identical to a missing `env://`).

Backends: `local` (AES store), `vault` (HashiCorp Vault KV v2), `env`,
`onepassword` (1Password Connect), `infisical`. Example:

```toml
# config.toml
[secret_manager]
backend = "vault"
vault_addr  = "https://vault.internal:8200"
vault_token_enc = "…"          # keyfile-encrypted; never plaintext in prod

# agent.toml
[[mcp.external]]
name = "chatwoot"
command = "npx"
args = ["-y", "@chatwoot/mcp-server-chatwoot"]
env = { CHATWOOT_BASE_URL = "https://app.chatwoot.com", CHATWOOT_API_TOKEN = "secret://vault/chatwoot_token" }
```

See the module docs in `crates/duduclaw-security/src/secret_manager/mod.rs` for
the full `[secret_manager]` field set (including 1Password / Infisical).

## Recipes — common SaaS servers

Each recipe is the `agent.toml` block plus the credentials to provision. Mount
one, restart the agent, follow the [live-verification runbook](#live-verification-runbook).
**Write/irreversible tools are marked ⚠ — list them in the agent's
`[capabilities] approval_required_tools` so they route through the HITL broker.**

> Status: these are **PENDING-LIVE** — the config shape + parsing are tested, but
> a live end-to-end mount needs the corresponding SaaS account. Server names
> reflect the ecosystem as of 2026-07; confirm the package/endpoint before use.

### Gmail / Google Calendar (Google official remote MCP)

```toml
[[mcp.external]]
name = "gmail"
command = "npx"
args = ["-y", "@google/gmail-mcp"]     # confirm the current official package
env = { GOOGLE_OAUTH_TOKEN = "secret://vault/google_oauth" }
allowed_tools = ["gmail_search", "gmail_get_thread", "gmail_create_draft"]  # read + draft only
denied_tools  = ["gmail_send"]         # ⚠ keep send behind approval, not auto
```
Provision: a Google Cloud OAuth app; run the OAuth flow to mint the token.
`gmail_send` ⚠ → `approval_required_tools`.

### Plane (official `plane-mcp-server`, mature)

```toml
[[mcp.external]]
name = "plane"
command = "npx"
args = ["-y", "@makeplane/plane-mcp-server"]
env = { PLANE_API_KEY = "secret://vault/plane_api_key", PLANE_WORKSPACE_SLUG = "my-workspace" }
allowed_tools = ["plane_list_issues", "plane_get_issue", "plane_create_issue"]
denied_tools  = ["plane_delete_issue"]  # ⚠
```
Optional: a one-way sync worker can pull Plane issues into the Task Board (see
IMPL-PLAN §E). Provision: a Plane API key + workspace slug.

### Invoice Ninja (community `Fuciuss/invoice-ninja-mcp`)

```toml
[[mcp.external]]
name = "invoice-ninja"
command = "npx"
args = ["-y", "invoice-ninja-mcp"]      # confirm package name
env = { INVOICE_NINJA_URL = "https://invoicing.example.com", INVOICE_NINJA_TOKEN = "secret://vault/invoiceninja_token" }
allowed_tools = ["in_list_invoices", "in_get_invoice", "in_create_invoice", "in_record_payment"]
```
**Money is irreversible** — put every write tool
(`in_create_invoice`, `in_record_payment`, …) in `approval_required_tools`.
Provision: an Invoice Ninja API token.

### Chatwoot (official `@chatwoot/mcp-server-chatwoot`)

```toml
[[mcp.external]]
name = "chatwoot"
command = "npx"
args = ["-y", "@chatwoot/mcp-server-chatwoot"]
env = { CHATWOOT_BASE_URL = "https://app.chatwoot.com", CHATWOOT_API_TOKEN = "secret://vault/chatwoot_token" }
allowed_tools = ["chatwoot_list_conversations", "chatwoot_get_conversation", "chatwoot_create_message"]
```
Nine-channel inbox → one agent; draft replies through the ApprovalBroker
(`chatwoot_create_message` ⚠ if you want human review before send). Provision: a
Chatwoot API access token.

### WooCommerce (official native MCP — dev preview)

```toml
[[mcp.external]]
name = "woocommerce"
command = "npx"
args = ["-y", "@woocommerce/mcp-adapter"]   # WordPress MCP Adapter
env = { WP_SITE_URL = "https://shop.example.com", WP_MCP_OAUTH_TOKEN = "secret://vault/woo_oauth" }
allowed_tools = ["wc_list_products", "wc_get_order", "wc_list_orders"]
```
**Use OAuth 2.1 via the WordPress MCP Adapter — the legacy `X-MCP-API-Key` was
deprecated 2026-06-23.** Provision: the WP MCP Adapter plugin + an OAuth client.

### DocuSeal (no server exists — build `duduclaw-docuseal-mcp`)

No MCP server ships for DocuSeal yet; the path is to build a small one (REST +
webhook: generate → send → webhook-complete) and mount it here, then contribute
it upstream. Tracked in IMPL-PLAN §D as effort M.

### Monica (personal PRM — thin MCP or IdentityProvider)

No MCP server exists. Either a thin MCP over `/api/contacts` (birthdays,
interaction history) or wire it as an `IdentityProvider` (see
`duduclaw-identity`). Tracked in IMPL-PLAN §D.

## Roadmap

- Per-server call auditing to `tool_calls.jsonl` (currently internal tools are
  attributed; external mounts are logged at connect time).
