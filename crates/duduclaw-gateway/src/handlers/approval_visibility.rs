//! One read policy for approval RPCs and live invalidations. Disk-backed
//! resource facts are loaded once per frame; reader identity is never cached.
use super::*;
use crate::approval::ApprovalRecord;
use crate::review_evidence::audience::{
    TaskAudience, dashboard_may_read, dashboard_may_read_task, readonly_db, task_audience,
};
use duduclaw_knowledge::acl::{AclEntry, Role};
use rusqlite::OptionalExtension;

#[derive(Clone)]
struct TaskVisibility {
    owner: Result<Option<String>, String>,
    audience: TaskAudience,
}

impl TaskVisibility {
    fn load(home: &Path, id: &str) -> Self {
        Self {
            owner: TaskStore::live_metadata(home, id).map(|r| r.map(|(owner, _)| owner)),
            audience: task_audience(home, id),
        }
    }

    fn may_read(&self, fresh: &UserContext, allow_removed_for_admin: bool) -> bool {
        let owner_allowed = match &self.owner {
            Ok(Some(owner)) => fresh.has_agent_access(owner, AccessLevel::Viewer),
            Ok(None) => allow_removed_for_admin && fresh.is_admin(),
            Err(_) => false,
        };
        owner_allowed && dashboard_may_read_task(fresh, &self.audience)
    }
}

#[derive(Clone)]
struct ActivationVisibility {
    owner: String,
    audience: Vec<String>,
    task: TaskVisibility,
}

impl ActivationVisibility {
    fn load(home: &Path, payload: &Value) -> Option<Self> {
        let request: crate::workflow::ActivationRequest =
            serde_json::from_value(payload.get("request")?.clone()).ok()?;
        if crate::workflow::activation::activation_payload(&request) != *payload {
            return None;
        }
        let db = readonly_db(&home.join("workflow.db")).ok()?;
        let raw: String = db.query_row(
            "SELECT record_json FROM workflow_drafts WHERE draft_id=?1 AND revision=?2",
            rusqlite::params![request.draft_id, request.revision.definition.revision],
            |r| r.get(0),
        ).ok()?;
        let draft: crate::workflow_drafts::WorkflowDraft = serde_json::from_str(&raw).ok()?;
        if draft.draft_hash != request.draft_hash { return None; }
        Some(Self {
            task: TaskVisibility::load(home, &draft.source_task),
            owner: draft.owner,
            audience: draft.audience,
        })
    }

    fn may_read(&self, fresh: &UserContext) -> bool {
        fresh.has_agent_access(&self.owner, AccessLevel::Viewer)
            && self.task.may_read(fresh, true)
            && dashboard_may_read(fresh, &self.audience)
    }
}

/// Who may read one approval kind. Paired with the decide branches in
/// `approvals_decide` (operator CLI, workflow activation, support pilot,
/// board kinds, agent-file change, knowledge add-source).
#[derive(Clone, Copy)]
pub(crate) enum DashboardOnlyReader {
    /// A fresh Admin (`UserContext::is_admin` from `fresh_reader`).
    Admin,
    /// Admin, or Editor/Owner on the collection ACL loaded for this frame.
    CollectionEditor,
    /// Manager with Viewer on the card's stored `agent_id`.
    EmployeeViewer,
}

/// Read policy for `record`. Dashboard-only delivery is not itself a filter;
/// the decide gate for that kind is.
pub(crate) fn dashboard_only_reader(record: &ApprovalRecord) -> DashboardOnlyReader {
    // Payload-detected activation is Admin even when `action_kind` differs.
    if crate::approval::is_activation_record(record)
        || record.action_kind == crate::approval::cli_egress::KIND
        || record.payload.get("egress_guard").is_some()
    {
        return DashboardOnlyReader::Admin;
    }
    if record.action_kind == crate::knowledge::add_source_approval::ACTION_KIND {
        return DashboardOnlyReader::CollectionEditor;
    }
    if record.action_kind == crate::agent_files::approval::ACTION_KIND
        || record.action_kind == "support_pilot_review"
        || crate::approval::operator_cli_gate::spec_for(&record.action_kind).is_some()
        || crate::governance::ApprovalKind::parse(&record.action_kind).requires_board()
    {
        return DashboardOnlyReader::Admin;
    }
    // `knowledge_quarantine` and `expert_skills_enable` have no tighter decide
    // predicate than Manager. Visibility stays employee Viewer: that is who
    // the list already admitted, and widening to every Manager is refused.
    // `expert_skills_enable` stores the pack slug in `agent_id`.
    DashboardOnlyReader::EmployeeViewer
}

