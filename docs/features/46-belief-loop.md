# Belief loop

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
      │ prompt (verified       ▼
      │ settlements only) belief_settle (MCP) ─▶ deterministic 3-way Brier
      └────────────────────────┘   (recorded as a self-report today)
```

## Using It

Agents get three MCP tools:

- `belief_submit` — subject (any external thing to forecast, e.g. a ticker
  `2317` or a KPI like `trial_conversion_rate`), horizon (a free-form label
  for when it settles, e.g. `today's close` or `this Friday`, ≤40 chars), direction
  (`up`/`down`/`flat`), probability 0–1, a short rationale, and the reference
  value the direction is measured against.
- `belief_settle` — belief id + realized value. Settlement is deterministic:
  direction vs the reference value, a configurable flat band
  (`[belief] flat_band_pct`, default 0.3%), three-way Brier scoring.
  The realized value is the agent's own report. The settlement code can
  cross-check it against a platform-supplied price (1% tolerance, refusing on
  divergence) and only then records it as verified
  (`settle_source = "agent+tick_verified"`), but **nothing supplies that
  price today**: the `belief_settle` tool passes none (the MCP server process
  has no access to live tick data), and a gateway-side settle against live
  ticks is not implemented. Every settlement is therefore recorded as a
  self-report (`settle_source = "agent_unverified"`). The response carries
  `counts_toward_calibration` (`false` for a self-report) and, for a
  self-report, a `note` saying it was recorded but does not count.
- `belief_stats` — the agent's own record. Calibration figures sit under
  `verified` and use cross-checked settlements only; `self_reported` is a
  count plus a descriptive rate. The reply also carries a `note` explaining
  that split.

Only a settlement whose `settle_source` is exactly `agent+tick_verified`
counts toward the hit rate, the Wilson lower bound, the mean Brier score and
overconfidence. Self-reports, unknown or empty values and older rows with no
`settle_source` are counted separately and never feed a calibration figure.
Because no verified path exists yet, an existing deployment reads "no
verified settlements" for every agent.

Dashboard: the Foresight page gains a **Beliefs & Verification** tab — prediction list
(direction + confidence vs realized, hit/miss, and a **Verified** or
**Self-reported (unverified)** label on each settled belief), Wilson-lower-bound
hit rate, mean Brier, overconfidence, all from verified settlements only.
Self-reported settlements are listed as a separate count that is not included
in those numbers. With no verified settlement it says accuracy cannot be
assessed yet; under 30 verified settlements it shows counts only and says so —
never "seems to work".

### `belief.summary` / `belief_stats` response (breaking change)

The dashboard RPC `belief.summary` returns `{ "stats": … }` and the MCP tool
`belief_stats` returns the same object (plus `note`). The flat shape is gone:
the top-level `n_total`, `n_settled`, `insufficient_samples`, `hit_rate`,
`hit_rate_wilson_low`, `mean_brier`, `overconfidence` and the per-subject
`n_settled` / `hits` / `mean_brier` were removed. Readers written against the
old shape need updating.

| Field | Meaning |
|---|---|
| `agent_id` | The agent |
| `n_submitted` | Every belief submitted, settled or not |
| `n_settled_all` | Every settled belief (`verified.n + self_reported.n`) |
| `calibration_status` | `no_verified_settlements` (0 verified), `insufficient_samples` (1–29 verified), `calibrated` (30 or more verified) |
| `verified.n`, `verified.hits` | Count of verified settlements and their hits |
| `verified.hit_rate`, `verified.hit_rate_wilson_low`, `verified.mean_brier`, `verified.overconfidence` | `null` unless `calibration_status` is `calibrated` |
| `self_reported.n` | Count of self-reported settlements (including unknown and legacy rows) |
| `self_reported.hit_rate` | Share the agent reported as hits; descriptive only, never a calibration result; `null` when `n` is 0 |
| `per_subject[]` | `{ subject, verified: { n, hits, mean_brier }, self_reported: { n } }` |

## Two Examples

- **Investment** (the first validated use case): subject = a ticker, horizon
  = `today's close`, reference value = the price at submission,
  realized value = the closing price. With a live tick feed configured, tick
  wake-ups show the belief next to the live value; tick fields map to subjects
  by the platform's `zXXXX → XXXX` naming convention or an explicit
  `[belief] tick_subject_map` entry in `config.toml` (key = tick field name,
  value = subject). The settlement itself is still a self-report: the feed is
  not used to verify it.
- **Business KPI**: subject = `trial_conversion_rate`, horizon = `this Friday`,
  reference value = this week's starting conversion rate, an
  agent submits a belief each Monday about which direction it will move and
  settles it against the actual CRM number at week's end. No tick feed
  required — settlement can be driven by a scheduled goal task instead.

## Honest Boundaries

- The calibration section of the dispatch prompt is injected only when the
  agent has at least one verified settlement; with none (every deployment
  today) nothing is injected. When it is injected, self-reported settlements
  are named as a separate count that is not part of any figure.
- Injecting an agent's calibration history into its prompt is an
  **experiment, not a proven mechanism** — the 2026-08 literature sweep found
  no first-hand evidence either way, so every belief row records whether
  stats were injected (`stats_injected`), making the question answerable
  from your own data.
- Calibration is scored separately from task outcomes, and the two are known
  to diverge (arXiv:2607.03015) — a well-calibrated agent is not
  automatically a high-performing one, and the dashboard never conflates the
  two.
- Under 30 verified settlements no number is presented as a conclusion
  (Wilson bounds and count-only displays throughout). Self-reported
  settlements never count toward that threshold.
- No verified settlement path exists yet (gateway-side settlement against
  live ticks is not implemented), so calibration is currently empty
  everywhere.
