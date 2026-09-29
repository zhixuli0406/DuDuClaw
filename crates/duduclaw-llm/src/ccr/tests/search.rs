//! CCR tests, part 3 — moved verbatim out of `ccr.rs`.

use super::*;
use super::super::preview::tabular_outlier_rows;

#[test]
fn find_discovers_only_valid_handles_in_exact_scope_and_route() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let allowed = CcrRuntime::new_unrestricted_for_test(store.clone(), scope("a"))
        .restrict_sources([("support-mcp".into(), "search".into())]);
    let key = allowed
        .source_key_for_call(Some("support-mcp"), "search")
        .unwrap();
    let first = store
        .put(&scope("a"), &key, "first", "甲乙 UNIQUE-MIDDLE 丙")
        .unwrap();
    let second = store
        .put(&scope("a"), &key, "second", "甲乙 UNIQUE-MIDDLE 丁")
        .unwrap();
    store
        .put(&scope("b"), &key, "other-tenant", "UNIQUE-MIDDLE")
        .unwrap();
    store
        .put(&scope("a"), "untrusted", "other-source", "UNIQUE-MIDDLE")
        .unwrap();
    assert_eq!(allowed.find("UNIQUE-MIDDLE", 1).unwrap()[0].id, second.id);
    let hits = allowed.find("UNIQUE-MIDDLE", 5).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[1].id, first.id);
    assert_eq!(hits[1].byte_offset, "甲乙 ".len());
    assert!(matches!(allowed.find(" ", 5), Err(CcrError::InvalidQuery)));
    store
        .revoke_source_call(&scope("a"), &key, "second")
        .unwrap();
    assert_eq!(allowed.find("UNIQUE-MIDDLE", 5).unwrap().len(), 1);
    let removed = CcrRuntime::new_unrestricted_for_test(store, scope("a"))
        .restrict_sources([("other-mcp".into(), "search".into())]);
    assert!(removed.find("UNIQUE-MIDDLE", 5).unwrap().is_empty());
}

#[test]
fn find_query_bounds_use_utf8_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let runtime = CcrRuntime::new_unrestricted_for_test(store.clone(), scope("a"))
        .restrict_sources([("support-mcp".into(), "search".into())]);
    let key = runtime
        .source_key_for_call(Some("support-mcp"), "search")
        .unwrap();
    let query = format!("{}ab", "客".repeat(42));
    assert_eq!(query.len(), 128);
    let saved = store.put(&scope("a"), &key, "boundary", &query).unwrap();
    assert_eq!(runtime.find(&query, 5).unwrap()[0].id, saved.id);
    assert!(matches!(
        runtime.find(&format!("{query}c"), 5),
        Err(CcrError::InvalidQuery)
    ));
    assert!(runtime.find("客", 5).is_ok());
}

#[test]
fn find_ranks_exact_phrase_then_bounded_lexical_overlap() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let runtime = CcrRuntime::new_unrestricted_for_test(store.clone(), scope("a"))
        .restrict_sources([("support-mcp".into(), "search".into())]);
    let key = runtime
        .source_key_for_call(Some("support-mcp"), "search")
        .unwrap();
    let exact = store
        .put(&scope("a"), &key, "exact", "cache miss billing: code 41")
        .unwrap();
    let near = store
        .put(&scope("a"), &key, "near", "billing cache miss: code 42")
        .unwrap();
    let far = store
        .put(
            &scope("a"),
            &key,
            "far",
            &format!("cache {} billing", "filler ".repeat(80)),
        )
        .unwrap();
    store
        .put(&scope("b"), &key, "wrong-tenant", "cache billing miss")
        .unwrap();
    store
        .put(
            &scope("a"),
            "untrusted",
            "wrong-route",
            "cache billing miss",
        )
        .unwrap();
    let hits = runtime.find("cache miss billing", 5).unwrap();
    assert_eq!(
        hits.iter().map(|hit| hit.id.as_str()).collect::<Vec<_>>(),
        vec![exact.id.as_str(), near.id.as_str(), far.id.as_str()]
    );
    assert_eq!(hits[0].byte_offset, 0);
    assert_eq!(hits[1].byte_offset, "billing ".len());
    assert!(hits[0].exact_phrase);
    assert!(!hits[1].exact_phrase);
    assert_eq!(hits[1].matched_terms, 3);
    assert_eq!(
        runtime.find("cache unrelated billing", 1).unwrap()[0].id,
        near.id
    );
    assert!(runtime.find("cache absent token", 5).unwrap().is_empty());
}

