# SOUL.md Versioning & Rollback

> Atomic personality updates with automatic rollback — a safety net for agent evolution.

---

## The Metaphor: Canary Releases for Software

In software deployment, a "canary release" works like this: deploy the new version to 5% of users, watch the metrics for 24 hours, and only roll it out to everyone if things look good. If the canary dies (metrics tank), roll back instantly.

DuDuClaw applies the same principle to agent personality updates. The personality file (SOUL.md) is the agent's "deployed software" — and every update goes through a canary process.

---

## How It Works

### The Update Process

When the GVU loop produces a new version of the personality file, it doesn't simply overwrite the old one. Instead, it follows a multi-step atomic write process:

```
GVU produces candidate personality file
     |
     v
Step 1: Write to temporary file
  (A separate file, not the live one)
     |
     v
Step 2: Compute cryptographic fingerprint
  (A unique hash of the content — if even one character
   changes, the hash is completely different)
     |
     v
Step 3: Atomic rename
  (Replace the live file with the temporary file in
   a single filesystem operation — no partial states)
     |
     v
Step 4: Record version metadata
  (Timestamp, fingerprint, reason for change,
   link to the feedback that triggered it)
     |
     v
Step 5: Start 24-hour observation
  (Monitor performance metrics under the new version)
```

### Why Atomic Writes Matter

Consider what happens without atomic writes:

```
Dangerous approach:
  1. Open the live file
  2. Clear its contents
  3. Write new contents
  4. Close the file

If the system crashes between steps 2 and 3,
the personality file is EMPTY.
The agent has no personality. Conversations fail.
```

With atomic writes (temp file + rename):

```
Safe approach:
  1. Write complete new content to temp file
  2. Rename temp file to live file path (single operation)

If the system crashes during step 1:
  → Temp file is incomplete, but live file is untouched.
If the system crashes during step 2:
  → Rename is atomic — either it happened or it didn't.
     The live file is either fully old or fully new. Never partial.
```

### The Observation Period

After the new version is live, the system monitors a set of indicators:

```
24-hour observation window
     |
     v
  Monitoring:
  +--------------------------------------------+
  | - User satisfaction signals                 |
  |   (Are users responding positively?)        |
  |                                             |
  | - Prediction engine accuracy                |
  |   (Is the agent's behavior predictable?)    |
  |                                             |
  | - Error rates                               |
  |   (Is the agent making mistakes?)           |
  |                                             |
  | - Conversation completion rates             |
  |   (Are users dropping off mid-conversation?)|
  +--------------------------------------------+
     |
     v
  +------+------+
  |             |
Stable/       Degraded
Improved      |
  |           v
  v         Automatic rollback:
Confirm     Restore previous version
new         using stored fingerprint
version
```

The rollback is equally atomic — the system keeps the previous version's content indexed by its fingerprint, and restoring it is the same temp-file-then-rename process.

### Drift Detection

Even outside the evolution cycle, the system periodically checks the personality file's integrity:

```
Periodic integrity check (every N minutes)
     |
     v
Compute current fingerprint
     |
     v
Compare with expected fingerprint
     |
  +--+--+
  |     |
Match   Mismatch
  |     |
  v     v
OK    ALERT:
      "Personality file was modified
       outside the evolution pipeline!"
```

This catches scenarios like:
- A human manually edited the file and introduced errors
- Another program accidentally overwrote it
- A malicious actor attempted to inject content

The alert is logged and surfaced in the dashboard. Depending on configuration, the system can automatically restore the expected version.

---

## Version History

Every version of the personality file is recorded with metadata:

```
Version History:
  v12 (current) - 2026-04-07T10:30:00Z
    Fingerprint: a3f8c1...
    Reason: "Improved greeting warmth based on user feedback"
    Status: Observing (18h remaining)

  v11 - 2026-04-06T14:00:00Z
    Fingerprint: b7d2e9...
    Reason: "Added error recovery language"
    Status: Confirmed (observation passed)

  v10 - 2026-04-05T09:15:00Z
    Fingerprint: c1a4f6...
    Reason: "Reduced formality in technical explanations"
    Status: Confirmed
```

This history serves multiple purposes:
- **Debugging**: If the agent's behavior changes, operators can trace it to a specific version
- **Rollback targets**: Any previous version can be restored, not just the immediately previous one
- **Evolution auditing**: Shows how the agent's personality has evolved over time

---

## Why This Matters

### Zero-Downtime Evolution

The agent never goes offline during a personality update. The atomic rename ensures a seamless transition — one moment the agent uses the old personality, the next moment it uses the new one. No restart, no gap, no partial state.

### Reversible by Default

Every change can be undone. This makes evolution *safe to try* — the cost of a bad change is 24 hours of slightly degraded performance, followed by an automatic correction. Not a permanent mistake.

### Tamper-Evident

The fingerprint system means any unauthorized modification is detectable. Combined with the security hooks that protect the file from direct access, the personality file has the same integrity guarantees as a signed software release.

### Compliance-Friendly

For regulated industries, the version history provides an auditable trail of every change to the agent's behavior, when it happened, why, and what the impact was.

---

## Interaction with Other Systems

- **GVU Loop**: Produces the candidate versions that this system manages
- **Security Hooks**: Protect the personality file from unauthorized access
- **Prediction Engine**: Accuracy metrics feed into the observation period's health assessment
- **Dashboard**: Version history and observation status are visible in the web interface
- **CONTRACT.toml**: Behavioral contracts define what the personality file *can't* become

---

## The Takeaway

Agent evolution is powerful but risky. SOUL.md versioning provides the safety infrastructure that makes evolution practical: atomic updates prevent corruption, observation periods catch regressions, automatic rollback limits blast radius, and drift detection catches unauthorized changes. Evolution without a safety net is reckless; evolution with this system is methodical.
