//! RM-027 org chart editing — `org.propose_change`, `org.apply_change`,
//! `org.pending_changes`, the `org_change` approval and the `agents.update`
//! reporting-line check, end to end through `MethodHandler::handle` with a
//! real `users.db` (roles and bindings are re-read live).
use super::*;
use super::agent_files_rpc_tests::audit_rows;
use crate::approval::{ApprovalBroker, ApprovalId, ApprovalStatus};
use duduclaw_core::org_store::{self, OrgEntry};

fn ctx(id: &str, role: UserRole, access: &[(&str, AccessLevel)]) -> UserContext {
    UserContext {
        credential_epoch: 0,
        user_id: id.into(),
        email: format!("{id}@example.invalid"),
        role,
        agent_access: access.iter().map(|(a, l)| (a.to_string(), *l)).collect(),
        must_change_password: false,
        team_access: Default::default(),
        acl_generation: 0,
    }
}

fn ok(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: true, payload: Some(p), .. } => p.clone(),
        other => panic!("expected ok, got {other:?}"),
    }
}

/// `(code, message)` of an error frame; `code` is empty for a plain-string error.
fn refusal(frame: &WsFrame) -> (String, String) {
    match frame {
        WsFrame::Response { ok: false, error: Some(e), .. } => match e {
            Value::String(s) => (String::new(), s.clone()),
            other => (
                other["code"].as_str().unwrap_or_default().to_string(),
                other["message"].as_str().unwrap_or_default().to_string(),
            ),
        },
        other => panic!("expected error, got {other:?}"),
    }
}

struct Fx {
    _dir: tempfile::TempDir,
    home: PathBuf,
    h: MethodHandler,
    admin: UserContext,
    /// Manager: Owner on `lead`, Operator on `rep`, Viewer on `other`.
    mgr: UserContext,
    /// Manager bound only to `other`.
    mgr2: UserContext,
    /// Employee with Viewer on `rep`.
    viewer: UserContext,
    /// Manager with only a Viewer binding on `rep`.
    viewer_mgr: UserContext,
}

/// ceo ← lead ← rep (業務) and ceo ← other (經營); four dashboard accounts.
async fn fixture() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    duduclaw_core::write_minimal_config(&home, "127.0.0.1", 18889).unwrap();
    let h = MethodHandler::new(home.clone()).await;
    let system = UserContext::admin_fallback();
    for p in [
        json!({ "name": "ceo", "department": "經營" }),
        json!({ "name": "lead", "reports_to": "ceo", "department": "業務" }),
        json!({ "name": "rep", "reports_to": "lead", "department": "業務" }),
        json!({ "name": "other", "reports_to": "ceo", "department": "經營" }),
    ] {
        ok(&h.handle_agents_create_as(p, Some(&system)).await);
    }
    let db = duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();
    let add = |id: &str, role: UserRole, binds: &[(&str, AccessLevel)]| {
        let u = db.create_user(&format!("{id}@example.invalid"), id, "isolated-test-password", role).unwrap();
        rusqlite::Connection::open(home.join("users.db"))
            .unwrap()
            .execute("UPDATE users SET id=?1 WHERE id=?2", rusqlite::params![id, u.id])
            .unwrap();
        for (a, l) in binds {
            db.bind_agent(id, a, *l).unwrap();
        }
        ctx(id, role, binds)
    };
    let admin = add("admin-1", UserRole::Admin, &[]);
    let mgr = add(
        "mgr-1",
        UserRole::Manager,
        &[("lead", AccessLevel::Owner), ("rep", AccessLevel::Operator), ("other", AccessLevel::Viewer)],
    );
    let mgr2 = add("mgr-2", UserRole::Manager, &[("other", AccessLevel::Operator)]);
    let viewer = add("viewer-1", UserRole::Employee, &[("rep", AccessLevel::Viewer)]);
    let viewer_mgr = add("mgr-3", UserRole::Manager, &[("rep", AccessLevel::Viewer)]);
    Fx { _dir: dir, home, h, admin, mgr, mgr2, viewer, viewer_mgr }
}

impl Fx {
    async fn call(&self, method: &str, params: Value, who: &UserContext) -> WsFrame {
        Box::pin(self.h.handle(method, params, who)).await
    }

    fn authority(&self, id: &str) -> OrgEntry {
        org_store::load(&self.home).get(id).cloned().unwrap_or_default()
    }

    fn mirror(&self, id: &str) -> OrgEntry {
        org_store::read_mirror(&self.home.join("agents").join(id).join("agent.toml")).unwrap_or_default()
    }

    /// An authority write made elsewhere (e.g. `duduclaw org sync`), under
    /// the global org lock like every authority writer.
    fn set_authority(&self, id: &str, entry: OrgEntry) {
        org_store::with_org_lock(&self.home, |org| org_store::upsert(org, id, entry)).unwrap();
    }

    /// The employee tampers with its own `agent.toml` (the display mirror).
    fn tamper_mirror(&self, id: &str, from: &str, to: &str) {
        let path = self.home.join("agents").join(id).join("agent.toml");
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains(from), "fixture must contain {from}: {raw}");
        std::fs::write(&path, raw.replace(from, to)).unwrap();
    }

    /// Move a card's `created_at` so that `secs_left` remain of its TTL.
    fn expire_in(&self, id: &str, secs_left: i64) {
        let created = (chrono::Utc::now()
            - chrono::Duration::seconds(crate::org_change_approval::TTL_SECS - secs_left))
        .to_rfc3339();
        rusqlite::Connection::open(self.home.join("approvals.db"))
            .unwrap()
            .execute("UPDATE approvals SET created_at=?1 WHERE id=?2", rusqlite::params![created, id])
            .unwrap();
    }

    async fn status(&self, id: &str) -> ApprovalStatus {
        ApprovalBroker::open(&self.home).unwrap().get(&ApprovalId::from(id.to_string())).await.unwrap().unwrap().status
    }

    /// The Manager proposes rep: lead → other.
    async fn propose(&self) -> String {
        let p = ok(&self
            .call(
                "org.propose_change",
                json!({ "agent_id": "rep", "reports_to": "other", "reason": "改由 other 帶" }),
                &self.mgr,
            )
            .await);
        assert_eq!(p["status"], "pending_approval", "{p}");
        p["approval_id"].as_str().unwrap().to_string()
    }
}

