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
use crate::prediction::rule_lifecycle::{RuleStats, PROBATION_RULE_TAG, RETIRED_RULE_TAG};

/// Section header — deliberately dropped the old "(from past mistakes)"
/// suffix since entries can now originate from signal-matched playbook
/// `Add`s, not only mistake consolidation.
pub const SECTION_HEADER: &str = "## Learned Rules";

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
            if mem_entry.tags.iter().any(|t| t == RETIRED_RULE_TAG) {
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
            let signal_matched = signals::matches(&meta.signals_match, turn_signals);
            let candidate = SelectedEntry {
                id: mem_entry.id,
                content: mem_entry.content,
                category: meta.category,
                net: stats.net(),
                success_streak: meta.success_streak,
                probation,
                signal_matched,
            };
            if signal_matched {
                matched.push(candidate);
            } else {
                unmatched.push(candidate);
            }
        }
    }

    let cmp = |a: &SelectedEntry, b: &SelectedEntry| {
        b.net
            .cmp(&a.net)
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
    let mut body_parts: Vec<&str> = Vec::new();
    let mut ids: Vec<String> = Vec::new();
    let header_len = SECTION_HEADER.chars().count() + 1; // + newline

    for e in entries {
        if ids.len() >= budget.max_entries {
            break;
        }
        // Recompute the actual joined length directly (simplest correct way
        // to know if appending this entry stays within `max_chars`).
        let would_be_body = if body_parts.is_empty() {
            e.content.clone()
        } else {
            format!("{}\n\n{}", body_parts.join("\n\n"), e.content)
        };
        let actual_len = header_len + would_be_body.chars().count();
        if actual_len > budget.max_chars {
            break;
        }
        body_parts.push(&e.content);
        ids.push(e.id.clone());
    }

    if ids.is_empty() {
        return None;
    }
    let body = body_parts.join("\n\n");
    Some((format!("{SECTION_HEADER}\n{body}"), ids))
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
            SelectedEntry { id: "1".into(), content: "a".repeat(50), category: PlaybookCategory::Repair, net: 5, success_streak: 0, probation: false, signal_matched: true },
            SelectedEntry { id: "2".into(), content: "b".repeat(50), category: PlaybookCategory::Repair, net: 4, success_streak: 0, probation: false, signal_matched: true },
        ];
        // Header (18 chars incl. newline) + one 50-char entry = 68 chars;
        // two entries would need 18 + 50 + 2 + 50 = 120. 100 fits exactly
        // one, never a truncated second.
        let budget = InjectionBudget { max_entries: 10, max_chars: 100 };
        let (section, ids) = render_section(&entries, &budget).unwrap();
        assert_eq!(ids, vec!["1".to_string()], "second entry dropped whole, never truncated mid-content");
        assert!(section.contains(&"a".repeat(50)));
        assert!(!section.contains('b'));
    }

    #[test]
    fn render_section_respects_max_entries() {
        let entries: Vec<SelectedEntry> = (0..10)
            .map(|i| SelectedEntry { id: i.to_string(), content: format!("entry {i}"), category: PlaybookCategory::Repair, net: 0, success_streak: 0, probation: false, signal_matched: false })
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
        }];
        // 30 chars + header (~19 chars) well under a byte-sized-but-char-tight budget.
        let budget = InjectionBudget { max_entries: 10, max_chars: 60 };
        let (section, ids) = render_section(&entries, &budget).unwrap();
        assert_eq!(ids.len(), 1);
        assert!(section.chars().count() <= 60);
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
