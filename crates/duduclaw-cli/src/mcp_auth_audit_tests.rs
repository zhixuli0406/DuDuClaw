use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use sha2::Digest;

use super::*;
use crate::mcp_auth::{AuthError, InactiveReason, Refusal};

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn peer(last: u8) -> Source {
    Source::Peer(IpAddr::V4(Ipv4Addr::new(203, 0, 113, last)))
}

fn nth_peer(n: u32) -> Source {
    Source::Peer(IpAddr::V4(Ipv4Addr::from(0x0A00_0000u32 + n)))
}

fn total(entries: &[Entry]) -> u64 {
    entries.iter().map(|e| e.count).sum()
}

/// The start of the current aligned UTC hour, in unix seconds.
fn aligned_hour() -> i64 {
    let now = chrono::Utc::now().timestamp();
    now - now.rem_euclid(CHAIN_WINDOW_SECS as i64)
}

/// The raw text of `name` in `home` ("" when absent).
pub(crate) fn raw(home: &Path, name: &str) -> String {
    std::fs::read_to_string(home.join(name)).unwrap_or_default()
}

/// Hash-chained `security_audit.jsonl` events of `event_type`.
pub(crate) fn chain_events(home: &Path, event_type: &str) -> Vec<Value> {
    raw(home, "security_audit.jsonl")
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event["event_type"] == event_type)
        .collect()
}

/// Lines of the detail file.
pub(crate) fn detail_lines(home: &Path) -> Vec<Value> {
    raw(home, DETAIL_FILE)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect()
}

fn sha256_hex(text: &str) -> String {
    format!("{:x}", sha2::Sha256::digest(text.as_bytes()))
}

/// No 12-character window of `secret` (nor the secret, nor its SHA-256, nor
/// the key store's digest of it) appears in `text`.
pub(crate) fn assert_nothing_of(secret: &str, text: &str) {
    assert!(!text.contains(secret), "secret leaked: {text}");
    assert!(!text.contains(&sha256_hex(secret)), "sha256 leaked: {text}");
    assert!(
        !text.contains(&duduclaw_core::mcp_key_store::digest(secret)),
        "store digest leaked: {text}"
    );
    let chars: Vec<char> = secret.chars().collect();
    for window in chars.windows(12) {
        let fragment: String = window.iter().collect();
        assert!(!text.contains(&fragment), "fragment {fragment:?} leaked: {text}");
    }
}

fn leak_recorder() -> &'static StdioRecorder {
    Box::leak(Box::new(StdioRecorder::new()))
}

/// Confirm every immediate stored-key record in `out` as enqueued.
fn confirm_all(chain: &mut ChainAggregator, out: &[ChainRecord]) {
    for record in out {
        if let Some(key) = &record.first_of {
            chain.confirm_first(key, true);
        }
    }
}

// ── Detail aggregator (injected clock) ────────────────────────────────────────

#[test]
fn first_failure_is_written_at_once_and_the_rest_once_at_rollover() {
    let t0 = Instant::now();
    let mut agg = Aggregator::new(t0);
    let mut out = Vec::new();
    agg.record(t0, FailureReason::UnknownKey, peer(7), "mcp", None, &mut out);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].aggregation, Aggregation::First);
    assert_eq!(out[0].count, 1);
    assert_eq!(out[0].source, "203.0.113.7");
    assert_eq!(out[0].route, "mcp");
    assert_eq!(out[0].transport, TRANSPORT_HTTP);
    assert_eq!(out[0].window_secs, WINDOW_SECS);
    out.clear();

    let n = 25;
    for i in 0..n {
        agg.record(t0 + Duration::from_millis(1000 + i * 1000), FailureReason::UnknownKey, peer(7), "mcp", None, &mut out);
    }
    assert!(out.is_empty(), "nothing more is written inside the window: {out:?}");
    agg.roll(t0 + secs(59), &mut out);
    assert!(out.is_empty(), "the window has not ended yet");

    agg.roll(t0 + secs(WINDOW_SECS), &mut out);
    assert_eq!(out.len(), 1, "exactly one summary at rollover: {out:?}");
    assert_eq!(out[0].aggregation, Aggregation::WindowSummary);
    assert_eq!(out[0].count, n);
    assert_eq!(out[0].reason, Some(FailureReason::UnknownKey));
    assert_eq!(out[0].source, "203.0.113.7");
    out.clear();

    // A rollover with nothing pending writes nothing; the next failure opens
    // a new window with a new immediate entry.
    agg.roll(t0 + secs(3 * WINDOW_SECS), &mut out);
    assert!(out.is_empty());
    agg.record(t0 + secs(3 * WINDOW_SECS + 1), FailureReason::UnknownKey, peer(7), "mcp", None, &mut out);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].aggregation, Aggregation::First);
}

#[test]
fn the_next_failure_after_the_window_rolls_it_over() {
    let t0 = Instant::now();
    let mut agg = Aggregator::new(t0);
    let mut out = Vec::new();
    for _ in 0..3 {
        agg.record(t0 + secs(5), FailureReason::KeyDisabled, peer(1), "mcp", Some("mk_abc"), &mut out);
    }
    agg.record(t0 + secs(7), FailureReason::MalformedKey, peer(1), "mcp_v1_call", None, &mut out);
    assert_eq!(out.len(), 2);
    out.clear();
    agg.record(t0 + secs(61), FailureReason::BadScheme, peer(2), "mcp", None, &mut out);
    assert_eq!(out.len(), 2, "{out:?}");
    assert_eq!(out[0].aggregation, Aggregation::WindowSummary);
    assert_eq!(out[0].count, 2);
    assert_eq!(out[0].key_id.as_deref(), Some("mk_abc"));
    assert_eq!(out[1].aggregation, Aggregation::First);
    assert_eq!(out[1].reason, Some(FailureReason::BadScheme));
}

#[test]
fn a_bucket_seen_on_several_routes_is_labelled_mixed() {
    let t0 = Instant::now();
    let mut agg = Aggregator::new(t0);
    let mut out = Vec::new();
    agg.record(t0, FailureReason::UnknownKey, peer(3), "mcp", None, &mut out);
    agg.record(t0, FailureReason::UnknownKey, peer(3), "mcp_v1_stream", None, &mut out);
    out.clear();
    agg.roll(t0 + secs(60), &mut out);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].route, ROUTE_MIXED);
}

