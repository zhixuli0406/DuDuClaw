//! Playbook entry schema (WP1.2, GEP G1/G2/G5/G6).
//!
//! A playbook entry is a `duduclaw-memory` semantic-layer row
//! (`source_event = `[`PLAYBOOK_SOURCE_EVENT`]) whose gene-shaped fields live
//! in the row's `metadata` JSON blob under the [`PLAYBOOK_KEY`] key, beside
//! (never replacing) the existing `rule_stats` key already maintained by
//! `crate::prediction::rule_lifecycle`. See
//! `commercial/docs/DESIGN-evolution-v3-aee.md` §1.2 for the full spec this
//! module implements.
//!
//! `helpful`/`harmful` deliberately stay in `RuleStats` / the `rule_stats`
//! metadata key — they are NOT duplicated here. `PlaybookMeta` only adds
//! `applications` / `success_streak` / `state` on top of the existing Janus
//! lifecycle (`prediction::rule_lifecycle`), which is why the six existing
//! Janus tests in that module need zero changes.

use serde::{Deserialize, Serialize};

use crate::prediction::rule_lifecycle::{PROBATION_RULE_TAG, RETIRED_RULE_TAG};

/// Schema version of the metadata blob. Bump on any breaking field change.
/// Readers MUST tolerate a higher version by degrading to the fields they
/// know (fail-open on read — `serde` already ignores unknown JSON keys by
/// default, so no `deny_unknown_fields` is used here); writers MUST refuse
/// to overwrite a blob whose stored version is higher than this constant
/// (fail-closed on write — see [`raw_schema_version`] and its caller in
/// `crate::playbook::store`).
pub const PLAYBOOK_SCHEMA_VERSION: u32 = 1;

/// `source_event` marking a memory row as a playbook entry.
pub const PLAYBOOK_SOURCE_EVENT: &str = "playbook_entry";
/// Legacy source_event still carrying v1.19 consolidated rules. Both are
/// scanned together on every read path (§1.8) so migration is lazy, not a
/// one-shot batch job (M-2).
pub const LEGACY_RULE_SOURCE_EVENT: &str = "reflexion_consolidation";

/// Metadata JSON key holding the whole gene-shaped payload. Sits beside the
/// existing `rule_stats` / `source_mistake_ids` keys — every write through
/// [`PlaybookMeta::merge_into`] preserves sibling keys.
pub const PLAYBOOK_KEY: &str = "playbook";

/// Applications ring size. Newest-first, capped so the blob stays small and
/// so this data — deliberately — never grows into something worth injecting
/// (only `content` is injected, never `applications`).
pub const APPLICATIONS_CAP: usize = 20;
/// Failure-history ring size (newest-first).
pub const FAILURE_HISTORY_CAP: usize = 5;
/// Compact-content hard ceiling (arXiv:2604.15097: expanding a gene into a
/// document *lowers* effectiveness). Enforced at write time, in `char`s.
pub const CONTENT_MAX_CHARS: usize = 400;
/// Per-agent active (Probation + Active) entry ceiling. MUST stay <=
/// `rule_lifecycle::CANDIDATE_SCAN_CAP` (50) or ranking sees a truncated set.
pub const PLAYBOOK_MAX_ENTRIES: usize = 40;
/// Signal token count ceiling per entry — keeps matching O(1)-ish.
pub const SIGNALS_MAX: usize = 8;
/// Wildcard-signal quota: at most this many of [`PLAYBOOK_MAX_ENTRIES`] may
/// carry only the `*` catch-all signal — the structural guard against the
/// playbook regressing into an always-on SOUL.md (§1.3).
pub const WILDCARD_QUOTA: usize = PLAYBOOK_MAX_ENTRIES / 4;

/// Gene category (GEP G1). Subset actually used this phase: Repair /
/// Optimize / Innovate. Regulatory / Explore are reserved in the enum so a
/// future writer does not need a version bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybookCategory {
    /// Consumes a concrete MistakeNotebook failure.
    Repair,
    /// Sharpens an existing low-streak entry.
    Optimize,
    /// Explores a new rule with no direct failure behind it.
    Innovate,
    /// Reserved: operator/compliance-authored, never auto-retired.
    Regulatory,
    /// Reserved: curiosity-driven probe.
    Explore,
}

