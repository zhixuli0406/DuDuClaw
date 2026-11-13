//! v1.81 P1g: the inverted default for Personal employees in protected
//! mode. Real users.db / tasks.db stores (hardened opener, private test
//! home); `protected` is passed in or given to `AclRestriction::with_flag` —
//! no test sets the process-wide flag, and no test installs the process-wide
//! ACL restriction (the `_under` predicates take it explicitly).
use super::*;
use super::super::personal_data_gate::{
    CONTROL_ACTION, METADATA_ACTION, agent_scoped, audit_personal_control, audit_personal_metadata, write_scoped,
};
use super::super::personal_delegation_gate::{delegation_decision, delegation_target_keys};
use super::super::task_privacy::PERMISSION_DENIED;
use crate::personal_visibility::tests::{PERSONAL, SHARED, home};
use crate::personal_visibility::{
    AclRestriction, OWNERSHIP_UNAVAILABLE, PERSONAL_DELEGATION_REFUSED, PERSONAL_FIRE_REFUSED, WorkItem,
    fire_time_refusal, goal_link_metadata, principal_user,
};
use crate::personal_visibility::DataCategory;
use duduclaw_auth::AccessLevel;
use duduclaw_auth::acl::PersonalRestriction;

/// `n` characters of `source` from byte offset `at` (never splits a char).
fn window(source: &str, at: usize, n: usize) -> String {
    source[at..].chars().take(n).collect()
}

fn hidden_of(h: &crate::personal_visibility::tests::Home, user: &str) -> BTreeSet<String> {
    visibility::hidden_from(h.path(), true, user).unwrap()
}

// ── 1. ACL inversion ─────────────────────────────────────────────────────

#[test]
fn p1g_acl_inversion_refuses_admin_non_owner() {
    let h = home();
    let ctx = |id: &str| h.db.live_user_context(id).unwrap().unwrap();
    let (alice, bob, admin) = (ctx(h.alice.as_str()), ctx(h.bob.as_str()), ctx(h.admin.as_str()));
    let on = AclRestriction::with_flag(h.path(), true);
    let r: Option<&dyn PersonalRestriction> = Some(&on);
    for level in [AccessLevel::Viewer, AccessLevel::Operator, AccessLevel::Owner] {
        assert!(!admin.has_agent_access_under(PERSONAL, level, r), "Admin is no longer let through: {level:?}");
        assert!(admin.has_agent_access_under(SHARED, level, r), "Shared employees are unchanged");
        assert!(alice.has_agent_access_under(PERSONAL, level, r), "the owner keeps its binding");
    }
    assert!(!admin.can_access_agent_under(PERSONAL, r));
    assert!(!admin.can_call_agent_under(PERSONAL, r));
    assert!(!admin.can_assign_to_under(PERSONAL, r));
    assert!(!bob.has_agent_access_under(PERSONAL, AccessLevel::Viewer, r), "a manager gets nothing either");
    // The explicit management rule keeps the pre-P1g Admin shortcut.
    assert!(admin.has_management_access(PERSONAL, AccessLevel::Operator));
    assert!(!bob.has_management_access(PERSONAL, AccessLevel::Operator));
    // The `system` principal is never an owner.
    assert!(visibility::acl_denies(h.path(), true, "system", PERSONAL));
    // Unprotected install: unchanged.
    let off = AclRestriction::with_flag(h.path(), false);
    assert!(admin.has_agent_access_under(PERSONAL, AccessLevel::Owner, Some(&off)));
    assert!(!visibility::acl_denies(h.path(), false, &h.admin, PERSONAL));
    // Unreadable ownership: every employee is refused (fail closed).
    let empty = crate::fs_safe::private_test_home().unwrap();
    let broken = AclRestriction::with_flag(empty.path(), true);
    assert!(!admin.has_agent_access_under(SHARED, AccessLevel::Viewer, Some(&broken)));
}

#[tokio::test]
async fn p1g_break_glass_is_request_scoped_and_never_delegation() {
    let h = home();
    let admin = h.db.live_user_context(&h.admin).unwrap().unwrap();
    let on = AclRestriction::with_flag(h.path(), true);
    let r: Option<&dyn PersonalRestriction> = Some(&on);
    // Outside a request scope a grant is not recorded.
    visibility::record_break_glass(&h.admin, PERSONAL);
    assert!(!visibility::break_glass_granted(&h.admin, PERSONAL));
    visibility::with_break_glass_scope(async {
        assert!(!admin.has_agent_access_under(PERSONAL, AccessLevel::Viewer, r));
        // What `personal_data_gate` records after an audited step-up.
        visibility::record_break_glass(&h.admin, PERSONAL);
        assert!(admin.has_agent_access_under(PERSONAL, AccessLevel::Viewer, r), "admitted for this request");
        assert!(!admin.can_assign_to_under(PERSONAL, r), "delegation never uses break-glass");
        assert!(on.denies_delegation(&h.admin, PERSONAL));
        assert!(!visibility::break_glass_granted(&h.bob, PERSONAL), "the grant names its user");
    })
    .await;
    // The next request starts with nothing.
    visibility::with_break_glass_scope(async {
        assert!(!admin.has_agent_access_under(PERSONAL, AccessLevel::Viewer, r));
    })
    .await;
}

