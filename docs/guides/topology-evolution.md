# Semi-automatic topology evolution (D5, human-gated)

DuDuClaw's autonomous evolution (GVU / AEE, defaulting from v3 onward to AEE playbook
entries rather than rewriting the whole SOUL.md; see chapter 12 of
`docs/architecture/evolution-engine.md`) only optimizes "nodes": each agent's prompt and
behavior rules. The "edges" between agents, the `reports_to` hierarchy that decides who
a given class of task routes to, have always been hardcoded. D5 turns that edge into
something evolvable, but every change still has to pass human approval; the machine
only handles proposing and gathering evidence.

Design lineage: GPTSwarm (arXiv:2402.16823, topology as a learnable object), AFlow
(2410.10762), and ADAS (2408.08435: fully automatic control-flow rewriting is the
highest-runaway-risk capability, which is exactly why D5 deliberately stops short of
full automation).

> Off by default. The whole mechanism only activates when `config.toml` sets
> `[topology_evolution] enabled = true`; with it off, the dispatch path is
> byte-identical to plain `FixedHierarchy`.

## How it works

1. **Evidence analysis** (a pure function, unit-testable)
   A background driver scans the task store every `tick_secs`, aggregating quality
   signals for each `(agent, task_class)` over the last `lookback_days` days: the
   MAV/review rejection rate, the needs_human escalation rate, and the count of
   goal-loop no-progress oscillations. `task_class` is taken from a task's first tag
   (the same convention as D4's RoundRobin), falling back to priority when there is no
   tag. The sample base is "settled goal-mode tasks" (status done / needs_human /
   failed), where needs_human, failed, or `retry_count > 0` each count as one
   rejection.

2. **Proposal** (not a direct change)
   When an agent's sample count for a given task class is ≥ `min_samples` and its
   rejection rate is ≥ `reject_rate_threshold`, and a sibling under the same
   `reports_to` parent handles the same task class better (lower rejection rate, also
   with enough samples), the driver produces one `reroute` proposal with evidence
   attached (sample count, rejection rate, up to 10 sample task ids). With no qualifying
   sibling, no proposal is made: an empty result beats a fabricated one. At most one
   proposal per tick.

3. **The human gate (cannot be bypassed)**
   Every proposal goes through `ApprovalBroker` (`action_kind = "topology_reroute"`).
   In ActionGuard terms this is **always-human**: no LLM judge is involved, it is not
   relaxed by `autonomy_level`, and the code only ever calls `request` + `poll`, with no
   automatic approval path. TTL expiry means DENY (the broker fails closed). A
   human approves or rejects it via the dashboard's `approvals.decide` or a channel
   button.

4. **Taking effect and the observation window**
   Once approved, it is written to `~/.duduclaw/routing_overrides.json` (advisory lock
   + atomic temp-file rename). `FixedHierarchy` checks for an active override before
   dispatching: a match on `(task_class, from_agent)` reroutes to `to_agent`. A missing
   or corrupted override file is always treated as no override, and routing falls back
   to the status quo (fail-safe). Once active, it enters an `observe_hours` (default
   24h) observation window.

5. **Automatic rollback**
   If, during the observation window, `to_agent`'s rejection rate for that task class
   reaches or exceeds `from_agent`'s historical baseline, the override is immediately
   `rolled_back` and routing reverts automatically. If the window passes and the new
   route genuinely beats the baseline, it becomes `confirmed`. Insufficient samples
   extend the observation window once; still insufficient after that and it is
   `rolled_back` (a conservative default).

6. **Guarding against a proposal storm**
   The same `(task_class, from_agent)` pair gets at most one proposal within
   `proposal_cooldown_days` (default 7 days, including rejected ones), recorded in the
   override file's proposal log; an active override or a pending proposal also
   suppresses a repeat. `dispatch_guard`'s sliding window still applies as usual; D5
   does not bypass it.

Every proposal, approval, rollback, and confirmation is written to `events.db` and the
dashboard Activity Feed (`topology.proposed` / `topology.approved` /
`topology.rejected` / `topology.rolled_back` / `topology.confirmed` /
`topology.extended`).

## Configuration

```toml
[topology_evolution]
enabled = false            # master switch, off by default
lookback_days = 14         # evidence lookback window (days)
min_samples = 5            # minimum settled sample count for an (agent, task_class) cell
reject_rate_threshold = 0.6  # rejection-rate threshold that triggers a proposal
observe_hours = 24         # observation window after approval (hours)
proposal_cooldown_days = 7 # proposal cooldown for the same edge (days)
tick_secs = 3600           # driver tick period (seconds)
approval_ttl_secs = 86400  # TTL for a reroute approval (seconds); expiry = reject
```

## Dashboard RPC

`topology.list` (require_manager) returns the current routing overrides and pending
reroute proposals for the dashboard to render D5 state. Approve/reject reuses the
existing `approvals.list` / `approvals.decide`.

## Risks and boundaries

- **Off by default**, and it also needs `ApprovalBroker` to be available or D5 does not
  start at all: a proposal mechanism without a human gate is not allowed to exist.
- The machine only ever does reversible things (propose, observe, roll back); the
  irreversible act (actually changing the route) is always left for a human to decide.
- D5 only layers on top of the default `FixedHierarchy` hierarchical routing. When an
  operator has explicitly chosen `RoundRobin` / `LlmSelect` (`[dispatch] policy`), that
  is a deliberate routing choice, and a D5 override only takes effect when that policy's
  empty roster falls back to the hierarchy.
- An override is a routing-layer change; it does not retroactively touch tasks already
  in flight. Already-dispatched in-flight tasks keep their original route; the new
  route (or a rollback) only applies to future dispatches.

Per the opus-playbook observation-window discipline, D5 should only be enabled once
D1–D4 are stable and the eval sample size is sufficient.
