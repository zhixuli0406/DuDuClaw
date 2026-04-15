# Multi-Runtime Agent Execution

> One platform, four AI backends — Claude, Codex, Gemini, and any OpenAI-compatible endpoint.

---

## The Metaphor: A Multilingual Office

Imagine an office that needs translators. Instead of hiring one translator who only speaks French, you build a translation desk that can assign work to a French speaker, a German speaker, a Japanese speaker, or any freelancer who speaks the client's language.

The desk doesn't care *which* translator handles the job — it cares that the translation is done well. If the French translator is busy, it routes to the next available one.

DuDuClaw's Multi-Runtime architecture is that translation desk — but for AI backends.

---

## How It Works

### The AgentRuntime Trait

At the core is a unified interface (`AgentRuntime`) that all backends implement:

```
AgentRuntime trait:
  fn execute(prompt, tools, context) → Response
  fn stream(prompt, tools, context) → Stream<Event>
  fn health_check() → Status
```

Every backend — Claude, Codex, Gemini, or any OpenAI-compatible endpoint — implements this same interface. The rest of the system doesn't know or care which backend is handling a particular request.

### The Four Backends

**Claude Runtime** — Calls the Claude Code CLI (`claude`) with JSONL streaming output. This is the most feature-rich backend, with native MCP tool support, bash execution, web search, and file operations built in.

```
Agent config: runtime = "claude"
     |
     v
Spawn: claude --json --print ...
     |
     v
Parse JSONL streaming events
     |
     v
Extract response + tool calls
```

**Codex Runtime** — Calls the OpenAI Codex CLI with `--json` flag for structured streaming events.

```
Agent config: runtime = "codex"
     |
     v
Spawn: codex --json ...
     |
     v
Parse JSONL STDOUT events
     |
     v
Extract response
```

**Gemini Runtime** — Calls the Google Gemini CLI with `--output-format stream-json` for structured output.

```
Agent config: runtime = "gemini"
     |
     v
Spawn: gemini --output-format stream-json ...
     |
     v
Parse streaming JSON events
     |
     v
Extract response
```

**OpenAI-compatible Runtime** — Calls any HTTP endpoint that speaks the OpenAI chat completions API (MiniMax, DeepSeek, local servers, etc.).

```
Agent config: runtime = "openai-compat"
              api_url = "http://localhost:8080/v1"
     |
     v
HTTP POST /v1/chat/completions
     |
     v
Parse SSE stream
     |
     v
Extract response
```

### RuntimeRegistry: Auto-Detection

When DuDuClaw starts, the **RuntimeRegistry** scans the system for available CLI tools:

```
Startup scan:
     |
     v
  Is `claude` in PATH? → Register Claude runtime
  Is `codex` in PATH?  → Register Codex runtime
  Is `gemini` in PATH? → Register Gemini runtime
  Any configured HTTP endpoints? → Register OpenAI-compat runtimes
     |
     v
Registry knows which backends are available
```

Agents can specify their preferred runtime in `agent.toml`:

```toml
[runtime]
preferred = "claude"    # Primary backend
fallback = "gemini"     # If primary is unavailable
```

If no preference is set, the registry uses the first available backend.

### Per-Agent Configuration

Different agents can use different backends simultaneously:

```
Agent "dudu" (customer support)  → Claude (best reasoning)
Agent "coder" (code generation)  → Codex (optimized for code)
Agent "analyst" (data analysis)  → Gemini (large context window)
Agent "local" (privacy-sensitive) → OpenAI-compat (local endpoint)
```

This means a single DuDuClaw installation can orchestrate agents across multiple AI providers, each using the backend best suited to their task.

---

## Cross-Provider Failover

When a backend becomes unavailable (rate-limited, down, or erroring), the **FailoverManager** automatically switches to the next available backend:

```
Claude runtime: rate-limited (cooldown: 2min)
     |
     v
FailoverManager checks agent config:
  fallback = "gemini"
     |
     v
Route to Gemini runtime
     |
     v
When Claude cools down → restore primary routing
```

The failover is transparent to the user — they see a response, regardless of which backend handled it. Health states are tracked independently per backend:

- **Healthy**: Normal operation
- **Rate-Limited**: Short cooldown (2 minutes)
- **Error**: Exponential backoff
- **Non-Retryable**: Manual intervention needed (auth failure, billing)

---

## Why This Matters

### No Vendor Lock-In

DuDuClaw doesn't bet on a single AI provider. If Claude raises prices, you can shift agents to Codex or Gemini. If Gemini adds a killer feature, you can adopt it without rebuilding your infrastructure.

### Best Tool for Each Job

Code generation might work better on Codex. Complex reasoning might work better on Claude. Data analysis might benefit from Gemini's large context window. Multi-Runtime lets you match the right brain to the right task.

### Resilience

If one provider goes down, the others keep your agents running. Combined with local inference fallback, DuDuClaw can survive any single-provider outage.

### Cost Optimization

Different providers have different pricing. The `LeastCost` rotation strategy can route to whichever provider offers the best price/performance for each query type.

---

## Interaction with Other Systems

- **Account Rotator**: Manages credentials across all providers, with cross-provider failover.
- **Confidence Router**: Sits below the runtime layer — decides local vs. cloud. The runtime layer decides *which* cloud.
- **CostTelemetry**: Tracks cost per provider, enabling informed routing decisions.
- **MCP Server**: Tools are exposed to all backends that support them (Claude via native MCP, others via tool injection).
- **Agent Config**: Each agent's `agent.toml` specifies its runtime preference and fallback chain.

---

## The Takeaway

The AI landscape is multi-provider. Building on a single CLI is like writing software for a single operating system — it works until it doesn't. The `AgentRuntime` trait abstracts away the differences, letting DuDuClaw treat Claude, Codex, Gemini, and any OpenAI-compatible endpoint as interchangeable backends. Your agents get the best available brain, every time.