// ── agents.update (the closed gap) ───────────────────────────────────────────

#[tokio::test]
async fn agents_update_refuses_a_cycle_and_a_missing_supervisor() {
    let f = fixture().await;
    let before = std::fs::read_to_string(f.home.join("org.toml")).unwrap();
    let (_, e) = refusal(&f.call("agents.update", json!({ "agent_id": "lead", "reports_to": "rep" }), &f.admin).await);
    assert!(e.contains("循環"), "{e}");
    let (_, e) = refusal(&f.call("agents.update", json!({ "agent_id": "lead", "reports_to": "lead" }), &f.admin).await);
    assert!(e.contains("自己"), "{e}");
    let (_, e) = refusal(&f.call("agents.update", json!({ "agent_id": "lead", "reports_to": "ghost" }), &f.admin).await);
    assert!(e.contains("不存在"), "{e}");
    // Nothing moved — neither the authority nor the mirror.
    assert_eq!(std::fs::read_to_string(f.home.join("org.toml")).unwrap(), before);
    assert_eq!(f.mirror("lead").reports_to, "ceo");
    // A legitimate move still works.
    ok(&f.call("agents.update", json!({ "agent_id": "rep", "reports_to": "other" }), &f.admin).await);
    assert_eq!(f.authority("rep").reports_to, "other");
}

/// `config.raw.set` of `agent:<id>` with `reports_to` rewritten.
async fn raw_set_reports_to(f: &Fx, id: &str, from: &str, to: &str) -> WsFrame {
    let raw = ok(&f.call("config.raw.get", json!({ "file": format!("agent:{id}") }), &f.admin).await);
    let old = format!("reports_to = \"{from}\"");
    let content = raw["content"].as_str().unwrap();
    assert!(content.contains(&old), "fixture must show {old}: {content}");
    let content = content.replace(&old, &format!("reports_to = \"{to}\""));
    f.call("config.raw.set", json!({ "file": format!("agent:{id}"), "content": content, "base_hash": raw["hash"] }), &f.admin).await
}

#[tokio::test]
async fn raw_editor_refuses_a_cycle_and_a_missing_supervisor() {
    let f = fixture().await;
    let org_before = std::fs::read_to_string(f.home.join("org.toml")).unwrap();
    let toml_before = std::fs::read_to_string(f.home.join("agents/lead/agent.toml")).unwrap();
    let (_, e) = refusal(&raw_set_reports_to(&f, "lead", "ceo", "rep").await);
    assert!(e.contains("循環"), "{e}");
    let (_, e) = refusal(&raw_set_reports_to(&f, "lead", "ceo", "ghost").await);
    assert!(e.contains("不存在"), "{e}");
    // The raw save was refused: neither the file nor the authority moved.
    assert_eq!(std::fs::read_to_string(f.home.join("agents/lead/agent.toml")).unwrap(), toml_before);
    assert_eq!(std::fs::read_to_string(f.home.join("org.toml")).unwrap(), org_before);
    assert_eq!(f.authority("lead").reports_to, "ceo");
}

#[tokio::test]
async fn raw_editor_move_updates_the_authority_then_the_mirror() {
    let f = fixture().await;
    ok(&raw_set_reports_to(&f, "rep", "lead", "other").await);
    assert_eq!(f.authority("rep").reports_to, "other");
    assert_eq!(f.mirror("rep").reports_to, "other");
    // The lock was released: another org writer is not stalled.
    ok(&f.call("agents.update", json!({ "agent_id": "rep", "reports_to": "lead" }), &f.admin).await);
    assert_eq!(f.authority("rep").reports_to, "lead");
}

// ── org.propose_change ───────────────────────────────────────────────────────

#[tokio::test]
async fn a_manager_proposal_files_one_approval_and_moves_nothing() {
    let f = fixture().await;
    let id = f.propose().await;
    let rec = ApprovalBroker::open(&f.home).unwrap().get(&ApprovalId::from(id.clone())).await.unwrap().unwrap();
    assert_eq!(rec.action_kind, crate::org_change_approval::ACTION_KIND);
    assert!(crate::approval_notify::is_dashboard_only_kind(&rec.action_kind));
    assert_eq!(rec.agent_id, "rep");
    assert_eq!(rec.payload["before"], json!({ "reports_to": "lead", "department": "業務" }));
    assert_eq!(rec.payload["after"], json!({ "reports_to": "other", "department": "業務" }));
    assert_eq!(rec.payload["proposer"], "mgr-1");
    assert_eq!(rec.payload["reason"], "改由 other 帶");
    assert_eq!(rec.payload["base"], duduclaw_core::org_change::entry_hash(&f.home, "rep"));
    // Nothing applied yet.
    assert_eq!(f.authority("rep").reports_to, "lead");
    assert_eq!(f.mirror("rep").reports_to, "lead");
    assert_eq!(audit_rows(&f.home, "org_change_requested").len(), 1);

    // One waiting proposal per employee.
    let (code, msg) = refusal(&f.call("org.propose_change", json!({ "agent_id": "rep", "department": "經營" }), &f.mgr).await);
    assert_eq!(code, "pending_exists", "{msg}");

    // Both the proposer and an Admin see it; an unrelated Manager does not.
    let mine = ok(&f.call("org.pending_changes", json!({}), &f.mgr).await);
    assert_eq!(mine["count"], 1);
    assert_eq!(mine["pending"][0]["approval_id"], json!(id));
    assert_eq!(mine["pending"][0]["stale"], false);
    assert_eq!(ok(&f.call("org.pending_changes", json!({ "agent_id": "rep" }), &f.admin).await)["count"], 1);
    assert_eq!(ok(&f.call("org.pending_changes", json!({}), &f.mgr2).await)["count"], 0);
}