// ── 2. allowlist + default deny + hidden rows ────────────────────────────

#[test]
fn p1g_allowlisted_management_actions_pass_with_audit() {
    for method in ["agents.status", "presets.status", "agents.avatar", "agents.pause", "agents.resume",
        "autonomy.pause", "autonomy.resume", "agents.list", "isolation.employee_tombstone",
        "migration.confirmations.list"] {
        assert!(metadata_allowlisted(method), "{method}");
    }
    // P1h: no prefix match (an unreviewed lifecycle-looking name is not
    // exempt) and `topology.list` left the allowlist (N6).
    for method in ["agents.update", "cron.update", "heartbeat.trigger", "memory.search", "odoo.agent_config_set",
        "mcp.update", "experts.remove", "org.apply_change", "finetune.datasets.preview", "belief.recent",
        "topology.list", "isolation.employee_restore", "isolation.anything_new", "migration.other",
        "me.isolation.anything_new"] {
        assert!(!metadata_allowlisted(method), "{method}");
    }
    let h = home();
    // Metadata reads: one row for an Admin on another person's Personal
    // employee; none for the owner, a Shared employee or an unprotected install.
    for method in ["agents.status", "presets.status", "agents.avatar"] {
        assert_eq!(audit_personal_metadata(h.path(), Some(&h.db), true, &h.admin, method, PERSONAL), Ok(true));
    }
    assert_eq!(audit_personal_metadata(h.path(), Some(&h.db), true, &h.alice, "agents.status", PERSONAL), Ok(false));
    assert_eq!(audit_personal_metadata(h.path(), Some(&h.db), true, &h.admin, "agents.status", SHARED), Ok(false));
    assert_eq!(audit_personal_metadata(h.path(), Some(&h.db), false, &h.admin, "agents.status", PERSONAL), Ok(false));
    let rows = h.db.query_audit_log(Some(&h.admin), Some(METADATA_ACTION), 50).unwrap();
    assert_eq!(rows.len(), 3);
    let detail: Value = serde_json::from_str(rows[0].detail.as_deref().unwrap()).unwrap();
    assert_eq!((detail["employee"].as_str(), detail["owner_user_id"].as_str()), (Some(PERSONAL), Some(h.alice.as_str())));
    assert_eq!(detail["category"], json!("metadata"));
    // F8: autonomy pause / resume leave the control row (fail closed).
    for method in ["autonomy.pause", "autonomy.resume"] {
        assert_eq!(audit_personal_control(h.path(), Some(&h.db), true, &h.admin, method, PERSONAL), Ok(()));
    }
    let control = h.db.query_audit_log(Some(&h.admin), Some(CONTROL_ACTION), 50).unwrap();
    let methods: BTreeSet<String> = control.iter()
        .map(|row| serde_json::from_str::<Value>(row.detail.as_deref().unwrap()).unwrap()["method"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(methods, ["autonomy.pause".to_string(), "autonomy.resume".to_string()].into());
    assert!(audit_personal_control(h.path(), None, true, &h.admin, "autonomy.pause", PERSONAL).is_err());
    let source = include_str!("autonomy_rpc.rs");
    assert!(source.contains("self.personal_control_audit(&live, method, agent)"), "F8: the handler audits");
    assert!(source.contains("require_live_management_access"), "F8: autonomy is on the management allowlist");
    let core = include_str!("dispatch_core.rs");
    for arm in ["\"agents.status\" => {", "\"presets.status\" => {", "\"agents.avatar\" => {"] {
        let at = core.find(arm).unwrap_or_else(|| panic!("{arm}"));
        let body = window(core, at, 400);
        assert!(body.contains("check_agent_mgmt!") && body.contains("personal_metadata_audit"), "{arm}");
    }
}

#[test]
fn p1g_default_deny_and_hidden_rows() {
    let h = home();
    let hidden = hidden_of(&h, &h.admin);
    assert_eq!(hidden, [PERSONAL.to_string()].into());
    assert!(hidden_of(&h, &h.alice).is_empty(), "the owner hides nothing");
    // Every key the default deny reads, trimmed.
    for key in EMPLOYEE_PARAM_KEYS {
        assert_eq!(default_deny_target("cron.update", &json!({ *key: PERSONAL }), &hidden), Some(PERSONAL.to_string()), "{key}");
        assert_eq!(default_deny_target("cron.update", &json!({ *key: format!(" {PERSONAL} ") }), &hidden),
            Some(PERSONAL.to_string()), "{key} padded");
        assert_eq!(default_deny_target("cron.update", &json!({ *key: SHARED }), &hidden), None, "{key}");
    }
    assert_eq!(default_deny_target("audit.unified_log", &json!({"agent_ids": [SHARED, PERSONAL]}), &hidden),
        Some(PERSONAL.to_string()));
    assert_eq!(default_deny_target("agents.status", &json!({"agent_id": PERSONAL}), &hidden), None, "allowlisted");
    assert_eq!(default_deny_target("cron.update", &json!({"agent_id": PERSONAL}), &BTreeSet::new()), None, "owner");
    // Hidden rows: unscoped listings drop the employee (hidden, not redacted).
    let mut payload = json!({
        "tasks": [{"agent_id": PERSONAL, "name": "secret"}, {"agent_id": SHARED, "name": "ok"},
            {"name": "nested", "owner": {"owner_agent_id": PERSONAL}}],
        "by_agent": {PERSONAL: 3, SHARED: 1},
        "names": [PERSONAL, SHARED],
        "count": 3,
    });
    assert_eq!(drop_hidden_rows(&mut payload, &hidden), 4);
    assert_eq!(payload["tasks"], json!([{"agent_id": SHARED, "name": "ok"}]));
    assert_eq!(payload["by_agent"], json!({SHARED: 1}));
    assert_eq!(payload["names"], json!([SHARED]));
    assert!(!names_hidden(&payload, &hidden));
    // A single-object answer still naming one is withheld by the caller.
    assert!(names_hidden(&json!({"event": {"agent_id": PERSONAL, "channel": "telegram"}}), &hidden));
    assert_eq!(drop_hidden_rows(&mut json!({"x": 1}), &BTreeSet::new()), 0);
    // Wired at dispatch (after the P1–P1e gates) and around every reply.
    let dispatch = include_str!("mod.rs");
    let gates = dispatch.find("self.personal_gates_for_params(method, &mut params, ctx)").unwrap();
    let deny = dispatch.find("self.personal_default_deny(method, &params, ctx)").unwrap();
    assert!(gates < deny, "the default deny runs after the gates (their step-ups admit their employee)");
    assert!(dispatch.contains("self.personal_hide_rows(method, ctx, frame)"));
    assert!(dispatch.contains("with_break_glass_scope"));
}

// ── P1h ──────────────────────────────────────────────────────────────────

/// Every `"isolation.…"` / `"migration.…"` / `"me.isolation.…"` string
/// literal of `source` that ends a method name (a quoted name followed by
/// `"`), for the dispatch-table scan below.
fn prefixed_literals(source: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for prefix in ["\"isolation.", "\"migration.", "\"me.isolation."] {
        let mut rest = source;
        while let Some(at) = rest.find(prefix) {
            let tail = &rest[at + 1..];
            let end = tail.find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '_')).unwrap_or(tail.len());
            if tail[end..].starts_with('"') {
                found.insert(tail[..end].to_owned());
            }
            rest = &rest[at + prefix.len()..];
        }
    }
    found
}

