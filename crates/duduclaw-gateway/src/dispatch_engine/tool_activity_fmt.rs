use super::*;

// ── WP4 GroundEval: judge-side tool_activity evidence (arXiv:2606.22737) ──
//
// The MAV judge previously scored a worker's self-reported `result_summary`
// against the acceptance criteria with zero independent evidence — a worker
// that merely *claims* to have called a tool was indistinguishable from one
// that actually did. This reads the existing `tool_calls.jsonl` audit trail
// (already written by every MCP tool invocation) for the claim→review
// window and folds a compact `<tool_activity>` summary into the judge
// prompt. Best-effort: a missing/unreadable audit file omits the block
// (never fails the review over an observability gap — current behavior is
// otherwise unchanged).

/// Cap on distinct tool lines rendered into `<tool_activity>` (keeps a
/// chatty task from ballooning the judge prompt).
const TOOL_ACTIVITY_LINE_CAP: usize = 20;
/// Safety char budget for the whole `<tool_activity>` block.
const TOOL_ACTIVITY_CHAR_CAP: usize = 4000;

// WP-A3 (2026-08): `ToolActivityRecord` / `filter_tool_activity` /
// `read_tool_activity_records` were extracted to `crate::tool_activity` so
// the A3 task-forward-model observation layer (`prediction::task_observe`)
// can share the exact same `tool_calls.jsonl` evidence shape instead of
// reimplementing it a second time. Pure code motion — behavior unchanged,
// this module's own tests below still exercise these functions directly.
pub(super) use crate::tool_activity::{ToolActivityRecord, read_tool_activity_records};

/// Aggregate filtered records into the `<tool_activity>` prompt block: one
/// line per distinct tool (`name: N ok, M err`, sorted by name for
/// determinism), capped at [`TOOL_ACTIVITY_LINE_CAP`] lines and
/// [`TOOL_ACTIVITY_CHAR_CAP`] chars (CJK-safe truncation). `None` when there
/// is nothing to show — the caller omits the block entirely.
///
/// BUG-2 fix (WP-A10 §6 復驗): `native` is the WP-A4 native-tool collector's
/// evidence for this same round (Read/Write/Bash — whatever the runtime saw
/// that never went through an MCP tool call). It is aggregated into the SAME
/// block the judge already reads, one line per tool, tagged `(native)` so a
/// same-named MCP tool never silently merges counts with a different
/// evidence source. Only the name + an ok/err count is rendered here — this
/// is deliberate and unchanged by R1 (2026-08): since R1 a native event MAY
/// carry masked `result_text`/`input_text` (see [`NativeToolEvent`]'s doc
/// comment), but that text is used ONLY for the B3 grounding pre-check
/// ([`grounding_precheck`]), never folded into this judge-facing prompt
/// block — keeps the judge prompt from ballooning with raw tool output and
/// keeps the judge's own injection surface unchanged. Before the original
/// BUG-2 fix the judge saw nothing at all for non-MCP tool use, which is
/// what let honest Read/Write/Bash work read as "zero tool call evidence";
/// a name+count line was already strictly more than that.
pub(super) fn format_tool_activity(
    records: &[ToolActivityRecord],
    native: &[NativeToolEvent],
) -> Option<String> {
    format_tool_activity_body(records, native).map(|b| wrap_tool_activity(&b))
}

/// Wrap an aggregated activity body in its prompt tag. Single source of the
/// tag so the judge prompt and the H1 evaluator transcript never drift.
fn wrap_tool_activity(body: &str) -> String {
    format!("<tool_activity>\n{body}\n</tool_activity>")
}

