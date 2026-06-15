# DuDuClaw 🐾

<div align="center">

[繁體中文](README.md) · **English** · [日本語](README.ja.md)

</div>

<div align="center">

### 🛠 Need a custom AI Agent built with the same engineering rigor?

I'm available for freelance work — building **production-grade Agents**
with multi-LLM routing, MCP integration, RAG, secrets vaults, and full
observability. Same architecture standards as DuDuClaw.

[**Hire me on Fiverr →**](https://www.fiverr.com/louis_li_0406/build-a-custom-ai-agent-with-claude-openai-or-gemini-for-your-workflow-fbf0)
&nbsp;·&nbsp;
[LinkedIn](https://www.linkedin.com/in/zhixuli0406/)
&nbsp;·&nbsp;
[Portfolio](https://github.com/zhixuli0406)

</div>

---

> **Multi-Runtime AI Agent Platform** — unifying the three major CLIs (Claude / Codex / Gemini) to build your multi-channel AI assistant

[![CI](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml/badge.svg)](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/Rust-2024_edition-orange?logo=rust)](https://www.rust-lang.org/)
[![Python](https://img.shields.io/badge/Python-3.9+-blue?logo=python)](https://www.python.org/)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE)
[![Version](https://img.shields.io/badge/version-1.18.0-blue)](https://github.com/zhixuli0406/DuDuClaw/releases)
[![npm](https://img.shields.io/npm/v/duduclaw?logo=npm)](https://www.npmjs.com/package/duduclaw)
[![PyPI](https://img.shields.io/pypi/v/duduclaw?logo=pypi)](https://pypi.org/project/duduclaw/)

---

> 🎉 **v1.18.0 — Dashboard budget/usage correctness + reliability fixes** ([Release](https://github.com/zhixuli0406/DuDuClaw/releases/tag/v1.18.0))
>
> The Dashboard's budget and usage now read from the persistent `CostTelemetry` ledger instead of the in-memory counter that reset to zero on every rebuild, plus a round of dashboard runtime bug cleanup.
>
> - **Budget/usage shows real numbers** — Previously every budget display read `AccountRotator.spent_this_month`, but this in-memory counter reset to zero every 5 minutes when the rotator was rebuilt, reset to zero on gateway restart, cost 0 per call for OAuth subscription accounts anyway, and showed the same "sum across all accounts" number for every agent. It now reads month-to-date usage from `CostTelemetry` (a persistent SQLite ledger): `agents.list` / `agents.inspect` show **each agent's own** monthly usage, and `accounts.budget_summary` shows the real global total
> - **Cost unit fix** — The `cost_millicents` field name was a misnomer; what's actually stored is integer cents (verified by back-calculating from pricing: one record of 309 = $3.09). Removed the redundant `/10` in the analytics cost/savings display (which previously under-reported by 10x)
> - **`marketplace.install` properly implemented** — Previously a stub that returned an error; it now installs the catalog MCP server into the specified agent's `.mcp.json`, with a target-agent selection dialog added to the frontend
> - **Settings persistence gaps filled** — `system.version` returns `edition` so the dashboard can determine Pro-only UI; `system.update_config` writes `[voice]` into `inference.toml`, and per-agent `[proactive]` is saved into `agent.toml` via `agents.update` and backfilled by `agents.inspect`
> - **Frontend fixes** — Scheduling now sends the required `name`+`task` on submit (previously failed), MCP servers are consumed in the backend-serialized array shape, added a theme store + wired language switching into the Header, added 88 i18n keys across `zh-TW` / `en` / `ja-JP`, budget progress bar division-by-zero guard, and account silent errors now surface as toasts

<details>
<summary><strong>v1.9.4 → v1.17.x cumulative highlights</strong></summary>

- **v1.17.0** — RFC-24 License v2.0 (Open Core foundation): new crate `duduclaw-license` (verification-only client, signing keys stay in `commercial/duduclaw-license`), a 7-tier inheritance chain `OpenSource` / `Hobby` / `Solo` / `Studio` / `Business` / `SelfHostPro` / `Oem`, an Ed25519 trust registry seeded from `DUDUCLAW_LICENSE_PUBKEY_<ID>` env (empty registry fail-safe falls back to OpenSource). The Apache 2.0 core is **available without restriction**; paid subscriptions unlock the `commercial/*` value-add modules
- **v1.16.0** — MCP Refresh Tokens + GVU `SoulPatchOp::Consolidate`: new module `mcp_refresh` provides long-lived credentials backed by `~/.duduclaw/mcp_tokens.db` (`ddc_refresh_<env>_<64hex>`, 90 days, revocable, hash-only storage), solving the silent disconnect-without-retry after Claude Desktop auth-fail; GVU adds a `SoulPatchOp::Consolidate` variant carrying a "shrink invariant" so SOUL.md can self-trigger consolidation as it approaches the 150-line / 8KB hard cap
- **v1.15.2** — `agent_update_soul` trust-backdoor patch: previously writing SOUL.md did not call `soul_guard::accept_soul_change` to update the integrity hash, so every legitimate call left permanent stored-vs-current drift; and the entire call chain didn't write to `tool_calls.jsonl`, making the backdoor completely invisible to post-hoc analysis. v1.15.2 fills in the audit row (logging success + all four rejection paths, with a 16-char hash prefix) and syncs the fingerprint after every write
- **v1.15.1** — GVU SOUL.md unbounded-growth fix: agnes/SOUL.md bloated from 61 to 592 lines over 5 GVU cycles. Three layers of defense: (1) `strip_proposal_meta` strips meta sections like `## 診斷` / `## rationale` / `## expected_improvement` on the legacy path; (2) `SOUL_MAX_LINES = 150` / `SOUL_MAX_BYTES = 8KB` hard caps independent of the ASI content-weight threshold; (3) added a structured `SoulPatch { section, op, content }` and `apply_patch_to_soul`, wiring the full Generator→Verifier→Updater chain
- **v1.15.0** — Cross-Platform PTY Pool + Worker: Anthropic's official alternative path after it blocked `claude -p` for OAuth-subscription accounts. New crate `duduclaw-cli-runtime` (`portable-pty` ConPTY/openpty cross-platform + sentinel-framed in-band protocol + `PtyPool` semaphore + idle eviction + supervisor + restart policy) and `duduclaw-cli-worker` (localhost JSON-RPC + Bearer + `/healthz`, gateway can run it in-process or out-of-process); `channel_reply` routes OAuth via the REPL and API keys via `oneshot_pty_invoke + claude -p`; Phase 8 `pty_pool_*` Prometheus metrics; all failures fall back to legacy `tokio::process::Command`. Off by default; enable with `agent.toml [runtime] pty_pool_enabled = true`
- **v1.14.0** — RFC-23 Sensitive Data Redaction: new crate `duduclaw-redaction`. Internal data (Odoo / shared wiki / file tools) is replaced with `<REDACT:CATEGORY:hash8>` tokens before being sent to the LLM, and automatically restored at trusted boundaries (user channel reply, whitelisted tool egress); AES-256-GCM encrypted SQLite vault (per-agent 32-byte key, 0o600 permissions) + a two-phase TTL 7d GC (mark→purge after 30 days) + 5 built-in profiles + a five-layer enable/disable resolver + JSONL audit with 10MB rotation
- **v1.13.1** — Odoo Test-Before-Save: the `odoo.test` RPC accepts inline params so the Dashboard "Test connection" button hits Odoo with the current form values without saving first; leaving the inline credential blank falls back to the stored key; the same SSRF / HTTPS / db-name validation chain applies, and `scrub_odoo_error()` truncates to 240 chars to prevent HTML error-page leakage
- **v1.13.0** — Runtime-health overhaul (16 issues / two rounds of fixes): restored GVU/SOUL self-evolution, added the `[prompt] mode = "minimal"` Anthropic Skills-style system prompt, the `[budget] max_input_tokens` compression pipeline, an async session summarizer, TF-IDF wiki relevance ranking, and the `duduclaw lifecycle flush` quarterly hot/cold-separation CLI
- **v1.12.x** — W22-P0 ADR-002 `x-duduclaw` capability negotiation (HTTP 422 early failure) + ADR-004 Secret Manager + RFC-22 multi-agent coordination fixes (agnes faking sub-agent responses / autopilot mass mis-triggering / channel-path token not logged) + the `duduclaw weekly-report` subcommand
- **v1.11.0** — RFC-21 ([Issue #21](https://github.com/zhixuli0406/DuDuClaw/issues/21)): `duduclaw-identity` crate (IdentityProvider trait + Wiki/Notion/Chained three implementations) + Odoo per-agent credential isolation (`OdooConnectorPool` replaces the global admin singleton) + shared wiki `.scope.toml` SoT namespace policy
- **v1.10.0** — Wiki RL Trust Feedback: `WikiTrustStore` per-agent SQLite trust, `CitationTracker` two-level LRU + bounded-time eviction to prevent DoS, `WikiJanitor` daily pass (auto-marking corrected / archive / frontmatter sync) + sub-agent turn_id propagation + multi-process flock + atomic batch upsert
- **v1.9.4** — `duduclaw-durability` five persistence mechanisms (idempotency / retry / circuit breaker / checkpoint / DLQ) + `duduclaw-governance` PolicyRegistry + MCP HTTP/SSE Transport + LOCOMO memory evaluation system (daily 03:00 UTC eval + 200 golden QA) + LLM Fallback + Discord RESUME + Web ReliabilityPage

</details>

---

## Table of Contents

- [What is DuDuClaw?](#what)
- [Core Features](#features)
- [Comparison](#comparison)
- [Agent Directory Structure](#directory)
- [Security Hooks](#security)
- [Installation](#install)
- [CLI Commands](#cli)
- [Project Structure](#structure)
- [Technical Decisions](#tech)
- [Testing](#testing)
- [Documentation](#docs)
- [License](#license)

---

<a id="what"></a>

## What is DuDuClaw?

DuDuClaw is a **Multi-Runtime AI Agent platform** — it supports the three major CLIs (**Claude Code / Codex / Gemini**) as AI backends simultaneously, with seamless switching and auto-detection via a unified `AgentRuntime` trait.

It is not tied to any single AI provider; instead, it gives your AI Agent the complete infrastructure of messaging channels, memory, self-evolution, local inference, and account management.

Core concepts:

- **Multi-Runtime** — the `AgentRuntime` trait unifies four backends (Claude / Codex / Gemini / OpenAI-compat), `RuntimeRegistry` auto-detects, and configuration is per-agent
- **Plumbing = DuDuClaw** — responsible for channel routing, session management, memory search, account rotation, local inference, and other infrastructure
- **Bridge = MCP Protocol** — `duduclaw mcp-server` acts as an MCP Server, exposing channel and memory tools to the AI Runtime

```
AI Runtime (brain) — Claude CLI / Codex CLI / Gemini CLI / OpenAI-compat
  ↕ MCP Protocol (JSON-RPC 2.0, stdin/stdout)
DuDuClaw (plumbing)
  ├─ Channel Router — Telegram / LINE / Discord / Slack / WhatsApp / Feishu / WebChat
  ├─ Multi-Runtime — Claude / Codex / Gemini / OpenAI-compat auto-detection + per-agent config
  ├─ Session Memory Stack — native --resume + Instruction Pinning + Snowball Recap + Key-Fact Accumulator
  ├─ MCP Server — 80+ tools (messaging, memory, Agent, Skill, inference, tasks, knowledge base, ERP), per-agent registration
  ├─ Evolution Engine — GVU² dual-loop evolution + prediction-driven + MistakeNotebook
  ├─ Inference Engine — llama.cpp / mistral.rs / Exo P2P / llamafile / MLX / ONNX
  ├─ Voice Pipeline — ASR (SenseVoice / Whisper) + TTS (Piper / MiniMax) + VAD (Silero)
  ├─ Account Rotator — multi-OAuth + API Key rotation, budget tracking, health checks, Cross-Provider Failover
  ├─ Browser Automation — 5-layer auto-routing (API Fetch → Scrape → Headless → Sandbox → Computer Use)
  ├─ Worktree Isolation — Git worktree L0 sandbox, atomic merge, cap of 5 per Agent
  ├─ Wiki Knowledge Layer — L0-L3 four-tier knowledge architecture + trust weighting + FTS5 + auto-injection
  ├─ ACP/A2A Server — `duduclaw acp-server` stdio JSON-RPC 2.0, Zed/JetBrains/Neovim integration
  └─ Web Dashboard — React 19 SPA (23 pages), embedded in the binary via rust-embed
```

---

<a id="features"></a>

## Core Features

### Channels & Messaging

| Feature | Description |
|------|------|
| **Seven-channel support** | Telegram (long polling), LINE (webhook), Discord (Gateway WebSocket, op 6 RESUME + stall watchdog + 1-5s jitter), Slack (Socket Mode), WhatsApp (Cloud API), Feishu (Open Platform v2), WebChat (WebSocket) |
| **Per-Agent Bot** | Each Agent can have its own Bot Token, with multiple Agents running in parallel on the same platform |
| **Channel hot-start/stop** | Adding/removing channels in the Dashboard takes effect immediately, no gateway restart needed |
| **WebChat** | Built-in `/ws/chat` WebSocket endpoint, real-time conversation in the React frontend |
| **Generic Webhook** | `POST /webhook/{agent_id}` + HMAC-SHA256 signature verification |
| **Media Pipeline** | Automatic image resizing (max 1568px) + MIME detection + Vision integration |
| **Sticker system** | LINE sticker catalog + emotion detection + Discord emoji equivalence mapping |

### AI Execution & Inference

| Feature | Description |
|------|------|
| **MCP Server architecture** | `duduclaw mcp-server` provides 80+ tools covering messaging, memory, Agent management, inference, scheduling, the Skill marketplace, the task board, the shared knowledge base, and Odoo ERP. Registered in each agent directory's `.mcp.json` (Claude CLI `-p --dangerously-skip-permissions` only reads project-level settings), auto-created/repaired at gateway startup |
| **MCP Refresh Tokens** (v1.16.0) | Long-lived credentials backed by `~/.duduclaw/mcp_tokens.db` — token form `ddc_refresh_<env>_<64hex>`, 90-day lifespan, individually revocable, hash-only storage (the original token never lands on disk); `authenticate_from_env` routes credentials by prefix, the legacy `ddc_<env>_<32hex>` is fully preserved; the new CLI `duduclaw mcp { issue-refresh-token \| revoke-token \| list-tokens }` solves the pain of Claude Desktop silently disconnecting without retry after an auth-fail |
| **Multi-Runtime** | The `AgentRuntime` trait — four backends (Claude / Codex / Gemini / OpenAI-compat), `RuntimeRegistry` auto-detection, per-agent config |
| **Local inference engine** | Unified `InferenceBackend` trait — llama.cpp (Metal/CUDA/Vulkan) / mistral.rs (ISQ + PagedAttention) / Exo P2P cluster / llamafile / MLX (Apple Silicon) / OpenAI-compat HTTP |
| **Three-tier confidence routing** | LocalFast → LocalStrong → CloudAPI, auto-routed via heuristic confidence scoring, with CJK-aware token estimation |
| **InferenceManager** | Multi-mode auto-switching: Exo P2P → llamafile → Direct backend → OpenAI-compat → Cloud API, periodic health checks + automatic failover |
| **Native multi-turn Session** | Claude CLI `--resume` with a SHA-256 deterministic session ID + history-in-prompt fallback (auto-retry on account rotation/stale session); Hermes-style turn trimming (>800 chars, CJK-safe); Direct API "system_and_3" breakpoint cache strategy |
| **Session memory stack** | Instruction Pinning (Haiku extracts the core task from the first message → injected at the tail of the session prompt) + Snowball Recap (a zero-cost `<task_recap>` prepended each turn) + P2 Key-Fact Accumulator (2-4 facts per turn → FTS5 index → top-3 injection, only 100-150 tokens vs MemGPT's 6,500 tokens, −87%) |
| **Claude CLI lightweight path** | `call_claude_cli_lightweight()` handles metadata tasks (compression, instruction/key-fact extraction) with `--effort medium --max-turns 1 --no-session-persistence --tools ""`, saving 25-40% in cost |
| **Claude CLI stabilization flags** | `--strict-mcp-config` (MCP isolation) + `--exclude-dynamic-system-prompt-sections` (cross-turn prompt stability, 10-15% token savings); `--bare` was removed in v1.8.11 because it broke the OAuth keychain |
| **Direct API** | Bypasses the CLI to call the Anthropic Messages API directly, reaching a 95%+ cache hit rate with `cache_control: ephemeral` |
| **Token compression** | Meta-Token (BPE-like 27-47%), LLMLingua-2 (2-5x lossy), StreamingLLM (infinite-length conversation) |
| **Cross-Provider Failover** | `FailoverManager` health tracking, cooldown, non-retryable error detection |
| **Cross-Platform PTY Pool** (v1.15.0) | An interactive REPL channel dedicated to OAuth accounts — cross-platform `portable-pty` (ConPTY on Win 10 1809+, openpty on Unix) + a sentinel-framed in-band response protocol (no scrollback scraping / no sidecar) + per-agent semaphore + idle eviction + health-check supervisor + restart policy. Off by default; enabled per-agent via `agent.toml [runtime] pty_pool_enabled = true`; an optional out-of-process mode (`worker_managed = true`) moves the pool to the `duduclaw-cli-worker` subprocess communicating over localhost JSON-RPC |
| **PTY Pool Observability** | Phase 8 production-rollout metrics — `pty_pool_*` Prometheus counters (acquires / cache-hit / spawn / three eviction reasons / 4 invoke outcomes / duration histogram) + `worker_health_misses_total` + `worker_restarts_total` + the `pty_pool_managed_worker_active` mode gauge + the `GET /api/runtime/status` JSON endpoint (loopback-only) |
| **Browser automation** | 5-layer routing (API Fetch → Static Scrape → Headless Playwright → Sandbox Container → Computer Use), deny-by-default |

### Voice & Multimedia

| Feature | Description |
|------|------|
| **ASR speech recognition** | ONNX SenseVoice (local) + Whisper.cpp (local) + OpenAI Whisper API |
| **TTS speech synthesis** | ONNX Piper (local) + MiniMax T2A |
| **VAD voice activity detection** | ONNX Silero VAD |
| **Discord voice channel** | Songbird integration, Discord voice conversation |
| **LiveKit voice room** | WebRTC multi-Agent voice conferencing |
| **ONNX embeddings** | BERT WordPiece tokenizer + ONNX Runtime vector embeddings |

### Agent Orchestration & Evolution

| Feature | Description |
|------|------|
| **Sub-Agent orchestration** | `create_agent` / `spawn_agent` / `list_agents` MCP tools + `reports_to` org hierarchy + D3.js architecture chart; the system prompt auto-injects a "## Your Team" sub-Agent roster + long report messages are auto-paginated (Discord 1900 / Telegram 4000 / LINE 4900 / Slack 3900 byte budget, labeled `📨 **agent** 的回報 (1/N)`) |
| **Cross-system prompt injection** | CLAUDE.md + CONTRACT.toml (must_not/must_always) + SOUL.md + Wiki L0+L1 + key_facts top-3 + pinned_instructions are injected consistently across the CLI/channel/dispatcher paths, with behavior aligned across the four Claude/Codex/Gemini/OpenAI runtimes |
| **Orphan response recovery** | On dispatcher startup, `reconcile_orphan_responses` scans `bus_queue.jsonl` and atomically replays `agent_response` callbacks left over after crash/Ctrl+C/hotswap |
| **GVU² dual-loop evolution** | Outer loop (Behavioral GVU — SOUL.md evolution) + inner loop (Task GVU — real-time task retry), with MistakeNotebook as cross-loop memory |
| **Prediction-driven evolution** | Active Inference + Dual Process Theory, ~90% of conversations at zero LLM cost; MetaCognition self-calibrates thresholds every 100 predictions |
| **4+2 layer verification** | L1-Format / L2-Metrics / **L2.5-MistakeRegression** / L3-LLMJudge / **L3.5-SandboxCanary** / L4-Safety, the first 4 layers at zero cost |
| **Adaptive Depth** | MetaCognition drives GVU iteration depth (3-7 rounds), auto-adjusted based on historical success rate |
| **Deferred GVU** | gradient accumulation + delayed retry (up to 3 deferrals, 9-21 effective iterations over a 72h span) |
| **ConversationOutcome** | Zero-LLM conversation outcome detection (TaskType / Satisfaction / Completion), bilingual zh-TW + en |
| **SOUL.md versioning** | 24h observation period + auto-rollback, atomic write (SHA-256 fingerprint) |
| **`SoulPatchOp::Consolidate`** (v1.16.0) | The structured patch path adds a "shrink invariant" variant — semantically equivalent to `Replace`, but `apply_patch_to_soul` rejects new content that isn't shorter than the existing body, so the LLM can self-trigger consolidation as SOUL.md approaches the 150-line / 8KB hard cap |
| **`agent_update_soul` trust chain** (v1.15.2) | After writing, automatically `soul_guard::accept_soul_change` syncs the integrity fingerprint + both success and all four rejection paths are written to `tool_calls.jsonl` (16-char hash prefix), patching the stored-vs-current drift and the backdoor-invisibility problem |
| **Agent-as-Evaluator** | An independent Evaluator Agent (Haiku for cost control) performs adversarial verification with a structured JSON verdict |
| **DelegationEnvelope** | Structured handoff protocol — context / constraints / task_chain / expected_output, backward-compatible with the Raw payload |
| **TaskSpec workflow** | Multi-step task planning — dependency-aware scheduling / auto-retry (3x) / replan (up to 2x) / persistence |
| **Orchestrator template** | 5-step planning strategy (Analyze → Decompose → Delegate → Evaluate → Synthesize) + complexity routing |
| **Skill lifecycle** | 7-stage management — Activation → Compression → Extraction → Reconstruction → Distillation → Diagnostician → Gap Analysis |
| **Skill auto-synthesis** | Detect repeated domain gaps → synthesize new Skills from episodic memory → sandbox trial (TTL-managed) → cross-Agent graduation |
| **Task Board** | SQLite task management — status/priority/assignment tracking + real-time Activity Feed (WebSocket push) |
| **Autopilot rule engine** | Automated task delegation, notification, and Skill triggering — supports task creation/status change/channel message/idle detection/Cron schedule |
| **Shared knowledge base** | `~/.duduclaw/shared/wiki/` cross-Agent shared knowledge (SOPs, policies, product specs) + author attribution |
| **Wiki knowledge tiering** | Vault-for-LLM inspired — L0 Identity / L1 Core (auto-injected into every conversation) / L2 Context (updated daily) / L3 Deep (searched on demand), each page carrying a `trust` (0.0-1.0) weight; FTS5 unicode61 tokenizer supports CJK full-text search; `wiki_dedup` detects duplicate pages, `wiki_graph` outputs a Mermaid knowledge graph |
| **Wiki auto-injection** | `build_system_prompt()` automatically injects L0+L1 pages into WIKI_CONTEXT; covers all three system-prompt assembly paths (CLI interaction, channel reply, dispatcher/cron), consistent across the four Claude/Codex/Gemini/OpenAI runtimes |
| **Git Worktree L0 isolation** | An independent worktree workspace per task (cheaper than a container sandbox), atomic merge (dry-run pre-check + global `Mutex`), friendly `wt/{agent_id}/{adjective}-{noun}` branch names; cap of 5 per agent, 20 globally; Snap workflow: create → execute → inspect → merge/cleanup |
| **ACP/A2A Protocol Server** | `duduclaw acp-server` provides a stdio JSON-RPC 2.0 server (`agent/discover` / `tasks/send` / `tasks/get` / `tasks/cancel`), compatible with the Agent Client Protocol, supporting Zed / JetBrains / Neovim IDE integration; outputs a `.well-known/agent.json` AgentCard |
| **Reminder scheduling** | One-time reminders (relative time `5m`/`2h`/`1d` or ISO 8601 absolute time), `direct` static message or `agent_callback` wake-up mode |

### Reliability & Governance (added in v1.9.x)

| Feature | Description |
|------|------|
| **`duduclaw-durability` crate** | Five persistence mechanisms — idempotency key management, exponential-backoff retry (jitter), three-state circuit breaker (Closed/Open/HalfOpen), checkpoint resume, Dead Letter Queue for terminal-failure messages |
| **`duduclaw-governance` crate** | PolicyRegistry + 4 PolicyTypes (Rate/Permission/Quota/Lifecycle) + quota_manager (soft/hard quotas) + error_codes (standardized QUOTA_EXCEEDED / POLICY_DENIED) + YAML hot reload + audit log |
| **LLM Fallback** | Auto-switches to a fallback model on primary-model timeout/503/429/overloaded; the pure functions `is_llm_fallback_error` / `should_attempt_model_fallback`, with the hard deadline uniformly returning a hard-timeout error to trigger fallback |
| **Evolution Events system** | 30+ event schemas, an async emitter (batch + retry), a query interface, reliability mechanisms; HTTP endpoints exposed on the gateway, visualized in the Web ReliabilityPage |
| **MCP HTTP/SSE Transport** (W20-P1/P2) | `duduclaw http-server --bind 127.0.0.1:8765` — `POST /mcp/v1/call` (single JSON-RPC tool call) + `GET /mcp/v1/stream` (SSE long-lived event stream) + `POST /mcp/v1/stream/call` (async + SSE push) + Bearer auth + token bucket rate limit |
| **Memory MCP scope enforcement** | The `memory:read` / `memory:write` scopes are checked at the execute() entry point of `store/read/search`, patching the pre-v1.9.3 auth gap where any valid API Key could bypass scope |
| **LOCOMO memory evaluation** | `memory_eval/` — retrieval_accuracy / retention_rate / locomo_integrity_check + cron_runner (daily 03:00 UTC) + a 5-minute smoke_test P0 + a 200-entry golden QA gold set |

### Security

| Feature | Description |
|------|------|
| **Claude Code Security Hooks** | Three-phase progressive defense — Layer 1 blacklist (<50ms) → Layer 2 obfuscation detection (YELLOW+) → Layer 3 Haiku AI judgment (RED only) |
| **Threat-level state machine** | GREEN → YELLOW → RED with auto-escalation/degradation, dropping one level after 24h with no events |
| **SOUL.md drift detection** | Real-time SHA-256 fingerprint comparison |
| **Prompt Injection scanning** | 6 rule categories, XML delimiter tags for injection resistance |
| **Secret leak scanning** | 20+ patterns (Anthropic/OpenAI/AWS/GitHub/Slack/Stripe/DB URL, etc.) |
| **Sensitive file protection** | Read/Write/Edit three-way protection of `secret.key`, `.env*`, `SOUL.md`, `CONTRACT.toml` |
| **Behavioral contracts** | `CONTRACT.toml` defines `must_not` / `must_always` boundaries + `duduclaw test` red-team testing (9 scenarios) |
| **Unified multi-source audit log** | `audit.unified_log` merges 4 JSONL streams (`security_audit.jsonl` / `tool_calls.jsonl` / `channel_failures.jsonl` / `feedback.jsonl`) into a unified envelope (timestamp / source / event_type / agent_id / severity / summary / details); the Logs page supports source filtering, a severity dropdown, and live/historical tabs |
| **JSONL audit log** | Async writes, format-compatible with the Rust `AuditEvent` schema |
| **CJK-Safe string slicing** | The new `truncate_bytes` / `truncate_chars` module replaces 31 instances of `s[..s.len().min(N)]` byte-index slicing (fixing the v1.8.11 multi-byte codepoint panic) |
| **Per-Agent key isolation** | AES-256-GCM encrypted storage, keys invisible between agents |
| **Container sandbox** | Docker / Apple Container (`--network=none`, tmpfs, read-only rootfs, 512MB limit) |
| **Browser automation** | 5-layer routing (API Fetch → Static Scrape → Headless → Sandbox → Computer Use), deny-by-default |

### Accounts & Cost

| Feature | Description |
|------|------|
| **Dual-mode account rotation** | OAuth subscription (Pro/Team/Max) + API Key hybrid — 4 strategies (Priority/LeastCost/Failover/RoundRobin) |
| **Health tracking** | Rate-limit cooldown (2min), billing-exhaustion cooldown (24h), token-expiry tracking (30d/7d warnings) |
| **Cost telemetry** | SQLite token tracking, cache efficiency analysis, 200K price-cliff warning, adaptive routing (auto-switch to local when cache efficiency <30%) |
| **Claude CLI binary probing** | `which_claude()` / `which_claude_in_home()` scan Homebrew (Intel + Apple Silicon) / Bun / Volta / npm-global / `.claude/bin` / `.local/bin` / asdf shims / NVM version directories, fixing the binary-not-found issue at launchd startup |
| **Structured failure classification** | The `FailureReason` enum (RateLimited / Billing / Timeout / BinaryMissing / SpawnError / EmptyResponse / NoAccounts / Unknown) + category-specific zh-TW messages + `channel_failures.jsonl` audit records |

### Integrations & Extensions

| Feature | Description |
|------|------|
| **Odoo ERP integration** | `duduclaw-odoo` middleware — 15 MCP tools (CRM/Sales/Inventory/Accounting/generic search-report), supporting CE/EE, with EditionGate auto-detection. The Dashboard settings page supports **test-before-save** (v1.13.1, falls back to the stored key when the credential is left blank) + **per-agent credential isolation** (v1.11.0, `OdooConnectorPool` replaces the global admin singleton) |
| **Skill marketplace** | GitHub Search API live indexing + 24h local cache + security scan + Dashboard marketplace page |
| **Prometheus metrics** | `GET /metrics` — requests, tokens, duration histogram, channel status |
| **CronScheduler** | `cron_tasks.jsonl` + cron expressions, scheduled tasks fired automatically |
| **ONNX embeddings** | BERT WordPiece tokenizer + ONNX Runtime vector embeddings, with semantic search support |
| **Experiment Logger** | Trajectory recording, supporting RL/RLHF offline analysis |
| **Memory Decay scheduling** | Background `run_decay` every 24h: low-importance + 30+ days → archive; 90+ days archived → permanent deletion |
| **RL Trajectory Collector** | Writes to `~/.duduclaw/rl_trajectories.jsonl` during channel interactions; the `duduclaw rl` CLI provides export/stats/reward functions, with a composite reward (outcome×0.7 + efficiency×0.2 + overlong×0.1) |
| **Marketplace RPC** | `marketplace.list` serves a real MCP catalog (Playwright, Browserbase, Filesystem, GitHub, Slack, Postgres, SQLite, Memory, Fetch, Brave Search), mergeable with user-defined entries via `~/.duduclaw/marketplace.json` |
| **Partner Portal** | SQLite `PartnerStore` (`~/.duduclaw/partner.db`) + 7 RPCs (profile/stats/customers CRUD) + sales statistics |

### Web Dashboard

| Feature | Description |
|------|------|
| **Tech stack** | React 19 + TypeScript + Tailwind CSS 4 + shadcn/ui, warm amber color scheme |
| **24 pages** | Dashboard / Agents / Channels / Accounts / Memory / Security / Settings / OrgChart / SkillMarket / Logs / WebChat / OnboardWizard / Billing / License / Report / PartnerPortal / Marketplace / KnowledgeHub / Odoo / Login / Users / Analytics / Export / **Reliability** (added in v1.9.4) |
| **Reliability dashboard** | circuit breaker status / retry stats / DLQ queue depth / real-time evolution events data; the `/reliability` route, integrating the `getEvolutionEvents` / `getReliabilityStats` / `getDlqItems` APIs |
| **Real-time logs** | BroadcastLayer tracing → WebSocket push, WS heartbeat ping/pong (server 30s / client 25s) + 60s idle close |
| **Logs history page rewrite** | Source-filter chips (All / Security / Tool calls / Channel failures / Feedback) + live entry-count + severity dropdown + severity-colored left border (emerald/amber/rose) + click-to-expand JSON details |
| **Memory page Key Insights** | The fourth tab presents the structured insights accumulated by the P2 Key-Fact Accumulator (the `key_facts` table) + `access_count` badge + timestamp + source metadata |
| **Memory page evolution history** | SOUL.md version history + before/after metric diffs (positive feedback / prediction error / user corrections) + status badges (Confirmed / RolledBack / Observing) |
| **Toast notification system** | Module-scoped event bus, max-5 queue, auto-dismiss, warm stone/amber/emerald/rose variants, respects `prefers-reduced-motion` |
| **Org chart** | D3.js interactive Agent hierarchy visualization |
| **Light/dark toggle** | Follows system preference, supports manual toggle |
| **Internationalization** | zh-TW / en / ja-JP trilingual support (600+ translation keys) |
| **Skill Market three tabs** | Marketplace / Shared Skills / My Skills three-tab architecture + Skill adoption flow |
| **Autopilot settings** | Automation rule creation/management/monitoring + history review |
| **Session Replay** | Conversation replay component, supports timeline view |

---

<a id="comparison"></a>

## Comparison

| | **DuDuClaw** | **OpenClaw** | **IronClaw** | **Moltis** | **Dify** |
|---|---|---|---|---|---|
| Language | Rust | TypeScript | Rust | Rust | Python |
| Channels | 7 | 25+ | 8 | 5 | 0 (API) |
| Multi-Runtime | **4 backends (Claude/Codex/Gemini/OpenAI)** | - | - | - | Multi-LLM |
| MCP Server | **80+ tools** | - | - | - | - |
| Self-evolution engine | **GVU² dual-loop** | - | - | - | - |
| Local inference | **6 backends + three-tier confidence routing** | - | - | - | - |
| Voice (ASR/TTS) | **4 ASR + 4 TTS providers** | - | - | - | - |
| Token compression | **3 strategies** | - | - | - | - |
| Browser automation | **5-layer routing** | - | - | - | - |
| Cost telemetry | **Cache efficiency analysis** | - | Basic | Basic | Basic |
| Behavioral contracts | **CONTRACT.toml + red team** | - | WASM sandbox | - | - |
| ERP integration | **Odoo 15 tools** | - | - | - | - |
| Security audit | **Three-layer defense + Hooks** | CVE-2026-25253 | WASM | Basic | Medium |
| License | **Apache 2.0 (Open Core)** | MIT | Open source | Open source | $59+/month |

---

<a id="directory"></a>

## Agent Directory Structure

Each Agent is a folder, fully compatible with the Claude Code structure:

```
~/.duduclaw/agents/
├── dudu/                    # Main Agent
│   ├── .claude/             # Claude Code settings
│   │   └── settings.local.json
│   ├── .mcp.json            # MCP Server config (DuDuClaw platform tools + agent-specific MCP such as Playwright)
│   │                        # auto-created/repaired at gateway startup; Claude CLI `-p` mode only reads this file
│   ├── SOUL.md              # Persona definition (SHA-256 protected)
│   ├── CLAUDE.md            # Claude Code guidance (includes the CLAUDE_WIKI template)
│   ├── CONTRACT.toml        # Behavioral contract (must_not / must_always), auto-injected into the system prompt
│   ├── agent.toml           # DuDuClaw config (model, budget, heartbeat, runtime, capabilities)
│   ├── SKILLS/              # Skill set (can be auto-generated by the evolution engine)
│   ├── wiki/                # Wiki knowledge base (L0-L3 tiering + trust weighting + FTS5)
│   ├── memory/              # Daily notes + memory.db (prediction error) + key_facts table
│   ├── tasks/               # TaskSpec workflow persistence (JSON)
│   └── state/               # Runtime state (SQLite: sessions.pinned_instructions, etc.)
│
└── coder/                   # Another Agent
    └── ...
```

Use `duduclaw migrate` to automatically convert a legacy `agent.toml` to the Claude Code-compatible format.

---

<a id="security"></a>

## Security Hooks

DuDuClaw builds a three-phase progressive defense on top of Claude Code's Hook system:

```
                    ┌─────────────────────────────────────┐
  SessionStart ──→  │ session-init.sh                     │  Key permission verification + environment init
                    └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  UserPrompt   ──→  │ inject-contract.sh                  │  CONTRACT.toml rule injection
                    └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  PreToolUse   ──→  │ bash-gate.sh (Bash)                 │  Layer 1: blacklist (<50ms)
     (Bash)         │   ├─ Layer 2: obfuscation detect (YELLOW+)    │  Layer 2: base64/eval/exfiltration
                    │   └─ Layer 3: Haiku AI (RED only)   │  Layer 3: AI safety judgment
                    └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  PreToolUse   ──→  │ file-protect.sh → ai-review.sh     │  Sensitive file protection + AI review
  (Write|Edit|Read) └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  PostToolUse  ──→  │ secret-scanner.sh → audit-logger.sh │  Secret scan → async audit
                    └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  Stop         ──→  │ threat-eval.sh                      │  Threat-level re-evaluation
                    └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  ConfigChange ──→  │ config-guard.sh                     │  Config tampering detection
                    └─────────────────────────────────────┘
```

### Threat-level state machine

| Level | Trigger | Defense behavior |
|------|---------|---------|
| **GREEN** (default) | Normal operation | Layer 1 blacklist + file protection + Secret scan |
| **YELLOW** | ≥ 2 interceptions within 1 hour | +Layer 2 obfuscation detection + external network restriction |
| **RED** | Injection/eval attack detected | +Layer 3 Haiku AI judgment of all commands + AI file review |

Degradation: automatically drops one level after 24 hours with no events (RED→YELLOW→GREEN).

---

<a id="install"></a>

## Installation

### npm (recommended)

```bash
npm install -g duduclaw
```

After installation it automatically downloads the precompiled binary for your platform (supports macOS ARM64/x64, Linux x64/ARM64, Windows x64).

### Homebrew (macOS / Linux)

```bash
brew install zhixuli0406/tap/duduclaw
```

### One-line install

```bash
curl -fsSL https://raw.githubusercontent.com/zhixuli0406/DuDuClaw/main/scripts/install.sh | sh
```

### Python SDK (required dependency)

DuDuClaw's evolution engine (Skill Vetter) and some channel bridges require a Python environment:

```bash
pip install duduclaw
```

This command installs the following required dependencies:

| Package | Minimum version | Purpose |
|------|---------|------|
| `anthropic` | ≥ 0.40 | Direct Claude API calls, Skill security scan |
| `httpx` | ≥ 0.27 | Async HTTP client (account rotation, health checks) |

For development environments, additionally install:

```bash
pip install duduclaw[dev]
# Includes: pytest>=8, pytest-asyncio>=0.24, ruff>=0.8
```

### Build from source

```bash
git clone https://github.com/zhixuli0406/DuDuClaw.git
cd DuDuClaw

# Install Python dependencies
pip install duduclaw

# Build the Dashboard
cd web && npm ci --legacy-peer-deps && npm run build && cd ..

# Build the Rust binary (with Dashboard)
cargo build --release -p duduclaw-cli -p duduclaw-gateway --features duduclaw-gateway/dashboard

# First-time setup
./target/release/duduclaw onboard

# Start
./target/release/duduclaw run
```

> **Prerequisites**: [Rust](https://rustup.rs/) 1.85+, [Python](https://www.python.org/) 3.9+, [Node.js](https://nodejs.org/) 20+, and at least one AI CLI: [Claude Code](https://docs.anthropic.com/en/docs/claude-code), [Codex](https://github.com/openai/codex), [Gemini CLI](https://github.com/google-gemini/gemini-cli) (one or more)

---

<a id="cli"></a>

## CLI Commands

```
duduclaw onboard             # Interactive first-time setup
duduclaw run                 # One-click start (gateway + channels + heartbeat + cron + dispatcher)
duduclaw migrate             # Convert agent.toml to Claude Code format
duduclaw mcp-server          # Start the MCP Server (for the AI Runtime, stdio JSON-RPC 2.0)
duduclaw http-server         # Start the MCP HTTP/SSE Transport (Bearer auth, default 127.0.0.1:8765)
duduclaw acp-server          # Start the ACP/A2A Server (IDE integration: Zed/JetBrains/Neovim)
duduclaw gateway             # Start only the WebSocket gateway server

duduclaw agent               # CLI interactive conversation
duduclaw agent list          # List all Agents
duduclaw agent create        # Create a new Agent (industry template optional)
duduclaw agent inspect       # View Agent details
duduclaw agent pause         # Pause an Agent
duduclaw agent resume        # Resume an Agent
duduclaw agent edit          # Edit Agent settings
duduclaw agent remove        # Remove an Agent

duduclaw test <agent>        # Red-team security test (9 built-in scenarios + JSON report)
duduclaw status              # System health snapshot
duduclaw doctor              # Health diagnostics
duduclaw wizard              # Interactive industry-template setup
duduclaw evolution finalize  # One-shot recovery of overdue SOUL.md observation windows (--dry-run / --agent <id>)

duduclaw rl export           # Export RL trajectory (~/.duduclaw/rl_trajectories.jsonl)
duduclaw rl stats            # Per-Agent trajectory statistics
duduclaw rl reward           # Compute composite reward (outcome×0.7 + efficiency×0.2 + overlong×0.1)

duduclaw service install     # Install as a system service
duduclaw service start/stop  # Start/stop the system service
duduclaw service status      # Service status
duduclaw service logs        # Service logs
duduclaw service uninstall   # Remove the system service

duduclaw license activate    # Activate license
duduclaw license status      # License status
duduclaw license verify      # Verify license
duduclaw update              # Check for and install updates
duduclaw version             # Version info
```

---

<a id="structure"></a>

## Project Structure

```
DuDuClaw/
├── crates/                         # Rust crates (20)
│   ├── duduclaw-core/              # Shared types, traits (Channel, MemoryEngine), error definitions
│   ├── duduclaw-agent/             # Agent registry, heartbeat, budget, contract, skill loader/registry
│   ├── duduclaw-auth/              # Multi-user auth (Argon2 passwords, JWT, ACL role permissions)
│   ├── duduclaw-security/          # AES-256-GCM, SOUL guard, input guard, audit, key vault
│   ├── duduclaw-container/         # Docker / Apple Container / WSL2 sandbox execution
│   ├── duduclaw-memory/            # SQLite + FTS5 full-text search + vector embeddings + eval batch query API
│   ├── duduclaw-inference/         # Local inference engine (llama.cpp / mistral.rs / ONNX / Exo / llamafile)
│   ├── duduclaw-gateway/           # Axum server, 7 channels, session, GVU², prediction, cron, dispatcher, LLM fallback, evolution events, PTY pool integration
│   ├── duduclaw-bus/               # tokio broadcast + mpsc message routing
│   ├── duduclaw-bridge/            # PyO3 Rust↔Python bridge layer
│   ├── duduclaw-odoo/              # Odoo ERP middleware (JSON-RPC, CE/EE, 15 MCP tools)
│   ├── duduclaw-cli/               # clap CLI entry + MCP server (stdio + HTTP/SSE) + migrate + test
│   ├── duduclaw-dashboard/         # rust-embed embedded React SPA
│   ├── duduclaw-desktop/           # Desktop wrapper (macOS/Windows/Linux)
│   ├── duduclaw-durability/        # Durability framework (idempotency / retry / circuit breaker / checkpoint / DLQ) — added in v1.9.4
│   ├── duduclaw-governance/        # PolicyRegistry / quota_manager / error_codes / audit / approval — added in v1.9.4
│   ├── duduclaw-identity/          # IdentityProvider trait + Wiki/Notion/Chained three implementations — added in v1.11.0
│   ├── duduclaw-redaction/         # Source-aware redaction + reversible vault (AES-256-GCM) + 5 profiles + JSONL audit — added in v1.14.0
│   ├── duduclaw-cli-runtime/       # Cross-platform PTY pool runtime (portable-pty / sentinel-framed) — added in v1.15.0
│   └── duduclaw-cli-worker/        # Standalone PTY pool worker subprocess (localhost JSON-RPC + Bearer token) — added in v1.15.0
│
├── python/duduclaw/                # Python extension layer
│   ├── channels/                   # LINE / Telegram / Discord channel plugins
│   ├── sdk/                        # Claude Code SDK chat + multi-account rotation
│   ├── evolution/                  # Skill Vetter security scan
│   ├── tools/                      # Agent dynamic management tools
│   ├── agents/                     # capability manifest + capability-based router + memory_resolver (v1.9.4)
│   ├── mcp/                        # MCP API Key auth (with key masking) + memory tools (store/read/search/namespace/quota)
│   └── memory_eval/                # LOCOMO memory evaluation (retrieval/retention + cron + 200 golden QA) — added in v1.9.4
│
├── npm/                            # npm published packages
│   ├── duduclaw/                   # Main package (platform-agnostic wrapper + postinstall binary download)
│   ├── darwin-arm64/               # macOS Apple Silicon precompiled binary
│   ├── darwin-x64/                 # macOS Intel precompiled binary
│   ├── linux-x64/                  # Linux x86-64 precompiled binary
│   ├── linux-arm64/                # Linux ARM64 precompiled binary
│   └── win32-x64/                  # Windows x64 precompiled binary
│
├── web/                            # React Dashboard
│   └── src/
│       ├── components/             # UI components (OrgChart, ApprovalModal, SessionReplay)
│       ├── pages/                  # 24 pages (including ReliabilityPage added in v1.9.4)
│       ├── stores/                 # Zustand state management (8 stores)
│       ├── lib/                    # API client (WebSocket JSON-RPC + evolution events / reliability HTTP)
│       └── i18n/                   # zh-TW / en / ja-JP
│
├── templates/                      # Industry templates + Agent role templates
│   ├── restaurant/                 # Restaurant (customer service, reservations, FAQ, proactive push)
│   ├── manufacturing/              # Manufacturing (equipment monitoring, SOP, anomaly alerts)
│   ├── trading/                    # Trading (quotes, orders, inventory, price lists)
│   ├── evaluator/                  # Evaluator Agent (adversarial verification)
│   ├── orchestrator/               # Orchestrator Agent (task orchestration)
│   └── wiki/                       # Wiki knowledge base template
│
├── .claude/                        # Claude Code Hook security system
│   ├── settings.local.json         # Hook config (6 events × 10 scripts)
│   └── hooks/                      # Three-phase progressive defense scripts
│
├── docs/                           # Public documentation
│   ├── spec/                       # Format specs (SOUL.md / CONTRACT.toml)
│   ├── api/                        # WebSocket RPC + OpenAPI spec
│   ├── guides/                     # Development guides (custom MCP tools, etc.)
│   └── *.md                        # Architecture, deployment, evolution engine, etc.
│
├── ARCHITECTURE.md                 # Complete architecture design document
└── CLAUDE.md                       # AI collaboration design context
```

---

<a id="tech"></a>

## Technical Decisions

| Item | Choice | Rationale |
|------|------|------|
| AI conversation | **Multi-Runtime (Claude / Codex / Gemini CLI)** | Not tied to a single provider; auto-detection + per-agent config |
| Core language | **Rust** | Memory safety, high performance, single-binary deployment |
| Extension language | **Python (PyO3)** | Claude Code SDK integration, channel-plugin flexibility |
| Frontend framework | **React 19 + TypeScript** | Real-time data updates, mature ecosystem |
| UI style | **shadcn/ui + Tailwind CSS 4** | Warm, customizable, good performance |
| Database | **SQLite + FTS5** | Zero dependency, embedded, full-text search |
| Tool protocol | **MCP (Model Context Protocol)** | Native Claude Code support, stdin/stdout JSON-RPC |
| Local inference | **ONNX Runtime + llama.cpp** | Cross-platform, Metal/CUDA/Vulkan GPU acceleration |
| Speech recognition | **SenseVoice + Whisper.cpp** | Multilingual, local offline, zero API cost |
| Real-time communication | **WebRTC (LiveKit)** | Low-latency voice, multi-party conferencing |

---

<a id="testing"></a>

## Testing

```bash
# Rust tests
cargo test --workspace --exclude duduclaw-bridge

# Python tests
pip install pytest pytest-asyncio ruff
ruff check python/
pytest tests/python/ -v

# Frontend type checking
cd web && npx tsc --noEmit
```

---

<a id="docs"></a>

## Documentation

- [ARCHITECTURE.md](ARCHITECTURE.md) — Complete system architecture design
- [CLAUDE.md](CLAUDE.md) — AI collaboration design context and principles
- [CHANGELOG.md](CHANGELOG.md) — Version change log
- [docs/features/README.md](docs/features/README.md) — Detailed feature breakdown (19 articles, with zh-TW / ja-JP translations)
- [docs/features/feature-inventory.md](docs/features/feature-inventory.md) — Complete feature inventory
- [docs/spec/soul-md-spec.md](docs/spec/soul-md-spec.md) — SOUL.md format spec v1.0
- [docs/spec/contract-toml-spec.md](docs/spec/contract-toml-spec.md) — CONTRACT.toml format spec v1.0
- [docs/api/README.md](docs/api/README.md) — WebSocket RPC protocol + JSON-RPC 2.0 interface
- [docs/evolution-engine.md](docs/evolution-engine.md) — Evolution Engine v2 design document
- [docs/deployment-guide.md](docs/deployment-guide.md) — Production deployment guide
- [docs/development-guide.md](docs/development-guide.md) — Developer setup and Agent development
- [docs/guides/custom-mcp-tool.md](docs/guides/custom-mcp-tool.md) — Custom MCP tool tutorial

---

<a id="license"></a>

## License

**Open Core model** — the core code is licensed under [Apache License 2.0](LICENSE), completely free to use, modify, and distribute.

Commercial value-add modules (the `commercial/` directory) are closed-source and paid, including: industry templates, evolution parameter sets, the enterprise dashboard, and license verification.

See [LICENSING.md](LICENSING.md) for details.

---

<p align="center">
  🐾 Built with louis.li
</p>