/// N3: the lifecycle allowlist is a frozen list equal to the methods the
/// dispatch table really routes in those namespaces — a new one is not
/// exempt until reviewed here.
#[test]
fn p1h_lifecycle_allowlist_equals_the_dispatch_table() {
    let frozen: BTreeSet<String> = LIFECYCLE_ALLOWLIST.iter().map(|(method, _)| (*method).to_owned()).collect();
    assert_eq!(frozen.len(), LIFECYCLE_ALLOWLIST.len(), "duplicate entry");
    for (method, reason) in LIFECYCLE_ALLOWLIST {
        assert!(!reason.trim().is_empty(), "{method}: every entry is reviewed");
    }
    // What dispatch routes (`handlers/mod.rs`): the connector and employee
    // handlers' method tables, the migration-confirmation methods and the
    // two recording methods.
    let mut routed: BTreeSet<String> = BTreeSet::new();
    routed.extend(super::super::isolation_connectors_rpc::METHODS.iter().map(|m| m.to_string()));
    // Merge P1h × F6: the employee handler also serves the F6 restore /
    // adoption / Shared-restore / quarantine-purge methods.
    routed.extend(super::super::isolation_employee_rpc::routed_methods().into_iter().map(str::to_owned));
    for method in ["me.isolation.list_restorable", "isolation.employee_restore_decide", "isolation.shared_restore_options",
        "isolation.adoption_request_for_owner", "isolation.quarantine_purge"] {
        assert!(routed.contains(method), "{method}: the F6 methods are in the scan");
    }
    let confirmations = include_str!("migration_confirmations_rpc.rs");
    let known = confirmations.find("pub(super) fn known_method(").unwrap();
    let known_end = known + confirmations[known..].find("\n}").unwrap();
    routed.extend(prefixed_literals(&confirmations[known..known_end]));
    let dispatch = include_str!("mod.rs");
    let recording = dispatch.find("if matches!(method, \"isolation.recording_status\"").unwrap();
    routed.extend(prefixed_literals(&dispatch[recording..recording + 120]));
    assert_eq!(frozen, routed, "LIFECYCLE_ALLOWLIST must equal the dispatched lifecycle methods");
    for method in &routed {
        assert!(super::super::isolation_connectors_rpc::owns(method) || super::super::isolation_employee_rpc::owns(method)
            || super::super::migration_confirmations_rpc::known_method(method)
            || matches!(method.as_str(), "isolation.recording_status" | "isolation.recording_stop"), "{method}");
    }
    // The namespaces dispatch hands to those handlers are exactly these.
    for needle in ["method.split('.').take(2).eq([\"migration\", \"confirmations\"])",
        "if isolation_connectors_rpc::owns(method)", "if isolation_employee_rpc::owns(method)"] {
        assert!(dispatch.contains(needle), "{needle}");
    }
    // No dispatch arm elsewhere names another lifecycle-looking method.
    for (file, source) in [("dispatch_system.rs", include_str!("dispatch_system.rs")),
        ("dispatch_org.rs", include_str!("dispatch_org.rs")), ("dispatch_core.rs", include_str!("dispatch_core.rs"))] {
        let arms: Vec<String> = prefixed_literals(source).into_iter()
            .filter(|m| source.contains(&format!("\"{m}\" =>")) || source.contains(&format!("\"{m}\" |"))).collect();
        assert!(arms.iter().all(|m| frozen.contains(m)), "{file}: unreviewed lifecycle arm {arms:?}");
    }
    // No prefix match any more.
    for method in ["isolation.connector_purge", "isolation.employee_approve", "migration.confirmations.approve_all",
        "me.isolation.approve_personal_employee", "isolation.recording_list"] {
        assert!(!metadata_allowlisted(method), "{method}");
    }
    // recording_status / recording_stop hide a non-owned Personal result.
    // Merge P1h × HX6: control moved to `boot_recording.rs` on the HX6
    // recording store (`(run, call)` rows); the owner is read per call.
    let boot = include_str!("../isolation_mcp/boot_recording.rs");
    let at = boot.find("fn control(").unwrap();
    let body = window(boot, at, 3200);
    assert!(body.contains("recording_jobs.admin_recording_owner(call)?") && body.contains("withhold(personal_owner.as_deref(), &ctx.user_id)")
        && body.contains("\"withheld\""), "{body}");
    assert!(boot.contains("recorded != actor"));
    assert!(include_str!("../isolation_mcp/boot.rs").contains("pub(crate) use recording::control_recording;"));
}

