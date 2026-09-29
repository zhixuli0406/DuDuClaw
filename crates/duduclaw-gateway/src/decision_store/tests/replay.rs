use super::*;

    #[test]
    fn immutable_scoped_inputs_replay_and_detect_changes() {
        let dir = tempfile::tempdir().unwrap();
        let store = DecisionStore::new(dir.path().join("decisions.db"));
        let scope = DecisionScope {
            tenant_id: "tenant-a".into(),
            acl: "support-private".into(),
        };
        let snapshot = DecisionSnapshot {
            id: "week-1".into(),
            queue_id: None,
            data_cutoff_utc: "2026-09-01T00:00:00Z".into(),
            source_version_hashes: vec!["ticket-export-v1".into()],
            seed: 42,
            arrivals_by_day: vec![4, 5],
            initial_backlog: vec![],
        };
        let model = QueueModel {
            version: "v1".into(),
            service_capacity_per_agent_day: 3,
            sla_days: 2,
            staff_cost_cents_per_agent_day: 1000,
        };
        let scenario = StaffingScenario {
            id: "base".into(),
            agents_by_day: vec![1, 1],
            fixed_extra_capacity_by_day: vec![0, 0],
        };
        let digest = store.put_snapshot(&scope, &snapshot).unwrap();
        assert_eq!(store.put_snapshot(&scope, &snapshot).unwrap(), digest);
        store.put_model(&scope, &model).unwrap();
        store.put_scenario(&scope, &scenario).unwrap();
        assert_eq!(
            store.replay(&scope, "week-1", "v1", "base").unwrap(),
            store.replay(&scope, "week-1", "v1", "base").unwrap()
        );
        let mut changed = snapshot.clone();
        changed.arrivals_by_day[0] = 100;
        assert!(matches!(
            store.put_snapshot(&scope, &changed),
            Err(DecisionStoreError::VersionConflict)
        ));
        let wrong_scope = DecisionScope {
            tenant_id: "tenant-b".into(),
            acl: scope.acl.clone(),
        };
        assert!(matches!(
            store.replay(&wrong_scope, "week-1", "v1", "base"),
            Err(DecisionStoreError::NotFound)
        ));
        let conn = Connection::open(dir.path().join("decisions.db")).unwrap();
        conn.execute(
            "UPDATE decision_inputs SET payload_json='{}' WHERE kind='snapshot' AND input_id='week-1'",
            [],
        ).unwrap();
        assert!(matches!(
            store.put_snapshot(&scope, &snapshot),
            Err(DecisionStoreError::Corrupt)
        ));
    }

    #[test]
    fn source_revocation_scrubs_only_matching_scoped_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let store = DecisionStore::new(dir.path().join("decisions.db"));
        let scope = DecisionScope {
            tenant_id: "a".into(),
            acl: "private".into(),
        };
        let mut affected = DecisionSnapshot {
            id: "affected".into(),
            queue_id: None,
            data_cutoff_utc: "2026-09-01T00:00:00Z".into(),
            source_version_hashes: vec!["tickets-v1".into()],
            seed: 1,
            arrivals_by_day: vec![3],
            initial_backlog: vec![],
        };
        let mut unaffected = affected.clone();
        unaffected.id = "unaffected".into();
        unaffected.source_version_hashes = vec!["tickets-v2".into()];
        store.put_snapshot(&scope, &affected).unwrap();
        store.put_snapshot(&scope, &unaffected).unwrap();
        store
            .put_model(
                &scope,
                &QueueModel {
                    version: "v1".into(),
                    service_capacity_per_agent_day: 2,
                    sla_days: 1,
                    staff_cost_cents_per_agent_day: 100,
                },
            )
            .unwrap();
        store
            .put_scenario(
                &scope,
                &StaffingScenario {
                    id: "base".into(),
                    agents_by_day: vec![1],
                    fixed_extra_capacity_by_day: vec![0],
                },
            )
            .unwrap();
        assert_eq!(
            store.revoke_source_version(&scope, "tickets-v1").unwrap(),
            1
        );
        assert!(matches!(
            store.replay(&scope, "affected", "v1", "base"),
            Err(DecisionStoreError::Revoked)
        ));
        assert!(store.replay(&scope, "unaffected", "v1", "base").is_ok());
        assert!(matches!(
            store.put_snapshot(&scope, &affected),
            Err(DecisionStoreError::Revoked)
        ));
        assert_eq!(
            store.revoke_source_version(&scope, "tickets-v1").unwrap(),
            0
        );
        let conn = Connection::open(dir.path().join("decisions.db")).unwrap();
        let payload: String = conn
            .query_row(
                "SELECT payload_json FROM decision_inputs WHERE input_id='affected'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(payload.is_empty());
        affected.id = "new-id".into();
        assert!(matches!(
            store.put_snapshot(&scope, &affected),
            Err(DecisionStoreError::Revoked)
        ));
        let other_scope = DecisionScope {
            tenant_id: "b".into(),
            acl: scope.acl.clone(),
        };
        store.put_snapshot(&other_scope, &affected).unwrap();
        assert!(
            store
                .get::<DecisionSnapshot>(&other_scope, "snapshot", "new-id")
                .is_ok()
        );
    }
