use super::*;
use crate::dept_grants::methods::{coverage_for, Coverage, METHOD_COVERAGE};
use crate::dept_grants::tests::{fixture, Fixture};
use duduclaw_auth::acl::ContextOrigin;
use std::future::Future;
use std::pin::Pin;

fn assert_permission_denied(method: &str, frame: WsFrame) {
    match frame {
        WsFrame::Response { ok: false, error: Some(Value::String(error)), .. } => {
            // require_role returns exactly "permission denied". Scoped record
            // handlers may wrap that phrase, e.g. reminders' unavailable/deny.
            assert!(error.to_ascii_lowercase().contains("permission denied"),
                "{method}: expected permission refusal, got {error:?}");
        }
        other => panic!("{method}: expected permission refusal, got {other:?}"),
    }
}

fn assert_success(method: &str, frame: WsFrame) {
    assert!(matches!(frame, WsFrame::Response { ok: true, .. }),
        "{method}: expected successful RPC, got {frame:?}");
}

fn admin_only_params(method: &str) -> Value {
    match method {
        "autopilot.create" => json!({
            "name": "matrix rule", "trigger_event": "task_created", "conditions": {},
            "action": {"type": "notify", "channel": "webchat", "chat_id": "matrix", "text": "matrix"}, "agent_id": "a1",
        }),
        "autopilot.update" | "autopilot.remove" | "autopilot.history" => {
            json!({"rule_id": "matrix-rule", "enabled": false, "agent_id": "a1"})
        }
        "skills.install" => json!({"scope": "a1", "content": MATRIX_SKILL}),
        _ => json!({"agent_id": "a1", "days": 7, "limit": 10}),
    }
}

#[tokio::test]
async fn admin_only_rows_refuse_manager_with_all_department_grants() {
    let f = fixture().await;
    let mut checked = 0;
    for &(method, _, coverage) in METHOD_COVERAGE {
        if coverage != Coverage::AdminOnly { continue; }
        let frame = Box::pin(f.h.handle(method, admin_only_params(method), &f.mgr_a)).await;
        assert_permission_denied(method, frame);
        checked += 1;
    }
    assert!(checked > 0, "AdminOnly matrix must not silently become empty");
}

#[tokio::test]
async fn admin_controls_succeed_through_the_same_rpc_path() {
    let f = fixture().await;
    // Both support empty telemetry; these are real allow controls,
    // rather than merely accepting an arbitrary non-permission error.
    for method in ["cost.summary", "cost.by_role"] {
        assert_eq!(coverage_for(method).map(|(_, c)| c), Some(Coverage::AdminOnly));
        assert_permission_denied(method,
            Box::pin(f.h.handle(method, admin_only_params(method), &f.mgr_a)).await);
        assert_success(method,
            Box::pin(f.h.handle(method, admin_only_params(method), &f.admin)).await);
    }
}

// Each case gets a fresh home and record setup, so remove/run/toggle calls
// cannot contaminate later cases. Setup should create records for the named
// target owned by another principal (not mgrA), avoiding creator shortcuts.
// Example setup signature: |f, agent| Box::pin(async move { /* seed record */ }).
// The outcome hook supports list RPCs: check returned rows for visibility
// instead of expecting an error when a handler intentionally filters a list.
// Mutation rows can use assert_scoped_rpc_outcome directly.
async fn assert_scoped<P, S, C>(method: &str, params_for_agent: P, setup: S, check: C)
where
    P: Fn(&str) -> Value,
    S: for<'a> Fn(&'a Fixture, &'a str) -> Pin<Box<dyn Future<Output = ()> + 'a>>,
    C: Fn(&str, WsFrame, bool),
{
    assert!(matches!(coverage_for(method), Some((_, Coverage::Scoped(_)))),
        "{method}: matrix helper requires a Scoped row");
    for case in ["allowed", "other_department", "main", "no_grants", "employee", "member_key", "other_origin", "moved_department"] {
        let f = fixture().await;
        let agent = match case {
            "other_department" => "b1",
            "main" => "main",
            _ => "a1",
        };
        setup(&f, agent).await;
        let mut ctx = match case {
            "no_grants" => f.mgr_b.clone(),
            "employee" => f.emp.clone(),
            _ => f.mgr_a.clone(),
        };
        match case {
            "member_key" => ctx.origin = ContextOrigin::MemberKey,
            "other_origin" => ctx.origin = ContextOrigin::Other,
            "moved_department" => {
                let path = f.home.join("agents/a1/agent.toml");
                let raw = std::fs::read_to_string(&path).unwrap();
                assert!(raw.contains("department = 'sales'"));
                std::fs::write(path, raw.replace("department = 'sales'", "department = 'mfg'")).unwrap();
            }
            _ => {}
        }
        let frame = Box::pin(f.h.handle(method, params_for_agent(agent), &ctx)).await;
        // Include the case in failures emitted by the supplied outcome hook.
        check(&format!("{method} [{case}]"), frame, case == "allowed");
    }
}