#[test]
fn find_keeps_older_multi_term_hit_inside_candidate_cap() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db")).with_limits(3600, 1_100);
    let runtime = CcrRuntime::new_unrestricted_for_test(store.clone(), scope("a"))
        .restrict_sources([("support-mcp".into(), "search".into())]);
    let key = runtime
        .source_key_for_call(Some("support-mcp"), "search")
        .unwrap();
    let older = store
        .put(&scope("a"), &key, "older", "alpha related to omega")
        .unwrap();
    for index in 0..1_000 {
        store
            .put(
                &scope("a"),
                &key,
                &format!("distractor-{index}"),
                &format!("alpha alone {index}"),
            )
            .unwrap();
    }
    let hits = runtime.find("alpha omega", 5).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, older.id);
    assert_eq!(hits[0].matched_terms, 2);
    assert!(!hits[0].exact_phrase);
}

#[test]
fn find_candidate_cap_excludes_ascii_substring_false_matches() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db")).with_limits(3600, 1_100);
    let runtime = CcrRuntime::new_unrestricted_for_test(store.clone(), scope("a"))
        .restrict_sources([("support-mcp".into(), "search".into())]);
    let key = runtime
        .source_key_for_call(Some("support-mcp"), "search")
        .unwrap();
    let older = store
        .put(&scope("a"), &key, "older", "alpha related to omega")
        .unwrap();
    for index in 0..1_000 {
        store
            .put(
                &scope("a"),
                &key,
                &format!("substring-{index}"),
                &format!("alpha123 omega123 {index}"),
            )
            .unwrap();
    }
    let hits = runtime.find("alpha omega", 5).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, older.id);
}

#[test]
fn find_recovers_nonexact_han_query_and_late_selected_terms() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let runtime = CcrRuntime::new_unrestricted_for_test(store.clone(), scope("a"))
        .restrict_sources([("support-mcp".into(), "search".into())]);
    let key = runtime
        .source_key_for_call(Some("support-mcp"), "search")
        .unwrap();
    let exact = store
        .put(&scope("a"), &key, "exact-han", "客服積壓已逾期")
        .unwrap();
    let near_text = "狀態: 客服目前積壓案件，部分已經逾期。";
    let near = store.put(&scope("a"), &key, "near-han", near_text).unwrap();
    store
        .put(&scope("b"), &key, "wrong-scope", near_text)
        .unwrap();
    let hits = runtime.find("客服積壓已逾期", 5).unwrap();
    assert_eq!(
        hits.iter().map(|hit| hit.id.as_str()).collect::<Vec<_>>(),
        vec![exact.id.as_str(), near.id.as_str()]
    );
    assert!(hits[0].exact_phrase);
    assert!(!hits[1].exact_phrase);
    assert!(hits[1].matched_terms >= 3);
    assert_eq!(hits[1].byte_offset, "狀態: ".len());
    let chunk = runtime
        .retrieve(&hits[1].id, None, hits[1].byte_offset, 64)
        .unwrap();
    assert!(chunk.text.starts_with("客服"));
    assert!(runtime.find("客服離職", 5).unwrap().is_empty());
    let late = store
        .put(&scope("a"), &key, "late-han", "庚辛工單與壬癸工單相關")
        .unwrap();
    let late_hits = runtime.find("甲乙丙丁戊己庚辛壬癸", 5).unwrap();
    assert_eq!(late_hits.len(), 1);
    assert_eq!(late_hits[0].id, late.id);
    assert!(!late_hits[0].exact_phrase);
}

#[test]
fn find_skips_expired_and_corrupt_entries() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let runtime = CcrRuntime::new_unrestricted_for_test(store.clone(), scope("a"));
    let expired = store
        .put(&scope("a"), "search", "expired", "FIND-ME expired")
        .unwrap();
    let corrupt = store
        .put(&scope("a"), "search", "corrupt", "FIND-ME original")
        .unwrap();
    let conn = rusqlite::Connection::open(store.path()).unwrap();
    conn.execute(
        "UPDATE ccr_entries SET expires_at=0 WHERE id=?1",
        params![expired.id],
    )
    .unwrap();
    conn.execute(
        "UPDATE ccr_entries SET original='FIND-ME changed' WHERE id=?1",
        params![corrupt.id],
    )
    .unwrap();
    assert!(runtime.find("FIND-ME", 5).unwrap().is_empty());
}

