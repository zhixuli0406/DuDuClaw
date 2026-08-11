//! Notification action-rate telemetry (W2-4, C12 / P4-5).
//!
//! ## The claim being tested
//!
//! Google SRE: *"Alerts that are less than 50% accurate are broken."*
//! `02-ux-methodology.md` P4-5 lifts that threshold verbatim: if fewer than
//! half the notifications of a given type ever cause a person to do something,
//! that type is noise and should be downgraded or deleted.
//!
//! DuDuClaw could not previously answer the question at all. This module makes
//! it answerable: every governed push records one row, every decision that a
//! person actually settles records another, and [`aggregate`] turns the pair
//! into a per-type action rate.
//!
//! ## Store
//!
//! `<home>/notify_events.jsonl`, appended under
//! [`duduclaw_core::with_file_lock`] (project convention 3 — the file is
//! written from the gateway and from the `duduclaw mcp-server` child process,
//! and interleaved records over `PIPE_BUF` would corrupt lines). Reads are a
//! bounded tail (`TAIL_BYTES`), the same posture `channel_alerts` and
//! `foresight` take: this is telemetry, so a huge file must cost bounded I/O
//! rather than an unbounded read.
//!
//! Two record shapes, distinguished by `kind`:
//!
//! ```json
//! {"kind":"push","notify_type":"decision.approval","level":"L3","ref":"apv:abc","timestamp":"…"}
//! {"kind":"action","notify_type":"decision.approval","ref":"apv:abc","timestamp":"…"}
//! ```
//!
//! ## What "acted" means
//!
//! A push carrying a `ref` (a decision id) counts as acted when an `action`
//! row with the same `ref` exists — matched by identity, not by counting, so
//! a person who taps the same card twice does not inflate the rate above
//! 100%. Pushes with no `ref` (a plain FYI line — there is nothing to press)
//! are counted in `pushed` and can never be acted on; that is the honest
//! answer and it is exactly the signal P4-5 wants, since a stream of
//! un-actionable messages is by definition noise.
//!
//! For that reason [`NotifyTypeStat::broken`] is only ever `true` for types
//! that have at least one actionable push — flagging "FYI is 0% actionable"
//! would be a tautology, not a finding.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::notify_governance::NotifyLevel;

/// Store file, relative to the DuDuClaw home directory.
const STORE_FILE: &str = "notify_events.jsonl";

/// Bytes read from the tail per query. Comfortably covers 30 days of a chatty
/// deployment (~180 bytes/row ⇒ ~46k rows) while bounding I/O.
const TAIL_BYTES: u64 = 8 * 1024 * 1024;

/// Size at which the store is rotated to `notify_events.jsonl.1`, mirroring
/// the audit-log convention. One generation is kept; older data is beyond the
/// 30-day question this store answers.
const ROTATE_BYTES: u64 = 16 * 1024 * 1024;

/// Pushes of a type below this count are reported but never flagged
/// [`NotifyTypeStat::broken`] — three notifications is not a sample.
pub const MIN_SAMPLE: u64 = 10;

/// The SRE threshold, lifted directly (P4-5).
pub const BROKEN_RATE: f64 = 0.5;

// ── Records ──────────────────────────────────────────────────────────────

/// One row of the store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NotifyEvent {
    /// `"push"` or `"action"`.
    pub kind: String,
    /// Stats bucket. Namespaced `<family>.<what>`, e.g. `decision.approval`,
    /// `budget.breaker`, `evolution.stagnation`.
    pub notify_type: String,
    /// Level the push was sent at. Absent on `action` rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    /// Correlation id — the decision this row is about. `None` for
    /// notifications with nothing to press.
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,
    pub timestamp: String,
}

impl NotifyEvent {
    fn at(&self) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(&self.timestamp)
            .ok()
            .map(|d| d.with_timezone(&Utc))
    }
}

