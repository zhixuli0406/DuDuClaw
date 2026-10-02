# Confidence Router & Local Inference Engine

> Smart model selection that saves 80%+ on API bills.

---

## The Metaphor: A Company's Travel Policy

Every company has a travel policy with tiers:

- **Economy**: Domestic flights, budget hotels. For routine business.
- **Business**: Better seats, better hotels. For important client meetings.
- **First Class**: Only for the CEO meeting a Fortune 500 partner.

Nobody flies first class to attend an internal stand-up. The travel desk looks at the trip's importance and assigns the appropriate tier.

DuDuClaw's Confidence Router is that travel desk — but for LLM queries. It evaluates each query's complexity and routes it to the cheapest model that can handle it well.

---

## How It Works

### The Three Tiers

| Tier | What Handles It | When It's Used | Cost |
|------|-----------------|----------------|------|
| **LocalFast** | Small local model (e.g. 7B parameters) | Simple queries, greetings, factual lookups | Free (local compute) |
| **LocalStrong** | Larger local model (e.g. 13B+ parameters) | Moderate complexity, summarization, translation | Free (local compute) |
| **CloudAPI** | Claude API | Complex reasoning, creative tasks, multi-step analysis | Pay-per-token |

### The Confidence Scoring

When a query arrives, the router computes a confidence score using lightweight heuristics:

```
Query arrives
     |
     v
+-----------------------+
| Count tokens          |  <-- Shorter queries are usually simpler
| Detect complexity     |  <-- Keywords like "analyze", "compare", "design"
|   keywords            |      signal higher complexity
| Estimate CJK ratio    |  <-- Chinese/Japanese text has different
|                       |      token density (~1.5 chars/token vs
|                       |      English ~4 chars/token)
+-----------------------+
     |
     v
Confidence score (0.0 - 1.0)
     |
     +---> > threshold_high  --> LocalFast
     |
     +---> > threshold_low   --> LocalStrong
     |
     +---> <= threshold_low  --> CloudAPI
```

The scoring is entirely rule-based — no LLM call needed to decide which LLM to use. The thresholds and keyword lists are configurable.

### CJK-Aware Token Estimation

This is a subtle but important detail for CJK (Chinese, Japanese, Korean) users. English text averages about 4 characters per token, but CJK text averages about 1.5 characters per token. A 100-character Chinese message consumes roughly 67 tokens, while a 100-character English message consumes about 25.

The router accounts for this difference when estimating query complexity. Without CJK awareness, the system would systematically underestimate the complexity of Chinese queries and route them to models that are too weak.

---

## The inference engine behind the router

For what models are available, how to pick one and how to install it, read **[53-local-models.md](53-local-models.md)** — that page owns the backend story now. This section covers only what the router needs to know.

### One shipped backend

**OpenAI-compatible HTTP** is the only `InferenceBackend` implementation that ships. It talks to anything speaking the OpenAI chat-completions API: llama-server, Ollama, llamafile single-binary servers, vLLM, SGLang. Configure it under `inference.toml [openai_compat]`.

The in-process backends this page used to list were removed on 2026-09-29 (`wiki/reports/feature-audit-2026-09-29.md` T1-D2/D3, T3-S4/S5):

- **llama.cpp** (`llama-cpp-2`) — the release build never compiled the `metal`/`cuda`/`vulkan` features, and its `generate()` was a stub returning "not yet fully implemented".
- **mistral.rs** (`mistralrs-core`, ISQ / PagedAttention / speculative decoding) — same: never compiled into a shipped binary.
- **MLX bridge** (`mlx_lm` Python subprocess) — zero call sites. The "local reflections without API calls" path this page described never existed in code.
- **Exo distributed clusters** — no example config anywhere in the repo made the mode unreachable for users; pointing `[openai_compat] base_url` at an Exo endpoint reaches the same cluster.

`BackendType::LlamaCpp` and `MistralRs` remain parseable config values so an existing `inference.toml` still loads, but selecting one returns `BackendUnavailable` with a message naming `openai_compat`.

Writes are checked as of v1.67.1:

