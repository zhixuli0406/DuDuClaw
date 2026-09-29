//! CCR tests, part 1 — moved verbatim out of `ccr.rs`.

use super::*;

#[test]
fn find_discloses_when_upstream_validation_budget_hides_a_valid_hit() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = CcrRuntime::new(CcrStore::new(dir.path().join("ccr.db")), scope("a"))
        .restrict_sources([("support-mcp".into(), "search".into())])
        .with_bound_source_validator(Arc::new(OnlyLiveSource));
    let source_tool = runtime
        .source_key_for_call(Some("support-mcp"), "search")
        .unwrap();
    let artifact = |id: &str| CcrSourceArtifact {
        connector: "causal".into(),
        artifact_id: id.into(),
        version: "v1".into(),
        acl_revision: "acl-v1".into(),
    };
    runtime
        .store
        .put_bound(
            &runtime.scope,
            &source_tool,
            "live-call",
            "source evidence live",
            &artifact("live"),
        )
        .unwrap();
    for index in 0..MAX_BOUND_FIND_VALIDATIONS {
        let id = format!("stale-{index}");
        runtime
            .store
            .put_bound(
                &runtime.scope,
                &source_tool,
                &id,
                "source evidence stale",
                &artifact(&id),
            )
            .unwrap();
    }
    let report = runtime.find_with_status("source evidence", 5).unwrap();
    assert!(report.hits.is_empty());
    assert!(report.source_validation_limited);
    assert_eq!(
        serde_json::to_value(&report).unwrap()["source_validation_limited"],
        true
    );
    assert!(runtime.find("source evidence", 5).unwrap().is_empty());
}

#[test]
fn bound_source_retention_caps_ccr_handle_lifetime() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db")).with_limits(3600, 100);
    let artifact = CcrSourceArtifact {
        connector: "causal".into(),
        artifact_id: "artifact".into(),
        version: "v1".into(),
        acl_revision: "immutable-acl".into(),
    };
    let deadline = unix_now() + 60;
    let saved = store
        .put_bound_until(
            &scope("a"),
            "source",
            "call",
            "exact source bytes",
            &artifact,
            deadline,
        )
        .unwrap();
    assert_eq!(saved.expires_at, deadline);
    let shorter = store
        .put_bound_until(
            &scope("a"),
            "source",
            "call",
            "exact source bytes",
            &artifact,
            deadline - 10,
        )
        .unwrap();
    assert_eq!(shorter.id, saved.id);
    assert_eq!(shorter.expires_at, deadline - 10);
    assert!(matches!(
        store.put_bound_until(
            &scope("a"),
            "source",
            "new-call",
            "exact source bytes",
            &artifact,
            unix_now(),
        ),
        Err(CcrError::Revoked)
    ));
}

#[test]
fn direct_store_and_runtime_without_validator_refuse_bound_originals() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let runtime = CcrRuntime::new(store.clone(), scope("a"))
        .restrict_sources([("support-mcp".into(), "search".into())]);
    let source_tool = runtime
        .source_key_for_call(Some("support-mcp"), "search")
        .unwrap();
    let bound = store
        .put_bound(
            &runtime.scope,
            &source_tool,
            "bound-call",
            "source evidence live",
            &CcrSourceArtifact {
                connector: "causal".into(),
                artifact_id: "live".into(),
                version: "v1".into(),
                acl_revision: "acl-v1".into(),
            },
        )
        .unwrap();
    let unbound = store
        .put(
            &runtime.scope,
            &source_tool,
            "unbound-call",
            "ordinary evidence",
        )
        .unwrap();
    assert!(matches!(
        store.retrieve(&runtime.scope, &bound.id, None, 0, 100),
        Err(CcrError::Revoked)
    ));
    assert!(matches!(
        runtime.retrieve(&bound.id, None, 0, 100),
        Err(CcrError::Revoked)
    ));
    assert!(runtime.find("source evidence", 5).unwrap().is_empty());
    assert!(
        !runtime
            .valid_saved_reference(
                &runtime.scope,
                &bound.id,
                &source_tool,
                "bound-call",
                bound.content_bytes
            )
            .unwrap()
    );
    assert_eq!(
        store
            .retrieve(&runtime.scope, &unbound.id, None, 0, 100)
            .unwrap()
            .text,
        "ordinary evidence"
    );
    let authorized = runtime.with_bound_source_validator(Arc::new(OnlyLiveSource));
    assert_eq!(
        authorized.retrieve(&bound.id, None, 0, 100).unwrap().text,
        "source evidence live"
    );
    assert_eq!(
        authorized.find("source evidence", 5).unwrap()[0].id,
        bound.id
    );
    assert!(
        authorized
            .valid_saved_reference(
                &authorized.scope,
                &bound.id,
                &source_tool,
                "bound-call",
                bound.content_bytes
            )
            .unwrap()
    );
    let conn = Connection::open(store.path()).unwrap();
    let refused: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ccr_retrieval_audit WHERE status='refused'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(refused, 2);
}

