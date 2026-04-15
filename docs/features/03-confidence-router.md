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

## The Multi-Backend Inference Engine

Behind the router sits a unified inference engine that supports multiple backends through a single interface:

### Backend Options

**llama.cpp** — The C++ workhorse. Supports hardware acceleration across:
- Apple Metal (macOS)
- NVIDIA CUDA (Linux/Windows)
- Vulkan (cross-platform GPU)
- CPU fallback (any platform)

**mistral.rs** — A Rust-native engine with advanced features:
- ISQ (In-Situ Quantization): Quantize models on-the-fly without pre-processing
- PagedAttention: Efficient memory management for longer contexts
- Speculative Decoding: Use a small model to draft tokens, verified by the main model

**OpenAI-compatible HTTP** — Connects to any server that speaks the OpenAI chat completions API:
- Exo distributed clusters
- llamafile single-binary servers
- vLLM, SGLang, and other serving frameworks

**MLX Bridge** — For Apple Silicon users, a Python subprocess calling `mlx_lm`:
- Local reflections without API calls
- LoRA adapter support for agent personality fine-tuning
- Saves API tokens by running reflections locally

### The InferenceManager State Machine

The system doesn't just pick one backend and stick with it. The InferenceManager maintains a priority chain with automatic failover:

```
Priority 1: Exo P2P Cluster
  (Multiple machines pooling GPU memory — can run 235B+ models)
     |
     v  (unavailable or unhealthy?)
Priority 2: llamafile
  (Single-binary server, zero installation)
     |
     v  (unavailable?)
Priority 3: Direct Backend
  (llama.cpp or mistral.rs loaded in-process)
     |
     v  (no local GPU / model too large?)
Priority 4: OpenAI-compatible Server
  (External vLLM, SGLang, etc.)
     |
     v  (no external server available?)
Priority 5: Cloud API
  (Claude API — the last resort, always available)
```

Each backend has periodic health checks. If a backend becomes unhealthy (crashes, runs out of memory, returns errors), the manager automatically falls to the next tier. When the backend recovers, it's promoted back.

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
| `inference_mode` | Current mode (exo-cluster/llamafile/direct/cloud-only) |
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
