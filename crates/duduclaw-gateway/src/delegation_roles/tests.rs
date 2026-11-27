use super::*;
use duduclaw_core::delegation_policy::*;

fn seed() -> (tempfile::TempDir, String, String) {
    let home = tempfile::tempdir().unwrap();
    let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
    let admin = db.create_user("admin@test.invalid", "Admin", "test-password", duduclaw_auth::UserRole::Admin).unwrap().id;
    let employee = db.create_user("employee@test.invalid", "Employee", "test-password", duduclaw_auth::UserRole::Employee).unwrap().id;
    let conn = Connection::open(home.path().join("users.db")).unwrap();
    conn.execute("UPDATE users SET avatar_agent_id='avatar' WHERE id=?1", [&admin]).unwrap();
    std::fs::write(home.path().join("config.toml"), "[delegation]\npolicy='none'\nunrestricted_roles=['admin']").unwrap();
    duduclaw_core::ensure_identity_key(home.path()).unwrap();
    (home, admin, employee)
}
fn decision(roles: &dyn OwnerRoleView) -> Result<DelegationAllowSource, DelegationDenied> {
    let rules = DelegationRules { policy: DelegationPolicy::Disabled, allow_pairs: vec![] };
    evaluate_delegation(&rules, &duduclaw_core::MapOrgView::new(), "avatar", "other", &DelegationContext {
        overrides: &DelegationOverrides { unrestricted_roles: vec!["admin".into()], ..Default::default() },
        owner_roles: Some(roles), extra_system_senders: &[], boundaries: DelegationBoundaries {
            personal_target_allowed: true, main_target_allowed: true, can_send_cross_agent: true,
        },
    })
}
#[test]
fn current_avatar_owner_and_requester_are_both_required() {
    let (home, admin, employee) = seed();
    for (id, expected) in [(&admin, true), (&employee, false)] {
        let roles = GatewayOwnerRoles::new(home.path(), Requester {
            who: RequesterIdentity::DashboardUser { user_id: id.clone() }, background: false,
        });
        assert_eq!(decision(&roles).is_ok(), expected);
        assert_eq!(decision(&roles.snapshot("avatar").unwrap()).is_ok(), expected);
    }
    let no_principal = GatewayOwnerRoles::new(home.path(), Requester::background());
    assert_eq!(no_principal.requester_role(), Ok(None));
    assert!(decision(&no_principal).is_err());
}
#[test]
fn background_initiator_is_rechecked_after_demotion_and_disable() {
    let (home, admin, _) = seed();
    let principal = format!("user:{admin}");
    initiators::record(home.path(), "message", "avatar", "other", "job", &Requester::attended(&admin)).unwrap();
    let recovered = initiators::lookup(home.path(), "message", "job", Some("avatar"), "other", Some(&principal)).unwrap();
    let roles = GatewayOwnerRoles::new(home.path(), recovered);
    assert_eq!(roles.requester_role(), Ok(Some(DelegationRequesterRole::BackgroundStartedBy("admin".into()))));
    assert_eq!(decision(&roles), Ok(DelegationAllowSource::UnrestrictedRole));
    let db = Connection::open(home.path().join("users.db")).unwrap();
    // Keep avatar ownership privileged; demote a distinct initiating account.
    let user_db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
    let initiator = user_db.create_user("initiator@test.invalid", "Initiator", "test-password", duduclaw_auth::UserRole::Admin).unwrap().id;
    initiators::record(home.path(), "message", "avatar", "other", "second", &Requester::attended(&initiator)).unwrap();
    let recovered = initiators::lookup(home.path(), "message", "second", Some("avatar"), "other", Some(&format!("user:{initiator}"))).unwrap();
    let roles = GatewayOwnerRoles::new(home.path(), recovered);
    assert_eq!(decision(&roles), Ok(DelegationAllowSource::UnrestrictedRole));
    db.execute("UPDATE users SET role='employee' WHERE id=?1", [&initiator]).unwrap();
    assert!(decision(&roles).is_err());
    db.execute("UPDATE users SET role='admin', status='disabled' WHERE id=?1", [&initiator]).unwrap();
    assert_eq!(roles.requester_role(), Ok(None));
    assert!(decision(&roles).is_err());
}
#[test]
fn unreadable_users_database_and_unbound_channel_grant_no_pass() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join("users.db")).unwrap();
    let roles = GatewayOwnerRoles::new(home.path(), Requester::background());
    assert_eq!(roles.avatar_owner_role("avatar"), Err(OwnerRoleLookupError));
    assert!(decision(&roles).is_err());
    let (home, admin, _) = seed();
    let roles = GatewayOwnerRoles::new(home.path(), Requester {
        who: RequesterIdentity::ChannelUser { user_id: Some(admin), channel: "telegram".into(),
            chat: "chat".into(), sender: "unbound".into(), attested_private: false }, background: false,
    });
    assert!(decision(&roles).is_err());
}

