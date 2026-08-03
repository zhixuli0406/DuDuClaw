# Wiki Knowledge Layer

> Four layers of knowledge, trust-weighted — always-on for identity and core facts, on-demand for the deep archive.

---

## The Metaphor: A Doctor's Clinic

A doctor walking into an exam room has four tiers of knowledge at different mental distances:

1. **Identity** — "I am Dr. Chen, a cardiologist." Always present. Never retrieved; simply *who they are*.
2. **Core facts** — "This patient is allergic to penicillin. Today is Tuesday. The EHR system is up." Needed every consultation. Glanced at without thinking.
3. **Context** — "Last week this patient had an abnormal ECG; we're following up today." Recent, relevant, refreshed daily.
4. **Deep archive** — "That paper about rare arrhythmias from 2019." Retrieved only when something cues its relevance.

Loading *all* knowledge into working memory on every consultation would be exhausting and counterproductive. The doctor's brain layers knowledge by **injection frequency**, and DuDuClaw's Wiki does the same.

---

## The Four Layers

Inspired by the [Vault-for-LLM](https://github.com/BurkhardHagmann/Vault-for-LLM) 4-layer knowledge architecture, every Wiki page declares one of:

| Layer | Symbol | Frequency | Use Cases |
|-------|--------|-----------|-----------|
| **L0 Identity** | `identity` | Injected into every conversation | Agent/user identity, role, mission |
| **L1 Core** | `core` | Injected into every conversation | Environment, active projects, invariant rules |
| **L2 Context** | `context` | Daily refresh / on request | Recent decisions, debug logs, current sprint |
| **L3 Deep** | `deep` | Search-only, on-demand | Knowledge archive, historical notes, rare references |

Only L0 and L1 are auto-injected. L2 and L3 require an explicit search or refresh.

```markdown
---
title: Agent Mission Statement
layer: identity
trust: 1.0
tags: [identity, mission]
---

I am duduclaw-pm, the project manager for the DuDuClaw
v1.9 roadmap. My authority extends to...
```

---

## Trust Weighting

Every page carries a `trust` score (0.0 to 1.0) in its frontmatter:

```
trust: 1.0   — Source of truth (contract, policy)
trust: 0.7   — Verified current information
trust: 0.4   — Auto-ingested, unverified
trust: 0.1   — Speculative, draft
```

Search results are ranked by **trust-weighted score** = `fts5_rank × trust`. A high-trust page with moderate keyword relevance beats a low-trust page with higher raw relevance. This prevents hallucinated or auto-scraped content from out-ranking curated material.

---

## Auto-Injection Flow

The injection happens at system prompt assembly time — in three places, so all four runtimes (Claude / Codex / Gemini / OpenAI) get the same knowledge:

```
User sends message
     |
     v
Gateway routes to runtime
     |
     v
build_system_prompt(agent_id) assembles:
  ├─ Agent SOUL.md
  ├─ CONTRACT.toml (must_not / must_always)
  ├─ ## Your Team (sub-agent roster)
  ├─ Pinned instructions (session-scoped)
  ├─ Top-3 key facts (cross-session)
  └─ WIKI_CONTEXT module:
       └─ Collect all pages WHERE layer IN (identity, core)
       └─ Budget-aware truncation by priority
     |
     v
Three paths use the same module:
  1. runner.rs        (CLI interactive)
  2. channel_reply.rs (Telegram/LINE/Discord/Slack/...)
  3. claude_runner.rs (dispatcher/cron delegation)
```

Before v1.8.9, the Wiki accumulated pages via channel ingest and GVU evolution but **never fed them back** into LLM system prompts. Agents had knowledge they couldn't see. The auto-injection closes that loop.

---

## FTS5 Full-Text Index

All pages (regardless of layer) are indexed in a SQLite FTS5 virtual table with the `unicode61` tokenizer — which handles CJK characters correctly:

```
write_page("api-design.md") ──┐
delete_page("old-spec.md") ───┤── auto-sync
wiki_rebuild_fts MCP tool ────┘   (manual rebuild)
     |
     v
WikiFts SQLite virtual table
     |
     v
Search queries:
  wiki_search("rate limiting", min_trust=0.5, layer="core")
  shared_wiki_search("SOP", expand=true)
```

### Search Filters

```
min_trust: filter out draft/auto-ingest content
layer:     restrict to specific layer
expand:    1-hop backlink/related expansion
           (find pages linked-from and linking-to the hits)
```

### Backlink Expansion

Backlink expansion traces `related:` frontmatter and body markdown links in both directions:

```
Search hit: "payment-flow.md"
     |
     v
Backlinks: pages that link TO payment-flow.md
  ├─ "refund-policy.md"
  ├─ "stripe-integration.md"
  └─ "checkout-audit.md"
     |
     v
Forward-links: pages that payment-flow.md links to
  ├─ "api-keys.md"
  └─ "webhook-handlers.md"
     |
     v
All 6 pages included in expanded result
```

This is how a single targeted search pulls in an entire neighborhood of related knowledge.

---

## The Knowledge Graph

`wiki_graph` exports Mermaid diagrams of the wiki's interlink structure:

```mermaid
graph LR
  A[identity: dudu-pm]:::id --> B[core: roadmap-v1.9]
  B --> C[context: sprint-12]
  C --> D[deep: historical-decisions]
  C --> E[context: blocker-analysis]
  B --> F[core: team-roster]

  classDef id fill:#f59e0b
  classDef core fill:#fb923c
```

Node shapes vary by layer (identity = circle, core = rounded rectangle, context = rectangle, deep = stadium). The graph is BFS-limited by `center` and `depth` parameters so you can export a focused subset instead of the entire wiki.

---

## Dedup Detection

Over months of auto-ingest (channel conversations, GVU reflections), duplicate or near-duplicate pages accumulate:

```
wiki_dedup:
     |
     v
For each pair of pages:
  1. Title match (exact or fuzzy ≥ 0.9)
  2. Tag Jaccard similarity ≥ 0.8
     |
     v
Report candidate duplicates:
  [
    { "keep": "stripe-integration.md",
      "merge": "stripe-api-notes.md",
      "reason": "Tag Jaccard 0.88, title 0.95" }
  ]
```

The tool doesn't auto-merge — it surfaces candidates for human review.

---

## Shared Wiki

Beyond per-agent wikis, there's a shared wiki at `~/.duduclaw/shared/wiki/` for knowledge that spans the whole organization:

```
~/.duduclaw/
├── agents/
│   ├── dudu/wiki/          ← per-agent knowledge
│   └── xianwen/wiki/       ← per-agent knowledge
└── shared/wiki/            ← cross-agent SOPs, policies, product specs
```

Visibility is controlled via the `wiki_visible_to` capability on each page — default is agent-private, but pages can be promoted to shared or restricted to a team. MCP tools: `shared_wiki_ls`, `shared_wiki_read`, `shared_wiki_write`, `shared_wiki_search`, `shared_wiki_delete`, `shared_wiki_stats`, `wiki_share`.

### Namespace SoT Policy (`.scope.toml`)

Operators can declare which top-level namespaces inside the shared wiki are **authoritative copies of an external system** (Notion, LDAP, governance policy bundles) and must not be silently overwritten by an evolving agent. Drop a `~/.duduclaw/shared/wiki/.scope.toml`:

```toml
# Identity is owned by the IdentityProvider sync — no agent may write here
[namespaces."identity"]
mode         = "read_only"
synced_from  = "identity-provider"

# Access control list is owned by the governance policy bundle
[namespaces."access"]
mode         = "read_only"
synced_from  = "policy-registry"

# SOPs continue to be agent-writable (also the default for unlisted namespaces)
[namespaces."SOP"]
mode         = "agent_writable"

# Production policies are operator-only — never writable via MCP
[namespaces."policies"]
mode         = "operator_only"
```

Three modes:

| Mode | Agents (MCP path) | Internal capability that matches `synced_from` | Operator CLI |
|---|---|---|---|
| `agent_writable` | ✅ allowed | ✅ allowed | ✅ allowed |
| `read_only` | ❌ denied | ✅ allowed | ✅ allowed |
| `operator_only` | ❌ denied | ❌ denied | ✅ allowed |

Both `shared_wiki_write` and `shared_wiki_delete` honour the policy. Unlisted namespaces are `agent_writable` by default — the policy *only tightens*, never relaxes.

**Fail-safe:** absent file ⇒ no policy ⇒ existing behaviour. Malformed TOML ⇒ logged warning + treated as no policy. The gateway is never blocked by a broken policy file.

**Hot-reload:** the policy is re-read on every write/delete (the file is small; performance impact negligible). Operator edits take effect immediately.

Use `wiki_namespace_status` MCP tool to inspect the active policy before writing.

### Department read-visibility (`visible_to_departments`)

The write `mode` above governs *who may write* a namespace. To govern *who may read* it at the **department** level, add a `visible_to_departments` array to the same `[namespaces."x"]` table. Only agents whose `[agent] department` is on the list see that namespace — both in **prompt injection** (auto-injected L0/L1 pages) and via **`shared_wiki_search` / `shared_wiki_read` / `shared_wiki_ls`**.

```toml
# HR pages are readable only by the hr and legal departments
[namespaces."hr"]
mode                   = "operator_only"   # writes: operator only
visible_to_departments = ["hr", "legal"]   # reads: hr + legal departments only
```

This is orthogonal to the write `mode` — a namespace may declare either, both, or neither. An agent's department comes from `[agent] department` in its `agent.toml` (empty/absent = no department).

**Fail-closed** for any declared namespace: an agent whose department is not on the list — including an agent with **no department** — is denied. Exact department match only (no prefix/substring). An empty list denies everyone.

**Fail-safe** when undeclared: a namespace without `visible_to_departments` stays readable by all agents, exactly as before. Absent / malformed `.scope.toml` ⇒ no filter.

This layers on top of the built-in `departments/<dept>/` isolation (see Department knowledge layering below): `departments/art/*` is always visible only to the `art` department regardless of `.scope.toml`, while `visible_to_departments` lets an operator restrict *any* namespace (`hr/`, `finance/`, …) to chosen departments. `wiki_namespace_status` surfaces the active `visible_to_departments` declarations.

### Department knowledge & skill layering

Knowledge and skills layer **company → department → personal**:

- **Wiki:** pages under `shared/wiki/departments/<dept>/` are visible only to agents whose `[agent] department` matches `<dept>`; the company layer (every other namespace) is open to all. Read isolation is always enforced.
- **Skills:** three layers merge per agent with **per-agent > department > global** precedence (nearest wins on a name collision):
  - global — `~/.duduclaw/skills/` (all agents)
  - department — `~/.duduclaw/shared/skills/departments/<dept>/` (only agents in `<dept>`)
  - per-agent — `<agent>/SKILLS/`

Install a skill into the department layer via the `skill_hub_install` MCP tool with `scope = "department:<name>"` (or `"global"` / an agent id). An agent with no department only ever sees the global + per-agent layers.

---

## Cloud Ingest Integration

When channel conversations or external documents are ingested, the ingester assigns sensible defaults:

```
Auto-ingested content defaults:
  ├─ Source pages:   layer: context, trust: 0.4
  └─ Entity pages:   layer: deep,    trust: 0.3
```

Low trust by default — the agent can promote to higher layers after verification. The Cloud Ingest prompt explicitly instructs the LLM to assign `layer` and `trust` during extraction, so raw inputs arrive with a reasonable first estimate.

---

## Auto-Filed Pages (`auto/` namespace, WP5c)

Pasting a company charter into a channel used to leave nothing behind: the
distillation classifier gated on the *assistant reply* length, so a 2,000-char
document answered with "got it" was skipped entirely. WP5c adds a second sink.

**Grading** (`knowledge_route.rs`, zero LLM cost for the decisive cases):

| Layer | Rule |
|---|---|
| L0 exclusions | < 80 chars · a short question · any `scan_input` rule hit · LLM-fallback narrative |
| L1 signals | document nouns (+40), explicit "file this" verbs (+50), `第…條` article markers ×2 (+35), ≥3 numbered lines (+25), markdown structure (+10), length (+15/+30), title line (+10); minus first-person preference (−45), time-bound context (−35), pronoun density (−20), multiple questions (−25) |
| Thresholds | ≥ 65 file · 30–64 ask the utility model · < 30 memory path |

Grading reads **only the user's text** and runs before `classify_for_ingest`.

**Where pages land:** `auto/{charter,sop,spec,policy,reference}/<slug>.md` in the
agent's own wiki. The hand-curated directories (`entities/`, `concepts/`,
`sources/`, `synthesis/`) are unreachable from this path.

