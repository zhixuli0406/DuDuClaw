# Skill Lifecycle Engine

> Seven stages from raw conversation to refined, reusable skill — fully automated.

---

## The Metaphor: An Apprentice Becoming a Master

A martial arts student learns through stages:

1. **Activation** — The master opens the training hall. The student shows up.
2. **Compression** — Hundreds of techniques are distilled down to the essentials. No wasted motion.
3. **Extraction** — After each sparring match, the student reviews what worked and what didn't.
4. **Reconstruction** — The student rebuilds techniques from scratch, understanding *why* each movement matters.
5. **Distillation** — Years of practice are compressed into a handful of principles that can be taught to others.
6. **Diagnostician** — The master examines the student's form and identifies subtle flaws.
7. **Gap Analysis** — "You're strong in defense but weak in counters. Let's work on that."

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

### Stage 2: Compression

Over time, an agent may accumulate overlapping or redundant skills. The compression stage identifies and merges them:

```
Skill inventory analysis
     |
     v
Find overlapping skills:
  - "complaint-handler-v1" and "complaint-handler-v2"
    share 80% of content
     |
     v
Merge into single unified skill
     |
     v
Remove duplicates, keep the best version
```

This prevents skill bloat — an agent that's been running for months doesn't end up with 50 near-identical skills.

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
Submit to Stage 6 (Diagnostician) for quality check
```

This is how agents learn from experience — successful strategies are automatically captured and formalized.

### Stage 4: Reconstruction

Sometimes a skill needs to be rebuilt from scratch rather than incrementally improved. Reconstruction reverse-engineers the *intent* behind a skill and generates a cleaner implementation:

```
Existing skill (messy, accumulated over time)
     |
     v
Analyze core intent:
  "What is this skill trying to achieve?"
     |
     v
Rebuild from principles:
  - Cleaner trigger conditions
  - More concise response pattern
  - Better error handling
     |
     v
Replace old skill with reconstructed version
```

This is the equivalent of rewriting code from scratch instead of patching — sometimes the accumulated technical debt makes it easier to start over.

### Stage 5: Distillation

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

### Stage 6: Diagnostician

Quality control for skills. The diagnostician examines each skill for:

```
Skill under review
     |
     v
Check:
  - Trigger accuracy: Does it fire at the right time?
  - Response quality: Does it produce good outcomes?
  - Consistency: Does it conflict with other skills?
  - Completeness: Are edge cases handled?
     |
     v
Report:
  ✓ Trigger accuracy: 94%
  ✗ Edge case: doesn't handle multi-language input
  ✓ No conflicts with existing skills
     |
     v
Recommend: Fix edge case or flag for reconstruction
```

### Stage 7: Gap Analysis

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
  "Agent handles complaints well (Stage 2 skill)
   but struggles with technical product questions.
   Recommendation: Extract skill from the 5 successful
   technical conversations last week."
```

Gap analysis closes the loop — it feeds back into Stage 3 (Extraction) by pointing out where new skills are needed.

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

Before installation, every skill goes through the Python-based **Skill Vetter**:

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

### MCP Tools

The marketplace is accessible through MCP:

| Tool | Purpose |
|------|---------|
| `skill_search` | Search GitHub for skills with weighted ranking |
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
- **CONTRACT.toml**: Skills must operate within contract boundaries. The diagnostician checks for conflicts.
- **Dashboard**: Skill marketplace, installed skills, and gap analysis reports are all visible in the web interface.

---

## The Takeaway

Skills are the bridge between raw experience and reliable capability. The 7-stage lifecycle ensures that every successful conversation contributes to the agent's permanent improvement — automatically extracted, quality-checked, and shared. The agent doesn't just handle today's conversations — it gets better at handling tomorrow's.
