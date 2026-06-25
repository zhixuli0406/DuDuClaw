# TODO: Gemini CLI → Antigravity CLI (`agy`) Migration

> Tracks DuDuClaw's adoption of Google's **Antigravity CLI** (`agy`), the
> 2026-06-18 successor to the personal-tier Gemini CLI. Default-off, fail-closed,
> backward-compatible: the legacy `gemini` backend is retained for paid
> `GEMINI_API_KEY` / enterprise users whose access continues.
> `[x]` = done, `[ ]` = pending.

Legend: **(test)** = ships with unit test(s). **(wire)** = integration/plumbing.
**(smoke)** = needs a live `agy` run to confirm.

---

## Background — is the deprecation real? (verified 2026-06-25)

Yes. Confirmed against Google's Developers Blog and `google-gemini/gemini-cli`
Discussion #27274:

- **2026-06-18**: the personal-tier `gemini` CLI + Gemini Code Assist IDE
  extensions stop serving requests for **free / AI Pro / Ultra** accounts.
- **Still works**: Code Assist **Standard / Enterprise** licenses, and the paid
  **`GEMINI_API_KEY`** path — so DuDuClaw's existing Gemini backend does **not**
  break for paid-key users.
- **Replacement**: `agy`, a Go single-binary (no Node runtime). Installs to
  `~/.local/bin/agy`; config under `~/.gemini/antigravity-cli/`. Multiplexes
  Gemini 3.x + Claude 4.6 + GPT-OSS models. API key env: `ANTIGRAVITY_API_KEY`.

## `agy --help` ground truth (v1.0.12, smoke-tested 2026-06-25)

```
-p / --print <prompt>           Single prompt, non-interactive. Takes the prompt as its VALUE.
--prompt                        Alias for --print
--dangerously-skip-permissions  Auto-approve all tool permission requests without prompting
--model <id>                    Model for the current CLI session
--add-dir <path>                Add a directory to the workspace (repeatable)
--print-timeout <dur>           Timeout for print mode wait (default 5m0s)
--project / --new-project       Project selection / creation
--sandbox                       Run in a sandbox with terminal restrictions
-c / --continue, --conversation Resume prior conversation
```

Two non-obvious gotchas found by live testing (both fixed in the runtime):

1. **`-p` consumes the next argv token as the prompt.** `agy -p` alone errors
   "flag needs an argument: -p". So `agy -p --foo "real prompt"` makes `--foo`
   the prompt and drops "real prompt". Fix: emit all other flags first and
   `-p <payload>` LAST. (Symptom before the fix: agy "answers" by explaining
   whatever flag immediately followed `-p`.)
2. **Untrusted workspace hangs headless.** Running in / `--add-dir`-ing a dir not
   in `trustedWorkspaces` triggers an interactive "trust this workspace?" prompt
   that blocks forever with no TTY. `--dangerously-skip-permissions` does NOT
   cover workspace trust. Fix: pre-seed the agent dir into
   `~/.gemini/antigravity-cli/settings.json → trustedWorkspaces` before spawning.

Notably **absent**: any `--output-format`/JSON surface and any `--system` flag.
→ plain stdout text capture is the only option (token stats unavailable);
system prompt + history are embedded inside the prompt argument.

---

## ✅ Round 1 (2026-06-25): backend landed

- [x] `RuntimeType::Antigravity` added (`duduclaw-core/src/types.rs`); parses
      from `provider = "antigravity"` or `"agy"`. **(test)**
- [x] `AntigravityRuntime` (`duduclaw-gateway/src/runtime/antigravity.rs`) —
      `agy -p --dangerously-skip-permissions --print-timeout 300s [--model X]
      [--add-dir agent_dir] <payload>`. Binary auto-resolve (PATH → `~/.local/bin/agy`).
      System prompt + history embedded via `build_prompt` (CJK-safe `truncate_bytes`,
      64KB cap, leading-`-` neutralized). **(test)** — 5 unit tests on `build_prompt`.
- [x] Registry auto-detect via `agy --version` (`runtime/mod.rs`). **(wire)**
- [x] Vision capability arm — Gemini + Claude families multimodal
      (`model_capabilities.rs`). **(test)**
- [x] `VALID_RUNTIME_PROVIDERS` accepts `antigravity` (`handlers.rs`). **(wire)**
- [x] MCP config writer → `~/.gemini/antigravity-cli/settings.json` (or per-agent
      `agent_dir/.gemini/antigravity-cli/settings.json`).
- [x] Direct `agy` smoke: confirmed `-p "prompt"` returns plain text, exit 0. **(smoke)**
- [x] Fix #1 — `-p` placed LAST with payload as its value (was: `-p` first →
      swallowed the next flag as the prompt). **(smoke)**
- [x] Fix #2 — `ensure_workspace_trusted()` pre-seeds the agent dir into
      `trustedWorkspaces` under `with_file_lock` (was: untrusted dir → headless
      hang on the interactive trust prompt). **(smoke)**

## ✅ Round 2 (2026-06-25): end-to-end & polish

- [x] End-to-end runtime test — `AntigravityRuntime::execute` against real `agy`
      (`#[ignore]`, env-gated `DUDUCLAW_AGY_E2E=1`), in a non-home temp dir to
      exercise the trust pre-seed. **PASSED** — agy returned `PONG`, no hang. **(smoke)(test)**