**How an auto page differs from a curated one:**

| | Auto-filed | Curated |
|---|---|---|
| `author` | `auto-distill` | operator / agent id |
| `tags` | includes `auto-distilled` | — |
| `layer` | `context` — **never auto-injected** | `identity` / `core` participate |
| `trust` | `0.300` (the `channel` origin ceiling) | up to `1.0` |
| `source_type` | `raw_dialogue` (ranking factor 0.6) | `verified_fact` (1.2) etc. |
| Searchable | yes (`do_not_inject` deliberately unset) | yes |

Non-injection is the design's risk pivot: a misgraded page costs one extra page
in the knowledge base, never a polluted system prompt.

**Determinism.** The page key is `auto/<doc_type>/<slug>.md` where `slug` is the
utility model's proposal validated against `^[a-z0-9][a-z0-9-]{0,63}$`, falling
back to `<doc_type>-<sha8(NFKC-normalised title)>`. Re-pasting identical content
is a no-op; changed content overwrites the body and appends a revision-log line.

**Gates, all fail-closed, all degrading to the memory path:**

1. `.scope.toml` — `[namespaces.auto]` may be `operator_only` / `read_only` for
   another capability to disable auto-filing. Unlike the shared-wiki fail-safe,
   a file that exists but does not parse **stops** the write.