impl PlaybookCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Repair => "repair",
            Self::Optimize => "optimize",
            Self::Innovate => "innovate",
            Self::Regulatory => "regulatory",
            Self::Explore => "explore",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "repair" => Self::Repair,
            "optimize" => Self::Optimize,
            "innovate" => Self::Innovate,
            "regulatory" => Self::Regulatory,
            "explore" => Self::Explore,
            _ => return None,
        })
    }
}

/// Lifecycle state machine (§1.7.1). Probation/Active/Retired transitions
/// are driven by the EXISTING tag-based Janus machinery in
/// `prediction::rule_lifecycle::settle_injected_rules` (unchanged — see the
/// module doc above); this field's Probation/Active/Retired values are a
/// snapshot written at Add/Revise time and by the sweep, not an
/// independently-authoritative filter (selection trusts the entry's tags for
/// that, exactly like the pre-existing rule path). The **only** value this
/// field is authoritative for is [`Self::Stale`], which nothing else knows
/// about — that is entirely `crate::playbook::sweep`'s concern (G5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybookState {
    /// Janus trial (arXiv:2606.31121). One harmful settle retires outright.
    Probation,
    /// Graduated. Retires on net-zero (existing rule_lifecycle rule).
    Active,
    /// Ebbinghaus R below threshold — excluded from injection, kept for revival.
    Stale,
    /// Terminal. Excluded from selection AND from the capacity count.
    Retired,
}

impl PlaybookState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Probation => "probation",
            Self::Active => "active",
            Self::Stale => "stale",
            Self::Retired => "retired",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "probation" => Self::Probation,
            "active" => Self::Active,
            "stale" => Self::Stale,
            "retired" => Self::Retired,
            _ => return None,
        })
    }
}

/// Failure history (GEP G1 — the paper's strongest single finding: failure
/// history attached to the gene beats the same text in free-form context).
/// Capped at [`FAILURE_HISTORY_CAP`] newest-first.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FailureNote {
    /// RFC3339. Truncated-second precision to keep the blob small.
    pub at: String,
    /// <= 160 chars, CJK-safe (`duduclaw_core::truncate_chars`).
    pub what: String,
    /// Which gate/measure dimension flagged it ("L1-Deterministic",
    /// "eval:<case>", "settle:critical", "retired_predecessor").
    pub source: String,
}

/// Capsule-style application record (GEP G2): one settle of one injected
/// turn. Newest-first, capped at [`APPLICATIONS_CAP`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Application {
    pub at: String,
    /// Mirrors `ErrorCategory`: negligible | moderate | significant |
    /// critical. Plus "eval_pass" / "eval_fail" for inner-loop settles, and
    /// "dup_add" for a merge-time deduplication record.
    pub outcome: String,
    /// 0.0-1.0. For a channel settle this is the mapped category score
    /// (negligible 1.0 / moderate 0.75 / significant 0.25 / critical 0.0);
    /// for an eval settle it is the case pass ratio.
    pub score: f64,
    /// Optional narrowing: the eval case or session that produced this record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx: Option<String>,
}

/// A validated reference to an eval case: `"<suite>/<case-name>"`. Defined
/// in `crate::playbook::gene` (G6's front door — the gene export mapping is
/// what actually needs it) and re-exported here for schema construction.
pub use super::gene::EvalCaseRef;

/// Per-list assertion token ceiling — assertions are matching tokens, not
/// prose; a long list is a smell (and an H2 weakening surface).
pub const ASSERTION_LIST_MAX: usize = 6;
/// Per-token char ceiling (CJK-safe, counted in `char`s).
pub const ASSERTION_TOKEN_MAX_CHARS: usize = 80;

