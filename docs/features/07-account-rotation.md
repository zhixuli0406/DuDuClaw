# Multi-Account Rotation

> Intelligent API credential scheduling — never hit a rate limit again.

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
     +---> Error
             Unexpected failures (network, auth, server)
             Cooldown: exponential backoff
```

When an account enters a cooldown state, the rotation strategy automatically skips it and uses the next available account. When the cooldown expires, the account is automatically restored to the rotation pool.

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

## Interaction with Other Systems

- **Confidence Router**: Queries routed to local inference don't consume any API account, extending quota lifetime.
- **CostTelemetry**: Provides the data that informs budget enforcement and cache efficiency feedback.
- **Direct API**: Provides a high-cache-hit bypass for simple queries.
- **Dashboard**: Shows real-time account health, usage, and remaining budget.

---

## The Takeaway

API credentials are a limited resource. Multi-account rotation treats them like a managed fleet — automatically selecting the best available option, cooling down overloaded accounts, enforcing budgets, and shifting to local inference when cloud usage is inefficient.
