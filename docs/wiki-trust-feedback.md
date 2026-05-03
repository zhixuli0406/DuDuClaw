# Wiki RL Trust Feedback

> Self-cleaning wiki via prediction-error reinforcement learning.
> When a cited wiki page misleads the agent (high prediction error),
> its trust score drops; when it helps (low error), trust rises.
> Pages that consistently mislead get auto-quarantined and eventually archived.

**Origin**: 2026-05-03 cron event — Agnes was repeating an outdated
"CronCreate is session-only" claim from a Discord conversation captured 2 weeks
earlier. Manually correcting one wiki page fixed the symptom; this system fixes
the loop.

## Architecture

```text
[RAG retrieval]
    │ search() / build_injection_context()
    │ → SearchHit { page_path, trust, source_type }
    ▼
[CitationTracker]                                  Phase 1
    │ in-memory map: conversation_id → [WikiCitation]
    │ TTL = 1h (orphans GC'd by background task)
    ▼
[LLM produces a reply]
    │ user reads reply, conversation continues
    ▼
[PredictionError finalised]                        existing
    │ composite_error ∈ [0.0, 1.0]
    │ │
    │ ▼
    │ TrustSignal::from_composite_error(err):
    │   err < 0.20 → Positive { magnitude: 0.005..0.02 }
    │   0.20 ≤ err < 0.55 → Neutral
    │   err ≥ 0.55 → Negative { magnitude: 0.02..0.10 }
    ▼
[TrustFeedbackBus.on_prediction_error]             Phase 2
    │ drains tracker by conversation_id
    │ for each citation:
    │   if VerifiedFact + Negative → ×0.5 resistance (Phase 5)
    │   upsert_signal(...)
    ▼
[WikiTrustStore.upsert_signal]                     Phase 2
    │ rate-limit guard: 10 signals/page/day      (Phase 5)
    │ per-conversation cap: |Δ| ≤ 0.10            (Phase 2)
    │ recovery boost: trust < 0.30 → ×1.5 positive (Phase 3)
    │ archive hysteresis: 0.10 / 0.20             (Phase 2)
    │ writes wiki_trust_state row
    │ appends wiki_trust_history audit row
    ▼
[next RAG retrieval reads live trust]
    │ search() consults WikiTrustStore.get_many
    │ → ranking factor: score × (0.5 + trust) × source_type.factor
    │ → do_not_inject pages dropped silently
```

### Daily janitor (Phase 3)

Once every 24 h, the gateway runs `WikiJanitor::run_once` per agent:

1. **Auto-correct tagging** — pages with ≥ 3 negative signals in 30 d get
   a `corrected` tag and a 📝 note appended to their body.
2. **Auto-archive** — pages quarantined (`do_not_inject = true`) ≥ 30 d are
   moved to `wiki/_archive/<original_path>`. Restorable via
   `WikiStore::restore_archived`.
3. **Frontmatter snapshot sync** — writes the live trust value back to the
   page's YAML frontmatter so offline tooling and `git diff` see the
   current state.

## Data model

### `WikiPage` frontmatter additions (Phase 0)

```yaml
---
title: …
trust: 0.85                # 0.0 – 1.0, higher = more reliable
source_type: verified_fact # raw_dialogue | tool_output | user_statement
                           # | verified_fact | unknown
last_verified: 2026-05-03T14:00:00Z
do_not_inject: true        # set automatically when trust < 0.10
citation_count: 47         # snapshot, mirrored from WikiTrustStore
error_signal_count: 2
success_signal_count: 41
---
```

`source_type` is auto-derived from the page path when not explicitly set:

| Path                | Default `source_type`    |
|---------------------|--------------------------|
| `sources/*`         | `raw_dialogue`           |
| `concepts/*` (≥0.7) | `verified_fact`          |
| `entities/*` (≥0.7) | `verified_fact`          |
| anything else       | `unknown`                |

### `wiki_trust_state` SQLite table (Phase 2, `~/.duduclaw/wiki_trust.db`)

Primary key `(page_path, agent_id)` — **per-agent trust** (Q1 decision):
two agents may hold different trust values for the same shared wiki page.

