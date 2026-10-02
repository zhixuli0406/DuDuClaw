# Cognitive Memory System

> Human-inspired memory with forgetting curves — the agent remembers what matters and forgets what doesn't.

---

## The Metaphor: How Your Brain Organizes Memories

Think about how you remember things:

- **"I had coffee with Sarah last Tuesday and she mentioned she's switching jobs."** — This is an **episodic memory**: a specific event at a specific time.
- **"Sarah works in marketing."** — This is a **semantic memory**: a general fact, detached from any specific event.

Your brain naturally separates these. When someone asks "What does Sarah do?", you don't replay every conversation you've had with her — you just access the semantic fact directly.

And over time, unimportant episodic memories fade. You don't remember what you had for lunch two Tuesdays ago. But you remember the lunch where your boss told you about the promotion — because it was *important*.

DuDuClaw's memory system mirrors this architecture.

---

## How It Works

### Two Memory Stores

**Episodic Memory** — Records of specific events, each stored with:
- Timestamp (when did this happen?)
- Tags and source event (what produced it, e.g. a prediction observation)
- Importance (0–10) and access history (how often and how recently it was recalled)

Example entries:
```
[2026-04-05 14:30] User asked about Rust lifetimes in Discord.
  Struggled with 'static lifetime. Explained with analogy.
  importance: 4

[2026-04-06 09:15] User reported a bug in the billing module.
  Root cause: null check missing in invoice calculation.
  importance: 8
```

**Semantic Memory** — Distilled facts and knowledge, without temporal context:
```
User is a backend developer focused on Rust.
User prefers analogy-based explanations.
The billing module has a history of null-related bugs.
```

### Memory Retrieval: 3D-Weighted Search

When the agent needs to recall something, it doesn't just search by keyword. It uses three dimensions to rank memory relevance:

```
Query: "Help me with a Rust lifetime issue"
     |
     v
For each memory entry, compute:
     |
     +---> Recency: How recently was this memory created/accessed?
     |       (Recent memories score higher)
     |
     +---> Importance: How significant was this event?
     |       (Critical decisions > casual chat)
     |
     +---> Relevance: How closely does it match the query?
             (Full-text keyword rank)
     |
     v
Final score = weighted combination of all three
     |
     v
Return top-N memories, sorted by score
```

The weights are fixed defaults in the engine (recency 0.25, importance 0.35, keyword relevance 0.35); they are not configured per agent. Two extra signals can add to the score: a knowledge-graph signal (0.15, Personalized PageRank over stored subject–predicate–object facts) and a vector-similarity signal (0.15, when an embedder is attached). Each score is then scaled by how trusted the memory's origin is (weight 0.10).

This approach is inspired by the Stanford **Generative Agents** research paper, which demonstrated that this 3D retrieval produces more human-like memory recall than simple keyword search.

### Memory Decay: Forgetting Curves

Not all memories should live forever. The recency score follows an **Ebbinghaus forgetting curve**, and a daily job archives memories that have faded:

```
Memory created
     |
     v
  Retrievability R = exp(-t / S)
  (t = days since last access, R starts at 1.0)
     |
     v
  Time passes without access...
     |
     v
  R decays toward 0
     |
     v
  Older than 30 days, importance below 3,
  not semantic, and R below 0.05?
     → Moved to the archive
     (No longer returned by retrieval;
      deleted after 90 days in the archive)
     |
     v
  If accessed again → t resets, so R is back to 1.0,
     and stability S grows with every access
```

Stability `S` depends on importance and recall:
- **Higher importance** (up to 2× at importance 10): slower decay; importance 3 and above is never archived
- **Frequent recall**: `S` grows with `ln(1 + access_count)`, capped at 365 days
- **Low importance, never recalled**: fastest decay (base stability 14 days, scaled down)
- **Semantic memories** are never archived

This prevents the memory store from growing unboundedly. Old, unimportant memories naturally fade away, keeping the retrieval system fast and focused.

---

## Full-Text Search

For direct keyword searches, the system uses full-text search capabilities built into the database:

```
User: "Find everything about the billing bug"
     |
     v
Full-text search index scans all memory content
     |
     v
Returns matches ranked by relevance
  - "User reported a bug in the billing module..."
  - "The billing module has a history of null-related bugs..."
  - "Fixed billing calculation for edge case..."
```