#[test]
fn signed_initiator_is_bound_to_record_sender_target_and_live_account() {
    let (home, admin, _) = seed();
    duduclaw_core::ensure_identity_key(home.path()).unwrap();
    let principal = format!("user:{admin}");
    let requester = Requester::attended(&admin);
    initiators::record(home.path(), "message", "avatar", "other", "job", &requester).unwrap();
    let load = |id, sender, target, principal| initiators::lookup(home.path(), "message", id, sender, target, principal);
    let recovered = load("job", Some("avatar"), "other", Some(&principal)).unwrap();
    assert!(decision(&GatewayOwnerRoles::new(home.path(), recovered)).is_ok());
    assert!(load("other-job", Some("avatar"), "other", Some(&principal)).is_none());
    assert!(load("job", Some("other-sender"), "other", Some(&principal)).is_none());
    assert!(load("job", Some("avatar"), "changed-target", Some(&principal)).is_none());
    assert!(load("job", Some("avatar"), "other", Some("user:forged")).is_none());
    let users = Connection::open(home.path().join("users.db")).unwrap();
    users.execute("UPDATE users SET role='employee' WHERE id=?1", [&admin]).unwrap();
    let recovered = load("job", Some("avatar"), "other", Some(&principal)).unwrap();
    assert!(decision(&GatewayOwnerRoles::new(home.path(), recovered)).is_err());
    let receipts = crate::fs_safe::sqlite::open(home.path(), "delegation-initiators.db").unwrap();
    receipts.execute("UPDATE initiators SET body=?1 WHERE id='job'", [b"{}".as_slice()]).unwrap();
    assert!(load("job", Some("avatar"), "other", Some(&principal)).is_none());
}

#[test]
fn ordinary_sender_cannot_launder_an_admin_started_job() {
    let (home, admin, _) = seed();
    let principal = format!("user:{admin}");
    let users = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
    let admin2 = users.create_user("second@test.invalid", "Second", "test-password", duduclaw_auth::UserRole::Admin).unwrap().id;
    Connection::open(home.path().join("users.db")).unwrap()
        .execute("UPDATE users SET avatar_agent_id='avatar2' WHERE id=?1", [&admin2]).unwrap();
    initiators::record(home.path(), "message", "avatar", "ordinary", "first", &Requester::attended(&admin)).unwrap();
    let ordinary_turn = initiators::lookup(home.path(), "message", "first", Some("avatar"), "ordinary", Some(&principal)).unwrap();
    initiators::record(home.path(), "message", "ordinary", "avatar2", "followup", &ordinary_turn).unwrap();
    let next = initiators::lookup(home.path(), "message", "followup", Some("ordinary"), "avatar2", Some(&principal));
    assert!(next.is_none());
    let roles = GatewayOwnerRoles::new(home.path(), next.unwrap_or_else(Requester::background));
    let read = read_delegation_config(home.path());
    let denied = evaluate_delegation(&read.rules, &duduclaw_core::MapOrgView::new(), "avatar2", "other",
        &DelegationContext { overrides: &read.overrides, owner_roles: Some(&roles), extra_system_senders: &[],
            boundaries: DelegationBoundaries { personal_target_allowed: true, main_target_allowed: true, can_send_cross_agent: true } }).unwrap_err();
    assert_eq!(denied.reason, DenyReason::DelegationDisabled);
}

