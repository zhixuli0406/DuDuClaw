# Multi-Account Rotation & Cross-Provider Failover

> Intelligent credential scheduling across Claude, Codex, Gemini — never hit a rate limit again.

---

## The Metaphor: A Family of Credit Cards

Your household has multiple credit cards:
- **Card A** (wife's): No annual fee, 2% cashback, $5,000 limit
- **Card B** (yours): Higher limit ($10,000), but charges a fee per transaction
- **Card C** (emergency): Highest limit ($20,000), highest fees, only used when others are maxed out

A smart family uses Card A first (cheapest), switches to Card B when Card A's limit is reached, and only touches Card C in emergencies.

DuDuClaw's account rotation does the same thing with API credentials — automatically, in real-time, with health monitoring and cooldown logic.

---

## How It Works

### Account Types

The system supports two types of API credentials:

**OAuth Sessions** — Linked to subscription plans (Pro, Team, Max). These typically include a monthly quota of free API calls as part of the subscription. They're the "cashback cards" — use them first.

**API Keys** — Pay-per-token. No quota limit, but every call costs money. They're the "emergency cards" — reliable but expensive.

### The Four Strategies

Operators choose one rotation strategy that governs how accounts are selected:

**Priority** — Accounts are ranked by a priority number. The system always uses the highest-priority account that's healthy. Think of it as a VIP list: #1 gets all the work until they can't handle it, then #2 takes over.

```
Request arrives
     |
     v
Try Account #1 (priority: 1)
     |
  +--+--+
  |     |
Healthy  Unhealthy
  |      |
  v      v
Use it   Try Account #2 (priority: 2)
         |
      +--+--+
      |     |
   Healthy  Unhealthy
      |      |
      v      v
   Use it   Try Account #3 ...
```

**LeastCost** — Prefers the cheapest option first. OAuth accounts (included in subscription) come before API keys (pay-per-token). Among accounts of the same type, it prefers the one with the most remaining quota.

```
Request arrives
     |
     v
Any healthy OAuth accounts with remaining quota?
     |
  +--+--+
  |     |
 Yes    No
  |     |
  v     v
Use     Any healthy API key accounts?
cheapest     |
OAuth     +--+--+
          |     |
         Yes    No
          |     |
          v     v
       Use API  All accounts
       key      exhausted (error)
```

**RoundRobin** — Distributes requests evenly across all healthy accounts. This prevents any single account from being overloaded and spreads the usage (and cost) uniformly.

**Failover** — Designates one account as the primary and all others as backups. The primary handles 100% of traffic unless it becomes unhealthy. Simple and predictable.

### Health Tracking

Each account has an independent health status:

```
Account Health States:
     |
     +---> Healthy
     |       Everything working normally
     |
     +---> Rate-Limited
     |       Too many requests in a short period
     |       Cooldown: 2 minutes
     |
     +---> Budget-Exhausted
     |       Monthly spending limit reached
     |       Cooldown: 24 hours (until next billing cycle)
     |
     +---> Token-Expiring
     |       OAuth token approaching expiration
     |       Warning at: 30 days and 7 days before expiry
     |
     +---> Auth-Dead
     |       Anthropic rejected the credential itself —
     |       an invalid/expired token, or the organization
     |       has disabled Claude Code subscription access
     |       Cooldown: 15 minutes, doubling on every repeat
     |       failure up to a 6-hour cap
     |
     +---> Broken
     |       The stored credential can't be decrypted (or
     |       decrypts to nothing) — excluded from rotation
     |       at startup rather than spawning credential-less
     |       runs
     |
     +---> Error
             Other unexpected failures (network, server)
             Cooldown: exponential backoff
```

When an account enters a cooldown state, the rotation strategy automatically skips it and uses the next available account. When the cooldown expires, the account is automatically restored to the rotation pool — except an Auth-Dead account, which only comes back early via a real credential check succeeding (see below) or by saving a fresh credential for it. A Broken account never comes back on its own; the credential has to be re-saved.

### Real Credential Checks, Not Guesses

Restoring an account used to mean running `claude auth status` and trusting its `loggedIn: true`. That check only proves *some* `CLAUDE_CODE_OAUTH_TOKEN` is sitting in the environment — it says nothing about whether *this account's* token still authenticates, and a revoked or organization-disabled token can keep reporting "logged in" indefinitely.

For OAuth accounts that store their own token and for API-key accounts, the rotator now probes the credential directly with a zero-cost call to Anthropic's `GET /v1/models`, using that exact account's secret:

- **200** — the credential works. The account is restored and its failure count resets.
- **401** — the token itself is invalid. The account stays parked.
- **403** — the organization has disabled Claude Code subscription access. The account stays parked.
- **429, or a network error** — inconclusive. The account is left untouched and re-checked next cycle.

A credential the API has conclusively rejected is re-checked on a backoff rather than once every cycle forever: one minute, then two, four, eight, sixteen, capped at thirty. An inconclusive answer never slows the schedule down, and a working credential or a fresh authentication failure from a real request clears it immediately.

Accounts that rely on a keychain login (no stored token to hand the probe) keep using `claude auth status`, since there's no per-account secret to check directly — but once such an account has gone Auth-Dead, this check can no longer resurrect it early; it still has to wait out the cooldown.

### Verifying Credentials Before They're Saved

Adding an account — from the dashboard or via `accounts.add` — now runs the same check before anything is written:

- A rejected credential (401) is refused outright.
- An organization-disabled credential (403) is refused with guidance to switch to an API key or contact the organization admin.
- A short-lived access token (`sk-ant-at01-…`, what `claude auth token` prints) is refused with a pointer to `claude setup-token`, which produces the long-lived `sk-ant-oat01-…` token the rotator actually wants.
- If the check can't complete at all — offline, most likely — the account is still saved, just flagged unverified until the next health cycle confirms it.

### Checking Credentials From the Terminal

`duduclaw doctor` prints one line per Anthropic account that stores its own token or key: valid, invalid token (401), organization disabled (403), or unreachable. It runs the same zero-cost check as the rotator and changes nothing, so it is safe to run against a live gateway. Accounts that rely on a keychain login are skipped — there is no secret here to check. An unreachable network is reported as a warning, never as a dead credential.

The `claude auth status` line above it now carries a caveat, because that check only proves a login file or environment variable exists — not that the token still authenticates.

### Auth Outage Alert

If every account in the pool is failing on authentication at the same time, that's not a per-account cooldown — it's a platform-wide outage. DuDuClaw posts one Activity Feed event and sends one notification to the affected agent's channel, explaining that scheduled work and replies are paused and pointing at the dashboard's account settings. It stays quiet for as long as the outage continues (no repeat pings), then sends exactly one recovery notice the moment any account authenticates successfully again.

### Budget Enforcement

Each account can have a monthly spending cap:

```
Before sending a request:
     |
     v
Estimate cost of this request
  (based on input tokens + expected output tokens)
     |
     v
Would this exceed the account's monthly budget?
     |
  +--+--+
  |     |
 No     Yes
  |     |
  v     v
Send   Skip this account,
       try next in rotation
```

This prevents surprise bills. Operators set budgets per account, and the system enforces them automatically. When an account's budget is exhausted, it enters the 24-hour cooldown and waits for the next billing cycle.

---

## Integration with Cache Efficiency

The account rotation system works hand-in-hand with cache efficiency tracking:

```
CostTelemetry calculates:
  cache_efficiency = cache_read / (input + cache_read + cache_creation)

If cache_efficiency < 30%:
  "We're paying full price for most tokens.
   Consider routing more queries to local inference."
     |
     v
  Automatically increase preference for local models
  in the Confidence Router
```

This creates a feedback loop: when cloud API usage is inefficient (low cache hit rates), the system automatically shifts more traffic to local inference, preserving API quota for queries that benefit from caching.

---

## The Direct API Shortcut

For scenarios where the full Claude CLI pipeline isn't needed (simple chat responses), the system offers a **Direct API** mode that calls the Anthropic Messages API directly:

```
Simple chat query
     |
     v
Direct API client (singleton HTTP client)
     |
     v
Add system prompt with cache hint
  (tells the API server to cache this prompt)
     |
     v
API response
```

Because the system prompt is cached, subsequent calls with the same system prompt hit the cache instead of re-processing it. This achieves 95%+ cache hit rates for repetitive conversations, dramatically reducing effective cost.

---

## Why This Matters

### Uninterrupted Service

Rate limits are a fact of life with API services. Without rotation, a rate limit means your agent stops responding. With rotation, it means traffic seamlessly shifts to the next available account while the rate-limited one cools down.

### Cost Optimization

The LeastCost strategy ensures free quota (from subscriptions) is consumed first. Paid API calls only happen when free options are exhausted. For most users, this means the bulk of their API usage costs nothing beyond the subscription fee.

### Budget Control

Monthly caps per account prevent runaway spending. Combined with the CostTelemetry dashboard, operators have full visibility into where every token goes and how much it costs.

### Hands-Off Operation

The entire system is automatic. Once configured, operators don't need to manually switch accounts, monitor rate limits, or rebalance traffic. The rotation strategy handles it all.

---

## Cross-Provider Failover

With DuDuClaw's Multi-Runtime architecture (Claude / Codex / Gemini / OpenAI-compat), account rotation extends across providers. The **FailoverManager** coordinates cross-provider health:

```
Primary provider (Claude) rate-limited
     |
     v
FailoverManager checks alternatives:
  - Codex CLI available? → Route there
  - Gemini CLI available? → Route there
  - Local inference available? → Route there
     |
     v
Non-retryable error? (auth failure, billing suspension)
  → Mark provider as unhealthy, longer cooldown
Retryable error? (timeout, temporary server error)
  → Short cooldown, retry soon
```

### Channel Failure Tracking

When a channel reply fails (the user-facing path), the system records structured failure data:

```
Failure record → ~/.duduclaw/channel_failures.jsonl:
  {
    "ts": "2026-04-15T10:30:00Z",
    "agent": "dudu",
    "channel": "telegram",
    "reason": "RateLimited",      // or Billing, Timeout, BinaryMissing, etc.
    "account": "oauth-pro-1",
    "message_zh": "API 用量已達上限..."
  }
```

Failure categories render **category-specific zh-TW messages** instead of the old generic "please run `claude auth status`" hint. The failure log feeds into the dashboard for observability.

### CLI Binary Discovery

DuDuClaw runs as a system service (via `duduclaw service install`), which means `PATH` may not include the AI CLI binary locations. The `which_claude()` function probes:

- Homebrew (Intel + Apple Silicon paths)
- Bun global installs
- Volta toolchain
- npm global
- `.claude/bin`
- `.local/bin`
- asdf shims
- NVM version directories

This ensures launchd/systemd-launched gateways discover the CLI binary without depending on `PATH` inheritance.

---

## Interaction with Other Systems

- **Multi-Runtime**: Account rotation works across Claude, Codex, Gemini, and OpenAI-compat providers.
- **Confidence Router**: Queries routed to local inference don't consume any API account, extending quota lifetime.
- **CostTelemetry**: Provides the data that informs budget enforcement and cache efficiency feedback.
- **FailoverManager**: Coordinates cross-provider health tracking and failover decisions.
- **Direct API**: Provides a high-cache-hit bypass for simple queries.
- **Dashboard**: Shows real-time account health, usage, remaining budget, and channel failure logs.

---

## The Takeaway

API credentials are a limited resource — and in a multi-provider world, they're a *fleet* of limited resources. Multi-account rotation treats them like a managed fleet — automatically selecting the best available option across providers, cooling down overloaded accounts, enforcing budgets, and shifting to local inference when cloud usage is inefficient.
