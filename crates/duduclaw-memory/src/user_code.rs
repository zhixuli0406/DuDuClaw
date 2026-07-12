//! M2 "User as Code" — read-only experiment.
//!
//! Paper: **User as Code: Executable Memory for Personalized Agents**
//! (Bojie Li, arXiv:2606.16707). UaC represents the user model as an executable
//! software project — typed objects hold state, code functions encode governing
//! rules — instead of unstructured text consulted via retrieval. The paper's
//! two-phase pipeline (append-only fact log, periodically checkpointed into
//! typed code) maps directly onto DuDuClaw's memory engine: the temporal SPO
//! triple store *is* the append-only log (supersession chains never discard a
//! fact), and this module is the checkpoint step — it **compiles** the
//! currently-valid user facts into a typed [`UserProfile`] whose conflict
//! resolution is deterministic execution, not prompt-time LLM judgment.
//!
//! ## Experiment status
//!
//! READ-ONLY EXPERIMENT (TODO-feature-gaps-2026-07-11 §2.2 M2). This module
//! reads the engine through its public API and writes nothing anywhere. No
//! production path consumes it yet; behavior of the running system is
//! unchanged. Everything here is deterministic — no LLM calls, no clock-free
//! randomness, no fallback guessing (ties are surfaced as [`Conflict`]s and
//! unparseable rows are counted in `unparsed_count`, never force-fitted).
//!
//! ## What would graduate this to production
//!
//! 1. A consumer wired to [`UserProfile::check`] — e.g. the proactive-message
//!    scheduler gating sends on `check("proactive_message@hour=H")`, or the
//!    approval UX surfacing matching constraints next to a pending action.
//! 2. Cache/invalidations: recompile on `store_temporal` writes for the user's
//!    subject instead of per-call compilation.
//! 3. Conflict surfacing in the dashboard so operators resolve `Conflicted`
//!    keys instead of the profile silently carrying them.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_core::word_contains_ci;
use serde::Serialize;

use crate::engine::SqliteMemoryEngine;

/// Reserved predicate for the consolidated free-text profile summary
/// (see `user_profile::SUMMARY_PREDICATE`); it is *derived* from the raw
/// traits, so compiling it would double-count every fact.
const SUMMARY_PREDICATE: &str = "profile_summary";

/// The canonical action tag quiet-hours constraints normalize to, so
/// `check("proactive_message@hour=23")` matches a stored `quiet_hours` /
/// `勿擾時段` fact without any string guessing at evaluation time.
pub const ACTION_PROACTIVE_MESSAGE: &str = "proactive_message";

/// Deterministic mapping for the fuzzy zh/en words "深夜" / "night":
/// 22:00 → 08:00 local, documented rather than guessed per-call.
const NIGHT_HOURS: (u8, u8) = (22, 8);

// ─────────────────────────────────────────────────────────────────────────────
// Typed model
// ─────────────────────────────────────────────────────────────────────────────

/// Where a compiled rule came from — enough to trace any rule back to the
/// exact memory row (and its supersession chain) that produced it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Provenance {
    /// Memory row id in the engine.
    pub memory_id: String,
    /// Effective start of validity: `valid_from` when set, else the row's
    /// write `timestamp`. `None` only when neither parses (kept honest).
    pub valid_from: Option<DateTime<Utc>>,
    /// Stored confidence (engine default 1.0).
    pub confidence: f64,
    /// The predicate exactly as stored, before synonym normalization.
    pub raw_predicate: Option<String>,
    /// Back-pointer of the supersession chain, when the engine recorded one.
    pub supersedes: Option<String>,
    /// Forward pointer — normally `None` for currently-valid rows.
    pub superseded_by: Option<String>,
}

/// Preference direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Polarity {
    Positive,
    Negative,
}

/// A deterministic, executable condition attached to a [`UserRule::Constraint`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Condition {
    /// Wrapping local-hour window `[start, end)` — `22..8` matches 22, 23,
    /// 0‥7. `start == end` means "all day".
    HourRange { start: u8, end: u8 },
}

impl Condition {
    /// Pure evaluation: is `hour` (0‥23) inside this condition's window?
    pub fn matches_hour(&self, hour: u8) -> bool {
        match *self {
            Condition::HourRange { start, end } => {
                if start == end {
                    true
                } else if start < end {
                    hour >= start && hour < end
                } else {
                    hour >= start || hour < end
                }
            }
        }
    }
}

/// One typed user rule, compiled from a stored fact. Each variant carries its
/// [`Provenance`] so every rule is traceable to the memory row it came from.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum UserRule {
    /// "User prefers/dislikes X" — from `prefers_*` / `喜歡` / `討厭` …
    Preference {
        /// What the preference is about (`language` for `prefers_language`,
        /// or the object itself for a bare `prefers`).
        topic: String,
        /// The preferred value when the predicate names the topic
        /// (`prefers_language: python` → topic `language`, value `python`).
        value: Option<String>,
        polarity: Polarity,
        /// Deterministic verb-class strength: loves/hates 1.0, prefers 0.7,
        /// likes/dislikes 0.5. NOT the same thing as provenance confidence.
        strength: f64,
        provenance: Provenance,
    },
    /// "Never do X (when C)" — from `must_not_*` / `quiet_hours` / `勿擾時段` …
    Constraint {
        /// Canonical action tag the constraint applies to.
        action: String,
        /// Optional executable condition; `None` = unconditional.
        condition: Option<Condition>,
        provenance: Provenance,
    },
    /// Any other full SPO triple — kept typed but shape-agnostic.
    Fact {
        subject: String,
        predicate: String,
        object: String,
        provenance: Provenance,
    },
}