- [x] `[runtime] provider = "antigravity"` example block added to all five
      agent.toml templates (`orchestrator/evaluator/restaurant/manufacturing/trading`),
      each pointing here. **(wire)**
- [x] Provider documented in `docs/guides/development-guide.md` §1.4
      (Multi-Runtime table + agy-specific notes).
- [x] Token accounting — chose **(b)**: estimate via the gateway's CJK-aware
      `prompt_compression::estimate_tokens` on payload (input) + response (output),
      so CostTelemetry is non-zero. Marked as heuristic in code. **(wire)**

## ✅ Round 3 (2026-06-25): PTY-pool / worker wiring (unbind from Claude)

Scope chosen: **mechanical wiring + oneshot routing** (not a full agy interactive
REPL — agy has no system-prompt flag for the sentinel bootstrap, and `agy -p`
works, so the persistent REPL is unnecessary for it).

- [x] `CliKind::Antigravity` in `duduclaw-cli-runtime` (`session.rs`) —
      enum / `as_str` / `parse` (`"antigravity"` + `"agy"` alias) / round-trip test.
      `inject_protocol_args` is a documented no-op (no system-prompt flag). **(test)**
- [x] Binary discovery in `duduclaw-core`: generic `which_cli` / `which_cli_in_home`
      + `which_codex` / `which_gemini` / `which_agy` (+ `_in_home`). 2 unit tests. **(test)**
- [x] `resolve_program` (`pty_runtime.rs`) now resolves **all four** CliKinds via
      their `which_*_in_home` — no more `None` for Codex/Gemini. **(wire)**
- [x] Worker `spawn_session_default` (`cli-worker/server.rs`) resolves all four
      kinds (no more wildcard reject); error message names the actual CLI. **(wire)**
- [x] `cli_kind_for_provider(RuntimeType) -> Option<CliKind>` mapping
      (`pty_runtime.rs`); **both** hardcoded `CliKind::Claude` PtyPool acquire sites
      (`claude_runner.rs`, `channel_reply.rs`) now derive the kind from the agent's
      `[runtime] provider`. **(wire)**

**Architecture note:** non-Claude providers are short-circuited to the oneshot
`runtime_dispatch` path *before* the PtyPool branch in both call sites
(`non_claude_provider` guards), so the derived kind is Claude in practice today —
but the literal coupling is gone, and the pool/worker layer now has real call
points for all four CLIs. A *validated interactive REPL* for Codex / Gemini /
Antigravity (boot dance + ANSI/chrome framing) remains future work; only Claude's
is implemented.

### Interactive REPL boot dance / framing — investigated, NOT pursued (2026-06-25)

Reconned with a real-PTY harness (`scratchpad/agy_recon.py`). Findings make the
sentinel-on-rolling-buffer PtyPool protocol a poor fit and unnecessary:

- **codex / gemini binaries are not installed** on the dev host → their interactive
  TUIs cannot be observed or verified. Writing boot-dance / chrome-marker / ready-marker
  parsing for a TUI you can't run is blind guessing — exactly the "misleading
  half-wiring" the project forbids (fail-closed, validate the artifact that takes
  effect). Not written.
- **agy's interactive TUI is a heavy full-screen alt-screen app** (`\x1b[?1049h`,
  cursor-hide, bracketed paste, `\x1b[21A` cursor-up redraws, `\x1b[2J`/`\x1b[K`
  full repaints — a React/Ink-style renderer like the Gemini/Claude TUIs). The
  PtySession protocol APPENDS to a rolling buffer and does positional sentinel
  pairing; a TUI that repaints the whole screen via cursor-up + clear-line
  corrupts that. Boot was also very slow (>45s before the banner rendered;
  `Antigravity CLI 1.0.12` + account + `Gemini 3.5 Flash` only appeared after the
  first 45s window). And agy has **no system-prompt flag**, so the sentinel
  bootstrap can't be installed persistently — only prepended per-turn, which the
  model is not guaranteed to honor across a reused session.
- **It's unnecessary**: `agy -p` (oneshot) works and is already wired
  (`AntigravityRuntime` + the oneshot routing). The whole reason the interactive
  PtyPool exists is Anthropic blocking `claude -p` for OAuth — no equivalent block
  exists for agy/gemini/codex.

**Decision**: keep oneshot as the terminal answer for non-Claude CLIs. Revisit only
if (a) a non-Claude CLI blocks its `-p`/oneshot mode the way Anthropic did, AND
(b) the binary is available to reverse-engineer + regression-test its TUI.

### Deferred (explicit decisions — not pending work)

- **Upstream watch**: if `agy` ships a structured/JSON output mode, switch the
  runtime off plain-text capture to recover real token stats + tool-call events.

## Risks / notes

- `agy` will spin up a default `~/.gemini/antigravity-cli/scratch/` workspace if
  no dir is given — mitigated by `--add-dir agent_dir` + `current_dir`.
- `--dangerously-skip-permissions` is required (subprocess has no TTY) and is a
  real, documented flag. NOTE: an early symptom where agy appeared to "explain
  the flag instead of answering" was NOT a model quirk — it was Fix #1 above
  (`-p` swallowing the flag as the prompt). With correct ordering agy answers
  the real prompt (verified: returns `PONG`).
- agy is agentic even in print mode — it may create files (e.g. a `readme.md`) in
  the workspace as a side effect. Benign, but expect writes under `agent_dir`.
- Wrapper timeout (330s) is kept above `--print-timeout` (300s) so `agy`
  self-bounds first and the hard kill is only a backstop.
