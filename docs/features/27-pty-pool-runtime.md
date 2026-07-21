# Cross-Platform PTY Pool + Worker

> When Anthropic blocked `claude -p` for OAuth accounts, we stopped mailing letters and kept a phone line open instead.

---

## The Metaphor: A Letter vs. an Open Phone Line

The old way of talking to the Claude CLI was like **mailing a letter for every question**. You wrote `claude -p "your prompt"`, sealed the envelope, sent it off, and a fresh courier (a brand-new process) carried it. Each letter was a complete, self-contained round trip. Simple — until the post office stopped delivering.

In mid-2026 Anthropic blocked `claude -p` for OAuth-subscription accounts. The letters bounced.

The fix is to **keep a phone line open to a colleague and talk in real time**. Instead of mailing a letter per question, you dial once, the line stays connected, and you speak your questions across the same call. The colleague is the real interactive `claude` REPL — the same program a human drives by hand.

But a phone call has a problem a letter doesn't: how do you know when the other person has *finished* speaking, versus just pausing? You agree on a **code word**. "Over." When you hear "over," you know the turn is complete and it's your turn again. DuDuClaw calls that code word the **sentinel** — a marker the model wraps around every answer so the runtime knows exactly where a response begins and ends. No guessing, no listening to the whole conversation history to find the answer.

This is the heart of the PTY Pool: a real terminal (the open line), a sentinel protocol (the code word), and a pool of pre-warmed sessions (colleagues already on hold, ready to talk).

---

## Status (2026-07): Do You Actually Need This Yet?

**Most agents should leave this OFF — and it is off by default.**

Anthropic had scheduled a 2026-06-15 change that would split programmatic usage (`claude -p`, the Agent SDK, GitHub Actions) onto a separate Agent SDK credit, which would have broken OAuth-subscription channel replies. **On 2026-06-15 that change was paused.** As of this writing `claude -p` still works for OAuth-subscription accounts, so the fresh-spawn (`FreshSpawn`) path — the default — is fully functional and the PTY pool is **not required**.

The PTY pool is therefore kept as a **standby**: if Anthropic re-activates the programmatic-usage split, flipping `pty_pool_enabled = true` restores OAuth channel replies without a code change. Until then, turn it on only if you have a specific reason and have read the limitation below.

---

## Known Limitation: Pool Sessions Are Not Per-Conversation

**Read this before enabling `pty_pool_enabled`.** The pool keys its long-lived REPL sessions by `(agent, cli_kind, bare_mode, account, model)` — **there is no conversation dimension.** A single agent's WebChat conversation A and conversation B share the *same* live `claude` REPL, and that REPL remembers its own prior turns. The result is **cross-conversation context bleed**: conversation B can see workflow state started in conversation A (e.g. B asks for a to-do list and gets A's; two different conversations receive the same weekly report).

This does **not** affect the default `FreshSpawn` (`claude -p`) path. Fresh-spawn carries no CLI-side session state — each turn's context comes solely from `SessionManager::get_messages(session_id)`, and the session id is per-conversation (WebChat composes `webchat:<conn>#agent:<id>#conv:<nonce>`; every external channel keys on its chat/thread id). So the default path isolates conversations correctly; only the opt-in PTY pool shares a REPL across them.

If you enable the pool for a single-conversation workload (one long-running task per agent, no concurrent distinct conversations) this is a non-issue. For multi-conversation agents (a WebChat bot serving many users/threads at once), **do not enable it** until a per-conversation pool key lands.

---

## Why a Real PTY, Not Scrollback Scraping

Driving an interactive REPL programmatically has two naive failure modes:

1. **Pipe it like a normal subprocess** — but `claude` detects it isn't attached to a real terminal and refuses to run interactively. Many CLIs do this.
2. **Screen-scrape the scrollback** — capture everything the terminal prints and try to parse the answer out of the noise (banners, spinners, ANSI color codes, prompt chrome). Fragile and slow.

DuDuClaw does neither. It allocates a **real pseudo-terminal (PTY)** so `claude` believes a human is typing, then uses an **in-band sentinel-framed protocol** so the answer arrives pre-delimited — no scrollback scraping, no sidecar process.

```
   Naive approach                    PTY Pool approach
   ─────────────                     ─────────────────
   claude (refuses pipe)             real PTY (claude sees a TTY)
        │                                  │
   scrape scrollback                  read_until(sentinel)
        │                                  │
   regex out of ANSI noise           payload arrives pre-framed
        │                                  │
   ❌ fragile                         ✅ deterministic
```

The PTY backend is cross-platform — that's what makes one code path span macOS, Linux, and Windows:

| Platform | PTY backend | Provided by |
|----------|-------------|-------------|
| Windows 10 (1809+) / 11 | ConPTY | `portable-pty` |
| macOS | openpty | `portable-pty` |
| Linux | openpty | `portable-pty` |

Earlier prior art (`dorkitude/maude` via tmux, `runtorque/torque` via a Unix-domain-socket PTY supervisor) was **Unix-only**. `portable-pty` is the piece that lets a single runtime cover all three operating systems.

---

## The Sentinel Protocol

