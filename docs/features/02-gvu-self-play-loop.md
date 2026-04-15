# GVU² Self-Play Loop

> The agent writes, reviews, and refines its own personality — automatically, across two feedback loops.

---

## The Metaphor: A One-Person Writer's Room — With a Director's Cut

Imagine a screenwriter who has to write, edit, and approve their own script — but with a strict process:

1. **The Writer** drafts a new version of the script based on audience feedback
2. **The Editor** (same person, different hat) reviews the draft against a checklist: grammar, plot consistency, audience guidelines, and length
3. **The Showrunner** (same person, third hat) decides whether to air the new version or stick with the old one — and if they do air it, they watch the ratings for 24 hours before committing

If the ratings drop, the original version is immediately restored. No permanent damage.

Now imagine the writer also keeps a **notebook of past failures** — every plot hole, every bad dialogue choice, every scene that flopped. Before writing the next draft, they flip through the notebook to avoid repeating those mistakes.

And sometimes, instead of rewriting the whole script, they do a quick **scene fix** — adjusting just the part that didn't land, live, between episodes.

This is the GVU² (Generator-Verifier-Updater, Squared) dual-loop architecture.

---

## How It Works

### The Dual-Loop Architecture

GVU² operates two complementary feedback loops:

**Outer Loop (Behavioral GVU)** — Evolves the agent's personality file (SOUL.md). This is the strategic, long-term loop. It fires when the prediction engine detects significant behavioral drift, and produces a new version of the agent's core identity.

**Inner Loop (Task GVU)** — Handles instant task-level retries. When a specific task fails, the inner loop can retry with adjusted parameters *without* modifying the personality. This is the tactical, immediate loop.

```
Outer Loop (Behavioral)          Inner Loop (Task)
┌─────────────────────┐          ┌──────────────────┐
│ Evolve SOUL.md      │          │ Retry failed task │
│ (long-term growth)  │  ←───→  │ (instant fix)     │
│ 24h observation     │          │ No SOUL.md change │
│ MistakeNotebook     │          │ Max 3 retries     │
└─────────────────────┘          └──────────────────┘
```

Both loops share the **MistakeNotebook** — a persistent log of failure patterns that prevents the same errors from recurring across loops.

### The Three Roles (Outer Loop)

