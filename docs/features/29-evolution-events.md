# Evolution Events System

> The agent's flight data recorder — every meaningful evolution, governance, and durability event captured with guaranteed-delivery batching.

---

## The Metaphor: A Flight Data Recorder

An aircraft's black box doesn't fly the plane. It sits in the tail, recording every meaningful event — altitude changes, engine state, control inputs, warnings — onto a write-once medium that survives even a crash. The pilots never read it mid-flight. But when something goes wrong, the recorder is the *only* source of truth about what actually happened, in what order, and why.

The Evolution Events System is DuDuClaw's flight data recorder for agents. It doesn't *drive* evolution — the GVU loop, prediction engine, governance layer, and durability framework do that. It quietly records every meaningful event each of them emits: a skill activated, a security scan run, a policy violated, a circuit breaker tripped, a retry exhausted. Each record lands in an append-only JSONL log with a fixed 8-field schema, never blocking the agent that emitted it.

When an operator opens the dashboard's **Reliability** page weeks later to ask "why did this agent's success rate drop?", the answer comes from the recorder — not from memory, not from guesswork.

---

## The Shape of One Record

Every event — regardless of which subsystem emitted it — serialises to a single JSONL line with the same 8 fields. A fixed-width schema (no missing keys; absent values serialise as `null`) keeps downstream parsers simple.

```
AuditEvent {
  timestamp:      "2026-06-21T07:14:02Z"     ← RFC3339, UTC
  event_type:     "gvu_generation"           ← which subsystem / action
  agent_id:       "duduclaw-pm"              ← who it happened to
  skill_id:       "python-patterns" | null   ← the skill involved, if any
  generation:     3 | null                    ← GVU generation counter
  outcome:        "success"                   ← success / failure / suppressed / ...
  trigger_signal: "prediction_error" | null   ← the upstream cause
  metadata:       { ... }                     ← small (<1 KB) structured diagnostics
}
```

The schema is **append-only and backward-compatible**: P0 variants are never renamed or removed, and every later expansion is purely additive. A parser written for the first release still reads the latest logs.

---

## Event Categories

The `AuditEventType` enum (in `schema.rs`) spans three domains. The original P0 set covers evolution; the W19-P1 expansion added Governance and Durability.

| Domain | Event types | What it records |
|--------|-------------|-----------------|
| **Evolution (P0)** | `skill_activate`, `skill_deactivate`, `security_scan`, `gvu_generation`, `signal_suppressed`, `skill_graduate` | Skills turning on/off, security vetting, GVU self-play cycles, stagnation suppression, cross-agent graduation |
| **Governance (W19-P1)** | `governance_violation`, `governance_approval_requested`, `governance_approval_decided`, `governance_policy_changed`, `governance_quota_reset` | Policy violations, approval workflow, policy CRUD, daily quota resets |
| **Durability (W19-P1)** | `durability_retry_attempt`, `durability_retry_exhausted`, `durability_circuit_opened`, `durability_circuit_recovered`, `durability_checkpoint_saved`, `durability_dlq_replayed` | Retry attempts and exhaustion, circuit-breaker transitions, checkpoint saves, DLQ replays |

The `Outcome` enum is similarly layered: P0 has `success` / `failure` / `suppressed`; W19-P1 adds `blocked`, `warned`, `throttled`, `pending`, `approved`, `rejected`, `triggered`, `recovered` — so a `governance_violation` can be `blocked`, an approval can be `pending`, and a `durability_circuit_opened` can be `triggered`.

---

## The Four Modules

The system is built from four focused modules under `gateway/evolution_events/`.

| Module | Role |
|--------|------|
| `schema.rs` | Defines `AuditEvent` (the 8-field record), `AuditEventType` (17 variants across 3 domains), and `Outcome`. The single source of truth for the wire format. |
| `emitter.rs` | The non-blocking front door. `EvolutionEventEmitter` exposes typed helpers (`emit_skill_activate`, `emit_gvu_generation`, …); every emit spawns a detached Tokio task so the caller is never blocked by I/O. A process-global singleton serves call sites that can't thread the emitter through. |
| `query.rs` | The read path. `AuditEventIndex` is a SQLite-backed index cache over the JSONL files; `AuditQueryFilter` / `AuditQueryResult` support paginated, filtered reads. |
| `reliability.rs` | The analytics layer. Pure functions roll raw events up into a `ReliabilitySummary` per agent over a time window. |

(A fifth file, `logger.rs`, is the JSONL appender the emitter writes through — day-based + 10 MB size rotation, retry-once on write error, and metadata redaction before persistence.)

---

## The Write Path: Emit → Batch → Store

The recorder's first job is to never slow down the agent. The write path is asynchronous end to end.