impl UserRule {
    pub fn provenance(&self) -> &Provenance {
        match self {
            UserRule::Preference { provenance, .. }
            | UserRule::Constraint { provenance, .. }
            | UserRule::Fact { provenance, .. } => provenance,
        }
    }

    /// The key two rules must share to be considered *about the same thing*
    /// (and therefore candidates for conflict resolution).
    pub fn conflict_key(&self) -> String {
        match self {
            UserRule::Preference { topic, .. } => {
                format!("preference:{}", topic.to_lowercase())
            }
            UserRule::Constraint { action, .. } => {
                format!("constraint:{}", action.to_lowercase())
            }
            UserRule::Fact {
                subject, predicate, ..
            } => format!(
                "fact:{}:{}",
                subject.to_lowercase(),
                predicate.to_lowercase()
            ),
        }
    }

    /// Provenance-free body fingerprint: two rules with equal fingerprints say
    /// the same thing and are deduplicated (newest kept) instead of conflicting.
    fn body_fingerprint(&self) -> String {
        match self {
            UserRule::Preference {
                topic,
                value,
                polarity,
                strength,
                ..
            } => format!(
                "p|{}|{}|{:?}|{}",
                topic.to_lowercase(),
                value.as_deref().unwrap_or("").to_lowercase(),
                polarity,
                strength
            ),
            UserRule::Constraint {
                action, condition, ..
            } => format!("c|{}|{:?}", action.to_lowercase(), condition),
            UserRule::Fact {
                subject,
                predicate,
                object,
                ..
            } => format!(
                "f|{}|{}|{}",
                subject.to_lowercase(),
                predicate.to_lowercase(),
                object.to_lowercase()
            ),
        }
    }
}

/// A same-key group the deterministic resolver could NOT settle: kept and
/// surfaced, never guessed at. Candidates preserve compile order
/// (newest effective time first).
#[derive(Debug, Clone, Serialize)]
pub struct Conflict {
    pub key: String,
    pub candidates: Vec<UserRule>,
}

/// The compiled, typed user profile — the "checkpoint" artifact of the UaC
/// pipeline. `rules` are conflict-free; unresolved groups live in `conflicts`;
/// rows the deterministic parsers could not type are *counted*, not invented.
#[derive(Debug, Clone, Serialize)]
pub struct UserProfile {
    pub agent_id: String,
    pub rules: Vec<UserRule>,
    pub conflicts: Vec<Conflict>,
    pub unparsed_count: usize,
}