This complements the 3D-weighted search: full-text search is for when you know *what* you're looking for; 3D-weighted search is for when you need contextually appropriate recall.

### Vector Similarity

For finding memories that are similar even when they don't share whole keywords, the engine can compare embedding vectors:

```
Query: "invoice calculation error"
     |
     v
Convert to embedding vector
     |
     v
Cosine similarity against the agent's embedded memories
     |
     v
Results include memories about:
  - "billing module null check" (semantically related)
  - "price rounding issue in orders" (similar domain)
  - "tax calculation edge case" (conceptually adjacent)
```

The shipped embedder is a local character n-gram hashing embedder (no model download), so it matches overlapping word fragments, including CJK text, rather than meaning in the way a neural embedding model does. The comparison is a brute-force scan; there is no separate vector index. It is attached on the gateway's memory engines when `[memory] novelty_gate` is on (the default), and on the MCP memory tools only when `DUDUCLAW_SEMANTIC_VECTORS=1` is set.

---

## Cross-Agent Knowledge Sharing

Memory is per agent. The memory tools (`memory_search`, `memory_store`, `memory_read`, …) only read and write the calling agent's own namespace; there are no per-memory sharing levels. Knowledge that several agents need goes into the shared wiki instead:

```
Agent A (customer support) needs product info
     |
     v
Search the shared wiki (wiki_search scope="shared")
     |
     v
Visibility check:
  Does wiki_visible_to allow this agent?
     |
  +--+--+
  |     |
 Yes    No
  |     |
  v     v
Return  Not
result  visible
```

Knowledge lives at two levels:
- **Agent memory and agent wiki**: Only the owning agent
- **Shared wiki** (`~/.duduclaw/shared/wiki/`): Agents allowed by their `wiki_visible_to` capability

This mirrors how organizations handle information: some knowledge is department-specific, some is company-wide, and some is need-to-know.

---

## Wiki Knowledge Base

Beyond conversational memory, the system keeps structured knowledge as wiki pages:

```
Knowledge source
  (wiki_write by an agent, operator edits,
   reference documents auto-filed from conversation)
     |
     v
Wiki page:
  - Markdown with frontmatter
  - Agent-local or shared scope
  - Indexed for full-text search
     |
     v
Knowledge base (searchable with wiki_search)
```

The dashboard's Knowledge Hub page includes a **relationship graph** that shows how wiki pages connect through shared topics. See [wiki knowledge layer](17-wiki-knowledge-layer.md).

---

## Why This Matters

### Personalized Interactions

An agent with memory doesn't start every conversation from scratch. It remembers user preferences, past issues, and communication style. This transforms the experience from "talking to a new person every time" to "talking to someone who knows you."

### Knowledge Accumulation

Over time, the agent builds a rich understanding of its domain. A support agent accumulates knowledge about common issues, known workarounds, and user-specific configurations. This knowledge persists across sessions and improves response quality over time.

### Scalable Memory

The forgetting curve ensures memory doesn't grow without bound. The system naturally maintains a working set of relevant, recent memories while allowing old, unimportant memories to fade. No manual cleanup needed.

### Cross-Agent Intelligence

The shared wiki means knowledge doesn't stay siloed. A product insight written there by one agent can serve the support agent, the sales agent, and the documentation agent, within the visibility rules the operator sets.

---

## Interaction with Other Systems

- **Prediction engine**: Writes episodic observations (`source_event = prediction_episodic`) after channel replies.
- **Conversation distillation**: Facts from conversations become semantic memories; reference documents become wiki pages with a short pointer in memory.
- **Memory intelligence**: Temporal supersession, reflexion rules and origin trust build on this engine. See [memory intelligence](20-memory-intelligence.md).
- **Dashboard**: Memory contents, search, and the Knowledge Hub graph are accessible through the web interface.

---

## The Takeaway

Memory is what separates a stateless chatbot from a useful assistant. By modeling memory after human cognition — episodic/semantic separation, importance-weighted retrieval, natural forgetting, and a shared knowledge base — DuDuClaw gives agents the ability to learn, remember, and grow from every interaction.
