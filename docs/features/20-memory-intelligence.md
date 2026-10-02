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

When `store_temporal(entry, TemporalMeta)` is called with **both** a `subject` and a `predicate`, the engine treats `(agent_id, subject, predicate)` as a fact identity. Any currently-valid row with the same triple is closed out before the new row is inserted, unless the supersession trust guard refuses the write because the current fact is more trusted (see [below](#supersession-trust-guard-v1671)):

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

Since v1.67.1 a caller treated as an AI employee may only invalidate the `channel`, `mcp_external` and `tool_echo` classes, the ones below the agent-derived ceiling. Any other origin is refused and audited as `memory_invalidate_refused`. The check fails closed: any caller on the gateway's shared internal key counts as an AI employee, whether or not an employee identity is present in the process, and so does a key that belongs to an employee or to an ephemeral employee (`eph-` ids). Only an admin key that maps to no employee is unrestricted. Before, a steered AI employee could expire every operator-level fact in its namespace in one call (which namespace that is: see the known limit below).

### Write-side poison protection (D2)

D1 lets you *undo* a poisoned source; D2 stops most poison from landing in the first place (PoisonedRAG, arXiv:2402.07867). The auto-distillation write path is guarded at two ends:

- **Write-side scan + burst detection.** Before a distilled fact is stored, its content and `(subject, predicate, object)` are run through the shared prompt-injection rule engine — a match **drops** the fact (fail-closed, never written) and records a `prompt_injection` security-audit event. Separately, a per-`(agent, origin, subject)` sliding-window counter (`knowledge_guard`, same durable + advisory-locked pattern as the dispatch breaker) quarantines a batch when one origin writes `>= max_per_subject` facts about the same subject inside the window (the "One Shot Dominance" / k-doc pattern). Quarantined facts are stored with `quarantined = 1` — **inert**: they never supersede a clean fact and are excluded from every retrieval read path (FTS, graph, vector, `list_recent`, `summarize`) until a human decides.
- **Processing.** A quarantine raises an `ApprovalBroker` request (`action_kind = "knowledge_quarantine"`, 24-hour deadline) and emits a `knowledge.quarantined` event. Approve → each fact is released through the normal temporal rules and the supersession trust guard below: it becomes current (or reaffirms, or lands as history), and a fact that a more trusted current fact outranks is turned into a held claim with its own review item instead of being applied (v1.67.1; before, approval only cleared the flag). A burst fact that the guard would refuse at write time goes to a held claim directly instead of into the burst batch. A converted row whose text is over 600 characters gets no review item and is audited as `memory_supersession_refused` with `not_held_reason: "too_long"`. Approving a burst item again after a partial failure files the conflict items that are still missing (only conflict items count when checking whether one already exists). Deny → they are expired (`invalidated_by_event = "quarantine_reject"`) and their `origin_trust` floored to ≤ 0.1; TTL expiry counts as deny (fail-closed).

**Ranking-side trust.** `origin_trust` now participates in retrieval ranking (weight `w_trust`, default 0.10): each candidate's score is multiplied by `(1 − w_trust) + w_trust · origin_trust`, so an unverified channel-distilled fact (trust 0.3) can't outrank a curated one (trust 1.0). In the HippoRAG-lite graph, a triple's edges are weighted by its `origin_trust`, shrinking a low-trust fact's Personalized-PageRank mass — this directly damps the "single poisoned triple amplified two hops by PPR" path. Legacy rows (trust 1.0) rank byte-identically to the pre-D2 path.

### Supersession trust guard (v1.67.1)

Before v1.67.1 a new fact for the same `(agent, subject, predicate)` always replaced the current one, whatever its source, so a fact distilled from a chat conversation (trust 0.3) replaced whatever was current for the same subject and predicate regardless of its trust: for example a fact the AI employee derived itself or a row with no recorded origin (0.6), or an imported fact (0.7) when the keys matched. Operator-approved facts (1.0) exist only from v1.67.1 on, written by the review approval below, and are protected by this check. `store_temporal` now compares trusts before it supersedes (`duduclaw-memory/src/supersession_guard.rs`):

- The write's trust is its effective `origin_trust` (after the origin-class ceiling and the `derived_from` clamp). The current fact's trust is its stored `origin_trust` capped at its class ceiling; a row written before origin binding reads as `unattributed` (0.6). Corroboration raises `confidence`, never trust, so it does not count.
- If any currently valid, non-quarantined row for the triple has strictly higher trust than the write, nothing is written. Equal or higher trust supersedes as before, and the old row stays in the history chain.
- Reaffirming the same object, writes without a full triple, and out-of-order historical segments (an older `valid_from`) are not checked.
- The guard compares exact `subject` and `predicate` strings. The same fact stored under a differently spelled subject or predicate is a different triple; the two coexist, as before.

Origin ceilings (`origin.rs`): `user_direct` (and the legacy alias `user`) 1.0, `operator` 1.0, `import` 0.7, `agent_derived` 0.6, `user_profile` 0.6, `unattributed` 0.6, `tool_echo` 0.5, `channel` 0.3, `mcp_external` 0.3. `user_profile` became its own class in v1.67.1: it was an alias of `user_direct` at 1.0. Both the `user_profile_record` MCP tool and profile distillation of the speaker's own statements now write `user_profile` (distillation wrote `channel`, 0.3, before), so the AI employee's record and the user's later statement can correct each other without review, and neither can replace an operator-approved value.

What each write path does with a refusal:

| Path | On refusal |
|---|---|
| Conversation fact distillation (`wiki_ingest`, origin `channel`) | Held for review (below) |
| User-profile trait distillation (`profile_distill`, origin `user_profile`) | Held for review (below) |
| `user_profile_record` MCP tool | Returns an error ("a more trusted value already exists for this field, so it was not changed"); nothing is written |
| `duduclaw migrate-from` | The item is reported as skipped |
| Footprint distillation, reflexion rule consolidation, night-engine schemas and consolidation | The item is skipped and logged |
| Any other `store_temporal` caller | Receives an error naming both trusts |

**Held claims.** In the two distillation paths a refused claim is stored inert (`quarantined = 1`, its triple kept only in `metadata.held_claim`, invisible to retrieval) and files a `knowledge_quarantine` review item in the dashboard inbox (收件匣). The item is built only from the stored held row and shows everything approval would write: the full new statement and the new value, side by side with the current content and current value; for a user profile it names whose profile it is (the user id). Current content longer than 600 characters is cut on the item, with a note saying so. A statement longer than 600 characters is not queued; the refusal is only audited (`not_held_reason: "too_long"`). An identical claim (same subject, predicate and object) already pending is not held again. At most 20 new held claims per AI employee per UTC day. The first refusal over that limit on a UTC day adds one Activity Feed event (`knowledge_review_cap_reached`); every refusal over the limit is only written to the audit log (`memory_supersession_refused` with `review_cap_hit: true`, `not_held_reason: "daily_cap"`).

- Approve: the stored claim the item was built from is written with operator authority (origin `operator`) and replaces the current fact. The item carries a digest of that claim (content, subject, predicate, object). If the claim or the protected fact changed after the item was filed, nothing is written, the result says the situation changed, and the held row is closed; the claim is held again if it appears again.
- The change is applied before the decision is recorded, and decisions on the same item are processed one at a time. If applying it fails, the item stays pending, the dashboard shows the server's error, and the decision can be retried. If the change was applied but the decision could not be recorded (the item expired in between, or a store error), the dashboard gets an error that says what was applied, and an audit row `knowledge_review_decision_unrecorded` is written.
- Deny: the claim is discarded.
- An item past its 24-hour deadline cannot be approved; expiry counts as deny.
- These items are decided in the dashboard only, by an account with the manager or admin role (`approvals.decide`). Chat channels receive a plain notice with no buttons and no claim text, never sent to the conversation the claim came from; an old button press or a text reply is refused with a pointer to the dashboard. The Telegram Mini App detail view shows only that notice for these items. Before v1.67.1 a channel button for a knowledge review marked the approval decided but did not release anything.
- A daily sweep (run with memory decay) closes quarantined rows, held claims and burst batches alike, whose review item is no longer pending and that are more than an hour old, as a rejection.
- Data-subject export and erase (`gdpr.rs`) also match a held claim's subject and object, and an erased held claim can no longer be approved. `duduclaw gdpr erase <contact> --confirm` also withdraws pending review items that cover an erased row, replaces the text of every such item in the review store whatever its state, and deletes the matching `knowledge.quarantined` events. Events that carry no row id (for example injection drops) cannot be matched and stay until the 7-day event retention removes them. Every step runs even if an earlier one fails; failures are listed and the command exits non-zero, saying to re-run the same command, which is safe to repeat. A re-run that finds no memory rows left still removes the person's text from review items and events by exact subject match.
- Scheduled and system prompts (pseudo-user `system`) no longer write a user profile. `user_profile_record` refuses pseudo-users (`system`, `anonymous`, `unknown`), and scans both the predicate and the value for prompt injection, refusing on a block-level hit.

`config.toml [memory] supersession_trust_guard` (default `true`) switches the guard off when set to `false`. It is read by engines built through the gateway's `memory_factory::build_memory_engine` and by the `duduclaw mcp-server` memory engine. Engines constructed directly (for example the dashboard's memory RPCs and `duduclaw migrate-from`) keep the guard on regardless of the setting. Not verified on a real chat channel.

### Known limit: two memory namespaces (not fixed in v1.67.1)

Memory that an AI employee writes or reads through the MCP memory tools (`memory_store`, `memory_search`, `memory_read`, `memory_fetch_batch`, `memory_alias_add` / `memory_alias_list`, `memory_get_history`, `memory_get_at`, `memory_invalidate_by_origin`, `user_profile_record`, `user_profile_get`, `user_code_profile`) lives in a namespace derived from the MCP key. Every employee the gateway spawns uses the gateway's internal key, so they all share one namespace, `internal/gateway-internal` (so since v1.44.0, when that key was introduced). Everything the gateway does itself (conversation and profile distillation, review approval, injecting key facts and the profile block into prompts) uses the employee's own id. Consequences:

- Employees of the same gateway share the memory they store through those tools.
- What an employee stores through the tools is not what the gateway injects into its prompts, and its `memory_search` does not see what the gateway distilled. The profile block reflects distilled traits and approved reviews, not `user_profile_record` calls.
- The trust guard compares facts within one namespace. It protects gateway-distilled and operator-approved facts from chat-derived writes, and does not arbitrate between the two pools.

Isolation between employees holds for gateway-written memory only. Changing this needs a data migration and has not been scheduled.

### Auto-filed knowledge pages (WP5c)

Conversation distillation now has a second sink: durable reference documents
(charter / SOP / spec / policy) become a wiki page under the agent's `auto/`
namespace instead of a pile of memory rows. The trust model is deliberately
shared rather than parallel — the page's frontmatter `trust` is `0.300`, the
same ceiling `origin.rs` gives the `channel` class, and its `source_type` is
`raw_dialogue` (ranking factor 0.6). A caller cannot raise either; promotion to
curated trust is a human action in the curation station.

Memory keeps exactly one pointer row per page —
`subject = wiki:auto/<doc_type>/<slug>`, `predicate = documented_in`, origin
`channel`, trust 0.3 — so the document's full text lives in one place while
supersession, rollback and retrieval still work on the memory side. Removing a
page expires that pointer by **exact subject** (`expire_by_subject`), never via
`invalidate_by_origin`, which would take every conversationally-learned memory
with it. See [17 — Wiki Knowledge Layer](17-wiki-knowledge-layer.md#auto-filed-pages-auto-namespace-wp5c).

### Graph retrieval evolution (D3)

The HippoRAG-lite graph gained four independent refinements (HippoRAG 2 + LightRAG alignment). Each is fail-safe: with no aliases, a small graph, and embedding seeding off, ranking is **byte-identical** to the earlier per-query build.

- **Persistent incremental graph cache.** Rebuilding the Personalized-PageRank graph on every query is wasteful once an agent accumulates many facts. The graph is now cached per agent (`RwLock`) and reused across queries; a per-agent **generation counter** — bumped by every triple-mutating write (`store_temporal`/supersession, quarantine release/reject, origin purge, decision expiry, decay archival, GDPR erase, agent reassignment) — invalidates a stale cache so a query always sees current facts. The cache only engages above `GRAPH_CACHE_MIN_TRIPLES` (500); below that the per-query build is cheaper and is kept.
- **Entity alias merging.** An `entity_alias(agent_id, canonical, alias)` table folds surface forms onto one node before the graph is built and seeded, so "老闆 / 李老闆 / zhixu" stop being three isolated islands. Both sides are normalized (trim + lowercase) and alias chains are flattened on store. Managed via the `memory_alias_add` / `memory_alias_list` MCP tools (write / read scope). With no aliases the graph is byte-identical.
- **Predicate edge labels.** Each SPO edge now carries its predicate as an attached label (the PPR math never reads it, so ranking is unchanged). The `engine.export_graph(agent, limit)` API returns a serializable `{ nodes, edges }` snapshot — including quarantined-but-pending facts, flagged — for the D6 knowledge-graph curation UI.
- **Embedding seeding (opt-in).** When `graph_embed_seed` is on **and** an embedder is attached, PPR seeds become the union of whole-word FTS entity matches and the query embedding's nearest entity vectors (same-model cosine, top-k). Entity vectors are cached lazily in `entity_embedding` and embedding failures fall back to FTS seeding. Off by default (and a no-op with no embedder), following HippoRAG 2's caution that a weak embedder loses recall.

---

## F2: The Reflexion Loop

F2 bridges the **existing** `MistakeNotebook` into the answering path — it is not a new store. The trigger signal is the existing `ErrorCategory` (Significant / Critical, MetaCognition-adaptive) — **not** the evolution engine's Gate/Measure verifier, which judges candidate playbook entries.

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

This bridges `MistakeNotebook` → cross-task learning, so the agent stops repeating past failures on similar topics — not just inside the evolution engine.

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
enabled = true          # master switch for the same-origin burst detector (default true)
window_secs = 3600      # sliding window length in seconds (default 3600 = 1 hour)
max_per_subject = 5     # facts one origin may write per subject inside the window; beyond this, quarantine (default 5)
```

The supersession trust guard is on by default:

```toml
[memory]
supersession_trust_guard = true   # false = a lower-trust write may replace a more trusted current fact again (pre-v1.67.1 behaviour)
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
