# ERP / CRM support matrix

> A one-page reference for sales and customer conversations. For the technical decision, see [ADR-004](../adr/ADR-004-erp-connector-abstraction.md).
> Last updated: 2026-07-09.

DuDuClaw lets AI employees read and write data in your ERP / CRM directly. The table below shows current coverage in three states: **Supported** (ready to go live today), **Abstraction ready** (the skeleton for plugging in new systems is finished, integrations scheduled), and **Planned** (on the backlog, not yet started).

| System | Version / type | Status | What it can do | Isolation & audit |
|--------|----------------|--------|----------------|-------------------|
| **Odoo** | CE (Community) / EE (Enterprise) | ✅ Supported | CRM leads, quotations and sales orders, inventory lookups, invoice and payment status (17 tools in total) | Per-agent credentials, action / model allowlists, every operation audited |
| **ERPNext** | v14+ | 🔜 Planned | First validation implementation once the abstraction layer lands | Shares the same isolation mechanisms as Odoo |
| **Twenty** | Open-source CRM | 📋 Planned | CRM scenarios (leads / opportunities / contacts) | Same as above |
| Other REST/JSON-RPC ERPs | — | 📋 Under evaluation | Extensible via the planned `ErpConnector` contract (ADR-004, not implemented yet) | Would be built into the contract |

## Talking points for sales

**When a prospect says "Odoo only fits small and mid-size companies — we're bigger than that":**

Odoo is indeed most at home in companies of 15-50 people; that's its sweet spot. DuDuClaw's approach to ERP is not meant to stay tied to Odoo. ADR-004 decides on an abstract contract called `ErpConnector` with Odoo as its first implementation; that contract is not implemented yet, and today the bridge is the Odoo connector only. Once the contract lands, connecting ERPNext, Twenty, or a customer's own REST/JSON-RPC system would follow the same path, and a newly connected system would inherit per-agent credential isolation, action allowlists, and full operation auditing from the contract instead of each integration rewriting them.

So the honest pitch to a large enterprise customer is: **"Odoo runs today; for the system you use, we have a standardized integration layer that can extend to it, and ERPNext is the first scheduled validation case."** No overselling with "we support everything", and no turning the customer away either.

## What "abstraction ready" means

Once ADR-004 is finalized, the `duduclaw-erp` skeleton crate provides the trait, connection pool, scope checks, and auditing, and Odoo becomes an implementer. At that point, the work of connecting a new ERP drops from "copy the whole Odoo stack" to "implement one trait". "Abstraction ready" in the matrix refers to exactly this state: the skeleton in place and the Odoo regression tests all green.

## Related documents

- [ADR-004: ERP connector abstraction](../adr/ADR-004-erp-connector-abstraction.md) — why a trait, and the trade-offs
- [features/12-industry-templates.md](12-industry-templates.md) — a deeper look at the Odoo ERP bridge
