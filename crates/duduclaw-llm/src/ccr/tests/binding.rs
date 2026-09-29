//! CCR tests, part 2 — moved verbatim out of `ccr.rs`.

use super::*;
use super::super::find::historical_task_score;
use super::super::preview::strip_json_whitespace;

#[test]
fn scope_and_expiry_reject_reads() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let entry = store
        .put(&scope("a"), "search", "call-1", "secret")
        .unwrap();
    assert!(matches!(
        store.retrieve(&scope("b"), &entry.id, None, 0, 100),
        Err(CcrError::NotFound)
    ));
    let mut wrong_session = scope("a");
    wrong_session.session_id = "s2".into();
    assert!(matches!(
        store.retrieve(&wrong_session, &entry.id, None, 0, 100),
        Err(CcrError::NotFound)
    ));
    let conn = Connection::open(store.path()).unwrap();
    conn.execute(
        "UPDATE ccr_entries SET expires_at = 0 WHERE id = ?1",
        [&entry.id],
    )
    .unwrap();
    assert!(matches!(
        store.retrieve(&scope("a"), &entry.id, None, 0, 100),
        Err(CcrError::NotFound)
    ));
}

#[test]
fn retrieval_audit_records_grants_and_refusals_without_content() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let entry = store
        .put(&scope("a"), "search", "call-1", "secret 甲乙")
        .unwrap();
    assert_eq!(
        store
            .retrieve(&scope("a"), &entry.id, None, 0, 6)
            .unwrap()
            .text,
        "secret"
    );
    assert!(matches!(
        store.retrieve(&scope("b"), &entry.id, None, 0, 100),
        Err(CcrError::NotFound)
    ));
    assert!(matches!(
        store.retrieve(&scope("a"), &entry.id, Some("absent"), 0, 100),
        Err(CcrError::NotFound)
    ));
    let conn = Connection::open(store.path()).unwrap();
    let rows: Vec<(String, String, i64)> = conn
        .prepare(
            "SELECT tenant_id, status, returned_bytes FROM ccr_retrieval_audit ORDER BY audit_id",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        rows,
        vec![
            ("a".into(), "granted".into(), 6),
            ("b".into(), "refused".into(), 0),
            ("a".into(), "refused".into(), 0),
        ]
    );
    let stored_id: String = conn
        .query_row(
            "SELECT requested_id_sha256 FROM ccr_retrieval_audit LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(stored_id, entry.id);
    assert_eq!(
        stored_id,
        format!("{:x}", Sha256::digest(entry.id.as_bytes()))
    );
}

#[test]
fn empty_retrieval_id_is_persistently_audited_as_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    assert!(matches!(
        store.retrieve(&scope("a"), "", None, 0, 100),
        Err(CcrError::InvalidScope)
    ));
    let conn = Connection::open(store.path()).unwrap();
    let row: (String, String, i64) = conn
        .query_row(
            "SELECT requested_id_sha256,status,returned_bytes FROM ccr_retrieval_audit",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(row.0, format!("{:x}", Sha256::digest(b"")));
    assert_eq!(row.1, "refused");
    assert_eq!(row.2, 0);
}

#[test]
fn source_revocation_is_exact_and_prevents_restoring_the_same_call() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let first = store.put(&scope("a"), "search", "call-1", "first").unwrap();
    let other_call = store
        .put(&scope("a"), "search", "call-2", "second")
        .unwrap();
    let other_tenant = store.put(&scope("b"), "search", "call-1", "third").unwrap();
    assert_eq!(
        store
            .revoke_source_call(&scope("a"), "search", "call-1")
            .unwrap(),
        1
    );
    assert!(matches!(
        store.retrieve(&scope("a"), &first.id, None, 0, 100),
        Err(CcrError::NotFound)
    ));
    assert!(matches!(
        store.put(&scope("a"), "search", "call-1", "first"),
        Err(CcrError::Revoked)
    ));
    assert_eq!(
        store
            .retrieve(&scope("a"), &other_call.id, None, 0, 100)
            .unwrap()
            .text,
        "second"
    );
    assert_eq!(
        store
            .retrieve(&scope("b"), &other_tenant.id, None, 0, 100)
            .unwrap()
            .text,
        "third"
    );
}