#[test]
fn missing_binding_row_remains_bound_and_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let runtime = CcrRuntime::new(store.clone(), scope("a"))
        .restrict_sources([("support-mcp".into(), "search".into())])
        .with_bound_source_validator(Arc::new(OnlyLiveSource));
    let source_tool = runtime
        .source_key_for_call(Some("support-mcp"), "search")
        .unwrap();
    let entry = store
        .put_bound(
            &runtime.scope,
            &source_tool,
            "bound-call",
            "source evidence live",
            &CcrSourceArtifact {
                connector: "causal".into(),
                artifact_id: "live".into(),
                version: "v1".into(),
                acl_revision: "acl-v1".into(),
            },
        )
        .unwrap();
    let conn = Connection::open(store.path()).unwrap();
    conn.execute(
        "DELETE FROM ccr_artifact_bindings WHERE entry_id=?1",
        [&entry.id],
    )
    .unwrap();
    let required: i64 = conn
        .query_row(
            "SELECT binding_required FROM ccr_entries WHERE id=?1",
            [&entry.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(required, 1);
    assert!(matches!(
        store.retrieve(&runtime.scope, &entry.id, None, 0, 100),
        Err(CcrError::Revoked)
    ));
    assert!(matches!(
        runtime.retrieve(&entry.id, None, 0, 100),
        Err(CcrError::Revoked)
    ));
    assert!(runtime.find("source evidence", 5).unwrap().is_empty());
    assert!(
        !runtime
            .valid_saved_reference(
                &runtime.scope,
                &entry.id,
                &source_tool,
                "bound-call",
                entry.content_bytes
            )
            .unwrap()
    );
    assert!(matches!(
        store.put(
            &runtime.scope,
            &source_tool,
            "bound-call",
            "source evidence live"
        ),
        Err(CcrError::Revoked)
    ));
}

#[test]
fn binding_deleted_after_store_grant_is_refused_before_runtime_return() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let source_tool = serde_json::to_string(&("support-mcp", "search")).unwrap();
    let scope = scope("a");
    let entry = store
        .put_bound(
            &scope,
            &source_tool,
            "call",
            "source evidence live",
            &CcrSourceArtifact {
                connector: "causal".into(),
                artifact_id: "live".into(),
                version: "v1".into(),
                acl_revision: "acl-v1".into(),
            },
        )
        .unwrap();
    let validator = Arc::new(DeleteBindingOnSecondValidation {
        db: store.path().to_path_buf(),
        entry_id: entry.id.clone(),
        checks: std::sync::atomic::AtomicUsize::new(0),
    });
    let runtime = CcrRuntime::new(store.clone(), scope)
        .restrict_sources([("support-mcp".into(), "search".into())])
        .with_bound_source_validator(validator);
    assert!(matches!(
        runtime.retrieve(&entry.id, None, 0, 100),
        Err(CcrError::Revoked)
    ));
    let conn = Connection::open(store.path()).unwrap();
    let (granted, refused): (i64, i64) = conn
        .query_row(
            "SELECT SUM(status='granted'),SUM(status='refused') FROM ccr_retrieval_audit",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((granted, refused), (1, 1));
}

#[test]
fn adding_binding_to_unbound_entry_marks_it_permanently_bound() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let owner = scope("a");
    let entry = store
        .put(&owner, "search", "call", "source evidence live")
        .unwrap();
    let conn = Connection::open(store.path()).unwrap();
    conn.execute(
        "INSERT INTO ccr_artifact_bindings
             (entry_id,tenant_id,connector,artifact_id,version,acl_revision)
             VALUES (?1,'a','causal','live','v1','acl-v1')",
        [&entry.id],
    )
    .unwrap();
    let required: i64 = conn
        .query_row(
            "SELECT binding_required FROM ccr_entries WHERE id=?1",
            [&entry.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(required, 1);
    assert!(matches!(
        store.retrieve(&owner, &entry.id, None, 0, 100),
        Err(CcrError::Revoked)
    ));
    conn.execute(
        "DELETE FROM ccr_artifact_bindings WHERE entry_id=?1",
        [&entry.id],
    )
    .unwrap();
    assert!(matches!(
        store.retrieve(&owner, &entry.id, None, 0, 100),
        Err(CcrError::Revoked)
    ));
}

#[test]
fn unbound_entry_deleted_after_store_grant_is_refused_before_runtime_return() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let owner = scope("a");
    let source_tool = serde_json::to_string(&("support-mcp", "search")).unwrap();
    let entry = store
        .put(&owner, &source_tool, "call", "ordinary evidence")
        .unwrap();
    let conn = Connection::open(store.path()).unwrap();
    conn.execute_batch(&format!(
        "CREATE TRIGGER drop_ccr_entry_after_grant
             AFTER INSERT ON ccr_retrieval_audit WHEN NEW.status='granted'
             BEGIN DELETE FROM ccr_entries WHERE id='{}'; END;",
        entry.id
    ))
    .unwrap();
    let runtime = CcrRuntime::new(store.clone(), owner)
        .restrict_sources([("support-mcp".into(), "search".into())]);
    assert!(matches!(
        runtime.retrieve(&entry.id, None, 0, 100),
        Err(CcrError::Revoked)
    ));
    let (granted, refused): (i64, i64) = conn
        .query_row(
            "SELECT SUM(status='granted'),SUM(status='refused') FROM ccr_retrieval_audit",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((granted, refused), (1, 1));
}

#[test]
fn artifact_version_revocation_scrubs_all_bound_handles_and_tombstones_calls() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let old = CcrSourceArtifact {
        connector: "support".into(),
        artifact_id: "ticket-1".into(),
        version: "v1".into(),
        acl_revision: "acl-1".into(),
    };
    let mut new = old.clone();
    new.version = "v2".into();
    let first = store
        .put_bound(&scope("a"), "search", "call-1", "unique old text", &old)
        .unwrap();
    let second = store
        .put_bound(&scope("a"), "search", "call-2", "another old text", &old)
        .unwrap();
    let current = store
        .put_bound(&scope("a"), "search", "call-3", "current text", &new)
        .unwrap();
    let other = store
        .put_bound(&scope("b"), "search", "call-1", "other tenant", &old)
        .unwrap();
    let unbound = store
        .put(&scope("a"), "search", "call-4", "unbound text")
        .unwrap();
    assert_eq!(
        store
            .revoke_artifact_version("a", "support", "ticket-1", "v1")
            .unwrap(),
        2
    );
    assert_eq!(
        store
            .revoke_artifact_version("a", "support", "ticket-1", "v1")
            .unwrap(),
        0
    );
    for entry in [&first, &second] {
        assert!(matches!(
            store.retrieve(&scope("a"), &entry.id, None, 0, 100),
            Err(CcrError::NotFound)
        ));
    }
    assert!(
        store
            .find(&scope("a"), "old text", None)
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        store.put_bound(&scope("a"), "search", "call-5", "old", &old),
        Err(CcrError::Revoked)
    ));
    assert!(matches!(
        store.put(&scope("a"), "search", "call-1", "unique old text"),
        Err(CcrError::Revoked)
    ));
    let current_runtime = CcrRuntime::new_unrestricted_for_test(store.clone(), scope("a"))
        .with_bound_source_validator(Arc::new(ExactTestSource {
            tenant: "a",
            version: "v2",
        }));
    assert!(current_runtime.retrieve(&current.id, None, 0, 100).is_ok());
    assert!(
        store
            .retrieve(&scope("a"), &unbound.id, None, 0, 100)
            .is_ok()
    );
    let other_runtime = CcrRuntime::new_unrestricted_for_test(store.clone(), scope("b"))
        .with_bound_source_validator(Arc::new(ExactTestSource {
            tenant: "b",
            version: "v1",
        }));
    assert!(other_runtime.retrieve(&other.id, None, 0, 100).is_ok());
    assert!(
        store
            .put_bound(&scope("a"), "search", "call-6", "fresh", &new)
            .is_ok()
    );
}

