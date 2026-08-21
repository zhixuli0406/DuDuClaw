// WP-S5b3-H (S5b 第三波, 2026-08-21) — data model + pure parsing/layout for
// the "工作時間軸" page (`screens/timeline.rs`). Split out for the same
// file-size reason `runs_data.rs`/`goals_data.rs`/`tasks_data.rs` are split
// from their sibling UI files.
//
// ── Data source (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed) ────────────────────────────────────────────
//   `timeline.list {"from","to","agent_id"?}` (dispatch ~L6593, handler
//   `handle_timeline_list` ~L30463, `check_agent_filter!(AccessLevel::
//   Viewer)` — non-admin callers MUST pass `agent_id` or the call fails
//   closed) → `{"rows":[{"agent_id","kind","label","started_at","ended_at",
//   "status","ref_id"}], "cap","truncated","from","to"}`. `kind` is one of
//   exactly SEVEN real values — `derive_timeline_rows`/`timeline_kind_for_
//   event` (~L36483/36597) — `"task"|"delegation"|"heartbeat"|"skill"|
//   "autopilot"|"governance"|"activity"` — matching `web/src/lib/api.ts`'s
//   own `TimelineKind` union verbatim. `ended_at: null` means the row is
//   still running (bar extends to "now"); `ended_at == started_at` means an
//   instant (rendered as a dot, never a zero-width bar).
//
// ── Honest deviation from the canvas's own color story ───────────────────
// `Timeline.dc.html` (B10) already colors every block purely by KIND, not by
// the task's own sub-status (every task block in the mockup is the same
// blue, `#2171cc`, regardless of whether it's `done`/`blocked`/etc — verified
// by reading the mockup's own inline styles, not assumed) — its 7-swatch
// legend is literally the 7 `TimelineKind` values. This module matches that
// simpler scheme rather than the web SVG page's separate per-task-status
// palette (`web/src/pages/TimelinePage.tsx`'s `fillFor` — a DIFFERENT, more
// granular scheme this native page intentionally does not replicate, since
// the approved canvas itself doesn't). Colors are theme tokens only — no hex
// — substituted for the mockup's own invented purple/teal hues (this theme
// has no purple/teal token): task→BRAND, delegation→INFO, heartbeat→CHART_3,
// skill→WARNING, autopilot→SUCCESS, governance→DESTRUCTIVE, activity→
// MUTED_FOREGROUND.

use std::collections::HashMap;

use serde_json::Value;

use crate::theme;

// ── `timeline.list` response ──────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineRow {
    pub agent_id: String,
    pub kind: String,
    pub label: String,
    /// RFC3339.
    pub started_at: String,
    /// `None` = still running (bar extends to "now"); `Some(s) == started_at`
    /// = instant (rendered as a dot). Never fabricated when absent on the
    /// wire.
    pub ended_at: Option<String>,
    pub status: String,
    pub ref_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimelineListResult {
    pub rows: Vec<TimelineRow>,
    pub cap: u64,
    pub truncated: bool,
    pub from: String,
    pub to: String,
}

pub fn parse_timeline_list(v: &Value) -> TimelineListResult {
    let rows = v
        .get("rows")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    let agent_id = r.get("agent_id")?.as_str()?.to_string();
                    let kind = r.get("kind").and_then(Value::as_str).unwrap_or("activity").to_string();
                    let label = r.get("label").and_then(Value::as_str).unwrap_or("").to_string();
                    let started_at = r.get("started_at")?.as_str()?.to_string();
                    let ended_at = r.get("ended_at").and_then(Value::as_str).map(str::to_string);
                    let status = r.get("status").and_then(Value::as_str).unwrap_or("").to_string();
                    let ref_id = r.get("ref_id").and_then(Value::as_str).unwrap_or("").to_string();
                    Some(TimelineRow { agent_id, kind, label, started_at, ended_at, status, ref_id })
                })
                .collect()
        })
        .unwrap_or_default();
    TimelineListResult {
        rows,
        cap: v.get("cap").and_then(Value::as_u64).unwrap_or(0),
        truncated: v.get("truncated").and_then(Value::as_bool).unwrap_or(false),
        from: v.get("from").and_then(Value::as_str).unwrap_or("").to_string(),
        to: v.get("to").and_then(Value::as_str).unwrap_or("").to_string(),
    }
}

