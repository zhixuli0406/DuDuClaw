# TODO — One missing agent.toml field makes the whole agent disappear

**Owner:** — &nbsp; **Status:** confirmed, not started
**Last updated:** 2026-08-16 &nbsp; **Severity:** 🟠 HIGH (looks like data loss to the operator)

## Problem

`AgentConfig` deserializes with no serde defaults, so a single missing field
aborts the parse for the entire file. The registry logs one WARN and moves on.
Every downstream surface then behaves as if the agent was never created:

- `agents.list` omits it.
- `FirstRunGate` sees zero agents and redirects to `/welcome`, presenting a
  working install as a brand-new one.
- Scheduled tasks that name the agent have no one to dispatch to.

Nothing on screen mentions a config problem. The operator sees "my agent is
gone".

## Evidence

Both halves observed on 2026-08-16.

**A newly seeded agent, three restarts in a row.** Each restart revealed only
the next missing field — there is no "here is everything that is wrong" pass:

```
missing field `display_name`     → fix → restart
missing field `fallback`         → fix → restart
```

**A whole team of six workers vanishing at once.** They were created through a
team-template path that wrote files the registry could not read:

```
ERROR failed to parse agent.toml path=…/agents/duduclaw-python/agent.toml
      missing field `trigger`
WARN  failed to load agent, skipping dir=duduclaw-python
…同 frontend / llm / qa / rust / devops
INFO  agent registry scan complete count=0 agents=[]
```

The second case is the more serious one: **a creation path shipped by the
product wrote files its own reader rejects.** Creator and reader disagree on
what is mandatory.

## Why it hurts more than it looks

- The failure is silent by default (one WARN in a log the operator is not
  watching) and the visible symptom points somewhere else entirely.
- Recovery is iterative. Fields surface one restart at a time, so a file
  missing four fields costs four rebuild-restart cycles.
- The `/welcome` redirect actively misleads: it invites the operator to run
  first-run onboarding on an install that already has agents and data.

## Proposed fix

**1. Degrade instead of dropping (primary).**
Give the optional-in-practice fields `#[serde(default)]` and load the agent with
defaults, attaching a visible "configuration incomplete" flag. An agent with a
missing `icon` should not cease to exist. Keep hard-failing only on the fields
without a sane default (`name`, and whatever genuinely cannot be inferred).

**2. Report every problem at once.**
Collect all missing/mistyped fields in one pass and log them together, so one
fix-restart cycle is enough.

**3. Surface it in the UI.**
`agents.list` should report load failures alongside the successes, so the
dashboard can show "3 agents failed to load — see details" rather than nothing.
This also stops `FirstRunGate` mistaking a broken install for a fresh one:
gate on "zero agents **and** zero load failures".

**4. Make the writers and the reader agree.**
Whatever wrote the six-worker team omitted `trigger`. Audit the creation paths
(`agent create`, MCP `create_agent`, team-template install, dashboard) against
the struct — ideally by having them serialize the same type rather than
hand-assembling TOML.

## Acceptance

- An agent.toml missing a defaultable field still loads, flagged as incomplete.
- A file missing several fields lists all of them in one log line.
- The dashboard shows a load-failure count instead of silently showing nothing.
- A team installed through the template path loads on the first try.

## Related code

- `crates/duduclaw-agent/src/registry.rs` — the scan and the skip
- `crates/duduclaw-agent/src/…` — `AgentConfig` definition
- `web/src/components/FirstRunGate.tsx` — the zero-agent redirect
- `templates/*/agent.toml` — the reference files that do parse