#[test]
fn preview_keeps_rare_error_line() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = CcrRuntime::new_unrestricted_for_test(
        CcrStore::new(dir.path().join("ccr.db")),
        scope("a"),
    );
    let original = format!(
        "{}\nERROR rare failure code 791\n{}",
        "normal\n".repeat(500),
        "normal\n".repeat(500)
    );
    let preview = runtime.preview(&original, "entry-id").unwrap();
    assert!(preview.contains("ERROR rare failure code 791"));
    assert!(preview.len() * 2 < original.len());
}

#[test]
fn later_fatal_line_outranks_earlier_warnings() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = CcrRuntime::new_unrestricted_for_test(
        CcrStore::new(dir.path().join("ccr.db")),
        scope("a"),
    );
    let original = format!(
        "{}{}{}{}",
        "normal\n".repeat(300),
        (0..20)
            .map(|id| format!("WARN: routine condition {id}\n"))
            .collect::<String>(),
        "FATAL: rare failure code 791\n",
        "normal\n".repeat(300)
    );
    let preview = runtime.preview(&original, "entry-id").unwrap();
    assert!(preview.contains("FATAL: rare failure code 791"));
    let diagnostics = preview
        .split("[CCR diagnostic lines]\n")
        .nth(1)
        .unwrap()
        .split("\n[CCR:")
        .next()
        .unwrap();
    assert!(diagnostics.contains("FATAL: rare failure code 791"));
    assert_eq!(diagnostics.lines().count(), 8);
}

#[test]
fn search_preview_keeps_a_query_hit_outside_head_and_tail() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = CcrRuntime::new_unrestricted_for_test(
        CcrStore::new(dir.path().join("ccr.db")),
        scope("a"),
    );
    let original = format!(
        "{}\ncase-791 contains the exact needle\n{}",
        "ordinary result\n".repeat(400),
        "ordinary result\n".repeat(400)
    );
    let preview = runtime
        .preview_with_query(&original, "entry-id", Some("exact needle"))
        .unwrap();
    assert!(preview.contains("case-791 contains the exact needle"));
    assert!(preview.len() * 2 < original.len());
    assert!(
        !runtime
            .preview(&original, "entry-id")
            .unwrap()
            .contains("case-791")
    );
}

#[test]
fn search_preview_ranks_middle_multi_term_hit_without_exact_phrase() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = CcrRuntime::new_unrestricted_for_test(
        CcrStore::new(dir.path().join("ccr.db")),
        scope("a"),
    );
    let original = format!(
        "{}{}{}{}",
        "opening filler\n".repeat(120),
        (0..20)
            .map(|index| format!("billing cache generic-{index}\n"))
            .collect::<String>(),
        "miss cache billing RESULT-773\n",
        "closing filler\n".repeat(120),
    );
    let preview = runtime
        .preview_with_query(&original, "saved-id", Some("billing cache miss"))
        .unwrap();
    assert!(preview.contains("RESULT-773"));
    assert!(preview.len() * 2 < original.len());
}

#[test]
fn search_preview_keeps_nonexact_han_query_line() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = CcrRuntime::new_unrestricted_for_test(
        CcrStore::new(dir.path().join("ccr.db")),
        scope("a"),
    );
    let original = format!(
        "{}客服目前積壓案件，部分已經逾期；案件 RESULT-773\n{}",
        "一般紀錄\n".repeat(180),
        "一般紀錄\n".repeat(180),
    );
    let preview = runtime
        .preview_with_query(&original, "saved-id", Some("客服積壓已逾期"))
        .unwrap();
    assert!(preview.contains("RESULT-773"));
    assert!(preview.len() * 2 < original.len());
    assert!(
        !runtime
            .preview(&original, "saved-id")
            .unwrap()
            .contains("RESULT-773")
    );
}

#[test]
fn long_markdown_preview_keeps_middle_section_heading_and_lead() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = CcrRuntime::new_unrestricted_for_test(
        CcrStore::new(dir.path().join("ccr.db")),
        scope("a"),
    );
    let original = format!(
        "# Report\n{}## Root cause\n\nQueue service capacity fell during the shift.\n{}",
        "Background context.\n".repeat(250),
        "Additional details.\n".repeat(250)
    );
    let preview = runtime.preview(&original, "entry-id").unwrap();
    assert!(preview.contains("[CCR sections]"));
    assert!(preview.contains("## Root cause"));
    assert!(preview.contains("Queue service capacity fell during the shift."));
    assert!(preview.len() * 2 < original.len());
}