| Column                 | Type    | Notes                                       |
|------------------------|---------|---------------------------------------------|
| `page_path`            | TEXT    | Relative to wiki root                       |
| `agent_id`             | TEXT    | Per-agent isolation                         |
| `trust`                | REAL    | Live RL value, [0, 1]                       |
| `citation_count`       | INTEGER | Bumped at retrieval                         |
| `error_signal_count`   | INTEGER | Negative TrustSignals                       |
| `success_signal_count` | INTEGER | Positive TrustSignals                       |
| `last_signal_at`       | TEXT    | ISO 8601                                    |
| `last_verified`        | TEXT    | Manual verify timestamp                     |
| `do_not_inject`        | 0/1     | RAG skip flag (set when trust < 0.10)       |
| `locked`               | 0/1     | Manual override; immune to auto adjustments |
| `last_correction_at`   | TEXT    | Cooldown anchor for auto-correct janitor    |
| `archive_due_at`       | TEXT    | Test/janitor helper                         |

### `wiki_trust_history` (audit log)

Every mutation appends a row — used by Phase 5 rollback and Phase 4 dashboard.
Triggers: `prediction_error`, `auto_correct`, `manual`, `rollback`,
`federated_import`.

## MCP tools (Phase 4)

| Tool                   | Purpose                                       | Auth     |
|------------------------|-----------------------------------------------|----------|
| `wiki_trust_audit`     | List low-trust pages + counters               | any user |
| `wiki_trust_history`   | Per-page audit history                        | any user |
| `wiki_trust_override`  | Manually set trust + lock (RPC only)          | admin    |

WebSocket RPC methods on the gateway:
`wiki.trust_audit`, `wiki.trust_history`, `wiki.trust_override`.

## Configuration

Defaults (in `TrustStoreConfig` and `JanitorConfig`):

```rust
TrustStoreConfig {
    per_conversation_cap: 0.10,                  // Phase 2 cap
    archive_threshold:    0.10,                  // Phase 2 do_not_inject trigger
    recovery_threshold:   0.20,                  // Phase 2 hysteresis
    default_trust:        0.50,                  // Cold-start
    daily_signal_limit:   10,                    // Phase 5 flood guard
    verified_fact_negative_resistance: 0.5,      // Phase 5 ×0.5 for verified
}

JanitorConfig {
    auto_correct_threshold:  3,                  // 3 negatives → corrected tag
    auto_correct_window_days: 30,
    archive_age_days:         30,
    re_correct_cooldown_hours: 24,
}
```

## Federated synchronisation (Q3)

`WikiTrustStore::export_federated(since)` serialises all rows updated after
`since` into a `Vec<FederatedTrustUpdate>`; the receiving peer applies them
via `import_federated`. Conflict resolution:

- Local row newer than incoming → drop incoming (LWW for the *time* axis).
- Local row older or absent → blend: `new_trust = (local + remote) / 2`.
  `do_not_inject` is OR'd (more cautious side wins).
- `locked = 1` rows are immune — manual local overrides always win.
- `error_signal_count` / `success_signal_count` are **not** propagated;
  they are local observations.

Transport (HTTP, p2p, or scheduled rsync) is out of scope for the trust store
itself — wire one in via the existing federation infrastructure when needed.

## Operational runbook

### "Page X is unfairly being suppressed — restore it"

```jsonc
// Manual override, lock so the loop won't drag it down again
{"jsonrpc":"2.0","method":"wiki.trust_override","params":{
  "agent_id":"agnes","page_path":"concepts/cron-facts.md",
  "trust":0.95,"lock":true,"reason":"Verified by human review 2026-05-03"
}}
```

### "Show me what the loop has been quarantining"

```bash
# CLI / MCP
duduclaw mcp call wiki_trust_audit --agent_id agnes --max_trust 0.30
```

### "Something went wrong — roll trust state back to yesterday"

```rust
let store = duduclaw_memory::trust_store::global_trust_store().unwrap();
store.rollback_since("agnes", chrono::Utc::now() - chrono::Duration::days(1))?;
```

### "Why is page X's trust at 0.18?"

```bash
duduclaw mcp call wiki_trust_history --agent_id agnes --page_path "concepts/x.md"
```

## Verification — 2026-05-03 incident reproduction