/// The `<tool_activity>` block for a set of agents' `[since, now]` window.
///
/// Team-as-Agent P1/WP-4 entry point: the team verifier must read the **same**
/// independent evidence the MAV judge reads (design §3.5 — VP-CONTROL
/// arXiv:2609.10969 measured 40.9 pp of the effect coming from the evidence
/// source and only 11.3 pp from model diversity, so a verifier on a different
/// model reading the executor's self-report would buy the small half of the
/// win). Sharing this builder is what keeps the two prompts from drifting into
/// two different notions of "what happened".
///
/// **Why a set** (live round 3 E3): a team's work is done by ephemeral role
/// members under their own agent ids, so a digest scoped to the employee alone
/// was empty and both the verifier and the settle path correctly read that as
/// "nothing was done". Callers pass the employee ∪ that round's members
/// (`crate::role_turns::member_ids_for_task_round`). A one-element set is
/// byte-identical to the pre-E3 single-agent read; duplicate ids are collapsed
/// so an id listed twice cannot double a tool's count.
///
/// `since` absent ⇒ no window can be computed. The block is then **present**
/// and says exactly that ([`TOOL_ACTIVITY_NO_WINDOW`]) — it is NOT folded into
/// the same `None` that means "the window held no tool calls". Review finding
/// 1: team tasks are never claimed, so `claimed_at` was always `None` here,
/// the verifier was handed `(無工具活動紀錄)` every single round, and the
/// independent-evidence mechanism read as "nothing was done" instead of as
/// "we cannot tell". Those are different statements and the reader must be
/// able to tell them apart.
///
/// Native tool events are deliberately not included here: the collector is
/// task-local to the dispatch scope
/// ([`crate::runtime::NATIVE_TOOL_COLLECTOR`]) and a role member runs in its
/// own scope, so there is nothing of the member's to hoist at this point. The
/// MCP audit trail is runtime-neutral and covers every runtime that called a
/// DuDuClaw tool — which is exactly what the composer grades a packet's
/// `fidelity` as `McpOnly` from.
pub(crate) fn tool_activity_block_for_agents(
    home_dir: &std::path::Path,
    agent_ids: &[impl AsRef<str>],
    since: Option<&str>,
) -> Option<String> {
    let Some(since) = since else {
        return Some(wrap_tool_activity(TOOL_ACTIVITY_NO_WINDOW));
    };
    let now = chrono::Utc::now().to_rfc3339();
    let records = read_tool_activity_records_for_agents(home_dir, agent_ids, since, &now);
    format_tool_activity(&records, &[])
}

/// Body rendered by [`tool_activity_block_for_agents`] when the caller could
/// not establish this round's evidence window.
///
/// Deliberately distinct from the "no tool calls in the window" rendering: a
/// verifier that cannot tell "we looked and saw nothing" from "we could not
/// look" will read the second as the first and reject honest work — or, worse,
/// accept a claim on the strength of an absence it never measured.
pub(crate) const TOOL_ACTIVITY_NO_WINDOW: &str =
    "(無法取得本輪證據時窗:起點不明,下方沒有任何可佐證的工具活動——這不等於「沒有動作」)";

/// Safety char budget for the whole `<artifact_receipts>` block. Same order of
/// magnitude as [`TOOL_ACTIVITY_CHAR_CAP`]; a receipt line is bounded (one
/// path + a size + a 64-char hash) so this holds dozens of them.
pub(crate) const ARTIFACT_RECEIPTS_CHAR_CAP: usize = 4000;

/// The `<artifact_receipts>` block for a set of agents' `[since, now]` window
/// — the deterministic sibling of [`tool_activity_block_for_agents`].
///
/// Team-as-Agent live round 8: the executor really did create
/// `notes/a.md b.md index.md`, and the settle really did reject with "no tool
/// activity exists to evidence that any of the files were created". Counting
/// tool *calls* was never going to close that gap — a name+count line says a
/// tool ran, not that a file exists. The team composer now stats and hashes
/// every path a packet declares and writes one
/// [`crate::team_composer::ARTIFACT_RECEIPT_TOOL_NAME`] audit row per
/// artifact; this reads those rows back.
///
/// **Why re-read the audit trail instead of re-hashing here.** Receipts are
/// written at the moment the member's work is on disk, by the code that knows
/// which workspace it wrote into. Re-deriving them at settle time would need
/// to re-resolve the workspace (a registry lookup this module does not have)
/// and would silently disagree with the verifier's copy whenever a later round
/// touched the same file. Reading the row keeps ONE observation with one
/// timestamp, and reuses the same `[since, now]` union the judge's
/// `<tool_activity>` digest already reads.
///
/// VP-CONTROL (arXiv:2609.10969) is the reason this is worth a separate block
/// rather than one more `<tool_activity>` line: shared-evidence cross-model
/// voting let 62.9% of unsafe proposals through, independent-evidence voting
/// only 22.9%. A sha256 of the bytes on disk is the independent source.
pub(crate) fn artifact_receipts_block_for_agents(
    home_dir: &std::path::Path,
    agent_ids: &[impl AsRef<str>],
    since: Option<&str>,
) -> Option<String> {
    let since = since?;
    let now = chrono::Utc::now().to_rfc3339();
    let records = read_tool_activity_records_for_agents(home_dir, agent_ids, since, &now);
    format_artifact_receipts_from_records(&records)
}