#[test]
fn long_markdown_preview_retains_late_relevant_section_and_document_end() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = CcrRuntime::new_unrestricted_for_test(
        CcrStore::new(dir.path().join("ccr.db")),
        scope("a"),
    );
    let original = (0..12)
        .map(|index| {
            if index == 9 {
                format!(
                    "## SLA breach mechanism\nQueue arrival exceeded capacity.\n{}",
                    "routine detail\n".repeat(90)
                )
            } else {
                format!(
                    "## Section {index}\nOrdinary section lead.\n{}",
                    "routine detail\n".repeat(90)
                )
            }
        })
        .collect::<String>();
    let preview = runtime
        .preview_with_query(&original, "entry-id", Some("SLA breach mechanism"))
        .unwrap();
    assert!(preview.contains("## SLA breach mechanism — Queue arrival exceeded capacity."));
    assert!(preview.contains("## Section 11"));
    assert!(preview.len() * 2 < original.len());
}

#[test]
fn tabular_preview_keeps_middle_numeric_outlier_with_row_number() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = CcrRuntime::new_unrestricted_for_test(
        CcrStore::new(dir.path().join("ccr.db")),
        scope("a"),
    );
    let original = std::iter::once("ticket_id,wait_minutes,status".to_string())
        .chain((0..400).map(|id| {
            format!(
                "ticket-{id:04},{},resolved",
                if id == 225 { 9000 } else { 5 }
            )
        }))
        .collect::<Vec<_>>()
        .join("\n");
    let preview = runtime.preview(&original, "entry-id").unwrap();
    assert!(preview.contains("[CCR numeric outlier rows]"));
    assert!(preview.contains("L227 ticket-0225,9000,resolved [wait_minutes]"));
    assert!(preview.len() * 2 < original.len());
    let tsv_outliers = tabular_outlier_rows(&original.replace(',', "\t"));
    assert!(
        tsv_outliers
            .iter()
            .any(|row| row.contains("ticket-0225\t9000\tresolved"))
    );
}

#[test]
fn tabular_outlier_parser_rejects_quoted_or_irregular_rows() {
    let regular = std::iter::once("ticket_id,wait_minutes,status".to_string())
        .chain((0..40).map(|id| format!("ticket-{id},5,resolved")))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(tabular_outlier_rows(&regular).is_empty());
    assert!(tabular_outlier_rows(&regular.replace("resolved", "\"resolved\"")).is_empty());
    assert!(tabular_outlier_rows(&regular.replacen(",5,resolved", ",5", 1)).is_empty());
}

#[test]
fn existing_store_gets_entry_version_column() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ccr.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE ccr_entries (
            id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, agent_id TEXT NOT NULL,
            session_id TEXT NOT NULL, source_acl TEXT NOT NULL, source_tool TEXT NOT NULL,
            source_call_id TEXT NOT NULL, content_sha256 TEXT NOT NULL, original TEXT NOT NULL,
            content_bytes INTEGER NOT NULL, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL
        );",
    )
    .unwrap();
    drop(conn);
    let store = CcrStore::new(path);
    let entry = store
        .put(&scope("a"), "search", "call-1", "content")
        .unwrap();
    assert_eq!(entry.transform_version, 1);
    assert_eq!(
        store
            .retrieve(&scope("a"), &entry.id, None, 0, 100)
            .unwrap()
            .text,
        "content"
    );
}

