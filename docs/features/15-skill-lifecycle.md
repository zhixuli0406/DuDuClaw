# Skill Lifecycle Engine

> Six stages from raw conversation to refined, reusable skill.

---

## The Metaphor: An Apprentice Becoming a Master

A martial arts student learns through stages:

1. **Activation** — The master opens the training hall. The student shows up.
2. **Compression** — A technique is written three ways: its name, a one-line reminder, the full form. You recall the shortest version that still works.
3. **Extraction** — After each sparring match, the student reviews what worked and what didn't.
4. **Distillation** — Years of practice are compressed into a handful of principles that can be taught to others.
5. **Diagnosis** — The master watches a *failure* and names its cause: was that a stance problem, or did you simply never learn this counter?
6. **Gap Analysis** — "You're strong in defense but weak in counters. Let's work on that."

DuDuClaw's skill lifecycle mirrors this progression — turning raw conversational experience into structured, reusable, shareable skills.

---

## How It Works

### Stage 1: Activation

When an agent starts, the skill loader reads its `SKILLS/` directory and activates installed skills:

```
Agent startup
     |
     v
Scan SKILLS/ directory
     |
     v
For each skill file:
  - Parse skill definition
  - Validate format and dependencies
  - Register with SkillRegistry
     |
     v
Skills are now available for the agent's runtime
```

Skills are structured markdown files with metadata:

```markdown
---
name: customer-complaint-handler
version: 1.2.0
triggers: [complaint, refund, dissatisfied]
---

## When to Use
When a customer expresses dissatisfaction...

## Response Pattern
1. Acknowledge the issue
2. Apologize sincerely
3. Offer concrete resolution
...
```

### Stage 2: Compression (three-layer progressive loading)

Compression is about *injection cost*, not deduplication. Every skill is stored
three ways, and the prompt builder injects the cheapest layer that still does
the job:

```
skill file (SKILLS/complaint-handling.md)
     |
     v
Layer 0 — the name tag           ~5 tokens
Layer 1 — a 1-2 line summary     ~30 tokens
Layer 2 — the full markdown      ~200+ tokens
     |
     v
Relevance decides which layer reaches the prompt
```

A skill the turn probably doesn't need costs 5 tokens to keep visible instead of
200. That is what keeps a well-skilled agent from paying for its whole library
on every turn.

### Stage 3: Extraction

After successful conversations, the system can automatically extract patterns that could become new skills:

```
Conversation completed successfully
     |
     v
Analyze conversation pattern:
  - Was this a novel approach?
  - Did the user express satisfaction?
  - Is this pattern repeatable?
     |
  +--+--+
  |     |
 Yes    No → No skill extracted
  |
  v
Generate candidate skill:
  - Identify the trigger conditions
  - Extract the response pattern
  - Define the success criteria
     |
     v
Run through the security scanner + vetting gate before it is written
```

This is how agents learn from experience — successful strategies are automatically captured and formalized.

> **Removed in 2026-09: "Stage 4 — Reconstruction."** Earlier versions of this
> page described a stage that reverse-engineered a messy skill's intent and
> rebuilt it from principles. The module that was supposed to do it
> (`skill_lifecycle/reconstruction.rs`) never had a caller — not even a test —
> so nothing ever reconstructed anything. The code was deleted rather than left
> as a promise. If a skill goes bad today, the honest answer is: retire it and
> let extraction produce a new one. Two sibling modules went with it for the
> same reason (`recommender.rs`, `dependency_resolver.rs` — both zero call
> sites), which is why the skill list has no "recommended for you" surface and
> skills have no declared dependencies between each other.

### Stage 4: Distillation

Distillation compresses a skill to its essential rules — the minimum viable knowledge needed to apply it effectively:

```
Full skill (500 lines, detailed examples)
     |
     v
Identify essential rules:
  - Core principles (5-10 rules)
  - Critical constraints
  - Key decision points
     |
     v
Distilled skill (50 lines, pure principles)
```

Distilled skills are faster to load, consume less context window, and are easier to share across agents.

### Stage 5: Diagnosis

The diagnostician does not grade skills. It reads a **prediction error** and
names its cause — zero LLM cost, pure rules over the `PredictionError` signal:

```
PredictionError (Significant / Critical)
     |
     v
Classify the cause:
  - StyleMismatch      — tone/length didn't match the user
  - DomainGap          — the agent lacks knowledge on this topic
  - PrecisionIssue     — answer wasn't accurate enough
  - ExpectationMismatch— user wanted different behaviour
  - Unknown            — cannot tell
     |
     v
DomainGap with no matching installed skill
     |
     v
emit a SkillGap { suggested_name, suggested_description, evidence }
```