#[test]
fn repeated_call_is_idempotent_but_changed_content_revokes_its_handles() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let owner = scope("a");
    let first = store
        .put(&owner, "search", "call-1", "first original")
        .unwrap();
    let repeat = store
        .put(&owner, "search", "call-1", "first original")
        .unwrap();
    assert_eq!(first, repeat);
    let other_tool = store.put(&owner, "fetch", "call-1", "other tool").unwrap();
    let other_tenant = store
        .put(&scope("b"), "search", "call-1", "other tenant")
        .unwrap();
    assert!(matches!(
        store.put(&owner, "search", "call-1", "changed original"),
        Err(CcrError::Revoked)
    ));
    assert!(matches!(
        store.retrieve(&owner, &first.id, None, 0, 64),
        Err(CcrError::NotFound)
    ));
    assert!(matches!(
        store.put(&owner, "search", "call-1", "first original"),
        Err(CcrError::Revoked)
    ));
    assert_eq!(
        store
            .retrieve(&owner, &other_tool.id, None, 0, 64)
            .unwrap()
            .text,
        "other tool"
    );
    assert_eq!(
        store
            .retrieve(&scope("b"), &other_tenant.id, None, 0, 64)
            .unwrap()
            .text,
        "other tenant"
    );
}

#[test]
fn source_status_check_covers_short_unstored_results() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let owner = scope("a");
    assert!(
        store
            .ensure_source_call_active(&owner, "search", "call-1")
            .is_ok()
    );
    store
        .revoke_source_call(&owner, "search", "call-1")
        .unwrap();
    assert!(matches!(
        store.ensure_source_call_active(&owner, "search", "call-1"),
        Err(CcrError::Revoked)
    ));
    assert!(
        store
            .ensure_source_call_active(&owner, "search", "call-2")
            .is_ok()
    );
    assert!(
        store
            .ensure_source_call_active(&scope("b"), "search", "call-1")
            .is_ok()
    );
    store.revoke_scope(&owner).unwrap();
    assert!(matches!(
        store.ensure_source_call_active(&owner, "search", "call-2"),
        Err(CcrError::Revoked)
    ));
}

#[test]
fn altered_original_fails_integrity_check_and_is_audited_as_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let entry = store
        .put(&scope("a"), "search", "call-1", "original")
        .unwrap();
    let conn = Connection::open(store.path()).unwrap();
    conn.execute(
        "UPDATE ccr_entries SET original='tampered' WHERE id=?1",
        [&entry.id],
    )
    .unwrap();
    assert!(matches!(
        store.retrieve(&scope("a"), &entry.id, None, 0, 100),
        Err(CcrError::Corrupt)
    ));
    let status: String = conn
        .query_row(
            "SELECT status FROM ccr_retrieval_audit ORDER BY audit_id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "refused");
}

#[test]
fn oversized_original_is_not_stored() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let big = "x".repeat(MAX_ORIGINAL_BYTES + 1);
    assert!(matches!(
        store.put(&scope("a"), "search", "call-1", &big),
        Err(CcrError::TooLarge)
    ));
}

#[test]
fn runtime_denies_unconfigured_sources_even_with_a_valid_scope() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let entry = store
        .put(&scope("a"), "search", "call-1", "private original")
        .unwrap();
    let runtime = CcrRuntime::new(store, scope("a"));
    assert!(
        runtime
            .source_key_for_call(Some("server"), "search")
            .is_none()
    );
    assert!(runtime.find("private original", 5).unwrap().is_empty());
    assert!(matches!(
        runtime.retrieve(&entry.id, None, 0, 64),
        Err(CcrError::NotFound)
    ));
}

#[test]
fn historical_task_score_requires_context_and_supports_chinese() {
    assert!(
        historical_task_score("Apple phone battery warranty", "apple phone battery")
            > historical_task_score("Apple fruit nutrition", "apple phone battery")
    );
    assert_eq!(historical_task_score("SLA for shipping", "refund SLA"), 0);
    assert!(historical_task_score("Refund SLA evidence", "refund SLA") > 0);
    assert!(
        historical_task_score("客服積壓與 SLA 升高", "請分析客服積壓")
            > historical_task_score("客服滿意度提高", "請分析客服積壓")
    );
    assert_eq!(historical_task_score("anything", "please show me this"), 0);
}

#[test]
fn historical_task_score_finds_interior_cluster_after_many_distractors() {
    let separated = format!("alpha {} beta {} ", "x".repeat(200), "y".repeat(200));
    let original = format!(
        "{}alpha beta {}",
        separated.repeat(2_300),
        separated.repeat(2_300)
    );
    assert!(original.len() < MAX_ORIGINAL_BYTES);
    assert_eq!(historical_task_score(&separated, "alpha beta"), 0);
    assert!(historical_task_score(&original, "alpha beta") > 0);
}