#[test]
fn a_full_table_folds_new_sources_into_the_overflow_bucket() {
    let t0 = Instant::now();
    let mut agg = Aggregator::new(t0);
    let mut out = Vec::new();
    let failures = 2000u32;
    for n in 0..failures {
        agg.record(t0, FailureReason::UnknownKey, nth_peer(n), "mcp", None, &mut out);
        assert!(agg.tracked() <= MAX_TRACKED_KEYS);
    }
    assert_eq!(agg.tracked(), MAX_TRACKED_KEYS);
    let anonymous_lines = (MAX_LINES_PER_WINDOW - RESERVED_STORED_KEY_LINES) as usize;
    assert_eq!(out.len(), anonymous_lines);
    assert!(out.iter().all(|e| e.aggregation == Aggregation::First));

    let mut rolled = Vec::new();
    agg.roll(t0 + secs(60), &mut rolled);
    assert_eq!(agg.tracked(), 0, "tables are emptied at rollover");
    let overflow: Vec<&Entry> = rolled.iter().filter(|e| e.aggregation == Aggregation::Overflow).collect();
    assert_eq!(overflow.len(), 1, "one overflow line");
    let overflow = overflow[0];
    assert_eq!(overflow.source, MANY);
    assert_eq!(overflow.route, MANY);
    assert_eq!(overflow.reason, Some(FailureReason::UnknownKey), "a single folded reason is kept");
    assert_eq!(overflow.reasons, vec![(FailureReason::UnknownKey, overflow.count)]);
    assert!(rolled.len() <= anonymous_lines + 1);
    // Every failure is accounted for exactly once.
    assert_eq!(total(&out) + total(&rolled), u64::from(failures));
    let details = overflow.details();
    assert_eq!(details["source"], "many");
    assert_eq!(details["reasons"]["unknown_key"], overflow.count);
    assert!(details.get("key_id").is_none());
    assert!(details.get("dropped_records").is_none());
}

#[test]
fn the_overflow_line_of_several_reasons_is_mixed() {
    let t0 = Instant::now();
    let mut agg = Aggregator::new(t0);
    let mut out = Vec::new();
    for n in 0..(MAX_TRACKED_KEYS as u32 + 10) {
        agg.record(t0, FailureReason::UnknownKey, nth_peer(n), "mcp", None, &mut out);
    }
    agg.record(t0, FailureReason::MalformedKey, nth_peer(999_999), "mcp", None, &mut out);
    out.clear();
    agg.roll(t0 + secs(60), &mut out);
    let overflow = out.iter().find(|e| e.aggregation == Aggregation::Overflow).unwrap();
    assert_eq!(overflow.reason, None);
    assert_eq!(overflow.reason_label(), REASON_MIXED);
    assert_eq!(overflow.details()["reason"], "mixed");
    assert!(overflow.reasons.iter().any(|(r, _)| *r == FailureReason::MalformedKey));
}

#[test]
fn dropped_records_are_reported_on_the_next_overflow_line() {
    let t0 = Instant::now();
    let mut agg = Aggregator::new(t0);
    let mut out = Vec::new();
    agg.note_dropped(3);
    agg.roll(t0 + secs(WINDOW_SECS), &mut out);
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(out[0].aggregation, Aggregation::Overflow);
    assert_eq!(out[0].count, 0);
    assert_eq!(out[0].details()["dropped_records"], 3);
    assert_eq!(out[0].reported_drops(), 3);
    out.clear();
    agg.roll(t0 + secs(2 * WINDOW_SECS), &mut out);
    assert!(out.is_empty(), "reported once");
}

#[test]
fn global_line_budget_holds_under_ten_thousand_sources_per_minute() {
    let t0 = Instant::now();
    let mut agg = Aggregator::new(t0);
    let windows = 3u64;
    let per_window = 10_000u32;
    let mut all = Vec::new();
    for w in 0..windows {
        let mut window_out = Vec::new();
        for i in 0..per_window {
            let at = t0 + secs(w * WINDOW_SECS) + Duration::from_millis(u64::from(i) * 5);
            let reason = FailureReason::ALL[(i as usize) % 5];
            agg.record(at, reason, nth_peer(w as u32 * per_window + i), "mcp", None, &mut window_out);
            assert!(agg.tracked() <= MAX_TRACKED_KEYS + MAX_TRACKED_STORED_KEYS);
        }
        let regular = window_out.iter().filter(|e| e.aggregation != Aggregation::Overflow).count();
        let overflow = window_out.len() - regular;
        assert!(regular <= MAX_LINES_PER_WINDOW as usize, "window {w}: {regular} lines");
        assert!(overflow <= 1, "window {w}: {overflow} overflow lines");
        all.extend(window_out);
    }
    let mut last = Vec::new();
    agg.roll(t0 + secs(windows * WINDOW_SECS), &mut last);
    assert!(last.len() <= MAX_LINES_PER_WINDOW as usize + 1);
    all.extend(last);
    assert_eq!(total(&all), windows * u64::from(per_window), "no failure lost or double-counted");
    assert!(all.len() as u64 <= (windows + 1) * (u64::from(MAX_LINES_PER_WINDOW) + 1));
}

#[test]
fn stored_keys_keep_reserved_room_during_a_flood() {
    let t0 = Instant::now();
    let mut agg = Aggregator::new(t0);
    let mut out = Vec::new();
    for n in 0..(MAX_TRACKED_KEYS as u32 * 3) {
        agg.record(t0, FailureReason::UnknownKey, nth_peer(n), "mcp", None, &mut out);
    }
    out.clear();
    agg.record(t0, FailureReason::KeyDisabled, peer(9), "mcp_v1_call", Some("mk_revoked"), &mut out);
    assert_eq!(out.len(), 1, "a revoked key still in use is written at once during a flood");
    assert_eq!(out[0].key_id.as_deref(), Some("mk_revoked"));
    agg.record(t0, FailureReason::KeyDisabled, peer(9), "mcp_v1_call", Some("mk_revoked"), &mut out);
    let mut rolled = Vec::new();
    agg.roll(t0 + secs(60), &mut rolled);
    assert_eq!(
        rolled.first().and_then(|e| e.key_id.as_deref()),
        Some("mk_revoked"),
        "stored-key summaries are written before anonymous ones"
    );
}

#[test]
fn reasons_map_from_every_auth_error() {
    use FailureReason as R;
    let cases = [
        (AuthError::MissingKey, false, R::MissingHeader),
        (AuthError::InvalidFormat, false, R::MalformedKey),
        (AuthError::UnknownKey, false, R::UnknownKey),
        (AuthError::UnknownKey, true, R::KeyDeleted),
        (AuthError::KeyExpired { days_old: 31 }, false, R::KeyExpired),
        (AuthError::KeyInactive(InactiveReason::Expired), true, R::KeyExpired),
        (AuthError::KeyInactive(InactiveReason::Disabled), true, R::KeyDisabled),
        (AuthError::StoreUnavailable, false, R::StoreUnavailable),
        (AuthError::ReloadFailed, false, R::StoreUnavailable),
        (AuthError::InvalidScope("x".into()), false, R::UnknownKey),
    ];
    for (error, stored, expected) in cases {
        assert_eq!(FailureReason::from_auth_error(&error, stored), expected, "{error:?}");
    }
    let labels: std::collections::HashSet<&str> = FailureReason::ALL.iter().map(|r| r.as_str()).collect();
    assert_eq!(labels.len(), FailureReason::ALL.len());
    for (index, reason) in FailureReason::ALL.iter().enumerate() {
        assert_eq!(reason.index(), index);
    }
}

