# Durability Framework

> Five pillars of persistence — so a flaky network, a paused service, or a mid-flight crash never silently drops an operation.

---

## The Metaphor: A Parcel Logistics Network

Think of every critical write — a wiki edit, a memory store, an MCP call, an outbound message — as a parcel handed to a courier.

1. **The tracking number** stops the same parcel being shipped twice. Scan it at the depot; if it's already in the system, you don't send a duplicate truck.
2. **Redelivery attempts** handle a recipient who isn't home. The courier waits a bit, comes back, waits longer, comes back again — spaced out so the whole fleet doesn't pile onto the same street at the same minute.
3. **A circuit breaker on a failing route** kicks in when a whole region is unreachable. Instead of sending truck after truck into a flooded road, dispatch stops the route, waits, then sends *one* scout truck to test it before reopening.
4. **A returns warehouse** holds parcels that genuinely can't be delivered after every attempt — so they're never lost, and a human can inspect and re-ship them later.
5. **A delivery manifest** records how far a long multi-stop route got, so if the truck breaks down halfway, the next driver resumes from the last completed stop instead of starting over.

DuDuClaw's `duduclaw-durability` crate is exactly this logistics network — five composable pillars that keep operations reliable across network jitter, service pauses, and process crashes.

---

## The Five Pillars

| Pillar | Module | Guarantee |
|--------|--------|-----------|
| **Idempotency** | `idempotency.rs` | The same operation runs at most once inside a dedup window — duplicates return the original result |
| **Retry** | `retry.rs` | Transient failures are re-attempted with exponential backoff + jitter, per-operation policy |
| **Circuit Breaker** | `circuit_breaker.rs` | A failing dependency is isolated (Open) before it drags the system down, then probed for recovery |
| **Checkpoint** | `checkpoint.rs` | A long task's progress is snapshotted so a crash resumes from the last phase, not from zero |
| **DLQ** | `dlq.rs` | Operations that exhaust all retries are quarantined, never lost, and can be replayed |

All five surfaces are re-exported from `duduclaw_durability::prelude` and assemble into a single `DurabilityLayer`.

---

## Pillar 1 — Idempotency: The Tracking Number

Every operation gets a deterministic key:

```
{agent_id}:{operation_type}:{content_hash}
```

The content hash is the first 32 hex chars of SHA-256 (128-bit collision space). `check_and_record` is a single atomic compare-and-set under one write lock — closing the TOCTOU race where two concurrent callers both see "New":

```
check_and_record(key, placeholder)
     |
     v
acquire write lock
     |
     +-- key already in dedup window? ──► CheckResult::Duplicate { original_result, ... }
     |
     +-- key is new ──► insert placeholder ──► CheckResult::New
     |                       |
     v                       v
release lock          caller runs op, then record(key, real_result)
```

`IdempotencyConfig` defaults: `dedup_window_seconds = 3600`, `max_key_length = 256`, `cleanup_interval_seconds = 7200`. The in-memory implementation can be swapped for a Redis / SQLite backend without changing the interface.

---

## Pillar 2 — Retry: Redelivery Attempts

`RetryEngine` holds a `RetryPolicy` per `operation_type`. The delay grows exponentially and is then perturbed by jitter so simultaneous failures don't synchronize into a retry storm:

```
delay = initial_delay_ms * multiplier^attempt   (capped at max_delay_ms)
        + random jitter in [0, 30% of delay)
```

```
attempt 0   ├─ 500ms ─────┤ retry
attempt 1   ├─ 1000ms ─────────┤ retry
attempt 2   ├─ 2000ms ──────────────────┤ retry
            (each bar also gets up to +30% random jitter)
            └─ exhausted ──► sent to DLQ
```

Errors are classified: `non_retryable_errors` (e.g. `PERMISSION_DENIED`, `INVALID_SCHEMA`) take priority and stop immediately; `retryable_errors` (e.g. `NETWORK_TIMEOUT`, `SERVICE_UNAVAILABLE`, `RATE_LIMITED`) are re-attempted. Default policies ship for `mcp_call`, `memory_write`, `wiki_write`, `message_send`, and `external_api`. The `RetryOutcome` reports `success`, `attempts`, `final_error`, and `sent_to_dlq` — non-retryable failures never go to the DLQ; exhausted retries do.

```toml
[retry.mcp_call]
max_attempts    = 3
initial_delay_ms = 500
max_delay_ms    = 10000
multiplier      = 2.0
jitter          = true
retryable_errors     = ["NETWORK_TIMEOUT", "SERVICE_UNAVAILABLE", "RATE_LIMITED"]
non_retryable_errors = ["PERMISSION_DENIED", "INVALID_SCHEMA", "NOT_FOUND"]
```

---

## Pillar 3 — Circuit Breaker: Closing a Failing Route

Each dependency gets its own three-state breaker. The breaker isolates a failing dependency before retries make things worse:

```
        failure_rate > threshold
          (over min_request_count)
 CLOSED ───────────────────────────►  OPEN
   ▲                                    │
   │ probe_success_required successes   │ reset_timeout elapsed
   │                                    ▼
   └──────────  HALF_OPEN  ◄────────────┘
                   │
                   │ probe fails
                   └──────────────────►  OPEN
```

- **Closed** — all requests pass; a sliding window of the last `window_size` results tracks the failure rate.
- **Open** — requests are rejected with `CircuitOpen` until `reset_timeout_seconds` elapses.
- **HalfOpen** — a limited number of probes are let through. `probe_inflight` accounting caps concurrent probes at `probe_success_required`, so several probes succeeding at once can't over-count and close the breaker prematurely.

A subtle safeguard: if a probe's result is never reported back (panic / cancel / dropped future), `probe_started_at` plus `probe_timeout_seconds` lets the breaker **re-arm** the abandoned slot — otherwise it would stay stuck Open forever. A stray `after_call` with no inflight probe is ignored entirely.

Every transition fires a `StateTransition` callback for the audit trail. Defaults ship for `memory_service` (threshold 0.5), `external_mcp_client` (0.3), and `wiki_service` (0.4).

---

## Pillar 4 — Checkpoint: The Delivery Manifest

A long-running task snapshots its progress so a crash resumes from the last phase:

```
save("task-123", "agent-1", "phase-2", state_json, ttl=3600)
     |
     v
   [crash / restart]
     |
     v
restore("task-123", "agent-1") ──► Some(Checkpoint { phase: "phase-2", state, ... })
     |
     v
   resume from phase-2  (not from zero)
```

Each `Checkpoint` carries `checkpoint_id`, `task_id`, `agent_id`, `phase`, arbitrary JSON `state`, `created_at` / `expires_at`, and a `parent_checkpoint_id` for lineage — enabling "explore an alternative approach from checkpoint X" branching of conversation state (RFC-26 §4.2). `CheckpointConfig` defaults: 24h TTL, `max_checkpoints = 10_000`, hourly cleanup.

---

## Pillar 5 — DLQ: The Returns Warehouse

When retries are exhausted, the operation isn't dropped — it's quarantined in the Dead Letter Queue:

```
retry exhausted
     |
     v
DLQ.enqueue(agent_id, operation_type, original_operation, retry_count, last_error, ttl)
     |
     v
DlqRecord { status: Pending }
     |
     +──► replay ──► status: Replayed
     |
     +──► give up ──► status: Abandoned
     |
     +──► TTL elapses ──► pruned
```

Each `DlqRecord` preserves the full `original_operation` JSON, the agent, the operation type, the retry count, the last error, and timestamps — enough context for a human (or an automated job) to understand and re-ship the failed work. Status moves through `Pending → Replayed` / `Abandoned`.

---

## How the Pillars Compose

A single durable write threads through all five:

```
incoming operation
     |
     v
1. Idempotency.check_and_record ── Duplicate? ──► return original_result
     | New
     v
2. CircuitBreaker.before_call ── Open? ──► reject (fail fast)
     | allowed
     v
3. RetryEngine.execute(op) ── transient failure? ──► backoff + jitter, retry
     |                                                     |
     | success                                             | exhausted
     v                                                     v
   CircuitBreaker.after_call(true)                 5. DLQ.enqueue
   Idempotency.record(real_result)                    CircuitBreaker.after_call(false)
   4. Checkpoint.save(next_phase)
```

This is the chain the gateway's LLM fallback path and durable cron jobs run on — the same five guarantees protect both automated scheduling and the primary inference fallback.

---

## Why This Matters

### No Silent Loss

The DLQ guarantees that a write which exhausts retries is captured, not dropped. Combined with checkpoints, a crash mid-task is recoverable rather than catastrophic.

### No Duplicate Side Effects

A retried or re-delivered message could otherwise post twice, store a memory twice, or double-charge an external API. The idempotency key — deterministic, content-addressed — makes "at most once inside the window" a hard guarantee, even under concurrent callers.

### No Cascading Failure

Without a breaker, a dead `memory_service` would absorb every retry of every agent until the whole gateway stalls. The breaker trips early, fails fast with `CircuitOpen`, and probes for recovery on its own schedule.

### Jitter Prevents Thundering Herds

When many agents fail at the same instant, deterministic backoff would have them all retry on the same millisecond. True random jitter (up to 30%) spreads the load.

### Configurable, No Magic Numbers

Every threshold — retry counts, delays, failure rates, timeouts, TTLs — is a config field with a validated default. Policies hot-reload via `upsert_policy` / `upsert_config`.

---

## The Takeaway

A parcel network doesn't promise that no truck ever breaks down — it promises that a broken-down truck never loses your parcel. DuDuClaw's durability framework makes the same promise for every critical write: tracked so it isn't duplicated, retried so a hiccup doesn't kill it, breaker-guarded so a dead dependency doesn't take the system with it, checkpointed so a crash resumes, and DLQ-backed so nothing that genuinely fails is ever silently lost.