/// W2-E (review finding 4 / P3). The header list used to be hand-copied
/// here, so *any* tool result printing `## Constraints` opted itself out
/// of CCR compression. The exemption now needs the embedding process's
/// marker, passed in by the caller — this crate can never mint one.
#[test]
fn protected_sections_are_not_previewed() {
    let dir = tempfile::tempdir().unwrap();
    let sentinel = "e".repeat(64);
    let marker = duduclaw_core::protected_section::protected_marker_line(&sentinel);
    let base = CcrRuntime::new_unrestricted_for_test(
        CcrStore::new(dir.path().join("ccr.db")),
        scope("a"),
    );
    let runtime = base.clone().with_protected_sentinel(&sentinel);

    // Composer-shaped: header + this process's marker ⇒ never previewed.
    let protected = format!(
        "{}\n## Constraints\n{marker}\nNever disclose secrets\n",
        "x".repeat(20_000)
    );
    assert!(runtime.preview(&protected, "id").is_none());

    // The same text a tool result can print on its own ⇒ compressed like
    // anything else.
    let unmarked = format!(
        "{}\n## Constraints\nNever disclose secrets\n",
        "x".repeat(20_000)
    );
    assert!(
        runtime.preview(&unmarked, "id").is_some(),
        "a bare header must not buy an exemption"
    );

    // Someone else's sentinel is not this process's.
    let foreign = format!(
        "{}\n## Constraints\n{}\nNever disclose secrets\n",
        "x".repeat(20_000),
        duduclaw_core::protected_section::protected_marker_line(&"f".repeat(64))
    );
    assert!(runtime.preview(&foreign, "id").is_some());

    // No sentinel configured (the default) ⇒ no exemption at all.
    assert!(base.preview(&protected, "id").is_some());
}

#[test]
fn preview_compacts_json_without_losing_fields() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = CcrRuntime::new_unrestricted_for_test(
        CcrStore::new(dir.path().join("ccr.db")),
        scope("a"),
    );
    let value = serde_json::json!({"records": (0..100).map(|n| serde_json::json!({"id": n, "status": "ok"})).collect::<Vec<_>>()});
    let pretty = serde_json::to_string_pretty(&value).unwrap();
    let preview = runtime.preview(&pretty, "entry-id").unwrap();
    let compact = preview.split("\n[CCR:").next().unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(compact).unwrap(),
        value
    );
    assert!(preview.contains("entry-id"));
}

#[test]
fn preview_compacts_json_lines_without_losing_records() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = CcrRuntime::new_unrestricted_for_test(
        CcrStore::new(dir.path().join("ccr.db")),
        scope("a"),
    );
    let records: Vec<_> = (0..80)
        .map(|id| serde_json::json!({"id": id, "details": {"state": "ready"}}))
        .collect();
    let original = records
        .iter()
        .map(|value| {
            serde_json::to_string_pretty(value)
                .unwrap()
                .replace('\n', "                          ")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let preview = runtime.preview(&original, "entry-id").unwrap();
    let compact = preview.split("\n[CCR:").next().unwrap();
    let restored: Vec<serde_json::Value> = compact
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(restored, records);
    assert!(preview.len() * 2 < original.len());
}

#[test]
fn json_previews_preserve_duplicate_keys_numeric_spelling_and_string_spaces() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = CcrRuntime::new_unrestricted_for_test(
        CcrStore::new(dir.path().join("ccr.db")),
        scope("a"),
    );
    let record = "{                    \"x\"                    :                    1e+00,                    \"x\"                    :                    2,                    \"text\"                    :                    \"a  b \\\" c\"                    }";
    let original = format!("[\n{}\n]", vec![record; 120].join(",\n"));
    let preview = runtime.preview(&original, "entry-id").unwrap();
    let compact = preview.split("\n[CCR:").next().unwrap();
    assert_eq!(compact, strip_json_whitespace(&original));
    assert!(compact.contains("\"x\":1e+00,\"x\":2"));
    assert!(compact.contains("a  b \\\" c"));

    let jsonl = vec![record; 120].join("\n");
    let preview = runtime.preview(&jsonl, "entry-id").unwrap();
    let compact = preview.split("\n[CCR:").next().unwrap();
    assert_eq!(compact, vec![strip_json_whitespace(record); 120].join("\n"));
}