#[tokio::test]
async fn viewers_outsiders_and_bad_targets_are_refused() {
    let f = fixture().await;
    // Below Manager.
    let (_, e) = refusal(&f.call("org.propose_change", json!({ "agent_id": "rep", "reports_to": "other" }), &f.viewer).await);
    assert!(e.contains("permission"), "{e}");
    assert!(refusal(&f.call("org.pending_changes", json!({}), &f.viewer).await).1.contains("permission"));
    assert!(refusal(&f.call("org.placement", json!({ "agent_id": "rep" }), &f.viewer).await).1.contains("permission"));
    assert!(refusal(&f.call("org.placement", json!({ "agent_id": "rep" }), &f.mgr2).await).1.contains("permission"));
    assert!(refusal(&f.call("org.apply_change", json!({ "agent_id": "rep", "reports_to": "other", "expected_base": "x" }), &f.viewer).await).1.contains("permission"));
    // A Manager without access to the subject…
    let (_, e) = refusal(&f.call("org.propose_change", json!({ "agent_id": "rep", "reports_to": "other" }), &f.mgr2).await);
    assert!(e.contains("permission"), "{e}");
    // …or to the new supervisor.
    let (_, e) = refusal(&f.call("org.propose_change", json!({ "agent_id": "rep", "reports_to": "ceo" }), &f.mgr).await);
    assert!(e.contains("permission"), "{e}");
    // Root placement is the Admin's call.
    let (code, _) = refusal(&f.call("org.propose_change", json!({ "agent_id": "rep", "reports_to": "" }), &f.mgr).await);
    assert_eq!(code, "root_requires_admin");
    // The shared validation runs for proposals too.
    let (code, _) = refusal(&f.call("org.propose_change", json!({ "agent_id": "lead", "reports_to": "rep" }), &f.mgr).await);
    assert_eq!(code, "cycle");
    let (code, _) = refusal(&f.call("org.propose_change", json!({ "agent_id": "rep", "department": "不存在的部門" }), &f.mgr).await);
    assert_eq!(code, "unknown_department");
    let (code, _) = refusal(&f.call("org.propose_change", json!({ "agent_id": "rep", "reports_to": "lead" }), &f.mgr).await);
    assert_eq!(code, "no_change");
    // A Manager may not apply directly.
    let base = duduclaw_core::org_change::entry_hash(&f.home, "rep");
    let (_, e) = refusal(&f.call("org.apply_change", json!({ "agent_id": "rep", "reports_to": "other", "expected_base": base }), &f.mgr).await);
    assert!(e.contains("permission"), "{e}");
    assert!(ApprovalBroker::open(&f.home).unwrap().list_by_kind("org_change").await.unwrap().is_empty());
    assert_eq!(f.authority("rep").reports_to, "lead");
}

// ── org.apply_change ─────────────────────────────────────────────────────────

#[tokio::test]
async fn an_admin_applies_with_a_fresh_base_and_a_stale_base_is_refused() {
    let f = fixture().await;
    let stale = "0".repeat(64);
    let (code, _) = refusal(&f
        .call("org.apply_change", json!({ "agent_id": "rep", "reports_to": "other", "expected_base": stale }), &f.admin)
        .await);
    assert_eq!(code, "stale_base");
    assert_eq!(f.authority("rep").reports_to, "lead");
    assert!(audit_rows(&f.home, "org_change_applied").is_empty());

    // The confirmation dialog reads the authoritative before-state + base.
    let placement = ok(&f.call("org.placement", json!({ "agent_id": "rep" }), &f.admin).await);
    assert_eq!(placement["placement"], json!({ "reports_to": "lead", "department": "業務" }));
    assert_eq!(placement["source"], "authority");
    assert_eq!(placement["mirror_drift"], false);
    let base = placement["base"].as_str().unwrap().to_string();
    assert_eq!(base, duduclaw_core::org_change::entry_hash(&f.home, "rep"));
    // Validation applies to the Admin as well.
    let (code, _) = refusal(&f
        .call("org.apply_change", json!({ "agent_id": "lead", "reports_to": "rep", "expected_base": duduclaw_core::org_change::entry_hash(&f.home, "lead") }), &f.admin)
        .await);
    assert_eq!(code, "cycle");

    let p = ok(&f
        .call("org.apply_change", json!({ "agent_id": "rep", "reports_to": "other", "department": "經營", "expected_base": base }), &f.admin)
        .await);
    assert_eq!(p["status"], "applied");
    assert_eq!(p["before"], json!({ "reports_to": "lead", "department": "業務" }));
    assert_eq!(p["after"], json!({ "reports_to": "other", "department": "經營" }));
    assert_ne!(p["base"], json!(base));
    assert_eq!(f.authority("rep"), OrgEntry::new("other", "經營"));
    assert_eq!(f.mirror("rep"), OrgEntry::new("other", "經營"));
    let rows = audit_rows(&f.home, "org_change_applied");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["details"]["actor"], "admin-1");
    assert_eq!(rows[0]["details"]["before"]["reports_to"], "lead");
    assert_eq!(rows[0]["details"]["after"]["department"], "經營");
    // The base it was made against is now stale.
    let (code, _) = refusal(&f
        .call("org.apply_change", json!({ "agent_id": "rep", "reports_to": "lead", "expected_base": base }), &f.admin)
        .await);
    assert_eq!(code, "stale_base");
}

#[tokio::test]
async fn an_admin_apply_withdraws_a_proposal_made_against_the_old_placement() {
    let f = fixture().await;
    let id = f.propose().await;
    let base = duduclaw_core::org_change::entry_hash(&f.home, "rep");
    let p = ok(&f
        .call("org.apply_change", json!({ "agent_id": "rep", "department": "經營", "expected_base": base }), &f.admin)
        .await);
    assert_eq!(p["withdrawn_pending"], json!([id]));
    assert_ne!(f.status(&id).await, ApprovalStatus::Pending);
}

// ── approvals.decide for org_change ──────────────────────────────────────────

#[tokio::test]
async fn approval_applies_and_audits_and_only_an_admin_decides() {
    let f = fixture().await;
    let id = f.propose().await;
    // The proposing Manager cannot decide it.
    let (_, e) = refusal(&f.call("approvals.decide", json!({ "id": id, "approve": true }), &f.mgr).await);
    assert!(!e.is_empty());
    assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
    assert_eq!(f.authority("rep").reports_to, "lead");

    let d = ok(&f.call("approvals.decide", json!({ "id": id, "approve": true }), &f.admin).await);
    assert_eq!(d["decided"], "approved", "{d}");
    assert_eq!(d["side_effect"]["applied"], true);
    assert_eq!(f.status(&id).await, ApprovalStatus::Approved);
    assert_eq!(f.authority("rep"), OrgEntry::new("other", "業務"));
    assert_eq!(f.mirror("rep"), OrgEntry::new("other", "業務"));
    let rows = audit_rows(&f.home, "org_change_applied");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["details"]["approved_by"], "admin-1");
    assert_eq!(rows[0]["details"]["proposer"], "mgr-1");
    assert_eq!(rows[0]["details"]["approval_id"], json!(id));
    // Decided cards drop out of the pending list.
    assert_eq!(ok(&f.call("org.pending_changes", json!({}), &f.admin).await)["count"], 0);
}