#[test]
fn route_labels_are_fixed_and_never_echo_the_path() {
    assert_eq!(route_class("/mcp"), "mcp");
    assert_eq!(route_class("/mcp/v1/call"), "mcp_v1_call");
    assert_eq!(route_class("/mcp/v1/stream"), "mcp_v1_stream");
    assert_eq!(route_class("/mcp/v1/stream/call"), "mcp_v1_stream_call");
    assert_eq!(route_class("/ingest/transcript"), "ingest_transcript");
    assert_eq!(route_class("/oauth/decision"), "oauth_decision");
    assert_eq!(route_class("/mcp/ddc_prod_secretsecret"), "other");
    assert_eq!(route_class("/mcp/"), "other");
    assert_eq!(Source::from_peer(None).label(), "unknown");
}

// ── Chain aggregator (injected clock) ─────────────────────────────────────────

#[test]
fn the_chain_gets_at_most_one_anonymous_record_per_hour() {
    use FailureReason as R;
    let base = aligned_hour();
    let reasons = [R::MissingHeader, R::BadScheme, R::MalformedKey, R::WrongCredentialType, R::UnknownKey, R::StoreUnavailable];
    let per_hour = 10_000u32;
    let mut chain = ChainAggregator::new();
    let mut all = Vec::new();
    for hour in 0..3u32 {
        let mut hour_out = Vec::new();
        for i in 0..per_hour {
            let at = base + i64::from(hour) * 3600 + i64::from(i % 3600);
            let reason = reasons[i as usize % reasons.len()];
            chain.record(at, reason, nth_peer(hour * per_hour + i), "mcp", None, &mut hour_out);
        }
        // Inside an hour only the previous hour's one summary is written.
        assert_eq!(hour_out.len(), usize::from(hour > 0), "hour {hour}: {hour_out:?}");
        all.extend(hour_out);
    }
    let mut last = Vec::new();
    chain.roll(base + 3 * 3600, &mut last);
    all.extend(last);
    assert_eq!(all.len(), 3, "one record per hour for 30,000 anonymous failures");
    for (hour, record) in all.iter().enumerate() {
        assert_eq!(record.event_type, EVENT_MCP_AUTH_FAILED_SUMMARY);
        let details = &record.details;
        assert_eq!(details["count"], u64::from(per_hour));
        assert_eq!(details["aggregation"], "hour_summary");
        assert_eq!(details["window_secs"], CHAIN_WINDOW_SECS);
        assert_eq!(details["hour_start"], hour_start_label(hour_index(base) + hour as u64));
        assert_eq!(details["distinct_sources"], MAX_COUNTED_SOURCES);
        assert_eq!(details["distinct_sources_capped"], true);
        assert_eq!(details["dropped_records"], 0);
        assert_eq!(details["stored_key_overflow"], 0);
        assert_eq!(details["detail_log"], DETAIL_FILE);
        let by_reason: u64 = details["reasons"].as_object().unwrap().values().map(|v| v.as_u64().unwrap()).sum();
        assert_eq!(by_reason, u64::from(per_hour));
        assert_eq!(details["reasons"].as_object().unwrap().len(), reasons.len());
        let text = details.to_string();
        assert!(details.get("source").is_none());
        assert!(!text.contains("10.0."), "no source address in the chain: {text}");
    }
}

#[test]
fn a_stored_key_is_written_at_once_then_once_per_hour() {
    let base = aligned_hour();
    let mut chain = ChainAggregator::new();
    let mut out = Vec::new();
    chain.record(base + 10, FailureReason::KeyDisabled, peer(1), "mcp_v1_call", Some("mk_revoked"), &mut out);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].event_type, EVENT_MCP_AUTH_FAILED);
    let details = out[0].details.clone();
    assert_eq!(details["aggregation"], "first_in_hour");
    assert_eq!(details["count"], 1);
    assert_eq!(details["key_id"], "mk_revoked");
    assert_eq!(details["reason"], "key_disabled");
    assert_eq!(details["route"], "mcp_v1_call");
    assert_eq!(details["transport"], "http");
    assert_eq!(details["detail_log"], DETAIL_FILE);
    assert!(details.get("source").is_none(), "no source address in the chain");
    confirm_all(&mut chain, &out);
    out.clear();

    for i in 0..4 {
        chain.record(base + 20 + i, FailureReason::KeyDisabled, peer(2 + i as u8), "mcp", Some("mk_revoked"), &mut out);
    }
    assert!(out.is_empty(), "the rest of the hour is only counted: {out:?}");
    // Another reason of the same key is a pair of its own.
    chain.record(base + 30, FailureReason::KeyExpired, peer(1), "mcp", Some("mk_revoked"), &mut out);
    assert_eq!(out.len(), 1);
    confirm_all(&mut chain, &out);
    out.clear();

    chain.roll(base + 3599, &mut out);
    assert!(out.is_empty(), "the hour has not ended");
    chain.roll(base + 3600, &mut out);
    assert_eq!(out.len(), 1, "one summary for the pair with pending failures, none for the rest: {out:?}");
    assert_eq!(out[0].event_type, EVENT_MCP_AUTH_FAILED);
    assert_eq!(out[0].details["aggregation"], "hour_summary");
    assert_eq!(out[0].details["count"], 4);
    assert_eq!(out[0].details["key_id"], "mk_revoked");
    assert_eq!(out[0].details["route"], ROUTE_MIXED);
    assert_eq!(out[0].details["hour_start"], hour_start_label(hour_index(base)));
    out.clear();

    chain.record(base + 3600 + 5, FailureReason::KeyDisabled, peer(1), "mcp", Some("mk_revoked"), &mut out);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].details["aggregation"], "first_in_hour", "a new hour writes at once again");
}

#[test]
fn at_most_thirty_two_stored_key_pairs_per_hour() {
    let base = aligned_hour();
    let mut chain = ChainAggregator::new();
    let mut out = Vec::new();
    let keys = 40;
    for n in 0..keys {
        let before = out.len();
        chain.record(base, FailureReason::KeyDeleted, peer(1), "mcp", Some(&format!("mk_{n:02}")), &mut out);
        confirm_all(&mut chain, &out[before..]);
        assert!(chain.tracked() <= MAX_CHAIN_STORED_KEYS_PER_HOUR);
    }
    assert_eq!(out.len(), MAX_CHAIN_STORED_KEYS_PER_HOUR);
    let mut rolled = Vec::new();
    chain.roll(base + 3600, &mut rolled);
    assert_eq!(rolled.len(), 1, "{rolled:?}");
    let summary = &rolled[0].details;
    assert_eq!(rolled[0].event_type, EVENT_MCP_AUTH_FAILED_SUMMARY);
    let folded = (keys - MAX_CHAIN_STORED_KEYS_PER_HOUR) as u64;
    assert_eq!(summary["stored_key_overflow"], folded);
    assert_eq!(summary["count"], folded);
    assert_eq!(summary["reasons"]["key_deleted"], folded);
    assert!(out.len() + rolled.len() <= MAX_CHAIN_RECORDS_PER_HOUR);
}

