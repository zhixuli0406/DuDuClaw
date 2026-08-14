# Belief Loop（信念迴圈）

> Give an agent a human-like belief cycle about the outside world: state a
> prediction, get scored against reality, see your own calibration next time.

## What It Is

The task forward model (v1.53/v1.54) predicts an agent's *own execution*
(which tools, how many calls, will the judge pass it). The Belief Loop
adds the missing outer layer: structured predictions about the **external
world** — any subject, not just a market — settled deterministically against
observed reality, with the agent's calibration record injected back into its
next decision prompt.

The loop (all hooks programmatic — the platform computes every
belief-vs-reality diff and injects it; the agent is never asked to *remember*
what it predicted, a design forced by the reflection-confabulation evidence
in arXiv:2605.29463):

```
belief_submit (MCP) ──▶ belief_log (prediction.db)
      ▲                        │
      │ calibration section    │ tick wake-ups carry a one-line
      │ in the next dispatch   │ "you said up 70% — it's down 1.2%" diff
      │ prompt (n<30 shows     ▼
      │ counts only)     belief_settle (MCP) ─▶ deterministic 3-way Brier
      └────────────────────────┘   (live-observation cross-check, ±1% tolerance)
```

## Using It

Agents get three MCP tools:

- `belief_submit` — subject (any external thing to forecast, e.g. a ticker
  `2317` or a KPI like `trial_conversion_rate`), horizon (a free-form label
  for when it settles, e.g. `今日收盤` or `本週五`, ≤40 chars), direction
  (`up`/`down`/`flat`), probability 0–1, a short rationale, and the reference
  value the direction is measured against.
- `belief_settle` — belief id + realized value. Settlement is deterministic:
  direction vs the reference value, a configurable flat band
  (`[belief] flat_band_pct`, default 0.3%), three-way Brier scoring.
  When the gateway has a live tick observation for the subject it
  cross-checks the reported value (1% tolerance, refuses on divergence — an
  agent cannot self-report reality). Tick fields map to subjects either by
  the platform's `zXXXX → XXXX` naming convention or an explicit
  `[belief] tick_subject_map` entry in `config.toml` (key = tick field name,
  value = subject) when a source doesn't follow that convention.
- `belief_stats` — the agent's own calibration record.

Dashboard: the Foresight page gains a **信念與驗證** tab — prediction list
(direction + confidence vs realized, hit/miss), Wilson-lower-bound hit rate,
mean Brier, overconfidence. Under 30 settled beliefs it shows counts only and
says so — never "seems to work".

## Two Examples

- **Investment** (the first validated use case): subject = a ticker, horizon
  = `今日收盤` (today's close), reference value = the price at submission,
  realized value = the closing price. Settlement cross-checks against a live
  tick feed when one is configured.
- **Business KPI**: subject = `trial_conversion_rate`, horizon = `本週五`
  (this Friday), reference value = this week's starting conversion rate, an
  agent submits a belief each Monday about which direction it will move and
  settles it against the actual CRM number at week's end. No tick feed
  required — settlement can be driven by a scheduled goal task instead.

## Honest Boundaries

- Injecting an agent's calibration history into its prompt is an
  **experiment, not a proven mechanism** — the 2026-08 literature sweep found
  no first-hand evidence either way, so every belief row records whether
  stats were injected (`stats_injected`), making the question answerable
  from your own data.
- Calibration is scored separately from task outcomes, and the two are known
  to diverge (arXiv:2607.03015) — a well-calibrated agent is not
  automatically a high-performing one, and the dashboard never conflates the
  two.
- Under 30 settled samples no number is presented as a conclusion
  (Wilson bounds and count-only displays throughout).