/// WP2.8 (D8=B) — E1 structured assertions carried by the entry itself.
///
/// E-levels (`TODO-evolution-v3-2026-08.md` §3.1): E0 = only a G6 eval-case
/// link; **E1 = the entry states, in machine-checkable form, what obeying it
/// looks like** — tool calls that must / must not appear, output substrings
/// that must / must not appear. Checked with zero LLM cost against a linked
/// case's recorded transcript (`playbook::assertions`); no transcript ⇒ the
/// check degrades to an advisory, never a veto (WP2.1's corpus is not
/// recorded yet — fail-closed here would freeze evolution outright, which is
/// a worse outcome than the pre-E1 status quo; the veto arms itself
/// automatically as transcripts land).
///
/// New `Add`s must be E1 ([`EntryAssertions::is_e1`] — at least one
/// non-empty list); legacy rows deserialize to the empty default (E0) and
/// are NOT retrofitted (D8: 存量不回填).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EntryAssertions {
    /// Tool names (or `mcp__`-stripped suffixes) that a compliant turn MUST
    /// call at least once.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub must_use_tools: Vec<String>,
    /// Tool names that a compliant turn must NOT call.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub must_not_use_tools: Vec<String>,
    /// Substrings (CJK-safe, case-insensitive for ASCII) the final reply
    /// MUST contain.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_contains: Vec<String>,
    /// Substrings the final reply must NOT contain.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_not_contains: Vec<String>,
}

impl EntryAssertions {
    /// E1 = at least one non-empty assertion list.
    pub fn is_e1(&self) -> bool {
        !(self.must_use_tools.is_empty()
            && self.must_not_use_tools.is_empty()
            && self.output_contains.is_empty()
            && self.output_not_contains.is_empty())
    }

    /// Structural validation (list/token caps, no blank tokens). H2's
    /// tautology detection (an assertion that can never fail) is WP2.10's
    /// job, not this one — this only rejects malformed shapes.
    pub fn validate(&self) -> Result<(), String> {
        for (name, list) in [
            ("must_use_tools", &self.must_use_tools),
            ("must_not_use_tools", &self.must_not_use_tools),
            ("output_contains", &self.output_contains),
            ("output_not_contains", &self.output_not_contains),
        ] {
            if list.len() > ASSERTION_LIST_MAX {
                return Err(format!(
                    "assertions.{name} has {} tokens (max {ASSERTION_LIST_MAX})",
                    list.len()
                ));
            }
            for t in list {
                if t.trim().is_empty() {
                    return Err(format!("assertions.{name} contains a blank token"));
                }
                if t.chars().count() > ASSERTION_TOKEN_MAX_CHARS {
                    return Err(format!(
                        "assertions.{name} token exceeds {ASSERTION_TOKEN_MAX_CHARS} chars"
                    ));
                }
            }
        }
        // A token in both the positive and negative list can never be
        // satisfied — reject the contradiction at write time.
        for t in &self.must_use_tools {
            if self.must_not_use_tools.iter().any(|n| n.eq_ignore_ascii_case(t)) {
                return Err(format!("tool `{t}` is in both must_use_tools and must_not_use_tools"));
            }
        }
        for t in &self.output_contains {
            if self.output_not_contains.iter().any(|n| n == t) {
                return Err(format!("`{t}` is in both output_contains and output_not_contains"));
            }
        }
        Ok(())
    }
}

