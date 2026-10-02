# Evolution switches — what each toggle controls

DuDuClaw agents can improve themselves over time: reflecting on prediction
errors, accumulating playbook rules (their `SOUL.md` is read-only to them),
synthesising new skills, and exploring underused domains. Each of those paths
has its own switch. This guide is the single map of which switch governs what, and how
to freeze an agent completely.

## The master switch

`agent.toml`:

```toml
[evolution]
enabled = true   # master kill-switch (default: true)
```

`enabled = false` makes **every autonomous evolution path on that agent inert**,
regardless of the individual toggles below. It is the one switch to flip when you
want an agent to stop changing itself. Defaults to `true` so agents created
before this field existed keep their previous behaviour.

Concretely, when `enabled = false`:

| Path | What stops |
|---|---|
| Evolution loop (AEE) | No playbook rounds run, no settlement windows opened |
| Heartbeat silence-breaker | Does **not** fire a forced reflection after silence |
| Channel prediction path | Skill diagnose/activate/synthesis/graduation and the GVU trigger are skipped |
| Sub-agent dispatch reflection | `maybe_run_gvu` short-circuits |
| Skill-synthesis auto-run scheduler | Skips a frozen target agent even when globally enabled |

Prediction-error **logging** still runs — that is passive observation
(telemetry), not self-modification, so your dashboards stay accurate.

## The per-feature toggles

Under the master switch, each capability has its own flag. With the master on,
`is_any_evolution_enabled()` is true as soon as at least one of these is on:

| Toggle | Default | Controls |
|---|---|---|
| `gvu_enabled` | **`true` from the factory** (see below) | The AEE playbook evolution loop |
| `strategy` | `"balanced"` | AEE round-intent mix: `balanced` / `innovate` / `harden` / `repair_only` |
| `skill_synthesis_enabled` | `false` | Synthesising new skills from repeated domain gaps |
| `skill_auto_activate` | `false` | Activating suggested skills mid-conversation |

**`gvu_enabled` ships `true` since 2026-09-29 (K2).** Two things changed to
make that safe. First, the engine no longer rewrites `SOUL.md` — the legacy
path was removed (S11), so what evolves is the playbook: small,
independently-retirable behaviour rules, each linked to at least one eval case,
each settled against that case after an observation window. `SOUL.md` stays
read-only for the agent. Second, the cost of a round is bounded on both sides:
`gvu_cooldown_minutes` throttles how often a round may start, and the
zero-LLM Gate rejects a doomed candidate *before* any judge call is paid for.

The value is still written explicitly by every scaffold, so the toggle is
visible in `agent.toml` rather than an absent key. An agent whose
`agent.toml` predates this change keeps whatever it already has (a missing key
still reads as `false` — the runtime gate is unchanged and fail-closed).
Set `gvu_enabled = false` to opt an agent out.

Four per-agent skill-lifecycle knobs became live on the same date (H3).
Before it they were written by the dashboard and read by nobody:

| Knob | Default | Controls |
|---|---|---|
| `max_active_skills` | `5` | Concurrent active skills before the weakest is evicted |
| `skill_synthesis_threshold` | `3` | Repeated domain-gap observations before a synthesis signal fires |
| `skill_synthesis_cooldown_hours` | `24` | Quiet period after a synthesis signal on the same topic |
| `skill_graduation_min_lift` | `0.1` | Minimum measured lift before a skill is a graduation candidate |

Eight sibling keys (`skill_graduation_enabled`, `skill_recommendation_enabled`,
`skill_recommendation_threshold`, `curiosity_enabled`, `curiosity_threshold`,
`curiosity_max_daily`, `skill_behavior_monitor_enabled`,
`skill_behavior_drift_threshold`) were removed instead — they had no reader at
all. Leaving one in an existing `agent.toml` is harmless; it is ignored, which
is exactly what it already was.

### GVU cooldown

Independent of the toggle above, every GVU trigger path (channel-reply
ε-exploration, silence-timer, sub-agent dispatch forced reflection) shares a
single per-agent cooldown so a burst of triggers can't chain multiple
multi-minute GVU cycles back to back:

```toml
[evolution]
gvu_cooldown_minutes = 60   # default 60; 0 disables the cooldown
```

The cooldown starts counting the moment a trigger is let through the gate
(not when the cycle finishes), and applies regardless of the outcome
(applied/abandoned/skipped) — the cost being throttled is
LLM calls *attempted*, not just calls that succeeded. State is in-memory and
resets on gateway restart.

### Fault attribution (`config.toml [evolution] fault_attribution`, default `true`)

