//! F3 readiness reasons on the real A2 stores (this is a child of the
//! lifecycle test module and reuses its fixture; the root is a fixture).
use super::*;
use std::collections::BTreeMap;
use duduclaw_auth::AccessLevel;
use crate::isolation_connectors::{ConnectorCatalog, ConnectorKind};
#[cfg(target_os = "linux")]
use crate::isolation_connectors::{RelinkSubject, RouteScope};
use crate::isolation_readiness::reasons::{assess, Assessment, ConnectorReport, Reason};

type Reports = BTreeMap<String, ConnectorReport>;

fn report(label: &str, employee: u8, outcome: Result<Vec<&'static str>, duduclaw_mcp_boundary::VerificationError>) -> Reports {
    BTreeMap::from([(label.to_owned(), ConnectorReport { employee: EmployeeId([employee; 16]), outcome })])
}
fn clean(label: &str, employee: u8) -> Reports { report(label, employee, Ok(Vec::new())) }

fn reasons_of(assessment: &Assessment, label: &str) -> Vec<Reason> {
    assessment.employees.iter().find(|employee| employee.label == label)
        .unwrap_or_else(|| panic!("{label} not listed: {assessment:?}")).reasons.clone()
}

/// Confirmed Shared employee "team" (id 2) in the installation tenant.
fn shared_team(f: &Fixture) -> Root {
    let item = applied(f.home(), 2, "team", &[("binding_kind", "shared"), ("tenant_id", f.tenant.as_str())]);
    let root = receipt(&item, 2, false);
    save(f.home(), &item, &root).unwrap();
    Root(vec![root])
}

/// Confirmed Personal employee LABEL (id 1) owned by alice, owner bound.
fn alice_personal(f: &Fixture) -> Root {
    let item = applied(f.home(), 1, LABEL, &personal(&f.alice));
    let root = receipt(&item, 1, false);
    save(f.home(), &item, &root).unwrap();
    let _ = f.users.bind_agent(&f.alice, LABEL, AccessLevel::Owner);
    Root(vec![root])
}

#[test]
fn readiness_reports_each_reason() {
    use Reason::*;
    // Baseline: verified employees with a clean factory build have no reason.
    {
        let f = fixture();
        let roots = shared_team(&f);
        let assessment = assess(f.home(), &roots, Some(&clean("team", 2)));
        assert_eq!(reasons_of(&assessment, "team"), vec![]);
        assert!(assessment.reasons.is_empty());
        assert!(assessment.employees[0].ready);
    }
    #[cfg(target_os = "linux")]
    {
        let f = fixture();
        let roots = alice_personal(&f);
        assert_eq!(reasons_of(&assess(f.home(), &roots, Some(&clean(LABEL, 1))), LABEL), vec![]);
    }

    // binding_evidence_missing: the confirmed row's tenant pin no longer
    // matches the installation (lifecycle Denied).
    {
        let f = fixture();
        let roots = shared_team(&f);
        sql(f.home(), APPLY_DB, "UPDATE confirmed_employees SET tenant_id=?1 WHERE label='team'",
            &[&uuid::Uuid::new_v4().to_string()]);
        assert_eq!(reasons_of(&assess(f.home(), &roots, Some(&clean("team", 2))), "team"), vec![BindingEvidenceMissing]);
    }

    // connector_setup_required{provider}: the factory's SetupRequired refusals.
    {
        let f = fixture();
        let roots = shared_team(&f);
        let reports = report("team", 2, Ok(vec!["computer", "database"]));
        let assessment = assess(f.home(), &roots, Some(&reports));
        assert_eq!(reasons_of(&assessment, "team"), vec![
            ConnectorSetupRequired { provider: Some("computer") }, ConnectorSetupRequired { provider: Some("database") }]);
        // No build for this employee (missing factory, matrix 12), or a build
        // of an older employee under the same label: setup required.
        for reports in [Reports::new(), clean("team", 9)] {
            assert_eq!(reasons_of(&assess(f.home(), &roots, Some(&reports)), "team"),
                vec![ConnectorSetupRequired { provider: None }]);
        }
    }
    // connector_setup_required{google}: E2 refuses two Google routes.
    #[cfg(target_os = "linux")]
    {
        let f = fixture();
        let roots = alice_personal(&f);
        let catalog = ConnectorCatalog::open(f.home()).unwrap();
        let binding = EmployeeBinding::Personal(text(&f.alice));
        catalog.register("work", "google", ConnectorKind::Personal, &binding).unwrap();
        assert_eq!(reasons_of(&assess(f.home(), &roots, Some(&clean(LABEL, 1))), LABEL), vec![]);
        catalog.register("work again", "google", ConnectorKind::Personal, &binding).unwrap();
        assert_eq!(reasons_of(&assess(f.home(), &roots, Some(&clean(LABEL, 1))), LABEL),
            vec![ConnectorSetupRequired { provider: Some("google") }]);
    }

    // personal_binding_conflict: a second member bound outside A3 (count 2).
    #[cfg(target_os = "linux")]
    {
        let f = fixture();
        let roots = alice_personal(&f);
        sql(f.home(), USERS_DB, "DROP TRIGGER isolation_personal_binding_insert", &[]);
        sql(f.home(), USERS_DB, "INSERT INTO user_agent_bindings(user_id,agent_name,access_level,bound_at) VALUES(?1,?2,'viewer','2026-11-08T00:00:00Z')",
            &[&f.bob, &LABEL]);
        assert_eq!(reasons_of(&assess(f.home(), &roots, Some(&clean(LABEL, 1))), LABEL), vec![PersonalBindingConflict]);
    }
    // personal_binding_conflict: the users.db mirror names another owner
    // than the lifecycle (lifecycle Stale on a Personal employee).
    {
        let f = fixture();
        let roots = alice_personal(&f);
        // Simulate a store that bypassed the mirror's no-update trigger.
        sql(f.home(), USERS_DB, "DROP TRIGGER isolation_personal_agents_no_update", &[]);
        sql(f.home(), USERS_DB, "UPDATE isolation_personal_agents SET owner_user_id=?1 WHERE agent_name=?2", &[&f.bob, &LABEL]);
        assert_eq!(mirror(f.home(), LABEL).as_deref(), Some(f.bob.as_str()));
        assert_eq!(reasons_of(&assess(f.home(), &roots, Some(&clean(LABEL, 1))), LABEL), vec![PersonalBindingConflict]);
    }

    // revoked: the root reports the allocation disabled, or the confirmed
    // row is tombstoned.
    {
        let f = fixture();
        let item = applied(f.home(), 2, "team", &[("binding_kind", "shared"), ("tenant_id", f.tenant.as_str())]);
        let live = receipt(&item, 2, false);
        save(f.home(), &item, &live).unwrap();
        let dead = Root(vec![receipt(&item, 2, true)]);
        assert_eq!(reasons_of(&assess(f.home(), &dead, Some(&clean("team", 2))), "team"), vec![Revoked]);
        let mut disabled = live.clone();
        disabled.disabled = true;
        store_receipt(f.home(), "team", &disabled);
        assert_eq!(reasons_of(&assess(f.home(), &Root(vec![live]), Some(&clean("team", 2))), "team"), vec![Revoked]);
    }
    // revoked: an owner disconnect revoked a route that awaits reconnect (C4).
    #[cfg(target_os = "linux")]
    {
        let f = fixture();
        let roots = alice_personal(&f);
        let catalog = ConnectorCatalog::open(f.home()).unwrap();
        let binding = EmployeeBinding::Personal(text(&f.alice));
        catalog.register("work", "google", ConnectorKind::Personal, &binding).unwrap();
        catalog.revoke_scope(RouteScope { kind: ConnectorKind::Personal, binding: Some(&binding), provider: Some("google") },
            Some(&RelinkSubject::of("google", "google-sub-1").unwrap())).unwrap();
        assert_eq!(reasons_of(&assess(f.home(), &roots, Some(&clean(LABEL, 1))), LABEL), vec![Revoked]);
    }

    // service_unavailable: root ReadBack fails, the factory build could not
    // read its stores, or the report registry is unreadable.
    {
        let f = fixture();
        let roots = shared_team(&f);
        assert_eq!(reasons_of(&assess(f.home(), &Root(Vec::new()), Some(&clean("team", 2))), "team"), vec![ServiceUnavailable]);
        let failed = report("team", 2, Err(duduclaw_mcp_boundary::VerificationError::Unavailable));
        assert_eq!(reasons_of(&assess(f.home(), &roots, Some(&failed)), "team"), vec![ServiceUnavailable]);
        assert_eq!(reasons_of(&assess(f.home(), &roots, None), "team"), vec![ServiceUnavailable]);
    }

    // Payload: fixed codes and provider families only.
    let value = serde_json::to_value(vec![ConnectorSetupRequired { provider: Some("google") }, Revoked,
        ConnectorSetupRequired { provider: None }]).unwrap();
    assert_eq!(value, serde_json::json!([
        { "code": "connector_setup_required", "provider": "google" }, { "code": "revoked" },
        { "code": "connector_setup_required" }]));
}