fn assert_scoped_rpc_outcome(label: &str, frame: WsFrame, allowed: bool) {
    if allowed { assert_success(label, frame); }
    else { assert_permission_denied(label, frame); }
}

#[tokio::test]
#[ignore = "only cost rows remain pending their grant wiring"]
async fn scoped_method_matrix_rows_pending_wiring() {
    // Add cost rows using assert_scoped with record setup
    // hooks and, for filtered list methods, a payload visibility assertion.
    panic!("only cost matrix rows remain pending their wiring package");
}

fn scoped_payload(label: &str, frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: true, payload: Some(value), .. } => value,
        other => panic!("{label}: expected payload, got {other:?}"),
    }
}

fn check_schedule_list(label: &str, frame: WsFrame, allowed: bool, key: &str) {
    let payload = scoped_payload(label, frame);
    let rows = payload[key].as_array().expect("schedule rows");
    assert_eq!(rows.len(), usize::from(allowed), "{label}: {payload}");
    if allowed {
        assert_eq!(rows[0]["id"], "matrix-record", "{label}");
        assert_eq!(rows[0]["agent_id"], "a1", "{label}");
        assert_eq!(rows[0]["can_manage"], true, "{label}");
        if key == "tasks" {
            for flag in ["can_edit", "can_resume", "can_run"] {
                assert_eq!(rows[0][flag], true, "{label}: {flag}");
            }
        }
        if key == "schedules" {
            assert!(rows[0]["creator_user_id"].is_null(), "{label}");
            assert!(rows[0]["creator_name"].is_null(), "{label}");
        }
    }
}

fn seed_matrix_cron<'a>(f: &'a Fixture, agent: &'a str) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let store = Arc::new(CronStore::open(&f.home).unwrap());
        let mut row = CronTaskRow::new("matrix-record".into(), "Daily report".into(), agent.into(),
            "0 9 * * *".into(), "Prepare a daily report".into());
        row.created_by_user_id = Some(f.admin.user_id.clone());
        row.on_behalf_of = Some(format!("user:{}", f.admin.user_id));
        store.insert(&row).await.unwrap();
        f.h.set_cron_store(store).await;
    })
}

fn cron_add_params(agent: &str) -> Value {
    json!({"agent_id": agent, "name": "Department report", "cron": "0 9 * * *", "task": "Prepare a daily report"})
}

#[tokio::test]
async fn schedules_cron_scoped_matrix() {
    assert_scoped("cron.list", |_| json!({}), seed_matrix_cron,
        |label, frame, allowed| check_schedule_list(label, frame, allowed, "tasks")).await;
    assert_scoped("cron.add", cron_add_params, seed_matrix_cron, assert_scoped_rpc_outcome).await;
    for method in ["cron.update", "cron.pause", "cron.resume", "cron.remove"] {
        assert_scoped(method, |_| json!({"id": "matrix-record", "task": "Updated daily report"}),
            seed_matrix_cron, assert_scoped_rpc_outcome).await;
    }
    assert_scoped("cron.run_now", |_| json!({"id": "matrix-record"}), seed_matrix_cron,
        |label, frame, allowed| {
            if allowed {
                // This fixture has a store but no running scheduler. Reaching
                // this exact error proves every preceding admission gate passed.
                assert!(matches!(&frame, WsFrame::Response {
                    ok: false, error: Some(Value::String(error)), ..
                } if error == "Automation scheduler unavailable"), "{label}: {frame:?}");
            } else {
                assert_permission_denied(label, frame);
            }
        }).await;
}

