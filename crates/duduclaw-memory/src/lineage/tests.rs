//! Lineage, source fence and trigger tests (P2-B).

use super::test_support::*;
use super::*;
use crate::engine::{SqliteMemoryEngine, TemporalMeta};
use crate::supersession_guard::TemporalWriteOutcome;
use chrono::{Duration, FixedOffset, TimeZone};
use duduclaw_core::traits::MemoryEngine;
use duduclaw_core::types::MemoryLayer;

const S: &str = "telegram:c1";

// ── types ──────────────────────────────────────────────────────────────────

#[test]
fn format_ts_is_utc_micros_z_whatever_the_input_offset() {
    let taipei = FixedOffset::east_opt(8 * 3600).unwrap();
    let local = taipei.with_ymd_and_hms(2026, 10, 4, 22, 0, 0).unwrap();
    let utc = local.with_timezone(&Utc);
    assert_eq!(format_ts(utc), "2026-10-04T14:00:00.000000Z");
    // Sub-second precision is fixed width, so string order == time order.
    let a = format_ts(t0() + Duration::microseconds(5));
    let b = format_ts(t0() + Duration::milliseconds(1));
    let c = format_ts(t0() + Duration::seconds(1));
    assert!(a < b && b < c, "{a} {b} {c}");
}

#[test]
fn digest_never_contains_the_raw_keys() {
    let d = source_digest("agent", "telegram:secret-chat", "m:1", "");
    assert_eq!(d.len(), 32);
    assert!(!d.contains("secret"));
    assert_ne!(d, source_digest("agent", "telegram:secret-chat", "m:2", ""));
    assert_ne!(d, source_digest("other", "telegram:secret-chat", "m:1", ""));
}

#[test]
fn malformed_sources_are_rejected() {
    assert!(msg(S, 1).validate().is_ok());
    let mut bad = msg(S, 1);
    bad.session = " ".into();
    assert!(bad.validate().is_err());
    let mut bad = msg(S, 1);
    bad.seq = None;
    assert!(bad.validate().is_err(), "a channel message needs its seq");
    let mut bad = run("cron:a", "x", t0());
    bad.seq = Some(3);
    assert!(bad.validate().is_err(), "only channel messages carry a seq");
    let mut bad = msg(S, 1);
    bad.session = "system:fake".into();
    assert!(bad.validate().is_err(), "reserved prefix");
    let mut bad = msg(S, 1);
    bad.content_hash = Some("ABC".into());
    assert!(bad.validate().is_err());
    let mut bad = msg(S, 1);
    bad.message = "x".repeat(MAX_SOURCE_KEY_BYTES + 1);
    assert!(bad.validate().is_err());
}

#[tokio::test]
async fn malformed_provenance_writes_nothing() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let r = e
        .store_temporal_outcome(
            AGENT,
            entry("x", MemoryLayer::Semantic),
            TemporalMeta::default(),
            Provenance::Sources(vec![]),
        )
        .await;
    assert!(r.is_err());
    assert_eq!(count(&e, "SELECT COUNT(*) FROM memories").await, 0);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM memory_origins").await, 0);
}

// ── writes record lineage ──────────────────────────────────────────────────

#[tokio::test]
async fn every_write_path_records_lineage_in_the_same_transaction() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let m = put(
        &e,
        "likes tea",
        triple("user:u1", "likes", "tea"),
        src(msg(S, 10)),
    )
    .await;
    let k = e
        .store_fact(
            AGENT,
            "user likes tea",
            "telegram",
            "c1",
            S,
            src(msg(S, 10)),
        )
        .await
        .unwrap();
    let mut plain = entry("trait path row", MemoryLayer::Episodic);
    plain.id = "trait-row".into();
    e.store(AGENT, plain).await.unwrap();

    let rows = |store: &str, id: &str| {
        format!(
            "SELECT COUNT(*) FROM memory_origins WHERE memory_store = '{store}' AND memory_id = '{id}'"
        )
    };
    assert_eq!(count(&e, &rows("memories", &m)).await, 1);
    assert_eq!(count(&e, &rows("key_facts", &k)).await, 1);
    assert_eq!(
        count(
            &e,
            "SELECT COUNT(*) FROM memory_origins WHERE memory_id = 'trait-row'
               AND source_session = 'system:memory_engine_trait' AND role = 'direct'"
        )
        .await,
        1,
        "the trait method records itself as a system producer"
    );
}

