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
        "autopilot.runs.cancel" => json!({"run_id": "matrix-run"}),
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
    assert_scoped_target(method, params_for_agent, setup,
        |label, _, frame, allowed| check(label, frame, allowed)).await;
}

async fn assert_scoped_target<P, S, C>(method: &str, params_for_agent: P, setup: S, check: C)
where
    P: Fn(&str) -> Value,
    S: for<'a> Fn(&'a Fixture, &'a str) -> Pin<Box<dyn Future<Output = ()> + 'a>>,
    C: Fn(&str, &str, WsFrame, bool),
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
        check(&format!("{method} [{case}]"), agent, frame, case == "allowed");
    }
}

fn assert_scoped_rpc_outcome(label: &str, frame: WsFrame, allowed: bool) {
    if allowed { assert_success(label, frame); }
    else { assert_permission_denied(label, frame); }
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

fn assert_heartbeat_scheduler_unavailable(label: &str, frame: WsFrame) {
    // The fixture has no scheduler: this exact non-permission error proves
    // admission passed, but does not claim a heartbeat was actually triggered.
    assert!(matches!(&frame, WsFrame::Response {
        ok: false, error: Some(Value::String(error)), ..
    } if error == "Heartbeat scheduler not started"), "{label}: {frame:?}");
}

#[tokio::test]
async fn schedules_heartbeat_scoped_matrix() {
    assert_scoped("heartbeat.trigger", |agent| json!({"agent_id": agent}),
        |_, _| Box::pin(async {}),
        |label, frame, allowed| {
            if allowed { assert_heartbeat_scheduler_unavailable(label, frame); }
            else { assert_permission_denied(label, frame); }
        }).await;
}

#[tokio::test]
async fn schedules_heartbeat_does_not_audit_failed_trigger() {
    let f = fixture().await;
    assert_heartbeat_scheduler_unavailable("heartbeat.trigger",
        Box::pin(f.h.handle("heartbeat.trigger", json!({"agent_id": "a1"}), &f.mgr_a)).await);
    assert!(schedule_grant_audits(&f).is_empty());
    // A live Operator binding takes precedence even while the grant exists.
    f.db.bind_agent(&f.mgr_a.user_id, "a1", AccessLevel::Operator).unwrap();
    for ctx in [&f.mgr_a, &f.admin] {
        assert_heartbeat_scheduler_unavailable("heartbeat.trigger",
            Box::pin(f.h.handle("heartbeat.trigger", json!({"agent_id": "a1"}), ctx)).await);
    }
    assert_permission_denied("heartbeat.trigger",
        Box::pin(f.h.handle("heartbeat.trigger", json!({"agent_id": "b1"}), &f.mgr_a)).await);
    assert!(schedule_grant_audits(&f).is_empty());
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
    seed_matrix_assign_by(f, agent, paused, &f.admin.user_id)
}

fn seed_matrix_assign_by<'a>(f: &'a Fixture, agent: &'a str, paused: bool, creator: &'a str)
    -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let store = Arc::new(TaskStore::open(&f.home).unwrap());
        let now = chrono::Utc::now().to_rfc3339();
        let row = crate::task_store::AssignScheduleRow {
            id: "matrix-record".into(), creator_user_id: creator.to_owned(),
            agent_id: agent.into(), kind: "task".into(), title: "Daily report".into(),
            payload_json: json!({"title": "Daily report", "description": "Summarize today's work."}).to_string(),
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
    assert_scoped_target("skills.list", |_| json!({}), seed_matrix_skill,
        |label, agent, frame, allowed| {
            let payload = scoped_payload(label, frame);
            let rows = payload["agents"].as_array().unwrap();
            assert_eq!(rows.iter().any(|r| r["agent_id"] == agent), allowed, "{label}: {payload}");
            assert!(!rows.iter().any(|r| r["agent_id"] == "b1"), "{label}: {payload}");
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


fn seed_matrix_budget_incidents<'a>(f: &'a Fixture, agent: &'a str) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let rows = [
            json!({"agent_id": agent, "event": "older"}),
            json!({"agent_id": agent, "event": "newer"}),
            json!({"agent_id": "b1", "event": "other-department"}),
            json!({"agent_id": "main", "event": "main"}),
            json!({"event": "unattributed"}),
        ];
        let raw = rows.iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
        std::fs::write(f.home.join("budget_events.jsonl"), raw).unwrap();
    })
}