/// Pull the receipt lines out of a window's audit records and wrap them.
/// Deduplicated (a re-run round re-verifies the same artifact and would
/// otherwise print the same line twice) while preserving first-seen order.
pub(super) fn format_artifact_receipts_from_records(records: &[ToolActivityRecord]) -> Option<String> {
    let mut seen = std::collections::HashSet::new();
    let mut lines: Vec<String> = Vec::new();
    for r in records {
        if r.tool_name != crate::team_composer::ARTIFACT_RECEIPT_TOOL_NAME {
            continue;
        }
        let Some(line) = r
            .result_text
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        if seen.insert(line.to_string()) {
            lines.push(line.to_string());
        }
    }
    crate::team_composer::format_artifact_receipts(&lines)
}

/// The union of [`read_tool_activity_records`] over several agent ids, in the
/// order given. Empty / duplicate ids are skipped.
pub(super) fn read_tool_activity_records_for_agents(
    home_dir: &std::path::Path,
    agent_ids: &[impl AsRef<str>],
    since: &str,
    until: &str,
) -> Vec<ToolActivityRecord> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for id in agent_ids {
        let id = id.as_ref().trim();
        if id.is_empty() || !seen.insert(id.to_string()) {
            continue;
        }
        out.extend(read_tool_activity_records(home_dir, id, since, until));
    }
    out
}

/// Did `agent_id` record ANY MCP tool call in `[since, until]`?
///
/// The composer's fidelity grader (design §4.3 E4) asks exactly this to tell
/// `McpOnly` from `None` — a member whose audit trail is empty observed
/// nothing, and saying `McpOnly` there would claim evidence that does not
/// exist. A missing/unreadable audit file answers `false`, same fail-open
/// convention as every other reader of this trail.
pub(crate) fn has_tool_activity(
    home_dir: &std::path::Path,
    agent_id: &str,
    since: &str,
    until: &str,
) -> bool {
    !read_tool_activity_records(home_dir, agent_id, since, until).is_empty()
}

/// The un-wrapped body of [`format_tool_activity`] (one `name: N ok, M err`
/// line per distinct tool). Split out so the H1 first-stage evaluator can fold
/// the same evidence into its own transcript under its own tag, without
/// nesting `<tool_activity>` inside `<tool_activity>`.
/// The absolute spellings of an assignee's workspace that may appear in
/// worker output, tool activity and receipts — the path as configured and its
/// canonical form (macOS `/tmp` vs `/private/tmp`), each with a trailing `/`.
pub(crate) fn workspace_prefixes_for(workspace: &std::path::Path) -> Vec<String> {
    let mut out = vec![format!("{}/", workspace.display())];
    if let Ok(c) = workspace.canonicalize() {
        let s = format!("{}/", c.display());
        if !out.contains(&s) {
            out.push(s);
        }
    }
    // Home-relative spellings (`/agents/<id>/…`, `agents/<id>/…`) — live
    // round 12: a member's findings carried them after an upstream layer had
    // already dropped the home prefix, and the evaluator read `/agents/<id>/`
    // as a foreign directory. Ordered longest-first so the absolute forms are
    // consumed before the bare suffix.
    if let Some(id) = workspace.file_name().and_then(|n| n.to_str()) {
        out.push(format!("/agents/{id}/"));
        out.push(format!("agents/{id}/"));
    }
    out
}

