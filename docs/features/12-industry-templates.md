# Industry Templates & Odoo ERP Bridge

> Out-of-the-box business intelligence — deploy a domain-expert agent in minutes.

---

## Manage templates in the dashboard (v1.80.0)

Administrators can browse employee and team templates in either edition. Details and the mandatory review step show the full raw `SOUL.md`, `CONTRACT.toml` and every `SKILL.md`, including each team member. Text is displayed literally rather than rendered as HTML. Review all retained text for secrets and personal data before checking the confirmation box. Correct unsafe free text in the source employee first; company-template editing only narrows tools and skill selection.

Copy/export uses `templates.copy` with `preview: true` without saving a template. Confirmation must include `reviewed: true` and the returned `review_digest`; the server recaptures the source and refuses changed content. `templates.save` and `templates.apply` also require the literal boolean `reviewed: true`; company templates additionally require the reviewed revision. Each employee creation in a team rechecks the revision. If it changes or cannot be validated, application stops with `template_changed` and the already-created employee ids; those employees are kept and the remaining members are not created.

Copies retain projected effective preset configuration and reviewed personality, contract and skill instruction text. They do not read memory/history, `.mcp.json`, channel configuration or arbitrary custom skill assets. Account-pool restrictions cause refusal. Runtime credential/environment fields and nonportable sections are omitted; credential keys in retained policy sections are refused. The entire `[fork]` table is dropped, `db_sources` and `recipients` are emptied, `git_credentials` is disabled, and employee `recipient_guard` is forced to `ask`. Heartbeat and autonomous evolution start disabled. Recognized credential shapes in retained text are masked by the existing diagnostic redactor, which can retain the first/last four characters. **Unknown tokens, short passwords, personal data and recipient identifiers embedded in free text are not reliably removed.** This is mandatory human review, not a secret/PII removal guarantee.

Every raw SOUL, contract and skill document is scanned before saving, and the actual documents are scanned again before employee writes. The existing hidden-content and input-injection scanners reject findings; short HTML comments and single invisible/zero-width characters are rejected too. Scan failure refuses the operation with the filename. No flagged content is copied. A detection rule can also reject instructional examples; revise the source rather than bypassing scanning. Unix reads use `O_NOFOLLOW` and metadata from the opened descriptor, including the hard-link and size checks. This does not pin ancestor directories against same-OS-user replacement.

All five library RPCs (`templates.catalog/get/copy/save/apply`) require a current Admin. Company templates live under `<home>/company_templates/`, with atomic saves, one revision per team, and audit records without document contents. Built-ins are read-only; premium copies retain their license requirement. A team copy includes its leader and descendants (up to 50), preserving internal reporting relationships. Edits may remove skills/members, reduce explicit tool allowlists or add denials, never widen permissions; removing a leader requires removing their descendants. Disabled skills remain disabled. Custom scripts are excluded; pristine bundled office skills restore their bundled assets. Company copies exclude pack knowledge extras and evaluation suites.

Apply asks for employee ids and display names, plus department and active-user Operator bindings in the company edition. Existing creation, product and license limits still apply. Partial results list committed employees and never automatically retry the whole team.


## The Metaphor: Restaurant Set Menus vs. A La Carte

When you open a new restaurant, you have two choices:
- **A la carte**: Design every dish from scratch. Maximum flexibility, maximum effort.
- **Set menus**: Start with a proven combination, then customize to taste. Fast to launch, easy to iterate.

DuDuClaw's industry templates are set menus for agent deployment. Each template includes everything an agent needs to operate in a specific industry — personality, behavioral rules, and domain knowledge — ready to deploy immediately.

---

## Industry Templates

### What's in a Template

Each template is a complete agent starter kit:

```
templates/{industry}/
├── SOUL.md           # Agent personality tuned for the industry
├── CONTRACT.toml     # Industry-specific behavioral boundaries
├── agent.toml        # Agent configuration
└── …                 # Industry extras: restaurant FAQ.json + PROACTIVE.md,
                      # manufacturing SOP-template/, trading price-list-template.csv
```

**SOUL.md** — The agent's personality is pre-configured with industry-appropriate communication style:
- A manufacturing agent speaks in precise, metric-driven language
- A restaurant agent is warm, service-oriented, and handles food-related queries naturally
- A trading agent is concise, numbers-focused, and risk-aware

**CONTRACT.toml** — Behavioral boundaries reflect industry regulations:
- A manufacturing agent must not approve an equipment restart without human confirmation
- A restaurant agent must always include allergen warnings when discussing menu items
- A trading agent must always include trade terms (FOB/CIF/EXW) in every quotation