/// The gene-shaped playbook payload stored under [`PLAYBOOK_KEY`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlaybookMeta {
    /// Schema evolution guard. Always written; missing on read is treated as
    /// legacy (see [`PlaybookMeta::legacy_default`]) and upgraded in place on
    /// first read (M-1/M-2, `crate::playbook::store::list_active`).
    pub schema_version: u32,

    /// Gene category (GEP G1).
    pub category: PlaybookCategory,

    /// Trigger signals (GEP G3). OR semantics: the entry is "signal-matched"
    /// when ANY token matches the turn's signal set. Sorted + deduped on
    /// write so the dedup key is stable.
    pub signals_match: Vec<String>,

    /// Ordered strategy steps (GEP G1). **Empty in phase 1 by design** — the
    /// planning doc's over-engineering guard defers this. Kept in the schema
    /// so the gene export mapping is total from day one.
    #[serde(default)]
    pub strategy: Vec<String>,

    /// Failure history (GEP G1). Capped at [`FAILURE_HISTORY_CAP`] newest-first.
    #[serde(default)]
    pub failure_history: Vec<FailureNote>,

    /// Linked eval cases (GEP G6). `Add` requires >= 1 (rejected otherwise —
    /// see `crate::playbook::delta::validate_delta`). Legacy rows upgraded
    /// via M-1 are exempt (start with an empty list).
    pub eval_cases: Vec<EvalCaseRef>,

    /// WP2.8 (D8=B) — E1 structured assertions. Empty default = E0 (legacy
    /// rows, direct-write paths like reflexion). New `Add`s must be E1.
    #[serde(default, skip_serializing_if = "assertions_is_empty")]
    pub assertions: EntryAssertions,

    /// Capsule-style application records (GEP G2), newest-first, capped at
    /// [`APPLICATIONS_CAP`]. Each entry is one settle of one injected turn.
    #[serde(default)]
    pub applications: Vec<Application>,

    /// Consecutive non-harmful settlements. Reset to 0 on any harmful settle
    /// and on any content revision.
    #[serde(default)]
    pub success_streak: u32,

    /// Lifecycle state machine snapshot (§1.7.1, see [`PlaybookState`] docs
    /// for what is/isn't authoritative).
    pub state: PlaybookState,

    /// Content revision counter — bumped by `Revise`, used by the revision log.
    #[serde(default)]
    pub revision: u32,

    /// Stable dedup key: 16 hex chars of `sha256(normalized content ‖
    /// category)`. Recomputed on every content revision.
    pub dedup_key: String,

    /// Optional embedding fingerprint for near-duplicate detection. `None`
    /// when no embedder is attached — the literal key alone is then
    /// authoritative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed_model: Option<String>,

    /// Where this entry came from. Mirrors (does not replace) the row's
    /// `origin` column so the gene export is self-contained.
    pub origin: String,

    /// Free-form provenance breadcrumb (mistake ids, gvu proposal id,
    /// operator user id). Never injected into any prompt.
    #[serde(default)]
    pub derived_from: Vec<String>,
}

/// `skip_serializing_if` helper — an all-empty assertions block is E0 and
/// serializes to nothing (keeps legacy blobs byte-stable on rewrite).
fn assertions_is_empty(a: &EntryAssertions) -> bool {
    !a.is_e1()
}

impl PlaybookMeta {
    /// Parse the `playbook` blob key out of an entry's metadata JSON.
    /// `None` when the key is absent or malformed (fail-open — caller falls
    /// back to [`Self::legacy_default`]).
    pub fn from_metadata(metadata: &serde_json::Value) -> Option<Self> {
        metadata
            .get(PLAYBOOK_KEY)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// The raw `schema_version` integer as stored, read WITHOUT going
    /// through the strict `PlaybookMeta` deserializer. Used by writers to
    /// implement the fail-closed "refuse to overwrite a newer schema" rule
    /// even when the rest of a newer blob no longer round-trips through this
    /// process's (older) `PlaybookMeta` shape.
    pub fn raw_schema_version(metadata: &serde_json::Value) -> Option<u32> {
        metadata
            .get(PLAYBOOK_KEY)?
            .get("schema_version")?
            .as_u64()
            .map(|v| v as u32)
    }

    /// Write `self` back into a metadata object, preserving all sibling keys
    /// (`rule_stats`, `source_mistake_ids`, ...).
    pub fn merge_into(&self, metadata: &mut serde_json::Value) {
        if !metadata.is_object() {
            *metadata = serde_json::json!({});
        }
        if let Some(obj) = metadata.as_object_mut() {
            obj.insert(
                PLAYBOOK_KEY.to_string(),
                serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({})),
            );
        }
    }

