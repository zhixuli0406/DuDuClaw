# Session Memory Stack

> Instruction Pinning, Snowball Recap, and a Key-Fact Accumulator — three cheap layers that replaced a 6,500-token heavyweight memory system.

---

## The Metaphor: A Chef's Sticky Notes

A head chef can memorize the recipe book, but during a dinner rush they don't. They rely on three quick-reference surfaces:

1. **A pinned ticket above the pass** — "Table 12: no shellfish, vegan desserts." Read on every plate leaving the kitchen.
2. **A running recap scribbled on the order slip** — "Step 3 done, sauce needs salt." Re-read between stirs.
3. **A small card box by the prep counter** — notes accumulated over weeks ("Chef Park hates parsley garnishes"). Pulled only when a card is relevant.

None of these surfaces are the recipe book itself. They are *cheap, load-bearing surfaces* placed exactly where the chef's attention passes anyway. DuDuClaw's session memory stack is built on the same idea.

---

## The Problem Being Solved

DuDuClaw briefly shipped a MemGPT-inspired 3-layer memory system (Core Memory, Recall Memory, Archival Bridge). It worked, but:

- **6,500 tokens of bloat per prompt** — even short conversations paid the full memory tax.
- **"Lost in the middle" attention degradation** — long injected blocks degraded response quality instead of improving it.
- **MCP tool plumbing required manual invocation** — `core_memory_append`, `recall_search`, etc. — which the agent often forgot to call.

v1.8.1 removed all 1,985 lines of it. v1.8.6 replaced it with three lightweight surfaces that together are ~87% cheaper and sit in positions the model already attends to.

---

## Layer 1: Instruction Pinning (v1.8.6 P0)

The first user message in a session usually contains the *core task*. Everything after is clarification. So:

```
Turn 1: "Help me migrate this React app from CRA to Vite,
         keep the existing tests, and don't touch the auth flow."
     |
     v
Async Haiku extraction:
   "migrate React app CRA → Vite; preserve tests; don't touch auth flow"
     |
     v
Stored in: sessions.pinned_instructions (SQLite column)
     |
     v
Injected at: system prompt tail (high-attention U-shape position)
```

The extraction runs *asynchronously* — it doesn't block the first response. It's a **metadata task**, so it uses the CLI lightweight path (`--effort medium --max-turns 1 --no-session-persistence --tools ""`) at ~25-40% of normal cost.

### Clarification Accumulation

When the agent asks a clarifying question ("should I preserve the service worker?") and the user answers, that answer is appended to the pinned instructions — capped at 1,000 chars to prevent drift.

```
Pinned instructions grow with clarifications:
  "migrate React app CRA → Vite; preserve tests;
   don't touch auth flow;
   [+] keep service worker behavior identical;
   [+] target Node 20 runtime"
```

### Why system prompt tail?

LLMs attend disproportionately to the start and end of the context window (U-shape). The system prompt's tail is one of the highest-attention slots. Instruction Pinning places the task statement exactly there — every turn, every call.

---

## Layer 2: Snowball Recap (v1.8.6 P0)

Each turn prepends a `<task_recap>` block to the user message:

```
<task_recap>
Pinned task: migrate React app CRA → Vite; preserve tests;
             don't touch auth flow
</task_recap>

Actual user turn: "what about the proxy config?"
```

The name "snowball" comes from the fact that recap accumulates naturally across the conversation without re-prompting the LLM to remember. It costs zero LLM calls — it's pure string concatenation.

Combined with the U-shaped attention tail effect, it means the model "sees" the task on every single turn without any extra LLM round-trip.

---

## Layer 3: P2 Key-Fact Accumulator (v1.8.6)

Some facts aren't specific to one session — they describe the user or the project across time. Examples:

- "User's deployment target is Cloudflare Workers"
- "Preferred testing library is vitest, not jest"
- "Codebase uses `pnpm`, never `npm i`"

MemGPT's Core Memory was trying to capture these, but at ~6,500 tokens/turn. The Key-Fact Accumulator does it at ~100-150 tokens.

### How it works

