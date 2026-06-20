# TODO — Agent Honesty: Stop Fabricated Tool Calls at Reply Time

**Owner:** — &nbsp; **Status:** phase 1 (shadow mode) landed in v1.3.17 dev
**Last updated:** 2026-04-12

## Problem

Main agents (Agnes) and the new team agents can produce final replies
that **claim actions they did not perform** — e.g. "✓ Created 12
sub-agents" when zero `create_agent` MCP tool calls were made. Three
confirmed manifestations so far:

1. Missing `.mcp.json` → tool literally unavailable → model narrates
   success to satisfy the user (fixed at *wiring* level in v1.3.16).
2. Tool exists but returned an error → model summarises it as success.
3. Task is partially solvable → model reports partial work as full.

The root cause is not any single bug: **the model's final text and the
runtime's tool-call event stream live in two different worlds, and no
component compares them before shipping the reply.** SOUL.md rules like
"絕不可在文字中假裝已建立" are aspirational prompts, not enforced
invariants.

## Literature backing (verified via arxiv)

| Paper | ID | Year | Used for |
|---|---|---|---|
| ToolBeHonest | 2406.20015 | NAACL 2025 | Taxonomy: solvability detection is the #1 error mode |
| Agent-as-a-Judge | 2410.10934 | NeurIPS 2024 | Trajectory-level judging (phase 3) |
| Reliability Alignment (Relign) | 2412.04141 | 2024 | Abstain actions added to tool action space (phase 2) |
| LLM-based Agents Hallucination Survey | 2509.18970 | 2025 | TSH / TCH / Output hallucination taxonomy |
| MCPVerse | 2508.16260 | 2025 | Benchmark for MCP-tool-use fidelity |
| Tool Receipts / NabaOS | 2603.10060 | 2026 | HMAC-signed receipts (phase 3+) |

## Strategy: three-phase defence-in-depth