#[tokio::test]
async fn derived_rows_flatten_all_ancestor_sources() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let m1 = put(&e, "a", TemporalMeta::default(), src(msg(S, 10))).await;
    let m2 = put(&e, "b", TemporalMeta::default(), src(msg(S, 11))).await;
    let m3 = put(
        &e,
        "c",
        TemporalMeta::default(),
        Provenance::derived(vec![m1.clone(), m2.clone()]),
    )
    .await;
    let m4 = put(
        &e,
        "d",
        TemporalMeta::default(),
        Provenance::derived(vec![m3.clone()]),
    )
    .await;
    let conn = e.conn_for_maintenance().await;
    let lineage = |id: &str| -> Vec<(String, String, Option<String>)> {
        let mut s = conn
            .prepare(
                "SELECT source_message, role, via_memory_id FROM memory_origins
                 WHERE memory_id = ?1 ORDER BY source_message",
            )
            .unwrap();
        s.query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    assert_eq!(
        lineage(&m3),
        vec![
            (
                "m:10".to_string(),
                "inherited".to_string(),
                Some(m1.clone())
            ),
            (
                "m:11".to_string(),
                "inherited".to_string(),
                Some(m2.clone())
            ),
        ]
    );
    let l4 = lineage(&m4);
    assert_eq!(l4.len(), 2);
    assert!(
        l4.iter()
            .all(|(_, role, via)| role == "inherited" && via.as_deref() == Some(m3.as_str()))
    );
}

#[tokio::test]
async fn untracked_parent_is_recorded_honestly() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    {
        let conn = e.conn_for_maintenance().await;
        conn.execute(
            "INSERT INTO memories (id, agent_id, content, timestamp) VALUES ('legacy', ?1, 'old', '2026-01-01T00:00:00Z')",
            [AGENT],
        )
        .unwrap();
    }
    let child = put(
        &e,
        "child",
        TemporalMeta::default(),
        Provenance::derived(vec!["legacy".into()]),
    )
    .await;
    assert_eq!(
        count(
            &e,
            &format!(
                "SELECT COUNT(*) FROM memory_origins WHERE memory_id = '{child}'
                   AND source_kind = 'untracked_parent' AND source_session = 'untracked'
                   AND source_message = 'legacy'"
            )
        )
        .await,
        1
    );
}

#[tokio::test]
async fn too_many_sources_is_lineage_overflow() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let ok: Vec<SourceRef> = (0..MAX_LINEAGE_SOURCES as i64).map(|i| msg(S, i)).collect();
    put(
        &e,
        "512 sources",
        TemporalMeta::default(),
        Provenance::Sources(ok),
    )
    .await;
    let too_many: Vec<SourceRef> = (0..=MAX_LINEAGE_SOURCES as i64)
        .map(|i| msg(S, i))
        .collect();
    let o = try_put(
        &e,
        "513 sources",
        TemporalMeta::default(),
        Provenance::Sources(too_many),
    )
    .await;
    assert!(is_fenced(&o, FenceReason::LineageOverflow), "{o:?}");

    // A derived row whose flattened lineage exceeds the cap is refused too.
    let a = put(
        &e,
        "half a",
        TemporalMeta::default(),
        Provenance::Sources((0..300).map(|i| msg("telegram:x", i)).collect()),
    )
    .await;
    let b = put(
        &e,
        "half b",
        TemporalMeta::default(),
        Provenance::Sources((0..213).map(|i| msg("telegram:y", i)).collect()),
    )
    .await;
    let o = try_put(
        &e,
        "513 inherited",
        TemporalMeta::default(),
        Provenance::derived(vec![a, b]),
    )
    .await;
    assert!(is_fenced(&o, FenceReason::LineageOverflow), "{o:?}");
}

