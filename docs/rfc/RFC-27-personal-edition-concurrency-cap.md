# RFC-27: Personal-edition concurrency cap — cross-process in-flight goal-dispatch limiting

**Status:** Implemented (core) — the concurrency gate primitive, goal-loop wiring, and edition resolution have landed; see [`docs/features`] and the CHANGELOG. Enforcement is default-on for the Personal edition (cap 2) and a no-op for Enterprise.
**Author:** DuDuClaw team
**Date:** 2026-08-12
**Supersedes decision:** none. Implements the **D1-C / D2-B** ruling in `commercial/docs/ux-redesign-2026-08/09-edition-split-features.md §4` — "個人版不限 Agent 數量,改限併發(同時執行的 goal loop／dispatch 數)". Item 16 of that document's §5.2 (規模 L) is the work this RFC covers.

---

## 1. Motivation

The 2026-07-16 decision (recorded in `commercial/docs/edition-split-moat-strategy-2026-07-13.md`) is a hard constraint: **the Personal edition never gets a hard agent-count cap** — the self-host / open-core promise wins, and `EditionProfile::personal_max_agents()` defaults to `0` (unlimited). The 2026-08-12 UX review then asked for "一點限制" (some visible limit) that a single user barely notices but a commercial workload feels.

Those two constraints collide only if the limit is placed on *agent count*. They do **not** collide on *resource concurrency*: capping how many goal dispatches run **simultaneously** limits resource use, never capability. A single owner rarely fires two autonomous goals at once; a team running a board of goals hits it immediately. This is the same shape as the one edition quota that already exists — `os_native_agent_quota()` returns `Some(1)` for Personal, `None` for Enterprise — "鎖 quota 不鎖能力" applied to a different resource.

**What is missing today:** there is no cross-process count of simultaneously in-flight goal dispatches. The goal loop has an in-process `max_concurrent = 3` spawn-storm guard (§4), but it is (a) per-gateway-process, (b) edition-blind, and (c) a resource ceiling that applies identically to everyone. `features.toml` carries a `max_messages_per_month` field with **zero readers**. This RFC builds the missing counter.

### 1.1 Non-goals

- **Not** a rate limiter. `dispatch_guard` (arXiv:2607.01641) already bounds *events per rolling window* — a spawn-storm brake. This RFC bounds *simultaneous holders* — a concurrency brake. The two are orthogonal and both stay.
- **Not** a security gate. It is a commercial quota / resource throttle. It **fails open** (§6), exactly like `dispatch_guard`, and unlike the MCP authorization gate (which fails closed).
- **Not** an agent-count cap. That decision stands at "unlimited by default" and is untouched.
- **Not** a per-message or per-token meter. D2 rejected message metering for BYO-key users; concurrency is the honest axis.

---

## 2. What is counted, and where it is enforced

**Counted:** one unit per goal task that is *in-flight* — admitted for autonomous dispatch by the `GoalLoopDriver` and not yet in a terminal state (`done` / `failed` / `cancelled` / `needs_human`). This is exactly the set the driver already tracks in its in-memory `inflight` map; the gate mirrors that lifecycle into a durable, cross-process count.

**Enforced at:** the goal-loop admission point — the same branch as the existing in-process concurrency guard (`goal_loop.rs`, the `is_new && active >= max_concurrent` check). A **new** admission first acquires a lease; a re-dispatch of an already-tracked task carries its existing lease forward and is never re-counted (identical semantics to the in-process guard, which also only gates new admissions).

**Why the admission point and not the CLI spawn:** the durable goal task is the unit the user cares about ("give a goal → loop to completion"). Counting at the admission gate keeps the count aligned with the `inflight` map that already drives every other goal-loop decision, so there is one source of truth for "how many goals are running", not two that can drift.

---

## 3. The concurrency gate primitive (`duduclaw-core::concurrency_gate`)

A lease-based, cross-process in-flight counter. It is the *concurrency* twin of `dispatch_guard`'s *rate* limiter and deliberately reuses that module's disciplines: `with_file_lock` around every read-modify-write, atomic temp+rename save, corrupt/missing state treated as empty, fail-open on lock failure.

### 3.1 Why leases (not a windowed counter, not a published count)

A concurrency count needs an explicit *release*, which a rate limiter does not. Three shapes were considered:

| Shape | How release works | Cross-process race | Crash recovery | Verdict |
|---|---|---|---|---|
| Windowed event counter (dispatch_guard-style) | events age out of the window | none | automatic | **rejected** — measures rate, not simultaneity; a long goal would age out of its own window while still running |
| Published per-process count | process rewrites its live count each tick; gate sums fresh entries | read-then-admit race across processes | stale entries pruned | rejected — precise only in the single-process case, and racy across processes |
| **Lease acquire/release + TTL** | caller releases on terminal; unreleased leases TTL-expire | eliminated (acquire is atomic under the lock) | TTL reclaim | **chosen** |

Leases are the only shape that is precise *and* race-free across processes. The cost is a release obligation and a TTL safety net — both handled in §5.

### 3.2 State & API

State lives in `<home>/concurrency_leases.json`: `{ lease_id → { class, expires_ms } }`. `class` is a short stable label for the resource pool (`"goal"` for goal dispatch); scoping by class means a future second consumer cannot starve the goal budget.

```
effective_limit(edition, cfg) -> Option<u32>
    Personal → Some(cfg.personal_max_concurrent) unless it is 0 (0 = unlimited)
    Enterprise / anything else → None (unlimited)

try_acquire(home, class, limit: Option<u32>, ttl_secs) -> AcquireOutcome
    limit == None            → Admitted(Lease::unguarded)   // ZERO file I/O — Enterprise pays nothing
    limit == Some(cap):
        under with_file_lock: prune expired → count `class` leases
            count < cap      → insert {id, class, now+ttl}, Admitted(Lease::guarded)
            count >= cap      → AtCapacity { active, limit: cap }
        lock/FS failure       → Admitted(Lease::unguarded)   // FAIL OPEN

release(home, &Lease)      // guarded → remove by id under lock; unguarded → no-op
renew(home, &Lease, ttl)   // guarded → extend expiry under lock; unguarded → no-op
active_count(home, class)  // observability / tests
```

- `Lease::unguarded` carries no id; `release`/`renew` on it are no-ops. Every "we did not actually take a slot" path (unlimited edition, fail-open) returns one, so the caller has a single uniform type and never branches on "did the gate apply".
- All file failures fail open (return `Admitted(unguarded)` / silently succeed). A broken counter file must never wedge autonomous goal completion — same posture as `dispatch_guard`, and consistent with "工具失效時停工上報" applying to *tools*, not to a soft quota brake.

### 3.3 Configuration

`config.toml [dispatch]` (the section already carries `enabled` / `grounding_precheck_enabled`):

```toml
[dispatch]
personal_max_concurrent   = 2      # Personal-edition in-flight goal cap; 0 = unlimited
concurrency_lease_ttl_secs = 1800  # crash-recovery TTL (renewed every tick while in-flight)
```