#[tokio::test]
async fn cron_receipt_requires_current_persisted_content_and_target() {
    let (home, admin, _) = seed();
    let requester = Requester::attended(&admin);
    let principal = format!("user:{admin}");
    let store = crate::cron_store::CronStore::open(home.path()).unwrap();
    assert_eq!(initiators::record(home.path(), "cron", "avatar", "avatar", "missing", &requester), Err(OwnerRoleLookupError));
    assert_eq!(initiators::record(home.path(), "unknown", "avatar", "avatar", "cron", &requester), Err(OwnerRoleLookupError));
    let row = crate::cron_store::CronTaskRow::new("cron".into(), "Cron".into(), "avatar".into(),
        "0 9 * * *".into(), "original work".into());
    store.insert(&row).await.unwrap();
    assert_eq!(initiators::record(home.path(), "cron", "avatar", "other", "cron", &requester), Err(OwnerRoleLookupError));
    initiators::record(home.path(), "cron", "avatar", "avatar", "cron", &requester).unwrap();
    let load = || initiators::lookup(home.path(), "cron", "cron", None, "avatar", Some(&principal));
    assert_eq!(decision(&GatewayOwnerRoles::new(home.path(), load().unwrap())), Ok(DelegationAllowSource::UnrestrictedRole));
    Connection::open(home.path().join("cron_tasks.db")).unwrap()
        .execute("UPDATE cron_tasks SET task='edited work' WHERE id='cron'", []).unwrap();
    assert!(load().is_none());
    assert_eq!(decision(&GatewayOwnerRoles::new(home.path(), load().unwrap_or_else(Requester::background))).unwrap_err().reason,
        DenyReason::DelegationDisabled);
    initiators::record(home.path(), "cron", "avatar", "avatar", "cron", &requester).unwrap();
    assert_eq!(decision(&GatewayOwnerRoles::new(home.path(), load().unwrap())), Ok(DelegationAllowSource::UnrestrictedRole));
    let count: i64 = crate::fs_safe::sqlite::open(home.path(), "delegation-initiators.db").unwrap()
        .query_row("SELECT COUNT(*) FROM initiators WHERE kind='cron' AND id='cron'", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 2);
}

#[tokio::test]
async fn initiator_scope_is_bound_to_registered_target() {
    let (home, admin, _) = seed();
    let principal = format!("user:{admin}");
    initiators::record(home.path(), "message", "avatar", "avatar2", "job", &Requester::attended(&admin)).unwrap();
    let requester = initiators::lookup(home.path(), "message", "job", None, "avatar2", Some(&principal));
    // An attended fallback must not leak into another employee registered inside this scope.
    crate::knowledge::turn_requesters::CURRENT_REQUESTER.scope(
        RequesterIdentity::DashboardUser { user_id: admin.clone() },
        initiators::scope("avatar2", requester, async {
            assert!(matches!(initiators::current("avatar2"), Some(Some(_))));
            assert!(matches!(initiators::current("avatar"), Some(None)));
            let target = crate::on_behalf_of::context::register(home.path(), "avatar2").unwrap();
            let other = crate::on_behalf_of::context::register(home.path(), "avatar").unwrap();
            let target_requester = crate::on_behalf_of::context::delegation_requester(home.path(), "avatar2", target.reference()).unwrap();
            assert_eq!(GatewayOwnerRoles::new(home.path(), target_requester).requester_role(),
                Ok(Some(DelegationRequesterRole::BackgroundStartedBy("admin".into()))));
            let other_requester = crate::on_behalf_of::context::delegation_requester(home.path(), "avatar", other.reference()).unwrap();
            assert_eq!(decision(&GatewayOwnerRoles::new(home.path(), other_requester)).unwrap_err().reason, DenyReason::DelegationDisabled);
        }),
    ).await;
}

