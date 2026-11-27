//! `tasks.skill_assign` (交辦任務 page 「一般」 tab) and the `attachments`
//! param of `tasks.goal_create`, end to end through `MethodHandler::handle`
//! with a real `users.db` and task store.
//!
//! Skill names carry a `zzsa-` prefix: the usage index reads the real events
//! directory, so a name nobody else uses keeps every holder "never ran".
use super::*;
use base64::Engine;

const SKILL: &str = "zzsa-quote";

fn ok(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => p.clone(),
        other => panic!("expected ok, got {other:?}"),
    }
}

fn err(frame: &WsFrame) -> String {
    match frame {
        WsFrame::Response {
            ok: false,
            error: Some(e),
            ..
        } => e.as_str().unwrap_or_default().to_string(),
        other => panic!("expected error, got {other:?}"),
    }
}

fn ctx(id: &str, role: UserRole, access: &[(&str, AccessLevel)]) -> UserContext {
    UserContext {
        origin: duduclaw_auth::acl::ContextOrigin::Other,
        member_key_id: None,
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

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

struct Fx {
    _dir: tempfile::TempDir,
    home: PathBuf,
    h: MethodHandler,
    store: Arc<TaskStore>,
    admin: UserContext,
    owner_a: UserContext,
    viewer_a: UserContext,
    operator_b: UserContext,
}

/// Employees a1 (Alice) and b1 (Bob), both loading the company-wide skill
/// `zzsa-quote`. Accounts: an Admin, a1's Owner, a1's Viewer, b1's Operator.
async fn fixture() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    duduclaw_core::write_minimal_config(&home, "127.0.0.1", 18889).unwrap();
    let h = MethodHandler::new(home.clone()).await;
    let system = UserContext::admin_fallback();
    for (id, name) in [("a1", "Alice"), ("b1", "Bob")] {
        ok(&h
            .handle_agents_create_as(json!({ "name": id, "display_name": name }), Some(&system))
            .await);
    }
    std::fs::create_dir_all(home.join("skills")).unwrap();
    std::fs::write(
        home.join(format!("skills/{SKILL}.md")),
        format!("---\nname: {SKILL}\ndescription: 產出一份報價單\n---\n照公司格式做報價單，金額含稅。\n"),
    )
    .unwrap();
    h.registry.write().await.scan().await.unwrap();
    let store = Arc::new(TaskStore::open(&home).unwrap());
    h.set_task_store(store.clone()).await;
    let db = duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();
    let add = |id: &str, role: UserRole, binds: &[(&str, AccessLevel)]| {
        let u = db
            .create_user(
                &format!("{id}@example.invalid"),
                id,
                "isolated-test-password",
                role,
            )
            .unwrap();
        rusqlite::Connection::open(home.join("users.db"))
            .unwrap()
            .execute(
                "UPDATE users SET id=?1 WHERE id=?2",
                rusqlite::params![id, u.id],
            )
            .unwrap();
        for (a, l) in binds {
            db.bind_agent(id, a, *l).unwrap();
        }
        ctx(id, role, binds)
    };
    let admin = add("admin-1", UserRole::Admin, &[]);
    let owner_a = add("owner-a", UserRole::Employee, &[("a1", AccessLevel::Owner)]);
    let viewer_a = add(
        "viewer-a",
        UserRole::Employee,
        &[("a1", AccessLevel::Viewer)],
    );
    let operator_b = add(
        "operator-b",
        UserRole::Employee,
        &[("b1", AccessLevel::Operator)],
    );
    Fx {
        _dir: dir,
        home,
        h,
        store,
        admin,
        owner_a,
        viewer_a,
        operator_b,
    }
}

fn disable(f: &Fx, agent: &str) {
    duduclaw_agent::skill_state::set_disabled(&f.home, agent, &[SKILL.to_string()], true).unwrap();
}

async fn assign(f: &Fx, params: Value, who: &UserContext) -> WsFrame {
    f.h.handle("tasks.skill_assign", params, who).await
}

async fn task_count(f: &Fx) -> usize {
    f.store.list_tasks(None, None, None).await.unwrap().len()
}

fn attachment_files(f: &Fx, agent: &str) -> Vec<PathBuf> {
    match std::fs::read_dir(f.home.join("agents").join(agent).join("attachments")) {
        Ok(rd) => rd.filter_map(|e| e.ok().map(|e| e.path())).collect(),
        Err(_) => Vec::new(),
    }
}

#[tokio::test]
async fn skill_assign_refuses_a_deleted_runner_from_a_stale_registry() {
    let f = fixture().await;
    disable(&f, "b1");
    let path = f.home.join("agents/a1/agent.toml");
    let profile = std::fs::read_to_string(&path).unwrap();
    let mut profile: toml::Value = toml::from_str(&profile).unwrap();
    profile["agent"]["status"] = toml::Value::String("deleted".into());
    std::fs::write(path, toml::to_string(&profile).unwrap()).unwrap();
    // Keep the registry's active snapshot: admission must inspect disk.
    let frame = assign(&f, json!({"skill_name": SKILL}), &f.owner_a).await;
    match frame {
        WsFrame::Response { ok: false, error: Some(error), .. } => {
            assert_eq!(error["code"], duduclaw_core::agent_seats::TASK_ASSIGNEE_ERROR_CODE);
            assert_eq!(error["message"], duduclaw_core::agent_seats::TASK_ASSIGNEE_ERROR_MESSAGE);
        }
        other => panic!("expected assignee refusal, got {other:?}"),
    }
    assert_eq!(task_count(&f).await, 0);
    assert!(attachment_files(&f, "a1").is_empty());
}

#[tokio::test]
async fn never_run_goes_to_the_first_enabled_holder_with_the_skill_text() {
    let f = fixture().await;
    let p = ok(&assign(
        &f,
        json!({ "skill_name": SKILL, "note": "給王先生\n三天內" }),
        &f.admin,
    )
    .await);
    assert_eq!(p["runner"]["agent_id"], "a1");
    assert_eq!(p["runner"]["display_name"], "Alice");
    assert_eq!(p["skill"]["name"], SKILL);
    assert!(p["iteration_cap"].as_u64().is_some());
    assert!(p["dispatch_enabled"].is_boolean());
    assert_eq!(p["plan_first"], false);
    let id = p["task"]["id"].as_str().unwrap();
    let row = f.store.get_task(id).await.unwrap().unwrap();
    assert_eq!(row.assigned_to, "a1");
    assert!(row.goal_mode);
    assert!(
        row.description.starts_with(&format!(
            "技能「{SKILL}」：給王先生 三天內\n技能 ID：{SKILL}"
        )),
        "{}",
        row.description
    );
    // The round prompt is the description, so the skill's own text is in it.
    assert!(
        row.description.contains("照公司格式做報價單，金額含稅。"),
        "{}",
        row.description
    );
    let criteria = row.acceptance_criteria_baseline.unwrap();
    assert!(
        criteria.contains("產出一份報價單") && criteria.contains("補充要求：給王先生 三天內"),
        "{criteria}"
    );
}

#[tokio::test]
async fn recent_skills_record_only_successful_assignments_for_the_caller() {
    let f = fixture().await;
    let _ = err(&assign(&f, json!({ "skill_name": SKILL }), &f.viewer_a).await);
    assert!(crate::recent_skills::list(&f.home, &f.viewer_a.user_id).unwrap().is_empty());
    ok(&assign(&f, json!({ "skill_name": SKILL }), &f.owner_a).await);
    assert_eq!(crate::recent_skills::list(&f.home, &f.owner_a.user_id).unwrap(), vec![("a1".into(), SKILL.into())]);
    assert!(crate::recent_skills::list(&f.home, &f.admin.user_id).unwrap().is_empty());
}

#[tokio::test]
async fn recent_skills_multiple_selection_inlines_every_skill_on_one_authorized_runner() {
    let f = fixture().await;
    std::fs::write(f.home.join("agents/a1/SKILLS/zzsa-check.md"),
        "---\nname: zzsa-check\ndescription: Check totals\n---\nVerify each total twice.").unwrap();
    let p = ok(&assign(&f, json!({ "skill_name": SKILL, "skill_names": [SKILL, "zzsa-check"],
        "note": "/zzsa-quote /zzsa-check extra details" }), &f.owner_a).await);
    let task = f.store.get_task(p["task"]["id"].as_str().unwrap()).await.unwrap().unwrap();
    assert!(task.description.contains("照公司格式做報價單"));
    assert!(task.description.contains("Verify each total twice."));
    assert_eq!(crate::recent_skills::list(&f.home, &f.owner_a.user_id).unwrap().len(), 2);
    let _ = err(&assign(&f, json!({ "skill_name": SKILL, "skill_names": ["zzsa-check"] }), &f.operator_b).await);
    assert!(crate::recent_skills::list(&f.home, &f.operator_b.user_id).unwrap().is_empty());
    assert_eq!(task_count(&f).await, 1);
}

#[tokio::test]
async fn a_switched_off_holder_is_skipped_and_nobody_enabled_is_refused() {
    let f = fixture().await;
    disable(&f, "a1");
    let p = ok(&assign(&f, json!({ "skill_name": SKILL }), &f.admin).await);
    assert_eq!(p["runner"]["agent_id"], "b1");
    disable(&f, "b1");
    let e = err(&assign(&f, json!({ "skill_name": SKILL }), &f.admin).await);
    assert!(e.starts_with("skill_not_enabled"), "{e}");
}

#[tokio::test]
async fn unknown_and_malformed_skill_names_are_refused() {
    let f = fixture().await;
    let e = err(&assign(
        &f,
        json!({ "skill_name": "zzsa-nobody-has-this" }),
        &f.admin,
    )
    .await);
    assert!(e.starts_with("skill_not_found"), "{e}");
    for bad in [
        "",
        " zzsa-quote",
        "../zzsa-quote",
        "a/b",
        "a\\b",
        ".hidden",
        "..",
    ] {
        let e = err(&assign(&f, json!({ "skill_name": bad }), &f.admin).await);
        assert_eq!(e, "valid skill_name is required", "{bad:?}");
    }
    let e = err(&assign(&f, json!({}), &f.admin).await);
    assert_eq!(e, "valid skill_name is required");
    let long = "字".repeat(501);
    let e = err(&assign(&f, json!({ "skill_name": SKILL, "note": long }), &f.admin).await);
    assert!(e.starts_with("note_too_long"), "{e}");
    assert_eq!(task_count(&f).await, 0);
}

#[tokio::test]
async fn the_right_to_assign_matches_goal_create() {
    let f = fixture().await;
    // a1's Viewer sees a1 hold the skill but may not hand it work — the
    // same answer goal_create gives for a1.
    assert_eq!(
        err(&assign(&f, json!({ "skill_name": SKILL }), &f.viewer_a).await),
        "permission denied"
    );
    let goal =
        f.h.handle(
            "tasks.goal_create",
            json!({ "agent_id": "a1", "description": "做報價" }),
            &f.viewer_a,
        )
        .await;
    assert_eq!(err(&goal), "permission denied");
    // a1's Owner and b1's Operator each get their own employee.
    assert_eq!(
        ok(&assign(&f, json!({ "skill_name": SKILL }), &f.owner_a).await)["runner"]["agent_id"],
        "a1"
    );
    assert_eq!(
        ok(&assign(&f, json!({ "skill_name": SKILL }), &f.operator_b).await)["runner"]["agent_id"],
        "b1"
    );
    // With a1 switched off, a1's Owner sees no enabled holder (b1 is not
    // theirs to see) — never routed to an employee outside their reach.
    disable(&f, "a1");
    let e = err(&assign(&f, json!({ "skill_name": SKILL }), &f.owner_a).await);
    assert!(e.starts_with("skill_not_enabled"), "{e}");
    assert_eq!(task_count(&f).await, 2);
}

#[tokio::test]
async fn files_land_in_the_runners_area_and_bad_files_refuse_everything() {
    let f = fixture().await;
    let good = json!({ "skill_name": SKILL, "attachments": [
        { "name": "需求.txt", "mime": "text/plain", "data_base64": b64("規格".as_bytes()) },
        { "name": "logo.png", "mime": "image/png", "data_base64": b64(&[0x89, 0x50, 0x4E, 0x47]) },
    ]});
    let p = ok(&assign(&f, good, &f.owner_a).await);
    assert_eq!(p["attachments_saved"], 2);
    let saved = attachment_files(&f, "a1");
    assert_eq!(saved.len(), 2);
    let row = f
        .store
        .get_task(p["task"]["id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(row.description.contains("附件"), "{}", row.description);
    assert!(
        row.description.contains("- 需求.txt："),
        "{}",
        row.description
    );
    for path in &saved {
        assert!(
            row.description.contains(&path.display().to_string()),
            "{}",
            row.description
        );
    }

    let bad = json!({ "skill_name": SKILL, "attachments": [
        { "name": "a.txt", "data_base64": b64(b"ok") },
        { "name": "b.txt", "data_base64": "%%% not base64 %%%" },
    ]});
    assert!(err(&assign(&f, bad, &f.owner_a).await).starts_with("attachments_invalid"));
    let many: Vec<Value> = (0..11)
        .map(|i| json!({ "name": format!("{i}.txt"), "data_base64": b64(b"x") }))
        .collect();
    assert!(err(&assign(
        &f,
        json!({ "skill_name": SKILL, "attachments": many }),
        &f.owner_a
    )
    .await)
    .starts_with("attachments_too_many"));
    assert_eq!(task_count(&f).await, 1, "a refused request creates nothing");
    assert_eq!(attachment_files(&f, "a1").len(), 2, "and writes no file");
}

#[tokio::test]
async fn goal_create_saves_files_and_lists_them_in_the_description() {
    let f = fixture().await;
    let p = ok(&f
        .h
        .handle(
            "tasks.goal_create",
            json!({ "agent_id": "a1", "description": "整理附件裡的名單", "attachments": [
                { "name": "名單.csv", "mime": "text/csv", "data_base64": b64(b"a,b\n1,2\n") },
            ]}),
            &f.owner_a,
        )
        .await);
    assert_eq!(p["attachments_saved"], 1);
    let saved = attachment_files(&f, "a1");
    assert_eq!(saved.len(), 1);
    assert_eq!(std::fs::read(&saved[0]).unwrap(), b"a,b\n1,2\n");
    let row = f
        .store
        .get_task(p["task"]["id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(
        row.description.starts_with("整理附件裡的名單\n\n附件"),
        "{}",
        row.description
    );
    assert!(row.description.contains(&saved[0].display().to_string()));
    // Criteria default to the goal text alone (live test 2026-10-28: the file
    // list had become criteria C2/C3); the list stays in the description.
    assert_eq!(
        row.acceptance_criteria_baseline.as_deref(),
        Some("整理附件裡的名單")
    );
    assert_eq!(row.title, "整理附件裡的名單");
}

#[tokio::test]
async fn goal_create_refuses_bad_files_before_writing_anything() {
    let f = fixture().await;
    let bad = f
        .h
        .handle(
            "tasks.goal_create",
            json!({ "agent_id": "a1", "description": "x", "attachments": [{ "name": "a", "data_base64": "@@" }] }),
            &f.owner_a,
        )
        .await;
    assert!(err(&bad).starts_with("attachments_invalid"));
    let blank = f
        .h
        .handle(
            "tasks.goal_create",
            json!({ "agent_id": "a1", "description": "  ", "attachments": [{ "name": "a", "data_base64": b64(b"1") }] }),
            &f.owner_a,
        )
        .await;
    assert_eq!(err(&blank), "description is required");
    // The right to assign is checked before any file is looked at.
    let denied = f
        .h
        .handle(
            "tasks.goal_create",
            json!({ "agent_id": "a1", "description": "x", "attachments": [{ "name": "a", "data_base64": b64(b"1") }] }),
            &f.viewer_a,
        )
        .await;
    assert_eq!(err(&denied), "permission denied");
    // A goal the core refuses (bad priority) removes the files it saved.
    let refused =
        f.h.handle(
            "tasks.goal_create",
            json!({ "agent_id": "a1", "description": "x", "priority": "whenever",
                    "attachments": [{ "name": "a", "data_base64": b64(b"1") }] }),
            &f.owner_a,
        )
        .await;
    assert!(err(&refused).contains("invalid priority"));
    // No attachment folder is made for an employee that does not exist.
    let ghost =
        f.h.handle(
            "tasks.goal_create",
            json!({ "agent_id": "ghost", "description": "x",
                    "attachments": [{ "name": "a", "data_base64": b64(b"1") }] }),
            &f.admin,
        )
        .await;
    assert!(err(&ghost).contains("agent_not_found"), "{}", err(&ghost));
    assert!(!f.home.join("agents/ghost").exists());
    assert_eq!(task_count(&f).await, 0);
    assert!(attachment_files(&f, "a1").is_empty());
}

#[tokio::test]
async fn a_scheduled_goal_saves_its_files_once() {
    let f = fixture().await;
    let p = ok(&f
        .h
        .handle(
            "tasks.goal_create",
            json!({
                "agent_id": "a1",
                "description": "每週整理名單",
                "schedule": { "kind": "weekly", "days": [1], "time": "09:00" },
                "schedule_timezone": "Asia/Taipei",
                "attachments": [{ "name": "名單.csv", "data_base64": b64(b"1") }],
            }),
            &f.owner_a,
        )
        .await);
    assert_eq!(p["schedule"]["agent_id"], "a1");
    assert_eq!(attachment_files(&f, "a1").len(), 1);
    assert_eq!(task_count(&f).await, 0, "nothing starts now");
}

#[tokio::test(flavor = "current_thread")]
async fn assignment_rpcs_refuse_broken_settings_before_attachments_or_planning() {
    for broken in [false, true] {
        let f = fixture().await;
        disable(&f, "b1");
        let path = {
            let reg = f.h.registry.read().await;
            crate::entry_gate::resolve_and_check(&f.home, &*reg, "a1").unwrap().config_path
        };
        let mut doc: toml::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        doc["model"]["preferred"] = toml::Value::String("gpt-5.4".into());
        doc.as_table_mut().unwrap().insert("runtime".into(), toml::Value::Table(toml::map::Map::from_iter([("provider".into(), toml::Value::String("grok".into()))])));
        std::fs::write(&path, if broken { "[model".into() } else { toml::to_string(&doc).unwrap() }).unwrap();
        let code = if broken { "agent_config_unreadable" } else { "model_provider_mismatch" };
        let before = crate::model_call_probe::calls();
        for (method, params) in [
            ("tasks.skill_assign", json!({"skill_name":SKILL,"attachments":[{"filename":"input.txt","data_base64":b64(b"input")}]})),
            ("tasks.goal_create", json!({"agent_id":"a1","description":"Report","plan_first":true,"attachments":[{"filename":"input.txt","data_base64":b64(b"input")}]})),
            ("tasks.goal_create", json!({"agent_id":"a1","description":"Report","schedule":{"kind":"weekly","days":[1],"hour":9,"minute":0},"attachments":[{"filename":"input.txt","data_base64":b64(b"input")}]})),
        ] {
            let frame = f.h.handle(method, params, &f.owner_a).await;
            let text = err(&frame);
            assert!(text.contains(code), "{method}: {frame:?}");
            assert!(!text.contains("agent.toml"));
            assert_eq!(task_count(&f).await, 0);
            assert!(attachment_files(&f, "a1").is_empty());
            assert_eq!(crate::model_call_probe::calls(), before);
        }
        let db = rusqlite::Connection::open(f.home.join("tasks.db")).unwrap();
        let count: i64 = db.query_row("SELECT COUNT(*) FROM assign_schedules", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn webchat_reachable_decision_rpcs_leave_broken_tasks_and_approvals_untouched() {
    for broken in [false, true] {
        let f = fixture().await;
        let created = crate::goal_create_core::create_goal_task(&f.home, &f.store,
            crate::goal_create_core::GoalCreateRequest { agent_id:"a1".into(), description:"Report".into(),
                created_by:"goal:dashboard".into(), ..Default::default() }).await.unwrap();
        let task_id = &created.task.id;
        let db = rusqlite::Connection::open(f.home.join("tasks.db")).unwrap();
        db.execute("UPDATE tasks SET status='needs_human' WHERE id=?1", [task_id]).unwrap();
        let broker = crate::approval::ApprovalBroker::open(&f.home).unwrap();
        let id = broker.request("a1", "goal_kickoff", "Start", json!({"task_id":task_id}), 600).await.unwrap();
        let path = { let reg = f.h.registry.read().await;
            crate::entry_gate::resolve_and_check(&f.home, &*reg, "a1").unwrap().config_path };
        let mut doc: toml::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        doc["model"]["preferred"] = toml::Value::String("gpt-5.4".into());
        doc.as_table_mut().unwrap().insert("runtime".into(), toml::Value::Table(toml::map::Map::from_iter([("provider".into(), toml::Value::String("grok".into()))])));
        std::fs::write(path, if broken { "[model".into() } else { toml::to_string(&doc).unwrap() }).unwrap();
        let before = crate::model_call_probe::calls();
        let code = if broken { "agent_config_unreadable" } else { "model_provider_mismatch" };
        for (method, params) in [
            ("tasks.goal_decide", json!({"task_id":task_id,"action":"retry"})),
            ("approvals.decide", json!({"id":id.as_str(),"approve":true})),
        ] {
            let frame = f.h.handle(method, params, &f.admin).await;
            assert!(err(&frame).contains(code), "{frame:?}");
            assert_eq!(f.store.get_task(task_id).await.unwrap().unwrap().status, "needs_human");
            assert_eq!(broker.get(&id).await.unwrap().unwrap().status, crate::approval::ApprovalStatus::Pending);
        }
        ok(&f.h.handle("approvals.decide", json!({"id":id.as_str(),"approve":false}), &f.admin).await);
        assert_eq!(broker.get(&id).await.unwrap().unwrap().status, crate::approval::ApprovalStatus::Denied);
        ok(&f.h.handle("tasks.goal_decide", json!({"task_id":task_id,"action":"abort"}), &f.admin).await);
        assert_eq!(f.store.get_task(task_id).await.unwrap().unwrap().status, "cancelled");
        assert_eq!(crate::model_call_probe::calls(), before);
        assert_eq!(task_count(&f).await, 1);
        assert!(attachment_files(&f, "a1").is_empty());
    }
}