fn seed_matrix_reminder<'a>(f: &'a Fixture, agent: &'a str) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let row = json!({"id": "matrix-record", "agent_id": agent,
            "trigger_at": "2099-01-01T00:00:00Z", "channel": "telegram", "chat_id": "private",
            "message": "Daily reminder", "on_behalf_of": format!("user:{}", f.admin.user_id)});
        std::fs::write(f.home.join("reminders.jsonl"), format!("{row}\n")).unwrap();
    })
}

#[tokio::test]
async fn schedules_reminders_scoped_matrix() {
    assert_scoped("reminders.list", |_| json!({}), seed_matrix_reminder,
        |label, frame, allowed| check_schedule_list(label, frame, allowed, "reminders")).await;
    assert_scoped("reminders.cancel", |_| json!({"id": "matrix-record"}),
        seed_matrix_reminder, assert_scoped_rpc_outcome).await;
}

fn seed_matrix_assign<'a>(f: &'a Fixture, agent: &'a str, paused: bool) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let store = Arc::new(TaskStore::open(&f.home).unwrap());
        let now = chrono::Utc::now().to_rfc3339();
        let row = crate::task_store::AssignScheduleRow {
            id: "matrix-record".into(), creator_user_id: f.admin.user_id.clone(),
            agent_id: agent.into(), kind: "task".into(), title: "Daily report".into(),
            payload_json: json!({"title": "Daily report"}).to_string(),
            schedule_json: json!({"kind": "daily", "hour": 9, "minute": 0}).to_string(),
            timezone: Some("UTC".into()), status: if paused { "paused" } else { "active" }.into(),
            pause_reason: paused.then(|| "by_user".into()), created_via: "dashboard".into(),
            source_channel: None, source_chat_id: None,
            next_fire_at: Some("2099-01-01T09:00:00Z".into()), last_fire_at: None,
            last_task_id: None, last_outcome: None, fire_count: 0, skip_count: 0,
            consecutive_failures: 0, created_at: now.clone(), updated_at: now,
        };
        assert!(store.insert_assign_schedule(&row, 20).await.unwrap());
        f.h.set_task_store(store).await;
    })
}

#[tokio::test]
async fn schedules_assign_scoped_matrix() {
    assert_scoped("assign_schedules.list", |_| json!({"scope": "accessible"}),
        |f, agent| seed_matrix_assign(f, agent, false),
        |label, frame, allowed| check_schedule_list(label, frame, allowed, "schedules")).await;
    for method in ["assign_schedules.pause", "assign_schedules.resume", "assign_schedules.remove"] {
        let paused = method == "assign_schedules.resume";
        assert_scoped(method, |_| json!({"id": "matrix-record"}),
            move |f, agent| seed_matrix_assign(f, agent, paused),
            |label, frame, allowed| {
                if allowed {
                    assert_success(label, frame);
                } else {
                    // Existing assignment controls deliberately hide inaccessible
                    // records behind not_found, including from non-granted mgrB.
                    assert!(matches!(&frame, WsFrame::Response {
                        ok: false, error: Some(Value::String(error)), ..
                    } if error == "not_found"), "{label}: {frame:?}");
                }
            }).await;
    }
}

fn schedule_grant_audits(f: &Fixture) -> Vec<(String, String, Value)> {
    let conn = rusqlite::Connection::open(f.home.join("users.db")).unwrap();
    let mut stmt = conn.prepare(
        "SELECT user_id, target, detail FROM auth_audit_log WHERE action='dept_grant_used' ORDER BY id"
    ).unwrap();
    let audits = stmt.query_map([], |row| {
        let detail: String = row.get(2)?;
        Ok((row.get(0)?, row.get(1)?, serde_json::from_str(&detail).unwrap()))
    }).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
    audits
}