#[test]
fn protected_background_without_ingress_and_system_principals_grant_no_pass() {
    use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord, EmployeeBinding, Lane, Principal},
        ids::{EmployeeId, RunId, Sha256Digest}};
    let (home, admin, _) = seed();
    let context = |principal| duduclaw_mcp_boundary::RequestContext::from_verified_record(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]), task: None,
        principal, employee_binding: EmployeeBinding::Shared(Text::new("company".into()).unwrap()), lane: Lane::Standard,
        tools: List::new(vec![]).unwrap(), resources: List::new(vec![]).unwrap(), policy_revision: 1,
        gateway_epoch: [4; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX, nonce: [5; 32], jti: [6; 16],
    });
    let human = context(Principal::Human { user_id: Text::new(admin.clone()).unwrap(), generation: 1 });
    let background = context(Principal::Background { tenant_id: Text::new("company".into()).unwrap(), generation: 1 });
    for call in [&human, &background] {
        assert_eq!(decision(&GatewayOwnerRoles::new(home.path(), Requester::isolated(call, None))).unwrap_err().reason,
            DenyReason::DelegationDisabled);
    }
    let mut metadata = crate::isolation_egress::RunMetadata::root();
    metadata.ingress = Some(crate::isolation_egress::IngressRequester::Dashboard);
    assert_eq!(decision(&GatewayOwnerRoles::new(home.path(), Requester::isolated(&human, Some(&metadata)))),
        Ok(DelegationAllowSource::UnrestrictedRole));
    let forged = Requester { who: RequesterIdentity::System { kind: "background".into(), on_behalf_of: Some(admin.clone()) },
        background: true };
    assert_eq!(decision(&GatewayOwnerRoles::new(home.path(), forged)).unwrap_err().reason, DenyReason::DelegationDisabled);
}

#[cfg(unix)]
#[test]
fn initiator_database_is_private_and_refuses_symlinks() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let (home, admin, _) = seed();
    initiators::record(home.path(), "message", "avatar", "other", "job", &Requester::attended(&admin)).unwrap();
    let path = home.path().join("delegation-initiators.db");
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    let moved = home.path().join("saved.db");
    std::fs::rename(&path, &moved).unwrap();
    symlink(&moved, &path).unwrap();
    assert!(initiators::lookup(home.path(), "message", "job", None, "other", Some(&format!("user:{admin}"))).is_none());
    assert_eq!(initiators::record(home.path(), "message", "avatar", "other", "new", &Requester::attended(&admin)), Err(OwnerRoleLookupError));
}


fn assert_receipt_access(home: &Path, kind: &str, id: &str, admin: &str, allowed: bool) {
    let requester = initiators::lookup(home, kind, id, None, "avatar", Some(&format!("user:{admin}")));
    assert_eq!(requester.is_some(), allowed);
    assert_eq!(decision(&GatewayOwnerRoles::new(home,
        requester.unwrap_or_else(Requester::background))).is_ok(), allowed);
}

#[tokio::test]
async fn cron_pause_resume_requires_a_new_qualifying_initiator() {
    let (home, admin, employee) = seed();
    let store = crate::cron_store::CronStore::open(home.path()).unwrap();
    let row = crate::cron_store::CronTaskRow::new("cron".into(), "Cron".into(), "avatar".into(),
        "0 9 * * *".into(), "work".into());
    store.insert(&row).await.unwrap();
    initiators::record(home.path(), "cron", "avatar", "avatar", &row.id, &Requester::attended(&admin)).unwrap();
    assert_receipt_access(home.path(), "cron", &row.id, &admin, true);
    assert!(store.set_enabled(&row.id, false).await.unwrap());
    assert!(store.set_enabled(&row.id, true).await.unwrap());
    assert_receipt_access(home.path(), "cron", &row.id, &admin, false);
    initiators::record(home.path(), "cron", "avatar", "avatar", &row.id, &Requester::attended(&employee)).unwrap();
    assert_receipt_access(home.path(), "cron", &row.id, &admin, false);
    initiators::record(home.path(), "cron", "avatar", "avatar", &row.id, &Requester::attended(&admin)).unwrap();
    assert_receipt_access(home.path(), "cron", &row.id, &admin, true);
}