#[test]
fn artifact_revocation_accepts_a_tenant_scope_that_could_hold_a_bound_handle() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let long_tenant = "t".repeat(513);
    let caller = scope(&long_tenant);
    let artifact = CcrSourceArtifact {
        connector: "causal".into(),
        artifact_id: "artifact".into(),
        version: "v1".into(),
        acl_revision: "private".into(),
    };
    let entry = store
        .put_bound(&caller, "search", "call", "bound original", &artifact)
        .unwrap();
    assert_eq!(
        store
            .revoke_artifact_version(&long_tenant, "causal", "artifact", "v1")
            .unwrap(),
        1
    );
    assert!(store.retrieve(&caller, &entry.id, None, 0, 100).is_err());
    assert!(matches!(
        store.put_bound(&caller, "search", "later", "bound original", &artifact),
        Err(CcrError::Revoked)
    ));
}

#[test]
fn replay_with_changed_artifact_binding_revokes_exact_call() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let old = CcrSourceArtifact {
        connector: "support".into(),
        artifact_id: "ticket-1".into(),
        version: "v1".into(),
        acl_revision: "acl-1".into(),
    };
    let mut changed = old.clone();
    changed.acl_revision = "acl-2".into();
    let entry = store
        .put_bound(&scope("a"), "search", "call-1", "same text", &old)
        .unwrap();
    assert_eq!(
        entry.id,
        store
            .put_bound(&scope("a"), "search", "call-1", "same text", &old)
            .unwrap()
            .id
    );
    assert!(matches!(
        store.put_bound(&scope("a"), "search", "call-1", "same text", &changed),
        Err(CcrError::Revoked)
    ));
    assert!(matches!(
        store.retrieve(&scope("a"), &entry.id, None, 0, 100),
        Err(CcrError::NotFound)
    ));
}

