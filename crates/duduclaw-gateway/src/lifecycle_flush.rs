//! Quarterly flush + promote — long-term context bloat defence (#16, 2026-05-12).
//!
//! Runtime compression (#11/#12/#13/#14) brings *current* prompts under
//! the cliff, but knowledge accumulates monotonically. After 18 months
//! a typical agent has hundreds of wiki pages and dozens of skill files,
//! and even with perfect retrieval-time ranking, the index alone gets
//! large. This module gives operators a periodic, **explicit** decision
//! point: rank artifacts by recent usage and move the cold tail to
//! `cold/` subdirs (still searchable via MCP, but excluded from
//! `build_injection_context`).
//!
//! ## Scope (this commit)
//!
//! - **Pure decision policy** — `decide_flush(items, params) -> FlushPlan`
//!   ranks input by `access_count` ascending and selects the bottom
//!   `archive_pct`. Caller drives file moves separately.
//! - **`FlushPlan`** is a structured report — what would be archived,
//!   what would stay — so the dry-run path can show the operator
//!   exactly what changes before committing.
//! - **No filesystem writes here** — that lives in a thin caller wrapper
//!   that handles `wiki/cold/`, `SKILLS/deprecated/`, SOUL distillation
//!   trigger. Keeping the policy pure means tests don't need temp dirs.
//!
//! ## Out of scope (future work)
//!
//! - Wiki access counter wiring (needs schema change in `wiki_trust.db`)
//! - SOUL distillation trigger (calls Haiku → out-of-scope here; future
//!   `SoulDistiller` module)
//! - CLI `duduclaw lifecycle flush [--dry-run]` (one-line wrapper around
//!   `decide_flush` + filesystem moves)
//!
//! ## Why no filesystem yet
//!
//! Two reasons: (1) testing pure policy is fast and produces reliable
//! signal; (2) the FS layout for "cold" wiki pages is still negotiable
//! — should they live in `wiki/cold/<original-path>`, `wiki/.archive/`,
//! or out-of-band? Locking the policy first gives the next iteration
//! room to pick the layout without re-doing the ranking logic.

use serde::{Deserialize, Serialize};

/// One candidate item to be considered for flushing. Lighter than a full
/// `WikiPage` / `SkillFile` so this module stays generic — the caller
/// constructs these from whichever source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FlushCandidate {
    pub id: String,
    pub access_count: u32,
    /// Days since last access. `None` if access tracking wasn't
    /// available for this item (rare; assume cold).
    pub days_since_access: Option<u32>,
}

/// Knobs for the flush decision.
///
/// `archive_pct` is the fraction of the **coldest** items to archive
/// (0.0 .. 1.0). `min_days_since_access` is a floor — items accessed
/// within this many days are protected from flush regardless of rank,
/// because recency alone matters more than total count for a young
/// knowledge base.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlushParams {
    pub archive_pct: f64,
    pub min_days_since_access: u32,
}

impl Default for FlushParams {
    fn default() -> Self {
        Self {
            // 30 % of the coldest tail is the proposal in #16. Aggressive
            // enough to make a dent on long-tenured agents; conservative
            // enough to avoid an avalanche on the first run.
            archive_pct: 0.30,
            // Anything touched in the last 14 days stays put, even if
            // its total access_count is low (could just be young).
            min_days_since_access: 14,
        }
    }
}

/// Structured plan emitted by `decide_flush`. The `archive` list is
/// what the caller should move to cold storage; `keep` is what stays.
/// Combining both lists must reproduce the input multiset.
#[derive(Debug, Clone)]
pub struct FlushPlan {
    pub archive: Vec<FlushCandidate>,
    pub keep: Vec<FlushCandidate>,
    /// Echo of the params used; useful for audit logs.
    pub params: FlushParams,
}

/// Compute the flush plan for a set of candidate items.
///
/// Algorithm:
/// 1. Items accessed within `min_days_since_access` are unconditionally
///    kept (regardless of their `access_count`).
/// 2. The remaining candidates are sorted by `access_count` ascending
///    (coldest first), then `days_since_access` descending (older first
///    for ties).
/// 3. The bottom `floor(len * archive_pct)` are placed in `archive`.
/// 4. Everything else lands in `keep`.
///
/// Pure: no I/O, no global state, deterministic for equal inputs.
pub fn decide_flush(candidates: &[FlushCandidate], params: &FlushParams) -> FlushPlan {
    let (recent, mut eligible): (Vec<FlushCandidate>, Vec<FlushCandidate>) =
        candidates.iter().cloned().partition(|c| {
            c.days_since_access
                .map(|d| d <= params.min_days_since_access)
                .unwrap_or(false) // None → not recently accessed → eligible
        });

    eligible.sort_by(|a, b| {
        a.access_count.cmp(&b.access_count).then_with(|| {
            // Older = more eligible for archive on ties.
            b.days_since_access
                .unwrap_or(u32::MAX)
                .cmp(&a.days_since_access.unwrap_or(u32::MAX))
        })
    });

    let eligible_count = eligible.len();
    let archive_count = (eligible_count as f64 * params.archive_pct).floor() as usize;
    let archive: Vec<FlushCandidate> = eligible.drain(..archive_count).collect();
    let keep_eligible = eligible;

    let mut keep = recent;
    keep.extend(keep_eligible);

    FlushPlan {
        archive,
        keep,
        params: params.clone(),
    }
}

