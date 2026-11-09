//! SR5 item 1: `scope` is an employee key; installs write only into a live
//! employee tree; approved requests re-check the Personal owner.
use super::*;
use crate::handlers::personal_data_gate::{
    authorize_break_glass, scoped_agent, write_scoped, Target, STEP_UP_REQUIRED_WRITE,
};
use crate::handlers::personal_p1g_gate::{default_deny_target, named_employees};
use crate::handlers::MethodHandler;
use crate::personal_visibility::tests::{home, record_personal, private_store, PERSONAL, SHARED};
use crate::personal_visibility::{DataCategory, Reader, NON_CANONICAL_AGENT};
use duduclaw_auth::mfa::MigrationConfirmationSession;
use serde_json::json;
use std::collections::BTreeSet;

struct Protected;
impl Protected {
    fn on() -> Self {
        crate::isolation_label_stores::TEST_PROTECTED.with(|cell| cell.set(Some(true)));
        Protected
    }
}
impl Drop for Protected {
    fn drop(&mut self) { crate::isolation_label_stores::TEST_PROTECTED.with(|cell| cell.set(None)); }
}

fn live_tree(home: &Path, label: &str) {
    let dir = home.join("agents").join(label);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("agent.toml"), format!("[agent]\nname = \"{label}\"\n")).unwrap();
}

/// An open (or closed, by `seal`) quarantine row on `label` in apply.db.
fn quarantine(home: &Path, label: &str, owner: Option<&str>, seal: &'static str) {
    use std::os::unix::fs::DirBuilderExt;
    let dir = home.join("migration-authority");
    if !dir.exists() { std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap(); }
    let db = rusqlite::Connection::open(dir.join("apply.db")).unwrap();
    crate::isolation_quarantine::ensure_employee_schema(&db).unwrap();
    crate::isolation_quarantine::insert_quarantine(&db, &crate::isolation_quarantine::NewQuarantine {
        employee: duduclaw_isolation::ids::EmployeeId([0x5a; 16]), incarnation: 0, label, owner, tenant: "tenant-a",
        quarantined_at: 1, root: None, seal, principal_snapshot: &json!({}), stripped: &json!({}),
    }).unwrap();
    crate::personal_visibility::invalidate_owner_cache(home);
}

const CLEAN: &str = "name: tidy-skill\ndescription: A helpful guide\n\n# Tidy Skill\n\nHelpful, harmless steps.\n";

#[test]
fn sr5_scope_names_an_employee_unless_org_wide() {
    assert_eq!(scope_employee("global"), None);
    assert_eq!(scope_employee("department:sales"), None);
    assert_eq!(scope_employee(""), None);
    assert_eq!(scope_employee(PERSONAL), Some(PERSONAL));
    // The P1e config-write gate runs on it (owner, or Admin + step-up + audit).
    assert_eq!(write_scoped("skills.install"), Some(("scope", DataCategory::Config)));
    assert_eq!(scoped_agent(&json!({"scope": "global"}), None, "scope"), Ok(None));
    assert_eq!(scoped_agent(&json!({"scope": "department:sales"}), None, "scope"), Ok(None));
    assert_eq!(scoped_agent(&json!({"scope": PERSONAL}), None, "scope"), Ok(Some(PERSONAL.to_string())));
    assert_eq!(scoped_agent(&json!({"scope": " alice-personal"}), None, "scope"), Err(NON_CANONICAL_AGENT));
    // The P1g default deny reads it for every method (install and request alike).
    let hidden: BTreeSet<String> = [PERSONAL.to_string()].into();
    for method in ["skills.install", "skills.install_request"] {
        assert_eq!(default_deny_target(method, &json!({"scope": PERSONAL}), &hidden), Some(PERSONAL.to_string()), "{method}");
        for org in ["global", "department:sales"] {
            assert_eq!(default_deny_target(method, &json!({"scope": org}), &hidden), None, "{method} {org}");
        }
    }
    assert!(named_employees(&json!({"scope": "global"})).is_empty());
    assert_eq!(named_employees(&json!({"scope": SHARED})), [SHARED.to_string()].into());
}