#[test]
fn a_stored_key_record_not_enqueued_stays_pending() {
    let base = aligned_hour();
    let mut chain = ChainAggregator::new();
    let mut out = Vec::new();
    chain.record(base, FailureReason::KeyDisabled, peer(1), "mcp", Some("mk_a"), &mut out);
    let key = out[0].first_of.clone().expect("an immediate record names its pair");
    chain.confirm_first(&key, false);
    out.clear();
    chain.record(base + 1, FailureReason::KeyDisabled, peer(1), "mcp", Some("mk_a"), &mut out);
    assert_eq!(out.len(), 1, "the pair is not done: the next failure is written at once again");
    assert_eq!(out[0].details["aggregation"], "first_in_hour");
    confirm_all(&mut chain, &out);
    out.clear();
    chain.roll(base + 3600, &mut out);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].details["count"], 1, "the failure whose record was dropped is in the summary");
}

#[test]
fn dropped_records_are_reported_in_the_next_hour_summary() {
    let base = aligned_hour();
    let mut chain = ChainAggregator::new();
    let mut out = Vec::new();
    chain.roll(base, &mut out);
    chain.note_dropped(5);
    chain.roll(base + 3600, &mut out);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].details["dropped_records"], 5);
    assert_eq!(out[0].reported_drops, 5);
    assert_eq!(out[0].details["count"], 0);
    out.clear();
    chain.roll(base + 7200, &mut out);
    assert!(out.is_empty(), "an hour without failures or drops writes nothing");
}

// ── Detail file ───────────────────────────────────────────────────────────────

#[test]
fn the_detail_file_rotates_at_the_cap_and_keeps_one_generation() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let cap = 1000u64;
    let line = |i: usize| format!("{i:04}{}\n", "x".repeat(295));
    assert_eq!(line(0).len(), 300);
    let main = home.join(DETAIL_FILE);
    let rotated = home.join(DETAIL_ROTATED_FILE);
    let size = |path: &Path| std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    for i in 0..10 {
        append_detail_line(home, line(i).as_bytes(), cap).unwrap();
        assert!(size(&main) <= cap, "line {i}: {} bytes", size(&main));
        assert!(size(&rotated) <= cap, "line {i}: rotated {} bytes", size(&rotated));
    }
    // 0-2 | 3-5 | 6-8 | 9: the third rotation replaced the older generations.
    assert_eq!(raw(home, DETAIL_FILE), line(9));
    assert_eq!(raw(home, DETAIL_ROTATED_FILE), format!("{}{}{}", line(6), line(7), line(8)));
    assert!(!home.join(format!("{DETAIL_ROTATED_FILE}.1")).exists(), "one generation only");
}

#[cfg(unix)]
#[test]
fn the_detail_file_is_owner_only_and_never_written_through_a_symlink() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let path = home.join(DETAIL_FILE);
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    append_detail_line(home, b"{}\n", DETAIL_MAX_BYTES).unwrap();
    assert_eq!(mode(&path), 0o600);
    // A file left wider is tightened.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    append_detail_line(home, b"{}\n", DETAIL_MAX_BYTES).unwrap();
    assert_eq!(mode(&path), 0o600);

    // A symlink at the detail path is refused, its target untouched.
    let elsewhere = tempfile::tempdir().unwrap();
    let victim = elsewhere.path().join("victim");
    std::fs::write(&victim, "").unwrap();
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&victim, &path).unwrap();
    assert!(append_detail_line(home, b"{}\n", DETAIL_MAX_BYTES).is_err());
    let entry = Entry::http(FailureReason::UnknownKey, "mcp", peer(1), None, 1, Aggregation::First);
    assert!(write_detail(home, &entry).is_err());
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "", "never written through the link");

    // A hard link is refused too.
    std::fs::remove_file(&path).unwrap();
    std::fs::hard_link(&victim, &path).unwrap();
    assert!(append_detail_line(home, b"{}\n", DETAIL_MAX_BYTES).is_err());
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "");
}

// ── Real refusals: identification without anything of the secret ─────────────

pub(crate) struct StoreFixture {
    pub(crate) home: tempfile::TempDir,
    pub(crate) disabled: (String, String),
    pub(crate) expired: (String, String),
    pub(crate) deleted: (String, String),
}

pub(crate) fn store_fixture() -> StoreFixture {
    use duduclaw_auth::mcp_keys::{Expiry, KeyBinding, McpKeyStore, NewMcpKey};
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "").unwrap();
    let store = McpKeyStore::open(home.path()).unwrap();
    let now = chrono::Utc::now();
    let issue = |name: &str| {
        let created = store
            .create(
                NewMcpKey {
                    name: name.into(),
                    client_id: format!("{name}-client"),
                    binding: KeyBinding::Integration,
                    env: "prod".into(),
                    scopes: vec!["memory:read".into()],
                    expiry: Expiry::Days(30),
                    created_by: Some("admin".into()),
                },
                now,
            )
            .unwrap();
        (created.secret, created.key.id)
    };
    let disabled = issue("off");
    let expired = issue("old");
    let deleted = issue("gone");
    store.set_disabled(&disabled.1, true, "admin", now).unwrap();
    store.delete(&deleted.1, "admin", now).unwrap();
    drop(store);
    rusqlite::Connection::open(duduclaw_core::mcp_key_store::store_path(home.path()))
        .unwrap()
        .execute("UPDATE mcp_api_keys SET expires_at = '2020-01-01T00:00:00Z' WHERE id = ?1", [&expired.1])
        .unwrap();
    StoreFixture { home, disabled, expired, deleted }
}

