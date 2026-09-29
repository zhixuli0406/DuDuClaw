# One-shot PTY invocation (and the pool that was removed)

> Some CLIs refuse to talk to a pipe. DuDuClaw gives them a real terminal to talk to.

---

## What exists today

Every agent reply spawns the AI CLI fresh: `claude -p "<prompt>"`, run, read the answer, exit. That is the `FreshSpawn` path, and it is the only path.

Most of the time the CLI is spawned as an ordinary subprocess. But some CLIs check whether their output is going to a real terminal and refuse to run interactively when it isn't. For those, DuDuClaw allocates a **pseudo-terminal** (`portable-pty`: ConPTY on Windows 10 1809+, openpty on macOS and Linux), spawns the CLI inside it, and drains stdout until the process exits. One call, one process, no state left behind.

```
gateway
   │
   ▼
invoke_oneshot(program, args, env, cwd, deadline, clear_env)
   │
   ├─ allocate a PTY (ConPTY / openpty)
   ├─ spawn the CLI inside it — the CLI sees a TTY
   ├─ drain stdout until EOF (or the deadline kills the child)
   ▼
captured stdout
```

That is the whole feature. Current users: the Grok runtime (whose CLI insists on a TTY) and `duduclaw`'s own CLI-login helper.

Two behaviours are worth knowing:

- **`clear_env`** — when set, the child starts from an empty environment and sees only the caller's explicit allowlist (plus `NO_COLOR` / `TERM`). This is how the gateway's own vendor API keys are kept out of a spawned agent CLI (credentials doctrine P3).
- **`deadline`** — an absolute wall-clock cap. A child that hasn't exited is killed and the call returns a read-timeout error.

---

## Removed in 2026-09: the PTY session pool

Until v1.65 this page described something much larger: a pool of **long-lived interactive `claude` REPL sessions**, each framed by an in-band sentinel so the runtime could tell where an answer began and ended; a per-agent `[runtime] pty_pool_enabled` opt-in; an out-of-process `duduclaw-cli-worker` subprocess with a supervisor and a SIGTERM→SIGKILL shutdown chain; a demotion breaker; a `GET /api/runtime/status` endpoint; and a family of `pty_pool_*` Prometheus counters. Roughly 8,000 lines.

All of it is gone. The two reasons:

**1. The threat it insured against never arrived.** The pool existed so that if Anthropic blocked `claude -p` for OAuth-subscription accounts, flipping one flag would keep channel replies working. Anthropic scheduled exactly that change for 2026-06-15 — and paused it on the day. Fifteen months later `claude -p` still works for OAuth subscriptions, and the insurance had been costing maintenance on every refactor that crossed the reply path.

**2. It could not be turned on safely anyway.** Pool sessions were keyed by `(agent, cli_kind, bare_mode, account, model)` — with **no conversation dimension**. One agent serving two WebChat conversations shared a single live REPL, and that REPL remembered its own prior turns, so conversation B could see conversation A's workflow state. The page said so itself, in a section headed "read this before enabling." A feature nobody could responsibly enable is not standby; it is unfinished work with a flag on it.

There was also a field incident: a dashboard bug wrote `pty_pool_enabled = true` into agents' config without consent, which put production installs on the interactive path, where a contended single OAuth account stalls. That needed a one-time startup migration (`wp10-pty-default-reset`) to undo. That migration is removed too — every install that had it has long since run it, and the settings it corrected no longer exist.

### What this changes for you

Nothing, unless you had explicitly opted in. If your `agent.toml` still carries `[runtime] pty_pool_enabled` / `worker_managed` / `pty_idle_timeout_secs` / `pty_interactive_timeout_secs`, those keys are now ignored — unknown keys are tolerated, so nothing breaks; delete them at your convenience. The `DUDUCLAW_DISABLE_PTY_POOL` kill switch, the `/api/runtime/status` endpoint, and the `pty_pool_*` / `worker_*` metrics are gone. `DUDUCLAW_PTY_DISABLE_RETRY` still exists for the one-shot path.

### If Anthropic re-activates the split

Rebuild it — deliberately, with a conversation key in the session identity from the first commit. The design notes live in `commercial/docs/runtime-pty-pool-design.md`, and the removed implementation is in git history at the v1.65 tag.

---

## The Takeaway

DuDuClaw drives CLIs through a real pseudo-terminal where one is required, one spawn per call. The pool of persistent REPL sessions that used to sit on top of this was insurance against a policy change that was paused and never resumed, and it had a context-bleed defect that made it unsafe to enable. Keeping an 8,000-line standby honest costs more than rebuilding it if the day comes.
