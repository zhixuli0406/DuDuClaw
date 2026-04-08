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

**Episodic Memory** — Records of specific interactions, tagged with:
- Timestamp (when did this happen?)
- Context (which channel? which user? what topic?)
- Emotional valence (was this positive, negative, or neutral?)

Example entries:
```
[2026-04-05 14:30] User asked about Rust lifetimes in Discord.
  Struggled with 'static lifetime. Explained with analogy.
  Interaction: positive (user said "that makes sense!")

[2026-04-06 09:15] User reported a bug in the billing module.
  Root cause: null check missing in invoice calculation.
  Interaction: negative (user frustrated, issue critical)
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
     +---> Relevance: How semantically close is this to the query?
             (Embedding distance or keyword overlap)
     |
     v
Final score = weighted combination of all three
     |
     v
Return top-N memories, sorted by score
```

The weights between the three dimensions are configurable. An agent that handles urgent support tickets might weight Recency and Importance higher. An agent that serves as a knowledge base might weight Relevance higher.

This approach is inspired by the Stanford **Generative Agents** research paper, which demonstrated that this 3D retrieval produces more human-like memory recall than simple keyword search.

### Memory Decay: Forgetting Curves

Not all memories should live forever. The system implements **spaced-repetition forgetting curves**:

```
Memory created
     |
     v
  Initial strength: 1.0
     |
     v
  Time passes without access...
     |
     v
  Strength decays: 0.8 → 0.6 → 0.4 → 0.2
     |
     v
  Below threshold? → Mark as "faded"
     (Still exists, but won't surface in normal retrieval)
     |
     v
  If accessed again → Strength resets to 1.0
     (The memory is "refreshed" and starts decaying again)
```

The decay rate varies by importance:
- **Critical memories** (security incidents, key decisions): Slow decay, high threshold
- **Important memories** (user preferences, recurring topics): Medium decay
- **Casual memories** (greetings, small talk): Fast decay, low threshold

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

### Vector Index

For semantic search (finding memories that are *conceptually* similar, even if they don't share keywords), the system maintains a vector index:

```
Query: "invoice calculation error"
     |
     v
Convert to embedding vector
     |
     v
Find nearest neighbors in vector space
     |
     v
Results include memories about:
  - "billing module null check" (semantically related)
  - "price rounding issue in orders" (similar domain)
  - "tax calculation edge case" (conceptually adjacent)
```

The vector index catches connections that keyword search misses — because "invoice calculation error" and "billing module null check" share no keywords, but are clearly about related topics.

---

## Federated Memory: Cross-Agent Knowledge Sharing

In a multi-agent setup, agents sometimes need to access each other's knowledge — but not everything. The federated memory system provides controlled sharing:

```
Agent A (customer support) needs product info
     |
     v
Query Agent B's (product specialist) memory
     |
     v
Privacy check:
  Is this memory marked as shareable?
     |
  +--+--+
  |     |
 Yes    No
  |     |
  v     v
Return  Access
result  denied
```

Memories have sharing levels:
- **Private**: Only the owning agent can access
- **Team**: Agents in the same group can access
- **Public**: Any agent can access

This mirrors how organizations handle information: some knowledge is department-specific, some is company-wide, and some is need-to-know.

---

## Wiki Knowledge Base

Beyond conversational memory, the system supports structured knowledge ingestion:

```
External knowledge source (URL, document, wiki)
     |
     v
Ingest pipeline:
  - Parse content
  - Extract structured data
  - Index for full-text search
  - Generate embeddings for vector search
     |
     v
Knowledge base (queryable by all agents)
```

The web dashboard includes an interactive **knowledge graph visualization** that shows how different pieces of knowledge are connected — topics, entities, and their relationships.

---

## Why This Matters

### Personalized Interactions

An agent with memory doesn't start every conversation from scratch. It remembers user preferences, past issues, and communication style. This transforms the experience from "talking to a new person every time" to "talking to someone who knows you."

### Knowledge Accumulation

Over time, the agent builds a rich understanding of its domain. A support agent accumulates knowledge about common issues, known workarounds, and user-specific configurations. This knowledge persists across sessions and improves response quality over time.

### Scalable Memory

The forgetting curve ensures memory doesn't grow without bound. The system naturally maintains a working set of relevant, recent memories while allowing old, unimportant memories to fade. No manual cleanup needed.

### Cross-Agent Intelligence

Federated memory means knowledge doesn't stay siloed. A product insight learned by one agent can benefit the support agent, the sales agent, and the documentation agent — all automatically, with privacy controls.

---

## Interaction with Other Systems

- **Evolution Engine**: Memory patterns inform the prediction engine's accuracy.
- **Session Manager**: Conversation history flows into episodic memory.
- **Wiki Ingestion**: Structured knowledge feeds into the semantic memory store.
- **Dashboard**: Memory contents, search interface, and knowledge graph are all accessible through the web interface.

---

## The Takeaway

Memory is what separates a stateless chatbot from a useful assistant. By modeling memory after human cognition — episodic/semantic separation, importance-weighted retrieval, natural forgetting, and controlled sharing — DuDuClaw gives agents the ability to learn, remember, and grow from every interaction.
