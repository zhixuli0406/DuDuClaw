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
fn message_initiator_is_rechecked_after_demotion_and_disable() {
    let (home, admin, _) = seed();
    let principal = format!("user:{admin}");
    initiators::record(home.path(), "message", "avatar", "other", "job", &Requester::attended(&admin)).unwrap();
    let recovered = initiators::lookup(home.path(), "message", "job", Some("avatar"), "other", Some(&principal)).unwrap();
    let roles = GatewayOwnerRoles::new(home.path(), recovered);
    assert_eq!(roles.requester_role(), Ok(Some(DelegationRequesterRole::Account("admin".into()))));
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
    receipts.execute("UPDATE message_initiators SET body=?1 WHERE id='job'", [b"{}".as_slice()]).unwrap();
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
    assert!(initiators::lookup(home.path(), "message", "job", Some("avatar"), "other", Some(&format!("user:{admin}"))).is_none());
    assert_eq!(initiators::record(home.path(), "message", "avatar", "other", "new", &Requester::attended(&admin)), Err(OwnerRoleLookupError));
}


#[test]
fn only_attended_pass_holders_can_mint_message_receipts() {
    let (home, admin, employee) = seed();
    for (id, sender, requester) in [
        ("background", "avatar", Requester::background()),
        ("ordinary-requester", "avatar", Requester::attended(&employee)),
        ("ordinary-sender", "ordinary", Requester::attended(&admin)),
    ] {
        initiators::record(home.path(), "message", sender, "other", id, &requester).unwrap();
        assert!(initiators::lookup(home.path(), "message", id, Some(sender), "other",
            Some(&format!("user:{admin}"))).is_none());
    }
    for kind in ["cron", "reminder", "task", "task_spec", "unknown"] {
        assert_eq!(initiators::record(home.path(), kind, "avatar", "other", "job",
            &Requester::attended(&admin)), Err(OwnerRoleLookupError));
    }
}

#[test]
fn message_receipt_expires_after_24_hours_and_rejects_future_timestamps() {
    let (home, admin, _) = seed();
    let principal = format!("user:{admin}");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    for (id, created_at, allowed) in [
        ("recent", now - 23 * 3600, true),
        ("expired", now - 24 * 3600, false),
        ("future", now + 3600, false),
    ] {
        initiators::record(home.path(), "message", "avatar", "other", id, &Requester::attended(&admin)).unwrap();
        initiators::set_created_at(home.path(), id, created_at);
        let result = initiators::consume_if(home.path(), id, "avatar", "other", Some(&principal),
            |requester| decision(&GatewayOwnerRoles::new(home.path(), requester)).ok());
        assert_eq!(result.is_some(), allowed, "{id}");
        assert!(initiators::lookup(home.path(), "message", id, Some("avatar"), "other", Some(&principal)).is_none());
    }
}

#[test]
fn message_receipt_is_consumed_only_by_an_unrestricted_allow() {
    let (home, admin, _) = seed();
    let principal = format!("user:{admin}");
    initiators::record(home.path(), "message", "avatar", "other", "job", &Requester::attended(&admin)).unwrap();
    for source in [None, Some(DelegationAllowSource::Mode), Some(DelegationAllowSource::OneWayPair),
        Some(DelegationAllowSource::TwoWayPair), Some(DelegationAllowSource::SystemSender)] {
        assert!(initiators::consume_if(home.path(), "job", "avatar", "other", Some(&principal), |_| source).is_none());
        assert!(initiators::lookup(home.path(), "message", "job", Some("avatar"), "other", Some(&principal)).is_some());
    }
    for (sender, target) in [("ordinary", "other"), ("avatar", "different")] {
        assert!(initiators::consume_if(home.path(), "job", sender, target, Some(&principal),
            |_| Some(DelegationAllowSource::UnrestrictedRole)).is_none());
    }
    assert_eq!(initiators::consume_if(home.path(), "job", "avatar", "other", Some(&principal),
        |requester| decision(&GatewayOwnerRoles::new(home.path(), requester)).ok()), Some(DelegationAllowSource::UnrestrictedRole));
    assert!(initiators::consume_if(home.path(), "job", "avatar", "other", Some(&principal),
        |_| Some(DelegationAllowSource::UnrestrictedRole)).is_none());
}

