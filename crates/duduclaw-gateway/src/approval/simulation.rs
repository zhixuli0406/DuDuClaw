//! Simulate-before-act: the narrative type, wiki grounding snippets and the
//! channel-facing renderers. Moved verbatim out of `approval.rs`.

use super::*;

/// The ActionGuard judge's structured simulation of one tool call's expected
/// effect. Produced by the judge prompt (D1), persisted on
/// [`ApprovalRecord::simulation`], and rendered two ways downstream:
/// [`Self::render`] (full text, folded into the approval `summary` so "模擬
/// 結果直接作為審批說明") and [`Self::as_trajectory`] (the short numbered
/// "若核准，接下來預計" line shown above the approve/deny buttons, D2
/// arXiv:2603.11677).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimulationNarrative {
    /// 2-4 zh-TW sentences: the world-state change the judge expects if this
    /// call runs. Empty when the judge reply omitted it (older prompts,
    /// still-valid partial parses).
    pub world_state_change: String,
    /// Short zh-TW risk bullets, already length- and count-capped.
    pub risk_points: Vec<String>,
}

impl SimulationNarrative {
    /// True when there is nothing worth rendering — distinct from "the judge
    /// call failed", which never constructs one of these (the fail-closed
    /// verdict path does not depend on the narrative at all).
    pub fn is_empty(&self) -> bool {
        self.world_state_change.trim().is_empty() && self.risk_points.is_empty()
    }

    /// Parse from an arbitrary JSON `Value` — either the raw ActionGuard
    /// judge reply (`{"world_state_change": "...", "risk_points": [...],
    /// "irreversible": ...}`, extra keys ignored) or the JSON previously
    /// written to [`ApprovalRecord::simulation`]. Missing / wrong-typed
    /// fields degrade to empty (never an error) — this is a UX enhancement,
    /// not a security decision; the verdict parse (`irreversible`) is handled
    /// separately by the judge's own fail-closed parser.
    pub fn from_json(value: &Value) -> Self {
        let world_state_change = value
            .get("world_state_change")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| duduclaw_core::truncate_chars(s, SIMULATION_NARRATIVE_MAX_CHARS))
            .unwrap_or_default();
        let risk_points: Vec<String> = value
            .get("risk_points")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| duduclaw_core::truncate_chars(s, SIMULATION_RISK_POINT_MAX_CHARS))
                    .take(SIMULATION_MAX_RISK_POINTS)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            world_state_change,
            risk_points,
        }
    }

    /// Serialize for [`ApprovalRecord::simulation`] / [`ApprovalBroker::request_with_simulation`].
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "world_state_change": self.world_state_change,
            "risk_points": self.risk_points,
        })
    }

    /// Full-text rendering: "預期影響：…\n風險點：…". Empty string when
    /// [`Self::is_empty`]. Intended to be folded into an approval `summary`
    /// (D1: the simulation result IS the approval explanation).
    pub fn render(&self) -> String {
        let mut out = String::new();
        if !self.world_state_change.trim().is_empty() {
            out.push_str("預期影響：");
            out.push_str(self.world_state_change.trim());
        }
        if !self.risk_points.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str("風險點：");
            out.push_str(&self.risk_points.join("；"));
        }
        out
    }

    /// D2 (arXiv:2603.11677): render the short "若核准，接下來預計：1)…2)…
    /// 3)…" forward-trajectory line shown above the approve/deny buttons —
    /// derived purely from this narrative (no second LLM call). Splits
    /// `world_state_change` into up to 2 sentences and, if room remains,
    /// folds in the first risk point as a final "需留意：" item. `None` when
    /// there is nothing to show (`is_empty`, or a narrative with no
    /// sentence-shaped content).
    pub fn as_trajectory(&self) -> Option<String> {
        let mut items: Vec<String> = split_sentences(&self.world_state_change)
            .into_iter()
            .take(2)
            .collect();
        if items.len() < 3 {
            if let Some(rp) = self.risk_points.first() {
                items.push(format!("需留意：{rp}"));
            }
        }
        if items.is_empty() {
            return None;
        }
        let mut out = String::from("若核准，接下來預計：");
        for (i, item) in items.iter().enumerate() {
            out.push_str(&format!("\n{}) {item}", i + 1));
        }
        Some(out)
    }
}