#[tokio::test]
async fn cost_budget_incidents_scoped_matrix() {
    assert_scoped("budget.incidents", |_| json!({"limit": 1}), seed_matrix_budget_incidents,
        |label, frame, allowed| {
            // Employees still fail the Manager floor; eligible roles receive
            // only visible rows, including an empty list when no scope matches.
            if label.ends_with("[employee]") {
                assert_permission_denied(label, frame);
                return;
            }
            let payload = scoped_payload(label, frame);
            if allowed {
                assert_eq!(payload["incidents"], json!([{"agent_id": "a1", "event": "newer"}]), "{label}");
                assert_eq!(payload["by_agent"], json!([{"agent_id": "a1", "open_events": 2}]), "{label}");
            } else {
                assert_eq!(payload["incidents"], json!([]), "{label}");
                assert_eq!(payload["by_agent"], json!([]), "{label}");
            }
        }).await;
}

#[tokio::test]
async fn cost_budget_incidents_preserves_bindings_and_admin_scope() {
    let f = fixture().await;
    seed_matrix_budget_incidents(&f, "a1").await;
    for ctx in [&f.mgr_a, &f.mgr_b] {
        f.db.bind_agent(&ctx.user_id, "b1", AccessLevel::Viewer).unwrap();
        let payload = scoped_payload("budget.incidents",
            Box::pin(f.h.handle("budget.incidents", json!({"limit": 1}), ctx)).await);
        assert_eq!(payload["incidents"], json!([{"agent_id": "b1", "event": "other-department"}]));
        let mut counts = payload["by_agent"].as_array().unwrap().clone();
        counts.sort_by_key(|row| row["agent_id"].as_str().unwrap().to_owned());
        let expected = if ctx.user_id == f.mgr_a.user_id {
            json!([{"agent_id": "a1", "open_events": 2}, {"agent_id": "b1", "open_events": 1}])
        } else {
            json!([{"agent_id": "b1", "open_events": 1}])
        };
        assert_eq!(Value::Array(counts), expected);
    }
    let all = scoped_payload("budget.incidents",
        Box::pin(f.h.handle("budget.incidents", json!({}), &f.admin)).await);
    // Preserve the existing Admin-all path, including unattributed incidents.
    assert_eq!(all["incidents"].as_array().unwrap().len(), 5);
    assert_eq!(all["by_agent"].as_array().unwrap().len(), 3);
    assert!(schedule_grant_audits(&f).is_empty(), "reads must not emit grant-use audits");
}

async fn seed_matrix_cost(f: &Fixture) {
    use crate::cost_telemetry::{CostTelemetry, RequestType, TokenUsage};
    let telemetry = CostTelemetry::new(&f.home.join("matrix-cost.db")).unwrap();
    for (agent, cost) in [("a1", 10), ("a2", 20), ("b1", 100), ("main", 1000)] {
        telemetry.record_priced(agent, RequestType::Chat, "matrix-model",
            &TokenUsage { input_tokens: cost * 10, output_tokens: cost,
                cache_read_tokens: cost * 5, ..Default::default() },
            cost, false, None).await.unwrap();
    }
    assert!(f.h.cost_telemetry_fixture.set(telemetry).is_ok());
}