#[tokio::test]
async fn reaffirm_past_the_cap_still_reaffirms_without_recording_lineage() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    // A stable fact carrying the maximum number of direct sources.
    let full: Vec<SourceRef> = (0..MAX_LINEAGE_SOURCES as i64).map(|i| msg(S, i)).collect();
    let id = put(
        &e,
        "name is Ann",
        triple("user:u1", "name", "Ann"),
        Provenance::Sources(full),
    )
    .await;
    let access_before = count(
        &e,
        &format!("SELECT access_count FROM memories WHERE id = '{id}'"),
    )
    .await;

    // A later conversation corroborates it: still reaffirmed, not refused.
    let o = try_put(
        &e,
        "name is Ann",
        triple("user:u1", "name", "Ann"),
        src(msg("telegram:c9", 1)),
    )
    .await;
    assert_eq!(o, TemporalWriteOutcome::Stored(id.clone()), "{o:?}");
    assert_eq!(
        count(
            &e,
            &format!("SELECT access_count FROM memories WHERE id = '{id}'")
        )
        .await,
        access_before + 1
    );
    // No reaffirm row was added past the cap, and the skip was counted.
    assert_eq!(
        count(
            &e,
            &format!("SELECT COUNT(*) FROM memory_origins WHERE memory_id = '{id}'")
        )
        .await,
        MAX_LINEAGE_SOURCES as i64
    );
    assert_eq!(e.reaffirm_lineage_skipped(), 1);
    assert_eq!(e.fence_refusals(), 0);
    // Only one current row for the fact.
    assert_eq!(
        count(
            &e,
            "SELECT COUNT(*) FROM memories WHERE subject = 'user:u1' AND valid_until IS NULL"
        )
        .await,
        1
    );

    // A direct write past the cap is still refused (unchanged rule).
    let too_many: Vec<SourceRef> = (0..=MAX_LINEAGE_SOURCES as i64)
        .map(|i| msg("telegram:c8", i))
        .collect();
    let o = try_put(
        &e,
        "other fact",
        TemporalMeta::default(),
        Provenance::Sources(too_many),
    )
    .await;
    assert!(is_fenced(&o, FenceReason::LineageOverflow), "{o:?}");
}

#[tokio::test]
async fn missing_parent_is_refused_d9() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let o = try_put(
        &e,
        "x",
        TemporalMeta::default(),
        Provenance::derived(vec!["ghost".into()]),
    )
    .await;
    assert!(is_fenced(&o, FenceReason::ParentMissing), "{o:?}");
    assert_eq!(e.fence_refusals(), 1);
}

// ── the fence after a forget ───────────────────────────────────────────────