#[tokio::test]
async fn schedules_cron_add_audits_only_grant_backed_writes() {
    let f = fixture().await;
    seed_matrix_cron(&f, "a1").await;
    let created = scoped_payload("cron.add",
        Box::pin(f.h.handle("cron.add", cron_add_params("a1"), &f.mgr_a)).await);
    assert_eq!(schedule_grant_audits(&f), vec![(f.mgr_a.user_id.clone(), "a1".into(),
        json!({"group": "schedules", "action": "create", "method": "cron.add"}))]);
    // Reads and a creator's narrowing controls never consume a grant.
    assert_success("cron.list", Box::pin(f.h.handle("cron.list", json!({}), &f.mgr_a)).await);
    for method in ["cron.pause", "cron.remove"] {
        assert_success(method, Box::pin(f.h.handle(method,
            json!({"id": created["task"]["id"]}), &f.mgr_a)).await);
    }
    assert_eq!(schedule_grant_audits(&f).len(), 1);
    // The same Manager still holds the department grant, but a live Operator
    // binding takes precedence and must not generate a grant-use audit.
    f.db.bind_agent(&f.mgr_a.user_id, "a1", AccessLevel::Operator).unwrap();
    assert_success("cron.add", Box::pin(f.h.handle("cron.add", cron_add_params("a1"), &f.mgr_a)).await);
    assert_eq!(schedule_grant_audits(&f).len(), 1);
}

#[tokio::test]
async fn schedules_reminder_and_assign_mutations_audit_the_checked_target() {
    let f = fixture().await;
    seed_matrix_reminder(&f, "a1").await;
    seed_matrix_assign(&f, "a1", false).await;
    for method in ["reminders.list", "assign_schedules.list"] {
        assert_success(method, Box::pin(f.h.handle(method, json!({"scope": "accessible"}), &f.mgr_a)).await);
    }
    assert!(schedule_grant_audits(&f).is_empty());
    for (method, action) in [("reminders.cancel", "remove"), ("assign_schedules.pause", "toggle"),
        ("assign_schedules.resume", "toggle"), ("assign_schedules.remove", "remove")] {
        assert_success(method, Box::pin(f.h.handle(method, json!({"id": "matrix-record"}), &f.mgr_a)).await);
        assert!(schedule_grant_audits(&f).contains(&(f.mgr_a.user_id.clone(), "a1".into(),
            json!({"group": "schedules", "action": action, "method": method}))));
    }
    assert_eq!(schedule_grant_audits(&f).len(), 4);
}


const MATRIX_SKILL: &str = "---\nname: matrix-skill\ndescription: matrix-skill fixture notes\n---\nWrite a short summary.\n";

fn skill_items(agent: &str) -> Value {
    json!({"items": [{"agent_id": agent, "skill_name": "matrix-skill"}], "enabled": false})
}

fn seed_matrix_skill<'a>(f: &'a Fixture, agent: &'a str) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        // The registry and archive use uppercase SKILLS on case-sensitive hosts.
        let dir = f.home.join("agents").join(agent).join("SKILLS/matrix-skill");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), MATRIX_SKILL).unwrap();
        f.h.registry().write().await.scan().await.unwrap();
    })
}

fn seed_matrix_archive<'a>(f: &'a Fixture, agent: &'a str) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        seed_matrix_skill(f, agent).await;
        let dir = f.home.join("agents").join(agent).join("SKILLS");
        let mut manifest = duduclaw_agent::skill_archive::archive(
            &f.home, agent, &dir, "matrix-skill", &f.admin.user_id,
        ).unwrap();
        // Give params_for_agent a deterministic, valid archive id.
        let root = duduclaw_agent::skill_archive::agent_archive_dir(&f.home, agent).unwrap();
        std::fs::rename(root.join(&manifest.archive_id), root.join("matrix-archive")).unwrap();
        manifest.archive_id = "matrix-archive".into();
        std::fs::write(root.join("matrix-archive/manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
    })
}