#[test]
fn sr5_admin_install_into_someone_elses_personal_employee_needs_step_up_and_is_audited() {
    let h = home();
    let now = chrono::Utc::now().timestamp();
    let Ok(Reader::BreakGlass { owner }) = crate::personal_visibility::classify_reader(h.path(), true, &h.admin, true, PERSONAL)
    else { panic!("Admin who is not the owner") };
    let target = Target { method: "skills.install", category: DataCategory::Config, agent: PERSONAL };
    assert_eq!(authorize_break_glass(&h.db, None, &h.admin, Some(&[7; 32]), now, &target, &owner),
        Err(STEP_UP_REQUIRED_WRITE));
    let proof = MigrationConfirmationSession { verified_at: (now - 60) as u64, csrf: [7; 32] };
    assert_eq!(authorize_break_glass(&h.db, Some(&proof), &h.admin, Some(&[7; 32]), now, &target, &owner), Ok(()));
    let rows = h.db.query_audit_log(Some(&h.admin), Some(WRITE_ACTION), 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].detail.as_deref().unwrap().contains("skills.install"));
}

#[test]
fn sr5_live_employee_dir_never_creates_a_tree() {
    let h = crate::fs_safe::private_test_home().unwrap();
    let home = h.path();
    assert_eq!(live_employee_dir(home, "../escape"), Err(INVALID_TARGET));
    assert_eq!(live_employee_dir(home, "ghost"), Err(NOT_LIVE_TARGET));
    assert!(!home.join("agents").join("ghost").exists());
    // A directory without agent.toml, or a symlinked tree, is no live employee.
    std::fs::create_dir_all(home.join("agents").join("half")).unwrap();
    assert_eq!(live_employee_dir(home, "half"), Err(NOT_LIVE_TARGET));
    live_tree(home, "alive");
    std::os::unix::fs::symlink(home.join("agents").join("alive"), home.join("agents").join("alias")).unwrap();
    assert_eq!(live_employee_dir(home, "alias"), Err(NOT_LIVE_TARGET));
    assert_eq!(live_employee_dir(home, "alive"), Ok(home.join("agents").join("alive")));
    // Protected mode: an open quarantine row refuses (F6h), even with a tree.
    let _protected = Protected::on();
    quarantine(home, "alive", None, "sealed");
    assert_eq!(live_employee_dir(home, "alive"), Err(crate::isolation_label_stores::LABEL_QUARANTINED));
}

#[tokio::test]
async fn sr5_run_skill_install_writes_only_into_a_live_employee() {
    let h = crate::fs_safe::private_test_home().unwrap();
    let home = h.path();
    let handler = MethodHandler::new(home.to_path_buf()).await;
    assert!(handler.run_skill_install("ghost", CLEAN, "tidy-skill").await.is_err());
    assert!(!home.join("agents").join("ghost").exists(), "agents/<label> is never created");
    live_tree(home, "alive");
    assert_eq!(handler.run_skill_install("alive", CLEAN, "tidy-skill").await, Ok("tidy-skill".to_string()));
    assert!(home.join("agents/alive/SKILLS/tidy-skill.md").exists());
    // A quarantined label (protected mode) is refused before anything is written.
    live_tree(home, "held");
    let _protected = Protected::on();
    quarantine(home, "held", None, "sealed");
    let refused = handler.run_skill_install("held", CLEAN, "tidy-skill").await.unwrap_err();
    assert!(refused.contains(crate::isolation_label_stores::LABEL_QUARANTINED), "{refused}");
    assert!(!home.join("agents/held/SKILLS").exists());
}

fn request(kind: &str, payload: serde_json::Value, requester: &str) -> crate::install_requests::InstallRequest {
    crate::install_requests::InstallRequest {
        id: "r1".into(), kind: kind.into(), title: "t".into(), description: "d".into(), requester_id: requester.into(),
        requester_email: "x@example.test".into(), requester_role: "employee".into(), requester_department: None,
        risk_level: "Clean".into(), scan: json!([]), payload, status: crate::install_requests::RequestStatus::Pending,
        manager_by: None, manager_at: None, admin_by: None, admin_at: None, decided_reason: None, executed: false,
        execute_error: None, created_at: String::new(), ttl_seconds: 3600,
    }
}