Only a `DomainGap` that no installed skill covers becomes a `SkillGap`. That
record is what Stage 6 accumulates.

### Stage 6: Gap Analysis

The final stage looks at the agent's overall skill portfolio and identifies what's missing:

```
Analyze conversation history
     |
     v
Identify patterns where the agent struggled:
  - Topics with low satisfaction scores
  - Queries that required fallback to cloud API
  - Conversations that were abandoned
     |
     v
Gap report:
  "Agent handles complaints well (has an installed skill)
   but struggles with technical product questions.
   Recommendation: Extract skill from the 5 successful
   technical conversations last week."
```

Gap analysis closes the loop — it feeds back into Stage 3 (Extraction) by pointing out where new skills are needed. A repeated gap in the same domain is what triggers skill synthesis.

---

## The Skill Marketplace

Beyond self-generated skills, agents can discover and install skills from the community:

### GitHub Live Indexing

The marketplace uses GitHub's Search API to find skill repositories in real-time:

```
Search query: "customer-support skill duduclaw"
     |
     v
GitHub Search API
     |
     v
Results (cached 24 hours):
  1. zhixuli0406/skill-customer-support (★ 45)
  2. community/duduclaw-skills-pack (★ 120)
     |
     v
Weighted ranking:
  - Stars
  - Recent activity
  - Skill format validity
  - Security scan results
```

### Security Scanning

Before installation, every skill goes through the Rust-native **Skill security scanner** (`skill_lifecycle::security_scanner`):

```
Candidate skill from marketplace
     |
     v
Security scan:
  - Prompt injection patterns?
  - Attempts to modify system files?
  - References to external URLs?
  - Obfuscated content?
     |
  +--+--+
  |     |
Clean   Flagged
  |     |
  v     v
Install  Warning + manual review required
```

### Choosing where to search

There are three places a skill can come from — GitHub, the curated skill hubs, and this deployment's own learned skill bank — and one tool searches all of them. `skill_search` takes a `source` parameter:

| `source` | Searches | Use when |
|---|---|---|
| `all` (default) | Hubs **and** the learned skill bank, de-duplicated by skill name | You don't already know where the skill lives. **This is the rule: leave `source` alone unless you do.** |
| `github` | The GitHub hub only | You expect the skill in a public GitHub repo |
| `hub` | The curated registries (anthropic-skills / clawhub / lobehub / skills-sh) | You want vetted, ranked results only |
| `bank` | The learned skill bank | You are looking for something this deployment taught itself |

The learned skill bank is currently an empty in-memory stub, so `source="bank"` reports an empty store rather than quietly returning hub results. `skill_bank_search` was a deprecated alias for `source="bank"` and was removed in v1.69.0; call `skill_search` with `source="bank"` instead — see [deprecations](../guides/deprecations.md).

### MCP Tools

The marketplace is accessible through MCP:

| Tool | Purpose |
|------|---------|
| `skill_search` | Search every skill source (hubs + learned bank) with weighted ranking; `source` narrows it |
| `skill_list` | List installed skills per agent |

---

## Why This Matters

### Compound Learning

Each conversation teaches the agent something. The skill lifecycle captures that learning and formalizes it. Over time, the agent becomes genuinely more capable — not because its model improved, but because its skill library grew.

### Knowledge Transfer

Skills can be shared between agents. A skill extracted from the support agent can be installed on the sales agent. This is organizational knowledge management, automated.

### Quality Control

The diagnostician and gap analysis stages ensure skills don't just accumulate — they *improve*. Low-quality skills are flagged for reconstruction. Missing capabilities are identified proactively.

### Community

The marketplace means you don't start from zero. Someone else's battle-tested customer support skill can bootstrap your agent in minutes.

---

## Interaction with Other Systems

- **Evolution Engine**: Skills inform the prediction engine. A well-skilled agent has fewer prediction errors.
- **GVU Loop**: Skill extraction is often triggered after a successful GVU cycle — the improvement in personality reveals new patterns worth capturing.
- **Memory System**: Skills complement memory — memory remembers *what happened*, skills remember *what to do*.
- **CONTRACT.toml**: Skills must operate within contract boundaries; the security scanner + vetting gate run on the markdown that is actually written to disk.
- **Dashboard**: Skill marketplace, installed skills, and gap analysis reports are all visible in the web interface.

---

## The Takeaway

Skills are the bridge between raw experience and reliable capability. The 6-stage lifecycle turns a successful conversation into a reusable skill and a repeated failure into a named gap — extracted, scanned, and shareable. The agent doesn't just handle today's conversations — it gets better at handling tomorrow's.