#[tokio::test]
async fn skills_read_scoped_matrix() {
    assert_scoped("skills.list", |_| json!({}), seed_matrix_skill,
        |label, frame, allowed| {
            let payload = scoped_payload(label, frame);
            let rows = payload["agents"].as_array().unwrap();
            assert_eq!(rows.iter().any(|r| r["agent_id"] == "a1"), allowed, "{label}: {payload}");
            assert!(!rows.iter().any(|r| r["agent_id"] == "b1"), "{label}: {payload}");
            if !allowed { assert!(rows.is_empty(), "{label}: {payload}"); }
        }).await;
    // Exercise the single-employee list refusal as well as aggregate visibility.
    assert_scoped("skills.list", |a| json!({"agent_id": a}), seed_matrix_skill,
        assert_scoped_rpc_outcome).await;
    assert_scoped("skills.search", |_| json!({"query": "matrix-skill"}),
        |f, agent| Box::pin(async move {
            seed_matrix_skill(f, agent).await;
            // A fresh unrelated market entry makes this RPC deterministic/offline.
            let config = f.home.join("config.toml");
            let raw = std::fs::read_to_string(&config).unwrap();
            std::fs::write(config, format!("{raw}\n[skill_hubs]\nenabled = ['anthropic-skills']\n")).unwrap();
            let cache = f.home.join("skill_hub_cache");
            std::fs::create_dir_all(&cache).unwrap();
            let index = json!({"updated_at": chrono::Utc::now().to_rfc3339(),
                "source": "anthropic-skills", "skills": [{"name": "unrelated-market-entry"}]});
            std::fs::write(cache.join("anthropic-skills.json"), index.to_string()).unwrap();
        }),
        |label, frame, allowed| {
            let payload = scoped_payload(label, frame);
            let rows = payload["skills"].as_array().unwrap();
            assert_eq!(rows.len(), usize::from(allowed), "{label}: {payload}");
            if allowed { assert_eq!(rows[0]["author"], "a1", "{label}"); }
        }).await;
    assert_scoped("skills.content", |a| json!({"agent_id": a, "skill_name": "matrix-skill"}),
        seed_matrix_skill, |label, frame, allowed| {
            if allowed {
                assert_eq!(scoped_payload(label, frame)["content"], MATRIX_SKILL, "{label}");
            } else { assert_permission_denied(label, frame); }
        }).await;
    assert_scoped("skills.archived", |a| json!({"agent_id": a}), seed_matrix_archive,
        |label, frame, allowed| {
            if allowed {
                let payload = scoped_payload(label, frame);
                assert_eq!(payload["archived"][0]["skill_name"], "matrix-skill", "{label}");
                assert_eq!(payload["can_restore"], true, "{label}");
            } else { assert_permission_denied(label, frame); }
        }).await;
}

#[tokio::test]
async fn skills_recent_scoped_matrix_preserves_callable_and_account_filters() {
    assert_scoped("skills.recent", |a| json!({"agent_id": a}),
        |f, agent| Box::pin(async move {
            seed_matrix_skill(f, agent).await;
            for ctx in [&f.mgr_a, &f.mgr_b, &f.emp] {
                crate::recent_skills::record(&f.home, &ctx.user_id, agent, &["matrix-skill".into()]).unwrap();
            }
        }),
        |label, frame, allowed| {
            if allowed {
                // The grant admits the agent visibility check. Personal history
                // still requires can_call_agent; a grant alone cannot run skills.
                assert_eq!(scoped_payload(label, frame)["skills"], json!([]), "{label}");
            } else { assert_permission_denied(label, frame); }
        }).await;
    let f = fixture().await;
    seed_matrix_skill(&f, "a1").await;
    crate::recent_skills::record(&f.home, &f.mgr_a.user_id, "a1", &["matrix-skill".into()]).unwrap();
    f.db.bind_agent(&f.mgr_a.user_id, "a1", AccessLevel::Viewer).unwrap();
    let p = scoped_payload("skills.recent",
        Box::pin(f.h.handle("skills.recent", json!({"agent_id": "a1"}), &f.mgr_a)).await);
    assert_eq!(p["skills"][0]["name"], "matrix-skill");
    for (params, ctx) in [(json!({"assigning": true}), &f.mgr_a),
        (json!({"user_id": f.mgr_a.user_id}), &f.emp)] {
        assert_eq!(scoped_payload("skills.recent",
            Box::pin(f.h.handle("skills.recent", params, ctx)).await)["skills"], json!([]));
    }
}