/// N6: `topology.list` left the allowlist, so its override / proposal rows
/// naming another person's Personal employee (from / to) are dropped.
#[test]
fn p1h_topology_rows_are_filtered_and_org_chart_hides() {
    let h = home();
    let hidden = hidden_of(&h, &h.admin);
    assert!(!metadata_allowlisted("topology.list"));
    let mut topology = json!({
        "overrides": [{"task_class": "report", "from_agent": SHARED, "to_agent": PERSONAL, "status": "active"},
            {"task_class": "mail", "from_agent": PERSONAL, "to_agent": SHARED, "status": "active"},
            {"task_class": "ops", "from_agent": SHARED, "to_agent": "other", "status": "active"}],
        "pending_proposals": [{"id": "p1", "from_agent": PERSONAL, "to_agent": SHARED}],
    });
    drop_hidden_rows(&mut topology, &hidden);
    assert_eq!(topology["overrides"].as_array().unwrap().len(), 1);
    assert_eq!(topology["overrides"][0]["task_class"], json!("ops"));
    assert_eq!(topology["pending_proposals"], json!([]));
    // agents.org_chart: hidden employees are never full rows (Admin or
    // department); an ancestor of a visible employee stays a stub.
    let listable: std::collections::HashMap<&str, &str> = [(PERSONAL, "ops"), (SHARED, "ops"), ("lead", "ops")].into();
    let full = super::super::agents_list_rpc::org_chart_full(&listable, &hidden, |_, _| true);
    assert_eq!(full, std::collections::HashSet::<&str>::from([SHARED, "lead"]));
    let owner_full = super::super::agents_list_rpc::org_chart_full(&listable, &hidden_of(&h, &h.alice), |_, _| true);
    assert!(owner_full.contains(PERSONAL), "its owner sees it in full");
    let source = include_str!("agents_list_rpc.rs");
    let at = source.find("pub(crate) async fn handle_agents_org_chart(").unwrap();
    let body = window(source, at, 7000);
    assert!(body.contains("self.personal_hidden_agents(ctx).await"));
    assert!(body.contains("org_chart_full(&listable, &hidden,"));
    assert!(body.contains("if !full.contains(cfg.name.as_str())"), "ancestors outside the scope stay name + role stubs");
}

// ── 3. fire-time check ───────────────────────────────────────────────────