/// One type's 30-day scorecard.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NotifyTypeStat {
    pub notify_type: String,
    /// Distinct pushes in the window.
    pub pushed: u64,
    /// Pushes that carried something to press.
    pub actionable: u64,
    /// Actionable pushes a person actually settled.
    pub acted: u64,
    /// `acted / actionable`, or `0.0` when nothing was actionable.
    pub action_rate: f64,
    /// SRE verdict: enough actionable samples, and fewer than half acted on.
    pub broken: bool,
}

// ── Store I/O ────────────────────────────────────────────────────────────

fn store_path(home_dir: &Path) -> PathBuf {
    home_dir.join(STORE_FILE)
}

/// Append one row under the advisory lock, rotating first when oversized.
/// Best-effort: a failure is logged and swallowed — telemetry must never
/// break a notification.
fn append(home_dir: &Path, event: &NotifyEvent) {
    let path = store_path(home_dir);
    let Ok(line) = serde_json::to_string(event) else {
        return;
    };
    let result = duduclaw_core::with_file_lock(&path, || {
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() >= ROTATE_BYTES {
                let _ = std::fs::rename(&path, path.with_extension("jsonl.1"));
            }
        }
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        f.write_all(format!("{line}\n").as_bytes())
    });
    if let Err(e) = result {
        debug!(error = %e, "notify-stats: 寫入 notify_events.jsonl 失敗（僅遙測，不影響通知）");
    }
}

/// Bounded tail-read of the store. Fail-safe: unreadable ⇒ empty.
fn tail_events(home_dir: &Path) -> Vec<NotifyEvent> {
    let path = store_path(home_dir);
    let Ok(mut f) = std::fs::File::open(&path) else {
        return Vec::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(TAIL_BYTES);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buf);
    // Skip a possibly-partial first line when the read started mid-file.
    let body: &str = if start > 0 {
        match text.find('\n') {
            Some(i) => &text[i + 1..],
            None => return Vec::new(),
        }
    } else {
        &text
    };
    body.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<NotifyEvent>(l).ok())
        .collect()
}

// ── Recording ────────────────────────────────────────────────────────────

/// Record that one notification of `notify_type` went out.
///
/// Called from the governance layer's delivery paths, so every governed exit
/// is counted exactly once whether it was sent immediately or after a quiet
/// window.
pub fn record_push(home_dir: &Path, notify_type: &str, level: NotifyLevel, ref_id: Option<&str>) {
    append(
        home_dir,
        &NotifyEvent {
            kind: "push".into(),
            notify_type: notify_type.to_string(),
            level: Some(level.as_str().to_string()),
            ref_id: ref_id.map(str::to_string),
            timestamp: Utc::now().to_rfc3339(),
        },
    );
}

/// Record that a person actually settled the thing a push was about.
///
/// Called after a decision is successfully applied — the press itself is not
/// enough, since a refused press is a failed interaction, not an action.
pub fn record_action(home_dir: &Path, notify_type: &str, ref_id: &str) {
    append(
        home_dir,
        &NotifyEvent {
            kind: "action".into(),
            notify_type: notify_type.to_string(),
            level: None,
            ref_id: Some(ref_id.to_string()),
            timestamp: Utc::now().to_rfc3339(),
        },
    );
}

// ── Aggregation ──────────────────────────────────────────────────────────