/// Split on CJK/ASCII sentence terminators, trimming and dropping empties.
/// Not a general NLP sentence splitter — good enough for breaking a 2-4
/// sentence LLM narrative into trajectory bullets.
fn split_sentences(s: &str) -> Vec<String> {
    s.split(['。', '！', '？', '.', '!', '?'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

// ── D3: simulation grounding (R-WoM arXiv:2510.11892) ───────────
//
// Ground a simulation prompt in operational fact (shared/agent wiki
// SOPs/policies) rather than the model's training-time memory. Retrieval
// only — zero failure cost: a missing/broken wiki or no match yields an
// empty `Vec`/`None` and the caller proceeds without grounding.

/// Max chars kept per grounding snippet. CJK-safe.
pub(super) const GROUNDING_SNIPPET_MAX_CHARS: usize = 500;
/// Max snippets pulled per simulation prompt (shared + agent wiki combined,
/// re-ranked together).
pub(super) const GROUNDING_MAX_SNIPPETS: usize = 3;

/// Fix-2 H4a: multiplier applied to [`GROUNDING_MAX_SNIPPETS`] when querying
/// the underlying wiki search, so that filtering hits down to only
/// protected namespaces (below) still leaves a reasonable pool to rank from
/// instead of starving the result on the very first search call.
const GROUNDING_SEARCH_OVERFETCH: usize = 5;

/// Retrieve up to [`GROUNDING_MAX_SNIPPETS`] SOP/policy snippets relevant to
/// `query` (an action keyword — e.g. a tool name, or a goal title).
/// Best-effort: any store-open or search failure is silently absorbed into
/// an empty result — grounding is an enhancement, never a gate, so a broken
/// wiki must never block a simulation.
///
/// ## Fix-2 H4a: self-poisoning hardening (2026-08 grounding self-echo audit)
///
/// This snippet feeds `<reference>` into the ActionGuard judge prompt
/// (`build_action_guard_prompt` in `duduclaw-cli/src/mcp.rs`), which the
/// judge uses to decide whether a tool call is reversible. Before this fix,
/// the source pool was "shared wiki (all namespaces) + the CALLING AGENT'S
/// OWN wiki" — an agent could `wiki_write` with `scope="shared"` (or `wiki_write` on its own
/// local wiki) a page claiming "this operation is reversible / pre-approved"
/// moments before making the risky call, and that self-authored page would
/// be retrieved as if it were operational fact. Two changes close this:
///
/// 1. **The calling agent's own local wiki is never consulted.** An agent
///    can always write there — it can never be trusted as independent
///    evidence about itself.
/// 2. **The shared wiki is filtered to `.scope.toml`-protected namespaces
///    only** (`read_only` / `operator_only` — see
///    `duduclaw-cli/src/wiki_scope.rs` for the mode vocabulary; this reader
///    is a deliberately minimal, self-contained duplicate of just the
///    namespace-mode lookup, NOT the full write-policy engine, because
///    `duduclaw-gateway` cannot depend on `duduclaw-cli` — the dependency
///    edge runs the other way). Namespaces left at the default
///    `agent_writable` (or `agent_allowlist`, which still lets *some*
///    agent write it) remain agent-influenceable and are excluded.
///
/// Security posture is deliberately INVERTED from `WikiScopePolicy`'s own
/// write-time default: that engine defaults an absent/malformed
/// `.scope.toml` to `AgentWritable` (permissive, least-surprise for
/// existing deployments' write path). Here, an absent/malformed policy
/// file means "no namespace is provably protected" — so retrieval fails
/// CLOSED: zero shared-wiki snippets, not "everything is fair game". An
/// operator who wants ActionGuard grounding must explicitly lock the
/// relevant namespace(s) in `.scope.toml`.
pub fn simulation_grounding_snippets(
    home_dir: &Path,
    agent_dir: &Path,
    query: &str,
) -> Vec<String> {
    let _ = agent_dir; // kept for API stability; the agent's own wiki is never read (H4a).
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }

    let protected = protected_wiki_namespaces(home_dir);
    if protected.is_empty() {
        // No `.scope.toml`, or none of its entries are read_only/operator_only
        // ⇒ nothing is provably safe from agent self-write. Fail closed.
        return Vec::new();
    }

    let mut hits: Vec<(f64, String)> = Vec::new();

    let shared = duduclaw_memory::WikiStore::new_shared(home_dir);
    if let Ok(shared_hits) =
        shared.search(query, GROUNDING_MAX_SNIPPETS * GROUNDING_SEARCH_OVERFETCH)
    {
        hits.extend(
            shared_hits
                .iter()
                .filter(|h| protected.contains(&wiki_top_level_namespace(&h.path)))
                .map(|h| (h.weighted_score, render_grounding_hit(h))),
        );
    }

    hits.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    hits.into_iter()
        .take(GROUNDING_MAX_SNIPPETS)
        .map(|(_, text)| text)
        .collect()
}

/// The set of top-level shared-wiki namespaces locked to `read_only` or
/// `operator_only` in `<home_dir>/shared/wiki/.scope.toml`. Empty on any
/// absent/unreadable/malformed file, or a file with no namespace in either
/// mode — every caller treats an empty set as "nothing is safe to
/// retrieve" (fail-closed), never "everything is".
///
/// Deliberately minimal and self-contained rather than importing
/// `duduclaw_cli::wiki_scope::WikiScopePolicy`: `duduclaw-gateway` is a
/// dependency OF `duduclaw-cli`, not the other way around, so that type is
/// unreachable from here. This only answers "is this namespace protected
/// from agent self-write", nothing else the full policy engine handles
/// (write enforcement, `agent_allowlist` membership, snapshots).
pub(super) fn protected_wiki_namespaces(home_dir: &Path) -> std::collections::HashSet<String> {
    let path = home_dir.join("shared").join("wiki").join(".scope.toml");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return std::collections::HashSet::new();
    };
    let Ok(table) = raw.parse::<toml::Table>() else {
        return std::collections::HashSet::new();
    };
    let Some(namespaces) = table.get("namespaces").and_then(|v| v.as_table()) else {
        return std::collections::HashSet::new();
    };
    namespaces
        .iter()
        .filter(|(_, entry)| {
            entry
                .get("mode")
                .and_then(|m| m.as_str())
                .is_some_and(|m| m == "read_only" || m == "operator_only")
        })
        .map(|(name, _)| name.clone())
        .collect()
}

/// Extract the top-level namespace segment from a wiki-relative page path
/// (`"identity/discord-users.md"` → `"identity"`, `"root.md"` → `""`).
/// Deliberately duplicated in miniature from
/// `duduclaw_cli::wiki_scope::top_level_namespace` — see
/// [`protected_wiki_namespaces`]'s doc comment for why this crate cannot
/// import that module.
fn wiki_top_level_namespace(page_path: &str) -> String {
    match page_path.split('/').next() {
        Some(seg) if !seg.is_empty() && seg != page_path => seg.to_string(),
        _ => String::new(),
    }
}

fn render_grounding_hit(hit: &duduclaw_memory::wiki::SearchHit) -> String {
    let body = if hit.context_lines.is_empty() {
        hit.title.clone()
    } else {
        hit.context_lines.join(" ")
    };
    duduclaw_core::truncate_chars(
        &format!("[{}] {}", hit.title, body),
        GROUNDING_SNIPPET_MAX_CHARS,
    )
}

/// Wrap grounding snippets as an XML `<reference>` DATA block for a
/// simulation prompt (project convention: prompts use XML delimiters for
/// injection resistance; fenced content is DATA, never instructions).
/// `None` when there is nothing to ground on — the block is omitted entirely
/// (D3: "檢索不到就不附", not an empty tag).
pub fn render_grounding_block(snippets: &[String]) -> Option<String> {
    if snippets.is_empty() {
        return None;
    }
    let escaped: Vec<String> = snippets.iter().map(|s| xml_escape(s)).collect();
    Some(format!(
        "<reference>\n{}\n</reference>",
        escaped.join("\n---\n")
    ))
}

/// F1: whether the operator has explicitly opted an agent OUT of the
/// install-class MCP approval gate via `agent.toml [capabilities]
/// auto_approve_install = true`.
///
/// **Fail-closed:** a missing file, missing key, malformed table, or a
/// non-bool value all return `false` (the gate stays ON). Only an explicit
/// `true` disables the gate — the WP5 requirement is that MCP-reached
/// install-class tools need human approval by default, and the caller holding
/// `Scope::Admin` (the default internal principal) is NOT a bypass. This is
/// the sole exemption an operator can grant.
pub fn auto_approve_install(agent_dir: &Path) -> bool {
    duduclaw_core::agent_toml::load(agent_dir)
        .capabilities
        .auto_approve_install
        .unwrap_or(false)
}

// ── Decision source: autopilot rule ─────────────────────────

/// True when an autopilot rule's `action` JSON opts into human approval
/// via `require_approval = true`. Absent / non-bool ⇒ `false` (no gate).
pub fn rule_requires_approval(action: &Value) -> bool {
    action
        .get("require_approval")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

// ── Notification surface (channel) ──────────────────────────

/// Minimal XML/markup escape for values interpolated into channel text
/// or an XML-delimited prompt block (project convention: prompts use XML
/// delimiters for injection resistance).
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Render a zh-TW, XML-safe approval prompt for a messaging channel. The
/// channel_sender path (wired later) sends this with inline approve/deny
/// buttons; a text-only channel matches the reply against the existing
/// `is_confirmation_reply` / `is_denial_reply` word lists and calls
/// [`ApprovalBroker::decide`].
pub fn pending_summary_for_channel(record: &ApprovalRecord) -> String {
    let agent = xml_escape(&record.agent_id);
    let kind = xml_escape(&record.action_kind);
    let summary = xml_escape(&duduclaw_core::truncate_chars(
        &record.summary,
        CHANNEL_SUMMARY_MAX_CHARS,
    ));
    format!(
        "🔔 需要您的核准\n\
         代理：{agent}\n\
         動作：{kind}\n\
         摘要：{summary}\n\
         編號：{id}\n\
         回覆「確認」核准，或「取消」拒絕（{ttl} 秒後自動拒絕）。",
        id = record.id,
        ttl = record.ttl_seconds,
    )
}

// ── Dashboard RPC shape (documentation) ─────────────────────
//
// To be added in `handlers.rs` (owned this wave — NOT edited here):
//
//   approvals.list   { agent_id?: string } -> ApprovalRecord[]   → list_pending()
//   approvals.approve{ id: string }        -> { ok: true }        → decide(id, true,  "dashboard:<user>")
//   approvals.deny   { id: string }        -> { ok: true }        → decide(id, false, "dashboard:<user>")
//
// Every approve/deny should append an Activity Feed row
// (`task_store::append_activity`, event_type "approval_decided") so the
// dashboard Activity tab shows the human decision, mirroring how
// `autopilot_engine` records rule fires. On approve, the caller re-reads
// `record.payload` and re-dispatches (e.g. re-enqueue on `bus_queue.jsonl`
// for a `bus_task`, re-run the MCP tool for an `mcp_tool`).