#[test]
fn p1g_fire_time_refuses_pre_existing_cron_and_reminder() {
    let h = home();
    let refusal = |principals: &[Option<&str>], acting: Option<&str>, target: &str| {
        fire_time_refusal(h.path(), true, principals, acting, target)
    };
    let principals = |row: &crate::cron_store::CronTaskRow, scoped: Option<&str>| {
        crate::cron_scheduler::cron_fire_principals(row, scoped)
    };
    fn as_refs(p: &[Option<String>; 4]) -> Vec<Option<&str>> {
        p.iter().map(Option::as_deref).collect()
    }
    // A routine an Admin created before the upgrade for alice's Personal employee.
    let mut admin_cron = crate::cron_store::CronTaskRow::new("c1".into(), "daily".into(), PERSONAL.into(),
        "0 9 * * *".into(), "report".into());
    admin_cron.on_behalf_of = Some(format!("user:{}", h.admin));
    admin_cron.created_by_user_id = Some(h.admin.clone());
    assert_eq!(refusal(&as_refs(&principals(&admin_cron, None)), None, PERSONAL), Some(PERSONAL_FIRE_REFUSED));
    // A legacy / MCP row with no recorded person never runs on it.
    let legacy = crate::cron_store::CronTaskRow::new("c2".into(), "legacy".into(), PERSONAL.into(),
        "0 9 * * *".into(), "x".into());
    assert_eq!(refusal(&as_refs(&principals(&legacy, None)), None, PERSONAL), Some(PERSONAL_FIRE_REFUSED));
    // The owner's routine fires; an Admin "run now" of it does not.
    let mut owned = legacy.clone();
    owned.on_behalf_of = Some(format!("user:{}", h.alice));
    owned.created_by_user_id = Some(h.alice.clone());
    assert_eq!(refusal(&as_refs(&principals(&owned, None)), None, PERSONAL), None);
    let admin_run = format!("user:{}", h.admin);
    assert_eq!(refusal(&as_refs(&principals(&owned, Some(&admin_run))), None, PERSONAL), Some(PERSONAL_FIRE_REFUSED));
    let mut edited = owned.clone();
    edited.scoped_by_user_id = Some(h.admin.clone());
    assert_eq!(refusal(&as_refs(&principals(&edited, None)), None, PERSONAL), Some(PERSONAL_FIRE_REFUSED),
        "every recorded person must be the CURRENT owner");
    // Shared employees and unprotected installs: unchanged.
    assert_eq!(refusal(&as_refs(&principals(&admin_cron, None)), None, SHARED), None);
    assert_eq!(fire_time_refusal(h.path(), false, &[None], None, PERSONAL), None);
    // A reminder callback with another person's origin; the owner's own.
    assert_eq!(refusal(&[Some(admin_run.as_str())], None, PERSONAL), Some(PERSONAL_FIRE_REFUSED));
    let alice_run = format!("user:{}", h.alice);
    assert_eq!(refusal(&[Some(alice_run.as_str())], None, PERSONAL), None);
    // Queue rows: the employee's own work, or another Personal employee of
    // the same owner, passes; another employee's delegation does not.
    assert_eq!(refusal(&[None], Some(PERSONAL), PERSONAL), None);
    assert_eq!(refusal(&[None], Some(SHARED), PERSONAL), Some(PERSONAL_FIRE_REFUSED));
    assert_eq!(refusal(&[None], Some("heartbeat-scheduler"), PERSONAL), Some(PERSONAL_FIRE_REFUSED));
    // (Same-owner Personal → Personal is `delegation_refusal`, covered by P1c.)
    // Fail closed.
    let empty = crate::fs_safe::private_test_home().unwrap();
    assert_eq!(fire_time_refusal(empty.path(), true, &[Some(alice_run.as_str())], None, PERSONAL),
        Some(OWNERSHIP_UNAVAILABLE));
    // Principal parsing.
    assert_eq!(principal_user("user:abc"), Some("abc".into()));
    assert_eq!(principal_user("abc"), Some("abc".into()));
    assert_eq!(principal_user("system"), None);
    assert_eq!(principal_user("user:"), None);
    // Wired into every fire point.
    for (name, source, needle) in [
        ("dispatcher (SQLite rail)", include_str!("../dispatcher.rs"), "fire_time_personal_refusal(home_dir, &msg)"),
        ("dispatcher (JSONL rail)", include_str!("../dispatcher.rs"), "bus_fire_time_refusal(home_dir, &msg)"),
        ("dispatcher (TaskSpec steps)", include_str!("../dispatcher.rs"), "Some(&acting), &target)"),
        ("cron", include_str!("../cron_scheduler.rs"), "cron_fire_time_refusal(home_dir, &task, scoped_principal)"),
        ("workflow routine", include_str!("../cron_scheduler.rs"), "cron_fire_time_refusal(home_dir, &task, None)"),
        ("reminders", include_str!("../reminder_scheduler.rs"), "ReminderMode::AgentCallback && crate::runtime_readiness"),
        ("assign schedules", include_str!("../assign_schedule/fire.rs"), "fire_time_refusal("),
    ] {
        assert!(source.contains(needle), "{name}: fire-time check missing");
    }
}

// ── 4. the named F-fixes ─────────────────────────────────────────────────

#[test]
fn p1g_f1_exports_are_owner_only_and_unscoped_rows_dropped() {
    let exports = include_str!("../data_exports/mod.rs");
    assert!(exports.contains("live.hides_agent(id) || !live.has_agent_access(id, AccessLevel::Viewer)"));
    // status / cancel / the HTTP download all go through `owned` → `authorize`.
    let owned = exports.find("fn owned(").unwrap();
    assert!(window(exports, owned, 600).contains("authorize(home, &live, &request)?"));
    let download = exports.find("pub fn download(").unwrap();
    assert!(window(exports, download, 300).contains("owned(home, ctx, id)?"));
    let source = include_str!("../data_exports/source.rs");
    assert_eq!(source.matches("live.hides_agent(text(&cells[").count(), 2, "tasks + usage rows drop hidden employees");
    // Conversations / memories / wiki rows use `agent_visible` = has_agent_access (inverted ACL).
    assert!(source.contains("Ok(live.has_agent_access(agent, AccessLevel::Viewer))"));
    assert!(agent_scoped("data_exports.create").is_none(), "no break-glass for bulk export");
}