/// Resource authorization facts, independent of any connection's identity.
#[derive(Clone)]
pub(crate) struct ApprovalVisibility {
    collection_acl: Option<Vec<AclEntry>>,
    activation: Option<Option<ActivationVisibility>>,
    task: Option<TaskVisibility>,
    employee_requires_admin: Option<bool>,
}

impl ApprovalVisibility {
    /// Call off the socket loop. Failure to load a required fact denies.
    pub(crate) fn load(home: &Path, record: &ApprovalRecord, fresh: &UserContext) -> Self {
        Self::load_shared(home, record).for_reader(home, record, fresh)
    }

    /// Only resource facts are shared; employee authority is loaded per reader.
    pub(crate) fn load_shared(home: &Path, record: &ApprovalRecord) -> Self {
        let collection_acl = if record.action_kind == crate::knowledge::add_source_approval::ACTION_KIND {
            record.payload.get("collection_id").and_then(Value::as_str).and_then(|id| {
                let db = readonly_db(&duduclaw_knowledge::schema::db_path(home)).ok()?;
                duduclaw_knowledge::acl::get_acl(&db, id).ok().map(|(acl, _)| acl)
            })
        } else { None };
        let activation = (record.payload.get("kind").and_then(Value::as_str) == Some("workflow_activation"))
            .then(|| ActivationVisibility::load(home, &record.payload));
        let task = approval_task_ref(&record.payload).filter(|_| record.binding.is_none())
            .map(|id| TaskVisibility::load(home, &id));
        Self { collection_acl, activation, task, employee_requires_admin: None }
    }

    pub(crate) fn for_reader(mut self, home: &Path, record: &ApprovalRecord, fresh: &UserContext) -> Self {
        self.employee_requires_admin = None;
        if matches!(dashboard_only_reader(record), DashboardOnlyReader::EmployeeViewer)
            && fresh.has_role(UserRole::Manager) && !fresh.requires_password_change()
            && fresh.has_agent_access(&record.agent_id, AccessLevel::Viewer) && !fresh.is_admin()
        {
            self.employee_requires_admin = Some(crate::machine_employee::requires_admin_for_id(home, &record.agent_id));
        }
        self
    }

    /// No I/O: both approvals.get/list and live pushes use this decision.
    /// Bound cards may be listed with their binding redacted; permission to
    /// decide is a separate gate and does not hide their metadata here.
    pub(crate) fn may_read(&self, record: &ApprovalRecord, fresh: &UserContext, department: Option<&str>) -> bool {
        if !fresh.has_role(UserRole::Manager) || fresh.requires_password_change() { return false; }
        let allowed = match dashboard_only_reader(record) {
            DashboardOnlyReader::Admin => fresh.is_admin(),
            // Same predicate as `add_source_approval::may_decide`, on this
            // frame's ACL snapshot. Admin is allowed without a grant. Everyone
            // else needs a current Editor or Owner grant. A missing snapshot
            // (collection absent or unreadable) denies non-admins. Employee
            // Viewer is not an extra requirement.
            DashboardOnlyReader::CollectionEditor => {
                fresh.is_admin()
                    || self.collection_acl.as_ref().is_some_and(|acl| {
                        duduclaw_knowledge::acl::role_from_grants(acl, &fresh.user_id, department)
                            .is_some_and(|role| role >= Role::Editor)
                    })
            }
            DashboardOnlyReader::EmployeeViewer => {
                fresh.has_agent_access(&record.agent_id, AccessLevel::Viewer)
                    && (fresh.is_admin() || self.employee_requires_admin == Some(false))
            }
        };
        if !allowed { return false; }
        if let Some(activation) = &self.activation {
            if !activation.as_ref().is_some_and(|a| a.may_read(fresh)) { return false; }
        }
        self.task.as_ref().is_none_or(|task| task.may_read(fresh, false))
    }
}