#[tokio::test]
async fn forgotten_message_fences_every_crate_producer_and_later_messages_pass() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let keep = put(&e, "unrelated", TemporalMeta::default(), src(msg(S, 9))).await;
    put(
        &e,
        "from A",
        triple("user:u1", "likes", "tea"),
        src(msg(S, 10)),
    )
    .await;
    forget(&e, &by_message(S, &["m:10"])).await;

    // store_temporal (M1)
    let o = try_put(&e, "again from A", TemporalMeta::default(), src(msg(S, 10))).await;
    assert!(is_fenced(&o, FenceReason::SourceForgotten), "{o:?}");
    assert!(
        e.store_temporal(
            AGENT,
            entry("x", MemoryLayer::Semantic),
            TemporalMeta::default(),
            src(msg(S, 10))
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("source forgotten:")
    );
    // store_fact (M3)
    match e
        .store_fact_outcome(AGENT, "fact from A", "telegram", "c1", S, src(msg(S, 10)))
        .await
        .unwrap()
    {
        FactWriteOutcome::Fenced(r) => assert_eq!(r.reason, FenceReason::SourceForgotten),
        other => panic!("{other:?}"),
    }
    // hold (held claims) and profile traits (M15)
    assert!(
        e.hold_refused_claim(
            AGENT,
            entry("held", MemoryLayer::Semantic),
            triple("s", "p", "o"),
            src(msg(S, 10))
        )
        .await
        .is_err()
    );
    assert!(
        crate::user_profile::record_trait(&e, AGENT, "u1", "likes", "tea", 1.0, src(msg(S, 10)))
            .await
            .unwrap_err()
            .to_string()
            .contains("source forgotten:")
    );
    // A derived write that adds A as an extra source (M9-shaped).
    let o = try_put(
        &e,
        "derived",
        TemporalMeta::default(),
        Provenance::Derived {
            parents: vec![keep.clone()],
            extra: vec![msg(S, 10)],
        },
    )
    .await;
    assert!(is_fenced(&o, FenceReason::SourceForgotten));
    // The message after the forgotten one is new information.
    put(&e, "from m:12", TemporalMeta::default(), src(msg(S, 12))).await;
    e.store_fact(
        AGENT,
        "fact from m:12",
        "telegram",
        "c1",
        S,
        src(msg(S, 12)),
    )
    .await
    .unwrap();
    // Content of the refused writes never landed.
    assert_eq!(
        count(
            &e,
            "SELECT COUNT(*) FROM memories WHERE content LIKE '%from A%'"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &e,
            "SELECT COUNT(*) FROM key_facts WHERE fact = 'fact from A'"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn session_watermark_fences_up_to_seq_and_time_only_then_new_sources_pass() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    put(&e, "early", TemporalMeta::default(), src(msg(S, 5))).await;
    let cron = "cron:agent-p2b";
    put(
        &e,
        "run 1",
        TemporalMeta::default(),
        src(run(cron, "r1", t0())),
    )
    .await;
    forget(&e, &by_session(S, Some(5), t0() + Duration::seconds(5))).await;
    forget(&e, &by_session(cron, None, t0() + Duration::minutes(1))).await;

    // ≤ watermark: fenced (seq for channel messages, time for runs).
    assert!(is_fenced(
        &try_put(&e, "x", TemporalMeta::default(), src(msg(S, 3))).await,
        FenceReason::SourceForgotten
    ));
    assert!(is_fenced(
        &try_put(&e, "x", TemporalMeta::default(), src(msg(S, 5))).await,
        FenceReason::SourceForgotten
    ));
    assert!(is_fenced(
        &try_put(
            &e,
            "x",
            TemporalMeta::default(),
            src(run(cron, "r0", t0() + Duration::seconds(59)))
        )
        .await,
        FenceReason::SourceForgotten
    ));
    // Same instant given in another offset is the same time.
    let taipei = FixedOffset::east_opt(8 * 3600).unwrap();
    let same_instant = (t0() + Duration::minutes(1))
        .with_timezone(&taipei)
        .with_timezone(&Utc);
    assert!(is_fenced(
        &try_put(
            &e,
            "x",
            TemporalMeta::default(),
            src(run(cron, "rt", same_instant))
        )
        .await,
        FenceReason::SourceForgotten
    ));
    // After the watermark: new information.
    put(&e, "m:6", TemporalMeta::default(), src(msg(S, 6))).await;
    put(
        &e,
        "later run",
        TemporalMeta::default(),
        src(run(
            cron,
            "r2",
            t0() + Duration::minutes(1) + Duration::microseconds(1),
        )),
    )
    .await;
}

#[tokio::test]
async fn triggers_refuse_what_the_rust_check_missed() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let id = put(&e, "from A", TemporalMeta::default(), src(msg(S, 10))).await;
    forget(&e, &by_message(S, &["m:10"])).await;
    let conn = e.conn_for_maintenance().await;
    // A lineage row carrying the forgotten source.
    let err = conn
        .execute(
            "INSERT INTO memory_origins (memory_store, memory_id, agent_id, source_kind,
                 source_session, source_message, source_seq, source_observed_at, role, created_at)
             VALUES ('memories', 'new', ?1, 'channel_message', ?2, 'm:10', 10, 'x', 'direct', 'x')",
            rusqlite::params![AGENT, S],
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("duduclaw:source_forgotten"),
        "{err}"
    );
    // Re-inserting the forgotten id (an old binary's id-preserving copy).
    let err = conn
        .execute(
            "INSERT INTO memories (id, agent_id, content, timestamp) VALUES (?1, ?2, 'back', 'x')",
            rusqlite::params![id, AGENT],
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("duduclaw:memory_forgotten"),
        "{err}"
    );
}