2. `scan_input` over the rendered page text — any rule match drops the page.
3. Same-origin burst detection (`knowledge_guard`).
4. Daily circuit breaker: 20 pages + 20 grey-band arbitrations per agent.

**Memory keeps a pointer, not the text:** one row with
`subject = wiki:auto/<doc_type>/<slug>`, `predicate = documented_in`, so
`store_temporal` supersedes the previous pointer and the curation station can
expire exactly this page's row on removal.

**Operator surface:** dashboard → 記憶與知識 → 策展台 → 自動建檔 (view / confirm
as curated / share to the shared wiki / remove).

---

## CLAUDE_WIKI Template

Every new agent's `CLAUDE.md` now includes a CLAUDE_WIKI template that teaches the LLM how to use wiki tools:

```markdown
## Wiki Knowledge Base

You have access to a persistent wiki at <agent>/wiki/.
Use these tools to retrieve and update knowledge:

- wiki_search(query, min_trust, layer, expand)
- wiki_read(page_name)
- wiki_write(page_name, content, layer, trust)
- wiki_graph(center, depth)
- wiki_dedup()

L0 Identity + L1 Core pages are auto-injected — you don't
need to call wiki_read for those. Call wiki_search when
you need historical context or deep references.
```

Before this template, agents had access to wiki tools but rarely used them because they weren't aware of the wiki's existence or conventions. The template closes that instruction gap.