#[test]
fn scope_revocation_removes_all_originals_and_blocks_reinsert() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let first = store.put(&scope("a"), "search", "call-1", "first").unwrap();
    let second = store.put(&scope("a"), "fetch", "call-2", "second").unwrap();
    let other_tenant = store.put(&scope("b"), "search", "call-1", "other").unwrap();
    let mut other_session = scope("a");
    other_session.session_id = "s2".into();
    let session_entry = store
        .put(&other_session, "search", "call-1", "session")
        .unwrap();
    assert_eq!(store.revoke_scope(&scope("a")).unwrap(), 2);
    assert_eq!(store.revoke_scope(&scope("a")).unwrap(), 0);
    for id in [&first.id, &second.id] {
        assert!(matches!(
            store.retrieve(&scope("a"), id, None, 0, 100),
            Err(CcrError::NotFound)
        ));
    }
    assert!(matches!(
        store.put(&scope("a"), "new-tool", "new-call", "new"),
        Err(CcrError::Revoked)
    ));
    let conn = Connection::open(store.path()).unwrap();
    conn.execute(
        "INSERT INTO ccr_entries
             (id,tenant_id,agent_id,session_id,source_acl,source_tool,source_call_id,
              content_sha256,transform_version,original,content_bytes,created_at,expires_at)
             VALUES (?1,'a','support','s1','private-thread','search','legacy-call',?2,1,'legacy',6,?3,?4)",
        params![first.id, format!("{:x}", Sha256::digest(b"legacy")), unix_now(), unix_now() + 60],
    ).unwrap();
    assert!(matches!(
        store.retrieve(&scope("a"), &first.id, None, 0, 100),
        Err(CcrError::NotFound)
    ));
    assert!(
        store
            .retrieve(&scope("b"), &other_tenant.id, None, 0, 100)
            .is_ok()
    );
    assert!(
        store
            .retrieve(&other_session, &session_entry.id, None, 0, 100)
            .is_ok()
    );
}