When a `PtySession` spawns, it injects an `--append-system-prompt` instruction that teaches the model the sentinel-wrapping protocol: emit a sentinel line, the answer text, then an identical sentinel line again — and nothing after the closing sentinel.

```
Gateway                          PTY                         claude REPL
   │                              │                              │
   │  invoke("what's the proxy   │                              │
   │          config?")          │                              │
   ├─────────────────────────────►  write prompt to PTY ───────►│
   │                              │                              │ thinks…
   │                              │  ◄─── <SENTINEL> ────────────┤
   │                              │  ◄─── answer text  ──────────┤
   │                              │  ◄─── <SENTINEL> ────────────┤
   │  read_until(closing         │                              │
   │            sentinel)        │                              │
   │  ◄── payload between the ────┤                              │
   │      sentinel pair          │                              │
```

The runtime reads until it sees the closing sentinel, then extracts the payload **between the sentinel pair**. Because the closing sentinel is the read-until probe, the runtime never has to interpret the surrounding terminal chrome — it just slices out the framed answer. The implementation deliberately takes the *last* pair of sentinel occurrences to survive cases where the terminal renders an opening sentinel inline with assistant chrome.

A separate **one-shot** path (`oneshot_pty_invoke`) exists for cases where a long-lived session isn't wanted. It still runs through a real PTY (so the CLI sees a TTY) but does **not** inject sentinel framing — it mirrors the lifecycle of a classic single invocation.

---

## RuntimeMode: Two Routes, One Default

Every agent's reply is routed by a `RuntimeMode` chosen from its `agent.toml`. The feature is **default OFF** — you opt in per agent.

| RuntimeMode | Path | When |
|-------------|------|------|
| `FreshSpawn` | Legacy `tokio::process::Command` via `call_claude_cli_rotated` | Default; whenever `agent.toml` is missing, malformed, or the flag is unset |
| `PtyPool` | This crate's pooled, sentinel-framed PTY sessions | Only when `[runtime] pty_pool_enabled = true` |

`runtime_mode_for_agent()` reads the agent directory and **fails safe to `FreshSpawn`** — a missing file, a parse error, or an unset flag all default to the legacy path. The gateway's public surface is `acquire_and_invoke` / `acquire_and_invoke_with`, which pull a session from the pool, run one sentinel round trip, and return it.

```
channel reply for agent X
        │
        ▼
runtime_mode_for_agent(agent_dir)
        │
   ┌────┴─────────────────────────┐
   │                              │
FreshSpawn                     PtyPool
   │                              │
tokio::process::Command        acquire_and_invoke()
claude -p (legacy)             pooled sentinel session
```

---

## OAuth vs. API-Key Routing

Within the `PtyPool` branch, `channel_reply` splits on account type — because the `claude -p` block only ever hit OAuth-subscription accounts:

| Account type | Route | Why |
|--------------|-------|-----|
| OAuth subscription | Long-lived interactive REPL (sentinel-framed) | `claude -p` is blocked for these; the REPL is the only path |
| API key | `oneshot_pty_invoke` + `claude -p` | `-p` still works for API-key auth; no need to hold a session |

So the OAuth accounts get the open phone line, and the API-key accounts keep mailing letters — each through a real PTY either way. The `claude_runner` dispatcher applies the same short-circuit, so sub-agent dispatch and channel reply stay consistent: when `pty_pool_enabled = true`, both skip local-offload and hybrid routing.

---

## Phase 7: The Managed Worker

For stronger isolation, the pool can live **out of process** in a separate `duduclaw-cli-worker` subprocess, gated by `[runtime] worker_managed = true`. The gateway's `worker_supervisor` owns its lifecycle — and crucially, sequences its shutdown into the gateway's graceful-shutdown future:

```
Gateway graceful shutdown
        │
        ▼
flush prediction engine
        │
        ▼
worker_supervisor: SIGTERM ──► duduclaw-cli-worker
        │  (wait)                    │ drain in-flight
        ▼                            │
worker_supervisor: SIGKILL ──► (if still alive)
        │
        ▼
axum drains HTTP connections
```

The worker is shut down **after** the prediction-engine flush and **before** axum drains — so no work is lost and no zombie process survives the gateway.

---

## The Fallback Chain: Recoverable, Not Fatal

The most important property of the whole runtime: **every PTY path falls back to the legacy `tokio::process::Command + claude -p` on error.** A missing worker, an unhealthy pool, or a spawn failure is recoverable — not fatal.

```
acquire_and_invoke()
     │
     ├─ pool healthy?  ──no──► fall back to legacy spawn ──┐
     │                                                     │
     ├─ session spawns? ─no──► fall back to legacy spawn ──┤
     │                                                     │
     ├─ sentinel arrives? no─► fall back to legacy spawn ──┤
     │                                                     │
     ▼ yes                                                 ▼
   return framed payload                          claude -p result
```

This means turning on `pty_pool_enabled` can never make an agent *less* reliable than the legacy path. The worst case is that it degrades silently to exactly what it was before.

---

## Phase 8.5: Runtime Status Endpoint

