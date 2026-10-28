# Industry Templates & Odoo ERP Bridge

> Out-of-the-box business intelligence — deploy a domain-expert agent in minutes.

---

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

### Available Operations (18 MCP Tools)

The bridge exposes these tools (`crates/duduclaw-cli/src/mcp/tools_def/odoo.rs`):

- **Connection**: `odoo_connect`, `odoo_status`
- **CRM**: `odoo_crm_leads` (list leads), `odoo_crm_create_lead`, `odoo_crm_update_stage`
- **Sales**: `odoo_sale_orders` (list orders), `odoo_sale_create_quotation`, `odoo_sale_confirm`
- **Inventory**: `odoo_inventory_products` (search products), `odoo_inventory_check` (stock level)
- **Accounting**: `odoo_invoice_list`, `odoo_payment_status`
- **Generic**: `odoo_search` (search any model), `odoo_execute` (call a model method), `odoo_report`, `odoo_partner_search`, `odoo_schema_fields`, `odoo_schema_overview`

### Schema scans and field compatibility

After an Odoo upgrade or customization, an administrator can use **Re-scan Odoo structure** on the Odoo page (dashboard RPC `odoo.schema_scan`, administrator only). The scan reads the server version, installed-module metadata, the model catalog and `fields_get`; it never reads business rows or binary values. It records field names, types, labels, relations and required / readonly / stored flags, including `x_*` custom-field markers. Manual scans have a persisted 60-second cooldown per saved profile, including failures. Invalid technical names are dropped and counted; labels are injection-screened and capped at 120 characters. The employee overview explicitly marks Odoo metadata as untrusted. All Odoo RPC response bodies are streamed with a 32 MiB cap before parsing.

A scan keeps at most **400 models and 500 fields per model**, with a **90-second total budget**. It prioritizes models used by built-in tools, then non-transient business models, and reports truncation and per-model metadata failures. The module and model discovery catalogs each have a 5,000-entry cap. A truncated comparison does not establish that a missing model or field was removed.

The schema-versioned snapshot is stored under `<home>/odoo/schema/<effective-profile>.json`, using an atomic write and owner-only permissions on Unix. The effective profile includes the endpoint, database, Odoo user and company scope, so a snapshot from another connection is not reused. The diff records added and removed models / fields and server-version changes. This snapshot is separate from the older **Scan database** workflow that writes shared-wiki summaries.

`odoo_schema_overview` (`odoo:read`) reads the saved snapshot. An optional `model` returns that model's fields; `query` searches model / field names and labels. Results are filtered by the employee's model and action permissions and the Odoo feature switches. Metadata inspection remains available on default-blocked models when those permissions allow it; it does not unblock their business records. `odoo_status` reports snapshot age, its server version, scan failures and a **re-scan recommended** hint when the live version differs.

The first successful connection without a snapshot, or a connection that detects a server-version change, starts a background scan. Automatic attempts are limited to once per hour per effective profile, including failed attempts; connecting does not wait for the scan. Same-version customizations need a manual re-scan. Reconnect other active employee sessions with `odoo_connect` to refresh their field cache.

Built-in reads resolve each model's live field names once per connection with `fields_get` and only remove unavailable fields from their requested projection. For example, customer search keeps `phone` and omits `mobile` when it no longer exists. It never adds fields to the safe customer projection. Metadata lookup failure, or a projection with no surviving fields, fails the read instead of requesting every field. Explicit field lists in generic queries use the same compatibility filter. The snapshot does not replace this live check. Product reads preserve `detailed_type` when available and otherwise use the server's `type` field; customer fields remain unchanged.

### Diagnosing a denied Odoo query

An empty `allowed_models` or `allowed_actions` list adds no employee restriction. It does not disable other gates: a disabled **功能模組 / Feature modules** switch, the generic-tool block list, MCP scopes / tool allowlists, and Odoo's own ACLs can still refuse the query. For a blocked model such as `res.partner`, generic record access needs an administrator to set `unblock_models`; `allowed_models` alone does not unblock it. An employee override's `unblock_models`, including an empty list, replaces the global value. The dedicated `odoo_partner_search` keeps its fixed safe projection.

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