- `inference.update` (the dashboard inference page) accepts `backend = "openai_compat"` or an empty value, which removes the key so the engine picks `openai_compat` itself. Any other value is refused before anything is written, with a message naming `openai_compat`. One exception keeps old files saveable: a removed value that is already the stored one may be sent back unchanged, so the page can still save other settings until you change it. The page shows such a value labelled as no longer supported.
- `agents.update` refuses any `[model.local] backend` other than `openai_compat` (this check predates v1.67.1).
- The scaffold default for `[model.local] backend`, `duduclaw onboard`, `duduclaw wizard` and the shipped agent templates now write `openai_compat` (they wrote `llama_cpp` before).
- The dashboard no longer shows fields nothing reads: 記憶體上限 (`max_memory_mb`), and the generation rows GPU Layers / Context 大小 (`[generation] gpu_layers` / `context_size`) on the inference page; Context 長度 / GPU Layers (`[model.local] context_length` / `gpu_layers`) on the agent edit page. The external server owns memory, GPU offload and context size. Saved values stay in the files untouched.

### The InferenceManager state machine

The manager keeps a priority chain with automatic failover:

```
Priority 1: llamafile
  (Single-binary server, zero installation)
     |
     v  (unavailable?)
Priority 2: Direct Backend
  (an in-process `InferenceBackend`; none ships today)
     |
     v  (no local GPU / model too large?)
Priority 3: OpenAI-compatible Server
  (llama-server, Ollama, vLLM, SGLang, …)
     |
     v  (no external server available?)
Priority 4: Cloud API
  (Claude API — the last resort, always available)
```

Each tier has periodic health checks. When one becomes unhealthy (crashes, runs out of memory, returns errors), the manager falls to the next. When it recovers, it is promoted back.

---

## llamafile: Zero-Install Inference

llamafile deserves a special mention. It's Mozilla's project that packages an LLM model and its inference engine into a single executable file. DuDuClaw manages llamafile as a subprocess:

```
User requests local inference
     |
     v
Is llamafile running?
     |
  +--+--+
  |     |
 Yes    No
  |     |
  |     v
  |  Start llamafile subprocess
  |  Wait for health check (ready-wait polling)
  |     |
  v     v
Route query to localhost:{port}
     |
     v
Return response
```

The manager handles the full lifecycle: starting, health monitoring, and stopping the subprocess. The llamafile server exposes an OpenAI-compatible API on localhost, so the router treats it like any other backend.

The result: portable, zero-install local inference that works on macOS, Linux, Windows, FreeBSD, and more.

---

## Why This Matters

### Cost Reduction

The most direct benefit: queries that don't need Claude's full reasoning power don't get sent to Claude. A "what time is it in Tokyo?" query costs zero when handled locally. Over thousands of daily queries, this adds up to 80%+ savings.

### Latency

Local models respond in milliseconds, not seconds. For simple queries, users get near-instant responses without waiting for a round-trip to the cloud.

### Privacy

Queries handled locally never leave the machine. For sensitive data or compliance-restricted environments, this is a critical advantage.

### Resilience

If the cloud API is down, rate-limited, or slow, local models keep the system running. The multi-tier fallback ensures there's always *something* available to handle queries.

---

## Model Management via MCP

The inference engine is fully manageable through MCP tools:

| Tool | Purpose |
|------|---------|
| `model_list` | List GGUF files in `~/.duduclaw/models/` |
| `model_load` / `model_unload` | Load/unload model lifecycle |
| `inference_status` | Loaded model, hardware, memory usage, backend type |
| `hardware_info` | GPU auto-detect, VRAM, RAM, recommendations |
| `route_query` | Preview routing decision without generation |
| `inference_mode` | Current mode (llamafile/direct/openai-compat/cloud-only) |
| `model_search` | Search HuggingFace + curated repos with RAM filtering |
| `model_download` | Download to `~/.duduclaw/models/` with resume + mirror fallback |
| `model_recommend` | Hardware-aware model suggestions |

---

## Interaction with Other Systems

- **Account Rotation**: When local inference handles a query, no API account is consumed. This extends the effective lifetime of API quotas.
- **CostTelemetry**: Tracks which tier handled each query, enabling operators to tune thresholds for optimal cost/quality balance. Adaptive routing auto-prefers local when cache efficiency drops below 30%.
- **Evolution Engine**: The router's decisions feed into the prediction engine's accuracy metrics.
- **Multi-Runtime**: The Confidence Router sits *below* the runtime layer — it decides the model, while the runtime decides the CLI backend.

---

## The Takeaway

Not every question deserves the most expensive answer. The Confidence Router ensures each query gets the cheapest model that can handle it well — and the multi-backend engine ensures there's always a model available, from a laptop GPU to a distributed cluster to the cloud.
