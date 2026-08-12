//! §1.8 injection selection + [`InjectionBudget`] (WP1.3).
//!
//! Replaces the direct call to `rule_lifecycle::select_rules` in the
//! "## Learned Rules" prompt section (`channel_reply.rs`) with signal-matched
//! priority + score-ranked fill, under an explicit byte budget (not just an
//! entry-count cap) — see the design doc §1.8 for why an unbounded injection
//! is a real cost/cache risk on this codebase's prompt-compression pipeline.
//! `rule_lifecycle::select_rules` itself is untouched and still used by any
//! other caller.

use std::path::Path;

use duduclaw_memory::SqliteMemoryEngine;

use super::entry::{PlaybookCategory, PlaybookMeta, PlaybookState, LEGACY_RULE_SOURCE_EVENT, PLAYBOOK_SOURCE_EVENT};
use super::signals::{self, TurnSignals};
use super::store::CANDIDATE_SCAN_CAP;
use crate::prediction::rule_lifecycle::{RuleStats, PROBATION_RULE_TAG, RETIRED_RULE_TAG, SHADOW_RULE_TAG};
use crate::prediction::rule_staleness::SOURCE_STALE_RULE_TAG;

/// Section header — deliberately dropped the old "(from past mistakes)"
/// suffix since entries can now originate from signal-matched playbook
/// `Add`s, not only mistake consolidation.
pub const SECTION_HEADER: &str = "## Learned Rules";

/// User-facing marker appended to a rule whose source fact has been superseded
/// (G6). Plain-language per the R2 basing: says the underlying information
/// changed, without leaking internal vocabulary. Both downweighted (sorts
/// after fresh rules) AND annotated, so the model can still see a superseded
/// rule but knows to treat it cautiously.
pub const SOURCE_STALE_MARKER: &str = "（來源已更新，僅供參考）";

/// W3-2 — one line telling the model these rules are quotable.
///
/// Without it the section is a silent behavioral constraint: the model obeys
/// the rules but has no licence to say *why*, so 「你為什麼這樣做?」 gets an
/// invented rationalization instead of the actual rule. Paired with the
/// `[法則 N]` labels below (which give each rule a stable handle to point at),
/// this is the whole cost of making the loop explainable — no pipeline change,
/// no extra call. Deliberately phrased as permission, not instruction: a rule
/// citation on every turn would be noise.
pub const SECTION_GUIDANCE: &str =
    "（這些是你從過去經驗歸納出的法則。回答時可自然引用，例如「因為我學過：…」；\
使用者問「你為什麼這樣做？」時，請指出你實際依據的那一條。）";

/// Prefix stamped on each rendered rule so the model can cite a specific one.
fn rule_label(n: usize) -> String {
    format!("[法則 {n}] ")
}

/// One candidate entry ready for rendering into the prompt section.
#[derive(Debug, Clone)]
pub struct SelectedEntry {
    pub id: String,
    pub content: String,
    pub category: PlaybookCategory,
    pub net: i64,
    pub success_streak: u32,
    /// From the memory row's `PROBATION_RULE_TAG` tag (NOT `meta.state`) —
    /// keeps ranking byte-compatible with the pre-existing
    /// `rule_lifecycle::select_rules` tie-break, which the settle path
    /// (`settle_injected_rules`, unmodified) still drives via tags.
    pub probation: bool,
    pub signal_matched: bool,
    /// G6: at least one of this rule's recorded source facts has been
    /// superseded (carries `rule_staleness::SOURCE_STALE_RULE_TAG`). Sorts
    /// after fresh rules and renders with a 「來源已更新」 marker.
    pub source_stale: bool,
}

/// Hard caps on the rendered "## Learned Rules" section (§1.8).
#[derive(Debug, Clone, Copy)]
pub struct InjectionBudget {
    /// Hard cap on entries actually rendered.
    pub max_entries: usize,
    /// Hard cap on characters of the rendered section, INCLUDING the header.
    pub max_chars: usize,
}

impl InjectionBudget {
    pub const DEFAULT_MAX_ENTRIES: usize = 6;
    pub const DEFAULT_MAX_CHARS: usize = 1200;