```
Each substantive turn (non-trivial content)
     |
     v
Async Haiku extraction (lightweight CLI path):
   "Extract 2-4 key facts about the user, project, or preferences.
    Skip ephemeral context."
     |
     v
Stored in: key_facts table (FTS5 indexed)
  ┌─────────────────────────────────────────┐
  │ id | agent_id | content | access_count  │
  │ timestamp | source_turn_id              │
  └─────────────────────────────────────────┘
     |
     v
Next turn's system prompt assembly:
  SELECT content FROM key_facts
  WHERE agent_id = ?
  ORDER BY fts5_rank(relevance) DESC
  LIMIT 3
     |
     v
Inject top-3 as ~100-150 tokens
```

Each injection bumps `access_count` — frequently-used facts stay surfaced; one-off facts drift out.

### vs MemGPT Core Memory

| | MemGPT Core Memory | Key-Fact Accumulator |
|-|-|-|
| Injection size | ~6,500 tokens | ~100-150 tokens |
| Retrieval | Full block, every prompt | Top-3 FTS5-ranked |
| Invocation | Manual MCP tools | Automatic injection |
| Storage | Persistent block editing | Append + access tracking |
| Effective reduction | baseline | **−87%** |

---

## The Native Multi-Turn Foundation (v1.8.1)

All three layers sit on top of a fixed **native session handle**:

```
Claude CLI --resume <session-id>
     |
     v
session-id = SHA-256(agent_id + channel_id + thread_id)
     |
     v
If --resume fails (stale handle, account rotation,
                   unknown stream-json error):
     ↓ auto-fallback
History-in-prompt (XML-delimited turns)
```

This fixes the previous behavior where Agnes would lose context between consecutive messages ("幫我全部開啟" → "你指的是什麼？"). The session id is deterministic and stable across the *entire* thread lifetime (post-v1.8.14 Discord fix: `is_thread || created_thread` instead of `auto_thread && !is_thread`).

### Hermes-inspired Turn Trimming

Long conversation turns (>800 chars) are trimmed before being sent to the model:

```
Original turn: [850 chars of user input]
     |
     v
Trimmed: [first 300 chars] ... [trimmed 350 chars] ... [last 200 chars]
```

CJK-safe character-level slicing — no multi-byte codepoint panics. Zero LLM cost. Prevents token bloat on verbose pastes without losing opening intent or final instruction.

### Direct API Cache Strategy

When falling back to the Direct API (`direct_api.rs`), the request uses Anthropic's "system_and_3" prompt cache breakpoint placement — cache breakpoints at the system prompt and at the 3rd-most-recent assistant turn. This yields ~75% cache hit rate on multi-turn conversations, 95%+ on pure system-prompt hits.

---

## Interaction with the Evolution Engine

The session memory stack isn't isolated from evolution:

- **Prediction errors** compare what the model said to what the pinned task would predict. Large deviations trigger GVU reflection.
- **Key facts** feed into `external_factors` — user corrections, preference signals — which drive SOUL.md updates.
- **Session compression** (50k token threshold) produces a summary that is injected into the *system prompt*, not as a new conversation turn.

---

## Why This Matters

### Cost

The lightweight CLI path, paired with only injecting top-3 key facts, keeps metadata overhead under 10% of the total token budget — vs MemGPT's 30-40%.

### Attention Quality

By placing pinned instructions and key facts at the system prompt tail (high attention), and the snowball recap at the user message head (also high attention), every turn has the task statement in *two* high-attention slots. The model doesn't have to fish through the middle of a long context to find them.

### No Tool-Use Dependency

The old MemGPT design required the model to actively call `core_memory_append`. Agents sometimes forgot. The new stack is purely injection-driven — it works whether the model is cooperative or distracted.

### Backwards-Compatible Degradation

If the Haiku extraction fails (rate limit, timeout), the session still works — it just doesn't get the benefit of pinning/facts for that turn. Nothing breaks.

---

## The Takeaway

The chef doesn't re-memorize the recipe book on every plate. They look at a sticky note above the pass, scan the running order slip, and occasionally pull a card from the prep box. DuDuClaw's session memory stack is the same architecture: cheap surfaces in high-attention positions, not a heavy memory block competing with the actual work.