#[test]
fn dense_json_and_json_lines_bypass_lossy_preview() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = CcrRuntime::new_unrestricted_for_test(
        CcrStore::new(dir.path().join("ccr.db")),
        scope("a"),
    );
    let rows: Vec<_> = (0..300)
        .map(|id| serde_json::json!({"id": id, "exact_value": format!("value-{id:04}")}))
        .collect();
    let dense_json = serde_json::to_string(&rows).unwrap();
    assert!(dense_json.len() >= runtime.min_compress_bytes);
    assert!(runtime.preview(&dense_json, "entry-id").is_none());
    let dense_json_lines = rows
        .iter()
        .map(|row| serde_json::to_string(row).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(dense_json_lines.len() >= runtime.min_compress_bytes);
    assert!(runtime.preview(&dense_json_lines, "entry-id").is_none());
}

#[test]
fn lossless_compactor_preserves_duplicate_keys_and_numeric_lexemes() {
    let original = "{ \"priority\": 1.0e+02, \"priority\": 100, \"message\": \"a b\" }";
    assert_eq!(
        CcrRuntime::lossless_compact_structured(original).unwrap(),
        "{\"priority\":1.0e+02,\"priority\":100,\"message\":\"a b\"}"
    );
    assert_eq!(
        CcrRuntime::lossless_compact_structured("{\"priority\":1.0e+02}"),
        Some("{\"priority\":1.0e+02}".into())
    );
    assert!(CcrRuntime::lossless_compact_structured("{bad json}").is_none());
}

#[test]
fn unsupported_json_number_never_falls_through_to_lossy_preview() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = CcrRuntime::new_unrestricted_for_test(
        CcrStore::new(dir.path().join("ccr.db")),
        scope("a"),
    );
    let ordinary = "{ \"value\": 1 }";
    let unusual = "{ \"exact_value\": 1e400 }";
    let original = format!(
        "[\n{}\n]",
        std::iter::repeat_n(ordinary, 500)
            .chain(std::iter::once(unusual))
            .chain(std::iter::repeat_n(ordinary, 500))
            .collect::<Vec<_>>()
            .join(",\n")
    );
    assert!(original.len() >= runtime.min_compress_bytes);
    assert!(serde_json::from_str::<serde_json::Value>(&original).is_err());
    assert!(CcrRuntime::lossless_compact_structured(&original).is_none());
    assert!(runtime.preview(&original, "entry-id").is_none());
}

