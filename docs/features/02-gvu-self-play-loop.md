# GVU Self-Play Loop

> The agent writes, reviews, and refines its own personality — automatically.

---

## The Metaphor: A One-Person Writer's Room

Imagine a screenwriter who has to write, edit, and approve their own script — but with a strict process:

1. **The Writer** drafts a new version of the script based on audience feedback
2. **The Editor** (same person, different hat) reviews the draft against a checklist: grammar, plot consistency, audience guidelines, and length
3. **The Showrunner** (same person, third hat) decides whether to air the new version or stick with the old one — and if they do air it, they watch the ratings for 24 hours before committing

If the ratings drop, the original version is immediately restored. No permanent damage.

This is exactly how the GVU (Generator-Verifier-Updater) loop evolves an agent's personality file.

---

## How It Works

### The Three Roles

**Generator** — Creates a candidate revision of the agent's personality file. It doesn't work in a vacuum; it receives:
- Specific feedback from the verification layer about *what* to improve
- History of previous attempts (so it doesn't repeat failed approaches)
- The current version as a starting point

The Generator's output is always a complete, valid personality file — not a diff or patch.

**Verifier** — Evaluates the candidate through four layers of checks:

| Layer | What It Checks | How | Cost |
|-------|---------------|-----|------|
| **L1: Format** | Structure valid? Required sections present? Length within bounds? | Rule-based parsing | Zero |
| **L2: Compliance** | Does it respect the agent's behavioral contract? Any forbidden patterns? | String matching against CONTRACT.toml | Zero |
| **L3: Quality** | Is it actually *better*? Does it address the feedback? Is the voice consistent? | LLM-as-judge evaluation | One LLM call |
| **L4: Regression** | Is anything *worse* than before? Did improvements in one area break another? | Deterministic comparison metrics | Zero |

The order matters: cheap checks run first. If L1 fails (bad format), there's no point running L3 (expensive quality check). This alone saves significant cost.

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

### Convergence Control

The loop runs at most **3 rounds**. Here's why:

- **Round 1**: Addresses the primary feedback. Fixes the biggest issues.
- **Round 2**: Refines based on any new issues introduced in Round 1.
- **Round 3**: Final polish. If it still doesn't pass after 3 rounds, the system keeps the current version.

The system also detects **convergence** — if Round 2's output is nearly identical to Round 1's, it stops early. No point burning tokens on diminishing returns.

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

Of the 4 verification layers, 3 are zero-cost deterministic checks. The LLM is only called for quality judgment (L3) and generation. A typical evolution cycle costs 2-3 LLM calls total, regardless of how many checks are performed.

---

## Interaction with Other Systems

- **Prediction Engine**: Decides *when* GVU fires. GVU decides *what* to change.
- **CONTRACT.toml**: L2 verification ensures evolution never violates behavioral boundaries.
- **Security Layer**: SHA-256 fingerprinting ensures no unauthorized modifications happen outside the GVU pipeline.
- **Dashboard**: Evolution history is visible in the web interface, showing each version, its feedback, and its observation metrics.

---

## The Takeaway

GVU turns prompt optimization from a manual, error-prone process into an automated, safe, and cost-efficient pipeline. The agent doesn't just *run* — it *grows*.