#[test]
fn refusals_reach_both_sinks_with_the_key_id_and_nothing_of_the_secret() {
    let fx = store_fixture();
    let home = fx.home.path();
    let unknown = "ddc_prod_0f1e2d3c4b5a69788796a5b4c3d2e1f0".to_string();
    let malformed = "ddc_prod_NOT-A-KEY-Zq9Xw8Vu7Ts6Rp5".to_string();
    let cases = [
        (fx.disabled.0.clone(), Some(fx.disabled.1.clone()), FailureReason::KeyDisabled),
        (fx.expired.0.clone(), Some(fx.expired.1.clone()), FailureReason::KeyExpired),
        (fx.deleted.0.clone(), Some(fx.deleted.1.clone()), FailureReason::KeyDeleted),
        (unknown.clone(), None, FailureReason::UnknownKey),
        (malformed.clone(), None, FailureReason::MalformedKey),
    ];
    let audit = AuthFailureAudit::new(home.to_path_buf());
    for (index, (secret, key_id, reason)) in cases.iter().enumerate() {
        let refusal = crate::mcp_auth::authenticate_with_key_call_refusal(secret, home).unwrap_err();
        // The decision is the one the plain entry point makes.
        assert_eq!(
            Some(&refusal.error),
            crate::mcp_auth::authenticate_with_key_call(secret, home).err().as_ref()
        );
        assert_eq!(refusal.audit_reason(), *reason, "{secret}");
        assert_eq!(&refusal.stored_key_id, key_id);
        assert_eq!(
            crate::mcp_auth::authenticate_with_key_refusal(secret, home).unwrap_err().stored_key_id,
            *key_id
        );
        // Different sources so every case gets its own detail line; no
        // runtime here, so the writes happen inline.
        audit.record(refusal.audit_reason(), peer(index as u8), "mcp", refusal.stored_key_id.as_deref());
    }

    // Detail: one line per failure, with the socket peer.
    let detail = detail_lines(home);
    assert_eq!(detail.len(), cases.len(), "{detail:?}");
    // Chain: the stored keys at once; the rest only in the hour summary.
    let chain = chain_events(home, EVENT_MCP_AUTH_FAILED);
    assert_eq!(chain.len(), 3, "{chain:?}");
    assert!(chain_events(home, EVENT_MCP_AUTH_FAILED_SUMMARY).is_empty());
    audit.flush_at(Instant::now() + secs(WINDOW_SECS), unix_now() + CHAIN_WINDOW_SECS as i64);
    let summary = chain_events(home, EVENT_MCP_AUTH_FAILED_SUMMARY);
    assert_eq!(summary.len(), 1);
    let summary = &summary[0]["details"];
    assert_eq!(summary["count"], 2);
    assert_eq!(summary["reasons"]["unknown_key"], 1);
    assert_eq!(summary["reasons"]["malformed_key"], 1);
    assert_eq!(summary["distinct_sources"], 2);
    assert_eq!(summary["detail_log"], DETAIL_FILE);
    assert_eq!(chain_events(home, EVENT_MCP_AUTH_FAILED).len(), 3, "no per-key summary: nothing pending");

    for (index, (_, key_id, reason)) in cases.iter().enumerate() {
        let line = detail
            .iter()
            .find(|l| l["reason"] == reason.as_str())
            .unwrap_or_else(|| panic!("no detail line for {reason:?}"));
        assert_eq!(line["event_type"], EVENT_MCP_AUTH_FAILED);
        assert_eq!(line["source"], format!("203.0.113.{index}"));
        assert_eq!(line.get("key_id").and_then(Value::as_str), key_id.as_deref());
        assert_eq!(line["aggregation"], "first");
        assert_eq!(line["count"], 1);
        if let Some(key_id) = key_id {
            let event = chain
                .iter()
                .find(|e| e["details"]["key_id"] == key_id.as_str())
                .unwrap_or_else(|| panic!("no chain record for {reason:?}"));
            assert_eq!(event["severity"], "warning");
            assert_eq!(event["details"]["reason"], reason.as_str());
            assert_eq!(event["details"]["aggregation"], "first_in_hour");
            assert_eq!(event["details"]["count"], 1);
            assert!(event["details"].get("source").is_none());
        }
    }
    let raw_chain = raw(home, "security_audit.jsonl");
    let raw_detail = raw(home, DETAIL_FILE);
    assert!(!raw_chain.contains("203.0.113."), "no source address in the chain");
    for (secret, _, _) in &cases {
        assert_nothing_of(secret, &raw_chain);
        assert_nothing_of(secret, &raw_detail);
    }
}

// ── Outboxes ──────────────────────────────────────────────────────────────────

#[test]
fn an_anonymous_flood_with_a_stuck_writer_cannot_drop_a_stored_key_record() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let t0 = Instant::now();
    let base = aligned_hour();
    let audit = AuthFailureAudit::new_at(home.to_path_buf(), t0);
    // Both writers "running" and stuck: nothing drains.
    audit.chain.running.store(true, Ordering::SeqCst);
    audit.detail.running.store(true, Ordering::SeqCst);
    let per_minute = 2000u32;
    for minute in 0..10u32 {
        for i in 0..per_minute {
            let offset = u64::from(minute) * WINDOW_SECS;
            audit.record_at(
                t0 + secs(offset),
                base + offset as i64,
                FailureReason::ALL[i as usize % 5],
                nth_peer(minute * per_minute + i),
                "mcp",
                None,
            );
        }
    }
    assert_eq!(audit.detail.len(), MAX_DETAIL_OUTBOX, "the detail outbox is bounded");
    assert_eq!(audit.chain.len(), 0, "anonymous failures never take chain room");

    audit.record_at(t0 + secs(600), base + 600, FailureReason::KeyDisabled, peer(9), "mcp_v1_call", Some("mk_revoked"));
    assert_eq!(audit.chain.len(), 1, "the revoked key's record is queued during the flood");
    let queued = lock(&audit.chain.queue).front().cloned().unwrap();
    assert_eq!(queued.details["key_id"], "mk_revoked");
    assert_eq!(queued.details["aggregation"], "first_in_hour");

    audit.flush_at(t0 + secs(CHAIN_WINDOW_SECS), base + CHAIN_WINDOW_SECS as i64);
    let summary = lock(&audit.chain.queue).back().cloned().unwrap();
    assert_eq!(summary.event_type, EVENT_MCP_AUTH_FAILED_SUMMARY);
    assert_eq!(summary.details["count"], 10 * per_minute);
    let dropped = summary.details["dropped_records"].as_u64().unwrap();
    assert!(dropped > 0, "the dropped detail records are reported: {summary:?}");

    // Unstick: the records reach the files (inline, no runtime).
    audit.chain.running.store(false, Ordering::SeqCst);
    audit.detail.running.store(false, Ordering::SeqCst);
    audit.after_enqueue(0);
    let chain = chain_events(home, EVENT_MCP_AUTH_FAILED);
    assert_eq!(chain.len(), 1);
    assert_eq!(chain[0]["details"]["key_id"], "mk_revoked");
    let summaries = chain_events(home, EVENT_MCP_AUTH_FAILED_SUMMARY);
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0]["details"]["dropped_records"], dropped);
    assert_eq!(detail_lines(home).len(), MAX_DETAIL_OUTBOX);
}

#[test]
fn a_stuck_writer_cannot_grow_the_chain_outbox() {
    let home = tempfile::tempdir().unwrap();
    let audit = AuthFailureAudit::new(home.path().to_path_buf());
    audit.chain.running.store(true, Ordering::SeqCst);
    let records: Vec<ChainRecord> = (0..200)
        .map(|n| {
            let key = StoredKey { key_id: format!("mk_{n}").into(), reason: FailureReason::KeyDisabled };
            ChainRecord::stored(&key, "mcp", 1, 0, Aggregation::HourSummary)
        })
        .collect();
    let dropped = audit.enqueue_locked(&mut lock(&audit.state), records, Vec::new());
    assert_eq!(dropped, 200 - MAX_CHAIN_OUTBOX as u64);
    assert_eq!(audit.chain.len(), MAX_CHAIN_OUTBOX);
    assert!(raw(home.path(), "security_audit.jsonl").is_empty());
}

// ── stdio ─────────────────────────────────────────────────────────────────────

