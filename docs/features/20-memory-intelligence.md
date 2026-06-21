# Memory Intelligence

> Facts that supersede each other, mistakes that become rules, and recall by the handful — three upgrades layered onto the live memory engine without a schema rewrite.

---

## The Metaphor: A Doctor's Patient Chart

A good doctor doesn't treat a chart as a flat pile of notes. They work it like three connected habits:

1. **Facts have a timeline.** "Patient is on 10mg of the drug" is true *until* the dose is changed. When a new dose is recorded, the old line isn't erased — it's stamped "valid through March 3," and the new line takes over. Ask "what was the dose last winter?" and the chart answers from the right moment in history.
2. **Mistakes turn into protocol.** After the third time a particular drug interaction is missed, the clinic doesn't just fix that one case — it writes a standing rule: "Always check for interaction X." The next doctor reads the rule, not the three incident reports.
3. **Recall comes by the handful.** When reviewing a case, the doctor pulls the exact pages they need by reference number — not by re-reading the whole binder one sheet at a time.

DuDuClaw's **Memory Intelligence** (v1.19.0) gives the agent the same three habits — built *non-invasively* on the existing `SqliteMemoryEngine` (no schema rewrite, `MemoryEntry` unchanged).

---

## The Three Features

| | Feature | What it does | Where it lives |
|-|---------|--------------|----------------|
| **F1** | Temporal Memory | Facts gain a validity window + knowledge-graph triple; new facts supersede old ones and link a chain | `engine.rs` — `store_temporal`, `get_history`, `get_at` |
| **F2** | Reflexion Loop | Inject recent unresolved mistakes into the prompt (F2a); consolidate ≥3 same-category mistakes into one semantic rule (F2b) | `channel_reply.rs`, `reflexion.rs`, `MistakeNotebook` |
| **F3** | Batch Fetch | Fetch up to 100 memory entries by ID in one call, with `missing_ids` for partial hits | `engine.rs` — `get_by_ids`; MCP `memory_fetch_batch` |

All three were implemented on the live engine — the migration is an **idempotent ALTER loop**, not a rebuild.

---

## F1: Temporal Memory

### New columns (idempotent migration)

The migration loop adds nine nullable / constant-default columns so `ALTER TABLE ... ADD COLUMN` is legal on existing rows, plus two indexes:

| Column | Meaning |
|--------|---------|
| `valid_from` | When the fact became true (NULL ⇒ fall back to `timestamp`) |
| `valid_until` | When it stopped being true (NULL ⇒ still valid) |
| `superseded_by` | The id of the row that replaced this one |
| `supersedes` | The id of the row this one replaced |
| `subject` / `predicate` / `object` | Knowledge-graph triple |
| `confidence` | 0.0–1.0, defaults to 1.0 |
| `metadata` | JSON blob, defaults to `{}` |

```sql
-- F1 Temporal Memory columns (v1.19.0) — all nullable / constant-default
ALTER TABLE memories ADD COLUMN valid_from    TEXT;
ALTER TABLE memories ADD COLUMN valid_until   TEXT;
ALTER TABLE memories ADD COLUMN superseded_by TEXT;
ALTER TABLE memories ADD COLUMN supersedes    TEXT;
ALTER TABLE memories ADD COLUMN subject       TEXT;
ALTER TABLE memories ADD COLUMN predicate     TEXT;
ALTER TABLE memories ADD COLUMN object        TEXT;
ALTER TABLE memories ADD COLUMN confidence    REAL NOT NULL DEFAULT 1.0;
ALTER TABLE memories ADD COLUMN metadata      TEXT NOT NULL DEFAULT '{}';

-- Triple index only covers currently-valid rows (cheap conflict lookup)
CREATE INDEX IF NOT EXISTS idx_memories_triple
    ON memories(agent_id, subject, predicate) WHERE valid_until IS NULL;
CREATE INDEX IF NOT EXISTS idx_memories_valid
    ON memories(agent_id, valid_until);
```

The loop swallows `duplicate column name` errors, so re-running on an already-upgraded database is a no-op.

### Automatic conflict resolution

When `store_temporal(entry, TemporalMeta)` is called with **both** a `subject` and a `predicate`, the engine treats `(agent_id, subject, predicate)` as a fact identity. Any currently-valid row with the same triple is closed out before the new row is inserted:

```
store_temporal(agent="dudu",
               subject="user", predicate="deploy_target",
               object="Cloudflare Workers")
     |
     v
Look up currently-valid row for (dudu, user, deploy_target)
     |
   found? ──no──> just INSERT new row (valid_until = NULL)
     |
    yes
     |
     v
UPDATE old row:  valid_until = now
                 superseded_by = <new id>
     |
     v
INSERT new row:  supersedes = <old id>
                 valid_until = NULL   (currently valid)
```

The two rows are now linked into a **supersession chain**:

```
[ deploy_target = Vercel ]      [ deploy_target = Cloudflare Workers ]
  valid_from  : Jan 1            valid_from  : Mar 3
  valid_until : Mar 3   ───────► valid_until : NULL  (current)
  superseded_by ──────────┘      supersedes ─────────┘
```

Without a full triple, `store_temporal` simply records a timestamped fact — no supersession.

### Default-filter to "currently valid"

`search()` / `search_layer()` add `AND (m.valid_until IS NULL OR m.valid_until > now)` to every query, so ordinary retrieval only ever returns facts that are true *right now*. Stale facts stay in the database for history but never leak into a prompt.

### Reading the timeline

Two read APIs expose the chain:

| API | Returns |
|-----|---------|
| `get_history(agent, subject, predicate)` | The full supersession chain, oldest → newest |
| `get_at(agent, subject, predicate, at)` | The single fact valid at a point in time (`valid_from <= at AND (valid_until IS NULL OR valid_until > at)`) |