Parsed in isolation from a generic `toml::Table` (dispatch_guard's `from_home` pattern) so unrelated malformed config can never break it; absent/malformed → defaults. `DUDUCLAW_PERSONAL_MAX_CONCURRENT` env overrides the config value (mirrors `DUDUCLAW_PERSONAL_MAX_AGENTS`).

**Default value rationale — why 2:** it must be `< 3` (the in-process default) or the edition gate is a no-op that never fires; it must be `≥ 2` or a single user with two agents feels it constantly. `2` is the smallest value that leaves the common single-goal and two-goal cases untouched while making a board of ≥3 concurrent goals a visible, honest limit. Operators and hosted deployments tune it; Enterprise ignores it entirely.

---

## 4. Relationship to the existing `max_concurrent = 3` guard

They are different guards at the same admission point and both apply; the stricter wins.

| | In-process `max_concurrent` | Edition concurrency gate (this RFC) |
|---|---|---|
| Purpose | spawn-storm / OOM brake for one gateway | commercial resource differentiation |
| Scope | one gateway process | cross-process (all writers sharing a home dir) |
| Edition-aware | no — applies to everyone | yes — Personal capped, Enterprise unlimited |
| Default | 3 | Personal 2 / Enterprise ∞ |
| Config | `[goal_loop] max_concurrent` | `[dispatch] personal_max_concurrent` |
| Failure mode | in-memory, cannot fail | fails open |

**Effective concurrency = `min(in-process guard, edition gate)`.**
- Personal: `min(3, 2) = 2`.
- Enterprise: `min(3, ∞) = 3` (raise `[goal_loop] max_concurrent` for more; the edition gate never bites).

The edition gate is checked *after* the in-process guard, so a candidate the cheap in-memory check already deferred never touches the file. Ordering is cost-first, mirroring the RPC edition gate's "cheap string test before edition resolve".

---

## 5. Lifecycle wiring in the goal loop

The `inflight` map is the authority for "in-flight". The lease mirrors its lifecycle:

- **Acquire** — at the admission branch, for a *new* task only, after the in-process guard passes. `AtCapacity` → `continue` (defer to next tick; see §6 on why defer, not reject). `Admitted(lease)` → the lease is stored on the task's `InFlight` record.
- **Carry forward** — a re-dispatch (rejection retry / stalled re-send) reuses the existing `InFlight` lease; it is never re-acquired or double-counted.
- **Release** — every `inflight.remove(id)` site (the reconcile loop's `done` and terminal arms, and `escalate()`) releases the removed record's lease. Three sites, one helper.
- **Renew** — at the top of each tick, every currently-held lease is renewed (`now + ttl`). With a 30 s tick and a 1800 s TTL the margin is 60×; a crashed gateway's leases self-expire within the TTL and the count self-heals without any operator action.

Edition is resolved at driver (re)spawn (`respawn_goal_loop_driver`) via the existing `resolve_edition_profile()` chain (env > override > license tier), turned into `effective_limit(...)`, and injected via a builder. A missing/`None` limit disables the gate entirely (byte-identical to pre-RFC behavior) — this keeps every existing goal-loop unit test, and the zero-config test driver, on the untouched path. An edition change takes effect on the next config reload / respawn (the same cadence at which `[goal_loop]` config itself is re-read), which is acceptable because edition changes are rare license events.

---

## 6. Fail-open vs fail-closed — the two "full" behaviors

Two distinct failure situations, two deliberate answers:

1. **Gate infrastructure failure** (lock contention, unreadable/corrupt state file) → **fail OPEN** (admit). Rationale: this is a soft commercial brake, not a security boundary. Fail-closed here would strand a durable goal on a transient filesystem blip, breaking the core "goal → loop to completion" promise. `dispatch_guard` sets the precedent explicitly, and CLAUDE.md's "security gates fail closed" convention is scoped to *security* gates — this is not one.

2. **Legitimately at capacity** (`AtCapacity`) → **defer / queue, not hard-reject.** Recommendation and implementation: the deferred task stays a candidate and is retried next tick when a lease frees up — identical to how the in-process `max_concurrent` guard already behaves (`continue`). Hard-rejecting would *drop a durable goal the user asked to be completed*, which is unacceptable; a concurrency cap is a throttle on *how fast* goals run, never a killer of goals. Queue depth is naturally bounded by the durable task board, so deferral cannot runaway.

The net user-visible effect on Personal: with 3+ goals queued, at most 2 run at once and the rest start as soon as a slot frees — a throughput ceiling, never a lost task. That is the "單人幾乎無感,商用有感" the decision asked for.

---

## 7. Security & convention compliance

- **No raw byte slicing / unanchored `contains` / unlocked appends** (2026-06 conventions): lease ids are UUIDs; class comparison is exact-equality; every mutation is under `with_file_lock`; save is atomic temp+rename.
- **Fail-open is deliberate and documented** (§6); it is the correct posture for a non-security resource brake and matches `dispatch_guard`.
- **No new edition-determination path** (鐵律 4): edition is resolved through the one existing `resolve_edition_profile()` / `EditionProfile::resolve` chain. No second source of truth.
- **State file bounded**: expired leases are pruned on every acquire; idle classes leave no residue after their leases expire.

---

## 8. Testing

- Core (`concurrency_gate.rs`): admit-under-cap then `AtCapacity`; release frees a slot; TTL-expired leases are reclaimed on the next acquire; `renew` keeps a lease alive; unlimited (`None`) never touches the file; corrupt/absent state fails open; `effective_limit` Personal-vs-Enterprise; config `from_home` partial-section + env override; cross-process (two calls sharing a home dir) respect one shared count.
- Goal loop: with the edition limit wired to 1, a second concurrent new goal defers (queued, then dispatched after the first reaches terminal and releases); with the limit `None`, behavior is byte-identical to today.

---

## 9. Rollout

- **Default-on for Personal at cap 2**, no-op for Enterprise. This is a behavior change for Personal deployments running ≥3 concurrent goals; the CHANGELOG entry documents it and the tune knob.
- Config + env override ship in the same change so a hosted deployment can widen or disable it without a rebuild.
- Dashboard surfacing of "N/M goals running" is a follow-up (out of scope here; `active_count` exposes the number).