**Extras** — Starter material the agent works from: a FAQ and a proactive-check schedule for the restaurant, an equipment-abnormality SOP template for manufacturing, a price-list template for trading. Replace the placeholder values with your own data.

### Available Templates

**Manufacturing** — A factory operations assistant: monitors production, reports anomalies with a severity tag, relays SOP procedures and coordinates between shifts.

**Restaurant** — A customer service assistant: answers inquiries and menu questions, takes reservations, and lists allergens when menu items come up.

**Trading** — An international trade assistant: answers buyer inquiries, quotes from the price list with trade terms and a validity period, and tracks orders between buyers and suppliers.

### Customization Flow

Templates are starting points, not straightjackets:

```
Step 1: Deploy template
  $ duduclaw wizard            # pick the industry from the menu

Step 2: Customize personality
  Edit SOUL.md to match your specific brand voice

Step 3: Adjust boundaries
  Modify CONTRACT.toml for your specific compliance requirements

Step 4: Add domain knowledge
  Import your menu, suppliers, procedures into the wiki

Step 5: Let evolution take over
  SOUL.md stays as you wrote it (agents cannot edit it). What the
  agent learns is a set of small playbook rules, each linked to an
  eval case and retired on its own when it stops helping, all inside
  your contract boundaries (see features/38)
```

---

## One Pack Format

An industry template, an expert pack (a whole team), and a job preset are three shapes of one idea: a declaration that produces pre-configured AI employees. They now share a single manifest schema and a single command.

`pack.toml` says which shape it is:

```toml
[pack]
schema  = 1
id      = "clinic-team"
kind    = "team"        # "preset" = one job config | "team" = a roster | "template" = one industry persona
tier    = "free"        # "free" | "premium"
version = "1.0.0"
label   = "Aesthetic / dental clinic"
description = "A front desk plus five department workers"

[[pack.agents]]
name = "clinic-assistant"
role = "front_desk"
```

One command reads and installs every shape:

```bash
duduclaw pack list                 # installed packs, local presets, and what the catalog offers
duduclaw pack inspect ./my-team    # normalized view, including which dialect it was read from
duduclaw pack install ./my-team    # routes by kind; premium content checks the licence in one place
```

`kind` decides the route — a preset lands in the preset store (`presets/<id>/preset.toml`, bound to an employee with `duduclaw preset bind`), a team or industry pack goes through the full expert-pack security pipeline described in [features/32](32-expert-packs.md). `tier` is the only premium decision left: four separate code paths used to re-derive "is this paid content" from a directory path, and now a pack carries its own tier and one predicate reads it.

**The three older manifest dialects are still read.** `expert.toml`, `team.toml` and `preset.toml` are read verbatim — nothing on disk is rewritten, and the premium content tree (whose compliance rules are reviewed line by line by a human) is never machine-converted. `expert.toml`, `team.toml` and the industry pack directory layout are deprecated and will be removed together with the rewritten premium templates in a later version (no version number is set); `preset.toml` is the storage format of job presets and is not deprecated. `duduclaw expert install` was removed in v1.69.0 in favour of `duduclaw pack install`. To see what your legacy file looks like under the new schema, run `duduclaw pack inspect <dir> --emit-canonical`; it prints, it does not write.

---

## The Odoo ERP Bridge

### The Problem

AI agents can *talk about* business operations, but they can't *execute* them. An agent might know that a customer needs an invoice, but it can't create one in your ERP system — unless it has a bridge.

### The Solution

DuDuClaw includes a middleware that connects agents directly to Odoo, one of the world's most widely-used open-source ERP systems:

```
User: "Create a sales order for customer ABC, 10 units of Widget X"
     |
     v
Agent understands the intent
     |
     v
Agent calls MCP tools: odoo_sale_create_quotation, then odoo_sale_confirm
     |
     v
DuDuClaw Odoo Bridge translates to JSON-RPC call
     |
     v
Odoo ERP creates the sales order
     |
     v
Bridge returns the result (order number, total)
     |
     v
Agent: "Sales order SO-2024-0042 created for ABC.
        10 units of Widget X, total: $1,500."
```

### Available Operations (20 MCP Tools)

The bridge exposes these tools (`crates/duduclaw-core/src/mcp_tool_defs/odoo.rs`):