    pub fn default_budget() -> Self {
        Self { max_entries: Self::DEFAULT_MAX_ENTRIES, max_chars: Self::DEFAULT_MAX_CHARS }
    }

    /// Derive from `agent.toml [budget] max_input_tokens`
    /// (`prompt_audit::read_max_input_tokens`). `None`/`0` (predomain
    /// default, budget disabled) keeps the fixed 1200-char default. A real
    /// budget caps the section at `min(1200, max_input_tokens * 0.05 * 1.5)`
    /// chars — at most ~5% of the total input-token budget, using the
    /// existing `chars/1.5 ≈ tokens` estimator convention
    /// (`prompt_compression::estimate_tokens`).
    pub fn from_max_input_tokens(max_input_tokens: Option<u64>) -> Self {
        let max_chars = match max_input_tokens {
            Some(n) if n > 0 => {
                let derived = (n as f64 * 0.05 * 1.5) as usize;
                derived.min(Self::DEFAULT_MAX_CHARS).max(1)
            }
            _ => Self::DEFAULT_MAX_CHARS,
        };
        Self { max_entries: Self::DEFAULT_MAX_ENTRIES, max_chars }
    }
}

/// Fetch + rank every eligible entry for `agent_id`. Does NOT apply
/// `budget` — that happens in [`render_section`] so the actually-injected
/// id list always matches what was rendered (never "we picked N but only
/// rendered M due to the char cap" drift).
pub async fn select_playbook(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    turn_signals: &TurnSignals,
) -> Vec<SelectedEntry> {
    let mut seen = std::collections::HashSet::new();
    let mut matched: Vec<SelectedEntry> = Vec::new();
    let mut unmatched: Vec<SelectedEntry> = Vec::new();

    for source_event in [PLAYBOOK_SOURCE_EVENT, LEGACY_RULE_SOURCE_EVENT] {
        let rows = match engine.list_valid_by_source_event(agent_id, source_event, CANDIDATE_SCAN_CAP).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(agent = %agent_id, source_event, "playbook: select_playbook list failed: {e}");
                continue;
            }
        };
        for (mem_entry, metadata) in rows {
            if !seen.insert(mem_entry.id.clone()) {
                continue;
            }
            // WP-P3: retired OR shadow-candidate rules are excluded from
            // injection. Shadow tags are only minted when the held-out gate is
            // on, so the `SHADOW_RULE_TAG` arm is a no-op (byte-identical) when
            // the gate is off.
            if mem_entry
                .tags
                .iter()
                .any(|t| t == RETIRED_RULE_TAG || t == SHADOW_RULE_TAG)
            {
                continue;
            }
            let meta = PlaybookMeta::from_metadata(&metadata)
                .unwrap_or_else(|| PlaybookMeta::legacy_default(&mem_entry.tags, &mem_entry.content));
            if meta.state == PlaybookState::Stale {
                // The ONE axis `meta.state` is authoritative for at
                // selection time (see entry::PlaybookState doc) —
                // everything else (Probation/Active/Retired) is read from
                // tags below, exactly like the pre-existing rule path.
                continue;
            }
            let stats = RuleStats::from_metadata(&metadata);
            let probation = mem_entry.tags.iter().any(|t| t == PROBATION_RULE_TAG);
            let source_stale = mem_entry.tags.iter().any(|t| t == SOURCE_STALE_RULE_TAG);
            let signal_matched = signals::matches(&meta.signals_match, turn_signals);
            let candidate = SelectedEntry {
                id: mem_entry.id,
                content: mem_entry.content,
                category: meta.category,
                net: stats.net(),
                success_streak: meta.success_streak,
                probation,
                signal_matched,
                source_stale,
            };
            if signal_matched {
                matched.push(candidate);
            } else {
                unmatched.push(candidate);
            }
        }
    }

    let cmp = |a: &SelectedEntry, b: &SelectedEntry| {
        // G6: a source-stale rule (its underlying fact was superseded) is
        // downweighted below any fresh rule regardless of net score — false
        // (fresh) sorts before true (stale). It is still injectable (with a
        // marker), just last in line.
        a.source_stale
            .cmp(&b.source_stale)
            .then_with(|| b.net.cmp(&a.net))
            .then_with(|| b.success_streak.cmp(&a.success_streak))
            .then_with(|| a.probation.cmp(&b.probation)) // false (graduated) sorts before true (probation)
    };
    matched.sort_by(cmp);
    unmatched.sort_by(cmp);

    matched.extend(unmatched);
    matched
}