Each phase is independently shippable. Together they cover DuDuClaw's
observed failure modes a-e (see [research notes](#research-notes)).

```
phase 1 — Action-Claim Verifier (shadow → enforce)
          Zero LLM cost. Regex extraction + audit-log diff. Catches most
          blatant "I created X but didn't call create_agent" cases.

phase 2 — Proxy State Verifier + Abstain Actions
          Zero LLM cost. Re-queries real state (list_agents) to confirm
          creation claims. Adds two MCP tools so the model can signal
          "I can't do this" instead of fabricating.

phase 3 — Tool Receipts + optional Trajectory Judge
          HMAC-signed receipts for every tool_use block. Model is
          prompted to self-tag factual claims with receipt IDs.
          Unsigned claims are provably ungrounded. Optional Haiku-class
          LLM judge for semantic claims that have no state footprint.
```

## Phase 1 — Action-Claim Verifier (shadow mode) — **DONE in dev**

**Goal:** connect the existing (already-tested) `action_claim_verifier`
module to the live reply path so we can start collecting
`ungrounded_claim_rate` baseline metrics — without blocking any
replies.

**Files touched:**
- [crates/duduclaw-security/src/action_claim_verifier.rs] — existing, unchanged
- [crates/duduclaw-security/src/audit.rs] — existing `log_tool_hallucination()`, unchanged
- [crates/duduclaw-gateway/src/channel_reply.rs:614-720] — capture `dispatch_start_time` before CLI call; after reply is unwrapped, run verifier, emit `warn!` + `log_tool_hallucination` per ungrounded claim; reply text untouched
- [crates/duduclaw-gateway/src/cron_scheduler.rs:200-260] — same integration on the cron execution path

**Verification signal:** stream-based. The verifier uses the
`tool_calls.jsonl` audit log, which is populated by the MCP server
([crates/duduclaw-cli/src/mcp.rs] `handle_tools_call` → `audit::append_tool_call`).
A successful MCP tool call produces a row with
`{agent_id, tool_name, params_summary, success: true, timestamp}`.

**Detection logic (zero LLM cost):**
1. Capture `dispatch_start_time = Utc::now().to_rfc3339()` before
   dispatching to Claude Code.
2. Await the reply.
3. `extract_action_claims(&reply)` — regex pass over zh-TW + en
   patterns for AgentCreated / AgentDeleted / SoulUpdated / MessageSent
   / AgentSpawned. Capped at `MAX_CLAIMS_PER_RESPONSE = 10` to
   prevent adversarial inputs from causing quadratic work.
4. `audit::read_tool_calls_since(home, agent_id, dispatch_start_time)`
   — bounded read of `tool_calls.jsonl` filtered to this turn + this
   agent, with a 2-second grace period for clock skew.
5. `verify_claims(claims, tool_calls)` — for each claim, require a
   matching `(tool_name == expected, success == true, params_summary
   contains target_id)` row.
6. Any `VerifyResult::Hallucination` is logged at `warn!` level and
   written to `security_audit.jsonl` via `log_tool_hallucination`.
   **Reply is not modified.**

**Rollout plan:**
- Day 0 (done): ship shadow-mode in CE v1.3.17-dev.
- Day 1: run for 24 hours, collect `grep tool_hallucination
  ~/.duduclaw/security_audit.jsonl | wc -l` baseline.
- Day 2-7: observe per-agent hallucination rates. Identify which
  regex patterns produce false positives; tighten if any >5%.
- Day 8: decide on enforce-mode policy (below).

**Enforce-mode options** (post-shadow):
- Option A — **Append disclaimer**: prepend
  `⚠️ 部分宣告的操作未通過事實核對，請以儀表板為準。`
  to the reply. Non-blocking, gives user visibility.
- Option B — **Hard refuse** for AgentCreated / AgentDeleted /
  SoulUpdated (state-mutating claims): replace the reply with
  `我偵測到自己聲稱執行了 X 但沒有實際呼叫 create_agent，請重試或
  指示我直接呼叫 list_agents 確認現況。` Blocks delivery.
- Option C — **Retry once** with a strengthened system prompt:
  prepend `<previous_reply_had_ungrounded_claims>...</>` to the next
  turn's system prompt so the model self-corrects.

Current lean: **Option A for chat, Option B for cron** (cron has no
user interlocutor to correct the model, so blocking is safer than
silent fabrication in a GitHub issue).

**Metrics to track (via security_audit.jsonl aggregation):**
```
# baseline
jq -c 'select(.event_type == "tool_hallucination") | .agent_id' \
    ~/.duduclaw/security_audit.jsonl | sort | uniq -c

# turn-normalised rate = hallucinations / total replies
# (total replies tracked in cost_telemetry.db via RequestType::Chat)
```

**Known limitations of phase 1:**
- Only catches claims that match the regex patterns. `extract_action_claims`
  currently covers 5 claim types; extending to cover
  `FileWritten / IssueCreated / CommitPushed / DocumentationUpdated`
  is a straightforward add in [action_claim_verifier.rs:39-74].
- Cannot catch *omissions* — the model saying *less* than it did is
  not addressed. Covered in phase 3 via NabaOS-style forward signing.
- `params_summary` is a free-form string, so `contains(target_id)`
  can produce false negatives if the tool call recorded
  `name=xianwen_tl` but the reply says `xianwen-tl` (underscore vs
  hyphen). Addressed by normalising both sides in the matcher.

## Phase 2 — Proxy State Verifier + Abstain Actions

**Goal:** for state-mutating claims, bypass regex + audit-log diff
entirely and **re-query the real world** to confirm the claim held.
This covers claims whose audit row was lost, agent confusion about
exact agent name, or any third-party mutation path we don't intercept.

### 2a. Proxy State Verifier

**New module:** `crates/duduclaw-security/src/proxy_state_verifier.rs`

**API:**
```rust
pub async fn verify_agent_creation(
    home_dir: &Path,
    claimed_agent_ids: &[&str],
    registry: &AgentRegistry,
) -> Vec<StateVerifyResult>;

pub async fn verify_file_written(
    claimed_paths: &[&Path],
) -> Vec<StateVerifyResult>;
```

**Wiring:** after the action-claim verifier flags `AgentCreated`
claims, pass the target IDs through `verify_agent_creation` which
calls `registry.scan().await` and checks `registry.get(id).is_some()`.
Disagreement between "regex says created" and "registry says absent"
is a hard hallucination — upgrade severity to Critical.

**Cost:** one `opendir` per verification. Effectively free.

**Ship:** Day 9-10.

### 2b. Abstain Actions

Add two pseudo-MCP tools so the model has an in-language way to
signal "I can't do this" instead of fabricating:

- `agent_escalate_missing_capability(reason: string)` — "my toolset
  does not contain the tool needed for this request"
- `agent_request_clarification(question: string)` — "I need the user
  to disambiguate before I can proceed"

These are *real* MCP tools (not prompt hints), so calling them
produces `tool_calls.jsonl` rows that can be audited. A reply that
contains a hallucinated `AgentCreated` claim **and** no
`agent_escalate_missing_capability` row = definite fabrication.

**New file:** `crates/duduclaw-cli/src/mcp.rs` — add two `ToolDef`
entries plus handlers that just append to `security_audit.jsonl` and
return a structured "escalation received" message.

**Prompt change:** update `cmd_agent_create`-written SOUL.md template
to reference these tools in the "Tool Use" section:
> If you cannot perform a requested action with your current tool
> set, call `agent_escalate_missing_capability` — NEVER narrate a
> fake result.

**Ship:** Day 11-12.

### Metric at end of phase 2

```
fabrication_caught_rate =
    (phase1_hallucinations + phase2_state_mismatches) / total_replies

abstain_usage_rate =
    escalate_missing_capability_calls / total_replies
```

Target: `fabrication_caught_rate` drops to <0.5% after one week
(because the model learns to abstain instead of fabricate); if
`abstain_usage_rate` stays at 0, the model isn't using the escape
hatch — investigate why prompt isn't landing.

## Phase 3 — Tool Receipts (NabaOS pattern) + optional Trajectory Judge

**Goal:** cryptographically sign every tool call, prompt the model to
self-tag every factual assertion in its reply with a receipt ID, then
verify deterministically before shipping.

### 3a. Receipt ledger

**New crate:** `crates/duduclaw-receipts/` (or submodule under
`duduclaw-security/`; single-crate simpler for CE bundle).

**Schema (SQLite, new DB `~/.duduclaw/receipts.db`):**
```sql
CREATE TABLE receipts (
    id              TEXT PRIMARY KEY,            -- ULID
    agent_id        TEXT NOT NULL,
    session_id      TEXT NOT NULL,
    turn_id         TEXT NOT NULL,
    tool_name       TEXT NOT NULL,
    input_hash      TEXT NOT NULL,               -- SHA-256 of params JSON
    output_hash     TEXT NOT NULL,               -- SHA-256 of result JSON
    result_count    INTEGER,                     -- for collection results
    extracted_facts TEXT,                        -- JSON map
    hmac            TEXT NOT NULL,               -- HMAC-SHA256 of the row
    created_at      TEXT NOT NULL
);

CREATE INDEX idx_receipts_session_turn ON receipts(session_id, turn_id);
```

**Key material:** new per-agent key in the existing keystore, fetched
from `~/.duduclaw/secret.key` + HKDF-derive with `agent_id` as context
so receipts can't be replayed across agents.

### 3b. Receipt emission

**Wiring:** the MCP server at [crates/duduclaw-cli/src/mcp.rs] in
`handle_tools_call` already writes `tool_calls.jsonl`. Add a parallel
`receipt_ledger.write()` call that produces a signed receipt and
returns the receipt ID to the agent via the tool-call response JSON
(new `_duduclaw_receipt_id` meta field).

### 3c. Self-tagging prompt

Updated SOUL.md template (auto-injected at agent creation, and
retroactively via one-shot migration):

```md
## Factual Claims

After completing any response that describes tool-mediated actions,
append a single line in the format:

    <claims>[{"text": "...", "receipt": "<id or null>"}, ...]</claims>

Every factual assertion must reference a receipt ID from a tool call
you actually made this turn, or `null` if you want to explicitly mark
the claim as `ungrounded`. The runtime will verify every receipt
before delivering your reply.
```

### 3d. Pre-delivery verifier

**New function** in `action_claim_verifier.rs` (or new module):
```rust
pub fn verify_claims_block(
    reply: &str,
    ledger: &ReceiptLedger,
    session_id: &str,
    turn_id: &str,
) -> Vec<VerifyResult>;
```

Parses `<claims>` block from reply, cross-references each `receipt`
field against the ledger (HMAC verify + session/turn match). Any
receipt that doesn't exist, has a bad signature, or belongs to a
different session is treated as definite fabrication.

**Outcome handling:**
- `ungrounded` explicit → allow; surface as dim footer
- missing `<claims>` block → fall back to regex verifier (phase 1)
- invalid receipt → hard refuse (NabaOS paper reports 94.2% precision)

### 3e. Optional Trajectory Judge

For claims with no state footprint ("I analysed the logs") — phase 1
and 2 can't verify because there's nothing to re-query. A single
Haiku 4.5 call per turn, with a rubric and the full trajectory, can:
- confirm the final text is consistent with the tool-call sequence
- flag narrative drift ("the model executed A but claims B")

**Gated behind a per-agent feature flag** `verification.use_judge = true`,
disabled by default. Budget impact: 1 extra Haiku call/turn; can be
routed to local GGUF for zero marginal cost.

**Ship:** phase 3a/3b/3c are week 3. Phase 3d is week 4. Phase 3e
research-project, ship if phase 1-3d leave residual fabrication rate
>0.5% for chat claims.

## Success metrics

| Phase | Metric | Target |
|---|---|---|
| 1 (baseline) | `shadow_hallucination_rate = flagged / replies` | Collect for 1 week |
| 1 (enforce) | `post_enforce_rate` | <70% of baseline |
| 2 | `fabrication_caught_rate` (phase 1 + 2) | <0.5% |
| 2 | `abstain_usage_rate` | >0 (model must actually use the escape hatch) |
| 3 | `unverified_claim_rate_after_receipts` | <0.1% |

**Secondary health metrics** (watch for negative side effects):
- `user_visible_refusals` (channel_failures.jsonl): must not >2×
  baseline after enforce-mode flip
- `p95_reply_latency_ms`: verification adds <50 ms; anything more
  means the regex matchers or audit read is doing something wrong
- `false_positive_rate`: manual review of 50 flagged replies/week
  to catch over-eager regexes

## Day-by-day schedule

| Day | Task | Outcome |
|---|---|---|
| 0 ✅ | Phase 1 shadow-mode wired into channel_reply + cron_scheduler | dev branch |
| 1 | Ship CE v1.3.17 + Pro v1.4.4 with shadow mode | Homebrew users on new version |
| 2-7 | Baseline collection: `grep tool_hallucination security_audit.jsonl` | Per-agent baseline numbers |
| 8 | Review false-positive rate on sample of 50 flagged replies | Tighten regex if >5% |
| 9-10 | Phase 2a — proxy_state_verifier module + wiring | Second verification layer |
| 11-12 | Phase 2b — abstain actions (MCP tools + SOUL template update) | Escape hatch live |
| 13 | Flip enforce mode: Option A for chat, Option B for cron | Shadow → enforce |
| 14 | Post-enforce metric review | Target <70% hallucination rate |
| 15-19 | Phase 3a-3c — receipt ledger + signing + prompt | Structural signing |
| 20 | Phase 3d — pre-delivery receipt verifier | Deterministic final gate |
| 21+ | Phase 3e — trajectory judge (if needed based on residual rate) | Research project |

## Open questions

1. **Retroactive SOUL.md update** — existing agents have old SOUL.md
   without the `<claims>` block instruction. One-shot migration at
   gateway startup? Or wait for next `agent_update_soul` call? Lean:
   one-shot idempotent migration in `ensure_agent_hook_settings`
   equivalent for SOUL.
2. **Multi-turn fabrication** — an agent could succeed on turn 1
   (create_agent call made), reference it on turn 2 ("I created X
   yesterday"), then narrate additional unrelated actions. Is
   cross-turn verification worth the complexity? Probably no for
   v1 — receipts are scoped to the current turn.
3. **Delegation depth** — when Agnes delegates to xianwen-tl which
   delegates to xianwen-pm, whose `dispatch_start_time` gates the
   verifier? Currently each agent has its own, which is correct, but
   worth adding an assertion in tests.

## Research notes

### Failure-mode taxonomy (from ToolBeHonest 2406.20015 + survey 2509.18970)

| Symptom | Research term | Phase that addresses it |
|---|---|---|
| (a) Claim tool call that didn't happen | Tool Selection Hallucination | 1, 3a-3d |
| (b) Narrate fake tool result | Output/Observation Hallucination | 1 (partial), 3e |
| (c) Execute tool X but narrate X+Y | Narrative Drift | 3e |
| (d) Partial → full | Planning Information Misapplication | 3a-3d |
| (e) Invent schema-valid params | Tool Calling Hallucination | 1, 3a-3d |

### Why Reasoning models fabricate more

*The Reasoning Trap* (2510.22977, 2025) shows that models with stronger
reasoning produce **more** fabrications when confronted with an
unsolvable task, because reasoning pressure directs the model toward
"find a way" rather than "abstain". This is why **phase 2b (abstain
actions) is critical** — it gives the model an explicit action that
reasoning can select instead of the fabrication path.