#[tokio::test]
async fn cost_department_scoped_matrix() {
    for method in ["cost.department_summary", "cost.department_recent"] {
        assert!(matches!(coverage_for(method), Some((_, Coverage::Scoped(_)))));
        for case in ["allowed", "other_department", "no_grants", "employee", "member_key",
            "other_origin", "moved_department"] {
            let f = fixture().await;
            seed_matrix_cost(&f).await;
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
                    std::fs::write(path, raw.replace("department = 'sales'", "department = 'mfg'")).unwrap();
                }
                _ => {}
            }
            let department = if case == "other_department" { "mfg" } else { "sales" };
            let label = format!("{method} [{case}]");
            let frame = Box::pin(f.h.handle(method, json!({"department": department}), &ctx)).await;
            if matches!(case, "allowed" | "moved_department") {
                let payload = scoped_payload(&label, frame);
                let key = if method == "cost.department_summary" { "agents" } else { "records" };
                let mut ids: Vec<_> = payload[key].as_array().unwrap().iter()
                    .map(|row| row["agent_id"].as_str().unwrap()).collect();
                ids.sort_unstable();
                let expected = if case == "allowed" { vec!["a1", "a2"] } else { vec!["a2"] };
                assert_eq!(ids, expected, "{label}");
                if case == "moved_department" && key == "agents" {
                    assert_eq!(payload["totals"]["total_cost_millicents"], 20);
                }
            } else {
                assert_permission_denied(&label, frame);
            }
        }
    }
}

#[tokio::test]
async fn department_cost_excludes_other_departments() {
    let f = fixture().await;
    seed_matrix_cost(&f).await;
    // Unequal request counts and efficiencies distinguish a weighted mean.
    f.h.cost_telemetry_fixture.get().unwrap().record_priced(
        "a2", crate::cost_telemetry::RequestType::Chat, "matrix-model",
        &crate::cost_telemetry::TokenUsage::default(), 0, false, None).await.unwrap();
    let payload = scoped_payload("cost.department_summary", Box::pin(f.h.handle(
        "cost.department_summary", json!({"department": "sales"}), &f.mgr_a)).await);
    assert_eq!(payload["available"], true);
    assert_eq!(payload["department"], "sales");
    assert_eq!(payload["days"], 30);
    let rows = payload["agents"].as_array().unwrap();
    assert_eq!(rows.iter().map(|r| r["agent_id"].as_str().unwrap()).collect::<Vec<_>>(), vec!["a1", "a2"]);
    assert!(rows.iter().all(|r| r["display_name"].is_string()));
    for field in ["total_requests", "total_input_tokens", "total_output_tokens",
        "total_cost_millicents", "total_cache_savings_millicents"] {
        let sum: u64 = rows.iter().map(|r| r[field].as_u64().unwrap()).sum();
        assert_eq!(payload["totals"][field].as_u64(), Some(sum), "{field}");
    }
    let requests: u64 = rows.iter().map(|r| r["total_requests"].as_u64().unwrap()).sum();
    assert_eq!(requests, 3);
    let efficiency: f64 = rows.iter().map(|r|
        r["avg_cache_efficiency"].as_f64().unwrap() * r["total_requests"].as_u64().unwrap() as f64
    ).sum::<f64>() / requests as f64;
    let actual = payload["totals"]["avg_cache_efficiency"].as_f64().unwrap();
    assert!((actual - efficiency).abs() < 1e-12);
    assert_eq!(payload["totals"]["total_cost_millicents"], 30);
    let recent = scoped_payload("cost.department_recent", Box::pin(f.h.handle(
        "cost.department_recent", json!({"department": "sales", "limit": 1}), &f.mgr_a)).await);
    let records = recent["records"].as_array().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["agent_id"], "a2");
    for field in ["request_type", "model", "input_tokens", "cache_read_tokens", "cache_creation_tokens",
        "output_tokens", "cache_efficiency", "cost_millicents", "cache_savings_millicents", "created_at"] {
        assert!(records[0].get(field).is_some(), "{field}");
    }
}

#[tokio::test]
async fn cost_by_model_scoped_matrix() {
    assert_scoped("cost.by_model", |agent| json!({"agent_id": agent}),
        |f, _| Box::pin(seed_matrix_cost(f)), assert_scoped_rpc_outcome).await;
    let f = fixture().await;
    seed_matrix_cost(&f).await;
    for params in [json!({}), json!({"agent_id": ""}), json!({"agent_id": " a1 "}),
        json!({"agent_id": "b1"})] {
        assert_permission_denied("cost.by_model", Box::pin(f.h.handle("cost.by_model", params, &f.mgr_a)).await);
    }
    let payload = scoped_payload("cost.by_model", Box::pin(f.h.handle(
        "cost.by_model", json!({"agent_id": "a1"}), &f.mgr_a)).await);
    assert_eq!(payload["agent_id"], "a1");
    assert_eq!(payload["by_model"][0]["cost_millicents"], 10);
    assert_success("cost.by_model", Box::pin(f.h.handle("cost.by_model", json!({}), &f.admin)).await);
}