/// Turn raw rows into per-type scorecards. Pure — unit-tested directly.
///
/// Rows outside `[since, now]` are ignored. An action is matched to its push
/// by `ref`, so an action whose push has aged out of the window does not
/// produce a >100% rate.
pub fn aggregate(events: &[NotifyEvent], since: DateTime<Utc>, now: DateTime<Utc>) -> Vec<NotifyTypeStat> {
    // type → set of pushed refs, count of pushes, count of un-referenced pushes
    let mut pushed: HashMap<String, u64> = HashMap::new();
    let mut pushed_refs: HashMap<String, HashSet<String>> = HashMap::new();
    let mut acted_refs: HashMap<String, HashSet<String>> = HashMap::new();

    for e in events {
        let Some(at) = e.at() else { continue };
        if at < since || at > now {
            continue;
        }
        match e.kind.as_str() {
            "push" => {
                *pushed.entry(e.notify_type.clone()).or_insert(0) += 1;
                if let Some(r) = &e.ref_id {
                    pushed_refs.entry(e.notify_type.clone()).or_default().insert(r.clone());
                }
            }
            "action" => {
                if let Some(r) = &e.ref_id {
                    acted_refs.entry(e.notify_type.clone()).or_default().insert(r.clone());
                }
            }
            _ => {}
        }
    }

    let mut out: Vec<NotifyTypeStat> = pushed
        .into_iter()
        .map(|(notify_type, count)| {
            let refs = pushed_refs.get(&notify_type).cloned().unwrap_or_default();
            let empty = HashSet::new();
            let acted_set = acted_refs.get(&notify_type).unwrap_or(&empty);
            let actionable = refs.len() as u64;
            let acted = refs.iter().filter(|r| acted_set.contains(*r)).count() as u64;
            let action_rate = if actionable == 0 {
                0.0
            } else {
                acted as f64 / actionable as f64
            };
            let broken = actionable >= MIN_SAMPLE && action_rate < BROKEN_RATE;
            NotifyTypeStat {
                notify_type,
                pushed: count,
                actionable,
                acted,
                action_rate,
                broken,
            }
        })
        .collect();
    // Deterministic order: worst-behaved first, then alphabetical.
    out.sort_by(|a, b| {
        b.broken
            .cmp(&a.broken)
            .then(b.pushed.cmp(&a.pushed))
            .then(a.notify_type.cmp(&b.notify_type))
    });
    out
}

/// The `notify.stats` RPC's data layer: last `days` days, per type.
pub fn stats(home_dir: &Path, days: i64) -> Vec<NotifyTypeStat> {
    let days = days.clamp(1, 365);
    let now = Utc::now();
    let since = now - chrono::Duration::days(days);
    aggregate(&tail_events(home_dir), since, now)
}