#[test]
fn p1g_f2_computer_sessions_view_is_break_glass_and_control_owner_only() {
    for method in ["computer_sessions.view", "computer_sessions.snapshot"] {
        assert_eq!(agent_scoped(method), Some(DataCategory::Sessions), "{method}");
    }
    for method in ["computer_sessions.takeover", "computer_sessions.hand_back", "computer_sessions.resume",
        "computer_sessions.stop"] {
        assert_eq!(agent_scoped(method), None, "{method}: no break-glass");
    }
    let view = include_str!("../computer_use_sessions/live_view.rs");
    assert!(view.contains("LiveAction::Watch => live.hides_agent(agent_id)"));
    assert!(view.contains("LiveAction::Control => live.owner_only_denied(agent_id)"));
    let rpc = include_str!("computer_sessions_rpc.rs");
    assert!(rpc.contains("method == \"computer_sessions.stop\" && live.owner_only_denied(agent)"));
}

#[test]
fn p1g_f3_f4_f5_f10_delegation_and_binding_surfaces() {
    let h = home();
    let ctx = |id: &str| h.db.live_user_context(id).unwrap().unwrap();
    let (alice, bob, admin) = (ctx(h.alice.as_str()), ctx(h.bob.as_str()), ctx(h.admin.as_str()));
    // F3: custom-skill builders are delegation keys (+ the stored builder).
    assert_eq!(delegation_target_keys("skills.custom_create"), &["built_by_agent"]);
    assert_eq!(delegation_target_keys("skills.custom_generate"), &["agent", "built_by_agent"]);
    assert!(include_str!("skills_custom.rs").contains("if ctx.owner_only_denied(&agent_id)"));
    // F4: handoff (both ends) and skill share are owner-only, no break-glass.
    assert_eq!(delegation_target_keys("agents.handoff"), &["from_agent", "to_agent"]);
    assert_eq!(write_scoped("agents.handoff"), None);
    assert_eq!(delegation_target_keys("skills.share"), &["agent_id"]);
    assert_eq!(agent_scoped("skills.share"), None);
    // F10: binding someone to a Personal employee is a config write.
    assert_eq!(write_scoped("users.bind_agent"), Some(("agent_name", DataCategory::Config)));
    assert_eq!(write_scoped("users.unbind_agent"), Some(("agent_name", DataCategory::Config)));
    let cases = [
        ("agents.handoff", json!({"from_agent": PERSONAL, "to_agent": SHARED})),
        ("agents.handoff", json!({"from_agent": SHARED, "to_agent": PERSONAL})),
        ("skills.share", json!({"agent_id": PERSONAL, "skill_name": "x"})),
        ("skills.custom_create", json!({"built_by_agent": PERSONAL, "slug": "x"})),
        ("skills.custom_generate", json!({"id": "d1", "agent": PERSONAL})),
        // F5: surfaces that make the employee run work.
        ("heartbeat.trigger", json!({"agent_id": PERSONAL})),
        ("cron.add", json!({"agent_id": PERSONAL, "cron": "0 9 * * *", "task": "x", "name": "n"})),
        ("responsibilities.create", json!({"owner_agent_id": PERSONAL})),
        ("skill_synthesis.update", json!({"target_agent": PERSONAL})),
    ];
    for (method, params) in cases {
        assert_eq!(delegation_decision(h.path(), true, &admin, method, &params), Some(PERSONAL_DELEGATION_REFUSED),
            "{method}: an Admin is refused and told why");
        assert_eq!(delegation_decision(h.path(), true, &bob, method, &params), Some(PERMISSION_DENIED), "{method}");
        assert_eq!(delegation_decision(h.path(), true, &alice, method, &params), None, "{method}: the owner");
        assert_eq!(delegation_decision(h.path(), false, &admin, method, &params), None, "{method}: unprotected");
    }
}