fn parse_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|dt| dt.timestamp_millis())
}

// ── Time range (client-computed `from`/`to`, matching `web/src/pages/
// TimelinePage.tsx`'s own `RANGE_HOURS` table — `timeline.list` has no
// server-side range enum, only raw RFC3339 bounds) ──────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeRange {
    OneHour,
    SixHours,
    TwentyFourHours,
    SevenDays,
}

impl TimeRange {
    pub const ALL: [TimeRange; 4] =
        [TimeRange::OneHour, TimeRange::SixHours, TimeRange::TwentyFourHours, TimeRange::SevenDays];

    pub fn hours(self) -> i64 {
        match self {
            TimeRange::OneHour => 1,
            TimeRange::SixHours => 6,
            TimeRange::TwentyFourHours => 24,
            TimeRange::SevenDays => 24 * 7,
        }
    }

    /// i18n key suffix (`timeline.range.<suffix>`) — also doubles as the
    /// fetch-key discriminator string (`timeline.rs::maybe_fetch`).
    pub fn key(self) -> &'static str {
        match self {
            TimeRange::OneHour => "1h",
            TimeRange::SixHours => "6h",
            TimeRange::TwentyFourHours => "24h",
            TimeRange::SevenDays => "7d",
        }
    }
}

// ── Kind → color / legend ──────────────────────────────────────────────

/// The 7 real `TimelineKind` values, in the canvas's own legend order.
pub const LEGEND_KINDS: [&str; 7] =
    ["task", "delegation", "heartbeat", "skill", "autopilot", "governance", "activity"];

/// See this module's header comment for the full rationale (canvas colors
/// purely by kind; theme tokens substitute for the mockup's invented hues).
pub fn kind_color(kind: &str) -> u32 {
    match kind {
        "task" => theme::BRAND,
        "delegation" => theme::INFO,
        "heartbeat" => theme::CHART_3,
        "skill" => theme::WARNING,
        "autopilot" => theme::SUCCESS,
        "governance" => theme::DESTRUCTIVE,
        _ => theme::MUTED_FOREGROUND, // "activity" and any future kind
    }
}

// ── Lane packing (port of `web/src/lib/timeline-layout.ts`'s `buildLanes`,
// simplified to this page's own owned-data shape — canvas paint closures in
// this crate capture `move`, so lanes own their rows rather than borrowing,
// same choice `spike_t7_panzoom.rs`'s node scatter already makes) ────────

