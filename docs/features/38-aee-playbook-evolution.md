# Agentic Evolution Engine and playbook evolution

> SOUL.md becomes a read-only persona layer; what actually learns, accumulates, and can be retired rule by rule is the playbook.

---

## The one-sentence version

Previously, when an agent learned a lesson it would ask an LLM to rewrite its entire persona file `SOUL.md`. Starting with DuDuClaw 1.53, `SOUL.md` is **read-only** for agents (only you can change it, through the dashboard or the operator terminal). What actually learns, accumulates, and can be retired entry by entry is a new **behavioral rule list — the playbook**. This document explains what changed, how it affects you, and how to configure and observe it.

---

## Why the old mechanism was replaced

Three months of production data showed the old mechanism often "broke without anyone noticing":

- Once the persona file grew past its safety cap, every subsequent learning proposal was blocked by the same gate, and the system's only response was to write "needs manual review" into a log line nobody read — the agent was stuck, unable to learn anything new, until someone happened to notice.
- The observation-window rule was "wait until there are enough conversations", but the waiting logic had a hole: in the end it behaved as "if we waited long enough, count it as verified", with no real evidence behind it.
- The industry is heading the same way: Anthropic's official memory API recommends many small, focused files over a few large ones; Letta (the successor to MemGPT) goes further and never lets the main AI edit its own core memory. The persona file itself shows no sign of dying out — what is being retired is letting an LLM rewrite the whole thing, which also happens to be the part most exposed to prompt-injection attacks.

---

## What changed for you

1. **You edit SOUL.md; agents cannot edit their own.** To adjust an agent's personality, tone, or responsibility boundaries, use the dashboard ("agents → details → edit") or edit the file directly, same as before. The difference is that an agent will no longer quietly rewrite its own persona file in the middle of the night.
2. **The playbook (behavioral rules) is the new learning container, replacing whole-persona rewrites.** Each rule is small, has its own category (fix a mistake / refine an existing approach / explore a new one), records which situations should trigger it, links at least one verification test case, and accumulates helpful/harmful scores. Rules that perform poorly retire automatically — they never pile up into a giant document nobody dares to touch.
3. **Verification became fine-grained.** The old flow was: rewrite the whole persona file, observe for 24 hours, confirm or roll back the whole thing. Now each rule is verified on its own and kept or dropped on its own — when one rule performs poorly, only that rule is rolled back, without dragging down the other good lessons.
4. **The old mechanism was kept as an escape hatch, off by default.** If you have a specific reason to keep the old whole-file SOUL.md rewrite behavior, set `[evolution] legacy_soul_evolution = true` in that agent's `agent.toml`. Agents on that path give up the new mechanism's protections (layered verification, finer rollback, stagnation alerts).

---

## How it works

Every newly learned rule walks the same lifecycle; verification and keep-or-drop decisions happen per rule:

```
Learning proposal (one rule)
       |
       v
+---------------------------+
| Zero-LLM cheat screening  |  <-- copied eval-case text / always-true filler /
+-------------+-------------+      teaching failure-hiding -> blocked, reason logged
              |
              v
+---------------------------+
| Link an eval case         |  <-- every rule links at least one case,
+-------------+-------------+      with machine-checkable assertions
              |
              v
+---------------------------+
| Observation window        |  <-- aee_settle_hours (default 24 hours)
+-------------+-------------+
              |
              v
+---------------------------+
| Per-rule keep-or-drop     |  <-- helpful/harmful scores accumulate;
|                           |      a poor performer rolls back alone
|                           |      and retires automatically
+---------------------------+
```

---

## How to enable it

Evolution learning has always been opt-in; both switches must be on:

```toml
# agent.toml
[evolution]
enabled = true        # master switch
gvu_enabled = true    # the learning loop itself (default false; you must turn it on explicitly)
```

The default for `gvu_enabled` is now `false` (this release fixed a long-standing configuration contradiction — config files produced by the templates could say `= true` while the runtime often treated the value as `false`, and the two sides disagreed). If you relied on the old behavior where omitting the key auto-enabled the loop, turn it on explicitly after upgrading.

Other common settings:

```toml
[evolution]
gvu_cooldown_minutes = 60     # per-agent learning cooldown (minutes);
                              # prevents repeated learning bursts from burning resources
aee_settle_hours = 24         # how long a newly learned rule is observed before keep-or-drop
strategy = "balanced"         # whether each learning round leans toward fixing / refining / exploring
```

`strategy` takes four values:

| Value | When to use it |
|-------|----------------|
| `balanced` (default) | An even mix of fixing mistakes, refining, and exploring |
| `innovate` | Leans toward exploring new approaches; suits new agents still finding their footing |
| `harden` | Leans toward refining existing rules; suits agents already running stably that need polish |
| `repair_only` | Only fixes mistakes, never explores new rules; the most conservative |