- **Connection**: `odoo_connect`, `odoo_status`
- **CRM**: `odoo_crm_leads` (list leads), `odoo_crm_create_lead`, `odoo_crm_update_stage`
- **Sales**: `odoo_sale_orders` (list orders), `odoo_sale_create_quotation`, `odoo_sale_confirm`
- **Inventory**: `odoo_inventory_products` (search products), `odoo_inventory_check` (stock level)
- **Accounting**: `odoo_invoice_list`, `odoo_payment_status`
- **Generic**: `odoo_search` (search any model), `odoo_execute` (call a model method), `odoo_report`, `odoo_partner_search`, `odoo_schema_fields`, `odoo_schema_overview`, `odoo_create` / `odoo_write` (create or update records in an explicitly granted model, v1.81.0)

### Schema scans and field compatibility

Rescan covers the saved global connection and every employee’s effective `[odoo]` settings, including company scope. Authenticated identities are deduplicated, at most two connections scan concurrently, and results show Ready or Failed for each employee. Metadata stays isolated by connection identity; one Odoo user never reuses another user’s snapshot. The dashboard actions share their persisted 60-second manual cooldown. After successful preparation, obsolete profile data is removed only if all effective identities are known. Cleanup holds a catalog lock while deleting idle lock files; an in-progress scan and its lock are preserved.

The overview without filters returns model names and short labels only, capped at 16 KiB. Fields are included only for a specific `model` or a query matching at most five authorized models, capped at 32 KiB. Responses include total matching model count and truncation. The shared-wiki summary is capped at 16 KiB; `odoo/schema-fields` remains a search-only `deep` reference page capped at 32 KiB and is not automatically injected into context.

After an Odoo upgrade or customization, an administrator can use **Re-scan Odoo structure** on the Odoo page (dashboard RPC `odoo.schema_scan`, administrator only). The scan reads the server version, installed-module metadata, the model catalog and `fields_get`; it never reads business rows or binary values. It records field names, types, labels, relations and required / readonly / stored flags, including `x_*` custom-field markers. Manual scans have a persisted 60-second cooldown per saved profile, including failures. Invalid technical names are dropped and counted; labels are injection-screened and capped at 120 characters. The employee overview explicitly marks Odoo metadata as untrusted. All Odoo RPC response bodies are streamed with a 32 MiB cap before parsing.

Each scan retains at most **400 models and 500 fields per model**. Metadata and field discovery have a **five-minute budget**, with four concurrent `fields_get` calls; account ACL probing has a separate 30-second overall limit. Built-in models come first, followed by models from installed business modules (CRM, sales, purchase, stock, accounting, project, HR, manufacturing, helpdesk and website), then remaining models alphabetically. A timed-out scan is marked truncated and reuses completed fields next cycle when identity, server version and modules are unchanged. Module/model catalogs cap at 5,000 entries. A partial diff never proves removal.

The schema-versioned snapshot is stored under `<home>/odoo/schema/<effective-profile>.json`, using an atomic write and owner-only permissions on Unix. The effective profile includes the endpoint, database, Odoo user and company scope, so a snapshot from another connection is not reused. The diff records added and removed models / fields and server-version changes. This snapshot is separate from the older **Scan database** workflow that writes shared-wiki summaries.

`odoo_schema_overview` (`odoo:read`) reads the saved snapshot. An optional `model` returns that model's fields; `query` searches model / field names and labels. Results are filtered by the employee's model and action permissions, the Odoo feature switches and the security block list with that employee's effective unblock list. A default-blocked model's name, fields and relations are never returned unless it is unblocked for that employee; `odoo_schema_fields` refuses it before contacting Odoo. The gateway scan applies the same block list and feature switches for each connection, so blocked or switched-off models are never probed or saved. `odoo_status` reports snapshot age, its server version, scan failures and a **re-scan recommended** hint when the live version differs.

The long-lived gateway holding the instance lock checks every effective identity every 30 minutes, starting at gateway startup. It scans missing or truncated snapshots, changed server versions, changed installed-module names/versions, or missing access metadata. Completed scans and definitive failures consume the one-hour automatic slot; cancellation does not. MCP turns never launch scans. While preparation is pending, use `odoo_schema_fields` for a specific model. Same-version field-only changes still require a manual scan; reconnect active sessions with `odoo_connect` to refresh live field caches.

With saved connection settings, data tools connect automatically on first use, after feature and permission checks. Calling `odoo_connect` each turn is unnecessary. Login identity, edition and server version are cached locally for six hours; credential changes use a new cache entry immediately. Status displays the connected metadata without probing the server. Each data request still sends the credential for Odoo to authenticate and authorize. Invalid credentials and policy denials are not retried; only transport failures, server errors and timeouts may trigger bounded authentication retries.