Before a rejected goal-loop round is allowed to count against injected playbook
rules (`harmful` increments, shadow-candidate scoring, F2b consolidation
sources), the gateway classifies the round's fault side with a deterministic,
zero-LLM rule chain (`crates/duduclaw-gateway/src/fault_attribution.rs`):
`grader` (judge contradicted deterministic grounding evidence), `environment`
(rate limit / billing / timeout / spawn failure), `harness` (reply claims tool
use but no native tool event was observed, or a capability gate blocked the
call), `unknown` (no observation fidelity), else `model`. Only `model` rounds
feed the learning loop; every excluded round writes a `fault_attributed` audit
event so the exclusion is visible. Set `fault_attribution = false` to restore
the pre-2026-09 behaviour byte-for-byte. Basis: arXiv:2605.22842 (attribution
systems blamed the model in 64/64 failures) and arXiv:2607.28802.

### Which engine runs: AEE, and only AEE

When `gvu_enabled = true`, the engine that runs is **AEE** (the Agentic
Evolution Engine). AEE evolves the agent's *playbook* — small, independently
retirable behaviour rules, each linked to at least one eval case — and never
writes `SOUL.md`. The persona file is operator-owned.

**Removed 2026-09-29 (S11): the `legacy_soul_evolution` escape hatch and the
whole Generator→Verifier→Updater `SOUL.md` rewrite cycle behind it**, together
with `SOUL.md` versioning, the 24-hour observation window, automatic rollback,
the cap-deadlock consolidation breaker, and the `duduclaw evolution finalize`
CLI. `SOUL.md` had already been read-only for agents since Evolution v3
(WP1.1), so the write path those mechanisms guarded no longer existed. A
`legacy_soul_evolution = true` still sitting in an `agent.toml` is ignored —
the agent takes the AEE path like every other.

After a committed AEE round, the entries it added are observed before their
verdict is settled:

```toml
[evolution]
aee_settle_hours = 24   # default 24; the agent runs no new AEE round until it elapses
```

### Strategy mix (which intent AEE picks each round)

Each AEE round deterministically picks one intent — `repair` (consume
`MistakeNotebook` backlog), `optimize` (refine an existing low-`success_streak`
entry), or `innovate` (propose a new entry) — from a per-agent mix, replacing
the previous raw ε-exploration:

```toml
[evolution]
strategy = "balanced"   # balanced (default) | innovate | harden | repair_only
```

| `strategy` | Repair | Optimize | Innovate |
|---|---|---|---|
| `balanced` | 5 | 3 | 2 |
| `innovate` | 2 | 3 | 5 |
| `harden` | 4 | 5 | 1 |
| `repair_only` | 10 | 0 | 0 |

An unrecognised value `warn!`s and falls back to `balanced` — a typo must not
silently change evolution behaviour. `repair` is starved down to `optimize`
when the mistake backlog is empty, regardless of the configured mix.

The commit gate's per-dimension noise band (how close to the champion counts
as a tie, i.e. "matches" under matches-or-improves) is also configurable —
defaults are starting values pending empirical calibration, not tuned numbers:

```toml
[evolution.noise_band]
cases = 0.05     # eval-case pass-rate dimension; hard-clamped to ≤ 0.10
                 # (a wider band means the cases are noisy, not that the band should widen)
judge = 0.15     # LLM judge score dimension (judges vary run to run)
anti_sycophancy = 0.0   # deterministic — zero band
novelty = 0.05
relevance = 0.10
```

### Eval corpus location (AEE's measurement)

AEE scores candidates by replaying the agent's eval suite. The suite for an
agent is the directory named after it under the suites root:

```toml
# ~/.duduclaw/config.toml
[evolution]
eval_suites_root = "evals"        # default: <home>/evals; relative paths resolve against the home dir
# eval_binary    = "/usr/local/bin/duduclaw"   # optional: which binary to spawn for `duduclaw eval`
```

`DUDUCLAW_EVAL_SUITES_ROOT` overrides `eval_suites_root` for one process.
A developer checkout typically points it at the repo's `commercial/evals`.

**Record the corpus once before the scores mean anything.** AEE measures in
replay mode (offline, zero LLM cost), which reads a recorded transcript per
case. A case with no `<stem>.transcript.jsonl` beside it cannot be replayed:

```bash
duduclaw eval ~/.duduclaw/evals/<agent-id> --record   # one live pass, then replay is free
```

Until that pass exists, the whole suite is treated as *unmeasured* rather than
as failing — an unrecorded case is an infrastructure gap, not a quality signal,
and scoring it 0.0 would enshrine a champion of zeroes nothing could improve on.