#[tokio::test]
async fn department_cost_validates_department_and_preserves_admin_scope() {
    let f = fixture().await;
    seed_matrix_cost(&f).await;
    for method in ["cost.department_summary", "cost.department_recent"] {
        for ctx in [&f.mgr_a, &f.admin] {
            for params in [json!({}), json!({"department": null}), json!({"department": 7}),
                json!({"department": ""}), json!({"department": " sales "}), json!({"department": "../sales"})] {
                let frame = Box::pin(f.h.handle(method, params, ctx)).await;
                assert!(matches!(frame, WsFrame::Response { ok: false, .. }), "{method}: {frame:?}");
            }
            for department in ["missing", "Sales", "sale"] {
                assert_permission_denied(method, Box::pin(f.h.handle(method,
                    json!({"department": department}), ctx)).await);
            }
        }
        let key = if method == "cost.department_summary" { "agents" } else { "records" };
        for (department, expected) in [("sales", vec!["a1", "a2", "main"]), ("mfg", vec!["b1"])] {
            let payload = scoped_payload(method, Box::pin(f.h.handle(method,
                json!({"department": department}), &f.admin)).await);
            let mut ids: Vec<_> = payload[key].as_array().unwrap().iter()
                .map(|row| row["agent_id"].as_str().unwrap()).collect();
            ids.sort_unstable();
            assert_eq!(ids, expected);
        }
    }
    for (days, expected) in [(0, 1), (999, 90)] {
        let payload = scoped_payload("cost.department_summary", Box::pin(f.h.handle(
            "cost.department_summary", json!({"department": "sales", "days": days}), &f.mgr_a)).await);
        assert_eq!(payload["days"], expected);
    }
    let payload = scoped_payload("cost.department_recent", Box::pin(f.h.handle(
        "cost.department_recent", json!({"department": "sales", "limit": 0}), &f.mgr_a)).await);
    assert_eq!(payload["records"].as_array().unwrap().len(), 1);
    std::fs::write(f.home.join("org.toml"), "malformed !").unwrap();
    for ctx in [&f.mgr_a, &f.admin] {
        for method in ["cost.department_summary", "cost.department_recent"] {
            assert_permission_denied(method, Box::pin(f.h.handle(method,
                json!({"department": "sales"}), ctx)).await);
        }
    }
}


#[tokio::test]
async fn department_cost_empty_scope_never_falls_back_to_global() {
    let f = fixture().await;
    seed_matrix_cost(&f).await;
    for id in ["a1", "a2"] {
        let path = f.home.join("agents").join(id).join("agent.toml");
        let raw = std::fs::read_to_string(&path).unwrap();
        std::fs::write(path, raw.replace("department = 'sales'", "department = 'mfg'")).unwrap();
    }
    for (method, key) in [("cost.department_summary", "agents"), ("cost.department_recent", "records")] {
        let payload = scoped_payload(method, Box::pin(f.h.handle(method,
            json!({"department": "sales"}), &f.mgr_a)).await);
        assert_eq!(payload["available"], true);
        assert!(payload[key].as_array().unwrap().is_empty());
        if key == "agents" {
            assert_eq!(payload["totals"]["total_requests"], 0);
            assert_eq!(payload["totals"]["avg_cache_efficiency"].as_f64(), Some(0.0));
            assert_eq!(payload["totals"]["total_cost_millicents"], 0);
        }
    }
}