#[test]
fn committed_original_is_retrievable_and_utf8_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let entry = store
        .put(&scope("a"), "search", "call-1", "甲乙丙 fatal 丁戊")
        .unwrap();
    assert_eq!(entry.transform_version, CCR_ENTRY_VERSION);
    let reopened = CcrStore::new(store.path());
    let chunk = reopened
        .retrieve(&scope("a"), &entry.id, Some("fatal"), 0, 8)
        .unwrap();
    assert_eq!(chunk.text, "fatal ");
    assert_eq!(chunk.byte_offset, "甲乙丙 ".len());
    assert!(chunk.truncated);
}

/// Regression (W3-1 #1): `open()` stamps `PRAGMA user_version` and skips
/// the DDL + `BEGIN IMMEDIATE` migration window on an already-current
/// file; a stale stamp must re-run them.
#[test]
fn open_skips_schema_work_only_for_a_file_stamped_with_the_current_version() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    store.open().unwrap();
    let stamped: i64 = Connection::open(store.path())
        .unwrap()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(stamped, SCHEMA_VERSION);
    // A current stamp short-circuits: a table dropped behind the store's
    // back is NOT recreated while the stamp still says "current".
    Connection::open(store.path())
        .unwrap()
        .execute_batch("DROP TABLE ccr_revoked_scopes;")
        .unwrap();
    store.open().unwrap();
    assert!(
        Connection::open(store.path())
            .unwrap()
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='ccr_revoked_scopes'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap()
            == 0
    );
    // Clearing the stamp re-runs the whole batch.
    Connection::open(store.path())
        .unwrap()
        .execute_batch("PRAGMA user_version=0;")
        .unwrap();
    store.open().unwrap();
    let rows: i64 = Connection::open(store.path())
        .unwrap()
        .query_row("SELECT count(*) FROM ccr_revoked_scopes", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 0);
}

/// Regression (W3-1 #1): the scope index the `find` CTE depends on is
/// created by the same versioned batch.
#[test]
fn open_creates_the_scope_index_find_depends_on() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    store.open().unwrap();
    let sql: String = Connection::open(store.path())
        .unwrap()
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='index' AND name='idx_ccr_scope'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(sql.contains("tenant_id"), "{sql}");
    assert!(sql.contains("source_acl"), "{sql}");
}

/// Regression (W3-1 #1): the expiry `DELETE` (a write lock) must not run
/// on every `open()`. The first open sweeps; a second open seconds later
/// on the same handle must not, and SQL-level expiry filtering must still
/// hide the row either way.
#[test]
fn expiry_sweep_is_throttled_per_store_handle() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let entry = store
        .put(&scope("a"), "search", "call-1", "secret")
        .unwrap();
    Connection::open(store.path())
        .unwrap()
        .execute("UPDATE ccr_entries SET expires_at=0 WHERE id=?1", [&entry.id])
        .unwrap();
    // Second open within the throttle window: the dead row stays on disk.
    store.open().unwrap();
    let remaining: i64 = Connection::open(store.path())
        .unwrap()
        .query_row("SELECT count(*) FROM ccr_entries", [], |row| row.get(0))
        .unwrap();
    assert_eq!(remaining, 1, "sweep must be throttled, not run per open");
    // Reads never trust the sweep for expiry.
    assert!(matches!(
        store.retrieve(&scope("a"), &entry.id, None, 0, 100),
        Err(CcrError::NotFound)
    ));
    // A fresh handle (or one whose window elapsed) does sweep.
    CcrStore::new(dir.path().join("ccr.db")).open().unwrap();
    let swept: i64 = Connection::open(store.path())
        .unwrap()
        .query_row("SELECT count(*) FROM ccr_entries", [], |row| row.get(0))
        .unwrap();
    assert_eq!(swept, 0);
}

