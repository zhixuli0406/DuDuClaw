# Night engine — tidying up while the agent is idle

An AI employee that has been talking to people all day accumulates a messy memory: the
same fact restated five ways, a procedure that was learned in pieces across three
conversations, an open question nobody came back to. The night engine is the background
pass that cleans that up during idle windows, and optionally does a little pre-reading
for tomorrow.

It is **off by default, behind two separate switches**, and this page exists because
without it there was no way for an operator to find out how to turn it on.

---

## What it actually does

Four sub-passes, run in one "night pass" per agent:

| Pass | Name | LLM? | What it produces |
|---|---|---|---|
| N1 | Sleep-time compute | yes | Pre-reasoning over the agent's active context, cached for the next wake-up |
| N2 | Proactive prefetch | yes | Evidence gathered ahead of a need predicted from history + memory |
| N3 | Schema induction | no | Recurring patterns in episodic memory promoted to schema entries |
| N4 | Recurrence-gated consolidation | no | Semantically recurring knowledge consolidated, behind a coverage / preservation / faithfulness check that rolls back on failure |

N3 and N4 are deterministic and cost nothing. N1 and N2 call the agent's **utility**
model (haiku-class by default), and only those two are affected by the spend cap.

References: sleep-time compute (arXiv:2504.13171), ProAct (arXiv:2605.25971),
DCPM schema induction (arXiv:2606.09483), RecMem (arXiv:2605.16045) + TRUSTMEM
(arXiv:2606.25161).

---

## Turning it on

Two switches, both required. This is deliberate: the global one is the operator's
budget decision, the per-agent one is the "which employees does this apply to" decision.

**1. Global — `~/.duduclaw/config.toml`:**

```toml
[night]
# Allow the LLM-backed sub-passes (N1/N2) to make real model calls.
# Absent, false, or malformed ⇒ disabled. This knob can only ever turn it ON.
llm_enabled = true
```

With `llm_enabled = false` (the default) the scheduler still runs, but N1/N2 get no
model and no-op with a note. N3 and N4 are unaffected — they never needed a model.

**2. Per agent — `~/.duduclaw/agents/<id>/agent.toml`:**

```toml
[night_engine]
enabled = true                  # master switch for this agent; default false
idle_threshold_minutes = 90     # no user interaction for this long ⇒ idle
max_pass_cost_cents = 20        # hard cap per pass; N1/N2 stop when reached
max_passes_per_day = 8          # circuit breaker, rolling 24h, per agent

sleep_time = true               # N1
prefetch = true                 # N2
schema_induction = true         # N3 (deterministic)
recurrence_consolidation = true # N4 (deterministic)

schema_min_support = 3          # N3: occurrences before a pattern becomes a schema
recurrence_threshold = 3        # N4: recurrence count before consolidation fires
context_window = 40             # N1/N2: recent memories / turns per pass
```

This block is read **only** from the agent's own `agent.toml`. There is no global
`[night_engine]` fallback in `config.toml` — the registry loads each agent's config as
written and nothing overlays a global default onto it, so an agent with no
`[night_engine]` block gets the built-in defaults above (`enabled = false`). To turn the
engine on for several employees you set `enabled = true` in each of their `agent.toml`
files.

---

## Cost and safety

- **Spend cap per pass.** `max_pass_cost_cents` is checked before every N1/N2 call and
  the estimated cost is recorded after it. Once the cap is reached, the remaining
  LLM sub-passes are skipped; N3/N4 continue.
- **Daily circuit breaker.** `max_passes_per_day` bounds the number of passes per
  agent over a rolling 24 hours. It persists across restarts, so a crash loop cannot
  reset the counter. A corrupt breaker file starts fresh, fail-safe.
- **Idle only.** A pass fires only after `idle_threshold_minutes` with no user
  interaction on that agent. An agent that has never been active counts as idle.
- **Memory content is data.** Both night prompts wrap memory snippets in a `<data>`
  block with an explicit instruction never to follow instructions found inside —
  memory rows are channel-derived and therefore attacker-reachable.
- **Writes go through the normal gate.** N3/N4 consolidation writes use the shared
  memory-engine construction point, so `[memory] novelty_gate` applies here as it does
  on every other gateway-internal write path.

---

## Checking whether it ran

Night passes log from the `duduclaw_gateway::night_engine` target. A completed pass
logs `night pass complete`
with the sub-passes that ran; a skipped one says why (`night pass skipped: daily
circuit breaker open`, or nothing at all when the agent was not idle).

```bash
# systemd
journalctl -u duduclaw -f | grep night

# foreground
RUST_LOG=duduclaw_gateway::night_engine=debug duduclaw run
```

If you see nothing at all: check that the agent is genuinely idle, that both switches
are on, and that `max_passes_per_day` has not been exhausted for the day.

---

## Related

- [Memory and knowledge guide](../guides/memory-and-knowledge.md) — what memory holds
  and how it is retrieved
- [Evolution switches](../guides/evolution-switches.md) — the other background
  self-improvement loops and their kill switches
