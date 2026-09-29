use super::*;

#[tokio::test(flavor = "current_thread")]
async fn tasks_create_allows_assigning_to_direct_report() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    create_test_agent(&agents_dir, "sales-lead", "");
    create_test_agent(&agents_dir, "sales-rep", "sales-lead");

    let result = handle_tasks_create(
        &serde_json::json!({ "title": "Call the lead", "assigned_to": "sales-rep" }),
        tmp.path(),
        "sales-lead",
    )
    .await;
    let created = parse_ok(&result);
    assert_eq!(created["task"]["assigned_to"], "sales-rep");
}

#[tokio::test(flavor = "current_thread")]
async fn tasks_create_allows_self_assign_without_org() {
    // Self-assignment never invokes the predicate — no agents/ directory
    // needed, self-delegation would be a hard DENY under every policy.
    let tmp = TempDir::new();
    let result = handle_tasks_create(
        &serde_json::json!({ "title": "My own task" }),
        tmp.path(),
        "lone-agent",
    )
    .await;
    let created = parse_ok(&result);
    assert_eq!(created["task"]["assigned_to"], "lone-agent");
}

#[tokio::test(flavor = "current_thread")]
async fn tasks_create_denies_cross_department_stranger_with_audit() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    create_test_agent_in_dept(&agents_dir, "sales-rep", "", "業務");
    create_test_agent_in_dept(&agents_dir, "mkt-rep", "", "行銷");

    let result = handle_tasks_create(
        &serde_json::json!({ "title": "Please do X", "assigned_to": "mkt-rep" }),
        tmp.path(),
        "sales-rep",
    )
    .await;
    assert!(
        result["isError"].as_bool().unwrap_or(false),
        "cross-department assign must be denied"
    );
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(text.contains("委派遭拒"), "message must be zh-TW: {text}");
    assert!(
        text.contains("sales-rep") && text.contains("mkt-rep"),
        "got: {text}"
    );

    let audit = fs::read_to_string(tmp.path().join("tool_calls.jsonl")).expect("audit written");
    assert!(audit.contains("delegation_denied"), "got: {audit}");
    assert!(audit.contains("tasks_create"), "got: {audit}");
}

#[tokio::test(flavor = "current_thread")]
async fn tasks_create_allows_whitelisted_cross_department_pair() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    create_test_agent_in_dept(&agents_dir, "sales-lead", "", "業務");
    create_test_agent_in_dept(&agents_dir, "mkt-lead", "", "行銷");
    fs::write(
        tmp.path().join("config.toml"),
        "[delegation]\npolicy = \"department\"\nallow = [[\"sales-lead\", \"mkt-lead\"]]\n",
    )
    .unwrap();

    let result = handle_tasks_create(
        &serde_json::json!({ "title": "Joint campaign", "assigned_to": "mkt-lead" }),
        tmp.path(),
        "sales-lead",
    )
    .await;
    let created = parse_ok(&result);
    assert_eq!(created["task"]["assigned_to"], "mkt-lead");
}

#[tokio::test(flavor = "current_thread")]
async fn tasks_create_open_policy_allows_any_assignment() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    create_test_agent_in_dept(&agents_dir, "sales-rep", "", "業務");
    create_test_agent_in_dept(&agents_dir, "mkt-rep", "", "行銷");
    fs::write(
        tmp.path().join("config.toml"),
        "[delegation]\npolicy = \"open\"\n",
    )
    .unwrap();

    let result = handle_tasks_create(
        &serde_json::json!({ "title": "Anything goes", "assigned_to": "mkt-rep" }),
        tmp.path(),
        "sales-rep",
    )
    .await;
    let created = parse_ok(&result);
    assert_eq!(created["task"]["assigned_to"], "mkt-rep");
}