#[test]
fn the_stdio_failure_is_written_once_per_process() {
    let fx = store_fixture();
    let home = fx.home.path();
    let recorder = leak_recorder();
    let refusal = crate::mcp_auth::authenticate_with_key_call_refusal(&fx.disabled.0, home).unwrap_err();
    assert_eq!(record_stdio_failure_now(recorder, home, &refusal), Some(true));
    assert_eq!(record_stdio_failure_now(recorder, home, &refusal), None);
    assert_eq!(record_stdio_failure_now(recorder, home, &Refusal::from(AuthError::MissingKey)), None);
    let lines = chain_events(home, EVENT_MCP_AUTH_FAILED);
    assert_eq!(lines.len(), 1);
    let details = &lines[0]["details"];
    assert_eq!(details["transport"], "stdio");
    assert_eq!(details["route"], "stdio");
    assert_eq!(details["source"], "local");
    assert_eq!(details["reason"], "key_disabled");
    assert_eq!(details["key_id"], fx.disabled.1.as_str());
    assert_eq!(details["aggregation"], "once_per_process");
    assert_nothing_of(&fx.disabled.0, &raw(home, "security_audit.jsonl"));
    assert!(!home.join(DETAIL_FILE).exists(), "stdio writes no detail line");
}

#[test]
fn a_failed_stdio_write_is_retried_once_then_stops() {
    let refusal = Refusal::from(AuthError::MissingKey);
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let missing = home.join("no-such-home");

    // Failure, then a successful retry.
    let recorder = leak_recorder();
    assert_eq!(record_stdio_failure_now(recorder, &missing, &refusal), Some(false));
    assert!(!recorder.written(), "a failed write does not set the mark");
    assert_eq!(recorder.attempts(), 1);
    assert_eq!(record_stdio_failure_now(recorder, home, &refusal), Some(true));
    assert!(recorder.written());
    assert_eq!(record_stdio_failure_now(recorder, home, &refusal), None);
    assert_eq!(chain_events(home, EVENT_MCP_AUTH_FAILED).len(), 1);

    // Two failures, then no more attempts even though a write would work.
    let other = tempfile::tempdir().unwrap();
    let recorder = leak_recorder();
    assert_eq!(record_stdio_failure_now(recorder, &missing, &refusal), Some(false));
    assert_eq!(record_stdio_failure_now(recorder, &missing, &refusal), Some(false));
    assert_eq!(recorder.attempts(), MAX_STDIO_ATTEMPTS);
    assert_eq!(record_stdio_failure_now(recorder, other.path(), &refusal), None);
    assert!(chain_events(other.path(), EVENT_MCP_AUTH_FAILED).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stuck_stdio_write_is_waited_for_only_up_to_the_bound() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let refusal = Refusal::from(AuthError::MissingKey);
    let recorder = leak_recorder();
    // Another writer holds the security-audit lock.
    let held = duduclaw_core::fs_lock::lock_exclusive(&home.join("security_audit.jsonl")).unwrap();
    let started = std::time::Instant::now();
    assert!(!record_stdio_failure_bounded(recorder, home, &refusal, Duration::from_millis(200)).await);
    assert!(started.elapsed() < secs(5), "the wait is bounded");
    assert!(!recorder.written());
    // A failure meanwhile starts no second write.
    assert!(!record_stdio_failure_bounded(recorder, home, &refusal, Duration::from_millis(50)).await);
    assert_eq!(recorder.attempts(), 1);
    drop(held);
    // The detached write completes on its own and sets the mark.
    for _ in 0..200 {
        if recorder.written() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(recorder.written());
    assert_eq!(chain_events(home, EVENT_MCP_AUTH_FAILED).len(), 1);
}

// ── HTTP plumbing ─────────────────────────────────────────────────────────────

/// Poll until `done` holds (the writers run on blocking threads).
pub(crate) async fn wait_until(mut done: impl FnMut() -> bool) {
    for _ in 0..200 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn observe_request_counts_the_tag_and_strips_it_from_the_response() {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    let home = tempfile::tempdir().unwrap();
    let audit = AuthFailureAudit::new(home.path().to_path_buf());
    let context = RequestContext { audit: audit.clone(), source: peer(42), route: "mcp" };
    let response = observe_request(context, async {
        tag((StatusCode::UNAUTHORIZED, "nope").into_response(), FailureReason::UnknownKey, None)
    })
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(response.extensions().get::<AuthFailureTag>().is_none());
    let body = axum::body::to_bytes(response.into_body(), 1024).await.unwrap();
    assert_eq!(&body[..], b"nope", "body unchanged");
    wait_until(|| !detail_lines(home.path()).is_empty()).await;
    let lines = detail_lines(home.path());
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["source"], "203.0.113.42");
    assert_eq!(lines[0]["reason"], "unknown_key");
    assert!(raw(home.path(), "security_audit.jsonl").is_empty(), "an anonymous failure waits for the hour summary");

    // A response without the marker is not counted.
    let context = RequestContext { audit, source: peer(43), route: "mcp" };
    let _ = observe_request(context, async { StatusCode::OK.into_response() }).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(detail_lines(home.path()).len(), 1);
}

#[tokio::test]
async fn unbound_refusals_are_counted_only_inside_an_http_request() {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    assert!(!record_remote_unbound(Some("mk_outside")), "no request context, nothing recorded");
    let home = tempfile::tempdir().unwrap();
    let audit = AuthFailureAudit::new(home.path().to_path_buf());
    let context = RequestContext { audit, source: peer(50), route: "mcp_v1_call" };
    let _ = observe_request(context, async {
        assert!(record_remote_unbound(Some("mk_unbound")));
        StatusCode::OK.into_response()
    })
    .await;
    wait_until(|| {
        !chain_events(home.path(), EVENT_MCP_AUTH_FAILED).is_empty() && !detail_lines(home.path()).is_empty()
    })
    .await;
    let chain = chain_events(home.path(), EVENT_MCP_AUTH_FAILED);
    assert_eq!(chain.len(), 1);
    let details = &chain[0]["details"];
    assert_eq!(details["reason"], "key_unbound");
    assert_eq!(details["key_id"], "mk_unbound");
    assert_eq!(details["route"], "mcp_v1_call");
    let detail = detail_lines(home.path());
    assert_eq!(detail.len(), 1);
    assert_eq!(detail[0]["source"], "203.0.113.50");
    assert_eq!(detail[0]["key_id"], "mk_unbound");
}

// ── Lost summaries keep their dropped counts ──────────────────────────────────

const N_DROPPED: u64 = 10_000;

fn hours(n: i64) -> i64 {
    n * CHAIN_WINDOW_SECS as i64
}

/// The (chain, detail) dropped counters not yet reported.
fn dropped_counters(audit: &AuthFailureAudit) -> (u64, u64) {
    let state = lock(&audit.state);
    (state.chain.dropped, state.detail.dropped)
}

#[test]
fn a_chain_summary_that_cannot_be_enqueued_gives_its_drops_back() {
    let home = tempfile::tempdir().unwrap();
    let (t0, base) = (Instant::now(), aligned_hour());
    let audit = AuthFailureAudit::new_at(home.path().to_path_buf(), t0);
    audit.chain.running.store(true, Ordering::SeqCst);
    audit.detail.running.store(true, Ordering::SeqCst);
    audit.flush_at(t0, base);
    lock(&audit.state).chain.note_dropped(N_DROPPED);
    // The chain outbox is full: the hour summary is not enqueued.
    let filler = StoredKey { key_id: "mk_fill".into(), reason: FailureReason::KeyDisabled };
    for _ in 0..MAX_CHAIN_OUTBOX {
        assert!(audit.chain.push(ChainRecord::stored(&filler, "mcp", 1, 0, Aggregation::HourSummary)));
    }
    audit.flush_at(t0, base + hours(1));
    assert_eq!(audit.chain.len(), MAX_CHAIN_OUTBOX);
    assert_eq!(
        dropped_counters(&audit),
        (N_DROPPED + 1, 1),
        "the summary's own N come back to the chain only; the lost summary counts once in each sink"
    );

    lock(&audit.chain.queue).clear();
    audit.flush_at(t0, base + hours(2));
    let summary = lock(&audit.chain.queue).back().cloned().unwrap();
    assert_eq!(summary.event_type, EVENT_MCP_AUTH_FAILED_SUMMARY);
    assert_eq!(summary.details["dropped_records"], N_DROPPED + 1);
    assert_eq!(dropped_counters(&audit).0, 0);
}

#[test]
fn a_chain_summary_whose_write_fails_gives_its_drops_back() {
    let dir = tempfile::tempdir().unwrap();
    // A home that does not exist: every write fails.
    let (t0, base) = (Instant::now(), aligned_hour());
    let audit = AuthFailureAudit::new_at(dir.path().join("gone"), t0);
    audit.detail.running.store(true, Ordering::SeqCst);
    audit.flush_at(t0, base);
    lock(&audit.state).chain.note_dropped(N_DROPPED);
    // Enqueued, then written inline (no runtime): the write fails.
    audit.flush_at(t0, base + hours(1));
    assert_eq!(audit.chain.len(), 0, "the writer took the summary");
    assert_eq!(dropped_counters(&audit), (N_DROPPED + 1, 1));

    audit.chain.running.store(true, Ordering::SeqCst);
    audit.flush_at(t0, base + hours(2));
    let summary = lock(&audit.chain.queue).back().cloned().unwrap();
    assert_eq!(summary.event_type, EVENT_MCP_AUTH_FAILED_SUMMARY);
    assert_eq!(summary.details["dropped_records"], N_DROPPED + 1);
}

#[test]
fn a_detail_overflow_line_that_cannot_be_enqueued_gives_its_drops_back() {
    let home = tempfile::tempdir().unwrap();
    let (t0, base) = (Instant::now(), aligned_hour());
    let audit = AuthFailureAudit::new_at(home.path().to_path_buf(), t0);
    audit.chain.running.store(true, Ordering::SeqCst);
    audit.detail.running.store(true, Ordering::SeqCst);
    audit.flush_at(t0, base);
    lock(&audit.state).detail.note_dropped(N_DROPPED);
    for _ in 0..MAX_DETAIL_OUTBOX {
        assert!(audit.detail.push(Entry::http(FailureReason::UnknownKey, "mcp", peer(1), None, 1, Aggregation::First)));
    }
    audit.flush_at(t0 + secs(WINDOW_SECS), base);
    assert_eq!(audit.detail.len(), MAX_DETAIL_OUTBOX);
    assert_eq!(dropped_counters(&audit), (1, N_DROPPED + 1));

    lock(&audit.detail.queue).clear();
    audit.flush_at(t0 + secs(2 * WINDOW_SECS), base);
    let line = lock(&audit.detail.queue).back().cloned().unwrap();
    assert_eq!(line.aggregation, Aggregation::Overflow);
    assert_eq!(line.details()["dropped_records"], N_DROPPED + 1);
    assert_eq!(dropped_counters(&audit).1, 0);
}

#[test]
fn a_detail_overflow_line_whose_write_fails_gives_its_drops_back() {
    let dir = tempfile::tempdir().unwrap();
    let (t0, base) = (Instant::now(), aligned_hour());
    let audit = AuthFailureAudit::new_at(dir.path().join("gone"), t0);
    audit.chain.running.store(true, Ordering::SeqCst);
    audit.flush_at(t0, base);
    lock(&audit.state).detail.note_dropped(N_DROPPED);
    audit.flush_at(t0 + secs(WINDOW_SECS), base);
    assert_eq!(audit.detail.len(), 0, "the writer took the line");
    assert_eq!(dropped_counters(&audit), (1, N_DROPPED + 1));

    audit.detail.running.store(true, Ordering::SeqCst);
    audit.flush_at(t0 + secs(2 * WINDOW_SECS), base);
    let line = lock(&audit.detail.queue).back().cloned().unwrap();
    assert_eq!(line.aggregation, Aggregation::Overflow);
    assert_eq!(line.details()["dropped_records"], N_DROPPED + 1);
}

#[test]
fn written_summaries_zero_the_dropped_counters() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let (t0, base) = (Instant::now(), aligned_hour());
    let audit = AuthFailureAudit::new_at(home.to_path_buf(), t0);
    audit.flush_at(t0, base);
    {
        let mut state = lock(&audit.state);
        state.chain.note_dropped(7);
        state.detail.note_dropped(7);
    }
    audit.flush_at(t0 + secs(WINDOW_SECS), base + hours(1));
    assert_eq!(dropped_counters(&audit), (0, 0));
    let summaries = chain_events(home, EVENT_MCP_AUTH_FAILED_SUMMARY);
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0]["details"]["dropped_records"], 7);
    let lines = detail_lines(home);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["aggregation"], "overflow");
    assert_eq!(lines[0]["dropped_records"], 7);

    audit.flush_at(t0 + secs(2 * WINDOW_SECS), base + hours(2));
    assert_eq!(chain_events(home, EVENT_MCP_AUTH_FAILED_SUMMARY).len(), 1, "reported once");
    assert_eq!(detail_lines(home).len(), 1);
}

