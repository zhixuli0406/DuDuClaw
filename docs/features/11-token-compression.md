# Token Compression Triad

> Three strategies to fit more into less — lossless, lossy, and streaming.

---

## The Metaphor: Three Ways to Pack a Suitcase

You're traveling with a fixed-size suitcase (the LLM's context window). You have more stuff than fits. Three strategies:

1. **Vacuum bags** — Compress everything without losing anything. The clothes come out wrinkled but intact. You can unpack perfectly.
2. **Leave non-essentials behind** — Only pack what you'll actually wear. You lose some items, but the important ones are there.
3. **Ship ahead and cycle** — Send a box ahead with rarely-needed items. Keep a rolling set of daily wear in your suitcase. Swap as needed.

DuDuClaw offers all three strategies, each suited to different scenarios.

---

## Strategy 1: Meta-Token Compression (Lossless)

### How It Works

Meta-Token compression finds repeated patterns in the text and replaces them with shorter symbols — similar to how a zip file works, but designed for token sequences.

```
Original text (simplified example):
  {"type":"message","role":"user","content":"Hello"}
  {"type":"message","role":"assistant","content":"Hi"}
  {"type":"message","role":"user","content":"How are you?"}

The pattern {"type":"message","role":" appears 3 times.
     |
     v
Create a substitution:
  T1 = {"type":"message","role":"

Compressed text:
  T1user","content":"Hello"}
  T1assistant","content":"Hi"}
  T1user","content":"How are you?"}

Substitution table:
  T1 → {"type":"message","role":"
```

The algorithm scans the entire input, identifies the most frequently repeated subsequences, and replaces them with meta-tokens. This is applied iteratively — the output of one pass may contain new repetitions that a second pass can compress further.

### Performance Characteristics

- **Compression ratio**: 27-47% reduction in token count
- **Best for**: Structured, repetitive content (JSON, code, templates, conversation logs)
- **Worst for**: Highly varied natural language with no repetition
- **Reversibility**: 100% lossless — decompression produces the exact original
- **Speed**: Fast (no LLM calls, pure pattern matching)

### When to Use It

This strategy shines when sending structured data to the LLM:
- Long conversation histories (the message envelope format repeats)
- Code with boilerplate (import statements, class declarations)
- API responses with repetitive structure
- Templates with shared headers/footers

---

## Strategy 2: LLMLingua-2 (Lossy)

### How It Works

LLMLingua-2 is a different philosophy: instead of compressing the *format*, it compresses the *content* by removing tokens that don't carry much meaning.

```
Original text:
  "The user then proceeded to ask about the specific details
   of how the billing system handles edge cases related to
   international currency conversion in the accounting module."

Token importance scoring:
  "The user [low] then [low] proceeded [low] to ask about
   the [low] specific details of [low] how the billing system
   handles edge cases related to international currency
   conversion in [low] the accounting module."
     |
     v
After removing low-importance tokens:
  "user ask specific details billing system handles edge cases
   international currency conversion accounting module."
```

The importance scoring is done by a lightweight model that evaluates how much each token contributes to the overall meaning. Tokens that are purely structural (articles, prepositions, filler words) score low. Tokens that carry semantic content (nouns, verbs, domain terms) score high.

### Performance Characteristics

- **Compression ratio**: 2-5x reduction
- **Best for**: Natural language, conversation history, verbose explanations
- **Worst for**: Code, structured data (where every token matters)
- **Reversibility**: NOT reversible — information is lost
- **Speed**: Moderate (requires a lightweight model evaluation)

### When to Use It

This strategy is ideal for compressing old conversation history:
- The agent needs context from 50 previous messages, but they're too long to fit
- Summary would lose nuance; lossy compression preserves more detail than summarization
- The exact wording doesn't matter, but the semantic content does

---

## Strategy 3: StreamingLLM (KV-Cache Management)

### How It Works

This isn't compression of the *text* — it's management of the model's *internal memory* (the KV-cache). The idea comes from the observation that LLMs have two types of important positions:

1. **Attention sinks**: The first few tokens in a conversation. LLMs disproportionately attend to these regardless of content. They serve as "anchors" for the attention mechanism.
2. **Recent context**: The most recent tokens, which contain the immediately relevant information.

Everything in between tends to receive less attention and contributes less to response quality.

```
Full conversation (10,000 tokens):
  [Token 1-4]  [Token 5-8000]  [Token 8001-10000]
  ^              ^                ^
  Attention      Middle section   Recent context
  sinks          (less attended)  (highly relevant)
     |
     v
StreamingLLM KV-cache:
  [Token 1-4]  +  [Token 8001-10000]
  ^                ^
  Preserved        Preserved
  sinks            recent window

Middle section evicted from cache.
```

The sliding window moves forward as new tokens arrive:

```
Time 1: [Sinks] + [Tokens 8001-10000]
Time 2: [Sinks] + [Tokens 8501-10500]  (window slides)
Time 3: [Sinks] + [Tokens 9001-11000]  (window slides)
```

### Performance Characteristics

- **Compression effect**: Enables theoretically infinite conversation length
- **Best for**: Very long conversations that would exceed the context window
- **Worst for**: Conversations where middle context is critical
- **Reversibility**: N/A (manages cache, not text)
- **Speed**: Very fast (just cache eviction policy)

### When to Use It

This strategy is for conversations that would otherwise be impossible:
- Multi-hour customer support sessions
- Ongoing project discussions spanning days
- Always-on monitoring agents that never "restart"

---

## Combining Strategies

The three strategies aren't mutually exclusive. They can be layered:

```
Long conversation with structured data
     |
     v
Step 1: Meta-Token compress the structured parts
  (JSON messages, code blocks → 27-47% smaller)
     |
     v
Step 2: LLMLingua-2 compress old conversation history
  (Verbose exchanges from hours ago → 2-5x smaller)
     |
     v
Step 3: StreamingLLM manages the remaining context
  (Keep sinks + recent window, evict the rest)
     |
     v
Result: A conversation that would have consumed 200K tokens
        now fits comfortably in 50K
```

### Selection Logic

The system can automatically choose the appropriate strategy based on content type:

```
Content type analysis:
     |
     +---> Structured (JSON, code, templates)
     |       → Meta-Token (lossless, best for repetitive structure)
     |
     +---> Natural language (chat history, explanations)
     |       → LLMLingua-2 (lossy, best for verbose text)
     |
     +---> Active conversation (ongoing, growing)
             → StreamingLLM (cache management, prevents overflow)
```

---

## Why This Matters

### Direct Cost Savings

In the LLM world, tokens are money. A 40% reduction in input tokens means a 40% reduction in API cost for that request. Over thousands of daily requests, this adds up to significant savings.

### Larger Effective Context

By compressing input, the agent can consider more information within the same context window. A 100K context window effectively becomes 150K-200K when compression is applied. This means better responses because the agent has access to more relevant context.

### Infinite Conversations

StreamingLLM removes the hard ceiling on conversation length. Without it, conversations that exceed the context window must be summarized or truncated, losing information. With it, the conversation can continue indefinitely while maintaining coherence.

### Composable Architecture

Each strategy is an independent module accessible via MCP tools. Operators and agents can invoke them individually or in combination, depending on the specific scenario.

---

## Interaction with Other Systems

- **Session Manager**: Applies compression before storing long conversations.
- **Confidence Router**: Compressed prompts consume fewer tokens, affecting routing decisions.
- **CostTelemetry**: Tracks compression ratios and the resulting cost savings.
- **Memory System**: Old episodic memories may be compressed before archival.

---

## The Takeaway

Context windows are finite; conversations are not. The compression triad gives DuDuClaw three complementary tools to bridge this gap: Meta-Token preserves everything (structure), LLMLingua-2 preserves what matters (semantics), and StreamingLLM preserves what's needed now (recency). Together, they turn a hard limitation into a manageable trade-off.
