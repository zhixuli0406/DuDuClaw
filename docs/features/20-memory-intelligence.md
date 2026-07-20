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

Two read APIs expose the chain, both also available over MCP as `memory_get_history` / `memory_get_at` (scope `memory:read`):

| API / MCP tool | Returns |
|-----|---------|
| `get_history(agent, subject, predicate)` — `memory_get_history { subject, predicate }` | The full supersession chain, oldest → newest, incl. per-record `ingested_at`, `invalidated_by_event`/`invalidated_at`, and `reaffirmed_by` |
| `get_at(agent, subject, predicate, at)` — `memory_get_at { subject, predicate, at }` | The single fact valid at a point in time (`valid_from <= at AND (valid_until IS NULL OR valid_until > at)`) |

### Bi-temporal + build-time provenance (D1)

The temporal store tracks **two** time axes: `valid_from`/`valid_until` (world-time — when a fact is true) and `ingested_at` (transaction-time — when the system learned it). Supersession is decided by world-time `valid_from`, not ingestion order, so scrambled ingestion (learning about a divorce before the earlier marriage) still resolves the correct fact at any point in time — a fact whose `valid_from` predates the current one is inserted as a bounded *historical segment* without disturbing the current fact. Re-observing an identical fact (same subject/predicate/object + content) **reaffirms** it — appending the new `source_event` to `reaffirmed_by` (capped at 20) and bumping `access_count` — instead of churning a new row. When a fact is closed out, the closing `source_event` and time are stamped onto the superseded row (`invalidated_by_event`/`invalidated_at`).

### Source rollback (`memory_invalidate_by_origin`)

`invalidate_by_origin(agent, origin, since)` — MCP `memory_invalidate_by_origin` (scope `admin`) — is the remediation valve for a poisoned source: it expires (never deletes) every currently-valid fact from an **exact** `origin` (equality, never substring), optionally limited to facts learned at/after `since`. Facts whose `derived_from` cites a purged id have their `origin_trust` floored to ≤ 0.1 (a derivation of poisoned input can't stay trusted). `search()` immediately stops returning the purged facts, while `get_history()` preserves the full chain with `invalidated_by_event = "origin_purge"`.

### Write-side poison protection (D2)

D1 lets you *undo* a poisoned source; D2 stops most poison from landing in the first place (PoisonedRAG, arXiv:2402.07867). The auto-distillation write path is guarded at two ends:

- **Write-side scan + burst detection.** Before a distilled fact is stored, its content and `(subject, predicate, object)` are run through the shared prompt-injection rule engine — a match **drops** the fact (fail-closed, never written) and records a `prompt_injection` security-audit event. Separately, a per-`(agent, origin, subject)` sliding-window counter (`knowledge_guard`, same durable + advisory-locked pattern as the dispatch breaker) quarantines a batch when one origin writes `>= max_per_subject` facts about the same subject inside the window (the "One Shot Dominance" / k-doc pattern). Quarantined facts are stored with `quarantined = 1` — **inert**: they never supersede a clean fact and are excluded from every retrieval read path (FTS, graph, vector, `list_recent`, `summarize`) until a human decides.
- **Processing.** A quarantine raises an `ApprovalBroker` request (`action_kind = "knowledge_quarantine"`) and emits a `knowledge.quarantined` event. Approve → the facts are released (`quarantined = 0`, now retrievable); deny → they are expired (`invalidated_by_event = "quarantine_reject"`) and their `origin_trust` floored to ≤ 0.1; TTL expiry counts as deny (fail-closed).

**Ranking-side trust.** `origin_trust` now participates in retrieval ranking (weight `w_trust`, default 0.10): each candidate's score is multiplied by `(1 − w_trust) + w_trust · origin_trust`, so an unverified channel-distilled fact (trust 0.3) can't outrank a curated one (trust 1.0). In the HippoRAG-lite graph, a triple's edges are weighted by its `origin_trust`, shrinking a low-trust fact's Personalized-PageRank mass — this directly damps the "single poisoned triple amplified two hops by PPR" path. Legacy rows (trust 1.0) rank byte-identically to the pre-D2 path.

### Graph retrieval evolution (D3)

The HippoRAG-lite graph gained four independent refinements (HippoRAG 2 + LightRAG alignment). Each is fail-safe: with no aliases, a small graph, and embedding seeding off, ranking is **byte-identical** to the earlier per-query build.

- **Persistent incremental graph cache.** Rebuilding the Personalized-PageRank graph on every query is wasteful once an agent accumulates many facts. The graph is now cached per agent (`RwLock`) and reused across queries; a per-agent **generation counter** — bumped by every triple-mutating write (`store_temporal`/supersession, quarantine release/reject, origin purge, decision expiry, decay archival, GDPR erase, agent reassignment) — invalidates a stale cache so a query always sees current facts. The cache only engages above `GRAPH_CACHE_MIN_TRIPLES` (500); below that the per-query build is cheaper and is kept.
- **Entity alias merging.** An `entity_alias(agent_id, canonical, alias)` table folds surface forms onto one node before the graph is built and seeded, so "老闆 / 李老闆 / zhixu" stop being three isolated islands. Both sides are normalized (trim + lowercase) and alias chains are flattened on store. Managed via the `memory_alias_add` / `memory_alias_list` MCP tools (write / read scope). With no aliases the graph is byte-identical.
- **Predicate edge labels.** Each SPO edge now carries its predicate as an attached label (the PPR math never reads it, so ranking is unchanged). The `engine.export_graph(agent, limit)` API returns a serializable `{ nodes, edges }` snapshot — including quarantined-but-pending facts, flagged — for the D6 knowledge-graph curation UI.
- **Embedding seeding (opt-in).** When `graph_embed_seed` is on **and** an embedder is attached, PPR seeds become the union of whole-word FTS entity matches and the query embedding's nearest entity vectors (same-model cosine, top-k). Entity vectors are cached lazily in `entity_embedding` and embedding failures fall back to FTS seeding. Off by default (and a no-op with no embedder), following HippoRAG 2's caution that a weak embedder loses recall.

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

### Write-side poison protection (D2)

The write-side burst detector is on by default and tunable in `config.toml`. Absent or malformed sections fall back to these defaults (fail-safe — the detector stays ON):

```toml
[knowledge_guard]
enabled = true          # master switch for the same-origin burst detector. 預設 true
window_secs = 3600      # 滑動窗長度（秒）。預設 3600（1 小時）
max_per_subject = 5     # 一個來源在窗內對同一 subject 可寫入的事實上限，超過即隔離。預設 5
```

The injection scan on the write path is unconditional (no config). Ranking trust weight `w_trust` (default 0.10) lives in `RetrievalWeights` (per-engine, not a config key); at `w_trust = 0.0` ranking is byte-identical to the pre-D2 path.

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
