# TaskPacket format v1.0

**Status:** Draft · **Since:** DuDuClaw 1.66 (P1/WP-1) · **Implementation:** `crates/duduclaw-core/src/task_packet.rs`

A TaskPacket is the only thing that crosses a role boundary inside one AI employee's team. When an employee runs as a team (planner → executor → verifier → utility, each on its own runtime and model), the roles never see each other's conversation — they exchange packets.

This document is the reference: field table, caps, the list of fields that deliberately do not exist, and a worked JSON example. For *why* teams exist and when they form, see [features/56-team-as-agent.md](../features/56-team-as-agent.md).

## Why not just forward the transcript

Three reasons, all structural rather than stylistic:

1. A tool call has a different wire shape per vendor (`tool_use`, `function_call`, `functionCall`), so a transcript is not portable across roles that run on different backends.
2. OpenAI documents that reasoning items are silently dropped when the model family changes — a replay that looks lossless while losing exactly the part that recorded the decision.
3. Structured notes measure better than both a raw trace and a prose summary: 20–59% fewer events and 42–63% fewer prompt tokens (arXiv:2606.02875).

So a packet carries decisions and *references*. Large content stays where it already lives (artifacts, wiki, memory) and the packet points at it.

## Top-level fields

| Field | Type | Required | Meaning |
|---|---|---|---|
| `packet_id` | string | ✔ | Unique id for this handoff. Non-blank. |
| `goal_id` | string | ✔ | The goal task this packet serves (the existing task id). Non-blank. |
| `parent_packet` | string? | — | The packet this one answers. Absent for the first packet of a round. |
| `round` | uint32 | ✔ | Goal-loop round, shared with `task_iterations.round`. |
| `from_role` | `planner` \| `executor` \| `verifier` \| `utility` | ✔ | Sender. |
| `to_role` | same enum | ✔ | Recipient. Must differ from `from_role`. |
| `objective` | string | ✔ | One sentence: what to do. Non-blank, ≤ 1000 chars. |
| `output_format` | object, see below | ✔ | Expected shape of the product. |
| `tool_scope` | `{allowed: string[], denied: string[]}` | — | **Not wired — human-readable only.** Carried and validated, read by nothing: it reaches no spawn and is not even rendered into a prompt, so a denial here has no effect. A role member's real tool envelope comes from the employee's `[capabilities]`. |
| `boundaries` | string[] | — | Explicitly what NOT to do. |
| `constraints` | `{id, text}[]` | — | Enumerated constraints. ≤ 12 items, each `text` ≤ 200 chars, ids unique (case-insensitive). **Incompressible.** |
| `audience` | string[] | — | Allowlist of role / channel ids this content may reach. ≤ 16 entries, each ≤ 64 chars. **Incompressible.** Empty means "no declared audience", which the composer reads as the enclosing task's audience — never "everyone". |
| `acceptance` | `{kind, value}[]` | — | Machine-checkable acceptance assertions, ≤ 6 per kind, each `value` ≤ 80 chars. |
| `acceptance_baseline_ref` | string? | — | Points back at the goal's frozen `acceptance_criteria_baseline`. |
| `artifacts` | `{id, path?, sha256?}[]` | — | References into `artifacts.jsonl`. Never the bytes. |
| `wiki_refs` | string[] | — | Shared-wiki / agent-wiki paths. |
| `memory_refs` | string[] | — | Memory ids, for `memory_fetch_batch`. |
| `state_keys` | string[] | — | `working_state` authoritative keys this role may rely on. |
| `evidence_index` | string[] | — | "Title + id" of the key material upstream actually looked at, ≤ 32 entries. Exists so the downstream role knows *what to ask for*. |
| `findings` | `{text, evidence: string[]}[]` | — | Confirmed facts, each with the evidence it rests on. A finding with no evidence is a claim, and the shape says so. |
| `open_questions` | string[] | — | Unresolved questions, so the downstream role can decide whether to pull. |
| `blockers` | string[] | — | Hard blocks (maps onto the six `pause_reason` classes). |
| `next_steps` | string[] | — | Ralph-style handoff steps, same shape as `working_state_handoff`. |
| `fidelity` | `full` \| `mcp_only` \| `none` | — (default `none`) | Evidence grade of the upstream role's observations. |
| `budget` | `{max_turns?, max_tokens?, wall_clock_secs?, max_cost_usd?}` | — | Unset fields mean "the dispatcher's own limit applies" — not "unlimited". |
| `irreversible` | bool | — (default `false`) | **Not wired — human-readable only.** Triggers nothing: no ActionGuard check, no artifact-receipt comparison, no prompt rendering. ActionGuard still guards irreversible calls at the point of the call, independently of this field. |