#[tokio::test]
async fn rejection_leaves_the_org_chart_as_it_was() {
    let f = fixture().await;
    let id = f.propose().await;
    let before = std::fs::read_to_string(f.home.join("org.toml")).unwrap();
    let d = ok(&f.call("approvals.decide", json!({ "id": id, "approve": false, "reason": "先不要" }), &f.admin).await);
    assert_eq!(d["decided"], "denied");
    assert_eq!(f.status(&id).await, ApprovalStatus::Denied);
    assert_eq!(std::fs::read_to_string(f.home.join("org.toml")).unwrap(), before);
    assert_eq!(f.mirror("rep").reports_to, "lead");
    let rows = audit_rows(&f.home, "org_change_rejected");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["details"]["why"], "rejected");
    assert_eq!(rows[0]["details"]["proposer"], "mgr-1");
    assert!(audit_rows(&f.home, "org_change_applied").is_empty());
    // The employee may be proposed again once the first request ended.
    f.propose().await;
}

#[tokio::test]
async fn expiry_is_a_denial() {
    let f = fixture().await;
    let id = f.propose().await;
    rusqlite::Connection::open(f.home.join("approvals.db"))
        .unwrap()
        .execute("UPDATE approvals SET created_at='2000-01-01T00:00:00Z' WHERE id=?1", [id.as_str()])
        .unwrap();
    let (_, e) = refusal(&f.call("approvals.decide", json!({ "id": id, "approve": true }), &f.admin).await);
    assert_eq!(e, crate::org_change_approval::EXPIRED_TEXT);
    assert_ne!(f.status(&id).await, ApprovalStatus::Approved);
    assert_eq!(f.authority("rep").reports_to, "lead");
    assert_eq!(f.mirror("rep").reports_to, "lead");
    assert!(audit_rows(&f.home, "org_change_applied").is_empty());
    // An expired request no longer blocks a new one.
    assert_eq!(ok(&f.call("org.pending_changes", json!({}), &f.mgr).await)["count"], 0);
    f.propose().await;
}

#[tokio::test]
async fn a_request_whose_base_moved_while_waiting_is_denied_not_applied() {
    let f = fixture().await;
    let id = f.propose().await;
    // The authority moves under the request (e.g. `duduclaw org sync`).
    f.set_authority("rep", OrgEntry::new("lead", "經營"));
    let card = ok(&f.call("org.pending_changes", json!({}), &f.admin).await);
    assert_eq!(card["pending"][0]["stale"], true);
    let (_, e) = refusal(&f.call("approvals.decide", json!({ "id": id, "approve": true }), &f.admin).await);
    assert!(e.contains("已被調整"), "{e}");
    assert_eq!(f.status(&id).await, ApprovalStatus::Denied);
    assert_eq!(f.authority("rep"), OrgEntry::new("lead", "經營"));
    let rows = audit_rows(&f.home, "org_change_rejected");
    assert_eq!(rows[0]["details"]["why"], "stale");
}

// ── Daybreak review regressions ──────────────────────────────────────────────

fn is_ok(frame: &WsFrame) -> bool {
    matches!(frame, WsFrame::Response { ok: true, .. })
}

/// Finding 1: two opposite moves validated against the same tree (lead →
/// other, other → lead) must not both commit and close a loop.
#[tokio::test]
async fn concurrent_opposite_moves_cannot_form_a_cycle() {
    let f = fixture().await;
    let lead_base = duduclaw_core::org_change::entry_hash(&f.home, "lead");
    let other_base = duduclaw_core::org_change::entry_hash(&f.home, "other");
    let (a, b) = tokio::join!(
        f.call("org.apply_change", json!({ "agent_id": "lead", "reports_to": "other", "expected_base": lead_base }), &f.admin),
        f.call("org.apply_change", json!({ "agent_id": "other", "reports_to": "lead", "expected_base": other_base }), &f.admin),
    );
    assert_eq!([&a, &b].iter().filter(|r| is_ok(r)).count(), 1, "{a:?}\n{b:?}");
    let loser = if is_ok(&a) { &b } else { &a };
    assert_eq!(refusal(loser).0, "cycle", "{loser:?}");
    let (lead, other) = (f.authority("lead").reports_to, f.authority("other").reports_to);
    assert!(!(lead == "other" && other == "lead"), "cycle in the authority: lead→{lead}, other→{other}");
}

/// Finding 1: the RPCs and `agents.update` share the one cross-process org
/// lock that `duduclaw org sync` / MCP `agent_update` take
/// (`<home>/locks/org-mutation.lock`): while another process holds it, no
/// org write commits.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn org_writers_wait_for_the_shared_cross_process_org_lock() {
    let f = fixture().await;
    let wait = std::time::Duration::from_millis(300);

    let held = org_store::lock_org_mutations(&f.home).unwrap();
    let base = duduclaw_core::org_change::entry_hash(&f.home, "rep");
    let apply = f.call("org.apply_change", json!({ "agent_id": "rep", "reports_to": "other", "expected_base": base }), &f.admin);
    tokio::pin!(apply);
    assert!(tokio::time::timeout(wait, &mut apply).await.is_err(), "org.apply_change must wait for the org lock");
    assert_eq!(f.authority("rep").reports_to, "lead");
    drop(held);
    assert_eq!(ok(&apply.await)["status"], "applied");

    let held = org_store::lock_org_mutations(&f.home).unwrap();
    let update = f.call("agents.update", json!({ "agent_id": "lead", "reports_to": "other" }), &f.admin);
    tokio::pin!(update);
    assert!(tokio::time::timeout(wait, &mut update).await.is_err(), "agents.update org fields must wait for the org lock");
    assert_eq!(f.authority("lead").reports_to, "ceo");
    assert_eq!(f.mirror("lead").reports_to, "ceo");
    drop(held);
    ok(&update.await);
    assert_eq!(f.authority("lead").reports_to, "other");
    assert_eq!(f.mirror("lead").reports_to, "other");

    // A field outside the org pair does not take the org lock.
    let held = org_store::lock_org_mutations(&f.home).unwrap();
    let icon = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        f.call("agents.update", json!({ "agent_id": "rep", "icon": "🧭" }), &f.admin),
    )
    .await
    .expect("a non-org save must not wait for the org lock");
    ok(&icon);
    drop(held);
}

