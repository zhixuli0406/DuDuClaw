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
    // Criteria default to the goal text as before; the file list is in the
    // description the employee reads.
    assert!(row
        .acceptance_criteria_baseline
        .unwrap()
        .contains("整理附件裡的名單"));
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
    assert_eq!(err(&ghost), "Agent not found: ghost");
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
