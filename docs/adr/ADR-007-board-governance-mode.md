# ADR-007: CEO/Board governance mode

- Status: Accepted (opt-in, off by default)
- Date: 2026-07-09
- Related: WP17 (`commercial/docs/TODO-client-demo-gaps-2026-07-08.md`), ADR-004, RFC-21

## Context

The target customer shifted toward company deployments — the demo客戶 runs a
~20-person team. For a non-engineering owner, "I am the board, my AI staff report
to a CEO" is a more natural mental model than raw agent config. Paperclip proves
the shape: a Board that can pause/resume/terminate and set budgets, a CEO that
proposes strategy for Board approval, and cascade budgets. A solo user does not
want any of this.

## Decision

Add an opt-in `[governance] board_mode` (default `false`). When off, every path
is byte-identical to today. When on, we map the metaphor onto existing primitives
rather than inventing new entity types:

- **Board** = human users holding board rights (aligns with WP15 finest-grain
  users + `rbac.rs`). Hard invariant: **the Board is always a human, never an
  agent.**
- **CEO** = the `reports_to` tree-root agent (existing concept).
- **Initiative** = a top-level Task Board task tagged `kind = "initiative"`
  (additive).
- All consequential decisions flow through the existing `ApprovalBroker` +
  audit. `action_kind` strings are collapsed into a typed `ApprovalKind`
  (serde-compatible with stored strings) so automation can only auto-decide safe
  kinds; `StrategicPlan` and `AgentHire` are Board-human-only.

Reuse over new build: freeze = WP1 `agent freeze`; approvals = ApprovalBroker;
budget = `budget.rs` + WP14 incident UI; hiring = existing `create_agent`;
org = `reports_to`; tasks = Task Board. Genuinely new: the strategic-proposal
flow, the Board panel, and a company-level budget layer.

## Fail-closed invariants (enforced, unit-tested — see `governance.rs`)

- `can_decide(StrategicPlan|AgentHire, decider)` is true only for a human with
  board rights; an agent identity is refused unconditionally + audited.
- `can_create_initiative` is Board-human-only; the CEO agent can be *delegated*
  an Initiative but cannot self-create one.
- In `board_mode`, no agent (including the CEO) may edit a `[budget]` value via
  MCP/tool paths — only the Board panel RPC — preventing an agent from raising
  its own cap (self-promotion). `agent_may_edit_budget(board_mode)` returns
  false when on.

## Consequences

- Solo users are unaffected (opt-in, default off; this is the hard test in the
  WP17 suite).
- The typed `ApprovalKind` benefits WP8 (skill activation) and WP16 (channel
  buttons) too — one enum, one place to reason about which kinds automation may
  touch.
- Remaining integration (tracked): CEO strategic-proposal generation, the Board
  dashboard panel, cascade company→agent budget wiring, and the AgentHire second
  approval on `create_agent`/`spawn_agent`. These build on the invariants here.