/// Finding 2: a seeded authority that is corrupt, of a newer schema, or lost
/// refuses every org write (no mirror fallback, no rebuild from an empty
/// store); read-only display and non-org saves keep working.
#[tokio::test]
async fn an_unusable_authority_refuses_org_writes_but_display_stays_up() {
    let f = fixture().await;
    let id = f.propose().await;
    let base = duduclaw_core::org_change::entry_hash(&f.home, "rep");
    let store_path = org_store::org_store_path(&f.home);
    for broken in ["[[ this is not toml", "schema = 99\n[agents.rep]\nreports_to = \"ceo\"\n"] {
        std::fs::write(&store_path, broken).unwrap();
        let (code, msg) = refusal(&f
            .call("org.apply_change", json!({ "agent_id": "rep", "reports_to": "other", "expected_base": base }), &f.admin)
            .await);
        assert_eq!(code, "org_authority_unavailable", "{msg}");
        let (code, _) = refusal(&f.call("org.propose_change", json!({ "agent_id": "lead", "department": "經營" }), &f.admin).await);
        assert_eq!(code, "org_authority_unavailable");
        let (code, _) = refusal(&f.call("approvals.decide", json!({ "id": id, "approve": true }), &f.admin).await);
        assert_eq!(code, "org_authority_unavailable");
        assert_eq!(f.status(&id).await, ApprovalStatus::Pending, "an authority problem is not the request's fault");
        let (_, e) = refusal(&f.call("agents.update", json!({ "agent_id": "rep", "reports_to": "other" }), &f.admin).await);
        assert!(e.contains("org.toml"), "{e}");
        let (_, e) = refusal(&f.call("agents.update", json!({ "agent_id": "rep", "department": "經營" }), &f.admin).await);
        assert!(e.contains("org.toml"), "{e}");
        // Nothing was rebuilt or moved.
        assert_eq!(std::fs::read_to_string(&store_path).unwrap(), broken);
        assert_eq!(f.mirror("rep"), OrgEntry::new("lead", "業務"));
        // Re-sending the current value (an autosave) and non-org fields still save.
        ok(&f.call("agents.update", json!({ "agent_id": "rep", "reports_to": "lead", "icon": "📌" }), &f.admin).await);
        // Read-only display is tolerant and says why writes refuse.
        let placement = ok(&f.call("org.placement", json!({ "agent_id": "rep" }), &f.admin).await);
        assert!(placement["authority_error"].is_string(), "{placement}");
        assert_eq!(ok(&f.call("org.pending_changes", json!({}), &f.admin).await)["count"], 1);
    }
    // Lost on a bootstrapped home: refused, and the file is not recreated.
    std::fs::remove_file(&store_path).unwrap();
    assert!(org_store::store_lost(&f.home));
    let (code, _) = refusal(&f
        .call("org.apply_change", json!({ "agent_id": "rep", "reports_to": "other", "expected_base": base }), &f.admin)
        .await);
    assert_eq!(code, "org_authority_unavailable");
    let (_, e) = refusal(&f.call("agents.update", json!({ "agent_id": "rep", "reports_to": "other" }), &f.admin).await);
    assert!(e.contains("org sync"), "{e}");
    assert!(!store_path.exists());
    assert_eq!(f.mirror("rep").reports_to, "lead");
}

/// Finding 3: the authority commits first; a mirror that cannot follow is
/// recorded as drift and reported, and re-applying the same placement
/// re-syncs it. `agents.update` never leaves the authority moved behind a
/// refused mirror write.
#[tokio::test]
async fn the_authority_commits_first_and_a_failed_mirror_is_drift_not_silence() {
    let f = fixture().await;
    let path = f.home.join("agents/rep/agent.toml");
    let original = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, "[agent\nthis does not parse").unwrap();
    let base = duduclaw_core::org_change::entry_hash(&f.home, "rep");
    let (code, msg) = refusal(&f
        .call("org.apply_change", json!({ "agent_id": "rep", "reports_to": "other", "expected_base": base }), &f.admin)
        .await);
    assert_eq!(code, "mirror_sync_failed", "{msg}");
    assert!(msg.contains("已經生效") && msg.contains("org sync"), "{msg}");
    assert_eq!(f.authority("rep"), OrgEntry::new("other", "業務"), "the authority is committed first");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "[agent\nthis does not parse");
    let drift = audit_rows(&f.home, "org_change_mirror_drift");
    assert_eq!(drift.len(), 1, "{drift:?}");
    assert_eq!(drift[0]["details"]["authority"]["reports_to"], "other");
    assert_eq!(audit_rows(&f.home, "org_change_applied").len(), 1);

    // The mirror is repaired (still showing the old placement); applying the
    // same placement again brings it along.
    std::fs::write(&path, &original).unwrap();
    assert_eq!(org_store::detect_drift(&f.home).len(), 1);
    let base = duduclaw_core::org_change::entry_hash(&f.home, "rep");
    let p = ok(&f
        .call("org.apply_change", json!({ "agent_id": "rep", "reports_to": "other", "expected_base": base }), &f.admin)
        .await);
    assert_eq!(p["status"], "mirror_synced", "{p}");
    assert_eq!(f.mirror("rep"), OrgEntry::new("other", "業務"));
    assert!(org_store::detect_drift(&f.home).is_empty());

    // agents.update: the mirror write is refused (bad role) → the
    // pre-committed authority entry is put back.
    let (_, e) = refusal(&f
        .call("agents.update", json!({ "agent_id": "lead", "reports_to": "other", "role": "not-a-role" }), &f.admin)
        .await);
    assert!(e.contains("Invalid role"), "{e}");
    assert_eq!(f.authority("lead"), OrgEntry::new("ceo", "業務"));
    assert_eq!(f.mirror("lead"), OrgEntry::new("ceo", "業務"));
}

