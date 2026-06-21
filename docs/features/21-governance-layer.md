# Governance Layer

> Building codes plus utility meters — declarative YAML policies that gate every agent action, fail safe, and reload without a restart.

---

## The Metaphor: Building Regulations and Utility Meters

A modern building runs on two kinds of governance.

**Building regulations** decide what you're *allowed* to do. You may add a balcony, but you may not knock out a load-bearing wall. Some changes are fine to make on your own; others (rewiring the mains) require a signed permit and an inspector. The rules are written down, posted publicly, and apply to everyone unless a specific override is granted.

**Utility meters** decide how much you're allowed to *consume*. Your electricity meter doesn't care *what* you do — it tracks total draw against a monthly allowance. Cross a soft threshold and you get a warning on your bill; cross the hard cap and the supply is cut until the next billing cycle resets the meter.

DuDuClaw's Governance Layer is exactly these two surfaces sitting in front of every agent. **Permission and Lifecycle policies** are the building regulations — what an agent may do, and what needs approval. **Rate and Quota policies** are the meters — how fast and how much an agent may consume before it's throttled or cut off. Both are declared in plain YAML, both reload on edit, and both fail closed if the rulebook is missing or malformed.

---

## Four Policy Types

The `duduclaw-governance` crate models all governance as a single tagged enum, `PolicyType`, with four variants:

| Policy Type | Governs | Key Fields | Default Action |
|-------------|---------|-----------|----------------|
| **Rate** (`RatePolicy`) | Operations per time window | `resource`, `limit`, `window_seconds`, `action_on_violation` | `reject` |
| **Permission** (`PermissionPolicy`) | What scopes an agent may use | `allowed_scopes`, `denied_scopes`, `requires_approval` | denied wins |
| **Quota** (`QuotaPolicy`) | Daily consumption budgets | `daily_token_budget`, `max_concurrent_tasks`, `max_memory_entries`, `reset_cron` | reset at `00:00` UTC |
| **Lifecycle** (`LifecyclePolicy`) | Agent health & idle behavior | `max_idle_hours`, `health_check_interval_seconds`, `auto_suspend_on_violation_count` | auto-suspend |

Every variant carries a `policy_id` (unique identifier) and an `agent_id` — where `"*"` means the policy applies to **all** agents. Every variant implements `validate()`, and an invalid policy (e.g. `limit: 0`) is rejected, not silently accepted.

---

## Policy Resolution: Agent Overrides Global

The `PolicyRegistry` loads policies from a directory and resolves them per-agent. The precedence is strict and one-directional:

```
Resolution order (highest priority first)
─────────────────────────────────────────
  policies/{agent_id}.yaml      ← agent-specific overrides
        ↓ overrides
  policies/global.yaml          ← global defaults ("*")
        ↓ overrides
  system built-in defaults
```

When `get_policies_for_agent("alice")` is called, the registry:

```
1. Collect agent-specific policies for "alice"
2. Collect global policies (agent_id = "*")
3. For each global policy:
     is there an agent policy with the SAME
     (policy_id, type_name)?
        ├─ yes → skip global (agent version wins)
        └─ no  → inherit global
4. Return merged list
```

The dedup key is **(policy_id, type_name)** — not `policy_id` alone. This matters: an agent's `permission` policy named `shared` must NOT erase a global `quota` policy that happens to also be named `shared`. Only a same-id *and* same-type agent policy overrides its global counterpart. Both survive otherwise.

---

## Fail-Safe Loading: Skip the Illegal, Keep the Valid

A governance system that crashes when one policy is malformed is worse than useless — it takes the whole gate down. The registry loads **fail-safe**: a bad policy is skipped with a warning, and every other valid policy keeps working.