Every field marked — has a serde default, so **omitting it is not the same as leaving the packet unfinished**: a packet carrying only the seven required keys is complete and valid. See [Minimal packet](#minimal-packet).

When the packet arrives through the `team_handoff` tool, four of the required keys may be omitted as well — `goal_id`, `round`, `from_role` and `to_role` are filled from the caller's own `[team_member]` record. A value the caller *does* write is cross-checked against that record, never trusted over it.

### `output_format`

Serialized (and stored on disk) as the internally-tagged object:

```json
{"kind": "markdown"}
{"kind": "json", "schema": null}
{"kind": "json", "schema": "{\"type\":\"array\"}"}
{"kind": "diff"}
{"kind": "files"}
```

**Deserialization also accepts the bare token** — `"markdown"`, `"json"`, `"diff"`, `"files"` (trimmed, ASCII-case-insensitive, exact token match) — because that is what a model writes when it has been told four legal values and nothing else. The short form is an input tolerance only: it is normalized to the tagged object before the packet is written, so the composer and every reader see one spelling. `schema` may be omitted from the tagged form (`{"kind":"json"}` is `schema: null`); carrying a schema is the one thing the short form cannot do.

### `acceptance[].kind`

The four values are spelled as the `EntryAssertions` field names on purpose, so the conversion to and from the playbook type is mechanical and lossless:

| `kind` | Meaning |
|---|---|
| `must_use_tools` | The turn MUST call this tool at least once. |
| `must_not_use_tools` | The turn must NOT call this tool. |
| `output_contains` | The final answer MUST contain this substring. |
| `output_not_contains` | The final answer must NOT contain this substring. |

Tool names are compared case-insensitively; output substrings are compared verbatim, because case can be the assertion. A value appearing in both a kind and its opposite can never be satisfied and is rejected at write time.

### `fidelity`

Mirrors the gateway's `ObservationFidelity` with the same three values and the same spellings (note the underscore in `mcp_only`):

| Value | Meaning |
|---|---|
| `full` | Native tool events plus MCP audit — both visible, with outcomes. |
| `mcp_only` | MCP audit log only (`tool_calls.jsonl`). The main branch today. |
| `none` | No tool evidence at all. |

The three are never conflated. A verifier that cannot tell `none` from `mcp_only` reads "no tool calls recorded" as "no tool calls made".

## Caps

| Cap | Value | Constant |
|---|---|---|
| Whole packet, compact JSON | 16384 bytes | `TASK_PACKET_MAX_BYTES` |
| `constraints` items | 12 | `CONSTRAINTS_MAX` |
| `constraints[].text` | 200 chars | `CONSTRAINT_TEXT_MAX_CHARS` |
| `constraints[].id` | 64 chars | `CONSTRAINT_ID_MAX_CHARS` |
| `audience` items | 16 | `AUDIENCE_MAX` |
| `audience[]` | 64 chars | `AUDIENCE_ID_MAX_CHARS` |
| `acceptance` items per kind | 6 | `ASSERTION_LIST_MAX` |
| `acceptance[].value` | 80 chars | `ASSERTION_TOKEN_MAX_CHARS` |
| `objective` | 1000 chars | `OBJECTIVE_MAX_CHARS` |
| `evidence_index` items | 32 | `EVIDENCE_INDEX_MAX` |

Two rules apply to every one of them:

**Characters, not bytes.** Every character cap counts Unicode scalar values, so a 200-character Traditional Chinese constraint is 200 — not the 600 a byte count would report.

**Reject, never truncate.** An over-cap packet is refused whole, exactly as `working_state_handoff` refuses an oversized note. `constraints` and `audience` are the incompressible section: compression taxes boundaries unilaterally (arXiv:2608.29028 measured violation rates going from under 15% with explicit constraints to 50–73% with vague ones, and found an audience allowlist "nearly eliminates" leakage), so trimming them silently converts an explicit boundary into an implicit one — the exact failure the fields exist to prevent.

### Error codes

`TaskPacket::validate` returns a closed `PacketError`, each with a stable code for the audit log: `blank_field`, `field_too_long`, `too_many_items`, `duplicate_constraint_id`, `contradictory_assertion`, `self_handoff`, `packet_too_large`.

Each message names the offending **item**, not just the field: `` `constraints[3].text` is 240 chars (max 200) ``, `` `audience[1]` must not be blank ``, `` `acceptance[0].value` is 92 chars (max 80) ``. With twelve constraints allowed, "which one" is the actionable half — and the caller fixing the packet is a model that cannot open this file.

## Deliberately absent fields

None of the following exists anywhere in the packet or its children, and none may be added:

- `transcript`, `messages`, or any conversation array
- `tool_use`, `function_call`, `functionCall` blocks
- `thinking`, `reasoning`, `encrypted_content`
- a `serde_json::Value` (or equivalent) escape hatch

This is enforced, not merely documented: every struct in the type is `deny_unknown_fields`, so a packet carrying one of those keys fails to deserialize rather than being quietly accepted and ignored.

## Minimal packet

The whole packet, with nothing optional. This exact text is what the `team_handoff` tool description shows a caller, and `duduclaw_core::task_packet::MINIMAL_PACKET_EXAMPLE` is the single copy both it and the composer's role header render from:

```json
{"packet_id":"pk-1","goal_id":"<your task_id>","round":1,"from_role":"planner","to_role":"executor","objective":"one sentence: what the next role must do","output_format":"markdown"}
```

Everything else defaults to empty. Add an optional key only when there is something to say with it — `constraints`, `audience`, `acceptance`, `artifacts`, `wiki_refs`, `memory_refs`, `state_keys`, `evidence_index`, `findings`, `open_questions`, `blockers`, `next_steps`, `tool_scope`, `boundaries`, `parent_packet`, `acceptance_baseline_ref`, `budget`, `fidelity`, `irreversible`.

## Example

The same packet with every optional field populated:

```json
{
  "packet_id": "pk-9f2a",
  "goal_id": "task-1183",
  "parent_packet": "pk-9f29",
  "round": 2,
  "from_role": "planner",
  "to_role": "executor",
  "objective": "整理 2026 Q2 到期的客戶合約清單，輸出 CSV",
  "output_format": { "kind": "files" },
  "tool_scope": {
    "allowed": ["db_select", "csv_read", "file_read"],
    "denied": ["mail_send"]
  },
  "boundaries": ["不要主動聯絡客戶", "不要修改 CRM 中的任何紀錄"],
  "constraints": [
    { "id": "c1", "text": "只讀 2026-04-01 至 2026-06-30 的合約" },
    { "id": "c2", "text": "金額一律以新台幣顯示，不換算" }
  ],
  "audience": ["verifier", "channel:telegram"],
  "acceptance": [
    { "kind": "must_use_tools", "value": "db_select" },
    { "kind": "must_not_use_tools", "value": "mail_send" },
    { "kind": "output_contains", "value": "到期日" }
  ],
  "acceptance_baseline_ref": "task-1183#baseline",
  "artifacts": [
    { "id": "a-77", "path": "data/contracts.csv", "sha256": "9c1f…" }
  ],
  "wiki_refs": ["auto/sop/contract-renewal"],
  "memory_refs": ["m-4021"],
  "state_keys": ["reporting_quarter"],
  "evidence_index": ["合約母檔 · a-77"],
  "findings": [
    { "text": "母檔共 412 筆，其中 14 筆落在本季", "evidence": ["a-77"] }
  ],
  "open_questions": ["自動續約的合約要不要算進來？"],
  "blockers": [],
  "next_steps": ["產出 CSV 後交給 verifier 核對筆數"],
  "fidelity": "mcp_only",
  "budget": {
    "max_turns": 5,
    "max_tokens": 120000,
    "wall_clock_secs": 900,
    "max_cost_usd": 1.25
  },
  "irreversible": false
}
```

## Handoff tool (`team_handoff`)

A packet only ever moves through one door: the `team_handoff` MCP tool. It is an explicit tool call by design — a packet is never parsed out of completion text — following the same discipline as `working_state_set`: the caller's identity comes from the process, never from a parameter; every write is audit-logged; every refusal carries a stable code.

**Parameters**

| Param | Required | Meaning |
|---|---|---|
| `packet` | yes | The full TaskPacket, as a JSON object or a JSON-encoded string of one (CLI runtimes serialize nested objects either way). Only the seven required keys are needed, and `goal_id` / `round` / `from_role` / `to_role` may be omitted too — see *Who may call it* under [Handoff tool](#handoff-tool-team_handoff) |

**Returns**

```json
{ "ok": true, "path": "team_packets/<task_id>/r<round>/<from>-to-<to>.json", "bytes": 1042 }
```

`path` is relative to the DuDuClaw home, which is also where the composer reads it from: `<home>/team_packets/<task_id>/<r{round}>/<from_role>-to-<to_role>.json`. The file is written atomically (temp file → fsync → rename) under a cross-process advisory lock, 0600.

**Fan-out and retries.** One round can legitimately carry several packets on one leg — a planner fanning a goal out into independent sub-tasks files one packet per sub-task. Re-filing the *same* `packet_id` overwrites its own file (a retry after a timeout is idempotent); a *different* `packet_id` on the same leg lands in a numbered sibling in the same directory (`planner-to-executor.01.json`, `…02.json`, …, up to 99, then `write_failed`). The composer enumerates that leg by slot number (the canonical file first, then `.01`, `.02`, …; a missing slot ends the scan) and revalidates `from_role`/`to_role` per file rather than trusting one filename; malformed siblings are skipped with an audit line, and a leg with zero valid packets fails the stage. Overwriting the canonical path instead would silently lose every sub-task but the last.

**Who may call it**

The caller must be a role member of a team. Its role, and optionally the task and round it was spawned for, are read from its own agent directory: the `[team_member]` table the spawn scaffold writes (`role`, `task_id`, `round`, `parent`), falling back to `[agent] role` parsed as a team role. An ordinary employee's `role` is an *org* role (`main` / `specialist` / …) and does not parse, so it can never file a packet.

That directory is resolved through the ephemeral resolver, so **both layouts work**: a long-lived agent at `<home>/agents/<id>/` and a team role member at `<home>/agents/.ephemeral/<eph-id>/` — which is where every real role member lives. (Reading only the first layout was the round-2 live-test defect: the one packet a planner did get past the validator came back `not_a_team_member`, and the round ended `planner_no_packets`.)

**Identity is supplied, not demanded.** Because the caller's record is authoritative for `(role, task, round)`, `from_role`, `to_role`, `goal_id` and `round` may all be omitted and are filled in: `from_role` from the record, `to_role` from `from_role` (each pipeline role has exactly one legal outgoing edge), `goal_id` and `round` from the record's pinning when it has one. A value the caller *does* write is left untouched and cross-checked, so a packet claiming someone else's role, task or round is still refused with the codes below — the fill is a convenience, never a bypass.

**Refusals teach.** An `invalid_packet` carries serde's own message first and verbatim (`` invalid_packet: missing field `objective` ``, `` invalid_packet: unknown field `objectives`, expected one of … ``), then ` · send this shape — these seven keys are enough: {…} · optional keys, all defaulting to empty: …`. A validation refusal carries the indexed message from [Error codes](#error-codes). The same text lands in `tool_calls.jsonl` (`error_class` plus the detail in the row's `params`, truncated to 200 CJK-safe chars).

**Legal edges**

`planner → executor`, `executor → verifier`, `verifier → executor` (repair). Nothing else, including every edge touching `utility`, which does not occupy a pipeline stage.

**Refusal codes**

| Code | Cause |
|---|---|
| `missing_packet` | No `packet` argument |
| `invalid_packet` | Not JSON, not an object, or not the TaskPacket shape |
| `forbidden_provider_field` | A `transcript` / `messages` / `tool_use` / `tool_calls` / `function_call` / `functionCall` / `thinking` / `reasoning` / `encrypted_content` key anywhere in the packet. The refusal names the offending key |
| `blank_field`, `field_too_long`, `too_many_items`, `duplicate_constraint_id`, `contradictory_assertion`, `self_handoff`, `packet_too_large` | `TaskPacket::validate` — see [Error codes](#error-codes). The packet is refused whole; nothing is truncated |
| `not_a_team_member` | The caller is not a role member (no `[team_member]`, no parseable `[agent] role`, or no readable `agent.toml`) |
| `team_member_unreadable` | A `[team_member]` table exists but cannot be read. Deliberately not a fallback: degrading would silently drop the task/round pinning |
| `from_role_mismatch` | `from_role` is not the caller's role |
| `task_id_mismatch` | `goal_id` is not the task the caller was assigned |
| `round_mismatch` | `round` is not the round the caller is on |
| `invalid_handoff_edge` | `from_role → to_role` is not one of the three stage transitions |
| `invalid_path_component` | `goal_id` is not usable as a path component, or `round` exceeds 10000 |
| `write_failed` | The packet could not be filed (I/O) |
| `invalid_agent_id` | The caller identity is not a usable agent id |

**Side effects of a successful call**

1. The packet file, at the path above.
2. One `artifacts.jsonl` provenance row: origin `produced`, carrying `task_id` and `round`, so the task's 產物 tab attributes it exactly rather than inferring it from a time window.
3. One `security_audit.jsonl` event `team_handoff` with `{task_id, round, from_role, to_role, bytes, constraints, audience, path}`. `constraints` is a count (bodies belong in the packet, not the security log); `audience` is recorded verbatim, because *who may see this* is the control the allowlist exists to make auditable.

**Scope and grounding**

`team_handoff` requires `team:handoff` (`Scope::TeamHandoff`), a scope that is **not** externally grantable — no operator can hand it to a remote MCP client, and neither `admin` nor any other claimed scope substitutes for it on an external key.

It used to require `memory:write`, on the argument that filing a packet is the same trust tier as `working_state_set` / `working_state_handoff`. The 2026-09-28 review retired that argument: `working_state_*` only ever writes the caller's own `<agent_dir>/state/`, whereas an unpinned `team_handoff` writes into `<home>/team_packets/<task>/r<n>/`, a directory shared across tasks — and `memory:write` *is* in `EXTERNALLY_GRANTABLE_SCOPES`, so any external key carrying it reached the team channel. Internal callers are unaffected and no key had to be reissued: the MCP dispatch gate accepts `Scope::Admin` as a substitute for any required scope, and every gateway-spawned MCP child authenticates with the `admin`-scoped `gateway-internal` key.

The tool is on `SELF_ECHO_TOOL_NAMES`: a packet is the sending role's own summary, so it can never ground that role's claims.

## Never-trim sections

`constraints` and `audience` are exempt from every stage of the prompt-compression pipeline (`TurnTrim`, `DropOldestToolEchoes`, `BisectAndSummarize`), at the same protection level as the `working_state` authority section. The composer renders them under these exact headers, exported from `prompt_compression` so the two cannot drift:

| Constant | Header | Also recognized |
|---|---|---|
| `SECTION_HEADER_CONSTRAINTS` | `## 約束` | `## Constraints` |
| `SECTION_HEADER_AUDIENCE` | `## 受眾` | `## Audience` |

A section runs from its header to the next level-1/level-2 heading. Matching is exact after trimming whitespace — a decorated variant like `## 約束（勿刪）` is *not* protected, because a fuzzy matcher would let arbitrary prose claim immunity from the budget.

When protecting these sections means the token budget cannot be met, the pipeline fails with `BudgetExceeded { protected_section_tokens > 0 }` and the caller sends the request uncompressed. Expensive is recoverable; a silently shortened constraint is not.

## Engineering constraints outside the schema

These are not fields, but a packet that ignores them is not doing its job:

1. `team_handoff` must be an explicit MCP tool call, never parsed out of completion text — the same discipline as `working_state_set`. It is audit-logged, recorded in `artifacts.jsonl`, and listed in `SELF_ECHO_TOOL_NAMES`. (Wired in WP-5, above.)
2. `constraints` and `audience` belong on `prompt_compression`'s never-trim list, at the same level as the `working_state` authority section. (Wired in WP-5, above.)
3. An over-cap packet is rejected, never truncated (see above).