/// Characters a path token may legitimately begin right after. Start of text
/// and any whitespace (line start included) count too — see
/// [`strip_workspace_prefixes`].
const PATH_TOKEN_BOUNDARY: &[char] = &['"', '\'', '`', '(', '['];

/// Strip every workspace prefix from `text` so `/…/agents/<id>/notes/a.md`
/// reads as `notes/a.md`. Live rounds 9–11 (2026-09-24): three correct team
/// rounds were rejected by the evaluator / the judge because absolute member
/// paths under `agents/<id>/` were read as "not the working directory". The
/// judge is shown the workspace once and paths relative to it, never both
/// spellings of the same file.
///
/// **Anchored at a path-token boundary** (2026-09-28 review,
/// `review_team.md` §3 "授權／證據"). This used a plain `String::replace`,
/// which matches anywhere: a worker line reading
/// `restored /mnt/backup/agents/agnes/old.md` came out as
/// `restored /mnt/backup/old.md` — a file that does not exist — and that
/// rewritten text is exactly what the acceptance judge is handed as evidence.
/// Mutating evidence is worse than the cosmetic problem the stripping solves,
/// so a prefix is now consumed only where a path token can actually begin:
/// at the start of the text, after whitespace, or after one of
/// [`PATH_TOKEN_BOUNDARY`] (quote / backtick / `(` / `[` — markdown links and
/// code spans). Notably **not** after `/`, which is what made the backup path
/// above match.
///
/// Char-boundary safe by construction: the scan advances whole `char`s and a
/// prefix match always ends on one, so CJK text is never sliced mid-character
/// (coding convention 1).
pub(crate) fn strip_workspace_prefixes(text: &str, prefixes: &[String]) -> String {
    let mut out = String::with_capacity(text.len());
    // Start of text is a boundary: a line that IS the path must still strip.
    let mut at_boundary = true;
    let mut i = 0usize;
    while i < text.len() {
        if at_boundary {
            if let Some(p) = prefixes
                .iter()
                .filter(|p| !p.is_empty())
                .find(|p| text[i..].starts_with(p.as_str()))
            {
                i += p.len();
                // What follows is the middle of one path token, so a second
                // prefix cannot be stripped out of it.
                at_boundary = false;
                continue;
            }
        }
        // `i` is always on a char boundary, so this cannot panic.
        let ch = text[i..].chars().next().unwrap_or('\u{0}');
        out.push(ch);
        at_boundary = ch.is_whitespace() || PATH_TOKEN_BOUNDARY.contains(&ch);
        i += ch.len_utf8();
    }
    out
}

pub(super) fn format_tool_activity_body(
    records: &[ToolActivityRecord],
    native: &[NativeToolEvent],
) -> Option<String> {
    if records.is_empty() && native.is_empty() {
        return None;
    }
    let mut counts: std::collections::BTreeMap<String, (u32, u32)> =
        std::collections::BTreeMap::new();
    for r in records {
        let entry = counts.entry(r.tool_name.clone()).or_insert((0, 0));
        if r.success {
            entry.0 += 1;
        } else {
            entry.1 += 1;
        }
    }
    for e in native {
        let key = format!("{} (native)", e.tool_name);
        let entry = counts.entry(key).or_insert((0, 0));
        if e.success {
            entry.0 += 1;
        } else {
            entry.1 += 1;
        }
    }
    let total_tools = counts.len();
    let mut lines: Vec<String> = counts
        .into_iter()
        .take(TOOL_ACTIVITY_LINE_CAP)
        .map(|(name, (ok, err))| format!("{name}: {ok} ok, {err} err"))
        .collect();
    if total_tools > TOOL_ACTIVITY_LINE_CAP {
        lines.push(format!(
            "… ({} more tool(s) omitted)",
            total_tools - TOOL_ACTIVITY_LINE_CAP
        ));
    }
    let body = duduclaw_core::truncate_chars(&lines.join("\n"), TOOL_ACTIVITY_CHAR_CAP);
    Some(body)
}