/// Regression: the "unparseable JSON keeps its bytes" guard used to test
/// the FIRST character only, so any prose that merely *opened* with `[`
/// or `{` — a bracketed log timestamp, a Markdown link — was permanently
/// excluded from CCR. A JSON document always closes with the matching
/// bracket; bracket-opening prose does not.
#[test]
fn bracket_opening_log_is_previewable_but_bracket_delimited_json_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = CcrRuntime::new_unrestricted_for_test(
        CcrStore::new(dir.path().join("ccr.db")),
        scope("a"),
    );

    let log = (0..400)
        .map(|index| {
            format!(
                "[2026-09-28T10:{:02}:{:02}Z] INFO worker-{index} processed batch {index} in {index} ms",
                index / 60,
                index % 60
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(log.starts_with('['), "fixture must open with a bracket");
    assert!(!log.trim_end().ends_with(']'));
    assert!(log.len() >= runtime.min_compress_bytes);
    assert!(CcrRuntime::lossless_compact_structured(&log).is_none());
    let preview = runtime
        .preview(&log, "entry-id")
        .expect("a bracket-opening log must still be compressible");
    assert!(preview.len() * 2 < log.len());

    let markdown = format!(
        "[report](https://example.invalid/report) summarises the run.\n{}",
        std::iter::repeat_n(
            "The connector retried the upstream call and then continued.",
            400
        )
        .collect::<Vec<_>>()
        .join("\n")
    );
    assert!(markdown.len() >= runtime.min_compress_bytes);
    assert!(
        runtime.preview(&markdown, "entry-id").is_some(),
        "prose opening with a Markdown link must still be compressible"
    );

    // Still refused: bracket-delimited at BOTH ends and unparseable.
    let unparseable = format!(
        "{{\n{}\n  \"exact_value\": 1e400\n}}",
        std::iter::repeat_n("  \"value\": 1,", 500)
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(unparseable.len() >= runtime.min_compress_bytes);
    assert!(serde_json::from_str::<serde_json::Value>(&unparseable).is_err());
    assert!(
        runtime.preview(&unparseable, "entry-id").is_none(),
        "an unvalidatable JSON document must keep its exact bytes"
    );
}

#[test]
fn trusted_source_allowlist_gates_storage_key_and_later_reads() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let allowed = CcrRuntime::new_unrestricted_for_test(store.clone(), scope("a"))
        .restrict_sources([("support-mcp".into(), "search".into())]);
    assert!(allowed.source_key_for_call(None, "search").is_none());
    assert!(
        allowed
            .source_key_for_call(Some("other-mcp"), "search")
            .is_none()
    );
    let key = allowed
        .source_key_for_call(Some("support-mcp"), "search")
        .unwrap();
    let entry = store
        .put(&scope("a"), &key, "call-1", "source-grounded original")
        .unwrap();
    assert_eq!(
        allowed.retrieve(&entry.id, None, 0, 64).unwrap().text,
        "source-grounded original"
    );
    let removed = CcrRuntime::new_unrestricted_for_test(store.clone(), scope("a"))
        .restrict_sources([("different-mcp".into(), "search".into())]);
    assert!(matches!(
        removed.retrieve(&entry.id, None, 0, 64),
        Err(CcrError::NotFound)
    ));
}

#[test]
fn saved_reference_rechecks_scope_route_call_integrity_and_revocation() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let runtime = CcrRuntime::new_unrestricted_for_test(store.clone(), scope("a"))
        .restrict_sources([("support-mcp".into(), "search".into())]);
    let source = runtime
        .source_key_for_call(Some("support-mcp"), "search")
        .unwrap();
    let entry = store
        .put(&scope("a"), &source, "call-1", "verified original")
        .unwrap();
    let valid = |scope: &CcrScope, route: &str, call: &str, bytes: usize| {
        runtime
            .valid_saved_reference(scope, &entry.id, route, call, bytes)
            .unwrap()
    };
    let score = |scope: &CcrScope, route: &str, call: &str| {
        runtime
            .saved_reference_task_score(
                scope,
                &entry.id,
                route,
                call,
                entry.content_bytes,
                "verified original",
            )
            .unwrap()
    };
    assert!(valid(
        &scope("a"),
        &source,
        "call-1",
        "verified original".len()
    ));
    assert!(score(&scope("a"), &source, "call-1").is_some());
    assert!(!valid(
        &scope("b"),
        &source,
        "call-1",
        "verified original".len()
    ));
    assert_eq!(score(&scope("b"), &source, "call-1"), None);
    assert!(!valid(
        &scope("a"),
        "other/search",
        "call-1",
        "verified original".len()
    ));
    assert_eq!(score(&scope("a"), "other/search", "call-1"), None);
    assert!(!valid(
        &scope("a"),
        &source,
        "wrong-call",
        "verified original".len()
    ));
    assert_eq!(score(&scope("a"), &source, "wrong-call"), None);
    assert!(!valid(&scope("a"), &source, "call-1", 1));
    let removed_route = CcrRuntime::new_unrestricted_for_test(store.clone(), scope("a"))
        .restrict_sources([("other-mcp".into(), "search".into())]);
    assert!(
        !removed_route
            .valid_saved_reference(
                &scope("a"),
                &entry.id,
                &source,
                "call-1",
                "verified original".len()
            )
            .unwrap()
    );
    assert_eq!(
        removed_route
            .saved_reference_task_score(
                &scope("a"),
                &entry.id,
                &source,
                "call-1",
                entry.content_bytes,
                "verified original",
            )
            .unwrap(),
        None
    );
    let conn = Connection::open(store.path()).unwrap();
    conn.execute(
        "UPDATE ccr_entries SET expires_at=0 WHERE id=?1",
        params![entry.id],
    )
    .unwrap();
    assert!(!valid(
        &scope("a"),
        &source,
        "call-1",
        "verified original".len()
    ));
    assert_eq!(score(&scope("a"), &source, "call-1"), None);
    conn.execute(
        "UPDATE ccr_entries SET expires_at=?1 WHERE id=?2",
        params![entry.expires_at, entry.id],
    )
    .unwrap();
    conn.execute(
        "UPDATE ccr_entries SET original='tampered original' WHERE id=?1",
        params![entry.id],
    )
    .unwrap();
    assert!(!valid(
        &scope("a"),
        &source,
        "call-1",
        "verified original".len()
    ));
    assert_eq!(score(&scope("a"), &source, "call-1"), None);
    conn.execute(
        "UPDATE ccr_entries SET original='verified original' WHERE id=?1",
        params![entry.id],
    )
    .unwrap();
    store
        .revoke_source_call(&scope("a"), &source, "call-1")
        .unwrap();
    assert!(!valid(
        &scope("a"),
        &source,
        "call-1",
        "verified original".len()
    ));
    assert_eq!(score(&scope("a"), &source, "call-1"), None);
}