/// Render a one-line human summary for log output. Stable format so
/// operators can grep `lifecycle_flush:` to track quarter-over-quarter
/// trends.
pub fn summarize_plan(plan: &FlushPlan) -> String {
    format!(
        "lifecycle_flush: archive={} keep={} archive_pct={} min_days={}",
        plan.archive.len(),
        plan.keep.len(),
        plan.params.archive_pct,
        plan.params.min_days_since_access,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: &str, access: u32, days: Option<u32>) -> FlushCandidate {
        FlushCandidate {
            id: id.to_string(),
            access_count: access,
            days_since_access: days,
        }
    }

    #[test]
    fn empty_input_returns_empty_plan() {
        let plan = decide_flush(&[], &FlushParams::default());
        assert!(plan.archive.is_empty());
        assert!(plan.keep.is_empty());
    }

    #[test]
    fn archive_bottom_30pct_by_access_count() {
        let items = vec![
            cand("a", 100, Some(30)), // hot, old enough to be eligible
            cand("b", 5, Some(30)),
            cand("c", 2, Some(30)),
            cand("d", 50, Some(30)),
            cand("e", 1, Some(30)),
            cand("f", 80, Some(30)),
            cand("g", 3, Some(30)),
            cand("h", 60, Some(30)),
            cand("i", 4, Some(30)),
            cand("j", 90, Some(30)),
        ];
        let plan = decide_flush(&items, &FlushParams::default());
        // 30 % of 10 = 3 items archived.
        assert_eq!(plan.archive.len(), 3);
        // The three lowest access_counts are 1, 2, 3 (e, c, g).
        let archived_ids: Vec<&str> = plan.archive.iter().map(|c| c.id.as_str()).collect();
        let expected: std::collections::HashSet<&str> = ["e", "c", "g"].into_iter().collect();
        let got: std::collections::HashSet<&str> = archived_ids.into_iter().collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn recent_items_never_archived_even_when_cold() {
        let items = vec![
            cand("freshly-accessed-but-zero-count", 0, Some(1)), // recent
            cand("week-old-zero-count", 0, Some(7)),             // recent (≤14d)
            cand("month-old-zero-count", 0, Some(30)),           // eligible
        ];
        let plan = decide_flush(&items, &FlushParams::default());
        // archive_pct=30%, eligible count=1 (only the month-old one),
        // floor(1*0.3)=0 → nothing actually archived. But the policy
        // must still classify recent items as "keep regardless".
        let kept_ids: std::collections::HashSet<&str> =
            plan.keep.iter().map(|c| c.id.as_str()).collect();
        assert!(kept_ids.contains("freshly-accessed-but-zero-count"));
        assert!(kept_ids.contains("week-old-zero-count"));
    }

    #[test]
    fn none_days_since_access_treated_as_old_enough_to_archive() {
        let items = vec![
            cand("untracked-a", 0, None),
            cand("untracked-b", 1, None),
            cand("untracked-c", 100, None),
            cand("untracked-d", 5, None),
        ];
        // archive 30 % of 4 = 1 → coldest (untracked-a with access=0).
        let plan = decide_flush(&items, &FlushParams::default());
        assert_eq!(plan.archive.len(), 1);
        assert_eq!(plan.archive[0].id, "untracked-a");
    }

    #[test]
    fn deterministic_tiebreak_oldest_first() {
        // Two items with the same access_count → older one archived.
        let items = vec![
            cand("oldest-tie", 5, Some(60)),
            cand("newer-tie", 5, Some(30)),
            cand("hot", 100, Some(30)),
            cand("very-hot", 1000, Some(30)),
        ];
        let plan = decide_flush(
            &items,
            &FlushParams {
                archive_pct: 0.25, // 1 of 4
                min_days_since_access: 14,
            },
        );
        assert_eq!(plan.archive.len(), 1);
        assert_eq!(plan.archive[0].id, "oldest-tie");
    }

    #[test]
    fn archive_plus_keep_partition_is_lossless() {
        let items = vec![
            cand("a", 1, Some(20)),
            cand("b", 2, Some(30)),
            cand("c", 3, Some(7)), // recent
            cand("d", 4, Some(40)),
            cand("e", 5, None),
        ];
        let plan = decide_flush(&items, &FlushParams::default());
        assert_eq!(plan.archive.len() + plan.keep.len(), items.len());

        // Every input ID appears exactly once in either set.
        let all: std::collections::HashSet<&str> = plan
            .archive
            .iter()
            .chain(plan.keep.iter())
            .map(|c| c.id.as_str())
            .collect();
        for item in &items {
            assert!(all.contains(item.id.as_str()), "lost id: {}", item.id);
        }
    }

    #[test]
    fn summarize_plan_is_stable_for_grep() {
        // Audit grep contract — the format must not drift silently.
        let plan = FlushPlan {
            archive: vec![cand("a", 1, Some(30))],
            keep: vec![cand("b", 50, Some(30))],
            params: FlushParams::default(),
        };
        let summary = summarize_plan(&plan);
        assert!(summary.starts_with("lifecycle_flush:"));
        assert!(summary.contains("archive=1"));
        assert!(summary.contains("keep=1"));
        assert!(summary.contains("archive_pct=0.3"));
        assert!(summary.contains("min_days=14"));
    }

    #[test]
    fn aggressive_archive_pct_archives_all_eligible() {
        // archive_pct = 1.0 → archive every eligible item.
        let items = vec![
            cand("eligible-1", 1, Some(30)),
            cand("eligible-2", 2, Some(30)),
            cand("protected", 0, Some(5)), // recent → protected
        ];
        let plan = decide_flush(
            &items,
            &FlushParams {
                archive_pct: 1.0,
                min_days_since_access: 14,
            },
        );
        assert_eq!(plan.archive.len(), 2);
        assert_eq!(plan.keep.len(), 1);
        assert_eq!(plan.keep[0].id, "protected");
    }
}
