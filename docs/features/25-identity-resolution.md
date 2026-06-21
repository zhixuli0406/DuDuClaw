# Identity Resolution

> The single source of truth for "who is this person" — one provider trait, three backends, and a `<sender>` block the agent reads on every turn.

---

## The Metaphor: The Receptionist and the Directory

Picture a building with a receptionist at the front desk. When a visitor walks in, the receptionist doesn't guess who they are. They check the corporate directory — the authoritative system that says "this is Ruby Lin, customer PM, cleared for projects Alpha and Beta."

But the directory server occasionally goes down for maintenance. A good receptionist keeps a printed roster in a drawer — a cached copy from the last time the directory was reachable. When the live system is offline, they fall back to the printed sheet rather than turning everyone away at the door.

And once the receptionist has identified you, they don't make every department re-verify you. They clip a **visitor badge** to your lapel that states your name, role, and which floors you may enter. Every department you visit reads the badge instead of re-checking the directory.

DuDuClaw's Identity Resolution is exactly this:

- The **directory** is the upstream provider (`NotionIdentityProvider`).
- The **printed roster in the drawer** is the wiki cache (`WikiCacheIdentityProvider`).
- The **receptionist's fall-back logic** is the `ChainedProvider` (live → cache).
- The **visitor badge** is the `<sender>` block injected into the agent's system prompt — resolved once, read every turn.

---

## The Problem Being Solved

Before RFC-21 §1, a DuDuClaw agent had no way to ask "who is this person talking to me?" The only mechanism available was to call `shared_wiki_read` on a hand-known path like `identity/discord-users.md`. That file listed two people. Everyone else — team members, customer contacts, engineers — was an invisible stranger.

Agents declared rules in SOUL.md like "reject non-project members," but they had no roster to evaluate that rule against and no mechanism to query the authoritative source. The boundary lived in prose, not in data.

The fix is **system-layer authority, not prompt-layer suggestion**: introduce an `IdentityProvider` trait, demote the wiki from source-of-truth to a transparent cache, and feed the resolved person into the system prompt as structured data the agent already has — so a SOUL.md rule becomes evaluable instead of aspirational.

---

## The Provider Trait

`IdentityProvider` (in the `duduclaw-identity` crate) is a small async trait. All methods are async because production providers (Notion, LDAP, custom HTTP) involve network IO, but a purely local implementation conforms to the same surface so it can be swapped in or out without changing call sites.

```
#[async_trait]
trait IdentityProvider: Send + Sync {
    async fn resolve_by_channel(channel, external_id)
        -> Result<Option<ResolvedPerson>, IdentityError>;

    async fn lookup_project_members(project_id)
        -> Result<Vec<ResolvedPerson>, IdentityError>;

    fn name(&self) -> &str;   // "notion" / "wiki-cache" / "chained"
}
```

### The `Ok(None)` semantic

A crucial design decision: `resolve_by_channel` returns `Ok(None)` when the person is unknown. That is the normal "a stranger sends a message" case and explicitly **not** an error. `Err` is reserved for genuine provider failures — an unreachable upstream, a malformed payload, an IO fault. This distinction is what lets the chained provider degrade gracefully.

---

## The Three Providers

| Provider | Source | Behaviour |
|----------|--------|-----------|
| `WikiCacheIdentityProvider` | `<home>/shared/wiki/identity/people/*.md` | Reads YAML-frontmatter records from local Markdown. A single malformed file is skipped (with a `tracing::warn!`), never taking the whole resolver down. |
| `NotionIdentityProvider` | Notion `databases/query` API | Queries a People DB row-per-person; operator maps logical fields to Notion property names via a configurable `field_map`. 5xx/network → `Unreachable`; 4xx/schema → `Malformed`. |
| `ChainedProvider` | cache → upstream | Tries cache first; on a miss, falls through to upstream; on upstream outage, degrades to a no-resolve instead of erroring. |

### WikiCacheIdentityProvider schema

Each `*.md` file under `identity/people/` carries a YAML frontmatter block; the body is free-form notes the provider ignores:

```
---
person_id: person_2f9
display_name: Ruby Lin
roles: [customer-pm]
project_ids: [proj-alpha, proj-beta]
emails: [ruby@example.com]
channel_handles:
  discord: "1234567890"
  line: "Uabc"
---

Free-form notes about Ruby — never read by the provider.
```

### NotionIdentityProvider field map

Notion property names vary per deployment, so the operator declares a `NotionFieldMap`. Defaults match a sensible convention but every field is overridable:

```
field_map = {
  name     = "Name",
  roles    = "Roles",
  projects = "Projects",
  channel_props = {
    discord  = "Discord ID",
    line     = "Line ID",
    telegram = "Telegram ID",
    email    = "Email",
  },
}
```

Each `resolve_by_channel` call queries `databases/query` with a filter narrowing to records whose channel-handle property equals the `external_id`.

---

## The ChainedProvider Fallback

The `ChainedProvider` is the receptionist's fall-back brain. It wraps a fast cache and a slow authoritative upstream:

```
resolve_by_channel(channel, external_id)
        |
        v
  ┌─────────────────────────────┐
  │ 1. Cache fast path          │
  │    cache.resolve(...)       │
  └─────────────────────────────┘
        |
   Ok(Some) ──────────────► return cached person (short-circuit)
        |
   Ok(None) [miss]          Err [cache fault]
        |                        |
        |   warn! "cache error — falling through"
        v                        v
  ┌─────────────────────────────┐
  │ 2. Upstream slow path       │
  │    upstream.resolve(...)    │
  └─────────────────────────────┘
        |
   Ok(person) ────────────► return upstream result
        |
   Err [upstream outage]
        |
   warn! "upstream error — degrading to no-resolve"
        v
   return Ok(None)   ← agent treats sender as a stranger, NOT a hard error
```

The key property: when Notion is unreachable, the channel reply still proceeds. The agent simply sees no `<sender>` and treats the message as coming from a stranger — exactly the receptionist falling back to the printed roster, never locking the front door.

`lookup_project_members` inverts the preference: it queries **upstream first** because project membership is precisely the kind of data that drifts in a cache. Only on an upstream error does it fall back to the cache (and emits a `tracing::warn!` so operators notice the degradation).

---

## The ResolvedPerson Record

A successful resolution returns a canonical `ResolvedPerson`. Only an upstream provider produces these; downstream callers receive them as immutable lookup results.

| Field | Type | Meaning |
|-------|------|---------|
| `person_id` | `String` | Stable canonical id from the source of truth (e.g. Notion page id). Treat as opaque. |
| `display_name` | `String` | Human-readable name, e.g. "Ruby Lin". |
| `roles` | `Vec<String>` | Domain roles, e.g. `["customer-pm", "engineer"]`. |
| `project_ids` | `Vec<String>` | Project memberships — what "reject non-project members" evaluates against. |
| `emails` | `Vec<String>` | Associated email addresses; may be empty. |
| `channel_handles` | `BTreeMap<String, String>` | `{channel-wire-name: external_id}`. A `BTreeMap` for deterministic serialisation. |
| `source` | `String` | Which provider produced this record (`"notion"`, `"wiki-cache"`). Surfaced into audit logs. |
| `fetched_at` | `DateTime<Utc>` | Cached records carry the cache write time; live records carry the upstream fetch time. |

The `ChannelKind` enum covers Discord, Line, Telegram, Slack, Whatsapp, Feishu, Webchat, and Email, plus an `Other(String)` catch-all so a self-hosted webhook or future channel never fails to parse.

---

## The identity_resolve MCP Tool

Agents reach Identity Resolution through one MCP tool, gated by a dedicated scope:

```
identity_resolve { channel, external_id }
        |
        v
  Scope check: caller principal must hold Scope::IdentityRead
        |  (missing scope → denied, fail closed)
        v
  provider.resolve_by_channel(channel, external_id)
        |
        v
  ResolvedPerson JSON   ── or ──   null (unknown sender)
```

The scope gate follows DuDuClaw's "security gates fail closed" convention — a key without `Scope::IdentityRead` is denied, never silently allowed through.

---

## The `<sender>` Block: Identity as Data

The most impactful integration is automatic. When a channel message arrives, the gateway resolves the sender **once per turn** and injects an XML-delimited `<sender>` block into the system prompt:

```
System prompt
  ├─ SOUL.md (personality + rules)
  ├─ ## Your Team (sub-agent roster)
  ├─ <sender>                          ← injected, resolved once per turn
  │    <person_id>person_2f9</person_id>
  │    <name>Ruby Lin</name>
  │    <roles>customer-pm</roles>
  │    <project_ids>proj-alpha, proj-beta</project_ids>
  │    <source>notion</source>
  │  </sender>
  └─ ... rest of context
```

This is the visitor badge. Before this feature, a SOUL.md rule like "reject non-project members" required the agent to remember, mid-reasoning, to call `shared_wiki_read` — a step it often skipped. Now the membership data is already in front of it, in a high-attention slot, every single turn. The rule becomes evaluable from data the agent already holds.

When the provider is unconfigured or the sender is unknown, no `<sender>` block is injected — the agent simply treats the message as coming from a stranger, and SOUL.md's stranger-handling rules apply.

---

## Why This Matters

### System-layer authority, not prompt-layer hope

A SOUL.md instruction is best-effort — the model may or may not follow it. Identity Resolution moves the boundary to where it can be evaluated against real data. "Reject non-project members" stops being a prompt and starts being a check against `project_ids` the agent can actually see.

### Graceful degradation, never a locked door

The `ChainedProvider`'s soft-fail design means an upstream outage downgrades fidelity (unknown sender) rather than breaking the conversation. A Notion maintenance window doesn't take down your agents — they fall back to the wiki cache, then to stranger-handling, and keep replying.

### The wiki becomes a cache, not a source of truth

By making `WikiCacheIdentityProvider` one backend among three, the shared wiki is demoted from "the place agents hand-roll identity lookups" to "a transparent cache of the authoritative system." This prevents the evolution loop from silently drifting the wiki into an unmanaged copy of external data.

### Pluggable by trait, not by fork

Because every backend implements the same `IdentityProvider` trait, swapping Notion for LDAP or a custom HTTP directory is a provider change, not a rewrite of the channel-reply path. The `<sender>` injection, the MCP tool, and the scope gate all stay identical.

---

## The Takeaway

A receptionist who guesses at visitors is a liability. One who checks the directory, falls back to a printed roster when the directory is down, and clips a badge on each visitor so no department has to re-check — that is a system you can build rules on. DuDuClaw's Identity Resolution gives every agent that receptionist: one trait, three providers, graceful degradation, and a `<sender>` badge the agent reads on every turn. "Who is this person?" stops being a guess and becomes a lookup.