// ── Detail file: size, partial writes, lock ──────────────────────────────────

#[test]
fn a_record_larger_than_the_cap_is_refused_and_never_rotated_for() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let cap = 100u64;
    let big = format!("{}\n", "x".repeat(150));
    assert!(append_detail_line(home, big.as_bytes(), cap).is_err());
    assert!(!home.join(DETAIL_FILE).exists(), "nothing created for it");

    append_detail_line(home, b"{\"n\":1}\n", cap).unwrap();
    let before = raw(home, DETAIL_FILE);
    assert!(append_detail_line(home, big.as_bytes(), cap).is_err());
    assert_eq!(raw(home, DETAIL_FILE), before);
    assert!(!home.join(DETAIL_ROTATED_FILE).exists(), "no rotation for a record that can never fit");
}

/// Writes `budget` bytes, then fails; cuts back the real file.
struct FailingWriter {
    file: std::fs::File,
    budget: usize,
}

impl Write for FailingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.budget == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "injected write failure"));
        }
        let n = self.file.write(&buf[..buf.len().min(self.budget)])?;
        self.budget -= n;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

impl CutBack for FailingWriter {
    fn cut_back(&self, len: u64) -> std::io::Result<()> {
        self.file.cut_back(len)
    }
}

// Unix only: on Windows the cut back through an append-only handle is best
// effort (it may be refused), so the byte-identical assertion is not made.
#[cfg(unix)]
#[test]
fn a_failed_append_leaves_no_fragment_behind() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let path = home.join(DETAIL_FILE);
    append_detail_line(home, b"{\"n\":1}\n", DETAIL_MAX_BYTES).unwrap();
    let before = std::fs::read(&path).unwrap();

    let file = open_private_append(&path).unwrap();
    let len = file.metadata().unwrap().len();
    let mut failing = FailingWriter { file, budget: 5 };
    assert!(append_or_cut_back(&mut failing, len, b"{\"n\":2,\"pad\":\"xxxxxxxxxx\"}\n").is_err());
    assert_eq!(failing.budget, 0, "a fragment was written before the failure");
    drop(failing);
    assert_eq!(std::fs::read(&path).unwrap(), before, "byte-identical to before");

    append_detail_line(home, b"{\"n\":3}\n", DETAIL_MAX_BYTES).unwrap();
    let text = raw(home, DETAIL_FILE);
    assert_eq!(text.lines().count(), 2);
    for line in text.lines() {
        serde_json::from_str::<Value>(line).unwrap_or_else(|e| panic!("{line:?}: {e}"));
    }
}