/// One `check()` match with full provenance for the consumer to act on.
#[derive(Debug, Clone, Serialize)]
pub struct RuleHit {
    pub rule: UserRule,
    /// `true` when the hit comes from an unresolved [`Conflict`] group —
    /// consumers should treat it as "needs a human", not as settled truth.
    pub conflicted: bool,
    /// `false` when the rule has a [`Condition`] but the action descriptor
    /// lacked the parameter needed to evaluate it (the hit is then surfaced
    /// conservatively instead of silently dropped — fail-closed for callers
    /// that gate on constraints).
    pub condition_evaluated: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// Predicate vocabulary — deterministic synonym tables (zh-TW + English)
// ─────────────────────────────────────────────────────────────────────────────
//
// Sources actually observed in this codebase:
//   - wiki_ingest distillation prompt: `prefers_<topic>` (e.g. prefers_language),
//     fallback `mentioned_in_conversation`, subjects `user:<name>`
//   - user_profile MCP tool suggestions: prefers / timezone / language / pronouns
//   - reserved: profile_summary (excluded here)

const PREF_POSITIVE: &[&str] = &[
    "prefers",
    "prefer",
    "likes",
    "like",
    "loves",
    "favorite",
    "favourite",
    "偏好",
    "喜歡",
    "喜好",
    "最愛",
    "愛用",
];
const PREF_NEGATIVE: &[&str] = &[
    "dislikes",
    "dislike",
    "hates",
    "hate",
    "討厭",
    "不喜歡",
    "不愛",
    "反感",
];
const PREF_POSITIVE_PREFIX: &[&str] = &[
    "prefers_",
    "preferred_",
    "likes_",
    "loves_",
    "favorite_",
    "偏好_",
    "喜歡_",
];
const PREF_NEGATIVE_PREFIX: &[&str] = &["dislikes_", "hates_", "討厭_", "不喜歡_"];
const QUIET_HOURS_PREDS: &[&str] = &[
    "quiet_hours",
    "do_not_disturb",
    "do_not_disturb_hours",
    "dnd_hours",
    "勿擾",
    "勿擾時段",
    "深夜勿擾",
];
const PROHIBITION_PREDS: &[&str] = &[
    "must_not", "never", "avoid", "forbid", "禁止", "不要", "勿", "避免",
];
const PROHIBITION_PREFIX: &[&str] = &["must_not_", "never_", "avoid_", "禁止_", "不要_"];

/// Deterministic verb-class strength (documented in [`UserRule::Preference`]).
fn verb_strength(pred: &str) -> f64 {
    match pred {
        "loves" | "hates" | "hate" | "最愛" => 1.0,
        "prefers" | "prefer" | "偏好" => 0.7,
        _ => 0.5,
    }
}

/// Strip a matching prefix (char-boundary-safe: prefixes are matched with
/// `strip_prefix`, never byte slicing).
fn strip_any_prefix<'a>(
    pred: &'a str,
    prefixes: &[&'static str],
) -> Option<(&'static str, &'a str)> {
    for p in prefixes {
        if let Some(rest) = pred.strip_prefix(p) {
            if !rest.is_empty() {
                return Some((p.trim_end_matches('_'), rest));
            }
        }
    }
    None
}

/// Parse an hour-range object like `22-8`, `22:00-08:00`, `23~07`, `22時到8時`,
/// or the documented fuzzy words `深夜` / `night` / `late night`.
/// Returns `None` for anything it cannot parse deterministically.
fn parse_hour_range(object: &str) -> Option<Condition> {
    let trimmed = object.trim();
    let lower = trimmed.to_lowercase();
    if trimmed == "深夜" || lower == "night" || lower == "late night" || lower == "overnight" {
        return Some(Condition::HourRange {
            start: NIGHT_HOURS.0,
            end: NIGHT_HOURS.1,
        });
    }
    // Normalize separators to '-'.
    let norm: String = trimmed
        .chars()
        .map(|c| match c {
            '–' | '—' | '~' | '～' | '至' => '-',
            other => other,
        })
        .collect();
    let norm = norm.replace("到", "-");
    let mut parts = norm.splitn(2, '-');
    let a = parse_hour(parts.next()?)?;
    let b = parse_hour(parts.next()?)?;
    Some(Condition::HourRange { start: a, end: b })
}

/// Parse a single hour token: leading ASCII digits of `"22"`, `"22:00"`,
/// `"8時"`, `" 08 "` → 0‥23.
fn parse_hour(token: &str) -> Option<u8> {
    let digits: String = token
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() || digits.len() > 2 {
        return None;
    }
    let h: u8 = digits.parse().ok()?;
    (h <= 23).then_some(h)
}

/// Deterministic parser: full SPO triple → typed rule body. Never fails for a
/// full triple (Fact is the honest fallback shape).
fn parse_triple(subject: &str, predicate: &str, object: &str, prov: Provenance) -> UserRule {
    let pred_norm = predicate.trim().to_lowercase();
    let pred_norm = pred_norm.as_str();

    // Quiet-hours family → canonical proactive-message constraint.
    if QUIET_HOURS_PREDS.contains(&pred_norm) {
        return UserRule::Constraint {
            action: ACTION_PROACTIVE_MESSAGE.to_string(),
            condition: parse_hour_range(object),
            provenance: prov,
        };
    }
    // Bare prohibitions: object is the forbidden action.
    if PROHIBITION_PREDS.contains(&pred_norm) {
        return UserRule::Constraint {
            action: object.trim().to_string(),
            condition: None,
            provenance: prov,
        };
    }
    // Prefixed prohibitions: `must_not_send_email: <maybe hours>`.
    if let Some((_, action)) = strip_any_prefix(pred_norm, PROHIBITION_PREFIX) {
        return UserRule::Constraint {
            action: action.to_string(),
            condition: parse_hour_range(object),
            provenance: prov,
        };
    }
    // Bare preferences: topic IS the object (`prefers: coffee`).
    if PREF_POSITIVE.contains(&pred_norm) {
        return UserRule::Preference {
            topic: object.trim().to_string(),
            value: None,
            polarity: Polarity::Positive,
            strength: verb_strength(pred_norm),
            provenance: prov,
        };
    }
    if PREF_NEGATIVE.contains(&pred_norm) {
        return UserRule::Preference {
            topic: object.trim().to_string(),
            value: None,
            polarity: Polarity::Negative,
            strength: verb_strength(pred_norm),
            provenance: prov,
        };
    }
    // Prefixed preferences: `prefers_language: python` → topic language.
    if let Some((verb, topic)) = strip_any_prefix(pred_norm, PREF_POSITIVE_PREFIX) {
        return UserRule::Preference {
            topic: topic.to_string(),
            value: Some(object.trim().to_string()),
            polarity: Polarity::Positive,
            strength: verb_strength(verb),
            provenance: prov,
        };
    }
    if let Some((verb, topic)) = strip_any_prefix(pred_norm, PREF_NEGATIVE_PREFIX) {
        return UserRule::Preference {
            topic: topic.to_string(),
            value: Some(object.trim().to_string()),
            polarity: Polarity::Negative,
            strength: verb_strength(verb),
            provenance: prov,
        };
    }
    // Everything else stays a typed Fact (timezone / language / pronouns /
    // mentioned_in_conversation / …) — no force-fitting.
    UserRule::Fact {
        subject: subject.trim().to_string(),
        predicate: predicate.trim().to_string(),
        object: object.trim().to_string(),
        provenance: prov,
    }
}

/// Narrow, documented content parser for triple-less user-profile entries.
/// Handles exactly two shapes deterministically:
///   1. `"<predicate>: <value>"` (the `record_trait` content format);
///   2. the zh/en night-DND idioms (`勿在深夜打擾` / `深夜勿擾` /
///      `do not disturb at night`).
/// Anything else is honest `None` → `unparsed_count`.
fn parse_content(subject: &str, content: &str, prov: Provenance) -> Option<UserRule> {
    let c = content.trim();
    let lower = c.to_lowercase();
    if (c.contains("勿") && c.contains("深夜") && (c.contains("打擾") || c.contains("打扰")))
        || lower == "do not disturb at night"
    {
        return Some(UserRule::Constraint {
            action: ACTION_PROACTIVE_MESSAGE.to_string(),
            condition: Some(Condition::HourRange {
                start: NIGHT_HOURS.0,
                end: NIGHT_HOURS.1,
            }),
            provenance: prov,
        });
    }
    let (pred, value) = c.split_once(':')?;
    let (pred, value) = (pred.trim(), value.trim());
    if pred.is_empty() || value.is_empty() {
        return None;
    }
    let mut prov = prov;
    prov.raw_predicate = Some(pred.to_string());
    Some(parse_triple(subject, pred, value, prov))
}

// ─────────────────────────────────────────────────────────────────────────────
// Compiler
// ─────────────────────────────────────────────────────────────────────────────

/// One raw row pulled from the engine (read-only).
struct RawRow {
    id: String,
    content: String,
    subject: Option<String>,
    predicate: Option<String>,
    object: Option<String>,
    valid_from: Option<String>,
    timestamp: String,
    confidence: Option<f64>,
    supersedes: Option<String>,
    superseded_by: Option<String>,
}

fn parse_rfc3339(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Compile the currently-valid user facts of `agent_id` into a typed
/// [`UserProfile`]. READ-ONLY: issues SELECTs only, writes nothing.
///
/// Inputs (via the engine's public connection, same pattern as
/// `user_profile::profile_traits`):
///   - currently-valid SPO triples whose `subject` starts with `user:`
///     (wiki-ingest distillation + `user_profile_record` writes);
///   - currently-valid entries tagged `user-profile` (covers triple-less
///     hand-tagged rows);
///   - the derived `profile_summary` row is excluded (it would double-count).
///
/// Conflict resolution is deterministic, in tier order:
///   1. supersession-chain link between candidates → chain head wins;
///   2. strictly newer effective `valid_from` (falls back to `timestamp`) wins;
///   3. strictly higher stored confidence wins;
///   4. still tied → the group is kept as a [`Conflict`], never guessed.
pub async fn compile_user_profile(
    agent_id: &str,
    engine: &SqliteMemoryEngine,
) -> Result<UserProfile> {
    let rows: Vec<RawRow> = {
        let conn = engine.conn_for_maintenance().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, content, subject, predicate, object,
                        valid_from, timestamp, confidence, supersedes, superseded_by
                 FROM memories
                 WHERE agent_id = ?1
                   AND valid_until IS NULL
                   AND (predicate IS NULL OR predicate != ?2)
                   AND ((subject LIKE 'user:%' AND predicate IS NOT NULL)
                        OR tags LIKE ?3)
                 ORDER BY COALESCE(valid_from, timestamp) DESC, id ASC",
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let mapped = stmt
            .query_map(
                rusqlite::params![agent_id, SUMMARY_PREDICATE, "%\"user-profile\"%"],
                |r| {
                    Ok(RawRow {
                        id: r.get(0)?,
                        content: r.get(1)?,
                        subject: r.get(2)?,
                        predicate: r.get(3)?,
                        object: r.get(4)?,
                        valid_from: r.get(5)?,
                        timestamp: r.get(6)?,
                        confidence: r.get(7)?,
                        supersedes: r.get(8)?,
                        superseded_by: r.get(9)?,
                    })
                },
            )
            .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let mut v = Vec::new();
        for row in mapped {
            v.push(row.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
        }
        v
    };

    let mut typed: Vec<UserRule> = Vec::new();
    let mut unparsed_count = 0usize;

    for row in rows {
        let prov = Provenance {
            memory_id: row.id.clone(),
            valid_from: row
                .valid_from
                .as_deref()
                .and_then(parse_rfc3339)
                .or_else(|| parse_rfc3339(&row.timestamp)),
            confidence: row.confidence.unwrap_or(1.0),
            raw_predicate: row.predicate.clone(),
            supersedes: row.supersedes.clone(),
            superseded_by: row.superseded_by.clone(),
        };
        let full_triple = match (&row.subject, &row.predicate, &row.object) {
            (Some(s), Some(p), Some(o))
                if !s.trim().is_empty() && !p.trim().is_empty() && !o.trim().is_empty() =>
            {
                Some((s.clone(), p.clone(), o.clone()))
            }
            _ => None,
        };
        match full_triple {
            Some((s, p, o)) => typed.push(parse_triple(&s, &p, &o, prov)),
            None => {
                let subject = row
                    .subject
                    .clone()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "user:unknown".to_string());
                match parse_content(&subject, &row.content, prov) {
                    Some(rule) => typed.push(rule),
                    None => unparsed_count += 1,
                }
            }
        }
    }

    // Group by conflict key. BTreeMap for deterministic key order; insertion
    // order inside a group preserves the newest-first row sort.
    let mut groups: BTreeMap<String, Vec<UserRule>> = BTreeMap::new();
    for rule in typed {
        groups.entry(rule.conflict_key()).or_default().push(rule);
    }

    let mut rules = Vec::new();
    let mut conflicts = Vec::new();
    for (key, group) in groups {
        match resolve_group(group) {
            Resolution::Winner(rule) => rules.push(rule),
            Resolution::Conflicted(candidates) => conflicts.push(Conflict { key, candidates }),
        }
    }

    Ok(UserProfile {
        agent_id: agent_id.to_string(),
        rules,
        conflicts,
        unparsed_count,
    })
}

enum Resolution {
    Winner(UserRule),
    Conflicted(Vec<UserRule>),
}

/// Deterministic same-key resolution (tiers documented on
/// [`compile_user_profile`]). `group` arrives newest-effective-first.
fn resolve_group(group: Vec<UserRule>) -> Resolution {
    // Dedupe identical bodies: newest (first seen) kept.
    let mut seen = std::collections::HashSet::new();
    let mut candidates: Vec<UserRule> = Vec::new();
    for rule in group {
        if seen.insert(rule.body_fingerprint()) {
            candidates.push(rule);
        }
    }
    if candidates.len() == 1 {
        return Resolution::Winner(candidates.into_iter().next().expect("len==1"));
    }

    // Tier 1 — supersession chain: a candidate is chain-dead when another
    // candidate points at it via `supersedes`, or it points forward via
    // `superseded_by` to another candidate. Pointers to rows OUTSIDE the group
    // (the normal already-invalidated predecessors) are ignored — tier 1 only
    // decides when the chain links two *currently-valid* group members.
    let dead: Vec<bool> = candidates
        .iter()
        .map(|c| {
            let p = c.provenance();
            let pointed_at = candidates.iter().any(|other| {
                other.provenance().memory_id != p.memory_id
                    && other.provenance().supersedes.as_deref() == Some(p.memory_id.as_str())
            });
            let points_forward = p
                .superseded_by
                .as_deref()
                .map(|succ| candidates.iter().any(|o| o.provenance().memory_id == succ))
                .unwrap_or(false);
            pointed_at || points_forward
        })
        .collect();
    if dead.iter().any(|d| *d) && dead.iter().any(|d| !*d) {
        let survivors: Vec<UserRule> = candidates
            .into_iter()
            .zip(dead)
            .filter_map(|(c, is_dead)| (!is_dead).then_some(c))
            .collect();
        if survivors.len() == 1 {
            return Resolution::Winner(survivors.into_iter().next().expect("len==1"));
        }
        candidates = survivors;
    }

    // Tier 2 — strictly newest effective valid_from.
    let newest = candidates
        .iter()
        .filter_map(|c| c.provenance().valid_from)
        .max();
    if let Some(newest) = newest {
        let at_newest: Vec<&UserRule> = candidates
            .iter()
            .filter(|c| c.provenance().valid_from == Some(newest))
            .collect();
        if at_newest.len() == 1 {
            return Resolution::Winner(at_newest[0].clone());
        }
    }

    // Tier 3 — strictly highest confidence.
    let best = candidates
        .iter()
        .map(|c| c.provenance().confidence)
        .fold(f64::NEG_INFINITY, f64::max);
    let at_best: Vec<&UserRule> = candidates
        .iter()
        .filter(|c| (c.provenance().confidence - best).abs() < f64::EPSILON)
        .collect();
    if at_best.len() == 1 {
        return Resolution::Winner(at_best[0].clone());
    }

    // Tier 4 — surfaced, never guessed.
    Resolution::Conflicted(candidates)
}

// ─────────────────────────────────────────────────────────────────────────────
// Evaluation API
// ─────────────────────────────────────────────────────────────────────────────

/// Parsed form of an action descriptor like `"proactive_message@hour=23"`:
/// action tag plus `key=value` params (comma-separated after `@`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionDescriptor {
    pub action: String,
    pub params: BTreeMap<String, String>,
}

impl ActionDescriptor {
    pub fn parse(s: &str) -> ActionDescriptor {
        let (action, rest) = match s.split_once('@') {
            Some((a, r)) => (a, Some(r)),
            None => (s, None),
        };
        let mut params = BTreeMap::new();
        if let Some(rest) = rest {
            for pair in rest.split(',') {
                if let Some((k, v)) = pair.split_once('=') {
                    let (k, v) = (k.trim(), v.trim());
                    if !k.is_empty() && !v.is_empty() {
                        params.insert(k.to_string(), v.to_string());
                    }
                }
            }
        }
        ActionDescriptor {
            action: action.trim().to_string(),
            params,
        }
    }