/// Render up to `budget` of `entries` into a "## Learned Rules" section,
/// never truncating a single entry's content — stops adding entries once
/// the next one would exceed `max_chars`. Returns `(section_text,
/// injected_ids)`; `None` when nothing was selected (matches the old
/// `build_rules_section_blocking` "no active rules" contract).
pub fn render_section(entries: &[SelectedEntry], budget: &InjectionBudget) -> Option<(String, Vec<String>)> {
    let mut body_parts: Vec<String> = Vec::new();
    let mut ids: Vec<String> = Vec::new();
    // Header + newline + guidance line + newline (W3-2). Both are charged to
    // the same `max_chars` budget the rules are, so an agent with a tiny
    // budget renders fewer rules rather than silently overflowing.
    let prefix_len = SECTION_HEADER.chars().count() + 1 + SECTION_GUIDANCE.chars().count() + 1;

    for e in entries {
        if ids.len() >= budget.max_entries {
            break;
        }
        // G6: append the 「來源已更新」 marker for a source-stale rule so the
        // model sees the caveat inline (the entry is already downweighted to
        // last by the selection sort). Charged to the same char budget.
        let labeled = if e.source_stale {
            format!("{}{} {}", rule_label(ids.len() + 1), e.content, SOURCE_STALE_MARKER)
        } else {
            format!("{}{}", rule_label(ids.len() + 1), e.content)
        };
        // Recompute the actual joined length directly (simplest correct way
        // to know if appending this entry stays within `max_chars`).
        let would_be_body = if body_parts.is_empty() {
            labeled.clone()
        } else {
            format!("{}\n\n{}", body_parts.join("\n\n"), labeled)
        };
        let actual_len = prefix_len + would_be_body.chars().count();
        if actual_len > budget.max_chars {
            break;
        }
        body_parts.push(labeled);
        ids.push(e.id.clone());
    }

    if ids.is_empty() {
        return None;
    }
    let body = body_parts.join("\n\n");
    Some((format!("{SECTION_HEADER}\n{SECTION_GUIDANCE}\n{body}"), ids))
}

/// Blocking helper for the channel-reply prompt-assembly path
/// (spawn_blocking) — mirrors the shape of the pre-existing
/// `rule_lifecycle::build_rules_section_blocking` so it drops into the same
/// call site.
pub fn build_playbook_section_blocking(
    db_path: &Path,
    agent_id: &str,
    turn_signals: &TurnSignals,
    budget: InjectionBudget,
) -> Option<(String, Vec<String>)> {
    let engine = SqliteMemoryEngine::new(db_path).ok()?;
    let rt = tokio::runtime::Handle::current();
    let entries = rt.block_on(select_playbook(&engine, agent_id, turn_signals));
    render_section(&entries, &budget)
}

/// Dialogue-layer armed-shadow scan (closes the v1.54 shadow-scoring debt —
/// design DESIGN-lwm-calibration-2026-08-10.md §6 item 3: every new
/// observation is held-out for every concurrently-trialed candidate).
///
/// [`select_playbook`] above deliberately *skips* shadow candidates (they are
/// scored, never injected). This scan collects the population it skipped:
/// every active shadow candidate whose `signals_match` tokens match THIS
/// turn's [`TurnSignals`] is **armed** — it implicitly predicts "this turn is
/// high-risk", and the settle path
/// (`rule_lifecycle::score_shadow_candidates_by_ids`) grades exactly these
/// ids against the turn's settled outcome. Matching happens here, at
/// prompt-build time, because that is when the turn's signal set exists — the
/// dialogue twin of the task pass's settle-time `goal_kind:` tag match.
///
/// The wildcard is deliberately inert for arming (unlike injection ranking):
/// only discriminating (non-wildcard) tokens can arm, and rows with no such
/// token are excluded from the trial family altogether — a candidate armed on
/// every turn is statistically unfalsifiable (see
/// `signals::has_discriminating_signal`).
#[derive(Debug, Clone, Default)]
pub struct ArmedShadow {
    /// Active shadow candidates signal-matched to this turn (to be scored at
    /// settle).
    pub ids: Vec<String>,
    /// The agent's whole active shadow family, matched or not — the
    /// Bonferroni `k` for the promotion gate (trialing more candidates at
    /// once makes promotion strictly harder).
    pub family_k: usize,
}