#[tokio::test]
async fn sr5_approved_request_paths_refuse_non_live_targets() {
    let h = crate::fs_safe::private_test_home().unwrap();
    let home = h.path();
    let handler = MethodHandler::new(home.to_path_buf()).await;
    let ghost = request("skill", json!({"scope": "ghost", "content": CLEAN}), "u1");
    assert!(handler.execute_approved_install(&ghost, "admin").await.is_err());
    assert!(crate::install_notify::apply_install_request(home, &ghost, None, "admin").await.is_err());
    assert!(!home.join("agents").join("ghost").exists());
    // An MCP request into a missing employee is refused the same way.
    let mcp = request("mcp", json!({"agent_id": "ghost", "server_name": "srv",
        "server_def": {"command": "echo", "args": []}}), "u1");
    assert!(crate::install_notify::apply_install_request(home, &mcp, None, "admin").await.is_err());
    assert!(!home.join("agents").join("ghost").exists());
    // A live employee installs through the channel twin too.
    live_tree(home, "alive");
    let alive = request("skill", json!({"scope": "alive", "content": CLEAN}), "u1");
    assert!(crate::install_notify::apply_install_request(home, &alive, None, "admin").await.is_ok());
    assert!(home.join("agents/alive/SKILLS/tidy-skill.md").exists());
    // Quarantined label: refused on both approved paths.
    live_tree(home, "held");
    let _protected = Protected::on();
    quarantine(home, "held", None, "sealed");
    let held = request("skill", json!({"scope": "held", "content": CLEAN}), "u1");
    assert!(handler.execute_approved_install(&held, "admin").await.is_err());
    assert!(crate::install_notify::apply_install_request(home, &held, None, "admin").await.is_err());
    assert!(!home.join("agents/held/SKILLS").exists());
    // What a request writes into.
    assert_eq!(request_employee(&request("skill", json!({"scope": "global"}), "u1")), None);
    assert_eq!(request_employee(&request("skill", json!({"scope": "department:x"}), "u1")), None);
    assert_eq!(request_employee(&held), Some("held"));
    assert_eq!(request_employee(&mcp), Some("ghost"));
    assert_eq!(request_method(&mcp), "mcp.install_request");
}

#[test]
fn sr5_approved_request_into_a_personal_employee_needs_its_owner() {
    let h = home();
    let write = |requester: &'static str| RequestedWrite {
        method: "skills.install_request", request_id: "r1", requester, decider: "decider", employee: PERSONAL,
    };
    let alice: &'static str = Box::leak(h.alice.clone().into_boxed_str());
    let bob: &'static str = Box::leak(h.bob.clone().into_boxed_str());
    // The owner's request: allowed, audited as a config write naming the decider.
    assert_eq!(authorize_requested_write(h.path(), Some(&h.db), true, &write(alice)), Ok(()));
    let rows = h.db.query_audit_log(Some(alice), Some(WRITE_ACTION), 10).unwrap();
    assert_eq!(rows.len(), 1);
    let detail: serde_json::Value = serde_json::from_str(rows[0].detail.as_deref().unwrap()).unwrap();
    assert_eq!((detail["decider_user_id"].as_str(), detail["owner_user_id"].as_str()), (Some("decider"), Some(alice)));
    // Anyone else's request (an Admin's included): refused, audited best effort.
    assert_eq!(authorize_requested_write(h.path(), Some(&h.db), true, &write(bob)), Err(REQUEST_NOT_OWNER));
    assert_eq!(authorize_requested_write(h.path(), Some(&h.db), true, &write("system")), Err(REQUEST_NOT_OWNER));
    assert_eq!(h.db.query_audit_log(Some("decider"), Some(WRITE_REFUSED_ACTION), 10).unwrap().len(), 2);
    // No audit store: refused for the owner too (fail closed).
    assert!(authorize_requested_write(h.path(), None, true, &write(alice)).is_err());
    // Shared employee / unprotected: no ownership rule.
    let shared = RequestedWrite { employee: SHARED, ..write(bob) };
    assert_eq!(authorize_requested_write(h.path(), Some(&h.db), true, &shared), Ok(()));
    assert_eq!(authorize_requested_write(h.path(), None, false, &write(bob)), Ok(()));
    // Unreadable ownership: refused.
    let empty = crate::fs_safe::private_test_home().unwrap();
    assert_eq!(authorize_requested_write(empty.path(), Some(&h.db), true, &write(alice)),
        Err(crate::personal_visibility::OWNERSHIP_UNAVAILABLE));
    // A tombstoned Personal employee (mirror gone, quarantine row open) keeps its owner.
    quarantine(h.path(), "alice-gone", Some(alice), "sealed");
    private_store(h.path());
    let gone = RequestedWrite { employee: "alice-gone", ..write(bob) };
    assert_eq!(authorize_requested_write(h.path(), Some(&h.db), true, &gone), Err(REQUEST_NOT_OWNER));
    let _ = record_personal;
}