#[tokio::test]
async fn system_provenance_cannot_be_targeted_by_a_forget() {
    let e = SqliteMemoryEngine::in_memory().unwrap();
    let r = e
        .plan_forget_source(
            AGENT,
            &by_message("system:memory_engine_trait", &[""]),
            Default::default(),
            &Default::default(),
        )
        .await;
    assert!(r.is_err());
    let r = e
        .plan_forget_source(
            AGENT,
            &by_session("system:playbook", None, t0()),
            Default::default(),
            &Default::default(),
        )
        .await;
    assert!(r.is_err());
}

#[tokio::test]
async fn reimporting_a_forgotten_file_is_fenced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("facts.jsonl");
    std::fs::write(
        &path,
        "{\"content\":\"alpha fact\"}\n{\"content\":\"beta fact\"}\n",
    )
    .unwrap();
    let e = SqliteMemoryEngine::in_memory().unwrap();
    assert_eq!(
        crate::import::import_jsonl(&e, AGENT, &path).await.unwrap(),
        2
    );
    let session = {
        let conn = e.conn_for_maintenance().await;
        conn.query_row(
            "SELECT DISTINCT source_session FROM memory_origins WHERE source_kind = 'import_item'",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap()
    };
    assert!(
        session.starts_with("import:") && !session.contains("facts"),
        "path is digested: {session}"
    );
    forget(&e, &by_session(&session, None, Utc::now())).await;
    assert_eq!(count(&e, "SELECT COUNT(*) FROM memories").await, 0);
    assert_eq!(
        crate::import::import_jsonl(&e, AGENT, &path).await.unwrap(),
        0,
        "same file, same records"
    );
    assert!(e.fence_refusals() >= 2);
}

// ── producer labelling locks (design §10 point 2, D4) ─────────────────────

fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for ent in std::fs::read_dir(dir).unwrap() {
        let p = ent.unwrap().path();
        if p.is_dir() {
            if p.file_name()
                .is_some_and(|n| n == "target" || n == "node_modules")
            {
                continue;
            }
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Every workspace crate's `src/` and `tests/` Rust files.
fn workspace_sources() -> Vec<std::path::PathBuf> {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let mut out = Vec::new();
    for c in std::fs::read_dir(crates).unwrap() {
        let c = c.unwrap().path();
        for sub in ["src", "tests"] {
            if c.join(sub).is_dir() {
                rust_files(&c.join(sub), &mut out);
            }
        }
    }
    assert!(out.len() > 50, "workspace scan found too few files");
    out
}

fn is_test_location(path: &std::path::Path, text: &str, offset: usize) -> bool {
    // Windows paths use `\`; the test-file rules below are written with `/`
    // (the CI Windows job reported every `…\tests.rs` as production code).
    let s = path.to_string_lossy().replace('\\', "/");
    if s.contains("/tests/")
        || s.ends_with("/tests.rs")
        || s.ends_with("_tests.rs")
        || path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("tests_"))
        || s.contains("/test_support")
    {
        return true;
    }
    cfg_test_spans(text)
        .iter()
        .any(|(start, end)| (*start..=*end).contains(&offset))
}

/// Byte spans of the items marked `#[cfg(test)]` (from the attribute to the
/// end of the item: its closing brace, or `;`). Only the marked item counts as
/// test code — production code after a `#[cfg(test)]` function is still
/// scanned. An out-of-line `mod x;` is a declaration with no body here.
fn cfg_test_spans(text: &str) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let mut spans = Vec::new();
    for (start, _) in text.match_indices("#[cfg(test)]") {
        let mut i = start + "#[cfg(test)]".len();
        // Skip whitespace and further attributes (`#[path = …]`, `#[tokio::test]`).
        loop {
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if b[i..].starts_with(b"#[") {
                i = skip_balanced(b, i + 1, b'[', b']');
            } else {
                break;
            }
        }
        // The item ends at the first `;` or at the brace matching the first `{`.
        let mut j = i;
        while j < b.len() && b[j] != b'{' && b[j] != b';' {
            j += 1;
        }
        let end = if j < b.len() && b[j] == b'{' {
            skip_balanced(b, j, b'{', b'}')
        } else {
            j
        };
        spans.push((start, end.min(b.len().saturating_sub(1))));
    }
    spans
}