/// Types flagged broken by the SRE 50% rule — the short list an operator
/// should actually act on. Logged by the digest so the finding reaches
/// someone even if nobody opens the dashboard.
pub fn broken_types(home_dir: &Path, days: i64) -> Vec<String> {
    let all = stats(home_dir, days);
    let broken: Vec<String> = all
        .into_iter()
        .filter(|s| s.broken)
        .map(|s| s.notify_type)
        .collect();
    if !broken.is_empty() {
        warn!(
            ?broken,
            "notify-stats: 這些通知類別的行動率低於 50%（SRE 判準：已壞掉，應降級或移除）"
        );
    }
    broken
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn push(ty: &str, r: Option<&str>, at: DateTime<Utc>) -> NotifyEvent {
        NotifyEvent {
            kind: "push".into(),
            notify_type: ty.into(),
            level: Some("L3".into()),
            ref_id: r.map(str::to_string),
            timestamp: at.to_rfc3339(),
        }
    }

    fn action(ty: &str, r: &str, at: DateTime<Utc>) -> NotifyEvent {
        NotifyEvent {
            kind: "action".into(),
            notify_type: ty.into(),
            level: None,
            ref_id: Some(r.into()),
            timestamp: at.to_rfc3339(),
        }
    }

    fn window() -> (DateTime<Utc>, DateTime<Utc>) {
        let now = Utc::now();
        (now - chrono::Duration::days(30), now)
    }

    #[test]
    fn action_rate_is_acted_over_actionable() {
        let (since, now) = window();
        let t = now - chrono::Duration::hours(1);
        let events = vec![
            push("decision.approval", Some("a1"), t),
            push("decision.approval", Some("a2"), t),
            action("decision.approval", "a1", t),
        ];
        let stats = aggregate(&events, since, now);
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].pushed, 2);
        assert_eq!(stats[0].actionable, 2);
        assert_eq!(stats[0].acted, 1);
        assert!((stats[0].action_rate - 0.5).abs() < 1e-9);
    }

    #[test]
    fn a_double_press_cannot_push_the_rate_over_one() {
        let (since, now) = window();
        let t = now - chrono::Duration::hours(1);
        let events = vec![
            push("decision.goal", Some("g1"), t),
            action("decision.goal", "g1", t),
            action("decision.goal", "g1", t),
        ];
        let stats = aggregate(&events, since, now);
        assert_eq!(stats[0].acted, 1);
        assert!((stats[0].action_rate - 1.0).abs() < 1e-9);
    }

    #[test]
    fn an_action_whose_push_aged_out_does_not_inflate_the_rate() {
        let (since, now) = window();
        let events = vec![
            // Push is 40 days old — outside the window.
            push("decision.goal", Some("old"), now - chrono::Duration::days(40)),
            action("decision.goal", "old", now - chrono::Duration::hours(1)),
        ];
        assert!(aggregate(&events, since, now).is_empty(), "no in-window push ⇒ no row");
    }

    #[test]
    fn un_actionable_pushes_are_counted_but_never_flagged_broken() {
        let (since, now) = window();
        let t = now - chrono::Duration::hours(1);
        let events: Vec<_> = (0..50).map(|_| push("evolution.stagnation", None, t)).collect();
        let stats = aggregate(&events, since, now);
        assert_eq!(stats[0].pushed, 50);
        assert_eq!(stats[0].actionable, 0);
        assert_eq!(stats[0].acted, 0);
        assert_eq!(stats[0].action_rate, 0.0);
        assert!(
            !stats[0].broken,
            "an FYI has nothing to press; calling it broken would be a tautology"
        );
    }

    #[test]
    fn the_sre_fifty_percent_rule_flags_broken_types() {
        let (since, now) = window();
        let t = now - chrono::Duration::hours(1);
        let mut events = Vec::new();
        // 12 actionable pushes, 5 acted ⇒ 41.7% ⇒ broken.
        for i in 0..12 {
            events.push(push("decision.install", Some(&format!("i{i}")), t));
        }
        for i in 0..5 {
            events.push(action("decision.install", &format!("i{i}"), t));
        }
        let stats = aggregate(&events, since, now);
        assert!(stats[0].broken, "{stats:?}");
    }

    #[test]
    fn exactly_fifty_percent_is_not_broken() {
        let (since, now) = window();
        let t = now - chrono::Duration::hours(1);
        let mut events = Vec::new();
        for i in 0..10 {
            events.push(push("decision.kickoff", Some(&format!("k{i}")), t));
        }
        for i in 0..5 {
            events.push(action("decision.kickoff", &format!("k{i}"), t));
        }
        let stats = aggregate(&events, since, now);
        assert!(!stats[0].broken, "the rule is 'less than 50%', not 'at most'");
    }

    #[test]
    fn a_small_sample_is_never_declared_broken() {
        let (since, now) = window();
        let t = now - chrono::Duration::hours(1);
        let events = vec![
            push("decision.approval", Some("a1"), t),
            push("decision.approval", Some("a2"), t),
        ];
        let stats = aggregate(&events, since, now);
        assert_eq!(stats[0].action_rate, 0.0);
        assert!(!stats[0].broken, "2 samples is not evidence");
    }

    #[test]
    fn types_are_tracked_independently_and_ordered_worst_first() {
        let (since, now) = window();
        let t = now - chrono::Duration::hours(1);
        let mut events = Vec::new();
        for i in 0..12 {
            events.push(push("decision.install", Some(&format!("i{i}")), t));
        }
        for i in 0..12 {
            events.push(push("decision.goal", Some(&format!("g{i}")), t));
            events.push(action("decision.goal", &format!("g{i}"), t));
        }
        let stats = aggregate(&events, since, now);
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0].notify_type, "decision.install");
        assert!(stats[0].broken);
        assert!(!stats[1].broken);
    }

    #[test]
    fn events_outside_the_window_are_ignored() {
        let (since, now) = window();
        let events = vec![
            push("x", Some("a"), now - chrono::Duration::days(31)),
            push("x", Some("b"), now + chrono::Duration::hours(1)), // clock skew
        ];
        assert!(aggregate(&events, since, now).is_empty());
    }

    #[test]
    fn unknown_kinds_and_bad_timestamps_are_skipped_not_counted() {
        let (since, now) = window();
        let t = now - chrono::Duration::hours(1);
        let mut junk = push("x", Some("a"), t);
        junk.kind = "sneeze".into();
        let mut bad_ts = push("x", Some("b"), t);
        bad_ts.timestamp = "whenever".into();
        assert!(aggregate(&[junk, bad_ts], since, now).is_empty());
    }

    // ── round-trip through the real store ────────────────────────

    #[test]
    fn recorded_events_come_back_out_of_the_store() {
        let dir = tempfile::tempdir().unwrap();
        record_push(dir.path(), "decision.approval", NotifyLevel::Act, Some("apv-1"));
        record_push(dir.path(), "decision.approval", NotifyLevel::Act, Some("apv-2"));
        record_action(dir.path(), "decision.approval", "apv-1");
        record_push(dir.path(), "evolution.stagnation", NotifyLevel::Fyi, None);

        let stats = stats(dir.path(), 30);
        let approval = stats.iter().find(|s| s.notify_type == "decision.approval").unwrap();
        assert_eq!(approval.pushed, 2);
        assert_eq!(approval.acted, 1);
        assert!((approval.action_rate - 0.5).abs() < 1e-9);

        let stagnation = stats.iter().find(|s| s.notify_type == "evolution.stagnation").unwrap();
        assert_eq!(stagnation.pushed, 1);
        assert_eq!(stagnation.actionable, 0);
    }

    #[test]
    fn a_missing_store_yields_no_stats_and_never_panics() {
        let dir = tempfile::tempdir().unwrap();
        assert!(stats(dir.path(), 30).is_empty());
        assert!(broken_types(dir.path(), 30).is_empty());
    }

    #[test]
    fn corrupt_lines_are_skipped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        record_push(dir.path(), "x", NotifyLevel::Fyi, None);
        let path = store_path(dir.path());
        let mut body = std::fs::read_to_string(&path).unwrap();
        body.push_str("{ half a record\n");
        std::fs::write(&path, body).unwrap();
        record_push(dir.path(), "x", NotifyLevel::Fyi, None);
        assert_eq!(stats(dir.path(), 30)[0].pushed, 2);
    }

    #[test]
    fn the_day_window_is_clamped_to_something_sane() {
        let dir = tempfile::tempdir().unwrap();
        record_push(dir.path(), "x", NotifyLevel::Fyi, None);
        // 0 and negative clamp to 1 day (still catches a just-written row);
        // an absurd value clamps to a year rather than overflowing.
        assert_eq!(stats(dir.path(), 0).len(), 1);
        assert_eq!(stats(dir.path(), -5).len(), 1);
        assert_eq!(stats(dir.path(), 10_000).len(), 1);
    }

    #[test]
    fn level_is_persisted_on_pushes_and_absent_on_actions() {
        let dir = tempfile::tempdir().unwrap();
        record_push(dir.path(), "x", NotifyLevel::Confirm, Some("r1"));
        record_action(dir.path(), "x", "r1");
        let body = std::fs::read_to_string(store_path(dir.path())).unwrap();
        let rows: Vec<NotifyEvent> = body
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(rows[0].level.as_deref(), Some("L2"));
        assert_eq!(rows[1].level, None);
    }
}