**Generator** — Creates a candidate revision of the agent's personality file. It doesn't work in a vacuum; it receives:
- Specific feedback from the verification layer about *what* to improve
- History of previous attempts (so it doesn't repeat failed approaches)
- The current version as a starting point

The Generator's output is always a complete, valid personality file — not a diff or patch.

**Verifier** — Evaluates the candidate through a 4+2 layer verification pipeline:

| Layer | What It Checks | How | Cost |
|-------|---------------|-----|------|
| **L1: Format** | Structure valid? Required sections present? Length within bounds? | Rule-based parsing | Zero |
| **L2: Metrics** | Does it respect the agent's behavioral contract? Any forbidden patterns? | String matching against CONTRACT.toml | Zero |
| **L2.5: MistakeRegression** | Does the candidate repeat any known failure patterns? | Compare against MistakeNotebook entries | Zero |
| **L3: LLM Judge** | Is it actually *better*? Does it address the feedback? Is the voice consistent? | LLM-as-judge evaluation | One LLM call |
| **L3.5: SandboxCanary** | Does it work correctly in a real conversation? | Execute test conversation in container sandbox | One LLM call |
| **L4: Safety** | Is anything *worse* than before? Did improvements break safety invariants? | Deterministic comparison metrics | Zero |

The order matters: cheap checks run first. If L1 fails (bad format), there's no point running L3 (expensive quality check). L2.5 and L3.5 are the new additions — they catch regression against *historical* failures and verify *real-world* behavior in a sandbox, respectively. Four of the six layers are zero-cost deterministic checks.

**Updater** — If all four layers pass, the Updater:
1. Writes the new version to a temporary file
2. Computes a cryptographic fingerprint of the content
3. Atomically replaces the old file (rename operation — no partial writes possible)
4. Records the version in history
5. Starts a 24-hour observation period

### The Feedback Loop

The critical innovation is *how* the Verifier communicates with the Generator. Instead of a score ("7 out of 10"), the Verifier provides **concrete, actionable feedback**:

```
Score-based (less useful):
  "Quality: 6/10. Needs improvement."

Feedback-based (what GVU actually does):
  "The greeting section is too formal for this agent's personality.
   Consider replacing 'I shall assist you' with something warmer
   like 'Happy to help!' Also, the error-handling paragraph
   contradicts the playful tone established in paragraph 2."
```

This is inspired by the TextGrad approach — treating text like a differentiable signal. The Generator can directly act on specific suggestions rather than guessing what "6/10" means.

### Adaptive Depth & Convergence Control

The loop doesn't have a fixed round limit — the **MetaCognition** module dynamically adjusts iteration depth based on the agent's history:

```
MetaCognition evaluates:
  - Historical GVU success rates
  - Current error severity
  - Budget remaining
     |
     v
Set iteration depth: 3-7 rounds
  - Low complexity + good history → 3 rounds
  - High complexity + weak history → up to 7 rounds
```

Within each run:
- **Round 1**: Addresses the primary feedback. Fixes the biggest issues.
- **Round 2**: Refines based on any new issues introduced in Round 1.
- **Round 3+**: Deeper refinement as needed.

The system detects **convergence** — if a round's output is nearly identical to the previous round's, it stops early. No point burning tokens on diminishing returns.

### Deferred GVU: Patient Evolution

Not every trigger needs immediate full evolution. The **Deferred GVU** mechanism accumulates gradient signals before committing to a cycle:

```
Significant error detected
     |
     v
Gradient buffer full enough?
  |
  +---> Yes → Fire GVU now
  |
  +---> No  → Accumulate and defer
              (max 3 deferrals across 72 hours)
              → 9-21 effective iterations
                 spread over days
```

This prevents over-reaction to isolated incidents and produces more stable evolution.

### Agent-as-Evaluator

For high-stakes evolution decisions, an independent **Evaluator Agent** (running on Haiku for cost control) performs adversarial verification:

```
GVU candidate passes L1-L4
     |
     v
Evaluator Agent (separate process):
  - Receives candidate SOUL.md
  - Runs structured evaluation
  - Returns JSON verdict: {accept, reject, revise}
  - Includes justification and specific concerns
     |
     v
Verdict informs Updater's final decision
```

This "second opinion" catches subtle quality issues that automated layers might miss.

---

## The 24-Hour Observation Period

Even after a candidate passes all four verification layers, it's not permanently adopted. The system enters an observation period:

```
New version deployed
       |
       v
  24 hours of monitoring
       |
       v
  Are performance metrics stable or improving?
       |
  +----+----+
  |         |
 Yes        No
  |         |
  v         v
Confirm   Rollback to
new       previous
version   version
```

What's being monitored:
- User satisfaction signals (explicit feedback, conversation length, re-engagement)
- Prediction engine accuracy (is the new personality harder to predict?)
- Error rates (is the agent making more mistakes?)

The rollback is **automatic and atomic** — the previous version's fingerprint is stored, and restoring it is a single file operation.

---

## Why This Matters

### Self-Improving Without Human Intervention

Traditional prompt engineering requires a human to read conversations, identify issues, rewrite the prompt, test it, and deploy it. GVU automates the entire cycle. The agent identifies its own weaknesses and fixes them.

### Safe Evolution

The 4-layer verification + 24-hour observation + automatic rollback means the system can evolve aggressively without risk. Bad changes are caught before they affect users (verification) or quickly reverted if they slip through (observation).

### Cost-Efficient Refinement

Of the 4+2 verification layers, 4 are zero-cost deterministic checks (L1, L2, L2.5, L4). The LLM is only called for quality judgment (L3) and sandbox testing (L3.5). A typical evolution cycle costs 2-4 LLM calls total, regardless of how many checks are performed.

---

## Interaction with Other Systems

- **Prediction Engine**: Decides *when* GVU fires. GVU decides *what* to change.
- **MistakeNotebook**: Shared across both loops — the outer loop records failures, the inner loop avoids repeating them.
- **CONTRACT.toml**: L2 verification ensures evolution never violates behavioral boundaries.
- **Security Layer**: SHA-256 fingerprinting ensures no unauthorized modifications happen outside the GVU pipeline.
- **MetaCognition**: Drives adaptive iteration depth based on historical performance.
- **Deferred GVU**: Accumulates gradient signals for patient, evidence-based evolution.
- **Agent-as-Evaluator**: Independent adversarial verification for high-stakes decisions.
- **Dashboard**: Evolution history is visible in the web interface, showing each version, its feedback, and its observation metrics.

---

## The Takeaway

GVU² turns prompt optimization from a manual, error-prone process into an automated, safe, and cost-efficient pipeline. The dual-loop architecture separates strategic growth (Behavioral GVU) from tactical fixes (Task GVU), while the MistakeNotebook ensures the agent never forgets its past failures. The agent doesn't just *run* — it *grows*, learns from mistakes, and evolves with patience.
