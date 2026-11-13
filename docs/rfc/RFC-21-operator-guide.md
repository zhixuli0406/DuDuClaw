# RFC-21 Operator Guide — Identity Resolution & Per-Agent Credential Isolation

**Companion to:** [`RFC-21-identity-credential-isolation.md`](RFC-21-identity-credential-isolation.md)
**Tracking:** [Issue #21](https://github.com/zhixuli0406/DuDuClaw/issues/21)
**Status:** All three sections implemented as of 2026-05-04.

This guide is the operator-facing playbook for the system-layer features
RFC-21 added to DuDuClaw. The RFC explains *why*; this guide explains
*how to deploy*.

---

## §3 — Shared wiki SoT namespace policy

### When to use

Drop a `.scope.toml` when you want any namespace under the shared wiki to
be treated as "owned by an external system" — typically because another
sync process writes to it and an evolving agent must not silently
overwrite the upstream truth.

### Setup

```bash
# Create the policy file alongside your existing shared wiki scaffold.
$EDITOR ~/.duduclaw/shared/wiki/.scope.toml
```

```toml
# ~/.duduclaw/shared/wiki/.scope.toml

# Identity is owned by the IdentityProvider sync — no agent may write here
[namespaces."identity"]
mode         = "read_only"
synced_from  = "identity-provider"

# ACL is owned by the governance policy bundle
[namespaces."access"]
mode         = "read_only"
synced_from  = "policy-registry"

# SOPs continue to be agent-writable (default for unlisted namespaces).
# This entry is technically redundant — it documents intent.
[namespaces."SOP"]
mode         = "agent_writable"

# Production policies are operator-only — never writable via MCP
[namespaces."policies"]
mode         = "operator_only"

# WP3: only these specific agents may write this namespace (exact id match).
# The operator can always write. An empty `agents = []` is fail-closed — it
# denies every agent (operator-only equivalent), never "open to all".
[namespaces."team-knowledge"]
mode         = "agent_allowlist"
agents       = ["agnes", "boss"]
```

### `agent_allowlist` — who may write (WP3)

This is the direct answer to "誰可以寫入這個 namespace". List the exact agent
ids allowed to write a top-level namespace; every other agent is denied and the
denial is audited. Matching is **exact-equality** — `agnes` on the list does not
admit an agent id `agnes-2`. The operator (CLI / sync) is always allowed. Both
`shared_wiki_write` and `shared_wiki_delete` honour the policy.

### Verify

Agents can introspect the active policy:

```text
> wiki_namespace_status
```

Returns the parsed policy as JSON plus the resolved file path.
Unlisted namespaces are reported as `agent_writable` (the default).

### Defaults

- **No `.scope.toml`** ⇒ no policy ⇒ existing v1.10.1 behaviour. No regression.
- **Malformed `.scope.toml`** ⇒ logged warning + treated as no policy. The gateway is never blocked by a broken policy file.
- **Hot reload** is automatic — the policy is re-read on every write/delete.

### Common pitfalls

- ❌ `mode = "read_only"` without `synced_from` is **rejected at parse time**. There is no "no one may write" mode short of `operator_only`.
- ⚠️ `mode = "agent_allowlist"` with an empty or absent `agents` list parses fine but denies **every** agent at write time (fail-closed). Fill in `agents` or the namespace is effectively operator-only.
- ❌ Locking down `SOP/` will block evolution writes silently from the agent's perspective. Use `wiki_namespace_status` first to confirm what's writable.

---

## §1 — Identity Resolution

### When to use

Whenever your authoritative person registry is an external system
(Notion People DB, LDAP, a custom HTTP service) — i.e. *whenever you
have more than two team members*. The wiki cache (`shared/wiki/identity/people/*.md`) is fine for solo / small deployments; it stops scaling at the point where you need centralised onboarding/offboarding.

### Setup A — wiki-cache-only (simplest)

For small deployments. Drop a per-person markdown file:

```yaml
# ~/.duduclaw/shared/wiki/identity/people/ruby.md
---
person_id: person_2f9
display_name: Ruby Lin
roles: [customer-pm, project-lead]
project_ids: [proj-alpha, proj-beta]
emails: [ruby@example.com]
channel_handles:
  discord: "1234567890"
  line: "Uabc..."
---

Free-form notes about Ruby.
```

That's it. Channel replies will now auto-resolve `1234567890` → `Ruby Lin` and
inject a `<sender>` block into the system prompt.

### Setup B — Notion-backed (production)

```toml
# config.toml
[identity]
provider = "chained"           # chained → cache → upstream
cache    = "wiki"              # uses ~/.duduclaw/shared/wiki/identity/people/
upstream = "notion"

[identity.notion]
database_id     = "abc123def456..."
api_key_enc     = "<encrypted via duduclaw-security>"
refresh_seconds = 300          # since v1.68.0: successful lookups are cached this long; 0 = no cache

# Optional — adjust if your Notion property names differ from defaults.
[identity.notion.field_map]
name           = "Name"
roles          = "Roles"
projects       = "Projects"
projects_kind  = "multi_select"      # or "relation"
emails         = "Email"

[identity.notion.field_map.channel_props]
discord  = "Discord ID"
line     = "Line ID"
telegram = "Telegram ID"
email    = "Email"
```

Lock the policy down so only the Notion sync can write the wiki cache:

```toml
# ~/.duduclaw/shared/wiki/.scope.toml
[namespaces."identity"]
mode         = "read_only"
synced_from  = "identity-provider"
```

### Verify

```text
> identity_resolve channel=discord external_id=1234567890
```

Returns the canonical record as JSON, or "no match" for unknowns.
Watch the `audit.unified_log` filter `source=identity` to see hit/miss
attribution per call.

### What changes for agents

- Channel system prompts gain a `<sender>` block so SOUL.md rules like
  "reject non-project members" are evaluable from data. The agent no
  longer needs to grep `shared_wiki_read("identity/discord-users.md")`.
- Agents that previously used the legacy `shared/wiki/identity/discord-users.md`
  single-document table can keep using it — `shared_wiki_read` still
  works against any path. The new structured format is opt-in.

### Common pitfalls

- ❌ Forgetting to lock down `identity/` namespace via `.scope.toml`. Without it, an evolving agent can still write into the cache directory and create drift with Notion.
- ❌ Not encrypting `api_key`. Use `duduclaw security encrypt <secret>` and store the result as `api_key_enc`.

---

## §2 — Per-agent Odoo credential isolation

### When to use

The moment you have **two or more agents touching the same Odoo
instance** — even if they share a tenant. Without per-agent isolation:
- Cross-project data is visible to every agent.
- Odoo's audit log attributes everything to one shared admin user.
- A compromised agent has full admin scope.

### Setup

For each agent, drop an `[odoo]` block in its `agent.toml`:

```toml
# ~/.duduclaw/agents/proj-alpha-pm/agent.toml

[agent]
name = "proj-alpha-pm"
# ... existing fields ...

[odoo]
profile         = "alpha"
url             = "https://erp.example.com" # optional: this agent's own Odoo server
db              = "alpha_prod"              # optional: database on that server
username        = "agent_alpha_pm"          # service account (provisioned in Odoo)
api_key_enc     = "<encrypted>"             # agent-specific Odoo API key
allowed_models  = ["crm.lead", "sale.order", "project.task"]
allowed_actions = ["read", "search", "write:crm.lead"]
company_ids     = [1, 2]                    # multi-company narrowing
```

Per-agent `[odoo]` blocks are entirely optional — agents without one
fall back to the global `config.toml [odoo]` exactly as before.

`url` and `db` are optional too: set them only when this agent must talk to a
different Odoo server than the global one. Both the runtime connection and the
dashboard's agent connection test merge the override over the global block
through the same function, so a passing test means the agent really connects
to that server. `url` must be HTTPS to a public host (plain `http://` only for `localhost` / `127.0.0.1`; private, link-local and metadata addresses and URLs with a user name are refused; every address the host name resolves to is checked and pinned), `db` is at most 63 characters of `[a-zA-Z0-9_-]`. An administrator can allow Odoo servers on an internal network with the single global switch `config.toml [odoo] allow_private_network = true` (default `false`; agents cannot set it): internal addresses are then accepted over HTTPS or HTTP, while loopback, link-local and cloud-metadata addresses stay refused. An override that fails these checks refuses the connection (it never falls back to the global server). When you change `url`, set the
agent's key again for the new server. The whole `[odoo]` table is admin-only.

### Action whitelist syntax

`allowed_actions` accepts:

- **Bare verbs** — `"read"`, `"search"`, `"create"`, `"write"`, `"unlink"`, `"action"`, `"execute"`. The verb applies to all models.
- **Qualified verbs** — `"write:crm.lead"`, `"execute:sale.order"`. The verb applies *only* to that model.

`action` / `action:model` cover generic `odoo_execute` calls to `action_*`, `button_*` and `toggle_*` methods. This runtime verb is separate from `execute`. The dashboard offers **Button actions** for `action` / `action:model` and **Other methods** for `execute` / `execute:model`. Existing button grants are editable in their table rule.

Mix freely:

```toml
allowed_actions = [
  "read",                 # all models, read-class
  "search",               # all models, search-class
  "write:crm.lead",       # write only to crm.lead
  "create:sale.order",    # create only sale orders
]
```

Empty `allowed_actions` ⇒ no action restriction (defer to Odoo ACL).
Empty `allowed_models` ⇒ no model restriction.

The dashboard groups rules by table. **Query and read** writes both `read` and `search`; a legacy single verb is marked partial and normalized on save. Selecting a sensitive table such as `hr.employee` requires confirmation and automatically adds the per-employee `unblock_models` exception. Removing its last use removes the exception. `ir.model`, `ir.model.fields` and `ir.attachment` remain read-only. Unknown legacy action entries remain read-only and are preserved until explicitly removed. See [Dashboard employee permissions](../guides/dashboard-settings.md#integrations--odoo--employee-permissions).

### Mint per-agent API keys with the right scopes

> **Since v1.82 (RM-134):** dashboard-issued keys live in the hashed key store
> `<home>/mcp_keys.db`, not in `config.toml`. Create a per-agent key on
> **Manage → Integrations → MCP API keys** with **Key for: An AI employee's own
> key** (`mcp_keys.create` with `agent_id` = the agent id, the scopes below,
> an expiry you choose). The agent id must be the agent's directory name under
> `agents/`, exact case; an `[agent] name` / display name is refused
> (`mcp_key_employee_id_required`). The key is bound to that agent at creation and
> the binding never changes. It is an internal key that acts as that agent and
> never carries `admin`. For a colleague's Personal employee, only the owner, or
> an Admin after a fresh audited step-up, can create it. Disable, re-date or delete
> it there by key id. A key created by **client ID** is always external and cannot
> use an agent id. At its first v1.82 start the gateway moves the `[mcp_keys]`
> entries shown below into the store; an internal entry is bound to an agent only
> when its `client_id` exactly equals an existing agent directory id (so
> `proj-alpha-pm` below is bound only if `agents/proj-alpha-pm/` exists) and every
> one of its scopes is one a per-agent key may carry (never `admin`). An agent's
> legacy entry with `admin` or another scope not allowed for a per-agent key is
> moved with its scopes unchanged but unbound — refused over HTTP and marked
> *Needs re-issue: scopes not allowed for a per-employee key* on the keys page —
> so re-issue that agent's key by its agent id and delete the old one. On a key
> bound to an agent only the per-agent scopes ever take effect. On that
> first start the gateway also issues itself a new internal key and ignores every
> `gateway-internal` entry already in `config.toml`. After that move, a
> `[mcp_keys]` entry added to `config.toml` is ignored and never authenticates.
> The `config.toml` example and the reload paragraph after it describe v1.81 and
> earlier.
>
> **Per-agent keys on the HTTP MCP server (`duduclaw http-server`).** A per-agent
> key used over HTTP can call only tools that act on its own agent by design:
> tools that operate on the HTTP server's own agent (`working_state_*`, `mail_*`,
> `wiki_*`, `canvas_*`, `send_message`, `tasks_list`, `odoo_*`, …) are refused to
> every key except the gateway's own internal key, and the remaining tools run as
> the key's bound agent, never as the server's. A key whose bound agent no longer
> exists (removed, renamed or `status = "deleted"`), or an internal key with no
> binding, is refused every tool over HTTP and is shown as *Employee no longer
> exists* on the keys page. Each agent's own stdio `mcp-server` is unchanged.
>
> **Deleting an agent revokes its keys for good.** Deleting an agent on the
> dashboard deletes (tombstones, audits) every key bound to it in the same
> operation. Deleted agents cannot be restored from the dashboard; even if an
> operator restores their files on disk, their keys stay deleted. A later agent
> created with the same id never gets them — issue it a new key. An agent removed
> outside the gateway (MCP `agent_remove`, a manually removed folder) or marked
> `status = "deleted"` loses its keys at the gateway's next key sweep (at start and
> every 10 minutes). Where the operating system reports folder creation times, a
> key older than its agent's folder (an agent re-created under a removed one's id)
> is refused over HTTP even before that sweep; restoring or copying the whole home
> resets those times, so per-agent keys issued before such a restore must be
> re-issued. See [MCP tools](../guides/mcp-tools.md#mcp-api-keys-ddc_env_-since-v182).

Before v1.82 the MCP key registry (`config.toml [mcp_keys]`) gated which scopes
each agent's API key may exercise:

```toml
[mcp_keys]
"ddc_prod_<hex>" = {
  client_id = "proj-alpha-pm",
  scopes    = "memory:read,memory:write,wiki:read,wiki:write,messaging:send,identity:read,odoo:read,odoo:write",
  is_external = false,
}

[mcp_keys."ddc_prod_<another_hex>"]
client_id = "auditor"
scopes    = "memory:read,wiki:read,odoo:read"   # read-only auditor agent
is_external = false
```

| Scope | Tools |
|---|---|
| `odoo:read` | `odoo_status`, `odoo_connect`, `odoo_search`, `odoo_crm_leads`, `odoo_sale_orders`, `odoo_inventory_*`, `odoo_invoice_list`, `odoo_payment_status` |
| `odoo:write` | `odoo_crm_create_lead`, `odoo_crm_update_stage`, `odoo_sale_create_quotation` |
| `odoo:execute` | `odoo_sale_confirm`, `odoo_execute`, `odoo_report` |

Both gates must pass: the **API key scope** (system layer) AND the
agent's **`allowed_actions`** (data-layer defence-in-depth).

Editing `[mcp_keys]` — minting, rescoping, or deleting a key — takes effect
on the **next MCP call**, no gateway restart required: the key registry
watches `config.toml`'s mtime and reloads whenever it changes. A reload that
itself fails (I/O error, malformed TOML) denies that call rather than
falling back to the previous cached registry — a broken edit can only ever
tighten access, never accidentally keep a revoked key alive.

### Verify

```text
> odoo_connect
```

Connection report shows `agent=<id>, profile=<profile>` so you can
confirm the right slot was used. Try a write tool with a read-only key
— it should be denied **before** any HTTP call leaves the process. Tail
`tool_calls.jsonl` (or filter `source=tool` in `audit.unified_log`) to
see the per-call attribution row with `profile=...; ok=...`.

### Module switches (`features_*`, enforced since v1.68.0)

The global `config.toml [odoo]` switches `features_crm`, `features_sale`, `features_inventory`, `features_accounting`, `features_project` and `features_hr` now gate calls. A call on a model whose module is switched off is refused before any connection and before the per-agent ACL, with an MCP error and an audit row. Models map to modules by prefix: `crm.*` → crm, `sale.*` → sale, `stock.*` / `product.*` → inventory, `account.*` → accounting, `project.*` → project, `hr.*` → hr; other models such as `res.partner` are never gated. The generic `odoo_search`, `odoo_execute` and `odoo_schema_fields` are checked against the model they name. Tools stay in `tools/list`.

**Behaviour change:** crm, sale, inventory and accounting default to on; **project and hr default to off**. An install that never wrote `features_project` / `features_hr` can no longer search or execute on `project.*` / `hr.*` until those switches are turned on (Odoo page in the dashboard, or `config.toml`).

`[odoo] protocol = "xmlrpc"` is refused when saved; only JSON-RPC (or an absent key) is accepted.

### Migration from a single global Odoo account

1. Provision per-agent service accounts in Odoo (one `res.users` row per agent).
2. For each agent, encrypt its API key (`duduclaw security encrypt <key>`) and add the `[odoo]` block to its `agent.toml` (with the agent-specific `api_key_enc`).
3. Tighten per-agent MCP key scopes — drop `admin` if you previously granted it; grant only the specific `odoo:*` scopes the agent needs.
4. Restart the gateway. Each agent's next `odoo_connect` will authenticate as its own user; subsequent calls are routed through the per-agent pool slot.
5. Verify in Odoo's audit log that distinct `res.users` IDs now appear for each agent.

The single-tenant deployment continues to work unchanged through every
step — there is no flag-day requirement.

### Common pitfalls

- ❌ Granting `admin` MCP scope to every agent — it bypasses every other gate. Use the explicit `odoo:read`/`write`/`execute` triplet.
- ❌ Forgetting `allowed_models` — without it, an `odoo:execute`-capable agent can call `odoo_execute` against `res.users` (or any other model). Defence-in-depth requires both layers.
- ❌ Sharing `api_key_enc` across agents (same key, different `agent.toml` files). Audit attribution would still distinguish them by `agent_id`, but Odoo's own audit log would not.

---

## SOUL.md adjustments

RFC-21 §1 step 5 originally proposed updating the SOUL.md *template
generator* — but DuDuClaw's `create_agent` MCP tool does not have a
template generator (the `soul:` parameter is written verbatim to disk).
There is therefore no programmatic step-5 change.

What operators **should** do when authoring SOUL.md for new agents:

- **Replace** patterns like
  > 確認對方是專案成員後再回覆 — 可從 `shared/wiki/identity/discord-users.md` 查詢
- **With** patterns like
  > 系統提示中的 `<sender>` 區塊已預先解析對方身份。`<sender>.project_ids` 若不含本專案，禮貌拒絕；含本專案，正常服務。

Existing SOUL.md files keep working unchanged — `shared_wiki_read` is
not deprecated. The new pattern is purely a UX improvement (mid-
reasoning wiki lookups become direct system-prompt evaluations).

---

## Audit log fields

After RFC-21 lands, three new audit categories appear in
`audit.unified_log`:

| `source` | `event_type` | When |
|---|---|---|
| `wiki` | `write_denied` | `shared_wiki_write` rejected by `.scope.toml` |
| `wiki` | `delete_denied` | `shared_wiki_delete` rejected by `.scope.toml` |
| `tool` (existing) | `policy_denied` | Odoo `allowed_models`/`allowed_actions` rejected the call before HTTP |
| `tool` (existing) | normal Odoo call | Now carries `params_summary = "profile=...; tool=...; ok=..."` for attribution |
| `identity` | `resolve` | Each `identity_resolve` call (hit/miss + person_id_hash) — surfaced via `tracing::info!`, not yet a structured audit row in v1.10.1 |

The dashboard's Logs page filter chips already cover `wiki` and `tool`;
`identity` will surface once a future patch wires the resolver into
`append_audit_event`.

---

## Reference

- [`RFC-21-identity-credential-isolation.md`](RFC-21-identity-credential-isolation.md) — the design doc this guide implements.
- [`docs/features/17-wiki-knowledge-layer.md`](../features/17-wiki-knowledge-layer.md) — overview of the shared wiki + namespace policy section.
- [`crates/duduclaw-identity/`](../../crates/duduclaw-identity) — `IdentityProvider` trait, `WikiCacheIdentityProvider`, `NotionIdentityProvider`, `ChainedProvider`.
- [`crates/duduclaw-cli/src/odoo_pool.rs`](../../crates/duduclaw-cli/src/odoo_pool.rs) — per-agent connector pool.
- [`crates/duduclaw-odoo/src/agent_config.rs`](../../crates/duduclaw-odoo/src/agent_config.rs) — `agent.toml [odoo]` parser + `OdooConfigResolver`.