```
load() walks policies/
     |
     v
For each *.yaml file:
     |
     ├─ Parse YAML
     │     ├─ parse error → warn + skip whole file
     │     └─ ok ↓
     |
     ├─ For each policy in file:
     │     ├─ validate() fails → warn + skip THIS policy
     │     └─ validate() ok    → keep
     |
     v
Filename stem = agent_id
     └─ stem fails [a-zA-Z0-9\-_] check
           → warn + skip file (path-traversal guard)
```

Concretely: a `global.yaml` containing one policy with `limit: 0` (invalid) and one with `limit: 100` (valid) loads exactly **one** policy — the good one. A completely unparseable `global.yaml` loads **zero** policies and the system continues with empty governance rather than panicking. A missing directory is not an error either; it yields an empty policy set.

The filename guard is a security boundary: agent IDs come from filenames, and a file named `../evil.yaml` or `agent.name.yaml` is rejected before its `file_stem` is ever used as a lookup key.

---

## Hot Reload Without a Restart

The registry can watch its directory with `notify` (inotify on Linux, equivalent on macOS/Windows). When a YAML file is created, modified, or removed, the registry reloads asynchronously:

```
operator edits policies/global.yaml
     |
     v
notify fires Create/Modify/Remove event
     |
     v
event path ends in .yaml?
     ├─ no  → ignore
     └─ yes → tokio::spawn(registry.load())
              "Policy file changed, reloading..."
```

Reload runs through the same fail-safe path, so a broken edit during live operation degrades gracefully — the bad file is skipped, the previous valid set in memory is replaced only by the new valid set. No restart, no downtime, no half-applied rulebook.

---

## Quota: Soft Tracking, Hard Enforcement

The `QuotaManager` is a separate per-agent usage tracker that the evaluator consults. It enforces three consumption dimensions against a `QuotaPolicy`, and the enforcement is **hard** — crossing the budget returns a typed `QuotaError`, not a warning.

```
consume_tokens(agent, policy, tokens)
     |
     v
needs_reset()?  (Utc::now() >= reset_at)
     ├─ yes → reset counters, schedule next 00:00 UTC,
     │         fire governance_quota_reset event
     └─ no  ↓
     |
     v
new_total = token_used + tokens
     |
     ├─ new_total > daily_token_budget
     │     → Err(TokenBudgetExhausted { used, budget })   ← HARD STOP
     │
     └─ new_total <= daily_token_budget
           → token_used = new_total                        ← OK
```

The boundary is precise: a consume is rejected only when the running total would **strictly exceed** the budget. Reaching *exactly* the budget is allowed — the last legitimate token still fits. But once `token_used >= budget`, `check_token_budget()` reports exhausted and no further positive consume can succeed. The two views stay consistent.

The three quota dimensions:

| Dimension | Method | Error on Breach |
|-----------|--------|-----------------|
| Daily tokens | `consume_tokens` / `check_token_budget` | `TokenBudgetExhausted { used, budget }` |
| Concurrent tasks | `increment_concurrent_tasks` (paired with `decrement_concurrent_tasks`) | `ConcurrentTasksExceeded { current, max }` |
| Memory entries | `set_memory_entries` | `MemoryEntriesExceeded { current, max }` |

Counters are per-agent and isolated — `agent-a` exhausting its budget never touches `agent-b`. The daily reset is computed as tomorrow's `00:00` UTC; on reset, a fire-and-forget `governance_quota_reset` audit event is emitted (with `reset_type` of `daily`, `manual`, or bulk `"*"`).

---

## Error Codes: From Violation to HTTP

When the evaluator denies an operation, the violation is mapped to a stable `PolicyErrorCode` with a fixed HTTP status — so callers and dashboards get consistent, machine-readable responses:

| Error Code | HTTP | Meaning |
|------------|------|---------|
| `POLICY_RATE_EXCEEDED` | 403 | Rate limit exceeded |
| `POLICY_PERMISSION_DENIED` | 403 | Scope not allowed |
| `POLICY_QUOTA_EXCEEDED` | 403 | Daily quota exhausted |
| `POLICY_LIFECYCLE_VIOLATION` | 403 | Lifecycle policy violated |
| `POLICY_NOT_FOUND` | 404 | Policy does not exist |
| `POLICY_CONFLICT` | 409 | Policy conflict (same id, different type) |
| `POLICY_INVALID_SCHEMA` | 422 | Schema validation failed |
| `POLICY_APPROVAL_REQUIRED` | 202 | Operation queued for approval (not an error) |

Note that `POLICY_APPROVAL_REQUIRED` is `202 Accepted`, not a 4xx — an operation needing approval is *pending*, not *denied*. An allowed operation that merely tripped a `warn`-level rate rule produces no API error at all.

---

## A Complete Policy File

The shipped `policies/global.yaml` declares the default rule set every agent inherits — and demonstrates all four types in one file:

```yaml
policies:
  # ── Rate: 200 MCP calls per minute ──
  - policy_type: rate
    policy_id: default-rate-mcp
    agent_id: "*"
    resource: mcp_calls
    limit: 200
    window_seconds: 60
    action_on_violation: reject

  # ── Quota: 500k daily tokens, 5 concurrent tasks ──
  - policy_type: quota
    policy_id: default-quota-daily
    agent_id: "*"
    daily_token_budget: 500000
    max_concurrent_tasks: 5
    max_memory_entries: 10000
    reset_cron: "0 0 * * *"

  # ── Permission: deny admin, gate agent CRUD on approval ──
  - policy_type: permission
    policy_id: default-permission
    agent_id: "*"
    allowed_scopes:
      - memory:read
      - memory:write
      - wiki:read
      - wiki:write
      - messaging:send
      - mcp:call
    denied_scopes:
      - admin
      - governance:write
    requires_approval:
      - agent:create
      - agent:modify
      - agent:remove

  # ── Lifecycle: auto-suspend after 48h idle ──
  - policy_type: lifecycle
    policy_id: default-lifecycle
    agent_id: "*"
    max_idle_hours: 48
    health_check_interval_seconds: 300
    auto_suspend_on_violation_count: 10
```

To raise the MCP rate limit for a single agent, drop a `policies/duduclaw-eng-infra.yaml` with a `default-rate-mcp` policy at `limit: 500` — it overrides the global limit for that agent only, while the global permission and quota policies are still inherited.

---

## Why This Matters

### Declarative, Not Hardcoded

Limits live in YAML, not in magic numbers scattered through code. `global.yaml` is the single source of truth for every default ceiling — auditable, version-controlled, and editable without a recompile.

### Fail-Safe by Design

Governance follows the project's "security gates fail closed" rule. A malformed policy is skipped, not fatal; a missing rulebook yields empty rules, not a panic. One bad edit never takes the whole gate offline.

### Override Without Forking

Per-agent files override global defaults by `(policy_id, type)` — a power agent gets a higher ceiling, a quarantined agent gets a stricter one, and neither requires touching the shared `global.yaml`. Same-id-different-type policies coexist instead of clobbering each other.

### Consistent, Machine-Readable Denials

Every violation maps to one stable error code and HTTP status. Dashboards, MCP clients, and audit logs all see the same vocabulary — `POLICY_QUOTA_EXCEEDED` always means the same thing, always returns 403.

### Hot Reload for Live Ops

Tightening a rate limit during an incident is an edit-and-save, not a redeploy. The watcher picks up the change and the new ceiling applies on the next evaluation.

---

## The Takeaway

A building needs both a rulebook (what you may build) and meters (how much you may use). DuDuClaw's Governance Layer is both, in front of every agent: Permission and Lifecycle policies say *what is allowed*, Rate and Quota policies say *how much is allowed*. All of it is declared in fail-safe YAML, resolved agent-over-global, reloadable without a restart, and denied with a consistent error vocabulary. It's the gate that lets you run untrusted, self-evolving agents at scale without trusting them blindly.