    /// Fail-open default for a legacy `reflexion_consolidation` row that has
    /// never carried a `playbook` blob key (M-1/M-2 lazy migration).
    /// `category = Repair` and `signals_match = ["*"]` per the migration
    /// note (§1.10 M-1) — these entries are exempt from the ">= 1 eval case"
    /// rule (only new `Add`s enforce it), so `eval_cases` starts empty.
    /// State is derived from the row's existing Janus tags so the upgrade
    /// doesn't change observable selection behavior for a single tick.
    pub fn legacy_default(tags: &[String], content: &str) -> Self {
        let state = if tags.iter().any(|t| t == RETIRED_RULE_TAG) {
            PlaybookState::Retired
        } else if tags.iter().any(|t| t == PROBATION_RULE_TAG) {
            PlaybookState::Probation
        } else {
            PlaybookState::Active
        };
        let category = PlaybookCategory::Repair;
        Self {
            schema_version: PLAYBOOK_SCHEMA_VERSION,
            category,
            signals_match: vec!["*".to_string()],
            strategy: Vec::new(),
            failure_history: Vec::new(),
            eval_cases: Vec::new(),
            // Legacy upgrade = E0 (D8: 存量不回填).
            assertions: EntryAssertions::default(),
            applications: Vec::new(),
            success_streak: 0,
            state,
            revision: 0,
            dedup_key: super::dedup::dedup_key(content, category),
            embed_model: None,
            origin: "agent_derived".to_string(),
            derived_from: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_round_trips_through_as_str() {
        for c in [
            PlaybookCategory::Repair,
            PlaybookCategory::Optimize,
            PlaybookCategory::Innovate,
            PlaybookCategory::Regulatory,
            PlaybookCategory::Explore,
        ] {
            assert_eq!(PlaybookCategory::from_str(c.as_str()), Some(c));
        }
        assert_eq!(PlaybookCategory::from_str("bogus"), None);
    }

    #[test]
    fn state_round_trips_through_as_str() {
        for s in [
            PlaybookState::Probation,
            PlaybookState::Active,
            PlaybookState::Stale,
            PlaybookState::Retired,
        ] {
            assert_eq!(PlaybookState::from_str(s.as_str()), Some(s));
        }
    }

    #[test]
    fn merge_into_preserves_sibling_keys() {
        let meta = PlaybookMeta::legacy_default(&[], "some content");
        let mut blob = serde_json::json!({"rule_stats": {"helpful": 3, "harmful": 1}});
        meta.merge_into(&mut blob);
        assert_eq!(blob["rule_stats"]["helpful"], 3);
        assert!(blob.get(PLAYBOOK_KEY).is_some());
    }

    #[test]
    fn from_metadata_is_fail_open_on_absent_key() {
        let blob = serde_json::json!({"rule_stats": {"helpful": 1, "harmful": 0}});
        assert!(PlaybookMeta::from_metadata(&blob).is_none());
    }

    #[test]
    fn legacy_default_derives_state_from_tags() {
        let m = PlaybookMeta::legacy_default(&["probation-rule".to_string()], "x");
        assert_eq!(m.state, PlaybookState::Probation);
        let m = PlaybookMeta::legacy_default(&["retired-rule".to_string()], "x");
        assert_eq!(m.state, PlaybookState::Retired);
        let m = PlaybookMeta::legacy_default(&[], "x");
        assert_eq!(m.state, PlaybookState::Active);
    }

    #[test]
    fn unknown_higher_schema_version_is_read_fail_open() {
        // Simulate a future writer's blob carrying extra unknown fields plus
        // a bumped schema_version — `from_metadata` must still parse the
        // fields it knows (serde default: unknown keys are ignored).
        let blob = serde_json::json!({
            "playbook": {
                "schema_version": 99,
                "category": "repair",
                "signals_match": ["*"],
                "eval_cases": [],
                "state": "active",
                "dedup_key": "abc",
                "origin": "agent_derived",
                "future_field_from_v99": {"nested": true}
            }
        });
        let meta = PlaybookMeta::from_metadata(&blob).expect("fail-open read");
        assert_eq!(meta.schema_version, 99);
        assert_eq!(meta.category, PlaybookCategory::Repair);
        assert_eq!(PlaybookMeta::raw_schema_version(&blob), Some(99));
    }
}