#[tokio::test]
async fn skills_write_scoped_matrix() {
    for method in ["skills.set_enabled", "skills.remove"] {
        assert_scoped(method, skill_items, seed_matrix_skill, |label, frame, allowed| {
            if allowed {
                let payload = scoped_payload(label, frame);
                assert_eq!(payload["results"][0]["ok"], true, "{label}: {payload}");
            } else { assert_permission_denied(label, frame); }
        }).await;
    }
    assert_scoped("skills.restore", |a| json!({"agent_id": a, "archive_id": "matrix-archive"}),
        seed_matrix_archive, |label, frame, allowed| {
            if allowed {
                assert_eq!(scoped_payload(label, frame)["restored"], true, "{label}");
            } else { assert_permission_denied(label, frame); }
        }).await;
    assert_scoped("skills.install_request", |a| json!({"scope": a, "content": MATRIX_SKILL}),
        seed_matrix_skill, |label, frame, allowed| {
            if allowed {
                let payload = scoped_payload(label, frame);
                assert_eq!(payload["status"], "pending", "{label}");
                assert_eq!(payload["stage"], "awaiting_admin", "{label}");
            } else { assert_permission_denied(label, frame); }
        }).await;
}

#[tokio::test]
async fn skills_mixed_targets_refuse_before_any_change() {
    let f = fixture().await;
    for agent in ["a1", "b1"] { seed_matrix_skill(&f, agent).await; }
    let before = std::fs::read(f.home.join("agents/a1/SKILLS/matrix-skill/SKILL.md")).unwrap();
    assert_permission_denied("skills.set_enabled", Box::pin(f.h.handle("skills.set_enabled",
        json!({"items": [
            {"agent_id": "a1", "skill_name": "matrix-skill"},
            {"agent_id": "b1", "skill_name": "matrix-skill"}], "enabled": false}), &f.mgr_a)).await);
    assert!(!duduclaw_agent::skill_state::read_disabled(&f.home, "a1").is_disabled("matrix-skill"));
    assert_eq!(std::fs::read(f.home.join("agents/a1/SKILLS/matrix-skill/SKILL.md")).unwrap(), before);
    assert!(schedule_grant_audits(&f).is_empty());
    // A binding on b1 may coexist with the grant on a1; every target now passes.
    f.db.bind_agent(&f.mgr_a.user_id, "b1", AccessLevel::Owner).unwrap();
    let p = scoped_payload("skills.set_enabled", Box::pin(f.h.handle("skills.set_enabled",
        json!({"items": [
            {"agent_id": "a1", "skill_name": "matrix-skill"},
            {"agent_id": "b1", "skill_name": "matrix-skill"}], "enabled": false}), &f.mgr_a)).await);
    assert!(p["results"].as_array().unwrap().iter().all(|r| r["ok"] == true));
    for agent in ["a1", "b1"] {
        assert!(duduclaw_agent::skill_state::read_disabled(&f.home, agent).is_disabled("matrix-skill"));
    }
    assert_eq!(schedule_grant_audits(&f), vec![(f.mgr_a.user_id.clone(), "a1".into(),
        json!({"group": "skills", "action": "toggle", "method": "skills.set_enabled"}))]);
}

#[tokio::test]
async fn skills_adopt_stays_owner_only_despite_department_grant() {
    let f = fixture().await;
    assert_permission_denied("skills.adopt", Box::pin(f.h.handle("skills.adopt",
        json!({"target_agent_id": "a1", "skill_name": "matrix-skill"}), &f.mgr_a)).await);
}