---

## F2: The Reflexion Loop

F2 bridges the **existing** `MistakeNotebook` into the answering path — it is not a new store. The trigger signal is the existing `ErrorCategory` (Significant / Critical, MetaCognition-adaptive) — **not** the GVU Verifier, which validates SOUL.md proposals.

### F2a — Inject past mistakes into the prompt

Before the agent answers a channel message, its recent unresolved mistakes are surfaced into the prompt under a `## Past Mistakes to Avoid` header:

```
Channel message arrives
     |
     v
Extract whitespace keywords (≥3 chars, up to 12)
     |
   keywords? ──no──> query_by_agent(agent, 3)   ← CJK recency fallback
     |                                              (CJK has no whitespace tokens)
    yes
     |
     v
query_by_topic(keywords, agent, 3)   ← topic-scoped recall
     |
   empty? ──yes──> query_by_agent(agent, 3)   ← recency fallback
     |
     v
Append to prompt:
  ## Past Mistakes to Avoid
  - <mistake 1 prompt section>
  - <mistake 2 prompt section>
```

This bridges `MistakeNotebook` → cross-task learning, so the agent stops repeating past failures on similar topics — not just inside the GVU SOUL.md path.

### F2b — Consolidate ≥3 same-category mistakes into one rule

When the same `MistakeCategory` accumulates `>= DEFAULT_CONSOLIDATE_THRESHOLD` (= **3**) unresolved entries, `reflexion::maybe_consolidate` synthesizes them into a single **semantic** memory rule, then marks the sources resolved:

```
Unresolved mistakes for agent, grouped by MistakeCategory
     |
     v
count_unresolved_by_category(agent, Capability) = 3
     |
   < 3? ──yes──> do nothing
     |
   >= 3
     |
     v
query_unresolved_by_category(...)  → MistakeEntry[]
     |
     v
synthesize_rule(category, mistakes)   ← deterministic, no LLM call
  "Recurring capability issues consolidated from 3 past mistakes.
   Apply extra care: ..."
     |
     v
store as ONE semantic memory   (source_event = "reflexion_consolidation")
     |
     v
mark_resolved(source ids)   ← the three originals are now resolved
```

The synthesis is **detached and deterministic** — no LLM round-trip. Three scattered incidents collapse into one standing rule the agent reads going forward.

```
Before:                         After:
  ☒ mistake A (capability)        ✓ A resolved ─┐
  ☒ mistake B (capability)  ───►  ✓ B resolved ─┼─► 1 semantic rule
  ☒ mistake C (capability)        ✓ C resolved ─┘   "Apply extra care: ..."
```

---

## F3: Batch Fetch (`memory_fetch_batch`)

Reconstructing context often means pulling many specific entries by id. Doing that one MCP call at a time is slow and chatty. `get_by_ids` (engine) and the `memory_fetch_batch` MCP tool fetch up to **100** entries in a single call:

```
memory_fetch_batch { "ids": ["m_1", "m_2", "m_404", ...] }   (max 100)
     |
     v
get_by_ids(namespace, ids)
  SELECT ... FROM memories WHERE agent_id = ? AND id IN (?,?,?...)
     |  (namespace / ownership enforced — entries in another
     |   namespace are indistinguishable from non-existent)
     v
Partition requested ids:
  found    → memories[]
  missing  → missing_ids[]   ← NOT an error
     |
     v
{ "memories": [...], "missing_ids": ["m_404"],
  "total_found": N, "total_missing": M }
```

Key properties:

- **Hard cap of 100** — `ids` over 100 is rejected, preventing runaway queries.
- **Partial hits are not errors** — found entries come back alongside a `missing_ids` list.
- **No existence leak** — an entry belonging to another namespace and a non-existent id both land in `missing_ids`. The caller can't probe what other agents own.

---

## Configuration

There is nothing to turn on. Memory Intelligence rides on the existing memory engine:

- **F1** activates the moment a caller passes a `subject` + `predicate` to `store_temporal`; plain stores are unchanged.
- **F2a** fires whenever `ctx.mistake_notebook` is present on the channel-reply path.
- **F2b** uses `DEFAULT_CONSOLIDATE_THRESHOLD = 3`.
- **F3** is exposed as the `memory_fetch_batch` MCP tool, scope-gated like every other memory tool.

The migration runs automatically at engine init — existing databases are upgraded in place by the idempotent ALTER loop.

---

## Why This Matters

### Facts stop going stale silently

Before F1, a memory said "deploy target is Vercel" forever, even after the user moved to Cloudflare. Now the old fact is closed out, the new one takes over, and ordinary search only ever returns what's true *now* — while history stays queryable via `get_history` / `get_at`.

### Mistakes compound into competence

F2 closes the loop between the prediction engine's error signal and the agent's future behavior. A mistake isn't just logged — it's surfaced on similar topics (F2a) and, once it recurs, hardened into a standing semantic rule (F2b). The agent gets better without the model changing.

### Recall without the round-trip tax

F3 turns N chatty MCP calls into one, with a clean partial-hit contract and no cross-namespace leakage. Context reconstruction becomes cheap.

### Non-invasive by design

None of this required a schema rewrite or a new `MemoryEntry`. Nine nullable columns, two indexes, an idempotent migration, and a notebook that already existed. The whole feature stacks onto the live engine.

---

## The Takeaway

A flat pile of notes forgets nothing and learns nothing. A good chart does both: it timestamps facts so the old ones retire gracefully, it turns repeated mistakes into standing protocol, and it lets you pull the exact pages you need in one reach. Memory Intelligence gives every DuDuClaw agent that chart — built on the memory engine it already had.