/// Refresh the identity and the department used by knowledge collection ACLs
/// together, off the socket loop. Never retain this result across frames.
pub(crate) fn fresh_reader(home: &Path, ctx: &UserContext) -> Option<(UserContext, Option<String>)> {
    let fresh = live_reader_context(home, ctx).ok()?;
    let department = if fresh.user_id == "system" { None } else {
        readonly_db(&home.join("users.db")).ok().and_then(|db| {
            db.query_row("SELECT department FROM users WHERE id=?1", [&fresh.user_id], |r| r.get::<_, Option<String>>(0))
                .optional().ok().flatten().flatten()
        }).filter(|d| duduclaw_core::department::is_valid_department(d))
    };
    Some((fresh, department))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use crate::handlers::task_privacy_tests::{admin, fixture, payload};

    #[tokio::test]
    async fn machine_employee_approval_refuses_old_viewer_binding_in_rpc_and_shared_push() {
        let f = fixture().await;
        let root = admin(&f);
        f.db.bind_agent(&f.bob.user_id, "sales", AccessLevel::Viewer).unwrap();
        let broker = crate::approval::ApprovalBroker::open(&f.home).unwrap();
        let id = broker.request("sales", "dynamic_review_kind", "Review", json!({}), 300).await.unwrap();
        let record = broker.get(&id).await.unwrap().unwrap();
        assert!(ApprovalVisibility::load(&f.home, &record, &f.bob).may_read(&record, &f.bob, None));
        duduclaw_core::preset::install_system_operator_preset(&f.home, false).unwrap();
        duduclaw_core::preset::bind(&f.home, "sales", &f.home.join("agents/sales"),
            "system-operator", "test", "test").unwrap();
        let line = serde_json::to_string(&WsFrame::event("approval.created", json!({"id":id.as_str()}))).unwrap();
        let shared = Arc::new(crate::handlers::push_filter::PreparedPush::prepare(f.home.clone(), line.clone()).await.unwrap());
        // The same frame goes to an Admin first, then the old bound Manager.
        for (ctx, expected) in [(&root, true), (&f.bob, false), (&root, true)] {
            let (fresh, department) = fresh_reader(&f.home, ctx).unwrap();
            let snapshot = ApprovalVisibility::load(&f.home, &record, &fresh);
            assert_eq!(snapshot.may_read(&record, &fresh, department.as_deref()), expected);
            let rpc = payload(f.handler.handle_approvals_list(json!({"id":id.as_str()}), ctx).await);
            assert_eq!(rpc["count"], if expected { 1 } else { 0 });
            let mut gate = crate::handlers::push_filter::PushGate::new(&f.home, ctx, false);
            assert_eq!(gate.filter_prepared(shared.clone()).await.is_some(), expected);
        }
        std::fs::remove_file(f.home.join("presets/system-operator/preset.toml")).unwrap();
        assert!(!ApprovalVisibility::load(&f.home, &record, &f.bob).may_read(&record, &f.bob, None));
        let path = duduclaw_core::preset::bindings_path(&f.home);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(!ApprovalVisibility::load(&f.home, &record, &f.bob).may_read(&record, &f.bob, None));
        assert!(ApprovalVisibility::load(&f.home, &record, &root).may_read(&record, &root, None));
        // A previously prepared frame must load this fact for each reader.
        let mut gate = crate::handlers::push_filter::PushGate::new(&f.home, &f.bob, false);
        assert!(gate.filter_prepared(shared).await.is_none());
    }

    #[tokio::test]
    async fn cli_egress_kind_or_payload_is_visible_only_to_admins() {
        let f = fixture().await;
        let root = admin(&f);
        f.db.bind_agent(&f.bob.user_id, "sales", AccessLevel::Viewer).unwrap();
        let broker = crate::approval::ApprovalBroker::open(&f.home).unwrap();
        for (kind, body) in [
            (crate::approval::cli_egress::KIND, json!({})),
            ("dynamic_kind", json!({"egress_guard":{}})),
            ("dynamic_kind", json!({"egress_guard":null})),
        ] {
            let id = broker.request("sales", kind, "Outbound review", body, 300).await.unwrap();
            let record = broker.get(&id).await.unwrap().unwrap();
            let line = serde_json::to_string(&WsFrame::event("approval.created", json!({"id":id.as_str()}))).unwrap();
            for (ctx, expected) in [(&f.bob, false), (&root, true)] {
                let (fresh, department) = fresh_reader(&f.home, ctx).unwrap();
                let snapshot = ApprovalVisibility::load(&f.home, &record, &fresh);
                assert_eq!(snapshot.may_read(&record, &fresh, department.as_deref()), expected);
                let rpc = payload(f.handler.handle_approvals_list(json!({"id":id.as_str()}), ctx).await);
                assert_eq!(rpc["count"], if expected { 1 } else { 0 });
                let mut gate = crate::handlers::push_filter::PushGate::new(&f.home, ctx, false);
                assert_eq!(gate.filter_live(&line, &f.handler).await.is_some(), expected);
            }
        }
    }

    #[tokio::test]
    async fn knowledge_visibility_matches_rpc_and_revocations() {
        let f = fixture().await;
        let db = duduclaw_knowledge::schema::open_home(&f.home).unwrap();
        db.execute("INSERT INTO collections (id,slug,name,created_by,created_at,updated_at) VALUES ('c','c','Collection','system','now','now')", []).unwrap();
        db.execute("INSERT INTO collection_acl (collection_id,principal_kind,principal_id,role,granted_by,granted_at) VALUES ('c','user',?1,'editor','system','now')", [&f.alice.user_id]).unwrap();
        let broker = crate::approval::ApprovalBroker::open(&f.home).unwrap();
        let id = broker.request("sales", crate::knowledge::add_source_approval::ACTION_KIND,
            "Review source", json!({"collection_id":"c"}), 300).await.unwrap();
        let record = broker.get(&id).await.unwrap().unwrap();
        let snapshot = ApprovalVisibility::load(&f.home, &record, &f.alice);
        let line = serde_json::to_string(&WsFrame::event("approval.created", json!({"id":id.as_str()}))).unwrap();
        for (ctx, expected) in [(&f.bob, false), (&f.alice, true), (&admin(&f), true)] {
            let (fresh, department) = fresh_reader(&f.home, ctx).unwrap();
            assert_eq!(snapshot.may_read(&record, &fresh, department.as_deref()), expected);
            let rpc = payload(f.handler.handle_approvals_list(json!({"id":id.as_str()}), ctx).await);
            assert_eq!(rpc["count"], if expected { 1 } else { 0 });
            let mut gate = crate::handlers::push_filter::PushGate::new(&f.home, ctx, false);
            assert_eq!(gate.filter_live(&line, &f.handler).await.is_some(), expected);
        }
        // Department grants use current user identity, never JWT metadata.
        db.execute("DELETE FROM collection_acl", []).unwrap();
        db.execute("INSERT INTO collection_acl (collection_id,principal_kind,principal_id,role,granted_by,granted_at) VALUES ('c','department','sales','editor','system','now')", []).unwrap();
        let users = rusqlite::Connection::open(f.home.join("users.db")).unwrap();
        users.execute("UPDATE users SET department='sales' WHERE id=?1", [&f.alice.user_id]).unwrap();
        let snapshot = ApprovalVisibility::load(&f.home, &record, &f.alice);
        let (fresh, department) = fresh_reader(&f.home, &f.alice).unwrap();
        assert!(snapshot.may_read(&record, &fresh, department.as_deref()));
        users.execute("UPDATE users SET department=NULL WHERE id=?1", [&f.alice.user_id]).unwrap();
        let (fresh, department) = fresh_reader(&f.home, &f.alice).unwrap();
        assert!(!snapshot.may_read(&record, &fresh, department.as_deref()));
        // A subsequent frame observes collection revocation.
        db.execute("DELETE FROM collection_acl", []).unwrap();
        assert!(!ApprovalVisibility::load(&f.home, &record, &f.alice).may_read(&record, &f.alice, Some("sales")));
        // An already prepared frame never retains the reader's role/status.
        db.execute("INSERT INTO collection_acl (collection_id,principal_kind,principal_id,role,granted_by,granted_at) VALUES ('c','user',?1,'editor','system','now')", [&f.alice.user_id]).unwrap();
        let frame = crate::handlers::push_filter::PreparedPush::prepare(f.home.clone(), line.clone()).await.unwrap();
        let mut gate = crate::handlers::push_filter::PushGate::new(&f.home, &f.alice, false);
        f.db.set_user_status(&f.alice.user_id, duduclaw_auth::UserStatus::Suspended).unwrap();
        assert!(fresh_reader(&f.home, &f.alice).is_none());
        assert!(gate.filter_prepared(Arc::new(frame)).await.is_none());
    }

    #[tokio::test]
    async fn approval_visibility_reuses_task_audience_without_retaining_reader_bindings() {
        let f = fixture().await;
        let broker = crate::approval::ApprovalBroker::open(&f.home).unwrap();
        let id = broker.request("sales", "goal_kickoff", "Review task",
            json!({"task_id": crate::handlers::task_privacy_tests::PRIVATE}), 300).await.unwrap();
        let record = broker.get(&id).await.unwrap().unwrap();
        let snapshot = ApprovalVisibility::load(&f.home, &record, &f.alice);
        assert!(snapshot.may_read(&record, &f.alice, None));
        assert!(!snapshot.may_read(&record, &f.bob, None));
        f.db.unbind_agent(&f.alice.user_id, "sales").unwrap();
        let (fresh, department) = fresh_reader(&f.home, &f.alice).unwrap();
        assert!(!snapshot.may_read(&record, &fresh, department.as_deref()));
        let rpc = payload(f.handler.handle_approvals_list(json!({"id":id.as_str()}), &f.alice).await);
        assert_eq!(rpc["count"], 0);
    }

    #[tokio::test]
    async fn dashboard_only_quarantine_follows_employee_access_not_admin_only() {
        let f = fixture().await;
        let broker = crate::approval::ApprovalBroker::open(&f.home).unwrap();
        let id = broker.request(
            "sales",
            crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE,
            "Review claim",
            json!({}),
            300,
        ).await.unwrap();
        let record = broker.get(&id).await.unwrap().unwrap();
        let snapshot = ApprovalVisibility::load(&f.home, &record, &f.alice);
        let line = serde_json::to_string(&WsFrame::event(
            "approval.created",
            json!({"id": id.as_str()}),
        )).unwrap();
        let (fresh, department) = fresh_reader(&f.home, &f.alice).unwrap();
        assert!(snapshot.may_read(&record, &fresh, department.as_deref()));
        let rpc = payload(f.handler.handle_approvals_list(json!({"id": id.as_str()}), &f.alice).await);
        assert_eq!(rpc["count"], 1);
        let mut gate = crate::handlers::push_filter::PushGate::new(&f.home, &f.alice, false);
        assert!(gate.filter_live(&line, &f.handler).await.is_some());

        f.db.unbind_agent(&f.bob.user_id, "sales").unwrap();
        let (fresh, department) = fresh_reader(&f.home, &f.bob).unwrap();
        assert!(!snapshot.may_read(&record, &fresh, department.as_deref()));
        let rpc = payload(f.handler.handle_approvals_list(json!({"id": id.as_str()}), &f.bob).await);
        assert_eq!(rpc["count"], 0);
        let mut gate = crate::handlers::push_filter::PushGate::new(&f.home, &f.bob, false);
        assert!(gate.filter_live(&line, &f.handler).await.is_none());

        let root = admin(&f);
        let (fresh, department) = fresh_reader(&f.home, &root).unwrap();
        assert!(snapshot.may_read(&record, &fresh, department.as_deref()));
        let rpc = payload(f.handler.handle_approvals_list(json!({"id": id.as_str()}), &root).await);
        assert_eq!(rpc["count"], 1);
    }

    #[tokio::test]
    async fn knowledge_add_source_editor_sees_list_and_push_non_editor_does_not() {
        let f = fixture().await;
        let db = duduclaw_knowledge::schema::open_home(&f.home).unwrap();
        db.execute(
            "INSERT INTO collections (id,slug,name,created_by,created_at,updated_at) VALUES ('c','c','Collection','system','now','now')",
            [],
        ).unwrap();
        db.execute(
            "INSERT INTO collection_acl (collection_id,principal_kind,principal_id,role,granted_by,granted_at) VALUES ('c','user',?1,'editor','system','now')",
            [&f.alice.user_id],
        ).unwrap();
        let broker = crate::approval::ApprovalBroker::open(&f.home).unwrap();
        let id = broker.request(
            "sales",
            crate::knowledge::add_source_approval::ACTION_KIND,
            "Review source",
            json!({"collection_id": "c"}),
            300,
        ).await.unwrap();
        let line = serde_json::to_string(&WsFrame::event(
            "approval.created",
            json!({"id": id.as_str()}),
        )).unwrap();
        // alice and bob are both non-admin Managers with Viewer on `sales`.
        // Only alice holds the collection Editor grant.
        for (ctx, expected) in [(&f.alice, true), (&f.bob, false)] {
            let rpc = payload(f.handler.handle_approvals_list(json!({"id": id.as_str()}), ctx).await);
            assert_eq!(rpc["count"], if expected { 1 } else { 0 });
            let mut gate = crate::handlers::push_filter::PushGate::new(&f.home, ctx, false);
            assert_eq!(gate.filter_live(&line, &f.handler).await.is_some(), expected);
        }
    }
}