The following sequence verifies the closed loop end-to-end. Run on a fresh
wiki containing only the two pages from the original incident:

1. `sources/2026-05-03-...session-only-claim.md` (frontmatter `trust: 0.4`,
   path-derived `source_type: raw_dialogue`)
2. `concepts/cron-scheduling-facts.md` (frontmatter `trust: 0.9`,
   path-derived `source_type: verified_fact`)

Expected behaviour:

| Step | RAG behaviour | Trust delta on `sources/...md` |
|------|----------------|---------------------------------|
| Initial query "為何還是使用 session-only" | Both pages match; verified_fact ranks first (×1.2 vs ×0.6 type factor) | — |
| User responds "你說錯了" → composite_error 0.85 | TrustSignal::Negative { magnitude: 0.10 } | trust: 0.40 → 0.30 |
| Same user × 2 more conversations, similar errors | per-conv cap 0.10 each | 0.30 → 0.10 |
| 3rd negative signal | `error_signal_count` reaches threshold → janitor adds `corrected` tag | trust: 0.10 → 0.05, `do_not_inject = true` |
| Subsequent query | RAG **excludes** `sources/...md`; uses only `concepts/...md` | — |
| 30 days later | janitor archives the page to `_archive/sources/...md` | row preserved, file moved |

## Implementation map

| Phase | File(s) | Tests |
|-------|---------|-------|
| 0 — schema | `crates/duduclaw-memory/src/wiki.rs` (`WikiPage`, `SourceType`, `derive_source_type`) | `wiki::tests::source_type_*`, `legacy_page_*` |
| 0 — types  | `crates/duduclaw-memory/src/feedback.rs` (`WikiCitation`, `TrustSignal`) | `feedback::tests::from_composite_error_*` |
| 1 — citation tracking | `feedback.rs` (`CitationTracker`), `wiki.rs` (`search_with_citation`, `build_injection_context_with_citations`) | `feedback::tests::tracker_*`, `wiki::tests::search_records_citations*`, `injection_records_citations` |
| 1 — wiring | `crates/duduclaw-gateway/src/channel_reply.rs` (`build_system_prompt`), `handlers.rs` (`wiki.search`, `shared_wiki.search`) | runtime check |
| 2 — store  | `crates/duduclaw-memory/src/trust_store.rs` (`WikiTrustStore`) | `trust_store::tests::*` |
| 2 — bus    | `crates/duduclaw-gateway/src/prediction/feedback_bus.rs` | `prediction::feedback_bus::tests::*` |
| 2 — search | `wiki.rs::search`, `collect_by_layer_with_meta` (live trust) | `wiki::tests::search_uses_live_trust_*` |
| 3 — janitor | `crates/duduclaw-memory/src/janitor.rs` | `janitor::tests::*` |
| 3 — cron    | `crates/duduclaw-gateway/src/server.rs::run_wiki_janitor_pass` | runtime check |
| 4 — MCP / RPC | `handlers.rs` (`wiki.trust_*`), `crates/duduclaw-cli/src/mcp.rs` | manual |
| 5 — flood / resistance / rollback | `trust_store.rs`, `feedback_bus.rs` | `trust_store::tests::daily_signal_limit_*`, `verified_fact_resists_*`, `rollback_since_*` |
| 6 — GVU + federated + auto-promote | `feedback_bus.rs::on_gvu_outcome`, `trust_store.rs::{export_federated, import_federated, list_promotion_candidates}` | `prediction::feedback_bus::tests::gvu_outcome_*`, `trust_store::tests::federated_*`, `promotion_candidates_*` |
| 7 — bootstrap + docs | `trust_store.rs::bootstrap_from_wiki`, this document | runtime check |

## Open follow-ups (deferred Phase 4 frontend)

- Dashboard page `WikiTrustPage.tsx` showing trust evolution per agent + per
  page, with manual-override UI hooked to `wiki.trust_override`.
- Configurable `TrustStoreConfig` / `JanitorConfig` via `config.toml`
  (currently hard-coded defaults — sufficient for v1).
- Federation transport: schedule daily `export_federated` between paired
  gateways (HTTP push, signed). Currently only the export/import primitives
  are in place.