#[test]
fn readiness_unreadable_store_is_service_unavailable() {
    // apply.db unsafe (group/other writable): the listing refuses, so the
    // whole assessment is service_unavailable and lists no employee as ready.
    {
        let f = fixture();
        let roots = shared_team(&f);
        set_mode(&f.home().join(APPLY_DB), 0o666);
        let assessment = assess(f.home(), &roots, Some(&clean("team", 2)));
        assert_eq!(assessment.reasons.into_iter().collect::<Vec<_>>(), vec![Reason::ServiceUnavailable]);
        assert!(assessment.employees.is_empty());
    }
    // A corrupt confirmed row: never a shortened list.
    {
        let f = fixture();
        let roots = shared_team(&f);
        sql(f.home(), APPLY_DB, "UPDATE confirmed_employees SET receipt='{' WHERE label='team'", &[]);
        let assessment = assess(f.home(), &roots, Some(&clean("team", 2)));
        assert!(assessment.reasons.contains(&Reason::ServiceUnavailable));
        assert!(assessment.employees.is_empty());
    }
    // users.db unsafe: the lifecycle read is Unavailable for that employee.
    {
        let f = fixture();
        let roots = shared_team(&f);
        set_mode(&f.home().join(USERS_DB), 0o666);
        let assessment = assess(f.home(), &roots, Some(&clean("team", 2)));
        assert_eq!(reasons_of(&assessment, "team"), vec![Reason::ServiceUnavailable]);
        assert!(!assessment.employees[0].ready);
    }
    // Connector catalog present but unsafe: unavailable, not "no routes".
    {
        let f = fixture();
        let roots = shared_team(&f);
        drop(ConnectorCatalog::open(f.home()).unwrap());
        set_mode(&f.home().join(crate::isolation_connectors::CATALOG_FILE), 0o666);
        assert_eq!(reasons_of(&assess(f.home(), &roots, Some(&clean("team", 2))), "team"), vec![Reason::ServiceUnavailable]);
    }
    // No apply.db at all: nothing was confirmed, so nothing to report.
    {
        let f = fixture();
        std::fs::remove_file(f.home().join(APPLY_DB)).unwrap();
        assert_eq!(assess(f.home(), &Root(Vec::new()), Some(&Reports::new())), Assessment::default());
    }
}
