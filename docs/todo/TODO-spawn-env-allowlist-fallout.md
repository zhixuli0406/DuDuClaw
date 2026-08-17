# TODO — The spawn-env allowlist silently breaks anything that relied on inheritance

**Owner:** — &nbsp; **Status:** ✅ fixed 2026-08-17 (see below; proposed fix 1 narrowed)
**Last updated:** 2026-08-17 &nbsp; **Severity:** 🔴 CRITICAL (one instance put a real-money agent on a simulated broker)

## Fix shipped (2026-08-17)

- **Failure 1** (container auth): `detect_default_oauth_session` captures
  `CLAUDE_CODE_OAUTH_TOKEN` onto the account when the session came from the
  env var, so the explicit-injection path carries it. Regression test
  `setup_token_account_injects_the_token_into_spawn_env` (was already in the
  working tree from the incident session).
- **Proposed fix 2** (fail closed on the mode switch): `ML_MCP_BACKEND` now has
  **no default** — absent or unknown ⇒ `build_backend()` refuses to start, and
  `server_start` preflights the backend so the refusal happens at startup, with
  the journal recording the actual selection (never an assumed "mock").
  Experiment suite 129/129 green. Container redeploy pending.
- **Proposed fix 3** (log the scrub): `spawn_env.rs` warns once per process —
  names only, never values — listing `DUDUCLAW_*` vars present in the gateway
  env that the allowlist will drop, with the remediation (`.mcp.json` env
  block / allowlist). Foreign namespaces stay unlisted (dropping them is the
  scrub working as designed).
- **Proposed fix 4**: `DUDUCLAW_SEMANTIC_VECTORS` added to the allowlist
  (feature flag, not a credential). `DUDUCLAW_PTC_SOCKET` needs **no** change —
  re-reading the sweep, it is injected explicitly at the sandbox spawn site
  (`ptc/sandbox.rs`), not inherited.
- **Proposed fix 1** deliberately narrowed: `.mcp.json` for the duduclaw MCP
  server already carries its required env; DuDuClaw cannot know an arbitrary
  third-party server's env contract, so the product answer is the fix-3 warning
  (silent → loud) plus fail-closed switches on the consumer side (fix 2), not
  generated env blocks for servers we don't own.
- **P3 guarantee preserved**: `allowlist_never_carries_a_secret_shaped_name` +
  vendor-key absence tests still pass; nothing secret-shaped was added.

## Problem

v1.61.0 replaced the spawn environment with an allowlist
(`duduclaw-core/spawn_env.rs`, credentials P3). The security goal is right:
a spawned CLI should receive credentials as explicit data, never as ambient
state. But **every consumer that was quietly relying on inheritance broke at
once, and each one fails silently in a different disguise.**

Two hit production within hours of the release. Neither reported anything that
pointed at the cause.

## Failure 1 — gateway dispatch could not authenticate at all

`detect_default_oauth_session` cannot distinguish a keychain session from a
`setup-token` one: `claude auth status` reports `loggedIn: true` for both. It
synthesized an account with `oauth_token: None`, assuming the child would find
its own credentials. Post-scrub the child gets nothing.

Symptom: every dispatch died `authentication_failed`, while `claude -p` run
manually **in the same container** worked (an interactive shell still inherits
the token). That contradiction is what made it hard to see.

**Blast radius: every container deployment**, since `setup-token` is how they
all authenticate.

Fixed in `account_rotator.rs`: capture `CLAUDE_CODE_OAUTH_TOKEN` when the
session came from the environment and put it on the account, so the existing
injection path carries it. Regression test:
`setup_token_account_injects_the_token_into_spawn_env`.

## Failure 2 — a real-money agent silently ran against a mock broker

`.mcp.json` only ever declared `ML_MCP_STATE_DIR`; the other fourteen `ML_*`
variables reached the MCP server by inheritance. After the scrub the server
read none of them and fell back to its defaults:

```python
journal("server_start", {"backend": os.environ.get("ML_MCP_BACKEND", "mock")})
```

**The default is `mock`.** For most of a live trading session the agent was
talking to a simulated matching engine while believing it was live. Three of
those `server_start backend=mock` rows came from real scheduled runs.

Two other variables went with it:

| Lost | Consequence |
|---|---|
| `ML_MCP_BACKEND` | agent saw a simulated account |
| `ML_AVAILABLE_CASH_TWD` | real buys blocked as `cash_unverifiable` (fail-closed — this is why no damage occurred) |
| `ML_TG_BOT_TOKEN` / `ML_TG_CHAT_ID` | per-trade Telegram alerts stopped firing |

Risk caps happened to have identical in-code defaults, so those were not
loosened. That was luck, not design: a future default that is more permissive
than the configured value would widen a risk gate with no signal at all.

Worked around by writing all fourteen into `.mcp.json` (explicit env is not
subject to the allowlist), 0600, both agents. Verified from a deliberately
clean environment: `backend: masterlink-nova`, `is_sim: False`.

## Sweep result

All 35 `DUDUCLAW_*` reads were checked against the allowlist. Only two are read
**inside a spawned MCP server** and absent from it:

| Variable | Behaviour when dropped | Severity |
|---|---|---|
| `DUDUCLAW_PTC_SOCKET` | explicit `RuntimeError` — loud, fail-closed | low |
| `DUDUCLAW_SEMANTIC_VECTORS` | feature silently stays off even when the operator set `=1` | medium |

The remaining 23 are read only in the gateway process and are unaffected.
`.mcp.json` already passes the five the duduclaw MCP server needs, which is why
that server never broke.

## The real defect

Not any single missing variable — **the allowlist has no counterpart on the
consumer side.** Nothing enforces that a spawned child's requirements are
declared, and nothing notices when one is missing. Every case degrades to a
default, and the defaults were written for a world where inheritance worked.

`mock` as the default broker is the sharpest edge: a safety-critical switch
whose fallback is the *wrong* mode rather than a refusal.

## Proposed fix

1. **Generate `.mcp.json` with the env a server needs.** `create_agent` (and
   the team-template path) should write the backend/config variables into the
   server's `env` block instead of trusting inheritance. Retrofit existing
   agent files on gateway boot.
2. **Fail closed on a missing critical switch.** `ML_MCP_BACKEND` should have
   no default: absent ⇒ refuse to start, not "mock". Same discipline for any
   variable that selects between simulated and real behaviour.
3. **Log the scrub.** When the allowlist drops variables that the process being
   spawned is known to read, warn once with their names. Today the drop is
   perfectly silent — that is what made both failures take hours instead of
   minutes.
4. **Add `DUDUCLAW_SEMANTIC_VECTORS` to the allowlist** (a feature flag, not a
   credential), or pass it explicitly in `.mcp.json`.

## Acceptance

- A fresh container deployment dispatches successfully with no hand-editing.
- An MCP server that requires a backend selector refuses to start without one,
  and says so.
- Setting `DUDUCLAW_SEMANTIC_VECTORS=1` on the gateway takes effect in spawned
  MCP servers.
- The P3 guarantee is preserved: no `*_TOKEN` / `*_API_KEY` is ever inherited
  ambiently.

## Related code

- `crates/duduclaw-core/src/spawn_env.rs` — the allowlist and its assertions
- `crates/duduclaw-agent/src/account_rotator.rs` — `detect_default_oauth_session`
- `crates/duduclaw-cli/src/mcp.rs` — `.mcp.json` generation
- `commercial/experiments/lwm-trading-2026-08/mcp-masterlink/src/mcp_masterlink/server.py` — the `mock` default