#[tokio::test]
async fn department_cost_recent_default_and_upper_limit() {
    use crate::cost_telemetry::{RequestType, TokenUsage};
    let f = fixture().await;
    seed_matrix_cost(&f).await;
    let telemetry = f.h.cost_telemetry_fixture.get().unwrap();
    for _ in 0..201 {
        telemetry.record_priced("a1", RequestType::Chat, "matrix-model",
            &TokenUsage::default(), 1, false, None).await.unwrap();
    }
    for (params, expected) in [(json!({"department": "sales"}), 50),
        (json!({"department": "sales", "limit": 999}), 200)] {
        let payload = scoped_payload("cost.department_recent", Box::pin(f.h.handle(
            "cost.department_recent", params, &f.mgr_a)).await);
        let rows = payload["records"].as_array().unwrap();
        assert_eq!(rows.len(), expected);
        assert!(rows.iter().all(|row| row["agent_id"] == "a1"));
    }
}

#[tokio::test]
async fn creator_access_lost_cron_is_display_only_and_fresh_after_revocation() {
    let f = fixture().await;
    let store = Arc::new(CronStore::open(&f.home).unwrap());
    let legacy = CronTaskRow::new("legacy".into(), "Legacy report".into(), "a1".into(),
        "0 9 * * *".into(), "Prepare report".into());
    store.insert(&legacy).await.unwrap();
    f.h.set_cron_store(store.clone()).await;
    let created = scoped_payload("cron.add",
        Box::pin(f.h.handle("cron.add", cron_add_params("a1"), &f.mgr_a)).await);
    let id = created["task"]["id"].as_str().unwrap();
    for expected in [false, true] {
        if expected {
            f.db.set_dept_grants("admin", "mgrA", "sales", &[], false).unwrap();
        }
        for ctx in [&f.admin, &f.mgr_a] {
            let listed = scoped_payload("cron.list", Box::pin(f.h.handle("cron.list", json!({}), ctx)).await);
            let row = listed["tasks"].as_array().unwrap().iter().find(|r| r["id"] == id).unwrap();
            assert_eq!(row["creator_access_lost"], expected);
            assert_eq!(row["enabled"], true);
            if ctx.is_admin() {
                let legacy = listed["tasks"].as_array().unwrap().iter().find(|r| r["id"] == "legacy").unwrap();
                assert_eq!(legacy["creator_access_lost"], false);
            }
        }
        assert!(store.get(id).await.unwrap().unwrap().enabled);
    }
    // A broken account store must not be labelled either authorized or revoked.
    rusqlite::Connection::open(f.home.join("users.db")).unwrap()
        .execute("DROP TABLE department_manager_grants", []).unwrap();
    let listed = scoped_payload("cron.list", f.h.handle_cron_list().await);
    let row = listed["tasks"].as_array().unwrap().iter().find(|r| r["id"] == id).unwrap();
    assert_eq!(row.get("creator_access_lost"), Some(&Value::Null));
    assert!(store.get(id).await.unwrap().unwrap().enabled);
}

#[tokio::test]
async fn creator_access_lost_assign_schedules_keep_running_after_revocation() {
    for creator in ["mgrA", ""] {
        let f = fixture().await;
        seed_matrix_assign_by(&f, "a1", false, creator).await;
        for revoked in [false, true] {
            if revoked {
                f.db.set_dept_grants("admin", "mgrA", "sales", &[], false).unwrap();
            }
            let listed = scoped_payload("assign_schedules.list", Box::pin(f.h.handle(
                "assign_schedules.list", json!({"scope": "all"}), &f.admin)).await);
            assert_eq!(listed["schedules"][0]["creator_access_lost"], revoked && !creator.is_empty());
            assert_eq!(listed["schedules"][0]["status"], "active");
            if !creator.is_empty() {
                let own = scoped_payload("assign_schedules.list", Box::pin(f.h.handle(
                    "assign_schedules.list", json!({}), &f.mgr_a)).await);
                assert_eq!(own["schedules"][0]["creator_access_lost"], revoked);
                assert_eq!(own["schedules"][0]["status"], "active");
            }
            let row = f.h.task_store().await.unwrap().get_assign_schedule("matrix-record").await.unwrap().unwrap();
            assert_eq!(row.status, "active");
        }
    }
}

#[path = "dept_automation_tests.rs"]
mod automation;
