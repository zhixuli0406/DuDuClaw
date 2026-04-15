# Three-Phase Security Defense

> Layered threat filtering — 90% of attacks stopped at zero cost.

---

## The Metaphor: Airport Security Screening

When you go through airport security, not everyone gets the same treatment:

1. **The metal detector** — everyone walks through. It catches obvious threats instantly. Zero human effort.
2. **The X-ray machine** — your bags are scanned. An operator glances at the screen. Only flagged bags get a second look.
3. **The private screening room** — only for passengers who triggered multiple alerts. A thorough manual inspection.

DuDuClaw's security defense works exactly like this: three layers of progressively more expensive checks, where the vast majority of threats are caught before reaching the expensive layers.

---

## How It Works

### Layer 1: Deterministic Blacklist

The first line of defense is a simple, fast pattern matcher. It blocks known-dangerous operations instantly:

```
Incoming tool call or command
     |
     v
Match against blacklist patterns:
  - Destructive shell commands
  - Direct access to sensitive files
  - Known injection patterns
     |
  +--+--+
  |     |
Match   No match
  |     |
  v     v
BLOCK   Pass to Layer 2
```

This layer runs in microseconds. No network calls, no model invocations, no ambiguity. If the pattern matches, it's blocked. Period.

The blacklist covers the most common attack vectors: commands that delete data, commands that exfiltrate environment variables, and patterns that attempt to bypass file permissions.

### Layer 2: Obfuscation & Exfiltration Detection

Attackers who know about Layer 1 will try to work around it — encoding commands in Base64, splitting dangerous strings across multiple operations, or gradually building up a payload.

Layer 2 activates when the threat level is elevated (YELLOW or higher):

```
Tool call passed Layer 1
     |
     v
Current threat level >= YELLOW?
     |
  +--+--+
  |     |
 Yes    No --> Pass through (Layer 2 skipped)
  |
  v
Scan for obfuscation patterns:
  - Base64-encoded command fragments
  - Environment variable references in unusual contexts
  - URLs that match known exfiltration endpoints
  - Encoded characters that reassemble into dangerous commands
     |
  +--+--+
  |     |
Found   Clean
  |     |
  v     v
BLOCK   Pass to Layer 3 (if RED)
```

This layer is still rule-based — no LLM call — but the rules are more sophisticated. It looks for *intent to circumvent* rather than direct dangerous commands.

### Layer 3: AI Judgment

The most expensive layer. Only activated at threat level RED (confirmed attack behavior):

```
Tool call passed Layers 1 and 2
     |
     v
Current threat level == RED?
     |
  +--+--+
  |     |
 Yes    No --> Pass through
  |
  v
Send context to lightweight LLM:
  "Given this sequence of tool calls and their context,
   is this a legitimate operation or an attack attempt?"
     |
  +--+--+
  |     |
Attack  Legitimate
  |     |
  v     v
BLOCK   Allow
```

This layer catches attacks that are semantically dangerous but syntactically innocent — things that look normal individually but form a malicious pattern when considered together.

### The Threat Level State Machine

The three layers are orchestrated by a threat level system:

```
GREEN (Normal)
  |
  | Suspicious pattern detected
  v
YELLOW (Elevated)
  |
  | Confirmed attack indicators
  v
RED (Active Threat)
  |
  | No incidents for observation period
  v
YELLOW --> GREEN (gradual de-escalation)
```

Key behaviors:
- **Escalation is fast**: A single confirmed attack indicator jumps from GREEN to YELLOW immediately.
- **De-escalation is slow**: The system waits for a quiet observation period before stepping down. This prevents attackers from triggering a block, waiting briefly, then trying again.
- **Layer activation follows level**: At GREEN, only Layer 1 runs. At YELLOW, Layers 1+2. At RED, all three layers.

---

## The Non-Invasive Architecture

A critical design decision: **none of this modifies Claude Code itself**.

The entire security system is implemented as shell scripts in the `.claude/hooks/` directory. These hooks are a standard extension mechanism provided by Claude Code — they run before or after specific tool calls, receiving the tool's parameters and returning allow/deny decisions.

```
Claude Code calls a tool
     |
     v
Hook system intercepts (PreToolUse)
     |
     v
Security scripts run the 3-layer check
     |
  +--+--+
  |     |
Allow   Deny
  |     |
  v     v
Tool    Tool call
runs    blocked with
        explanation
```

This means:
- DuDuClaw security works with any Claude Code version
- No forking, patching, or monkey-patching required
- The security layer can be updated independently of Claude Code

---

## Specialized Protections

Beyond the three layers, the hook system provides targeted protections:

**Personality File Protection** — The agent's identity file is protected from unauthorized reads and writes. Only the evolution engine (running under a specific environment flag) can modify it.

**Secret Scanner** — All file writes are scanned for patterns that look like API keys, passwords, tokens, or other credentials. If found, the write is blocked and an alert is raised.

**Audit Logger** — Every tool call (allowed or denied) is recorded in an append-only log file. This provides a complete forensic trail for incident investigation.

**Configuration Guard** — Critical configuration files are monitored for unauthorized changes. If a configuration file is modified outside of approved channels, the system alerts.

**Unicode Normalization** — All input is NFKC-normalized before processing to detect homograph attacks (e.g., using Cyrillic characters that look like Latin letters). This prevents visual-spoofing bypass attempts.

**Action Claim Verifier** — Validates cryptographic signatures on tool execution claims, ensuring that claimed tool results actually came from the expected tool.

**RBAC (Role-Based Access Control)** — A role-based access control matrix governs what each user/agent can do. Different roles (admin, operator, viewer) have different permission sets, enforced at the API layer.

---

## Why This Matters

### Cost Efficiency

By reserving AI judgment for the rarest cases (RED level only), the security system adds near-zero cost to normal operations. Most threats are caught by the microsecond-fast Layer 1 blacklist.

### Defense in Depth

No single layer is responsible for all security. An attacker who bypasses Layer 1 (obfuscation) still faces Layer 2 (pattern analysis). An attacker who bypasses Layer 2 still faces Layer 3 (semantic AI judgment).

### Minimal False Positives

Layer 1's blacklist is intentionally conservative — it only blocks things that are *definitely* dangerous. Ambiguous cases are left to the higher layers, which have more context to make accurate decisions.

### Auditability

The JSONL audit log means every security decision is recorded and reviewable. When a security incident occurs (or a false positive is reported), operators can reconstruct exactly what happened, what was blocked, and why.

---

## Interaction with Other Systems

- **CONTRACT.toml**: The behavioral contract defines *what* the agent must not do. The security hooks enforce *how* that's implemented at the tool-call level.
- **Evolution Engine**: The security layer protects the personality file from unauthorized modification, ensuring only the GVU pipeline can evolve the agent.
- **Dashboard**: Threat level and recent security events are visible in the web interface.
- **Audit System**: Integrates with the broader JSONL audit trail for compliance.

---

## The Takeaway

Security doesn't have to be expensive. By layering cheap deterministic checks before expensive AI judgment, DuDuClaw catches the vast majority of threats at near-zero cost — while keeping AI judgment available for the truly ambiguous cases.