#[test]
fn legacy_bound_rows_are_backfilled_before_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ccr.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE ccr_entries (
                id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, agent_id TEXT NOT NULL,
                session_id TEXT NOT NULL, source_acl TEXT NOT NULL, source_tool TEXT NOT NULL,
                source_call_id TEXT NOT NULL, content_sha256 TEXT NOT NULL,
                transform_version INTEGER NOT NULL DEFAULT 1, original TEXT NOT NULL,
                content_bytes INTEGER NOT NULL, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL
            );
            CREATE TABLE ccr_artifact_bindings (
                entry_id TEXT PRIMARY KEY REFERENCES ccr_entries(id) ON DELETE CASCADE,
                tenant_id TEXT NOT NULL, connector TEXT NOT NULL, artifact_id TEXT NOT NULL,
                version TEXT NOT NULL, acl_revision TEXT NOT NULL
            );",
    ).unwrap();
    let text = "legacy source evidence";
    conn.execute(
        "INSERT INTO ccr_entries VALUES (?1,?2,?3,?4,?5,?6,?7,?8,1,?9,?10,?11,?12)",
        params![
            "legacy-id",
            "a",
            "support",
            "s1",
            "private-thread",
            "search",
            "call",
            format!("{:x}", Sha256::digest(text.as_bytes())),
            text,
            text.len() as i64,
            unix_now(),
            unix_now() + 300
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ccr_artifact_bindings VALUES ('legacy-id','a','causal','live','v1','acl-v1')",
        [],
    ).unwrap();
    drop(conn);
    let store = CcrStore::new(&path);
    assert!(matches!(
        store.retrieve(&scope("a"), "legacy-id", None, 0, 100),
        Err(CcrError::Revoked)
    ));
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.query_row::<i64, _, _>(
            "SELECT binding_required FROM ccr_entries WHERE id='legacy-id'",
            [],
            |row| row.get(0)
        )
        .unwrap(),
        1
    );
    conn.execute(
        "DELETE FROM ccr_artifact_bindings WHERE entry_id='legacy-id'",
        [],
    )
    .unwrap();
    assert!(matches!(
        store.retrieve(&scope("a"), "legacy-id", None, 0, 100),
        Err(CcrError::Revoked)
    ));
}