/// Finding 4: when recording the decision fails after the apply, the card
/// stays pending, no terminal audit is written, a denial is refused (it
/// would lie), and the retry completes the ORIGINAL approval instead of
/// denying it as stale.
#[tokio::test]
async fn an_unrecorded_decision_after_the_apply_completes_on_retry() {
    use crate::handlers::org_change_rpc::decide_failpoint;
    let f = fixture().await;
    let id = f.propose().await;
    decide_failpoint::arm(&id);
    let (code, msg) = refusal(&f.call("approvals.decide", json!({ "id": id, "approve": true }), &f.admin).await);
    assert_eq!(code, "decision_unrecorded", "{msg}");
    assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
    assert_eq!(f.authority("rep"), OrgEntry::new("other", "業務"), "the apply happened");
    assert!(crate::org_change_approval::journal::exists(&f.home, &id));
    assert!(audit_rows(&f.home, "org_change_applied").is_empty(), "no terminal audit before the decision is recorded");
    assert!(audit_rows(&f.home, "org_change_rejected").is_empty());
    assert_eq!(audit_rows(&f.home, "org_change_decision_unrecorded").len(), 1);
    decide_failpoint::disarm(&id);

    // A denial of an applied card would record a lie: refused.
    let (code, _) = refusal(&f.call("approvals.decide", json!({ "id": id, "approve": false }), &f.admin).await);
    assert_eq!(code, "already_applied");
    assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
    // An Admin apply on the same employee does not withdraw the journaled card.
    let base = duduclaw_core::org_change::entry_hash(&f.home, "rep");
    let p = ok(&f.call("org.apply_change", json!({ "agent_id": "rep", "department": "經營", "expected_base": base }), &f.admin).await);
    assert_eq!(p["withdrawn_pending"], json!([]), "{p}");
    assert_eq!(f.status(&id).await, ApprovalStatus::Pending);

    // The retry completes the original approval — not "stale" — and does
    // not re-apply over the later change.
    let d = ok(&f.call("approvals.decide", json!({ "id": id, "approve": true }), &f.admin).await);
    assert_eq!(d["decided"], "approved", "{d}");
    assert_eq!(f.status(&id).await, ApprovalStatus::Approved);
    assert_eq!(f.authority("rep"), OrgEntry::new("other", "經營"));
    let applied: Vec<_> = audit_rows(&f.home, "org_change_applied")
        .into_iter()
        .filter(|r| r["details"]["approval_id"] == json!(id))
        .collect();
    assert_eq!(applied.len(), 1, "{applied:?}");
    assert_eq!(applied[0]["details"]["approved_by"], "admin-1");
    assert!(audit_rows(&f.home, "org_change_rejected").is_empty());
    assert!(!crate::org_change_approval::journal::exists(&f.home, &id));
}

/// Finding 4: a stale card whose denial cannot be recorded is not audited as
/// rejected nor collapsed; it stays pending and the retry denies it.
#[tokio::test]
async fn a_denial_that_fails_to_record_is_not_audited_as_rejected() {
    use crate::handlers::org_change_rpc::decide_failpoint;
    let f = fixture().await;
    let id = f.propose().await;
    f.set_authority("rep", OrgEntry::new("lead", "經營"));
    decide_failpoint::arm(&id);
    let (_, e) = refusal(&f.call("approvals.decide", json!({ "id": id, "approve": true }), &f.admin).await);
    assert!(e.contains("仍待決定"), "{e}");
    assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
    assert!(audit_rows(&f.home, "org_change_rejected").is_empty());
    decide_failpoint::disarm(&id);
    let (_, e) = refusal(&f.call("approvals.decide", json!({ "id": id, "approve": true }), &f.admin).await);
    assert!(e.contains("已被調整"), "{e}");
    assert_eq!(f.status(&id).await, ApprovalStatus::Denied);
    assert_eq!(audit_rows(&f.home, "org_change_rejected").len(), 1);
}

/// Finding 5: a Manager whose only link to the employee is a Viewer binding
/// sees nothing of a waiting proposal (proposer email, free-text reason).
#[tokio::test]
async fn a_viewer_binding_does_not_reveal_pending_org_changes() {
    let f = fixture().await;
    f.propose().await;
    let seen = ok(&f.call("org.pending_changes", json!({}), &f.viewer_mgr).await);
    assert_eq!(seen["count"], 0, "{seen}");
    assert_eq!(seen["pending"], json!([]));
    let seen = ok(&f.call("org.pending_changes", json!({ "agent_id": "rep" }), &f.viewer_mgr).await);
    assert_eq!(seen["count"], 0, "{seen}");
    // The proposer and an Admin still see it.
    assert_eq!(ok(&f.call("org.pending_changes", json!({}), &f.mgr).await)["count"], 1);
    assert_eq!(ok(&f.call("org.pending_changes", json!({}), &f.admin).await)["count"], 1);
}

/// Finding 6: proposals carry the proposer as `on_behalf_of`, so
/// `[approval] forbid_self_approval` stops an Admin approving their own
/// proposal — also for a card without the stamp (payload check). Rejecting
/// one's own proposal stays possible.
#[tokio::test]
async fn forbid_self_approval_covers_org_change_proposals() {
    let f = fixture().await;
    let mgr_card = f.propose().await;
    let rec = ApprovalBroker::open(&f.home).unwrap().get(&ApprovalId::from(mgr_card.clone())).await.unwrap().unwrap();
    assert_eq!(rec.on_behalf_of.as_deref(), Some("user:mgr-1"));

    {
        use std::io::Write;
        let mut cfg = std::fs::OpenOptions::new().append(true).open(f.home.join("config.toml")).unwrap();
        cfg.write_all(b"\n[approval]\nforbid_self_approval = true\n").unwrap();
    }
    let own = ok(&f.call("org.propose_change", json!({ "agent_id": "lead", "department": "經營" }), &f.admin).await);
    let own = own["approval_id"].as_str().unwrap().to_string();
    let rec = ApprovalBroker::open(&f.home).unwrap().get(&ApprovalId::from(own.clone())).await.unwrap().unwrap();
    assert_eq!(rec.on_behalf_of.as_deref(), Some("user:admin-1"));

    let (_, e) = refusal(&f.call("approvals.decide", json!({ "id": own, "approve": true }), &f.admin).await);
    assert!(!e.is_empty());
    assert_eq!(f.status(&own).await, ApprovalStatus::Pending);
    assert_eq!(f.authority("lead").department, "業務");
    // Without the stamp the payload's proposer still decides it.
    rusqlite::Connection::open(f.home.join("approvals.db"))
        .unwrap()
        .execute("UPDATE approvals SET on_behalf_of=NULL WHERE id=?1", [own.as_str()])
        .unwrap();
    let (_, e) = refusal(&f.call("approvals.decide", json!({ "id": own, "approve": true }), &f.admin).await);
    assert!(e.contains("自己提出"), "{e}");
    assert_eq!(f.status(&own).await, ApprovalStatus::Pending);
    assert_eq!(f.authority("lead").department, "業務");
    // Self-rejection is allowed.
    assert_eq!(ok(&f.call("approvals.decide", json!({ "id": own, "approve": false }), &f.admin).await)["decided"], "denied");
    // Someone else's proposal is approved normally.
    assert_eq!(ok(&f.call("approvals.decide", json!({ "id": mgr_card, "approve": true }), &f.admin).await)["decided"], "approved");
}