```
GVU loop / governance / durability subsystem
         |
         | emitter.emit_gvu_generation(agent, gen, outcome, ...)
         v
EvolutionEventEmitter        ← returns immediately
         |
         | tokio::spawn (detached) — caller never blocks on I/O
         v
EvolutionEventLogger.log(event)
         |
         | redact sensitive metadata (e.g. last_error → [REDACTED])
         v
append one JSON line to events/YYYY-MM-DD.jsonl
         |
         +── date changed?      → rotate to new day file
         +── file ≥ 10 MB?      → rotate to YYYY-MM-DD-{seq}.jsonl
         |
         v
fsync on flush() for durability
```

If a write fails (rotation hiccup, transient FS error), the logger invalidates the stale handle, reopens, and **retries once** before dropping the record — an audit event should survive a momentary glitch, not vanish.

---

## The Read Path: Store → Query → Reliability Page

JSONL is great for appending but slow for filtered queries. So the read path indexes the logs into SQLite, then aggregates.

```
events/*.jsonl  ──(background sync)──>  AuditEventIndex (SQLite)
                                              |
                  ┌───────────────────────────┼───────────────────────────┐
                  v                            v                           v
        audit.evolution_query        audit.reliability_summary    /api/reliability/summary
        (filtered, paginated)        (rolled-up metrics)          (HTTP endpoint)
                  |                            |                           |
                  └────────────────┬──────────┴───────────────────────────┘
                                   v
                          Web ReliabilityPage.tsx
                   consistency · task success · skill adoption · fallback rate
```

The `AuditEventIndex` is opened once and shared behind an `Arc`, kept synced by a background task, so the RPC handlers and the `/api/reliability/summary` HTTP endpoint reuse one connection rather than each re-scanning the JSONL.

### Query safety

`query.rs` is hardened against abuse:

- **Column allowlist** — every filterable column must appear in `ALLOWED_FILTER_COLS`; an out-of-list column name is rejected, closing any future SQL-injection vector.
- **Clamped pagination** — `limit` is clamped to `[1, MAX_LIMIT]` and `offset` is bounded, so an enormous OFFSET can't make SQLite scan unbounded rows (a DoS guard).

---

## The Reliability Summary

`reliability.rs` turns the raw event stream into operator-readable health metrics. All rate fields are in `[0.0, 1.0]`; with no events in the window the success-oriented metrics return a conservative neutral `1.0` and the adoption/fallback rates return `0.0`.

```
ReliabilitySummary (per agent, default 7-day window)
├─ consistency_score      = mean over event_types of (success / total)
├─ task_success_rate      = success events / all events
├─ skill_adoption_rate    = skill_activate events / all events
├─ fallback_trigger_rate  = llm_fallback events / all events
├─ total_events           = audit rows counted in the window
└─ generated_at           = RFC3339 timestamp of computation
```

Each metric is computed by a small **pure function** (`avg_success_rate`, `task_success_rate`, `skill_adoption_rate`, `fallback_trigger_rate`) taking aggregate counts — which makes them trivially unit-testable with fixtures and free of any I/O.

---

## Reliability Guarantees

The recorder is engineered so that recording never harms the thing being recorded, and so that records survive ordinary failures.

```
GUARANTEE                      MECHANISM
─────────────────────────────  ────────────────────────────────────────
Never blocks the caller        detached tokio::spawn per emit
Survives transient write fail  invalidate handle → reopen → retry once
Bounded file growth            day rotation + 10 MB size rotation
No secret leakage              metadata redaction before JSONL write
Durable on flush               fsync via flush()
Backward-compatible schema     P0 variants never renamed; additions only
Query can't be weaponised      column allowlist + clamped limit/offset
Fixed-width JSON               absent fields serialise as null
```

---

## Why This Matters

### Observability without coupling

Each subsystem emits one line and moves on. The GVU loop doesn't know about the dashboard; the governance layer doesn't know about SQLite. The recorder decouples *producing* events from *consuming* them, so any new subsystem can start logging by calling one emit helper.

### A truthful audit trail

Because the schema is backward-compatible and the log is append-only, the history can't be silently rewritten. Old records mean exactly what they meant when written — essential for governance and security review.

### Operator answers, not guesses

The Reliability page turns thousands of raw events into four numbers an operator actually cares about: is this agent consistent, succeeding, adopting skills, and avoiding cloud fallback? When a number moves, the underlying events are right there to drill into.

### Safe under load and adversaries

Non-blocking emits keep hot paths fast. Rotation keeps disk bounded. The query allowlist and offset clamp keep the read path from being turned into a DoS or injection surface.

---

## The Takeaway

A black box doesn't fly the plane — but without it, every incident is a mystery. The Evolution Events System plays that role for DuDuClaw agents: a non-blocking emitter, a rotating JSONL log with retry-once durability, a SQLite-indexed query layer, and a reliability roll-up surfaced on the dashboard. Evolution, governance, and durability all flow through one fixed 8-field schema, so the story of how an agent changed over time is always recorded, always queryable, and always safe to read.
