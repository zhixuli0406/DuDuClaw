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
└── wiki/             # Domain knowledge base
    ├── glossary.md   # Industry terminology
    ├── processes.md  # Standard operating procedures
    └── compliance.md # Regulatory requirements
```

**SOUL.md** — The agent's personality is pre-configured with industry-appropriate communication style:
- A manufacturing agent speaks in precise, metric-driven language
- A restaurant agent is warm, service-oriented, and handles food-related queries naturally
- A trading agent is concise, numbers-focused, and risk-aware

**CONTRACT.toml** — Behavioral boundaries reflect industry regulations:
- A manufacturing agent must never approve materials that fail quality thresholds
- A restaurant agent must never recommend dishes to customers with declared allergies without explicit warnings
- A trading agent must always include risk disclaimers in investment-related responses

**Wiki** — Domain knowledge that the agent can reference:
- Industry terminology and abbreviations
- Standard operating procedures
- Regulatory requirements and compliance checklists
- Common scenarios and recommended responses

### Available Templates

**Manufacturing** — Covers supply chain management, production scheduling, quality control, and equipment maintenance. The agent understands concepts like lead time, defect rates, BOM (Bill of Materials), and MRP (Material Requirements Planning).

**Restaurant** — Covers order management, inventory tracking, customer service, and food safety. The agent understands concepts like table turnover, food cost percentage, FIFO inventory rotation, and allergen management.

**Trading** — Covers market data interpretation, portfolio management, risk assessment, and compliance. The agent understands concepts like P/E ratios, margin requirements, stop-loss orders, and regulatory reporting.

### Customization Flow

Templates are starting points, not straightjackets:

```
Step 1: Deploy template
  $ duduclaw agent create --template restaurant --name "my-restaurant-agent"

Step 2: Customize personality
  Edit SOUL.md to match your specific brand voice

Step 3: Adjust boundaries
  Modify CONTRACT.toml for your specific compliance requirements

Step 4: Add domain knowledge
  Import your menu, suppliers, procedures into the wiki

Step 5: Let evolution take over
  The agent's personality refines itself through GVU cycles
  while staying within your customized contract boundaries
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

**The three older manifest dialects keep working until v1.68.0.** `expert.toml`, `team.toml` and `preset.toml` are read verbatim — nothing on disk is rewritten, and the premium content tree (whose compliance rules are reviewed line by line by a human) is never machine-converted. `duduclaw expert install` and `duduclaw preset` remain aliases over the same code path. To see what your legacy file looks like under the new schema, run `duduclaw pack inspect <dir> --emit-canonical`; it prints, it does not write.

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
Agent calls MCP tool: sale_order_create
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

### Available Operations (15 MCP Tools)

The bridge exposes operations across four business domains:

**CRM (Customer Relationship Management)**
- Qualify leads (score likelihood to convert)
- Create opportunities from qualified leads
- Update lead status and notes

**Sales**
- Create sales orders with line items
- Check order status
- Generate quotations

**Inventory**
- Check stock levels
- Adjust inventory quantities
- Track shipments

**Accounting**
- Create invoices from sales orders
- Process payments
- Check account balances

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
- **Channel Integration**: Template agents work with all 7 supported communication channels.
- **Cost Management**: ERP bridge operations are tracked in CostTelemetry for budget visibility.

---

## The Takeaway

Industry templates and the Odoo ERP bridge solve the "last mile" problem for agent deployment: getting from a general-purpose AI to a domain-expert that can actually *do things* in the real world. Templates provide the knowledge and personality; the ERP bridge provides the operational capability; and the evolution system ensures continuous improvement.
