# Evolution switches — what each toggle controls

DuDuClaw agents can improve themselves over time: reflecting on prediction
errors, rewriting their own `SOUL.md`, synthesising new skills, and exploring
underused domains. Every one of those paths is opt-in and independently
switchable. This guide is the single map of which switch governs what, and how
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
| GVU self-play loop | No `SOUL.md` proposals, no observation windows opened |
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
| `gvu_enabled` | `true` | GVU generator→verifier→updater loop (SOUL.md rewrites) |
| `skill_synthesis_enabled` | `false` | Synthesising new skills from repeated domain gaps |
| `skill_graduation_enabled` | `false` | Promoting a proven skill to global scope |
| `skill_recommendation_enabled` | `false` | Auto-activating recommended skills for new agents |
| `curiosity_enabled` | `false` | Proactive exploration of underused domains |
| `skill_auto_activate` | `false` | Activating suggested skills mid-conversation |
| `skill_behavior_monitor_enabled` | `false` | Behavioural-drift detection after activation |

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
2. Watch `prediction.db` (`evolution_events` / `gvu_experiment_log`): no new GVU
   rows should appear.
3. `SOUL.md`'s SHA-256 fingerprint should not change.
4. No observation window should open (no pending version in the version store).

This mirrors the automated verification the project runs for this feature.