#[tokio::test]
async fn attended_created_jobs_and_queued_turns_never_inherit_the_pass() {
    let (home, admin, _) = seed();
    let principal = format!("user:{admin}");
    let who = RequesterIdentity::DashboardUser { user_id: admin.clone() };
    crate::knowledge::turn_requesters::CURRENT_REQUESTER.scope(who, async {
        assert!(decision(&GatewayOwnerRoles::new(home.path(), Requester::current(home.path(), "avatar"))).is_ok());
        let mut cron = crate::cron_store::CronTaskRow::new("cron".into(), "Cron".into(), "avatar".into(),
            "0 9 * * *".into(), "work".into());
        cron.on_behalf_of = Some(principal.clone());
        crate::cron_store::CronStore::open(home.path()).unwrap().insert(&cron).await.unwrap();
        let reminder: crate::reminder_scheduler::Reminder = serde_json::from_value(serde_json::json!({
            "id":"reminder", "agent_id":"avatar", "trigger_at":"2099-01-01T00:00:00Z",
            "channel":"test", "chat_id":"test", "prompt":"work", "mode":"agent_callback",
            "sender_agent":"avatar", "on_behalf_of":principal,
        })).unwrap();
        crate::reminder_scheduler::append_reminder(home.path(), &reminder).await.unwrap();
        let mut task = crate::task_store::TaskRow::new("task".into(), "Task".into(), "work".into(),
            "medium".into(), "avatar".into(), "avatar".into());
        task.on_behalf_of = Some(principal.clone());
        crate::task_store::TaskStore::open(home.path()).unwrap().insert_task(&task).await.unwrap();
        let mut spec = crate::task_spec::TaskSpec::new("avatar", "work", vec![crate::task_spec::Step {
            id: 0, description: "work".into(), agent: "avatar".into(), depends_on: vec![],
            acceptance_criteria: vec![], status: crate::task_spec::StepStatus::Pending,
            result: None, retry_count: 0,
        }]);
        spec.on_behalf_of = Some(principal.clone());
        let agent_dir = home.path().join("agents/avatar");
        std::fs::create_dir_all(&agent_dir).unwrap();
        spec.save(&agent_dir).unwrap();
        // Even an inherited attended task-local and stored admin principal cannot
        // turn any dispatched run into an attended conversation.
        for kind in ["cron", "reminder", "task", "task_spec", "heartbeat", "message"] {
            crate::memory_provenance::DISPATCH_RUN.scope(Some((format!("{kind}:avatar"), "run-id".into())),
                crate::on_behalf_of::scope_first(Some(principal.clone()), async {
                    assert_background_policy(home.path());
                    let guard = crate::on_behalf_of::context::register(home.path(), "avatar").unwrap();
                    let requester = crate::on_behalf_of::context::delegation_requester(home.path(), "avatar", guard.reference()).unwrap();
                    assert!(decision(&GatewayOwnerRoles::new(home.path(), requester)).is_err(), "{kind}");
                })).await;
        }
        crate::runtime::GOAL_ROUND_ATTRIBUTION.scope(crate::runtime::GoalRoundAttribution {
            episode_id: "task".into(), round: Some(1),
        }, async { assert_background_policy(home.path()); }).await;
    }).await;
}

fn assert_background_policy(home: &Path) {
    let requester = Requester::current(home, "avatar");
    let roles = GatewayOwnerRoles::new(home, requester.clone());
    assert!(roles.requester_role().unwrap().is_none());
    let org = duduclaw_core::MapOrgView::new().with_agent("avatar", "", "sales")
        .with_agent("other", "", "engineering");
    for policy in [DelegationPolicy::Disabled, DelegationPolicy::Department] {
        let rules = DelegationRules { policy, allow_pairs: vec![] };
        assert!(evaluate_delegation(&rules, &org, "avatar", "other", &DelegationContext {
            overrides: &DelegationOverrides { unrestricted_roles: vec!["admin".into()], ..Default::default() },
            owner_roles: Some(&roles), extra_system_senders: &[], boundaries: DelegationBoundaries {
                personal_target_allowed: true, main_target_allowed: true, can_send_cross_agent: true,
            },
        }).is_err());
    }
    initiators::record(home, "message", "avatar", "other", "background-followup", &requester).unwrap();
}

#[test]
fn concurrent_message_consumers_get_only_one_allow() {
    let (home, admin, _) = seed();
    let principal = format!("user:{admin}");
    initiators::record(home.path(), "message", "avatar", "other", "job", &Requester::attended(&admin)).unwrap();
    let barrier = std::sync::Barrier::new(2);
    let consume = || {
        barrier.wait();
        initiators::consume_if(home.path(), "job", "avatar", "other", Some(&principal),
            |requester| decision(&GatewayOwnerRoles::new(home.path(), requester)).ok()).is_some()
    };
    let allowed = std::thread::scope(|scope| {
        let first = scope.spawn(&consume);
        let second = scope.spawn(&consume);
        usize::from(first.join().unwrap()) + usize::from(second.join().unwrap())
    });
    assert_eq!(allowed, 1);
}