#[tokio::test(flavor = "current_thread")]
async fn tasks_update_allows_reassign_to_direct_report() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    create_test_agent(&agents_dir, "sales-lead", "");
    create_test_agent(&agents_dir, "sales-rep", "sales-lead");

    let create = handle_tasks_create(
        &serde_json::json!({ "title": "Unassigned yet" }),
        tmp.path(),
        "sales-lead",
    )
    .await;
    let id = parse_ok(&create)["task"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let updated = handle_tasks_update(
        &serde_json::json!({ "task_id": id, "assigned_to": "sales-rep" }),
        tmp.path(),
        "sales-lead",
    )
    .await;
    let updated = parse_ok(&updated);
    assert_eq!(updated["task"]["assigned_to"], "sales-rep");
}

#[tokio::test(flavor = "current_thread")]
async fn tasks_update_denies_cross_department_reassign_with_audit() {
    let tmp = TempDir::new();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    create_test_agent_in_dept(&agents_dir, "sales-rep", "", "業務");
    create_test_agent_in_dept(&agents_dir, "mkt-rep", "", "行銷");

    let create = handle_tasks_create(
        &serde_json::json!({ "title": "Sales task" }),
        tmp.path(),
        "sales-rep",
    )
    .await;
    let id = parse_ok(&create)["task"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let result = handle_tasks_update(
        &serde_json::json!({ "task_id": id, "assigned_to": "mkt-rep" }),
        tmp.path(),
        "sales-rep",
    )
    .await;
    assert!(
        result["isError"].as_bool().unwrap_or(false),
        "cross-department reassign must be denied"
    );
    let audit = fs::read_to_string(tmp.path().join("tool_calls.jsonl")).expect("audit written");
    assert!(audit.contains("tasks_update"), "got: {audit}");
}

#[tokio::test(flavor = "current_thread")]
async fn tasks_claim_is_unaffected_by_the_delegation_gate() {
    // tasks_claim never reassigns to a third party — the caller always
    // takes it for itself — so it must keep working with zero org setup.
    let tmp = TempDir::new();
    let create = handle_tasks_create(
        &serde_json::json!({ "title": "Up for grabs" }),
        tmp.path(),
        "agnes",
    )
    .await;
    let id = parse_ok(&create)["task"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let claim =
        handle_tasks_claim(&serde_json::json!({ "task_id": id }), tmp.path(), "agnes").await;
    let claimed = parse_ok(&claim);
    assert_eq!(claimed["task"]["status"], "in_progress");
}

#[tokio::test(flavor = "current_thread")]
async fn tasks_create_then_list_returns_task() {
    let tmp = TempDir::new();
    let create = handle_tasks_create(
        &serde_json::json!({
            "title": "Ship feature X",
            "priority": "high",
        }),
        tmp.path(),
        "agnes",
    )
    .await;
    let created = parse_ok(&create);
    let task_id = created["task"]["id"].as_str().unwrap().to_string();
    assert_eq!(created["task"]["assigned_to"], "agnes");
    assert_eq!(created["task"]["created_by"], "agnes");
    assert_eq!(created["task"]["status"], "todo");

    let list = handle_tasks_list(&serde_json::json!({}), tmp.path(), "agnes").await;
    let listed = parse_ok(&list);
    let tasks = listed["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["id"], task_id);
}

#[tokio::test(flavor = "current_thread")]
async fn tasks_list_filters_to_caller_by_default() {
    let tmp = TempDir::new();
    // WP21 T5: agnes assigning to bruno now goes through the delegation
    // gate — give bruno a `reports_to = "agnes"` so the cross-assignment
    // below is legitimate under the default `department` policy (rule 3:
    // sender is an ancestor of target), unrelated to what this test
    // actually verifies (assigned_to filtering).
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    create_test_agent(&agents_dir, "agnes", "");
    create_test_agent(&agents_dir, "bruno", "agnes");
    // agnes creates a task assigned to bruno
    handle_tasks_create(
        &serde_json::json!({
            "title": "For bruno",
            "assigned_to": "bruno",
        }),
        tmp.path(),
        "agnes",
    )
    .await;
    // agnes creates a task for herself
    handle_tasks_create(
        &serde_json::json!({ "title": "For agnes" }),
        tmp.path(),
        "agnes",
    )
    .await;

    // agnes listing — should only see her own
    let agnes_list = handle_tasks_list(&serde_json::json!({}), tmp.path(), "agnes").await;
    let agnes_tasks = parse_ok(&agnes_list);
    let titles: Vec<&str> = agnes_tasks["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["For agnes"]);

    // '*' sees all
    let all = handle_tasks_list(
        &serde_json::json!({ "assigned_to": "*" }),
        tmp.path(),
        "agnes",
    )
    .await;
    let all_tasks = parse_ok(&all);
    assert_eq!(all_tasks["tasks"].as_array().unwrap().len(), 2);
}

// ── U4 co-edited plan tools ──────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn plan_get_defaults_to_callers_active_plan() {
    let tmp = TempDir::new();
    let (plan_id, ..) = seed_plan(tmp.path()).await;

    // No plan_id ⇒ the caller's most recently updated active plan.
    let got = parse_ok(&handle_plan_get(&serde_json::json!({}), tmp.path(), "agnes").await);
    assert_eq!(got["plan"]["id"], plan_id.as_str());
    assert_eq!(got["steps"].as_array().unwrap().len(), 2);

    // An agent with no plan gets an explicit empty answer, not an error.
    let none = parse_ok(&handle_plan_get(&serde_json::json!({}), tmp.path(), "bruno").await);
    assert!(none["plan"].is_null());
    assert_eq!(none["steps"].as_array().unwrap().len(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn plan_update_step_is_holder_guarded_fail_closed() {
    let tmp = TempDir::new();
    let (_plan, agent_step, user_step) = seed_plan(tmp.path()).await;

    // The agent may tick ITS OWN step.
    let ok = parse_ok(
        &handle_plan_update_step(
            &serde_json::json!({ "step_id": agent_step, "status": "done" }),
            tmp.path(),
            "agnes",
        )
        .await,
    );
    assert_eq!(ok["step"]["status"], "done");

    // A USER step is off-limits to the agent (fail-closed).
    let denied = handle_plan_update_step(
        &serde_json::json!({ "step_id": user_step, "status": "done" }),
        tmp.path(),
        "agnes",
    )
    .await;
    assert!(
        denied["isError"].as_bool().unwrap_or(false),
        "user step must be denied"
    );

    // Another agent may not tick agnes' step either.
    let denied2 = handle_plan_update_step(
        &serde_json::json!({ "step_id": agent_step, "status": "todo" }),
        tmp.path(),
        "bruno",
    )
    .await;
    assert!(
        denied2["isError"].as_bool().unwrap_or(false),
        "other agent must be denied"
    );

    // Invalid status is rejected by the store's enum gate.
    let bad = handle_plan_update_step(
        &serde_json::json!({ "step_id": agent_step, "status": "finished" }),
        tmp.path(),
        "agnes",
    )
    .await;
    assert!(
        bad["isError"].as_bool().unwrap_or(false),
        "invalid status rejected"
    );

    // The tick landed in the Activity Feed (co-editing timeline).
    let store = duduclaw_gateway::task_store::TaskStore::open(tmp.path()).unwrap();
    let (events, _total) = store
        .list_activity(Some("agnes"), None, 50, 0)
        .await
        .unwrap();
    assert!(
        events.iter().any(|e| e.event_type == "plan_step_updated"),
        "plan_step_updated activity recorded"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn tasks_claim_transitions_to_in_progress_and_rejects_stealing() {
    // Updated for the LOW 2026-07 fix: the old test asserted the buggy
    // behavior (agnes silently RE-assigning bruno's todo task to herself
    // via the legacy fallback). The fallback is now restricted to todo
    // tasks that are unassigned or already assigned to the caller.
    let tmp = TempDir::new();
    let create = handle_tasks_create(
        &serde_json::json!({
            "title": "Bruno's task",
            "assigned_to": "bruno",
        }),
        tmp.path(),
        "bruno",
    )
    .await;
    let id = parse_ok(&create)["task"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // agnes must NOT be able to steal bruno's assigned todo task.
    let steal =
        handle_tasks_claim(&serde_json::json!({ "task_id": id }), tmp.path(), "agnes").await;
    assert!(
        steal["isError"].as_bool().unwrap_or(false),
        "stealing must error"
    );

    // The assignee himself claims it → in_progress.
    let claim =
        handle_tasks_claim(&serde_json::json!({ "task_id": id }), tmp.path(), "bruno").await;
    let claimed = parse_ok(&claim);
    assert_eq!(claimed["task"]["assigned_to"], "bruno");
    assert_eq!(claimed["task"]["status"], "in_progress");
}

#[tokio::test(flavor = "current_thread")]
async fn tasks_claim_is_gated_by_unfinished_dependencies() {
    // HIGH-1 (2026-07 review): a durable task whose depends_on are not all
    // `done` must not be claimable, and the error names the unmet ids.
    let tmp = TempDir::new();
    let dep = handle_tasks_create(
        &serde_json::json!({ "title": "dep", "durable": true }),
        tmp.path(),
        "agnes",
    )
    .await;
    let dep_id = parse_ok(&dep)["task"]["id"].as_str().unwrap().to_string();
    let child = handle_tasks_create(
        &serde_json::json!({ "title": "child", "durable": true, "depends_on": [dep_id] }),
        tmp.path(),
        "agnes",
    )
    .await;
    let child_id = parse_ok(&child)["task"]["id"].as_str().unwrap().to_string();

    // Claiming the child while the dep is pending errors with the dep id.
    let blocked = handle_tasks_claim(
        &serde_json::json!({ "task_id": child_id }),
        tmp.path(),
        "agnes",
    )
    .await;
    assert!(blocked["isError"].as_bool().unwrap_or(false));
    let msg = blocked["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        msg.contains("blocked by unfinished dependencies") && msg.contains(&dep_id),
        "error must name the unmet dependency: {msg}"
    );

    // Finish the dep (claim + complete), then the child unlocks.
    parse_ok(
        &handle_tasks_claim(
            &serde_json::json!({ "task_id": dep_id }),
            tmp.path(),
            "agnes",
        )
        .await,
    );
    parse_ok(
        &handle_tasks_complete(
            &serde_json::json!({ "task_id": dep_id, "summary": "done" }),
            tmp.path(),
            "agnes",
        )
        .await,
    );
    let claim = handle_tasks_claim(
        &serde_json::json!({ "task_id": child_id }),
        tmp.path(),
        "agnes",
    )
    .await;
    let claimed = parse_ok(&claim);
    assert_eq!(claimed["task"]["status"], "in_progress");
    assert!(
        claimed["lease_note"]
            .as_str()
            .unwrap_or("")
            .contains("tasks_renew"),
        "leased claim response must point at tasks_renew"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn tasks_complete_and_block_are_holder_guarded() {
    // HIGH-2 (2026-07 review): a task claimed by X can only be completed /
    // blocked by X — a reclaimed zombie worker must not clobber the new
    // holder's in_progress task.
    let tmp = TempDir::new();
    let create = handle_tasks_create(
        &serde_json::json!({ "title": "guarded", "durable": true }),
        tmp.path(),
        "agnes",
    )
    .await;
    let id = parse_ok(&create)["task"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    parse_ok(
        &handle_tasks_claim(&serde_json::json!({ "task_id": id }), tmp.path(), "agnes").await,
    );

    // Non-holder completion / block are rejected.
    let zombie_complete = handle_tasks_complete(
        &serde_json::json!({ "task_id": id, "summary": "stale result" }),
        tmp.path(),
        "bruno",
    )
    .await;
    assert!(zombie_complete["isError"].as_bool().unwrap_or(false));
    let zombie_block = handle_tasks_block(
        &serde_json::json!({ "task_id": id, "reason": "stale blocker" }),
        tmp.path(),
        "bruno",
    )
    .await;
    assert!(zombie_block["isError"].as_bool().unwrap_or(false));

    // The holder completes normally and the result is hers.
    let done = parse_ok(
        &handle_tasks_complete(
            &serde_json::json!({ "task_id": id, "summary": "real result" }),
            tmp.path(),
            "agnes",
        )
        .await,
    );
    assert_eq!(done["task"]["status"], "done");
    assert_eq!(done["task"]["result_summary"], "real result");
}

#[tokio::test(flavor = "current_thread")]
async fn tasks_complete_marks_done_and_sets_completed_at() {
    let tmp = TempDir::new();
    let create = handle_tasks_create(
        &serde_json::json!({ "title": "Finish me" }),
        tmp.path(),
        "agnes",
    )
    .await;
    let id = parse_ok(&create)["task"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let complete = handle_tasks_complete(
        &serde_json::json!({
            "task_id": id,
            "summary": "Shipped",
        }),
        tmp.path(),
        "agnes",
    )
    .await;
    let done = parse_ok(&complete);
    assert_eq!(done["task"]["status"], "done");
    assert!(done["task"]["completed_at"].as_str().is_some());

    // Activity log should contain task_completed
    let activity = handle_activity_list(
        &serde_json::json!({ "event_type": "task_completed" }),
        tmp.path(),
        "agnes",
    )
    .await;
    let log = parse_ok(&activity);
    let activities = log["activities"].as_array().unwrap();
    assert!(activities.iter().any(|a| a["type"] == "task_completed"));
}

#[tokio::test(flavor = "current_thread")]
async fn tasks_block_requires_reason() {
    let tmp = TempDir::new();
    let create = handle_tasks_create(
        &serde_json::json!({ "title": "Stuck" }),
        tmp.path(),
        "agnes",
    )
    .await;
    let id = parse_ok(&create)["task"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Missing reason → error
    let no_reason =
        handle_tasks_block(&serde_json::json!({ "task_id": id }), tmp.path(), "agnes").await;
    assert!(no_reason["isError"].as_bool().unwrap_or(false));

    // With reason → success
    let blocked = handle_tasks_block(
        &serde_json::json!({
            "task_id": id,
            "reason": "Waiting for API key",
        }),
        tmp.path(),
        "agnes",
    )
    .await;
    let b = parse_ok(&blocked);
    assert_eq!(b["task"]["status"], "blocked");
    assert_eq!(b["task"]["blocked_reason"], "Waiting for API key");
}

#[tokio::test(flavor = "current_thread")]
async fn tasks_create_rejects_empty_title() {
    let tmp = TempDir::new();
    let result =
        handle_tasks_create(&serde_json::json!({ "title": "   " }), tmp.path(), "agnes").await;
    assert!(result["isError"].as_bool().unwrap_or(false));
}

#[tokio::test(flavor = "current_thread")]
async fn activity_post_then_list() {
    let tmp = TempDir::new();
    let post = handle_activity_post(
        &serde_json::json!({
            "summary": "Checked in on research task",
            "event_type": "progress",
        }),
        tmp.path(),
        "agnes",
    )
    .await;
    let posted = parse_ok(&post);
    assert_eq!(posted["activity"]["type"], "progress");
    assert_eq!(posted["activity"]["agent_id"], "agnes");

    let list = handle_activity_list(&serde_json::json!({}), tmp.path(), "agnes").await;
    let items = parse_ok(&list)["activities"].as_array().unwrap().clone();
    assert_eq!(items.len(), 1);
}