#[tokio::test]
async fn p1g_f7_goal_links_unmasked_and_unmatched_rows_fail_closed() {
    use crate::task_store::{ActivityRow, GoalRow, TaskStore};
    // Only an exact goal link of a `goal_created` row is stored unmasked.
    assert_eq!(goal_link_metadata("goal_created", r#"{"goal_id":"g-1"}"#), Some(r#"{"goal_id":"g-1"}"#.into()));
    assert!(goal_link_metadata("goal_created", r#"{"goal_ids":["a","b"],"goal_backfill":"p1e"}"#).is_some());
    for raw in [r#"{"goal_id":"g 1"}"#, r#"{"goal_id":"g1","note":"x"}"#, r#"{"goal_backfill":"p1e"}"#, "[1]", "x",
        r#"{"goal_ids":[]}"#] {
        assert_eq!(goal_link_metadata("goal_created", raw), None, "{raw}");
    }
    assert_eq!(goal_link_metadata("agent_comment", r#"{"goal_id":"g-1"}"#), None);
    let h = home();
    let store = TaskStore::open(h.path()).unwrap();
    for id in ["g-linked", "g-company"] {
        store.insert_goal(&GoalRow::new(id.into(), id.into(), String::new())).await.unwrap();
    }
    let row = |id: &str, metadata: Option<String>| ActivityRow {
        id: id.into(),
        event_type: visibility::GOAL_CREATED_EVENT.into(),
        agent_id: PERSONAL.into(),
        task_id: None,
        summary: "Created goal: renamed since".into(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        metadata,
    };
    store.append_activity(&row("linked", Some(json!({"goal_id": "g-linked"}).to_string()))).await.unwrap();
    let employees = |goal: &str| visibility::work_item_employees(h.path(), WorkItem::Goal, goal).unwrap();
    assert_eq!(employees("g-linked"), [PERSONAL.to_string()].into(), "the link survives the store write");
    assert!(employees("g-company").is_empty(), "every row is linked: an unlinked goal belongs to nobody");
    // An unmatched legacy row of a Personal employee: every unlinked goal is
    // held by it (hidden from those it is hidden from) until reviewed.
    store.append_activity(&row("legacy", None)).await.unwrap();
    assert_eq!(employees("g-company"), [PERSONAL.to_string()].into());
    let creators = visibility::goal_creators(h.path()).unwrap();
    assert!(visibility::goal_hidden(creators.get("g-company"), &hidden_of(&h, &h.admin)));
    assert!(!visibility::goal_hidden(creators.get("g-company"), &hidden_of(&h, &h.alice)));
}

#[test]
fn p1g_f9_fork_tools_answer_unknown_and_foreign_alike() {
    let source = include_str!("../../../duduclaw-cli/src/mcp_fork.rs");
    assert!(!source.contains("unavailable to this agent"));
    assert!(!source.contains("belongs to another agent"));
    assert!(source.matches("fork_not_found(").count() >= 6);
}

// ── 5. a* payloads + HTTP routes ─────────────────────────────────────────

#[test]
fn p1g_a_star_payloads_carry_no_personal_content() {
    let h = home();
    let hidden = hidden_of(&h, &h.admin);
    // redaction.recent_audit: one JSONL audit event per line (agent_id,
    // tokens, categories — never text); another person's are dropped.
    let mut audit = json!({"entries": [
        {"event": "redact", "agent_id": PERSONAL, "session_id": "s", "token": "<PII_1>", "category": "email"},
        {"event": "egress_allow", "agent_id": SHARED, "tool": "send", "tokens_restored": 1},
        {"event": "vault_gc", "expired_marked": 0, "purged": 0},
    ]});
    drop_hidden_rows(&mut audit, &hidden);
    assert_eq!(audit["entries"].as_array().unwrap().len(), 2);
    // takeover.list: rows (agent_id, conversation, chat_id).
    let mut takeover = json!({"count": 2, "items": [{"agent_id": PERSONAL, "conversation": "c1", "chat_id": "1"},
        {"agent_id": SHARED, "conversation": "c2", "chat_id": "2"}]});
    drop_hidden_rows(&mut takeover, &hidden);
    assert_eq!(takeover["items"], json!([{"agent_id": SHARED, "conversation": "c2", "chat_id": "2"}]));
    // channel_ingress.inspect: one event object → withheld when it names one.
    assert!(names_hidden(&json!({"event": {"agent_id": PERSONAL, "status": "accepted"}, "attempts": []}), &hidden));
    // logs.subscribe: the unfiltered tail is refused while anything is hidden.
    let system = include_str!("dispatch_system.rs");
    let at = system.find("\"logs.subscribe\" => {").unwrap();
    assert!(window(system, at, 700).contains("self.p1g_log_tail_allowed(ctx)"));
}

/// HTTP routes of `server.rs` that take an employee id or move data out:
/// each reviewed.
const HTTP_ROUTES: &[(&str, &str)] = &[
    ("/ws/chat", "WebChat: authorize_target_inner refuses another person's Personal employee before the Admin shortcut"),
    ("/api/company/logo/{filename}", "company branding; no employee"),
    ("/api/files", "authorize_file_access: can_access_agent (P1g ACL); unscoped = Admin shared attachments"),
    ("/api/files/download", "authorize_file_access: can_access_agent (P1g ACL)"),
    ("/api/files/preview", "authorize_file_access: can_access_agent (P1g ACL)"),
    ("/api/chat/attachments", "history_attachment_bytes: owner only for a Personal employee (P1, no step-up)"),
    ("/api/data-exports/{id}/download", "F1: owned → authorize refuses another person's Personal employee"),
    ("/api/decision/task-board-export", "Admin aggregate shadow dataset (counts / durations); names no employee"),
    ("/api/decision/odoo-export", "Admin Odoo shadow dataset; no employee data"),
    ("/api/causal/source/import-memory", "P1g: hides_agent(&request.agent_id) refused (owner only)"),
    ("/api/causal/source/import-wiki", "P1g: hides_agent(&request.agent_id) refused (owner only)"),
    ("/api/reliability/summary", "P1g: caller.hides_agent(&agent_id) refused (owner only)"),
    ("/api/experts/upload", "stages an expert pack; installing into an employee is experts.* (default deny)"),
    ("/api/knowledge/upload", "knowledge-base collection ACLs; no employee"),
    ("/api/knowledge/sources/{id}/original", "knowledge-base collection ACLs; no employee"),
    ("/api/device/backup-upload", "appliance Admin backup restore"),
    ("/api/device/backups/download", "appliance Admin backup"),
    ("/api/tasks/rpc", "runs MethodHandler::handle: every dashboard gate, the default deny and hidden rows apply"),
    ("/a2a", "A2A peers call published Shared agents (agent card); Personal employees are not published"),
    ("/.well-known/agent-card.json", "A2A agent card (published agents only)"),
    ("/.well-known/agent.json", "A2A agent card (published agents only)"),
];
const HTTP_KEYWORDS: &[&str] = &["{", "download", "export", "files", "attachments", "import-memory", "import-wiki",
    "reliability", "upload", "original", "agent", "rpc", "a2a", "chat"];

/// Every `.route(` path literal (same or next line).
fn route_paths(source: &str) -> BTreeSet<String> {
    let lines: Vec<&str> = source.lines().collect();
    let mut paths = BTreeSet::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(at) = line.find(".route(") else { continue };
        let rest = line[at + 7..].trim_start();
        let candidate = if rest.is_empty() { lines.get(i + 1).map(|l| l.trim_start()).unwrap_or("") } else { rest };
        if let Some(quoted) = candidate.strip_prefix("\"/")
            && let Some(end) = quoted.find('"')
        {
            paths.insert(format!("/{}", &quoted[..end]));
        }
    }
    paths
}

#[test]
fn p1g_http_routes_naming_an_employee_are_reviewed() {
    let source = include_str!("../server.rs");
    let paths = route_paths(source);
    assert!(paths.len() >= 120, "only {} routes found", paths.len());
    let flagged: BTreeSet<&String> = paths.iter().filter(|p| HTTP_KEYWORDS.iter().any(|k| p.contains(k))).collect();
    let missing: Vec<&&String> = flagged.iter().filter(|p| !HTTP_ROUTES.iter().any(|(r, _)| *r == p.as_str())).collect();
    assert!(missing.is_empty(), "HTTP routes not reviewed for Personal employees: {missing:?}");
    for (route, reason) in HTTP_ROUTES {
        assert!(!reason.is_empty() && paths.contains(*route), "stale HTTP_ROUTES entry {route}");
    }
    // The P1g route fixes are in place.
    let body = |name: &str| {
        let at = source.find(&format!("async fn {name}(")).unwrap_or_else(|| panic!("{name}"));
        let end = source[at + 10..].find("\nasync fn ").map_or(source.len(), |e| at + 10 + e);
        &source[at..end]
    };
    assert!(body("handle_causal_import_memory").contains("ctx.hides_agent(&request.agent_id)"));
    assert!(body("handle_causal_import_wiki").contains("ctx.hides_agent(&request.agent_id)"));
    assert!(body("handle_reliability_summary_http").contains("caller.hides_agent(&agent_id)"));
    let webchat = include_str!("../webchat/access.rs");
    let refuse = webchat.find("denied(\"personal_employee\")").unwrap();
    let shortcut = webchat.rfind("if snapshot.role() == duduclaw_auth::UserRole::Admin { return Ok(()); }").unwrap();
    assert!(refuse < shortcut, "the Personal check runs before the Admin shortcut");
}

#[test]
fn protected_primary_repair_uses_gate_ownership_and_survives_hidden_row_filter() {
    let h = home();
    h.db.set_primary_agent(&h.alice, None).unwrap();
    let preview = h.db.repair_primary_agents(&h.admin, None, |agent| {
        visibility::personal_owner(h.path(), agent).map(|owner| owner.is_some()).map_err(|_| "unavailable".into())
    }).unwrap();
    assert_eq!(preview.excluded_personal_count, 1);
    assert!(preview.members.iter().all(|member| member.agent_id != PERSONAL));
    assert!(preview.members.iter().any(|member| member.user_id == h.alice && member.agent_id == SHARED));
    assert!(!metadata_allowlisted("users.repair_primary_agents"));
    let mut payload = serde_json::to_value(&preview).unwrap();
    let original = payload.clone();
    assert_eq!(drop_hidden_rows(&mut payload, &hidden_of(&h, &h.admin)), 0);
    assert_eq!(payload, original);
    let changed = h.db.repair_primary_agents(&h.admin, Some(&preview.preview_digest), |agent| {
        visibility::personal_owner(h.path(), agent).map(|owner| owner.is_some()).map_err(|_| "unavailable".into())
    }).unwrap();
    assert_eq!(changed.members, preview.members);
    assert_eq!(h.db.configured_primary_agent(&h.alice).unwrap().as_deref(), Some(SHARED));
}
