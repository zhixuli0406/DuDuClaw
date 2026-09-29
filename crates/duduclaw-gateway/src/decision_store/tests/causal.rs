use super::*;

    #[test]
    fn coordinator_retries_after_causal_commit_without_decision_scrub() {
        let dir = tempfile::tempdir().unwrap();
        let causal = CausalStore::new(dir.path().join("memory.db"));
        let store =
            DecisionStore::with_causal_store(dir.path().join("decisions.db"), causal.clone());
        let scope = DecisionScope {
            tenant_id: "a".into(),
            acl: "private".into(),
        };
        let evidence_scope = EvidenceScope {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
        };
        let artifact = causal
            .add_artifact(
                &evidence_scope,
                "export",
                "tickets",
                "v1",
                "tickets",
                "source",
                1,
                i64::MAX,
            )
            .unwrap();
        let snapshot = DecisionSnapshot {
            id: "snap".into(),
            queue_id: None,
            data_cutoff_utc: "2026-09-01T00:00:00Z".into(),
            source_version_hashes: vec![artifact.content_sha256],
            seed: 1,
            arrivals_by_day: vec![1],
            initial_backlog: vec![],
        };
        store.put_snapshot(&scope, &snapshot).unwrap();
        store
            .bind_causal_artifact(&scope, &snapshot.id, &artifact.id)
            .unwrap();
        causal
            .invalidate_artifact(&evidence_scope, &artifact.id)
            .unwrap();
        assert_eq!(
            store
                .invalidate_causal_artifact(&scope, &artifact.id)
                .unwrap()
                .scrubbed_snapshots,
            1
        );
        assert!(matches!(
            store.get::<DecisionSnapshot>(&scope, "snapshot", &snapshot.id),
            Err(DecisionStoreError::Revoked)
        ));
    }

    #[test]
    fn causal_binding_revocation_scrubs_snapshot_and_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let causal = CausalStore::new(dir.path().join("memory.db"));
        let db = dir.path().join("decisions.db");
        let store = DecisionStore::with_causal_store(&db, causal.clone());
        let scope = DecisionScope {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
        };
        let evidence_scope = EvidenceScope {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
        };
        let artifact = causal
            .add_artifact(
                &evidence_scope,
                "ticket_export",
                "week-1",
                "v1",
                "tickets",
                "ticket bytes",
                1,
                i64::MAX,
            )
            .unwrap();
        let snapshot = DecisionSnapshot {
            id: "week-1".into(),
            queue_id: None,
            data_cutoff_utc: "2026-09-01T00:00:00Z".into(),
            source_version_hashes: vec![artifact.content_sha256.clone()],
            seed: 1,
            arrivals_by_day: vec![3],
            initial_backlog: vec![],
        };
        store.put_snapshot(&scope, &snapshot).unwrap();
        let second = DecisionSnapshot {
            id: "week-1-copy".into(),
            ..snapshot.clone()
        };
        store.put_snapshot(&scope, &second).unwrap();
        let unrelated = DecisionSnapshot {
            id: "unrelated".into(),
            source_version_hashes: vec!["different-source".into()],
            ..snapshot.clone()
        };
        store.put_snapshot(&scope, &unrelated).unwrap();
        assert!(matches!(
            store.bind_causal_artifact(&scope, &unrelated.id, &artifact.id),
            Err(DecisionStoreError::Invalid)
        ));
        store
            .bind_causal_artifact(&scope, &snapshot.id, &artifact.id)
            .unwrap();
        store
            .bind_causal_artifact(&scope, &second.id, &artifact.id)
            .unwrap();
        assert!(
            store
                .get::<DecisionSnapshot>(&scope, "snapshot", &snapshot.id)
                .is_ok()
        );
        let wrong_scope = DecisionScope {
            tenant_id: "tenant-b".into(),
            acl: scope.acl.clone(),
        };
        assert!(matches!(
            store.bind_causal_artifact(&wrong_scope, &snapshot.id, &artifact.id),
            Err(DecisionStoreError::Causal(CausalStoreError::NotFound))
        ));
        assert!(matches!(
            store.invalidate_causal_artifact(&wrong_scope, &artifact.id),
            Err(DecisionStoreError::Causal(CausalStoreError::NotFound))
        ));
        let unconfigured = DecisionStore::new(&db);
        assert!(matches!(
            unconfigured.get::<DecisionSnapshot>(&scope, "snapshot", &snapshot.id),
            Err(DecisionStoreError::CausalStoreRequired)
        ));
        let wrong_causal = CausalStore::new(dir.path().join("other-memory.db"));
        wrong_causal
            .add_artifact(
                &evidence_scope,
                "other",
                "other",
                "v1",
                "other",
                "unrelated",
                1,
                i64::MAX,
            )
            .unwrap();
        let wrong_store = DecisionStore::with_causal_store(&db, wrong_causal);
        assert!(matches!(
            wrong_store.get::<DecisionSnapshot>(&scope, "snapshot", &snapshot.id),
            Err(DecisionStoreError::Causal(CausalStoreError::NotFound))
        ));
        assert!(
            store
                .get::<DecisionSnapshot>(&scope, "snapshot", &snapshot.id)
                .is_ok()
        );
        assert_eq!(
            store
                .invalidate_causal_artifact(&scope, &artifact.id)
                .unwrap(),
            CausalInvalidationResult {
                demoted_claims: 0,
                scrubbed_snapshots: 2
            }
        );
        assert_eq!(
            store
                .invalidate_causal_artifact(&scope, &artifact.id)
                .unwrap(),
            CausalInvalidationResult {
                demoted_claims: 0,
                scrubbed_snapshots: 0
            }
        );
        assert!(matches!(
            store.get::<DecisionSnapshot>(&scope, "snapshot", &snapshot.id),
            Err(DecisionStoreError::Revoked)
        ));
        let conn = Connection::open(&db).unwrap();
        let payload: String = conn
            .query_row(
                "SELECT payload_json FROM decision_inputs WHERE input_id='week-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(payload.is_empty());
        assert!(matches!(
            store.get::<DecisionSnapshot>(&scope, "snapshot", &second.id),
            Err(DecisionStoreError::Revoked)
        ));
        assert!(
            store
                .get::<DecisionSnapshot>(&scope, "snapshot", &unrelated.id)
                .is_ok()
        );
        assert!(matches!(
            store.put_snapshot(&scope, &snapshot),
            Err(DecisionStoreError::Revoked)
        ));
    }

    #[test]
    fn admin_erase_retries_after_causal_commit_and_scrubs_bound_ccr_and_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let causal = CausalStore::new(dir.path().join("memory.db"));
        let store =
            DecisionStore::with_causal_store(dir.path().join("decisions.db"), causal.clone());
        let scope = DecisionScope {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
        };
        let evidence_scope = EvidenceScope {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
        };
        let artifact = causal
            .add_artifact(
                &evidence_scope,
                "export",
                "week-1",
                "v1",
                "lineage",
                "sensitive source text",
                1,
                i64::MAX,
            )
            .unwrap();
        let snapshot = DecisionSnapshot {
            id: "snap".into(),
            queue_id: None,
            data_cutoff_utc: "2026-09-01T00:00:00Z".into(),
            source_version_hashes: vec![artifact.content_sha256.clone()],
            seed: 1,
            arrivals_by_day: vec![1],
            initial_backlog: vec![],
        };
        store.put_snapshot(&scope, &snapshot).unwrap();
        store
            .bind_causal_artifact(&scope, &snapshot.id, &artifact.id)
            .unwrap();
        let ccr_db = dir.path().join("ccr.db");
        let ccr = duduclaw_llm::CcrStore::new(&ccr_db);
        let ccr_scope = duduclaw_llm::CcrScope {
            tenant_id: scope.tenant_id.clone(),
            agent_id: "support".into(),
            session_id: "session".into(),
            source_acl: scope.acl.clone(),
        };
        let ccr_runtime = duduclaw_llm::CcrRuntime::new(ccr.clone(), ccr_scope.clone())
            .restrict_sources([("test-causal".into(), "search".into())])
            .with_bound_source_validator(std::sync::Arc::new(TestActiveCausalSource(
                causal.clone(),
            )));
        let source_tool = ccr_runtime
            .source_key_for_call(Some("test-causal"), "search")
            .unwrap();
        let version = causal
            .source_record_version(&evidence_scope, &artifact.id)
            .unwrap();
        let entry = ccr
            .put_bound(
                &ccr_scope,
                &source_tool,
                "call",
                "sensitive source text",
                &duduclaw_llm::CcrSourceArtifact {
                    connector: "causal".into(),
                    artifact_id: artifact.id.clone(),
                    version: version.clone(),
                    acl_revision: scope.acl.clone(),
                },
            )
            .unwrap();
        let wrong_scope = DecisionScope {
            tenant_id: "tenant-b".into(),
            acl: scope.acl.clone(),
        };
        assert!(matches!(
            store.remove_causal_artifact_with_dependents(
                &wrong_scope,
                &artifact.id,
                CausalSourceRemoval::Erase,
                Some(&ccr_db)
            ),
            Err(DecisionStoreError::Causal(CausalStoreError::NotFound))
        ));
        assert!(ccr_runtime.retrieve(&entry.id, None, 0, 100).is_ok());
        // Simulate a prior attempt that committed its causal erase and then
        // lost the decision/CCR steps. The retry must use retained metadata.
        causal
            .erase_artifact(&evidence_scope, &artifact.id)
            .unwrap();
        let result = store
            .remove_causal_artifact_with_dependents(
                &scope,
                &artifact.id,
                CausalSourceRemoval::Erase,
                Some(&ccr_db),
            )
            .unwrap();
        assert_eq!(result.scrubbed_snapshots, 1);
        assert_eq!(result.scrubbed_ccr_originals, Some(1));
        assert_eq!(result.demoted_claims, 0);
        assert!(matches!(
            store.get::<DecisionSnapshot>(&scope, "snapshot", &snapshot.id),
            Err(DecisionStoreError::Revoked)
        ));
        assert!(ccr.retrieve(&ccr_scope, &entry.id, None, 0, 100).is_err());
        assert_eq!(
            causal
                .source_record_version(&evidence_scope, &artifact.id)
                .unwrap(),
            version
        );
        assert!(matches!(
            causal.source_text(&evidence_scope, &artifact.id),
            Err(CausalStoreError::NotFound)
        ));
        let retry = store
            .remove_causal_artifact_with_dependents(
                &scope,
                &artifact.id,
                CausalSourceRemoval::Erase,
                Some(&ccr_db),
            )
            .unwrap();
        assert_eq!(retry.scrubbed_snapshots, 0);
        assert_eq!(retry.scrubbed_ccr_originals, Some(0));
        assert_eq!(retry.demoted_claims, 0);
    }

    #[test]
    fn admin_invalidation_tombstones_absent_ccr_database_before_later_put() {
        let dir = tempfile::tempdir().unwrap();
        let causal = CausalStore::new(dir.path().join("memory.db"));
        let store =
            DecisionStore::with_causal_store(dir.path().join("decisions.db"), causal.clone());
        let scope = DecisionScope {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
        };
        let evidence_scope = EvidenceScope {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
        };
        let artifact = causal
            .add_artifact(
                &evidence_scope,
                "export",
                "week-1",
                "v1",
                "lineage",
                "source text",
                1,
                i64::MAX,
            )
            .unwrap();
        store.open().unwrap();
        let ccr_db = dir.path().join("ccr").join("ccr.db");
        assert!(!ccr_db.exists());
        let version = causal
            .source_record_version(&evidence_scope, &artifact.id)
            .unwrap();
        let result = store
            .remove_causal_artifact_with_dependents(
                &scope,
                &artifact.id,
                CausalSourceRemoval::Invalidate,
                Some(&ccr_db),
            )
            .unwrap();
        assert_eq!(result.scrubbed_ccr_originals, Some(0));
        assert!(ccr_db.is_file());
        let ccr_scope = duduclaw_llm::CcrScope {
            tenant_id: scope.tenant_id.clone(),
            agent_id: "support".into(),
            session_id: "session".into(),
            source_acl: scope.acl.clone(),
        };
        let put = duduclaw_llm::CcrStore::new(&ccr_db).put_bound(
            &ccr_scope,
            "search",
            "later-call",
            "source text",
            &duduclaw_llm::CcrSourceArtifact {
                connector: "causal".into(),
                artifact_id: artifact.id.clone(),
                version,
                acl_revision: scope.acl.clone(),
            },
        );
        assert!(matches!(put, Err(duduclaw_llm::CcrError::Revoked)));
    }

    #[test]
    fn causal_removal_requires_ccr_and_tombstones_before_failed_source_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let causal = CausalStore::new(dir.path().join("memory.db"));
        let store =
            DecisionStore::with_causal_store(dir.path().join("decisions.db"), causal.clone());
        let scope = DecisionScope {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
        };
        let evidence_scope = EvidenceScope {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
        };
        let artifact = causal
            .add_artifact(
                &evidence_scope,
                "ticket",
                "ticket-1",
                "v1",
                "lineage",
                "exact causal source",
                1,
                i64::MAX,
            )
            .unwrap();
        store.open().unwrap();
        let ccr_db = dir.path().join("ccr.db");
        let ccr = duduclaw_llm::CcrStore::new(&ccr_db);
        let ccr_scope = duduclaw_llm::CcrScope {
            tenant_id: scope.tenant_id.clone(),
            agent_id: "agent".into(),
            session_id: "session".into(),
            source_acl: scope.acl.clone(),
        };
        let ccr_runtime = duduclaw_llm::CcrRuntime::new(ccr.clone(), ccr_scope.clone())
            .restrict_sources([("test-causal".into(), "source".into())])
            .with_bound_source_validator(std::sync::Arc::new(TestActiveCausalSource(
                causal.clone(),
            )));
        let source_tool = ccr_runtime
            .source_key_for_call(Some("test-causal"), "source")
            .unwrap();
        let binding = duduclaw_llm::CcrSourceArtifact {
            connector: "causal".into(),
            artifact_id: artifact.id.clone(),
            version: artifact.version.clone(),
            acl_revision: scope.acl.clone(),
        };
        let entry = ccr
            .put_bound(
                &ccr_scope,
                &source_tool,
                "call-1",
                "exact causal source",
                &binding,
            )
            .unwrap();
        assert!(matches!(
            store.remove_causal_artifact_with_dependents(
                &scope,
                &artifact.id,
                CausalSourceRemoval::Invalidate,
                None,
            ),
            Err(DecisionStoreError::Invalid)
        ));
        assert!(ccr_runtime.retrieve(&entry.id, None, 0, 100).is_ok());
        assert!(causal.source_text(&evidence_scope, &artifact.id).is_ok());

        Connection::open(causal.path())
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER fail_causal_invalidation
                 BEFORE UPDATE OF invalidated_at ON causal_artifacts
                 BEGIN SELECT RAISE(ABORT, 'injected causal write failure'); END;",
            )
            .unwrap();
        assert!(
            store
                .remove_causal_artifact_with_dependents(
                    &scope,
                    &artifact.id,
                    CausalSourceRemoval::Invalidate,
                    Some(&ccr_db),
                )
                .is_err()
        );
        assert!(
            ccr.retrieve(&ccr_scope, &entry.id, None, 0, 100).is_err(),
            "CCR handle must already be revoked when causal write fails"
        );
        // Revocation is staged before the cross-database tombstone: once the
        // fence is written, readers see the source as gone even though the
        // local mutation has not committed yet. The failed attempt therefore
        // leaves a retryable fence, not a readable source.
        assert!(
            causal.source_text(&evidence_scope, &artifact.id).is_err(),
            "a staged revocation must hide the source while the mutation is pending"
        );
        Connection::open(causal.path())
            .unwrap()
            .execute_batch("DROP TRIGGER fail_causal_invalidation;")
            .unwrap();
        let retried = store
            .remove_causal_artifact_with_dependents(
                &scope,
                &artifact.id,
                CausalSourceRemoval::Invalidate,
                Some(&ccr_db),
            )
            .expect("retrying the same removal completes the pending mutation");
        assert_eq!(retried.scrubbed_ccr_originals, Some(0));
        assert!(
            causal.source_text(&evidence_scope, &artifact.id).is_err(),
            "the retried invalidation keeps the source unreadable"
        );
        let invalidated_at: Option<i64> = Connection::open(causal.path())
            .unwrap()
            .query_row(
                "SELECT invalidated_at FROM causal_artifacts WHERE id=?1",
                [&artifact.id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(invalidated_at.is_some(), "retry must commit the invalidation");
    }

    #[test]
    fn admin_erase_accepts_source_version_too_long_for_any_ccr_binding() {
        let dir = tempfile::tempdir().unwrap();
        let causal = CausalStore::new(dir.path().join("memory.db"));
        let store =
            DecisionStore::with_causal_store(dir.path().join("decisions.db"), causal.clone());
        let scope = DecisionScope {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
        };
        let evidence_scope = EvidenceScope {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
        };
        let version = "v".repeat(513);
        let artifact = causal
            .add_artifact(
                &evidence_scope,
                "export",
                "week-1",
                &version,
                "lineage",
                "sensitive source text",
                1,
                i64::MAX,
            )
            .unwrap();
        store.open().unwrap();
        let ccr_db = dir.path().join("ccr").join("ccr.db");
        let result = store
            .remove_causal_artifact_with_dependents(
                &scope,
                &artifact.id,
                CausalSourceRemoval::Erase,
                Some(&ccr_db),
            )
            .unwrap();
        assert_eq!(result.scrubbed_ccr_originals, Some(0));
        assert!(matches!(
            causal.source_text(&evidence_scope, &artifact.id),
            Err(CausalStoreError::NotFound)
        ));
        let stored_content: String = Connection::open(causal.path())
            .unwrap()
            .query_row(
                "SELECT content FROM causal_artifacts WHERE id=?1",
                [&artifact.id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(stored_content.is_empty());
        assert!(!ccr_db.exists());
    }