    fn hour(&self) -> Option<u8> {
        self.params.get("hour").and_then(|v| parse_hour(v))
    }
}

/// Boundary-aware topic/action matching: exact case-insensitive equality, or
/// ASCII whole-word containment (`word_contains_ci` — the codebase's anchored
/// matcher), or plain containment for non-ASCII needles (CJK has no word
/// boundaries to anchor on).
fn tag_matches(haystack: &str, needle: &str) -> bool {
    if needle.trim().is_empty() {
        return false;
    }
    if haystack.eq_ignore_ascii_case(needle) {
        return true;
    }
    if needle.is_ascii() {
        word_contains_ci(haystack, needle)
    } else {
        haystack.contains(needle)
    }
}

impl UserProfile {
    /// Pure evaluation: which compiled rules apply to a proposed action?
    ///
    /// `action_descriptor` is an action tag with optional params, e.g.
    /// `"proactive_message@hour=23"`. Returns matching constraints and
    /// preferences with provenance; hits from unresolved [`Conflict`] groups
    /// are included with `conflicted = true` so a consumer can escalate
    /// instead of guessing. No I/O, no clock, no LLM.
    pub fn check(&self, action_descriptor: &str) -> Vec<RuleHit> {
        let desc = ActionDescriptor::parse(action_descriptor);
        let mut hits = Vec::new();
        for rule in &self.rules {
            if let Some(hit) = match_rule(rule, &desc, false) {
                hits.push(hit);
            }
        }
        for conflict in &self.conflicts {
            for rule in &conflict.candidates {
                if let Some(hit) = match_rule(rule, &desc, true) {
                    hits.push(hit);
                }
            }
        }
        hits
    }
}

fn match_rule(rule: &UserRule, desc: &ActionDescriptor, conflicted: bool) -> Option<RuleHit> {
    match rule {
        UserRule::Constraint {
            action, condition, ..
        } => {
            if !tag_matches(&desc.action, action) && !tag_matches(action, &desc.action) {
                return None;
            }
            match condition {
                None => Some(RuleHit {
                    rule: rule.clone(),
                    conflicted,
                    condition_evaluated: true,
                }),
                Some(cond) => match desc.hour() {
                    Some(h) => cond.matches_hour(h).then(|| RuleHit {
                        rule: rule.clone(),
                        conflicted,
                        condition_evaluated: true,
                    }),
                    // Param missing → surface conservatively (fail-closed for
                    // consumers that gate on constraints).
                    None => Some(RuleHit {
                        rule: rule.clone(),
                        conflicted,
                        condition_evaluated: false,
                    }),
                },
            }
        }
        UserRule::Preference { topic, value, .. } => {
            let topic_hit = tag_matches(&desc.action, topic)
                || desc.params.values().any(|v| tag_matches(v, topic))
                || value
                    .as_deref()
                    .map(|val| desc.params.values().any(|v| tag_matches(v, val)))
                    .unwrap_or(false);
            topic_hit.then(|| RuleHit {
                rule: rule.clone(),
                conflicted,
                condition_evaluated: true,
            })
        }
        // Facts are shape-agnostic state, not action rules — check() skips them.
        UserRule::Fact { .. } => None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use duduclaw_core::types::{MemoryEntry, MemoryLayer};

    use crate::engine::TemporalMeta;

    fn entry(agent: &str, content: &str, tags: Vec<&str>) -> MemoryEntry {
        MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent.to_string(),
            content: content.to_string(),
            timestamp: Utc::now(),
            tags: tags.into_iter().map(str::to_string).collect(),
            embedding: None,
            layer: MemoryLayer::Semantic,
            importance: 5.0,
            access_count: 0,
            last_accessed: None,
            source_event: "test".to_string(),
        }
    }

    async fn store_triple(
        engine: &SqliteMemoryEngine,
        agent: &str,
        subject: &str,
        predicate: &str,
        object: &str,
        valid_from: Option<DateTime<Utc>>,
        confidence: Option<f64>,
    ) -> String {
        engine
            .store_temporal(
                agent,
                entry(agent, &format!("{predicate}: {object}"), vec![]),
                TemporalMeta {
                    subject: Some(subject.to_string()),
                    predicate: Some(predicate.to_string()),
                    object: Some(object.to_string()),
                    valid_from,
                    confidence,
                    ..Default::default()
                },
            )
            .await
            .unwrap()
    }

    fn t(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
    }

    // ── Typed parsing across zh-TW / en spellings ────────────────────────────

    #[tokio::test]
    async fn parses_en_and_zh_predicates_into_typed_rules() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        store_triple(
            &engine,
            "a",
            "user:alice",
            "prefers_language",
            "python",
            None,
            None,
        )
        .await;
        store_triple(&engine, "a", "user:alice", "喜歡", "貓", None, None).await;
        store_triple(&engine, "a", "user:alice", "討厭", "會議", None, None).await;
        store_triple(&engine, "a", "user:alice", "勿擾時段", "22-08", None, None).await;
        store_triple(
            &engine,
            "a",
            "user:alice",
            "timezone",
            "Asia/Taipei",
            None,
            None,
        )
        .await;

        let profile = compile_user_profile("a", &engine).await.unwrap();
        assert!(profile.conflicts.is_empty());
        assert_eq!(profile.unparsed_count, 0);
        assert_eq!(profile.rules.len(), 5);

        let lang = profile
            .rules
            .iter()
            .find(|r| matches!(r, UserRule::Preference { topic, .. } if topic == "language"))
            .expect("prefers_language typed as Preference");
        match lang {
            UserRule::Preference {
                value,
                polarity,
                strength,
                ..
            } => {
                assert_eq!(value.as_deref(), Some("python"));
                assert_eq!(*polarity, Polarity::Positive);
                assert!((strength - 0.7).abs() < 1e-9, "prefers verb class = 0.7");
            }
            _ => unreachable!(),
        }

        assert!(profile.rules.iter().any(|r| matches!(
            r,
            UserRule::Preference { topic, polarity: Polarity::Positive, .. } if topic == "貓"
        )));
        assert!(profile.rules.iter().any(|r| matches!(
            r,
            UserRule::Preference { topic, polarity: Polarity::Negative, .. } if topic == "會議"
        )));
        assert!(profile.rules.iter().any(|r| matches!(
            r,
            UserRule::Constraint { action, condition: Some(Condition::HourRange { start: 22, end: 8 }), .. }
                if action == ACTION_PROACTIVE_MESSAGE
        )));
        assert!(profile.rules.iter().any(|r| matches!(
            r,
            UserRule::Fact { predicate, object, .. }
                if predicate == "timezone" && object == "Asia/Taipei"
        )));
    }