#[tokio::test]
async fn cron_delete_removes_all_receipt_versions() {
    let (home, admin, _) = seed();
    let store = crate::cron_store::CronStore::open(home.path()).unwrap();
    let row = crate::cron_store::CronTaskRow::new("cron".into(), "Cron".into(), "avatar".into(),
        "0 9 * * *".into(), "work".into());
    store.insert(&row).await.unwrap();
    initiators::record(home.path(), "cron", "avatar", "avatar", &row.id, &Requester::attended(&admin)).unwrap();
    assert_receipt_access(home.path(), "cron", &row.id, &admin, true);
    // Seed another target/version to prove revocation is not digest-scoped.
    {
        let db = crate::fs_safe::sqlite::open(home.path(), "delegation-initiators.db").unwrap();
        db.execute("INSERT INTO initiators SELECT kind,id,'other','old',body,mac FROM initiators", []).unwrap();
    }
    assert!(store.delete(&row.id).await.unwrap());
    let db = crate::fs_safe::sqlite::open(home.path(), "delegation-initiators.db").unwrap();
    let count: i64 = db.query_row("SELECT COUNT(*) FROM initiators WHERE kind='cron' AND id='cron'", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 0);
    drop(db);
    store.insert(&row).await.unwrap();
    assert_receipt_access(home.path(), "cron", &row.id, &admin, false);
}

#[tokio::test]
async fn task_cancel_reopen_and_reassign_revoke_initiator() {
    let (home, admin, _) = seed();
    let store = crate::task_store::TaskStore::open(home.path()).unwrap();
    let row = crate::task_store::TaskRow::new("task".into(), "Task".into(), "work".into(),
        "medium".into(), "avatar".into(), "avatar".into());
    store.insert_task(&row).await.unwrap();
    for fields in [serde_json::json!({"status":"cancelled"}), serde_json::json!({"assigned_to":"other"})] {
        initiators::record(home.path(), "task", "avatar", "avatar", &row.id, &Requester::attended(&admin)).unwrap();
        assert_receipt_access(home.path(), "task", &row.id, &admin, true);
        store.update_task(&row.id, &fields).await.unwrap().unwrap();
        assert_receipt_access(home.path(), "task", &row.id, &admin, false);
        store.update_task(&row.id, &serde_json::json!({"status":"todo","assigned_to":"avatar"})).await.unwrap().unwrap();
        assert_receipt_access(home.path(), "task", &row.id, &admin, false);
    }
}

#[tokio::test]
async fn reminder_cancel_revokes_initiator() {
    let (home, admin, _) = seed();
    let row: crate::reminder_scheduler::Reminder = serde_json::from_value(serde_json::json!({
        "id":"reminder", "agent_id":"avatar", "trigger_at":"2099-01-01T00:00:00Z",
        "channel":"test", "chat_id":"test", "prompt":"work", "mode":"agent_callback",
        "sender_agent":"avatar"
    })).unwrap();
    crate::reminder_scheduler::append_reminder(home.path(), &row).await.unwrap();
    initiators::record(home.path(), "reminder", "avatar", "avatar", &row.id, &Requester::attended(&admin)).unwrap();
    assert_receipt_access(home.path(), "reminder", &row.id, &admin, true);
    assert!(crate::reminder_scheduler::cancel_reminder(home.path(), &row.id, Some("avatar")).await.unwrap());
    assert_receipt_access(home.path(), "reminder", &row.id, &admin, false);
    // Restore identical pending material without re-recording to detect resurrection.
    std::fs::write(home.path().join("reminders.jsonl"), format!("{}\n", serde_json::to_string(&row).unwrap())).unwrap();
    assert_receipt_access(home.path(), "reminder", &row.id, &admin, false);
}

#[tokio::test]
async fn receipt_revoke_failure_does_not_block_cron_pause() {
    let (home, admin, _) = seed();
    let store = crate::cron_store::CronStore::open(home.path()).unwrap();
    let row = crate::cron_store::CronTaskRow::new("cron".into(), "Cron".into(), "avatar".into(),
        "0 9 * * *".into(), "work".into());
    store.insert(&row).await.unwrap();
    initiators::record(home.path(), "cron", "avatar", "avatar", &row.id, &Requester::attended(&admin)).unwrap();
    assert_receipt_access(home.path(), "cron", &row.id, &admin, true);
    let path = crate::fs_safe::sqlite::database_path(home.path(), "delegation-initiators.db");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(store.set_enabled(&row.id, false).await.unwrap());
    assert!(!store.get(&row.id).await.unwrap().unwrap().enabled);
    assert_receipt_access(home.path(), "cron", &row.id, &admin, false);
    let audit = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
    assert!(audit.lines().any(|line| {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        line.contains("delegation_initiator_revoke_failed")
            && event["details"]["kind"] == "cron" && event["details"]["id"] == "cron"
    }));
}
