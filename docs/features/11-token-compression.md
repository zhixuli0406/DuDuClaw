# Prompt Budget Enforcement

> One pipeline on the reply path: estimate the prompt, and if it is over budget, walk three stages from least to most lossy — or refuse.

---

## A note on history

This page used to describe a "compression triad": Meta-Token/LTSC (lossless pattern substitution), an LLMLingua-2 bridge (lossy token pruning), and StreamingLLM (KV-cache eviction), each exposed as its own MCP tool.

That compressor was **removed from `duduclaw-inference` in v1.33**. It was reachable only through manual MCP tools, nothing on the reply path called it, and it duplicated the pipeline described below. Its two leftover dashboard config sections (`[llmlingua]`, `[streaming_llm]` in `inference.toml`, still accepted by `inference.update`) were removed in 2026-09 — they were writing keys nothing read.

What exists now is narrower and always on the hot path: `crates/duduclaw-gateway/src/prompt_compression.rs`.

---

## The problem it closes

Before this pipeline, the 200K price cliff was diagnosed *after* the request was sent: `cost_telemetry::record` raised a `cost_pressure` event, the operator got a warning, and the next request still went out unchanged. Budget enforcement closes the loop at the request boundary.

---

## Opting in

Enforcement is per agent, and **off unless configured**:

```toml
[budget]
max_input_tokens = 150000       # 0 or absent ⇒ enforcement disabled
cache_guard_min_eff = 0.5       # see "The cache guard" below
cache_guard_max_overshoot = 0.15
```

`prompt_audit::read_max_input_tokens` reads the first key; a missing file, unparseable TOML or an absent key means 0, which means the pipeline is never entered. That is deliberately the opposite convention from the cache guard, which defaults *on*.

---

## Token estimation

`estimate_tokens` is a CJK-aware heuristic, not a tokenizer: **1.306 tokens per CJK codepoint, 1 token per 3.6 characters otherwise**. Those constants came from a 2026-08 local-corpus calibration that replaced a uniform "1.5 characters per token" guess, which was underestimating real usage by roughly 22% on a Traditional Chinese workload.

`estimate_request_tokens` sums the system prompt, the history and the pending user message.

---

## The three stages

Stages are pure functions over `(system, history, user)`. Each either returns a smaller version or `None` ("I can't help further"), and the caller walks them in order until the estimate fits.

**1. `turn_trim`** — per-turn tail trim. A turn longer than 800 characters keeps its first 300 and last 200 characters with a `[trimmed N chars]` marker in between; character-level slicing, so CJK never splits mid-codepoint. Under cost pressure the threshold drops from 800 to 200. Short replies lose nothing.

**2. `drop_oldest_tool_echoes`** — strips old tool content, marking its bytes unavailable when that history path has no durable retrieval handle. Tool echoes are the cheapest thing to lose: the result is usually already reflected in the assistant turn that followed it.

**3. `bisect_and_summarize`** — the async stage. After the pure stages fail, the gateway summarizes only the *unprotected* older turns and rechecks the budget. `partition_turns_for_summary` decides the split.

If the pipeline still cannot fit the request, it does **not** silently send an over-budget prompt: it returns `BudgetExceeded` and emits a `budget_exceeded` event.

### Never-trim sections

`split_never_trim_sections` / `is_never_trim_header` / `never_trim_tokens` carve out system-prompt sections that no stage may touch — identity, the working-state authority block, and the safety boundaries. A budget can be missed; those sections cannot be quietly dropped to meet it.

---

## The cache guard

Compression is not free when the prompt cache is healthy. Rewriting even a small tail of the history changes the bytes the cache is keyed on, which forces a full cache-prefix rebuild — and arXiv:2607.12161 measures that rebuild as ~87% of the overhead in that regime. In other words, the tokens you save can cost more than they save.

`should_skip_for_cache` is the deterministic gate `maybe_compress_history` consults *before* entering the pipeline at all:

| Condition | Default | Effect |
|---|---|---|
| trailing cache efficiency | > 50% (`cache_guard_min_eff`) | and… |
| budget overshoot | < 15% (`cache_guard_max_overshoot`) | …skip compression entirely |

Unlike `max_input_tokens`, this guard is **enabled by default** — a missing file or absent key falls back to the paper's thresholds. Only an explicit `cache_guard_min_eff = 0` turns it off. It is a safety optimization that should protect agents without per-agent opt-in.

`CompressionInfo` (`compressed` / `compression_stages`) is threaded from `maybe_compress_history` down to the eventual `cost_telemetry` record, several async frames later, through a task-local.

---

## What is deliberately not here

Stated so nobody re-adds it by accident:

- **An LLMLingua-2 bridge.** Python subprocess startup latency makes it a poor fit for per-request synchronous compression. If it comes back it belongs in the async summarizer, not the hot path.
- **Meta-token / LTSC substitution.** It costs decode time on the agent side, so it would have to be an explicit opt-in knob. Deferred, not scheduled.
- **KV-cache eviction (StreamingLLM).** DuDuClaw drives CLIs and vendor APIs; it does not own the model's KV-cache.

---

## Where the tokens actually went

The largest single reduction in this area was not compression at all. `[runtime] minimal_context` (default on) narrows each CLI spawn's `--tools` list and passes `--setting-sources project,local`, taking measured fixed overhead from 35,892 to 10,974 tokens per spawn — about 69%. The remaining fixed cost is DuDuClaw's own MCP tool schemas.

---

## Interaction with other systems

- **Session memory stack** — compression summaries are injected into the system prompt, never as conversation turns. See [16-session-memory-stack.md](16-session-memory-stack.md).
- **CostTelemetry** — records `compressed` / `compression_stages` per request, and `cache_attribution_snapshot()` reports which cached block keeps breaking the prefix.
- **Direct API** — layered `cache_control` breakpoints split the system prompt via `CACHE_SPLIT_MARKER`, which is what makes the cache guard's efficiency number meaningful.

---

## The takeaway

The honest version of this page is one pipeline, three stages, two config keys and an explicit refusal path. The triad it replaced was three impressive strategies that nothing on the reply path ever called.