/// See [`ArmedShadow`]. Excludes retired rows and `Stale` (superseded)
/// entries — a superseded rule must not earn promotion. Errors degrade to an
/// empty result: arming is an enhancement, never a reply blocker.
pub async fn collect_armed_shadow(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    turn_signals: &TurnSignals,
) -> ArmedShadow {
    let mut seen = std::collections::HashSet::new();
    let mut out = ArmedShadow::default();
    for source_event in [PLAYBOOK_SOURCE_EVENT, LEGACY_RULE_SOURCE_EVENT] {
        let rows = match engine.list_valid_by_source_event(agent_id, source_event, CANDIDATE_SCAN_CAP).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(agent = %agent_id, source_event, "playbook: collect_armed_shadow list failed: {e}");
                continue;
            }
        };
        for (mem_entry, metadata) in rows {
            if !seen.insert(mem_entry.id.clone()) {
                continue;
            }
            let is_shadow = mem_entry.tags.iter().any(|t| t == SHADOW_RULE_TAG);
            let is_retired = mem_entry.tags.iter().any(|t| t == RETIRED_RULE_TAG);
            if !is_shadow || is_retired {
                continue;
            }
            let meta = PlaybookMeta::from_metadata(&metadata)
                .unwrap_or_else(|| PlaybookMeta::legacy_default(&mem_entry.tags, &mem_entry.content));
            if meta.state == PlaybookState::Stale {
                continue;
            }
            // Falsifiability guard: a shadow candidate is only trialable
            // through a discriminating (non-wildcard) trigger. Wildcard-only
            // rows — real `["*"]` entries and `legacy_default` fallbacks
            // alike — would arm on every turn, so their hit rate converges to
            // the baseline itself and the gate can never resolve them (see
            // `signals::has_discriminating_signal`). They are excluded from
            // BOTH the armed set and the Bonferroni family (they are not
            // being trialed); they stay shadow, un-injected, until curated.
            if !signals::has_discriminating_signal(&meta.signals_match) {
                continue;
            }
            out.family_k += 1;
            if signals::matches_ignoring_wildcard(&meta.signals_match, turn_signals) {
                out.ids.push(mem_entry.id);
            }
        }
    }
    out
}

