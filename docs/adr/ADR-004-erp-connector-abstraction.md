# ADR-004: ERP connector abstraction (`trait ErpConnector`)

- Status: Accepted
- Date: 2026-07-09
- Deciders: DuDuClaw maintainers

## Context

DuDuClaw's ERP bridge currently has only one concrete type, `struct OdooConnector`
(`crates/duduclaw-odoo/src/connector.rs:103`), with no abstraction layer above it. It
provides a set of methods — connect / execute_kw / search_read / create / write / count /
version / status — and all 15 Odoo MCP tools (CRM / Sales / Inventory / Accounting,
dispatched in `crates/duduclaw-cli/src/mcp.rs`) hang directly off this one type. Per-agent
credential and scope isolation already landed in RFC-21 §2: `AgentOdooConfig` /
`OdooConfigResolver` (`crates/duduclaw-odoo/src/agent_config.rs`), a connection pool
`OdooConnectorPool` (`crates/duduclaw-cli/src/odoo_pool.rs:54`) keyed by `(agent_id, profile)`,
plus `Scope::OdooRead / OdooWrite / OdooExecute` (`crates/duduclaw-cli/src/mcp_auth.rs:52`).
The problem is that this isolation mechanism was tailor-made for Odoo — swapping in a
different ERP means rebuilding the whole thing from scratch.

Customer research (§1) sends a clear signal: Odoo's sweet spot is 15-50-person companies;
large enterprise customers won't run on Odoo alone. Bringing systems like SAP, ERPNext, and
Twenty into the same agent platform requires an adapter layer first — not copy-pasting
`OdooConnector`'s guts every time a new ERP is connected.

This pattern has already run in the project for half a year. `duduclaw-llm`'s
`trait ChatProvider` (`crates/duduclaw-llm/src/provider.rs:103`) uses `#[async_trait]` to
define three methods — `id()` / `complete()` / `stream()` — with four implementations
underneath (Anthropic / OpenAI / Gemini / OpenAI-compat), plus a data-driven capability table
in `ModelRegistry`. On the LLM side, the same platform has already proven that "one trait +
N providers + a registry" is maintainable long-term. There's no reason to reinvent this on
the ERP side.

The full spec for the trait surface (method signatures, the `duduclaw-erp` skeleton crate
split, ERPNext implementation details) was already planned out in
`commercial/docs/TODO-feature-gaps-2026-07.md` §1 — that document is the research output.
This ADR does exactly one thing: promote that plan to a formal decision and record the
trade-offs. The spec is not repeated here — both documents should be open side by side
during implementation.

## Decision

Extract a `trait ErpConnector`, mirroring the `ChatProvider` pattern: `#[async_trait]` +
a stable `id()` + registry-declared capabilities. Trait methods:

- `id()` — a stable connector id (`"odoo"` / `"erpnext"` / …), mirroring `ChatProvider::id()`.
- `capabilities()` — declares supported models / actions / webhooks, so the layer above can
  route data-drivenly.
- `search` / `read` / `create` / `update` — the CRUD four-set, mapping to Odoo's existing
  search_read / create / write.
- `execute` — generic actions (mapping to Odoo's execute_kw / business actions such as
  sale_confirm).
- `webhook_subscribe` (optional) — event subscription; connectors that don't support it
  return not-supported, so nobody is forced to implement it.

**Odoo is the first implementation**: `OdooConnector` becomes `impl ErpConnector` with zero
behavior change; the output of the existing 15 MCP tools stays byte-compatible, and the
existing test suite serves as the regression net. **ERPNext is the second implementation**,
and its purpose is to validate this abstraction — the abstraction only counts as correctly
drawn once a second vendor plugs in. A trait with only one implementation is a guess; two
is evidence.

**The per-agent credential / scope / audit triplet is part of the trait contract, not an
Odoo special case.** The RFC-21 §2 machinery — the `(agent_id, profile)`-keyed connection
pool, `allowed_models` / `allowed_actions` filtering, `profile` flowing into the audit
record — moves up to be generic: any `ErpConnector` implementation obtains its connection
through a shared `ConnectorPool<C>` and goes through the same scope checks and audit
attribution. New connectors get isolation for free; nobody connecting ERPNext can forget
to wire up permission isolation.

**MCP tool names are unified under `erp_*`** (`erp_record_search` / `erp_record_create` /
`erp_record_update` / `erp_execute` …); the old `odoo_*` names are kept as an alias for one
deprecation cycle. During the deprecation window both names remain callable; once the
window closes, `odoo_*` is removed. This means existing agents' prompts and skills don't
break within a single release.

## Consequences

**Gained:** connecting a second vendor (ERPNext) is no longer copy-paste; the isolation
mechanism is written correctly once and shared by every connector; there's now a clear
talking point for enterprise customers — "the abstraction layer is ready, X vendors are on
the roadmap" (see `docs/features/erp-support-matrix.md`); MCP tool naming converges on
`erp_*` and no longer leaks which vendor sits underneath.

**Paid:** extracting the trait has an immediate cost — splitting out the `duduclaw-erp`
skeleton crate, turning Odoo into an implementor, and getting all 15 tools green on
regression — and none of this produces any externally visible new functionality before
ERPNext actually starts. The honest trade-off: **extract now vs. wait for the second vendor**.

We chose to extract now, because the customer context bumped ERP expansion priority up
into this round, ERPNext is already queued in the backlog
(`IMPL-PLAN-remaining-gaps-2026-07.md` §E), and landing the trait together with the second
connector is the only way to actually validate whether the abstraction is correct. Waiting
until the second vendor starts to extract would force doing "abstraction + new
implementation + regression" all at once under time pressure — higher risk. The cost is
accepting a stretch of work that ships no new features, only structural rework, backed by
Odoo's existing tests to keep regression risk contained.

**Deprecation compatibility:** the `odoo_*` alias is removed one release later. Mark it
Deprecated in CHANGELOG before removal, Removed at removal time, and write the upgrade
guidance into the guide.

**If the assumption doesn't hold:** if connecting ERPNext reveals that the trait surface is
missing something (e.g. some ERPs have batch / transaction semantics Odoo doesn't), revise
this decision with a new ADR — don't expand the method list in place in this file.