---

## Why This Matters

### Signal over Noise

Auto-injecting L0+L1 pages is roughly the same as having the doctor's identity and the current patient's allergies always in view. You don't wade through chart history to find them.

### Trust as a First-Class Signal

A `trust` score means the agent can reason about the reliability of its knowledge: "this pattern has trust 0.3, I should verify before acting." Knowledge isn't a boolean (present / absent) — it's a distribution.

### Runtime Agnostic

Claude, Codex, Gemini, and OpenAI-compatible runtimes all see the same wiki — because the injection happens *before* the runtime boundary, in `build_system_prompt`.

### Closes the Accumulation Loop

Before v1.8.9, the Wiki was write-only from the LLM's perspective: everyone could write, nobody could read (except through explicit `wiki_search` calls the LLM rarely made). Now every conversation reads the identity + core layers automatically.

---

## Interaction with Other Systems

- **GVU Loop**: SOUL.md updates can be triggered by patterns detected via wiki search — the evolution engine knows what the agent knows.
- **Skill Lifecycle**: Skill extraction consults the wiki for context. A skill synthesized from memory can cite the wiki pages that support it.
- **Security**: Wiki pages containing secrets get flagged by the same scanner that runs on other writable surfaces. The `must_not` rules in CONTRACT.toml can restrict which layers an agent is allowed to write to.
- **Dashboard**: The Knowledge Hub page renders the wiki with layer filters and a graph visualization.

---

## The Takeaway

Knowledge isn't a flat pile of documents — it's layered by how often you need to see it. DuDuClaw's Wiki makes that layering explicit, trust-weights every page, and auto-injects the must-always-remember tier directly into every system prompt. The deep archive stays quiet until summoned.