`runtime_status.rs` exposes `GET /api/runtime/status` — a **loopback-only** JSON endpoint (non-loopback peers get 403; the loopback boundary *is* the auth). It reports live pool counters and whether the global kill switch is active.

```
$ curl http://127.0.0.1:<port>/api/runtime/status
{
  "kill_switch_active": false,
  "pool": {
    "acquires_cache_hit_total": 412,
    "acquires_spawn_total": 9,
    "evicted_idle_total": 3,
    "evicted_unhealthy_total": 0,
    "invokes_ok_total": 421,
    "invokes_empty_total": 0
  }
}
```

---

## Phase 8: Prometheus Observability

The runtime exports a family of `pty_pool_*` counters plus worker-health gauges so you can watch cache efficiency and failover behavior:

| Metric | Meaning |
|--------|---------|
| `pty_pool_acquires_cache_hit_total` | Sessions reused from the pool (warm) |
| `pty_pool_acquires_spawn_total` | Sessions freshly spawned (cold) |
| `pty_pool_evicted_idle_total` / `_unhealthy_total` / `_shutdown_total` | Three eviction reasons |
| `pty_pool_invokes_ok_total` / `_empty_total` | Invoke outcomes |
| `pty_pool_invoke_duration_*` | Round-trip duration histogram |
| `worker_health_misses_total` / `worker_restarts_total` | Managed-worker health |
| `pty_pool_managed_worker_active` | Mode gauge (worker on/off) |

A high `cache_hit` to `spawn` ratio means the pool is doing its job — most turns reuse a warm session instead of paying cold-start cost.

---

## Configuration

Everything is per-agent in `agent.toml`, default off:

```toml
[runtime]
pty_pool_enabled = true   # opt in to the interactive PTY pool (default false)
worker_managed   = true   # run the pool in an out-of-process duduclaw-cli-worker

# Interactive-REPL timeouts (stall detection + hard cap). Both optional.
pty_idle_timeout_secs        = 120   # fail fast if no substantive progress for this long (default 120)
pty_interactive_timeout_secs = 1800  # absolute wall-clock hard cap / safety net (default 1800)
```

With both `pty_*` runtime flags unset, the agent runs exactly as before on the `FreshSpawn` legacy path. Setting only `pty_pool_enabled` runs the pool in-process; adding `worker_managed` moves it into the supervised subprocess.

**Interactive-REPL timeouts.** A turn fails on whichever fires first: **stall detection** (`pty_idle_timeout_secs`) when the REPL emits no *substantive progress* — a rising token counter or new answer text; spinner animations and the elapsed-time counter deliberately don't count — for the idle window; or the absolute **hard cap** (`pty_interactive_timeout_secs`). Stall detection means a long-but-working task (multi-minute tool calls, agentic work) is no longer false-killed, while a genuinely wedged session still fails fast into the fresh-spawn `claude -p` fallback (recorded to `channel_failures.jsonl` with a `reason` of `stall`/`hard_cap`/`boot` and a `mid_task` flag). Env overrides: `DUDUCLAW_PTY_IDLE_TIMEOUT_SECS`, `DUDUCLAW_PTY_INTERACTIVE_TIMEOUT_SECS`.

---

## Why This Matters

### It Unblocks OAuth Accounts

When Anthropic blocked `claude -p` for OAuth subscriptions mid-2026, every channel reply backed by a Pro/Team/Max account would have failed. The interactive REPL path restores those accounts by driving `claude` the way a human does — no policy is bypassed, the program simply runs the way it expects to be run.

### One Code Path, Three Operating Systems

`portable-pty` (ConPTY on Windows, openpty on Unix) means the same runtime works on macOS, Linux, and Windows. The prior art it draws from was Unix-only; this is the cross-platform version.

### Deterministic Parsing

The sentinel protocol means the runtime never guesses where an answer ends. No scrollback scraping, no fragile ANSI regex — the answer arrives pre-framed between two markers.

### Safe to Turn On — With One Caveat

Default off, fails safe to `FreshSpawn`, and every PTY error degrades to the legacy `claude -p` path. On the *reliability* axis, enabling the pool can only ever match or beat the old behavior. The one caveat is **isolation, not reliability**: pool sessions are keyed without a conversation dimension, so a multi-conversation agent will bleed context across conversations (see [Known Limitation](#known-limitation-pool-sessions-are-not-per-conversation)). Because `claude -p` is still available as of 2026-07 (the programmatic-usage split was paused on 2026-06-15), most deployments should leave this off and stay on the fully-isolated `FreshSpawn` default.

### Observable

The loopback status endpoint and `pty_pool_*` Prometheus metrics make pool warmth, eviction, and worker health visible, so you can confirm the pool is actually saving cold starts.

---

## The Takeaway

Anthropic took away the ability to mail a letter per question. DuDuClaw answered by keeping a phone line open — a real PTY that makes `claude` think a human is at the keyboard, a sentinel code word so the runtime knows exactly when each answer ends, and a pool of warm sessions so most turns skip the cold start. It spans macOS, Linux, and Windows from one code path, it's off by default, and every failure quietly falls back to the way things worked before. The post office changed the rules; the conversation kept going.