**Measurement degrades gracefully without a suite.** An agent whose corpus is
unrecorded (or whose eval binary is unreachable) is still measured — the
`cases` dimension is reported as *absent*, never as zero, and the commit gate
compares the dimensions that do exist. The degradation is visible in the
round's audit record (`case_dimension_available: false`) and in a `warn!`
line, not silent.

**But new entries do require at least one eval case (v1.53, G6/E1).** Every
playbook `Add` must link ≥1 eval case and carry machine-checkable assertions
(`must_use_tools` / `output_contains` / …) — an agent with zero eval cases
cannot accumulate *new* rules. To bootstrap a corpus from an agent's SOUL
behaviour rules:

```bash
duduclaw eval-scaffold --agent <agent-id>   # drafts into evals-drafts/
```

Review the drafts, move the good ones into `evals/<agent-id>/`, then record
them as above. Drafts are deliberately written to a separate `evals-drafts/`
directory so unreviewed cases can never leak into the live corpus. Assertion
replay against a case with no recorded transcript reports *Unverified*
(advisory), never a silent pass.

Recording is side-effect-free since v1.53: `--record` rewrites the agent's
`.mcp.json` to a temporary copy whose `DUDUCLAW_HOME` points at the eval home
(and a placeholder MCP key), so a recording run can't touch production state
or leak real keys into transcripts.

## Autopilot is deliberately NOT governed by the master switch

Autopilot rules (`autopilot.*`) are **explicit user automation** — you wrote the
rule, so DuDuClaw treats it as an instruction, not as the agent evolving on its
own. The master evolution switch does not touch autopilot. If you want to stop a
specific autopilot rule, disable it in the dashboard's Autopilot page.

The one exception is the emergency freeze below, which is meant as a blunt "stop
everything" and reminds you to disable autopilot separately.

## One-shot freeze / unfreeze (enterprise escape hatch)

When something looks wrong and you want an agent to stop changing itself *now*:

```bash
duduclaw agent freeze <agent-id>
```

This sets both `[evolution] enabled = false` and `[heartbeat] enabled = false`
in one edit and writes a `security_audit.jsonl` record (`event_type =
agent_freeze`). Nothing is deleted; reverse it with:

```bash
duduclaw agent unfreeze <agent-id>
```

which restores `[evolution] enabled = true` and `[heartbeat] enabled = true`.
Autopilot rules are not auto-modified — the command prints a reminder to disable
those from the dashboard if needed.

## Verifying a freeze actually took effect

The point of the master switch is that you can prove nothing evolves after you
flip it. To check:

1. Set `[evolution] enabled = false` on the agent.
2. Watch `evolution.db` (`gvu_experiment_log`) and `prediction.db`
   (`evolution_events`): no new rows should appear.
3. `SOUL.md` should not change — though as of 2026-09-29 nothing in the
   platform can write it on an agent's behalf anyway.
4. No settlement window should open (no pending row in `aee_pending_settlement`).

This mirrors the automated verification the project runs for this feature.

## Related switches on other pages (v1.53)

Not evolution toggles, but part of the same learn-and-verify surface:

| Key | Default | Page |
|---|---|---|
| `config.toml [memory] novelty_gate` | `true` | [memory-and-knowledge.md](./memory-and-knowledge.md) — rejects near-duplicate semantic memories |
| `config.toml [dispatch] grounding_precheck_enabled` | `true` | [goal-loop.md](./goal-loop.md) — zero-LLM evidence check before the acceptance judge |
| `config.toml [dispatch] two_stage_judge` | `true` | [goal-loop.md](./goal-loop.md) — cheap first-stage evaluator before the MAV acceptance panel |
| `config.toml [goal_loop] resume_on_restart` | `"pause"` | [goal-loop.md](./goal-loop.md) — escalates in-flight goal tasks to `needs_human` on gateway restart; set `"auto"` to resume them instead. Dashboard: Settings → Automation |
| `config.toml [task_forward_model] enabled` | `true` (since v1.54) | [goal-loop.md](./goal-loop.md) — task-level predict-act-verify world model; set `false` to opt out |
| `config.toml [goal_loop] progress_report_minutes` | `10` | [goal-loop.md](./goal-loop.md) — notifies (never intervenes) when a claimed goal task has shown no progress signal for this many minutes; `0` disables |
| `config.toml [goal_loop] tool_streak_advisory` | `true` | [goal-loop.md](./goal-loop.md) — injects an escalating advisory hint after 3/5/8 identical consecutive tool calls in one round; zero LLM cost, never blocks |
| `config.toml [dispatch] admission` | `"queue"` | [goal-loop.md](./goal-loop.md) — over-capacity ephemeral sub-agent spawns durably FIFO-queue instead of failing immediately; set `"fail"` for the pre-H19 hard-reject behavior |
