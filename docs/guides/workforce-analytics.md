# Workforce analytics — per-user AI usage

The boss's question is simple: which employee is using the AI, for what, and how
much is it costing? DuDuClaw answers it from the same token-usage telemetry it
already collects, now attributed per end-user and per channel.

## Privacy boundary (read this first)

What this feature exposes is company AI-resource usage — the same thing a company
already audits when it logs corporate email or VPN access. Two rules keep it from
becoming surveillance:

- **Aggregate first.** The default views are totals per user (request count, token
  count, cost). Nobody's individual messages show up in the usage report.
- **Detail needs operator rights.** Drilling into what a specific person asked is
  an operator-scoped action and is itself audited.

Any downstream feature that scores or flags individuals (question-quality
coaching, private-use detection) is opt-in, off by default, and documented as
"for a heads-up, never as grounds for discipline".

## Per-user cost attribution

Every channel reply now records the end-user id and channel alongside the agent
and token counts. The `token_usage` table gained two additive columns (`user_id`,
`channel`) via an idempotent migration — existing rows keep working, they just
read as unattributed.

Query it:

```text
> cost_users            # MCP tool, admin scope
{ "hours": 24 }
```

Returns each user ranked by cost:

```json
[
  { "user_id": "u-alice", "total_requests": 42, "total_input_tokens": 130000,
    "total_output_tokens": 8000, "total_cost_millicents": 5400 },
  { "user_id": "(system)", "total_requests": 12, ... }
]
```

`(system)` buckets non-human traffic (sub-agent dispatch, evolution, utility
calls) that has no end-user.

## What's attributed today

The channel-reply path (the surface most channels share) attributes user + channel.
Paths without a human user — sub-agent dispatch, cron, evolution — record as
`(system)` by design. Wiring the remaining per-channel media/tool sub-paths to the
same attribution is an ongoing sweep; where a path is not yet wired, spend still
lands on the agent, just not on a specific user.

## Roadmap (opt-in, off by default)

- **Question-quality coaching** — a periodic batch scores a sample of each user's
  messages (clarity, has-an-actionable-goal) with a cheap model and surfaces a
  "suggest training" list. Aggregate score only; no message text in the list.
- **Usage-anomaly flag** — a user whose spend deviates from their own baseline by
  N sigma is flagged, using the existing burn-rate anomaly math. Zero semantic
  judgement, so no false accusation risk.
- **Private-use detection** (semantic) — the false-positive guards are
  implemented (`workforce_private.rs`): the feature refuses to run without an
  operator-defined business-scope baseline (fail-closed), only flags
  high-confidence "suspected private" (never "undetermined"), honours an exempt
  list, and auto-expires unconfirmed flags after 30 days. Flags are advisory
  ("建議關注"), never employee-visible, and explicitly not grounds for discipline.
  The Haiku classification batch and the operator-only review UI sit on top of
  these guards.