#[derive(Debug, Clone, PartialEq)]
pub struct PlacedRow {
    pub row: TimelineRow,
    pub sub_row: usize,
    pub start_ms: i64,
    pub end_ms: i64,
    pub instant: bool,
    pub running: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Lane {
    pub agent_id: String,
    pub rows: Vec<PlacedRow>,
    pub sub_row_count: usize,
}

/// Groups rows into one lane per agent (ordered by `agent_order`, unknown
/// agents appended alphabetically after), clips every row to `[from_ms,
/// to_ms]`, and greedily packs overlapping rows into sub-rows within a lane
/// — the same "first sub-row whose last block already ended" algorithm
/// `buildLanes` uses, so coincident work never silently overlaps on screen.
pub fn build_lanes(rows: &[TimelineRow], agent_order: &[String], from_ms: i64, to_ms: i64, now_ms: i64) -> Vec<Lane> {
    let min_pack_ms = (((to_ms - from_ms) as f64) * 0.008) as i64;
    let mut by_agent: HashMap<String, Vec<PlacedRow>> = HashMap::new();
    for row in rows {
        let Some(raw_start) = parse_ms(&row.started_at) else { continue };
        let (raw_end, instant, running) = match &row.ended_at {
            None => (now_ms, false, true),
            Some(e) => match parse_ms(e) {
                Some(em) if em > raw_start => (em, false, false),
                _ => (raw_start, true, false),
            },
        };
        if raw_end < from_ms || raw_start > to_ms {
            continue; // entirely outside the window
        }
        let start_ms = raw_start.clamp(from_ms, to_ms);
        let end_ms = raw_end.clamp(from_ms, to_ms).max(start_ms);
        by_agent.entry(row.agent_id.clone()).or_default().push(PlacedRow {
            row: row.clone(),
            sub_row: 0,
            start_ms,
            end_ms,
            instant,
            running,
        });
    }

    let mut ordered_ids: Vec<String> = agent_order.iter().filter(|id| by_agent.contains_key(*id)).cloned().collect();
    let mut remaining: Vec<String> = by_agent.keys().filter(|k| !ordered_ids.contains(k)).cloned().collect();
    remaining.sort();
    ordered_ids.extend(remaining);

    ordered_ids
        .into_iter()
        .filter_map(|id| {
            let mut placed = by_agent.remove(&id)?;
            placed.sort_by_key(|p| p.start_ms);
            let mut sub_row_ends: Vec<i64> = Vec::new();
            for p in placed.iter_mut() {
                let pack_end = p.end_ms.max(p.start_ms + min_pack_ms);
                match sub_row_ends.iter().position(|&end| end <= p.start_ms) {
                    Some(i) => {
                        p.sub_row = i;
                        sub_row_ends[i] = pack_end;
                    }
                    None => {
                        p.sub_row = sub_row_ends.len();
                        sub_row_ends.push(pack_end);
                    }
                }
            }
            Some(Lane { agent_id: id, sub_row_count: sub_row_ends.len().max(1), rows: placed })
        })
        .collect()
}

/// Kind → fraction-of-total-weight breakdown (the "這段時間做了什麼類別最多"
/// aggregate bar) — weight is wall-clock duration, dots/short bars counted
/// at their packed minimum width so an instant still contributes a sliver
/// rather than nothing. Returns `(kind, fraction)` pairs in `LEGEND_KINDS`
/// order, omitting kinds with zero weight; empty input ⇒ empty output (never
/// divides by zero).
pub fn kind_breakdown(lanes: &[Lane]) -> Vec<(&'static str, f32)> {
    let mut weights: HashMap<&'static str, i64> = HashMap::new();
    let mut total: i64 = 0;
    for lane in lanes {
        for p in &lane.rows {
            let w = (p.end_ms - p.start_ms).max(1);
            let key = LEGEND_KINDS.iter().find(|k| **k == p.row.kind).copied().unwrap_or("activity");
            *weights.entry(key).or_insert(0) += w;
            total += w;
        }
    }
    if total <= 0 {
        return Vec::new();
    }
    LEGEND_KINDS
        .iter()
        .filter_map(|k| weights.get(k).map(|w| (*k, *w as f32 / total as f32)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(agent: &str, kind: &str, start: &str, end: Option<&str>) -> TimelineRow {
        TimelineRow {
            agent_id: agent.into(),
            kind: kind.into(),
            label: "x".into(),
            started_at: start.into(),
            ended_at: end.map(str::to_string),
            status: "todo".into(),
            ref_id: format!("{agent}-{start}"),
        }
    }

    #[test]
    fn parse_timeline_list_reads_every_field() {
        let v = json!({
            "rows": [{"agent_id":"a1","kind":"task","label":"hi","started_at":"2026-08-21T08:00:00Z",
                       "ended_at":"2026-08-21T08:30:00Z","status":"done","ref_id":"t1"}],
            "cap": 2000, "truncated": false,
            "from": "2026-08-20T00:00:00Z", "to": "2026-08-21T00:00:00Z",
        });
        let r = parse_timeline_list(&v);
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.rows[0].kind, "task");
        assert_eq!(r.cap, 2000);
    }

    #[test]
    fn parse_timeline_list_missing_array_is_empty() {
        assert!(parse_timeline_list(&json!({})).rows.is_empty());
    }

    #[test]
    fn build_lanes_packs_non_overlapping_rows_into_one_sub_row() {
        let rows = vec![
            row("a1", "task", "2026-08-21T08:00:00Z", Some("2026-08-21T08:10:00Z")),
            row("a1", "task", "2026-08-21T08:20:00Z", Some("2026-08-21T08:30:00Z")),
        ];
        let from = parse_ms("2026-08-21T00:00:00Z").unwrap();
        let to = parse_ms("2026-08-21T23:59:59Z").unwrap();
        let lanes = build_lanes(&rows, &[], from, to, to);
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].sub_row_count, 1);
    }