#[test]
fn a_detail_lock_that_cannot_be_created_counts_as_dropped() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    // A directory where the lock sidecar goes: it cannot be opened as a file.
    std::fs::create_dir(home.join(format!("{DETAIL_FILE}.lock"))).unwrap();
    assert!(append_detail_line(home, b"{}\n", DETAIL_MAX_BYTES).is_err());

    let (t0, base) = (Instant::now(), aligned_hour());
    let audit = AuthFailureAudit::new_at(home.to_path_buf(), t0);
    audit.record_at(t0, base, FailureReason::UnknownKey, peer(1), "mcp", None);
    assert_eq!(dropped_counters(&audit), (1, 1), "the lost line is counted, nothing panicked");
    assert!(!home.join(DETAIL_FILE).exists());
}

#[test]
fn the_rotated_detail_file_holds_nothing_of_the_secrets() {
    let fx = store_fixture();
    let home = fx.home.path();
    let secrets = [
        fx.disabled.0.clone(),
        fx.expired.0.clone(),
        fx.deleted.0.clone(),
        "ddc_prod_0f1e2d3c4b5a69788796a5b4c3d2e1f0".to_string(),
    ];
    let lines: Vec<String> = secrets
        .iter()
        .enumerate()
        .map(|(index, secret)| {
            let refusal = crate::mcp_auth::authenticate_with_key_call_refusal(secret, home).unwrap_err();
            let entry = Entry::http(
                refusal.audit_reason(),
                "mcp",
                peer(index as u8),
                refusal.stored_key_id.as_deref(),
                1,
                Aggregation::First,
            );
            format!("{}\n", entry.detail_line("2026-11-14T00:00:00.000Z"))
        })
        .collect();
    for _ in 0..20 {
        for line in &lines {
            append_detail_line(home, line.as_bytes(), 2048).unwrap();
        }
    }
    let rotated = raw(home, DETAIL_ROTATED_FILE);
    assert!(!rotated.is_empty(), "the detail file rotated");
    assert!(rotated.lines().all(|line| serde_json::from_str::<Value>(line).is_ok()));
    for secret in &secrets {
        assert_nothing_of(secret, &rotated);
        assert_nothing_of(secret, &raw(home, DETAIL_FILE));
    }
}

// ── Chain: anonymous expired / unbound, clock ─────────────────────────────────

#[test]
fn expired_legacy_keys_and_unbound_keys_without_an_id_go_to_the_hour_summary() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let (t0, base) = (Instant::now(), aligned_hour());
    let audit = AuthFailureAudit::new_at(home.to_path_buf(), t0);
    // What the decision returns for a legacy `[mcp_keys]` entry and for the
    // gateway-internal key past the 30-day rule: no stored row, no key id.
    let legacy = Refusal::from(AuthError::KeyExpired { days_old: 31 });
    let internal = Refusal::from(AuthError::KeyExpired { days_old: 90 });
    for (index, refusal) in [legacy, internal].iter().enumerate() {
        assert_eq!(refusal.audit_reason(), FailureReason::KeyExpired);
        assert!(refusal.stored_key_id.is_none());
        audit.record_at(t0, base + 10, refusal.audit_reason(), peer(60 + index as u8), "mcp", None);
    }
    // `record_remote_unbound(None)`: an unbound key without a stored id.
    audit.record_at(t0, base + 20, FailureReason::KeyUnbound, peer(62), "mcp_v1_call", None);
    assert!(chain_events(home, EVENT_MCP_AUTH_FAILED).is_empty(), "no immediate chain record");
    assert_eq!(detail_lines(home).len(), 3);

    audit.flush_at(t0 + secs(WINDOW_SECS), base + hours(1));
    assert!(chain_events(home, EVENT_MCP_AUTH_FAILED).is_empty());
    let summary = chain_events(home, EVENT_MCP_AUTH_FAILED_SUMMARY);
    assert_eq!(summary.len(), 1);
    let details = &summary[0]["details"];
    assert_eq!(details["count"], 3);
    assert_eq!(details["reasons"]["key_expired"], 2);
    assert_eq!(details["reasons"]["key_unbound"], 1);
    assert_eq!(details["stored_key_overflow"], 0);
}

#[test]
fn a_backward_clock_does_not_roll_and_a_forward_jump_closes_the_hour_once() {
    let base = aligned_hour();
    let mut chain = ChainAggregator::new();
    let mut out = Vec::new();
    chain.record(base + 100, FailureReason::UnknownKey, peer(1), "mcp", None, &mut out);
    chain.record(base - hours(2), FailureReason::UnknownKey, peer(2), "mcp", None, &mut out);
    chain.roll(base - hours(5), &mut out);
    chain.roll(0, &mut out);
    chain.roll(-1, &mut out);
    assert!(out.is_empty(), "a clock that went back never rolls over: {out:?}");

    chain.roll(base + hours(1000), &mut out);
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(out[0].details["count"], 2, "the backward failure counted into the current hour");
    assert_eq!(out[0].details["hour_start"], hour_start_label(hour_index(base)));
    out.clear();
    chain.roll(base + hours(1000) + 10, &mut out);
    chain.roll(base + hours(1001), &mut out);
    chain.roll(base, &mut out);
    assert!(out.is_empty(), "the jump closed the hour exactly once: {out:?}");

    // The detail windows: the same for a large jump of the monotonic clock.
    let t0 = Instant::now();
    let mut agg = Aggregator::new(t0);
    let mut lines = Vec::new();
    agg.record(t0 + secs(5), FailureReason::UnknownKey, peer(1), "mcp", None, &mut lines);
    agg.record(t0 + secs(6), FailureReason::UnknownKey, peer(1), "mcp", None, &mut lines);
    lines.clear();
    agg.roll(t0 + secs(10 * CHAIN_WINDOW_SECS), &mut lines);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].aggregation, Aggregation::WindowSummary);
    assert_eq!(lines[0].count, 1);
    lines.clear();
    agg.roll(t0 + secs(10 * CHAIN_WINDOW_SECS + 30), &mut lines);
    agg.roll(t0 + secs(5), &mut lines);
    assert!(lines.is_empty());
}