/// From an opening delimiter at `open`, the index just past its matching
/// close, skipping string and char literals and comments.
fn skip_balanced(b: &[u8], open: usize, o: u8, c: u8) -> usize {
    let mut depth = 0usize;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 1;
            }
            b'"' => {
                // Raw strings: count the `#`s before the quote.
                let mut hashes = 0;
                while i > hashes && b[i - 1 - hashes] == b'#' {
                    hashes += 1;
                }
                let raw = hashes > 0 || (i > 0 && b[i - 1] == b'r');
                i += 1;
                while i < b.len() {
                    if !raw && b[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if b[i] == b'"' && b[i + 1..].iter().take(hashes).all(|&h| h == b'#') {
                        i += hashes;
                        break;
                    }
                    i += 1;
                }
            }
            b'\'' => {
                // A char literal ('x' or '\x'), not a lifetime.
                if b.get(i + 2) == Some(&b'\'') {
                    i += 2;
                } else if b.get(i + 1) == Some(&b'\\') {
                    if let Some(k) = b[i + 2..].iter().position(|&x| x == b'\'') {
                        i += 2 + k;
                    }
                }
            }
            x if x == o => depth += 1,
            x if x == c => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    b.len()
}

#[test]
fn scanner_treats_only_the_cfg_test_item_as_test_code() {
    let src = "fn prod_a() {}\n\
               #[cfg(test)]\n\
               fn helper() { let s = \"}\"; let c = '}'; }\n\
               fn prod_b() { Provenance::System { producer: \"unlisted\" } }\n\
               #[cfg(test)]\n\
               #[path = \"x_tests.rs\"]\n\
               mod x_tests;\n\
               fn prod_c() {}\n\
               #[cfg(test)]\n\
               mod tests { fn t() { Provenance::System { producer: \"t\" }; } }\n";
    let p = std::path::Path::new("crates/x/src/lib.rs");
    let at = |needle: &str| src.find(needle).unwrap();
    assert!(is_test_location(p, src, at("let s =")));
    assert!(!is_test_location(p, src, at("fn prod_a")));
    assert!(
        !is_test_location(p, src, at("producer: \"unlisted")),
        "after a cfg(test) fn"
    );
    assert!(
        !is_test_location(p, src, at("fn prod_c")),
        "after an out-of-line test mod"
    );
    assert!(is_test_location(p, src, at("producer: \"t\"")));
}

#[test]
fn test_only_provenance_is_not_used_in_production() {
    let mut offenders = Vec::new();
    for path in workspace_sources() {
        if path.ends_with("duduclaw-memory/src/lineage.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for (off, _) in text.match_indices("Provenance::test_only(") {
            if !is_test_location(&path, &text, off) {
                offenders.push(format!("{}@{off}", path.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "test-only provenance in production code: {offenders:?}"
    );
}

/// The `System` producers allowed in production (design §5.3). A new one
/// means a write path claims to be non-conversation content — add it here
/// only after checking that nothing a user said can reach it.
const ALLOWED_SYSTEM_PRODUCERS: &[&str] = &[
    "memory_engine_trait",
    "reflexion",
    "persona_induction",
    "task_transition",
    "task_rule_induce",
    "playbook",
];

#[test]
fn system_producers_match_the_allow_list() {
    let mut unknown = Vec::new();
    for path in workspace_sources() {
        let text = std::fs::read_to_string(&path).unwrap();
        for (off, _) in text.match_indices("Provenance::System {") {
            if is_test_location(&path, &text, off) {
                continue;
            }
            let rest = &text[off + "Provenance::System {".len()..];
            let inner = &rest[..rest.find('}').unwrap_or(rest.len())];
            let Some(p) = inner.find("producer:") else {
                // A destructuring pattern (`System { producer }`), not a value.
                continue;
            };
            let after = inner[p + "producer:".len()..].trim_start();
            let lit = after
                .strip_prefix('"')
                .and_then(|a| a.split('"').next())
                .map(str::to_string);
            match lit {
                Some(l) if ALLOWED_SYSTEM_PRODUCERS.contains(&l.as_str()) => {}
                other => unknown.push(format!("{}@{off}: {other:?}", path.display())),
            }
        }
    }
    assert!(unknown.is_empty(), "unlisted System producers: {unknown:?}");
}