#[test]
fn loop_telemetry_is_numeric_tenant_scoped_and_globally_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ccr.db");
    let store = CcrStore::new(&path);
    let telemetry = crate::tool_loop::ToolLoopTelemetry {
        provider_rounds: 2,
        usage_reported_rounds: 1,
        ccr_compressed_results: 1,
        ccr_original_bytes: 4_096,
        ccr_delivered_bytes: 512,
        ccr_find_attempts: 1,
        ccr_find_misses: 1,
        elapsed_millis: 25,
        ..Default::default()
    };
    store
        .record_loop_telemetry(&scope("tenant-a"), &telemetry)
        .unwrap();
    let mut conn = Connection::open(&path).unwrap();
    let tx = conn.transaction().unwrap();
    for _ in 1..CCR_LOOP_TELEMETRY_MAX_ROWS {
        tx.execute(
            "INSERT INTO ccr_loop_telemetry (
                    tenant_id,observed_at,provider_rounds,usage_reported_rounds,
                    input_tokens,output_tokens,cache_read_tokens,cache_write_tokens,
                    reasoning_tokens,ccr_compressed_results,ccr_original_bytes,
                    ccr_delivered_bytes,ccr_find_attempts,ccr_find_hits,ccr_find_misses,
                    ccr_retrieve_attempts,ccr_retrieve_successes,ccr_retrieve_misses,
                    ccr_retrieved_bytes,elapsed_millis)
                 SELECT tenant_id,observed_at,provider_rounds,usage_reported_rounds,
                    input_tokens,output_tokens,cache_read_tokens,cache_write_tokens,
                    reasoning_tokens,ccr_compressed_results,ccr_original_bytes,
                    ccr_delivered_bytes,ccr_find_attempts,ccr_find_hits,ccr_find_misses,
                    ccr_retrieve_attempts,ccr_retrieve_successes,ccr_retrieve_misses,
                    ccr_retrieved_bytes,elapsed_millis
                 FROM ccr_loop_telemetry WHERE event_id=1",
            [],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    store
        .record_loop_telemetry(&scope("tenant-b"), &telemetry)
        .unwrap();
    let (count, first_id, b_count): (i64, i64, i64) = conn
        .query_row(
            "SELECT COUNT(*),MIN(event_id),SUM(tenant_id='tenant-b') FROM ccr_loop_telemetry",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(count, CCR_LOOP_TELEMETRY_MAX_ROWS);
    assert_eq!(first_id, 2);
    assert_eq!(b_count, 1);
    let columns: Vec<String> = conn
        .prepare("PRAGMA table_info(ccr_loop_telemetry)")
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for secret_column in [
        "original",
        "query",
        "handle",
        "source_acl",
        "agent_id",
        "session_id",
    ] {
        assert!(!columns.iter().any(|column| column == secret_column));
    }
}

/// Regression (W3-2 #3): `ccr_loop_telemetry` is created lazily, so its
/// new `ccr_find_rate_limited` column can only reach an existing file
/// through the versioned migration window. A file stamped with the OLD
/// `user_version` must re-enter that window, get the `ALTER TABLE`, keep
/// its existing rows readable, and be re-stamped current.
#[test]
fn stale_schema_stamp_migrates_the_telemetry_rate_limit_column() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ccr.db");
    // Exactly what a pre-schema-2 binary leaves behind.
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE ccr_loop_telemetry (
                    event_id INTEGER PRIMARY KEY AUTOINCREMENT,
                    tenant_id TEXT NOT NULL,
                    observed_at INTEGER NOT NULL,
                    provider_rounds INTEGER NOT NULL,
                    usage_reported_rounds INTEGER NOT NULL,
                    input_tokens INTEGER NOT NULL,
                    output_tokens INTEGER NOT NULL,
                    cache_read_tokens INTEGER NOT NULL,
                    cache_write_tokens INTEGER NOT NULL,
                    reasoning_tokens INTEGER NOT NULL,
                    ccr_compressed_results INTEGER NOT NULL,
                    ccr_original_bytes INTEGER NOT NULL,
                    ccr_delivered_bytes INTEGER NOT NULL,
                    ccr_find_attempts INTEGER NOT NULL,
                    ccr_find_hits INTEGER NOT NULL,
                    ccr_find_misses INTEGER NOT NULL,
                    ccr_retrieve_attempts INTEGER NOT NULL,
                    ccr_retrieve_successes INTEGER NOT NULL,
                    ccr_retrieve_misses INTEGER NOT NULL,
                    ccr_retrieved_bytes INTEGER NOT NULL,
                    elapsed_millis INTEGER NOT NULL
                );
                INSERT INTO ccr_loop_telemetry (
                    tenant_id,observed_at,provider_rounds,usage_reported_rounds,
                    input_tokens,output_tokens,cache_read_tokens,cache_write_tokens,
                    reasoning_tokens,ccr_compressed_results,ccr_original_bytes,
                    ccr_delivered_bytes,ccr_find_attempts,ccr_find_hits,
                    ccr_find_misses,ccr_retrieve_attempts,ccr_retrieve_successes,
                    ccr_retrieve_misses,ccr_retrieved_bytes,elapsed_millis)
                VALUES ('tenant-a',1,3,2,0,0,0,0,0,1,4096,512,1,0,1,0,0,0,0,25);
                PRAGMA user_version=1;",
        )
        .unwrap();

    let store = CcrStore::new(&path);
    store.open().unwrap();

    let conn = Connection::open(&path).unwrap();
    let stamped: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(stamped, SCHEMA_VERSION);
    let columns: Vec<String> = conn
        .prepare("PRAGMA table_info(ccr_loop_telemetry)")
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        columns.iter().any(|c| c == "ccr_find_rate_limited"),
        "{columns:?}"
    );
    // The pre-existing row survives and reads the column default.
    let (rounds, limited): (i64, i64) = conn
        .query_row(
            "SELECT provider_rounds,ccr_find_rate_limited FROM ccr_loop_telemetry
                 WHERE tenant_id='tenant-a'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(rounds, 3);
    assert_eq!(limited, 0);

    // A fresh write persists the counter instead of only logging it.
    store
        .record_loop_telemetry(
            &scope("tenant-b"),
            &crate::tool_loop::ToolLoopTelemetry {
                provider_rounds: 1,
                ccr_find_attempts: 2,
                ccr_find_misses: 2,
                ccr_find_rate_limited: 4,
                elapsed_millis: 5,
                ..Default::default()
            },
        )
        .unwrap();
    let written: i64 = conn
        .query_row(
            "SELECT ccr_find_rate_limited FROM ccr_loop_telemetry WHERE tenant_id='tenant-b'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(written, 4);
}

#[test]
fn locked_telemetry_write_uses_short_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ccr.db");
    let store = CcrStore::new(&path);
    let telemetry = crate::tool_loop::ToolLoopTelemetry {
        provider_rounds: 1,
        elapsed_millis: 1,
        ..Default::default()
    };
    store
        .record_loop_telemetry(&scope("a"), &telemetry)
        .unwrap();
    let lock = Connection::open(&path).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let started = std::time::Instant::now();
    assert!(
        store
            .record_loop_telemetry(&scope("a"), &telemetry)
            .is_err()
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    lock.execute_batch("ROLLBACK").unwrap();
}