/// Blocking wrapper for [`collect_armed_shadow`] — same spawn_blocking
/// convention as [`build_playbook_section_blocking`], so the channel-reply
/// prompt-assembly closure can run both against one engine handle per call.
pub fn collect_armed_shadow_blocking(
    db_path: &Path,
    agent_id: &str,
    turn_signals: &TurnSignals,
) -> ArmedShadow {
    let Ok(engine) = SqliteMemoryEngine::new(db_path) else {
        return ArmedShadow::default();
    };
    let rt = tokio::runtime::Handle::current();
    rt.block_on(collect_armed_shadow(&engine, agent_id, turn_signals))
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_core::types::{MemoryEntry, MemoryLayer};
    use duduclaw_memory::TemporalMeta;

    async fn store_playbook_entry(
        engine: &SqliteMemoryEngine,
        agent: &str,
        content: &str,
        signals_match: Vec<String>,
        net_helpful: u32,
        probation: bool,
    ) -> String {
        use crate::playbook::entry::PLAYBOOK_KEY;
        use crate::playbook::gene::EvalCaseRef;

        let mut tags = vec!["playbook".to_string()];
        if probation {
            tags.push(PROBATION_RULE_TAG.to_string());
        }
        let meta = PlaybookMeta {
            schema_version: crate::playbook::entry::PLAYBOOK_SCHEMA_VERSION,
            category: PlaybookCategory::Repair,
            signals_match,
            strategy: Vec::new(),
            failure_history: Vec::new(),
            eval_cases: vec![EvalCaseRef("s/c".to_string())],
            applications: Vec::new(),
            success_streak: 0,
            state: PlaybookState::Active,
            revision: 0,
            dedup_key: "0".repeat(16),
            embed_model: None,
            origin: "agent_derived".to_string(),
            derived_from: Vec::new(),
            assertions: Default::default(),
        };
        let entry = MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent.to_string(),
            content: content.to_string(),
            timestamp: chrono::Utc::now(),
            tags,
            embedding: None,
            layer: MemoryLayer::Semantic,
            importance: 8.0,
            access_count: 0,
            last_accessed: None,
            source_event: PLAYBOOK_SOURCE_EVENT.to_string(),
        };
        let mut blob = serde_json::json!({"rule_stats": {"helpful": net_helpful, "harmful": 0}});
        meta.merge_into(&mut blob);
        assert!(blob.get(PLAYBOOK_KEY).is_some());
        let temporal = TemporalMeta { metadata: Some(blob), ..Default::default() };
        engine.store_temporal(agent, entry, temporal).await.unwrap()
    }

    #[tokio::test]
    async fn signal_matched_entry_beats_higher_score_unmatched_entry() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-sel";
        // Higher net score, but no signal match this turn.
        store_playbook_entry(&engine, agent, "generic high-score rule", vec!["channel:discord".to_string()], 10, false).await;
        // Lower net score, but matches this turn's signal.
        let matched_id = store_playbook_entry(&engine, agent, "specific refund rule", vec!["mistake:capability".to_string()], 1, false).await;

        let turn = TurnSignals::new().with_mistake_category("capability");
        let selected = select_playbook(&engine, agent, &turn).await;
        assert_eq!(selected[0].id, matched_id, "signal-matched entry ranks first regardless of net score");
    }

    #[tokio::test]
    async fn retired_entries_are_excluded() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-sel-ret";
        let id = store_playbook_entry(&engine, agent, "will be retired", vec!["*".to_string()], 1, false).await;
        engine.set_importance_and_add_tag(agent, &id, 1.0, RETIRED_RULE_TAG).await.unwrap();

        let turn = TurnSignals::new();
        let selected = select_playbook(&engine, agent, &turn).await;
        assert!(selected.is_empty());
    }

    #[tokio::test]
    async fn stale_entries_are_excluded_from_selection() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-sel-stale";
        let id = store_playbook_entry(&engine, agent, "stale rule", vec!["*".to_string()], 1, false).await;
        let mut meta = engine.get_metadata(agent, &id).await.unwrap().unwrap();
        let mut m = PlaybookMeta::from_metadata(&meta).unwrap();
        m.state = PlaybookState::Stale;
        m.merge_into(&mut meta);
        engine.update_metadata(agent, &id, &meta).await.unwrap();

        let turn = TurnSignals::new();
        let selected = select_playbook(&engine, agent, &turn).await;
        assert!(selected.is_empty());
    }

    #[test]
    fn render_section_never_truncates_a_single_entry() {
        let entries = vec![
            SelectedEntry { id: "1".into(), content: "a".repeat(50), category: PlaybookCategory::Repair, net: 5, success_streak: 0, probation: false, signal_matched: true, source_stale: false },
            SelectedEntry { id: "2".into(), content: "b".repeat(50), category: PlaybookCategory::Repair, net: 4, success_streak: 0, probation: false, signal_matched: true, source_stale: false },
        ];
        // Prefix (header + W3-2 guidance line, both newline-terminated) plus
        // one labeled 50-char entry, with a little slack. A second entry
        // needs another blank line + label + 50 chars, which does not fit —
        // so it must be dropped whole, never truncated mid-content.
        let budget = InjectionBudget {
            max_entries: 10,
            max_chars: prefix_chars() + rule_label(1).chars().count() + 50 + 10,
        };
        let (section, ids) = render_section(&entries, &budget).unwrap();
        assert_eq!(ids, vec!["1".to_string()], "second entry dropped whole, never truncated mid-content");
        assert!(section.contains(&"a".repeat(50)));
        assert!(!section.contains('b'));
    }

    #[test]
    fn render_section_respects_max_entries() {
        let entries: Vec<SelectedEntry> = (0..10)
            .map(|i| SelectedEntry { id: i.to_string(), content: format!("entry {i}"), category: PlaybookCategory::Repair, net: 0, success_streak: 0, probation: false, signal_matched: false, source_stale: false })
            .collect();
        let budget = InjectionBudget { max_entries: 3, max_chars: 100_000 };
        let (_, ids) = render_section(&entries, &budget).unwrap();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn render_section_empty_input_returns_none() {
        let budget = InjectionBudget::default_budget();
        assert!(render_section(&[], &budget).is_none());
    }

    #[test]
    fn render_section_cjk_content_char_counted_not_byte_counted() {
        // Each CJK char is 3 bytes but 1 char — budget must be char-based
        // (spec: "渲染時逐條累加...chars not bytes" per CONTENT_MAX_CHARS convention).
        let entries = vec![SelectedEntry {
            id: "1".into(),
            content: "測".repeat(30), // 30 chars, 90 bytes
            category: PlaybookCategory::Repair,
            net: 0,
            success_streak: 0,
            probation: false,
            signal_matched: false,
            source_stale: false,
        }];
        // 30 chars + prefix + label, with slack — a byte-based budget would
        // have blown past this three times over.
        let max_chars = prefix_chars() + rule_label(1).chars().count() + 30 + 5;
        let budget = InjectionBudget { max_entries: 10, max_chars };
        let (section, ids) = render_section(&entries, &budget).unwrap();
        assert_eq!(ids.len(), 1);
        assert!(section.chars().count() <= max_chars);
        assert!(section.len() > max_chars, "content is multi-byte; a byte budget would differ");
    }

    /// Chars consumed by everything that is not a rule: the header line plus
    /// the W3-2 guidance line, each newline-terminated.
    fn prefix_chars() -> usize {
        SECTION_HEADER.chars().count() + 1 + SECTION_GUIDANCE.chars().count() + 1
    }

    #[test]
    fn rendered_rules_carry_citable_labels_and_the_quotation_guidance() {
        let entries: Vec<SelectedEntry> = (0..3)
            .map(|i| SelectedEntry {
                id: format!("id-{i}"),
                content: format!("rule body {i}"),
                category: PlaybookCategory::Repair,
                net: 0,
                success_streak: 0,
                probation: false,
                signal_matched: false,
                source_stale: false,
            })
            .collect();
        let (section, ids) = render_section(&entries, &InjectionBudget::default_budget()).unwrap();
        assert!(section.starts_with(SECTION_HEADER));
        assert!(section.contains(SECTION_GUIDANCE), "agent has no licence to cite: {section}");
        // Labels are 1-based and in the same order as the returned ids, so a
        // model citing 「法則 2」 points at `ids[1]`.
        assert_eq!(ids.len(), 3);
        for (i, _) in ids.iter().enumerate() {
            assert!(section.contains(&format!("[法則 {}] rule body {i}", i + 1)), "{section}");
        }
        // Internal vocabulary must not reach the prompt's user-visible copy.
        for internal in ["playbook", "shadow", "probation", "GVU"] {
            assert!(!SECTION_GUIDANCE.contains(internal), "guidance leaks `{internal}`");
        }
    }

    // ── G6: source-stale downweight + marker ──

    #[test]
    fn render_section_appends_marker_only_to_source_stale_entries() {
        let entries = vec![
            SelectedEntry { id: "fresh".into(), content: "fresh rule".into(), category: PlaybookCategory::Repair, net: 5, success_streak: 0, probation: false, signal_matched: true, source_stale: false },
            SelectedEntry { id: "stale".into(), content: "outdated rule".into(), category: PlaybookCategory::Repair, net: 4, success_streak: 0, probation: false, signal_matched: true, source_stale: true },
        ];
        let (section, ids) = render_section(&entries, &InjectionBudget::default_budget()).unwrap();
        assert_eq!(ids, vec!["fresh".to_string(), "stale".to_string()]);
        // Marker present exactly once, on the stale rule only.
        assert!(section.contains(&format!("outdated rule {SOURCE_STALE_MARKER}")), "{section}");
        assert!(!section.contains(&format!("fresh rule {SOURCE_STALE_MARKER}")), "{section}");
        assert_eq!(section.matches(SOURCE_STALE_MARKER).count(), 1);
    }

    #[tokio::test]
    async fn select_playbook_downweights_source_stale_rule_below_fresh() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-g6-sel";
        // Stale rule with a HIGHER net score than the fresh one.
        let stale = store_playbook_entry(&engine, agent, "stale high-score rule", vec!["*".to_string()], 10, false).await;
        engine.set_importance_and_add_tag(agent, &stale, 8.0, SOURCE_STALE_RULE_TAG).await.unwrap();
        let fresh = store_playbook_entry(&engine, agent, "fresh low-score rule", vec!["*".to_string()], 1, false).await;

        let turn = TurnSignals::new();
        let selected = select_playbook(&engine, agent, &turn).await;
        let order: Vec<&str> = selected.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            order,
            vec![fresh.as_str(), stale.as_str()],
            "a source-stale rule sorts after a fresh one even with a higher net score"
        );
        assert!(selected.iter().find(|e| e.id == stale).unwrap().source_stale);
        assert!(!selected.iter().find(|e| e.id == fresh).unwrap().source_stale);
    }

    // ── collect_armed_shadow (v1.54 shadow-scoring debt closure) ──

    /// Store a shadow-tagged entry (excluded from injection, candidate for
    /// arming). `extra_tags` lets tests add RETIRED etc.
    async fn store_shadow_entry(
        engine: &SqliteMemoryEngine,
        agent: &str,
        source_event: &str,
        signals_match: Vec<String>,
        extra_tags: Vec<String>,
    ) -> String {
        use crate::playbook::entry::PLAYBOOK_KEY;

        let mut tags = vec![SHADOW_RULE_TAG.to_string()];
        tags.extend(extra_tags);
        let meta = PlaybookMeta {
            schema_version: crate::playbook::entry::PLAYBOOK_SCHEMA_VERSION,
            category: PlaybookCategory::Repair,
            signals_match,
            strategy: Vec::new(),
            failure_history: Vec::new(),
            eval_cases: Vec::new(),
            applications: Vec::new(),
            success_streak: 0,
            state: PlaybookState::Probation,
            revision: 0,
            dedup_key: "0".repeat(16),
            embed_model: None,
            origin: "agent_derived".to_string(),
            derived_from: Vec::new(),
            assertions: Default::default(),
        };
        let entry = MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent.to_string(),
            content: "shadow candidate".to_string(),
            timestamp: chrono::Utc::now(),
            tags,
            embedding: None,
            layer: MemoryLayer::Semantic,
            importance: 8.0,
            access_count: 0,
            last_accessed: None,
            source_event: source_event.to_string(),
        };
        let mut blob = serde_json::json!({"rule_stats": {"helpful": 1, "harmful": 0}});
        meta.merge_into(&mut blob);
        assert!(blob.get(PLAYBOOK_KEY).is_some());
        let temporal = TemporalMeta { metadata: Some(blob), ..Default::default() };
        engine.store_temporal(agent, entry, temporal).await.unwrap()
    }

    #[tokio::test]
    async fn collect_armed_shadow_matches_signals_and_counts_whole_family() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-armed";

        // Matching shadow candidates across BOTH source events.
        let armed_playbook = store_shadow_entry(
            &engine, agent, PLAYBOOK_SOURCE_EVENT,
            vec!["mistake:capability".to_string()], vec![],
        )
        .await;
        let armed_reflexion = store_shadow_entry(
            &engine, agent, LEGACY_RULE_SOURCE_EVENT,
            vec!["mistake:capability".to_string()], vec![],
        )
        .await;
        // Active shadow but non-matching signal: counts toward family_k only.
        store_shadow_entry(
            &engine, agent, LEGACY_RULE_SOURCE_EVENT,
            vec!["channel:discord".to_string()], vec![],
        )
        .await;
        // Retired shadow: excluded from BOTH ids and family.
        store_shadow_entry(
            &engine, agent, LEGACY_RULE_SOURCE_EVENT,
            vec!["mistake:capability".to_string()],
            vec![RETIRED_RULE_TAG.to_string()],
        )
        .await;
        // Active NON-shadow entry: not part of the shadow family at all.
        store_playbook_entry(
            &engine, agent, "active rule",
            vec!["mistake:capability".to_string()], 3, false,
        )
        .await;

        let turn = TurnSignals::new().with_mistake_category("capability");
        let armed = collect_armed_shadow(&engine, agent, &turn).await;
        assert_eq!(armed.family_k, 3, "family = every active shadow candidate, matched or not");
        assert_eq!(armed.ids.len(), 2, "armed = signal-matched active shadow candidates only");
        assert!(armed.ids.contains(&armed_playbook));
        assert!(armed.ids.contains(&armed_reflexion));
    }

    #[tokio::test]
    async fn collect_armed_shadow_excludes_unfalsifiable_wildcard_candidates() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-armed-wildcard";

        // Wildcard-only signal set — real entry or legacy_default fallback
        // shape — is unfalsifiable as a risk predictor: excluded from ids AND
        // family_k.
        let wildcard_only = store_shadow_entry(
            &engine, agent, LEGACY_RULE_SOURCE_EVENT,
            vec!["*".to_string()], vec![],
        )
        .await;
        // Mixed set: the wildcard is inert for arming; the discriminating
        // token governs. Counted in the family.
        let mixed = store_shadow_entry(
            &engine, agent, PLAYBOOK_SOURCE_EVENT,
            vec!["*".to_string(), "mistake:capability".to_string()], vec![],
        )
        .await;

        // Turn WITHOUT the discriminating token: the mixed entry must NOT be
        // armed via its wildcard.
        let unrelated = TurnSignals::new().with_channel("telegram");
        let armed = collect_armed_shadow(&engine, agent, &unrelated).await;
        assert_eq!(armed.family_k, 1, "only the mixed entry is trialable");
        assert!(armed.ids.is_empty(), "wildcard must never arm a shadow candidate");

        // Turn WITH the discriminating token: the mixed entry arms; the
        // wildcard-only entry still doesn't exist as far as the trial goes.
        let matching = TurnSignals::new().with_mistake_category("capability");
        let armed = collect_armed_shadow(&engine, agent, &matching).await;
        assert_eq!(armed.family_k, 1);
        assert_eq!(armed.ids, vec![mixed.clone()]);
        assert!(!armed.ids.contains(&wildcard_only));

        // The wildcard-only row stays shadow and un-injected (unchanged
        // selection behavior) — parked for curation, not silently trialed.
        let selected = select_playbook(&engine, agent, &matching).await;
        assert!(!selected.iter().any(|e| e.id == wildcard_only));
    }

    #[tokio::test]
    async fn collect_armed_shadow_excludes_stale_entries() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-armed-stale";
        let id = store_shadow_entry(
            &engine, agent, PLAYBOOK_SOURCE_EVENT,
            vec!["mistake:capability".to_string()], vec![],
        )
        .await;
        let mut meta = engine.get_metadata(agent, &id).await.unwrap().unwrap();
        let mut m = PlaybookMeta::from_metadata(&meta).unwrap();
        m.state = PlaybookState::Stale;
        m.merge_into(&mut meta);
        engine.update_metadata(agent, &id, &meta).await.unwrap();

        let turn = TurnSignals::new().with_mistake_category("capability");
        let armed = collect_armed_shadow(&engine, agent, &turn).await;
        assert_eq!(armed.family_k, 0, "a superseded (Stale) shadow entry is not trialed");
        assert!(armed.ids.is_empty());
    }

    #[test]
    fn injection_budget_from_max_input_tokens_caps_at_default_when_absent() {
        let b = InjectionBudget::from_max_input_tokens(None);
        assert_eq!(b.max_chars, InjectionBudget::DEFAULT_MAX_CHARS);
        let b = InjectionBudget::from_max_input_tokens(Some(0));
        assert_eq!(b.max_chars, InjectionBudget::DEFAULT_MAX_CHARS);
    }

    #[test]
    fn injection_budget_from_max_input_tokens_scales_down_for_small_budgets() {
        // 1000 tokens * 0.05 * 1.5 = 75 chars — well under the 1200 default.
        let b = InjectionBudget::from_max_input_tokens(Some(1000));
        assert_eq!(b.max_chars, 75);
    }
}