    #[tokio::test]
    async fn provenance_carries_memory_id_and_confidence() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let id = store_triple(
            &engine,
            "a",
            "user:u",
            "prefers",
            "tea",
            Some(t(0)),
            Some(0.8),
        )
        .await;
        let profile = compile_user_profile("a", &engine).await.unwrap();
        let p = profile.rules[0].provenance();
        assert_eq!(p.memory_id, id);
        assert!((p.confidence - 0.8).abs() < 1e-9);
        assert_eq!(p.valid_from, Some(t(0)));
    }

    // ── Conflict resolution tiers ────────────────────────────────────────────

    #[tokio::test]
    async fn tier1_supersession_chain_wins() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        // Craft two currently-valid, chain-linked rows directly (the engine
        // normally closes the old row; a crash between UPDATE and INSERT, or an
        // external import, can leave both valid — exactly what tier 1 covers).
        // Same normalized key via synonym predicates so they group together.
        let old = store_triple(
            &engine,
            "a",
            "user:u",
            "prefers_editor",
            "vim",
            Some(t(0)),
            None,
        )
        .await;
        let new = store_triple(
            &engine,
            "a",
            "user:u",
            "likes_editor",
            "emacs",
            Some(t(0)),
            None,
        )
        .await;
        {
            let conn = engine.conn_for_maintenance().await;
            // Link the chain but leave BOTH rows valid, and keep timestamps +
            // confidence identical so only tier 1 can decide.
            conn.execute(
                "UPDATE memories SET supersedes = ?1 WHERE id = ?2",
                rusqlite::params![old, new],
            )
            .unwrap();
        }
        let profile = compile_user_profile("a", &engine).await.unwrap();
        assert!(profile.conflicts.is_empty(), "chain link must resolve");
        let winner = profile
            .rules
            .iter()
            .find(|r| r.conflict_key() == "preference:editor")
            .unwrap();
        assert_eq!(winner.provenance().memory_id, new);
    }

    #[tokio::test]
    async fn tier2_newer_valid_from_wins() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        store_triple(
            &engine,
            "a",
            "user:u",
            "prefers_language",
            "python",
            Some(t(0)),
            None,
        )
        .await;
        let newer = store_triple(
            &engine,
            "a",
            "user:u",
            "likes_language",
            "rust",
            Some(t(100)),
            None,
        )
        .await;
        let profile = compile_user_profile("a", &engine).await.unwrap();
        assert!(profile.conflicts.is_empty());
        let winner = profile
            .rules
            .iter()
            .find(|r| r.conflict_key() == "preference:language")
            .unwrap();
        assert_eq!(winner.provenance().memory_id, newer);
    }

    #[tokio::test]
    async fn tier3_higher_confidence_wins() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        store_triple(
            &engine,
            "a",
            "user:u",
            "prefers_language",
            "python",
            Some(t(0)),
            Some(0.5),
        )
        .await;
        let confident = store_triple(
            &engine,
            "a",
            "user:u",
            "likes_language",
            "rust",
            Some(t(0)),
            Some(0.9),
        )
        .await;
        let profile = compile_user_profile("a", &engine).await.unwrap();
        assert!(profile.conflicts.is_empty());
        let winner = profile
            .rules
            .iter()
            .find(|r| r.conflict_key() == "preference:language")
            .unwrap();
        assert_eq!(winner.provenance().memory_id, confident);
    }

    #[tokio::test]
    async fn tier4_full_tie_is_surfaced_as_conflict() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        store_triple(
            &engine,
            "a",
            "user:u",
            "prefers_language",
            "python",
            Some(t(0)),
            Some(0.5),
        )
        .await;
        store_triple(
            &engine,
            "a",
            "user:u",
            "likes_language",
            "rust",
            Some(t(0)),
            Some(0.5),
        )
        .await;
        let profile = compile_user_profile("a", &engine).await.unwrap();
        assert!(
            !profile
                .rules
                .iter()
                .any(|r| r.conflict_key() == "preference:language"),
            "tied group must not produce a guessed winner"
        );
        assert_eq!(profile.conflicts.len(), 1);
        assert_eq!(profile.conflicts[0].key, "preference:language");
        assert_eq!(profile.conflicts[0].candidates.len(), 2);
    }

    #[tokio::test]
    async fn identical_bodies_dedupe_instead_of_conflicting() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        // Same meaning through two synonym predicates → same fingerprint.
        store_triple(
            &engine,
            "a",
            "user:u",
            "likes_language",
            "rust",
            Some(t(0)),
            None,
        )
        .await;
        store_triple(
            &engine,
            "a",
            "user:u",
            "喜歡_language",
            "rust",
            Some(t(50)),
            None,
        )
        .await;
        let profile = compile_user_profile("a", &engine).await.unwrap();
        assert!(profile.conflicts.is_empty());
        assert_eq!(
            profile
                .rules
                .iter()
                .filter(|r| r.conflict_key() == "preference:language")
                .count(),
            1
        );
    }

    // ── check() matching ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn check_matches_quiet_hours_constraint() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        store_triple(
            &engine,
            "a",
            "user:u",
            "quiet_hours",
            "22:00-08:00",
            None,
            None,
        )
        .await;
        let profile = compile_user_profile("a", &engine).await.unwrap();

        let hits = profile.check("proactive_message@hour=23");
        assert_eq!(hits.len(), 1);
        assert!(hits[0].condition_evaluated);
        assert!(!hits[0].conflicted);

        assert!(
            profile.check("proactive_message@hour=12").is_empty(),
            "noon is outside 22-08"
        );

        // Hour param missing → surfaced conservatively, marked unevaluated.
        let cautious = profile.check("proactive_message");
        assert_eq!(cautious.len(), 1);
        assert!(!cautious[0].condition_evaluated);

        assert!(
            profile.check("send_invoice@hour=23").is_empty(),
            "unrelated action"
        );
    }

    #[tokio::test]
    async fn check_matches_preferences_and_flags_conflicts() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        store_triple(
            &engine,
            "a",
            "user:u",
            "prefers_language",
            "python",
            Some(t(0)),
            Some(0.5),
        )
        .await;
        store_triple(
            &engine,
            "a",
            "user:u",
            "likes_language",
            "rust",
            Some(t(0)),
            Some(0.5),
        )
        .await;
        store_triple(&engine, "a", "user:u", "must_not", "send_email", None, None).await;

        let profile = compile_user_profile("a", &engine).await.unwrap();

        let hits = profile.check("draft@language=python");
        assert!(
            hits.iter().any(|h| h.conflicted),
            "conflicted preference must be surfaced, flagged"
        );

        let email = profile.check("send_email");
        assert_eq!(email.len(), 1);
        assert!(matches!(email[0].rule, UserRule::Constraint { .. }));
        assert!(!email[0].conflicted);
    }

    #[test]
    fn hour_range_wraps_and_parses_variants() {
        for raw in ["22-8", "22:00-08:00", "22~08", "22時到8時", "深夜", "night"] {
            let cond = parse_hour_range(raw).unwrap_or_else(|| panic!("parse {raw}"));
            assert!(cond.matches_hour(23), "{raw} matches 23");
            assert!(cond.matches_hour(3), "{raw} matches 3");
            assert!(!cond.matches_hour(12), "{raw} excludes noon");
        }
        assert_eq!(parse_hour_range("whenever"), None);
        assert_eq!(parse_hour_range("25-99"), None);
    }

    // ── Empty store / malformed input honesty ────────────────────────────────

    #[tokio::test]
    async fn empty_store_compiles_empty_profile() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let profile = compile_user_profile("nobody", &engine).await.unwrap();
        assert!(profile.rules.is_empty());
        assert!(profile.conflicts.is_empty());
        assert_eq!(profile.unparsed_count, 0);
    }

    #[tokio::test]
    async fn malformed_rows_count_as_unparsed_without_panicking() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        // Tagged user-profile but no triple and no "pred: value" content.
        engine
            .store_temporal(
                "a",
                entry("a", "總之他人很好", vec!["user-profile"]),
                TemporalMeta::default(),
            )
            .await
            .unwrap();
        // Tagged, no triple, but content in record_trait format → recovered.
        engine
            .store_temporal(
                "a",
                entry("a", "timezone: Asia/Taipei", vec!["user-profile"]),
                TemporalMeta::default(),
            )
            .await
            .unwrap();
        // Tagged, triple-less zh night-DND idiom → recovered as Constraint.
        engine
            .store_temporal(
                "a",
                entry("a", "勿在深夜打擾", vec!["user-profile"]),
                TemporalMeta::default(),
            )
            .await
            .unwrap();
        // Triple with empty object → not a full triple, content is
        // "empty_pred: " which fails the value check → unparsed.
        engine
            .store_temporal(
                "a",
                entry("a", "empty_pred:", vec!["user-profile"]),
                TemporalMeta {
                    subject: Some("user:u".to_string()),
                    predicate: Some("empty_pred".to_string()),
                    object: Some("   ".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let profile = compile_user_profile("a", &engine).await.unwrap();
        assert_eq!(profile.unparsed_count, 2, "free text + empty object");
        assert!(profile.rules.iter().any(|r| matches!(
            r,
            UserRule::Fact { predicate, object, .. }
                if predicate == "timezone" && object == "Asia/Taipei"
        )));
        assert!(profile
            .rules
            .iter()
            .any(|r| matches!(r, UserRule::Constraint { .. })));
    }

    #[tokio::test]
    async fn profile_summary_row_is_excluded() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        crate::user_profile::record_trait(&engine, "a", "u1", "prefers", "tea", 1.0)
            .await
            .unwrap();
        crate::user_profile::record_trait(&engine, "a", "u1", "timezone", "Asia/Taipei", 1.0)
            .await
            .unwrap();
        crate::user_profile::record_trait(&engine, "a", "u1", "language", "zh-TW", 1.0)
            .await
            .unwrap();
        crate::user_profile::consolidate_profile(&engine, "a", "u1", 3)
            .await
            .unwrap()
            .expect("summary written");

        let profile = compile_user_profile("a", &engine).await.unwrap();
        assert_eq!(profile.rules.len(), 3, "summary excluded, raw traits kept");
        assert_eq!(profile.unparsed_count, 0);
    }

    #[tokio::test]
    async fn cross_agent_isolation() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        store_triple(&engine, "a1", "user:u", "prefers", "tea", None, None).await;
        let profile = compile_user_profile("a2", &engine).await.unwrap();
        assert!(profile.rules.is_empty());
    }

    #[test]
    fn action_descriptor_parsing() {
        let d = ActionDescriptor::parse("proactive_message@hour=23,channel=telegram");
        assert_eq!(d.action, "proactive_message");
        assert_eq!(d.params.get("hour").map(String::as_str), Some("23"));
        assert_eq!(
            d.params.get("channel").map(String::as_str),
            Some("telegram")
        );
        assert_eq!(d.hour(), Some(23));

        let bare = ActionDescriptor::parse("send_email");
        assert_eq!(bare.action, "send_email");
        assert!(bare.params.is_empty());
        assert_eq!(bare.hour(), None);
    }
}