    #[test]
    fn build_lanes_packs_overlapping_rows_into_separate_sub_rows() {
        let rows = vec![
            row("a1", "task", "2026-08-21T08:00:00Z", Some("2026-08-21T08:30:00Z")),
            row("a1", "delegation", "2026-08-21T08:10:00Z", Some("2026-08-21T08:20:00Z")),
        ];
        let from = parse_ms("2026-08-21T00:00:00Z").unwrap();
        let to = parse_ms("2026-08-21T23:59:59Z").unwrap();
        let lanes = build_lanes(&rows, &[], from, to, to);
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].sub_row_count, 2);
    }

    #[test]
    fn build_lanes_running_row_extends_to_now() {
        let rows = vec![row("a1", "task", "2026-08-21T08:00:00Z", None)];
        let from = parse_ms("2026-08-21T00:00:00Z").unwrap();
        let to = parse_ms("2026-08-21T23:59:59Z").unwrap();
        let now = parse_ms("2026-08-21T10:00:00Z").unwrap();
        let lanes = build_lanes(&rows, &[], from, to, now);
        assert!(lanes[0].rows[0].running);
        assert_eq!(lanes[0].rows[0].end_ms, now);
    }

    #[test]
    fn build_lanes_orders_agents_by_agent_order_then_alphabetically() {
        let rows = vec![
            row("zed", "task", "2026-08-21T08:00:00Z", Some("2026-08-21T08:10:00Z")),
            row("amy", "task", "2026-08-21T08:00:00Z", Some("2026-08-21T08:10:00Z")),
            row("bob", "task", "2026-08-21T08:00:00Z", Some("2026-08-21T08:10:00Z")),
        ];
        let from = parse_ms("2026-08-21T00:00:00Z").unwrap();
        let to = parse_ms("2026-08-21T23:59:59Z").unwrap();
        let lanes = build_lanes(&rows, &["zed".to_string()], from, to, to);
        let ids: Vec<&str> = lanes.iter().map(|l| l.agent_id.as_str()).collect();
        assert_eq!(ids, vec!["zed", "amy", "bob"]);
    }

    #[test]
    fn kind_breakdown_sums_to_one_and_omits_zero_weight_kinds() {
        let rows = vec![
            row("a1", "task", "2026-08-21T08:00:00Z", Some("2026-08-21T08:30:00Z")),
            row("a1", "skill", "2026-08-21T09:00:00Z", Some("2026-08-21T09:10:00Z")),
        ];
        let from = parse_ms("2026-08-21T00:00:00Z").unwrap();
        let to = parse_ms("2026-08-21T23:59:59Z").unwrap();
        let lanes = build_lanes(&rows, &[], from, to, to);
        let breakdown = kind_breakdown(&lanes);
        let sum: f32 = breakdown.iter().map(|(_, f)| f).sum();
        assert!((sum - 1.0).abs() < 0.001);
        assert!(breakdown.iter().any(|(k, _)| *k == "task"));
        assert!(!breakdown.iter().any(|(k, _)| *k == "heartbeat"));
    }

    #[test]
    fn kind_breakdown_empty_input_is_empty() {
        assert!(kind_breakdown(&[]).is_empty());
    }
}