#[tokio::test]
async fn skills_remove_audits_grant_once_per_employee_but_not_live_owner() {
    let f = fixture().await;
    seed_matrix_skill(&f, "a1").await;
    let extra = f.home.join("agents/a1/SKILLS/matrix-extra");
    std::fs::create_dir_all(&extra).unwrap();
    std::fs::write(extra.join("SKILL.md"), MATRIX_SKILL.replace("matrix-skill", "matrix-extra")).unwrap();
    let p = scoped_payload("skills.remove", Box::pin(f.h.handle("skills.remove", json!({"items": [
        {"agent_id": "a1", "skill_name": "matrix-skill"},
        {"agent_id": "a1", "skill_name": "matrix-extra"}]}), &f.mgr_a)).await);
    assert!(p["results"].as_array().unwrap().iter().all(|r| r["ok"] == true));
    assert_eq!(schedule_grant_audits(&f), vec![(f.mgr_a.user_id.clone(), "a1".into(),
        json!({"group": "skills", "action": "remove", "method": "skills.remove"}))]);
    seed_matrix_skill(&f, "a1").await;
    f.db.bind_agent(&f.mgr_a.user_id, "a1", AccessLevel::Owner).unwrap();
    let p = scoped_payload("skills.remove",
        Box::pin(f.h.handle("skills.remove", skill_items("a1"), &f.mgr_a)).await);
    assert_eq!(p["results"][0]["ok"], true);
    assert_eq!(schedule_grant_audits(&f).len(), 1);
}

#[tokio::test]
async fn skills_list_flags_follow_grants_or_owner_not_viewer() {
    let f = fixture().await;
    seed_matrix_skill(&f, "a1").await;
    seed_matrix_skill(&f, "b1").await;
    f.db.bind_agent(&f.emp.user_id, "a1", AccessLevel::Viewer).unwrap();
    for (ctx, expected) in [(&f.mgr_a, true), (&f.emp, false)] {
        let p = scoped_payload("skills.list", Box::pin(f.h.handle("skills.list", json!({}), ctx)).await);
        let rows = p["agents"].as_array().unwrap();
        assert!(!rows.iter().any(|r| r["agent_id"] == "b1"));
        let a1 = rows.iter().find(|r| r["agent_id"] == "a1").unwrap();
        for flag in ["can_manage", "can_restore"] {
            assert_eq!(a1[flag], expected);
            let skill = a1["skills"].as_array().unwrap().iter().find(|r| r["name"] == "matrix-skill").unwrap();
            assert_eq!(skill[flag], expected);
        }
        let p = scoped_payload("skills.list",
            Box::pin(f.h.handle("skills.list", json!({"agent_id": "a1"}), ctx)).await);
        assert_eq!(p["can_manage"], expected);
        assert_eq!(p["can_restore"], expected);
    }
}

#[tokio::test]
async fn skills_install_request_audits_grant_and_routes_only_to_admin() {
    let f = fixture().await;
    let p = scoped_payload("skills.install_request", Box::pin(f.h.handle("skills.install_request",
        json!({"scope": "a1", "content": MATRIX_SKILL}), &f.mgr_a)).await);
    let store = crate::install_requests::InstallRequestStore::open(&f.home).unwrap();
    let request = store.get(p["request_id"].as_str().unwrap()).await.unwrap().unwrap();
    assert!(!request.needs_manager());
    assert_eq!(request.stage(), "awaiting_admin");
    let users = [&f.admin, &f.mgr_a, &f.mgr_b].into_iter()
        .map(|ctx| f.db.get_user(&ctx.user_id).unwrap().unwrap()).collect::<Vec<_>>();
    let approvers = crate::install_notify::approvers_for(&users, &request);
    assert_eq!(approvers.iter().map(|u| u.id.as_str()).collect::<Vec<_>>(), vec![f.admin.user_id.as_str()]);
    assert!(store.decide(&request.id, &f.mgr_b.user_id, "manager", None, true, "").await.is_err());
    assert_eq!(schedule_grant_audits(&f), vec![(f.mgr_a.user_id.clone(), "a1".into(),
        json!({"group": "skills", "action": "request_install", "method": "skills.install_request"}))]);
}