/// Regression (W3-1 #3): the CTE rewrite must return exactly what the
/// inline-expression form returned — same hits, same exact-match-first
/// ordering, same exclusion of non-matching rows.
#[test]
fn find_cte_rewrite_keeps_hits_and_exact_match_ordering() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let runtime = CcrRuntime::new_unrestricted_for_test(store.clone(), scope("a"));
    let loose = store
        .put(
            &scope("a"),
            "search",
            "loose",
            "invoice numbers appear here and the total appears far later",
        )
        .unwrap();
    let exact = store
        .put(&scope("a"), "search", "exact", "the invoice total is due")
        .unwrap();
    let other = store
        .put(&scope("a"), "search", "other", "unrelated passage 甲乙丙")
        .unwrap();
    let hits = runtime.find("invoice total", 5).unwrap();
    assert_eq!(hits.len(), 2, "{hits:?}");
    assert_eq!(hits[0].id, exact.id, "exact phrase must rank first: {hits:?}");
    assert!(hits[0].exact_phrase);
    assert_eq!(hits[1].id, loose.id, "{hits:?}");
    assert!(
        hits.iter().all(|hit| hit.id != other.id),
        "a non-matching row must not be returned: {hits:?}"
    );
}

/// Regression (W3-2 #2): projecting `lower(original)` once in a
/// `MATERIALIZED` CTE must not change what `find` returns when the scope
/// holds many large originals — the case the temp-store copy actually
/// costs something. Exact-phrase rows still sort first, case-insensitive
/// term matching still works on the lowercased column, and a row matching
/// no term is still excluded.
#[test]
fn find_returns_correct_hits_across_many_large_scoped_originals() {
    let dir = tempfile::tempdir().unwrap();
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let runtime = CcrRuntime::new_unrestricted_for_test(store.clone(), scope("a"));
    // ~24 KiB of filler per row so the lowercased projection is not free.
    let filler = "lorem ipsum dolor sit amet ".repeat(900);
    let mut term_only = Vec::new();
    for index in 0..40 {
        let entry = store
            .put(
                &scope("a"),
                "search",
                &format!("loose-{index}"),
                &format!("{filler} INVOICE ledger {index} and the TOTAL is elsewhere"),
            )
            .unwrap();
        term_only.push(entry.id);
    }
    // Newest exact-phrase row: must outrank all forty term-only rows.
    let exact = store
        .put(
            &scope("a"),
            "search",
            "exact",
            &format!("{filler} the invoice total is due"),
        )
        .unwrap();
    let unrelated = store
        .put(
            &scope("a"),
            "search",
            "unrelated",
            &format!("{filler} 甲乙丙 unrelated passage"),
        )
        .unwrap();

    let hits = runtime.find("invoice total", 5).unwrap();
    assert_eq!(hits.len(), 5, "{hits:?}");
    assert_eq!(hits[0].id, exact.id, "exact phrase must rank first");
    assert!(hits[0].exact_phrase);
    assert!(
        hits[1..].iter().all(|hit| !hit.exact_phrase),
        "only the exact-phrase row may be flagged: {hits:?}"
    );
    assert!(
        hits[1..]
            .iter()
            .all(|hit| term_only.contains(&hit.id) && hit.matched_terms == 2),
        "uppercase INVOICE/TOTAL must still match both lowercased terms: {hits:?}"
    );
    assert!(
        hits.iter().all(|hit| hit.id != unrelated.id),
        "a row matching no term must not be returned: {hits:?}"
    );
}