Built-in reads resolve each model's live field names once per connection with `fields_get` and only remove unavailable fields from their requested projection. For example, customer search keeps `phone` and omits `mobile` when it no longer exists. It never adds fields to the safe customer projection. Metadata lookup failure, or a projection with no surviving fields, fails the read instead of requesting every field. Explicit field lists in generic queries use the same compatibility filter. The snapshot does not replace this live check. Product reads preserve `detailed_type` when available and otherwise use the server's `type` field; customer fields remain unchanged.

### Diagnosing a denied Odoo query

An empty `allowed_models` or `allowed_actions` list adds no employee restriction. It does not disable other gates: a disabled **功能模組 / Feature modules** switch, the generic-tool block list, MCP scopes / tool allowlists, and Odoo's own ACLs can still refuse the query. For a blocked model such as `res.partner`, generic record access needs an administrator to confirm the sensitive-table selection in the dashboard, which adds `unblock_models` automatically. Editing `allowed_models` alone in a configuration file does not unblock it. Removing the last table selection or action rule removes the dashboard-managed exception. **Query and read** saves both `read` and `search`; older single-verb rules are shown as partial and normalized on save. An absent employee `unblock_models` inherits the global list, including when only a username and credential are overridden. An explicit empty list unblocks nothing; an explicit list replaces the global list. The employee edit page shows the currently blocked tables and whether the list is inherited. The dedicated `odoo_partner_search` keeps its fixed safe projection.

The action editor offers `read`/`search` (Search and read), `create`, `write`, `unlink`, `action` (Button actions) and `execute` (Other methods). For generic `odoo_execute` calls, `action` covers `action_*`, `button_*` and `toggle_*` methods such as confirm, validate and archive; `execute` covers other methods. Use `action` for all tables or `action:sale.order` for one table. Both method categories use write access as a hint; Odoo still checks each operation.

### Generic create and update: explicit grant plus a separate opt-in (v1.81.0)

`odoo_create` and `odoo_write` create or update records in any model, but unlike every other Odoo tool they follow an **explicit-grant rule**: the employee must have its own Odoo settings, the table must be ticked under **Tables** (`allowed_models` names it exactly), a rule must give that table **Create** or **Update** (`allowed_actions` holds `create` / `write`, or `create:<model>` / `write:<model>`), and the rule's **Allow general create / update** (允許通用新增／修改) switch must be on for that table (stored as `generic_create:<model>` / `generic_write:<model>` in `allowed_actions`). The switch is off by default and is per table only, so **existing settings are unchanged**: an employee that already had Create / Update does not gain the generic tools until it is switched on. Switching it on lets the AI set any writable field of that table. No own settings, an empty list or a `*` grants nothing, and the tools are not even listed to the employee until some table is granted and opted in. Users, groups, companies, `ir.*` system tables and mail / discuss records (followers, messages, templates, channels) are never generically writable, also not through x2many commands; the tools never delete, unlink, clear, replace or archive. The empty-means-unrestricted rule above still applies to all other Odoo tools, so `odoo_crm_create_lead`, `odoo_sale_create_quotation` and the rest work exactly as before.

A granted call still passes the feature switch, the block list (system metadata tables such as `ir.model` can never be written: "This capability is not available"), the Odoo account's own create / write right from the schema snapshot (unknown counts as not allowed) and a field check: every key must be a stored, non-readonly field of the scanned model, `values` is capped at 64 KiB, and x2many lists may only create (0), update (1), link (4) or set (6) — deleting, unlinking or clearing (2, 3, 5) is refused. Record deletion is not offered by any tool. Until the background schema preparation has produced a snapshot for the connection, the calls ask the employee to wait.

DuDuClaw denial messages identify the relevant gate and setting. The separate read-only SQL connector has different semantics: `allowed_tables` must be non-empty, and its save RPC rejects an empty list. It is not the Odoo model allowlist.

### Edition Detection

Odoo comes in two editions: Community Edition (CE, open-source) and Enterprise Edition (EE, paid). Some features are only available in EE.

The bridge handles this automatically:

```
On first connection:
     |
     v
Detect Odoo edition (CE or EE)
     |
     v
Only expose MCP tools that the detected edition supports
     |
     v
If agent tries to use an EE-only feature on CE:
  → Clear error message: "This feature requires Odoo Enterprise Edition"
```

No configuration needed — the bridge probes the Odoo instance and adapts automatically.

### Event Synchronization

Beyond executing operations, the bridge can also listen for events in Odoo. Two
transports feed the same automation bus, and **both are off until you turn them
on**:

| Transport | Switch | How it works |
|---|---|---|
| Polling | `config.toml [odoo] poll_enabled` (default `false`) | Every `poll_interval_seconds` (60–86400, default 60) the gateway asks Odoo for records in `poll_models` whose `write_date` moved since the last cycle. |
| Webhook | `config.toml [odoo] webhook_enabled` (default `false`) | An Odoo automated action posts to `POST /webhook/odoo` with the shared secret from `[odoo] webhook_secret`. |

```
Odoo event occurs:
  - New lead created
  - Order status changed
  - Invoice overdue
     |
     v
Polling picks up the change  OR  Odoo posts to /webhook/odoo
     |
     v
An `odoo_event` lands on the automation bus
     |
     v
Your autopilot rules decide what happens:
  - Notify the sales team about the new lead
  - Delegate a follow-up task to an agent
  - Send a payment reminder for the overdue invoice
```

Write rules against `odoo_event` the same way you would against any other
trigger. Alongside `event_type` / `model` / `record_id`, each record's
top-level scalar fields are available directly, so a condition reads
`{"field": "state", "op": "eq", "value": "sale"}` with no extra plumbing.

Since v1.67.1 such a rule can also be created in the dashboard (Settings →
Autopilot → New Rule, trigger "An Odoo record changes"): type the Odoo field
name, for example `state`, as the condition field. Before v1.67.1 the dashboard
form could not create any rule.

Security notes, because this endpoint is reachable from outside:

- While `webhook_enabled` is off the route returns **404** — a stock install
  gives away nothing, not even that the endpoint exists.
- A missing or wrong secret is **401**. So is an *empty* configured secret: a
  half-finished setup refuses everything rather than accepting anything.
- Polling needs `poll_enabled`, a configured Odoo connection, and at least one
  valid model name in `poll_models`. Missing any of the three means no task is
  started at all.

This turns the agent from a passive tool-user into a proactive business participant that reacts to real-world events.

---

## Combining Templates with the ERP Bridge

The real power emerges when templates and the ERP bridge work together:

```
Manufacturing Template + Odoo Bridge:
  Agent monitors inventory levels (Odoo) →
  Detects low stock on critical materials →
  Automatically creates purchase orders →
  Notifies the production manager via the configured channel

Restaurant Template + Odoo Bridge:
  Agent receives a large catering order (channel message) →
  Checks ingredient availability (Odoo inventory) →
  Creates a sales order (Odoo sales) →
  Flags any allergen concerns (wiki knowledge) →
  Confirms with the customer

Trading Template + Odoo Bridge:
  Agent receives market data update →
  Cross-references with portfolio positions (Odoo) →
  Identifies positions that exceed risk thresholds →
  Sends alert to the trader with recommended actions
```

Each scenario combines the agent's domain knowledge (from the template), behavioral boundaries (from the contract), and operational capability (from the ERP bridge) into a complete business workflow.

---

## Why This Matters

### Time to Value

Without templates, deploying an industry-specific agent requires:
1. Researching the industry's terminology and processes
2. Writing a personality file that sounds natural in that domain
3. Defining appropriate behavioral boundaries
4. Building a domain knowledge base
5. Testing and iterating

With templates, steps 1-4 are pre-built. An operator can have a functioning industry-specific agent in minutes instead of days.

### Operational Depth

The Odoo bridge transforms agents from conversational assistants into operational tools. They don't just *recommend* creating an invoice — they *create* it. This bridges the gap between AI advice and business action.

### Standardization

Templates encode industry best practices. A manufacturing agent built from the template already knows about quality control standards, safety protocols, and inventory management practices. Individual operators don't need to reinvent this knowledge.

### Composability

Templates, the ERP bridge, and the evolution system work together seamlessly. The template provides the starting point, the ERP bridge provides operational capability, and the evolution system continuously improves the agent based on real-world performance — all within the safety boundaries of the contract.

---

## Interaction with Other Systems

- **Evolution Engine**: Agents deployed from templates evolve like any other agent. The template is the starting point, not the permanent state.
- **Behavioral Contracts**: Each template includes a contract tailored to the industry's compliance requirements.
- **Memory System**: Domain knowledge from the wiki is indexed and searchable through the memory system.
- **Channel Integration**: Template agents work with all 11 supported communication channels.
- **Cost Management**: ERP bridge operations are tracked in CostTelemetry for budget visibility.

---

## The Takeaway

Industry templates and the Odoo ERP bridge solve the "last mile" problem for agent deployment: getting from a general-purpose AI to a domain-expert that can actually *do things* in the real world. Templates provide the knowledge and personality; the ERP bridge provides the operational capability; and the evolution system ensures continuous improvement.