// ── Daybreak review round 3 regressions ─────────────────────────────────────

/// Round 3, finding 1: agent creation (a gated authority writer outside the
/// `org.*` RPCs) waits for the shared org lock too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_creation_waits_for_the_shared_org_lock() {
    let f = fixture().await;
    let held = org_store::lock_org_mutations(&f.home).unwrap();
    let create = f.h.handle_agents_create_as(json!({ "name": "late", "reports_to": "ceo", "department": "經營" }), Some(&f.admin));
    tokio::pin!(create);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(300), &mut create).await.is_err(),
        "agents.create must wait for the org mutation lock before recording the authority"
    );
    assert!(org_store::load(&f.home).get("late").is_none());
    drop(held);
    ok(&create.await);
    assert_eq!(f.authority("late"), OrgEntry::new("ceo", "經營"));
}

/// Round 3, finding 2: an employee tampers with its mirror's `reports_to`;
/// an Admin then changes only the department. The tampered value must not
/// be promoted into the authority — not when the field is left out, and not
/// when the form echoes the (tampered) prefilled value back.
#[tokio::test]
async fn agents_update_never_promotes_a_tampered_mirror_field() {
    let f = fixture().await;
    f.tamper_mirror("rep", "reports_to = \"lead\"", "reports_to = \"ceo\"");
    assert_eq!(f.mirror("rep").reports_to, "ceo");
    assert_eq!(f.authority("rep").reports_to, "lead");

    ok(&f.call("agents.update", json!({ "agent_id": "rep", "department": "經營" }), &f.admin).await);
    assert_eq!(f.authority("rep"), OrgEntry::new("lead", "經營"), "the tampered reports_to must not reach the authority");
    assert_eq!(f.mirror("rep"), OrgEntry::new("lead", "經營"), "the mirror follows the authority");

    f.tamper_mirror("rep", "reports_to = \"lead\"", "reports_to = \"ceo\"");
    ok(&f
        .call("agents.update", json!({ "agent_id": "rep", "reports_to": "ceo", "department": "業務" }), &f.admin)
        .await);
    assert_eq!(f.authority("rep"), OrgEntry::new("lead", "業務"), "an echoed prefilled value is not a request");
    assert_eq!(f.mirror("rep"), OrgEntry::new("lead", "業務"));
    // An explicit move is still a move.
    ok(&f.call("agents.update", json!({ "agent_id": "rep", "reports_to": "other" }), &f.admin).await);
    assert_eq!(f.authority("rep"), OrgEntry::new("other", "業務"));
}

/// Round 3, finding 2: the raw editor compares both org fields with the
/// authority; a drifted field the admin did not change refuses the save
/// (explicit drift resolution), and nothing moves.
#[tokio::test]
async fn raw_editor_refuses_to_carry_a_drifted_org_field_into_the_authority() {
    let f = fixture().await;
    f.tamper_mirror("rep", "reports_to = \"lead\"", "reports_to = \"ceo\"");
    let org_before = std::fs::read_to_string(f.home.join("org.toml")).unwrap();
    let raw = ok(&f.call("config.raw.get", json!({ "file": "agent:rep" }), &f.admin).await);
    let content = raw["content"].as_str().unwrap().to_string();
    assert!(content.contains("department = \"業務\""), "{content}");
    let edited = content.replace("department = \"業務\"", "department = \"經營\"");
    let (_, e) = refusal(&f
        .call("config.raw.set", json!({ "file": "agent:rep", "content": edited, "base_hash": raw["hash"] }), &f.admin)
        .await);
    assert!(e.contains("不一致") && e.contains("org sync") && e.contains("reports_to"), "{e}");
    assert_eq!(std::fs::read_to_string(f.home.join("org.toml")).unwrap(), org_before);
    assert_eq!(f.mirror("rep"), OrgEntry::new("ceo", "業務"), "the refused raw save wrote nothing");

    // Resolving the drift in the same edit is an explicit choice: saved.
    let fixed = edited.replace("reports_to = \"ceo\"", "reports_to = \"lead\"");
    ok(&f
        .call("config.raw.set", json!({ "file": "agent:rep", "content": fixed, "base_hash": raw["hash"] }), &f.admin)
        .await);
    assert_eq!(f.authority("rep"), OrgEntry::new("lead", "經營"));
    assert_eq!(f.mirror("rep"), OrgEntry::new("lead", "經營"));
}

/// Round 3, finding 4: the prepared journal entry is written BEFORE the
/// authority commit — if it cannot be written, nothing is applied and the
/// card stays pending.
#[tokio::test]
async fn no_prepared_journal_means_no_apply() {
    use crate::org_change_approval::journal::{self, record_failpoint, State};
    let f = fixture().await;
    let id = f.propose().await;
    record_failpoint::arm(&id, State::Prepared);
    let (code, msg) = refusal(&f.call("approvals.decide", json!({ "id": id, "approve": true }), &f.admin).await);
    record_failpoint::disarm(&id, State::Prepared);
    assert_eq!(code, "journal_unavailable", "{msg}");
    assert_eq!(f.authority("rep"), OrgEntry::new("lead", "業務"));
    assert_eq!(f.mirror("rep"), OrgEntry::new("lead", "業務"));
    assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
    assert!(!journal::exists(&f.home, &id));
    assert!(audit_rows(&f.home, "org_change_applied").is_empty());
    // The lease was given back: the retry applies normally.
    let d = ok(&f.call("approvals.decide", json!({ "id": id, "approve": true }), &f.admin).await);
    assert_eq!(d["decided"], "approved", "{d}");
    assert_eq!(f.authority("rep"), OrgEntry::new("other", "業務"));
    assert!(!journal::exists(&f.home, &id));
}