---

## How to observe it

The dashboard's Memory page gained a "self-directed learning" (自主學習) tab, showing:

- **Evolution mode overview**: how many agents have learning enabled, and whether each runs the new mechanism or the legacy escape hatch
- **Version history**: a timeline of every learning event
- **Stagnation detection**: when an agent's learning gets rejected round after round, or it has gone a long time without learning anything new, a warning appears here (previously this situation was completely invisible)
- **Rejection statistics chart**: which gate blocks the most proposals, so you can tell whether the rules are too strict or the agent's proposal quality is poor
- **Learned-rule list**: the rules each agent currently holds. Each card renders in **plain language** ("when a task hits 'can't do this, capability missing', I will 'first check which tools I have'"), with a one-line "why this rule exists" underneath describing source and evidence (distilled from how many failures, guarded by how many eval cases, actually used how many times, and helpful in how many of those); the raw rule text shown to the model folds under "view raw rule content" and expands on demand. Status uses consistent plain-language labels: **under observation (not yet active) / on trial / active / unused for a long time, shelved / retired**. One click exports everything as JSON, or disables a rule you disagree with.

  The plain-language rendering is **pure template assembly with no model calls**, so reloading the list page costs nothing and adds no latency. When a fluent sentence cannot be assembled it never forces one — the card falls back to the raw text and is clearly marked "this rule cannot be auto-rendered in plain language yet".

---

## Checking rules from your chat app

You can ask without opening the dashboard:

```
/rules       # top 3 currently active rules (plain language)
/rules all   # everything (including under-observation and shelved; retired rules stay on the dashboard)
```

Also zero model cost: the command is intercepted and handled before it reaches the AI.

You can also ask "why did you do it that way?" in a reply: rules injected into the AI now carry a number and a one-line description, so the AI can point at the exact rule it followed ("because I learned: ...") instead of inventing a plausible-sounding reason after the fact.

Trial results for rules (adopted / rolled back / insufficient evidence) roll up into the learning-events section of the **daily digest**; they never push separate notifications at you.

---

## Advanced: exporting rules and running eval suites

To export an agent's current rules in one batch (for manual review, or to later copy experience across agents):

```bash
duduclaw playbook export --agent <agent id> --out rules.json
```

Each rule's fate is decided by its linked eval cases. To run a verification pass yourself, or to build and grow a suite:

```bash
duduclaw eval evals/<agent id>                        # run the full suite once
duduclaw eval evals/<agent id> --case foo,bar         # run only the named cases
duduclaw eval evals/<agent id> --exclude-dir held-out # skip held-out cases (so agents never see the answers)
```

Since 1.53, every newly learned rule must link at least one eval case and carry a few machine-checkable assertions (which tools must be used, which strings the output must or must not contain). If you have no suite yet, draft cases from the behavioral rules in SOUL.md:

```bash
duduclaw eval-scaffold --agent <agent id>    # draft cases into evals-drafts/
```

Drafts land in `evals-drafts/`; only after you review them and move them into `evals/` does the learning mechanism use them — unreviewed drafts never mix into the official suite. If a rule's assertions have no recorded transcript to replay against, the system honestly marks them "unverified" and downgrades them to advisory; it never pretends they were tested.

If an agent already wrote many behavioral rules into SOUL.md under the old mechanism, you can migrate them into the playbook (draft first, apply after your review):

```bash
duduclaw playbook migrate-soul --agent <agent id>          # step 1: generate a migration draft
duduclaw playbook migrate-soul --agent <agent id> --apply  # step 2: apply after review
```

---

## Anti-cheat screening (built in since 1.53)

Before a learning proposal enters verification, it passes a zero-LLM cheat check: a rule that copies large chunks of eval-case text (memorizing the answers), reads as an always-true platitude, or teaches the agent to hide failures without reporting them is blocked outright, with the reason recorded. Softer signals such as judge-pleasing phrasing only feed statistics for you to watch; they never veto on their own, to avoid killing legitimate rules.

---

## What is not done yet

This is a staged rebuild. The items below are planned and did not ship in this release; they will be announced separately:

- Turning each learning event into a falsifiable hypothesis, so the observation window waits for a concrete answer instead of vaguely watching statistics
- Periodic semantic deduplication of accumulated rules (today's dedup only catches near-verbatim repeats; it cannot yet see that five rules are really saying the same thing — an overlap that takes understanding to spot)

---

## Related documents

- Switch details: [`docs/guides/evolution-switches.md`](../guides/evolution-switches.md)
- Full eval guide: [`docs/guides/evals.md`](../guides/evals.md)
- Technical architecture: [`docs/architecture/evolution-engine.md`](../architecture/evolution-engine.md), chapter 12