/// Round 3, finding 4: a prepared entry left by an attempt that stopped
/// around the commit is settled against the authority under the org lock —
/// committed ⇒ the original approval completes without re-applying; not
/// committed ⇒ the request is judged (and applied) normally.
#[tokio::test]
async fn a_prepared_journal_entry_is_settled_against_the_authority() {
    use crate::handlers::org_change_rpc::decide_failpoint;
    use crate::org_change_approval::journal::{self, record_failpoint, State};

    // (a) The commit landed, but neither "applied" nor the decision did.
    let f = fixture().await;
    let id = f.propose().await;
    record_failpoint::arm(&id, State::Applied);
    decide_failpoint::arm(&id);
    let (code, _) = refusal(&f.call("approvals.decide", json!({ "id": id, "approve": true }), &f.admin).await);
    record_failpoint::disarm(&id, State::Applied);
    decide_failpoint::disarm(&id);
    assert_eq!(code, "decision_unrecorded");
    assert_eq!(f.authority("rep"), OrgEntry::new("other", "業務"));
    assert_eq!(journal::read(&f.home, &id).unwrap().unwrap().state, State::Prepared);
    // A denial would lie about a live change.
    let (code, _) = refusal(&f.call("approvals.decide", json!({ "id": id, "approve": false }), &f.admin).await);
    assert_eq!(code, "already_applied");
    let d = ok(&f.call("approvals.decide", json!({ "id": id, "approve": true }), &f.admin).await);
    assert_eq!(d["decided"], "approved", "{d}");
    assert_eq!(f.status(&id).await, ApprovalStatus::Approved);
    assert_eq!(audit_rows(&f.home, "org_change_applied").len(), 1);
    assert!(!journal::exists(&f.home, &id));

    // (b) A prepared entry whose commit never happened (crash before it).
    let g = fixture().await;
    let id = g.propose().await;
    let rec = ApprovalBroker::open(&g.home).unwrap().get(&ApprovalId::from(id.clone())).await.unwrap().unwrap();
    let prepared = journal::Applied::prepared(
        &rec,
        "rep",
        &OrgEntry::new("lead", "業務"),
        &OrgEntry::new("other", "業務"),
        "admin-1",
    );
    journal::record(&g.home, &prepared).unwrap();
    let d = ok(&g.call("approvals.decide", json!({ "id": id, "approve": true }), &g.admin).await);
    assert_eq!(d["decided"], "approved", "{d}");
    assert_eq!(g.authority("rep"), OrgEntry::new("other", "業務"));
    assert!(!journal::exists(&g.home, &id));

    // (c) An entry written for a different payload is refused, nothing moves.
    let h = fixture().await;
    let id = h.propose().await;
    let mut rec = ApprovalBroker::open(&h.home).unwrap().get(&ApprovalId::from(id.clone())).await.unwrap().unwrap();
    rec.payload["reason"] = json!("something else");
    let foreign = journal::Applied::prepared(&rec, "rep", &OrgEntry::new("lead", "業務"), &OrgEntry::new("ceo", ""), "admin-1");
    journal::record(&h.home, &foreign).unwrap();
    let (code, _) = refusal(&h.call("approvals.decide", json!({ "id": id, "approve": true }), &h.admin).await);
    assert_eq!(code, "journal_mismatch");
    assert_eq!(h.authority("rep"), OrgEntry::new("lead", "業務"));
    assert_eq!(h.status(&id).await, ApprovalStatus::Pending);
}

/// Round 3, finding 5: a near-expiry approval that waits for the org lock
/// holds a decision lease (the TTL sweep cannot expire it underneath), is
/// re-read once the lock is held, and — having expired meanwhile — is NOT
/// applied; it is recorded as expired.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_approval_that_expires_while_waiting_for_the_org_lock_is_not_applied() {
    let f = fixture().await;
    let id = f.propose().await;
    f.expire_in(&id, 2);
    let held = org_store::lock_org_mutations(&f.home).unwrap();
    let decide = f.call("approvals.decide", json!({ "id": id, "approve": true }), &f.admin);
    tokio::pin!(decide);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(500), &mut decide).await.is_err(),
        "the decision must wait for the org lock"
    );
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    // Past the TTL now — but the lease keeps the sweep (and other deciders) off.
    let broker = ApprovalBroker::open(&f.home).unwrap();
    assert_eq!(broker.expire_stale().await.unwrap(), 0);
    assert_eq!(f.status(&id).await, ApprovalStatus::Pending);
    assert!(!broker.withdraw(&ApprovalId::from(id.clone()), "test").await.unwrap(), "withdraw must not race the lease");
    drop(held);
    let (_, e) = refusal(&decide.await);
    assert_eq!(e, crate::org_change_approval::EXPIRED_TEXT);
    assert_eq!(f.status(&id).await, ApprovalStatus::Expired);
    assert_eq!(f.authority("rep"), OrgEntry::new("lead", "業務"));
    assert_eq!(f.mirror("rep"), OrgEntry::new("lead", "業務"));
    assert!(audit_rows(&f.home, "org_change_applied").is_empty());
    assert_eq!(audit_rows(&f.home, "org_change_rejected")[0]["details"]["why"], "expired");
    assert!(!crate::org_change_approval::journal::exists(&f.home, &id));
}

/// Round 3, finding 5: an approval decided inside its TTL completes its
/// effect and terminal state under the lease (no lease left behind).
#[tokio::test]
async fn an_approval_in_time_applies_under_the_lease_and_releases_it() {
    let f = fixture().await;
    let id = f.propose().await;
    f.expire_in(&id, 60);
    let d = ok(&f.call("approvals.decide", json!({ "id": id, "approve": true }), &f.admin).await);
    assert_eq!(d["decided"], "approved", "{d}");
    assert_eq!(f.authority("rep"), OrgEntry::new("other", "業務"));
    let lease: Option<String> = rusqlite::Connection::open(f.home.join("approvals.db"))
        .unwrap()
        .query_row("SELECT lease_token FROM approvals WHERE id=?1", [id.as_str()], |r| r.get(0))
        .unwrap();
    assert!(lease.is_none(), "the lease is cleared with the decision");
}
