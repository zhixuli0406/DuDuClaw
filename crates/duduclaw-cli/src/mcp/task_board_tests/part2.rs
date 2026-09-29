use super::*;

#[tokio::test(flavor = "current_thread")]
async fn shared_skill_share_then_list_then_adopt() {
    let tmp = TempDir::new();
    // Seed a skill file under agents/agnes/SKILLS/
    let skills_dir = tmp.path().join("agents").join("agnes").join("SKILLS");
    fs::create_dir_all(&skills_dir).unwrap();
    fs::write(
        skills_dir.join("pricing-audit.md"),
        "# Pricing Audit\n\nSteps:\n1. Pull price list.\n2. Diff against rules.\n",
    )
    .unwrap();

    // Share
    let share = handle_shared_skill_share(
        &serde_json::json!({ "skill_name": "pricing-audit" }),
        tmp.path(),
        "agnes",
    )
    .await;
    parse_ok(&share);

    // List — should appear
    let list = handle_shared_skill_list(&serde_json::json!({}), tmp.path()).await;
    let skills = parse_ok(&list)["skills"].as_array().unwrap().clone();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0]["name"], "pricing-audit");
    assert_eq!(skills[0]["shared_by"], "agnes");

    // Adopt into bruno
    let adopt = handle_shared_skill_adopt(
        &serde_json::json!({
            "skill_name": "pricing-audit",
            "target_agent": "bruno",
        }),
        tmp.path(),
        "agnes",
    )
    .await;
    parse_ok(&adopt);
    let bruno_path = tmp
        .path()
        .join("agents")
        .join("bruno")
        .join("SKILLS")
        .join("pricing-audit.md");
    assert!(bruno_path.exists());

    // Shared frontmatter should now record usage_count=1 and adopted_by includes bruno
    let list2 = handle_shared_skill_list(&serde_json::json!({}), tmp.path()).await;
    let skills2 = parse_ok(&list2)["skills"].as_array().unwrap().clone();
    assert_eq!(skills2[0]["usage_count"], 1);
    assert!(
        skills2[0]["adopted_by"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "bruno")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn autopilot_list_respects_enabled_only() {
    let tmp = TempDir::new();
    // Seed two rules directly via AutopilotStore
    let store = duduclaw_gateway::autopilot_store::AutopilotStore::open(tmp.path()).unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    let mut enabled = duduclaw_gateway::autopilot_store::AutopilotRuleRow {
        id: "r1".into(),
        name: "r1".into(),
        enabled: true,
        trigger_event: "task_created".into(),
        conditions: "{}".into(),
        action: "{}".into(),
        created_at: now.clone(),
        last_triggered_at: None,
        trigger_count: 0,
        sequence: None,
        metadata: None,
    };
    store.insert_rule(&enabled).await.unwrap();
    enabled.id = "r2".into();
    enabled.name = "r2".into();
    enabled.enabled = false;
    store.insert_rule(&enabled).await.unwrap();

    let only_enabled =
        handle_autopilot_list(&serde_json::json!({ "enabled_only": true }), tmp.path()).await;
    let rules = parse_ok(&only_enabled)["rules"].as_array().unwrap().clone();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0]["id"], "r1");

    let all =
        handle_autopilot_list(&serde_json::json!({ "enabled_only": false }), tmp.path()).await;
    let all_rules = parse_ok(&all)["rules"].as_array().unwrap().clone();
    assert_eq!(all_rules.len(), 2);
}

#[test]
fn strip_frontmatter_removes_leading_block() {
    let input = "---\nfoo: bar\n---\n\nBody line 1\nBody line 2\n";
    let out = strip_frontmatter(input);
    assert_eq!(out, "Body line 1\nBody line 2");
}

#[test]
fn strip_frontmatter_preserves_content_without_fence() {
    let input = "No frontmatter here\nJust body\n";
    let out = strip_frontmatter(input);
    // Only the leading whitespace is trimmed; trailing newline stays
    assert_eq!(out, "No frontmatter here\nJust body\n");
}

#[test]
fn extract_frontmatter_reads_first_match() {
    let content = "---\nshared_by: agnes\nusage_count: 3\n---\nBody\n";
    assert_eq!(
        extract_frontmatter(content, "shared_by"),
        Some("agnes".into())
    );
    assert_eq!(
        extract_frontmatter(content, "usage_count"),
        Some("3".into())
    );
    assert_eq!(extract_frontmatter(content, "missing"), None);
}

#[test]
fn update_frontmatter_field_bumps_counter() {
    let content = "---\nshared_by: a\nusage_count: 0\n---\nbody";
    let updated = update_frontmatter_field(content, "usage_count", |old| {
        let n: i64 = old.parse().unwrap_or(0);
        (n + 1).to_string()
    });
    assert!(updated.contains("usage_count: 1"));
}

// ── G1 lease renewal (tasks_renew) ──────────────────────

#[tokio::test(flavor = "current_thread")]
async fn tasks_renew_extends_lease_for_holder_only() {
    let tmp = TempDir::new();
    let create = handle_tasks_create(
        &serde_json::json!({ "title": "durable work", "durable": true }),
        tmp.path(),
        "agnes",
    )
    .await;
    let task_id = parse_ok(&create)["task"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let claim = handle_tasks_claim(
        &serde_json::json!({ "task_id": task_id }),
        tmp.path(),
        "agnes",
    )
    .await;
    let lease_before = parse_ok(&claim)["task"]["lease_expires_at"]
        .as_str()
        .unwrap()
        .to_string();

    // Holder renews → lease pushed forward, lease_renewed_at stamped.
    let renew = handle_tasks_renew(
        &serde_json::json!({ "task_id": task_id }),
        tmp.path(),
        "agnes",
    )
    .await;
    let renewed = parse_ok(&renew);
    let lease_after = renewed["task"]["lease_expires_at"].as_str().unwrap();
    assert!(
        lease_after >= lease_before.as_str(),
        "lease must not regress"
    );
    assert!(renewed["task"]["lease_renewed_at"].is_string());

    // A non-holder cannot renew (fail-closed).
    let intruder = handle_tasks_renew(
        &serde_json::json!({ "task_id": task_id }),
        tmp.path(),
        "bruno",
    )
    .await;
    assert!(intruder["isError"].as_bool().unwrap_or(false));

    // Unknown task id → error, not silence.
    let missing = handle_tasks_renew(
        &serde_json::json!({ "task_id": "nope" }),
        tmp.path(),
        "agnes",
    )
    .await;
    assert!(missing["isError"].as_bool().unwrap_or(false));
}

// ── G8 goal chain (goals_create / goals_list / tasks goal_id) ──

#[tokio::test(flavor = "current_thread")]
async fn goals_create_list_and_task_linkage() {
    let tmp = TempDir::new();
    let init = handle_goals_create(
        &serde_json::json!({ "title": "Grow revenue", "description": "2026 north star" }),
        tmp.path(),
        "agnes",
    )
    .await;
    let init_id = parse_ok(&init)["goal"]["id"].as_str().unwrap().to_string();

    let proj = handle_goals_create(
        &serde_json::json!({
            "title": "Launch pricing page",
            "description": "convert trials",
            "parent_goal_id": init_id,
        }),
        tmp.path(),
        "agnes",
    )
    .await;
    let proj_id = parse_ok(&proj)["goal"]["id"].as_str().unwrap().to_string();
    assert_eq!(
        parse_ok(&proj)["goal"]["parent_goal_id"].as_str().unwrap(),
        init_id
    );

    // Unknown parent is rejected fail-closed.
    let orphan = handle_goals_create(
        &serde_json::json!({ "title": "x", "parent_goal_id": "ghost" }),
        tmp.path(),
        "agnes",
    )
    .await;
    assert!(orphan["isError"].as_bool().unwrap_or(false));

    let list = handle_goals_list(&serde_json::json!({}), tmp.path()).await;
    let listed = parse_ok(&list);
    assert_eq!(listed["total"], 2);

    // Task linked to a goal carries goal_id; unknown goal_id is rejected.
    let task = handle_tasks_create(
        &serde_json::json!({ "title": "Write copy", "goal_id": proj_id }),
        tmp.path(),
        "agnes",
    )
    .await;
    assert_eq!(
        parse_ok(&task)["task"]["goal_id"].as_str().unwrap(),
        proj_id
    );

    let bad = handle_tasks_create(
        &serde_json::json!({ "title": "x", "goal_id": "ghost" }),
        tmp.path(),
        "agnes",
    )
    .await;
    assert!(bad["isError"].as_bool().unwrap_or(false));
}

// ── depends_on cycle validation ─────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn tasks_depends_on_rejects_cycles_and_unknown_ids() {
    let tmp = TempDir::new();
    let a =
        handle_tasks_create(&serde_json::json!({ "title": "A" }), tmp.path(), "agnes").await;
    let a_id = parse_ok(&a)["task"]["id"].as_str().unwrap().to_string();

    // B depends on A — legal.
    let b = handle_tasks_create(
        &serde_json::json!({ "title": "B", "depends_on": [a_id] }),
        tmp.path(),
        "agnes",
    )
    .await;
    let b_id = parse_ok(&b)["task"]["id"].as_str().unwrap().to_string();

    // Rewiring A to depend on B closes A → B → A ⇒ rejected.
    let cycle = handle_tasks_update(
        &serde_json::json!({ "task_id": a_id, "depends_on": [b_id] }),
        tmp.path(),
        "agnes",
    )
    .await;
    assert!(
        cycle["isError"].as_bool().unwrap_or(false),
        "cycle must be rejected"
    );

    // Self-dependency via update ⇒ rejected.
    let self_dep = handle_tasks_update(
        &serde_json::json!({ "task_id": a_id, "depends_on": [a_id] }),
        tmp.path(),
        "agnes",
    )
    .await;
    assert!(self_dep["isError"].as_bool().unwrap_or(false));

    // Unknown dependency id ⇒ rejected on create and update.
    let unknown_create = handle_tasks_create(
        &serde_json::json!({ "title": "C", "depends_on": ["ghost"] }),
        tmp.path(),
        "agnes",
    )
    .await;
    assert!(unknown_create["isError"].as_bool().unwrap_or(false));
    let unknown_update = handle_tasks_update(
        &serde_json::json!({ "task_id": a_id, "depends_on": "ghost" }),
        tmp.path(),
        "agnes",
    )
    .await;
    assert!(unknown_update["isError"].as_bool().unwrap_or(false));

    // A legal rewire still works (clear B's deps).
    let clear = handle_tasks_update(
        &serde_json::json!({ "task_id": b_id, "depends_on": [] }),
        tmp.path(),
        "agnes",
    )
    .await;
    let cleared = parse_ok(&clear);
    assert!(cleared["task"]["depends_on"].as_array().unwrap().is_empty());
}

// ── Edge-case tests (security + idempotency + error paths) ─────

#[tokio::test(flavor = "current_thread")]
async fn tasks_create_rejects_invalid_assigned_to() {
    let tmp = TempDir::new();
    // Wildcard is nonsensical — equality filter would match nothing
    let wildcard = handle_tasks_create(
        &serde_json::json!({ "title": "x", "assigned_to": "*" }),
        tmp.path(),
        "agnes",
    )
    .await;
    assert!(wildcard["isError"].as_bool().unwrap_or(false));

    // Path-traversal style
    let traversal = handle_tasks_create(
        &serde_json::json!({ "title": "x", "assigned_to": "../etc/passwd" }),
        tmp.path(),
        "agnes",
    )
    .await;
    assert!(traversal["isError"].as_bool().unwrap_or(false));
}

#[tokio::test(flavor = "current_thread")]
async fn tasks_complete_is_idempotent() {
    let tmp = TempDir::new();
    let create = handle_tasks_create(
        &serde_json::json!({ "title": "Already done" }),
        tmp.path(),
        "agnes",
    )
    .await;
    let id = parse_ok(&create)["task"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    handle_tasks_complete(
        &serde_json::json!({ "task_id": id.clone() }),
        tmp.path(),
        "agnes",
    )
    .await;
    let second =
        handle_tasks_complete(&serde_json::json!({ "task_id": id }), tmp.path(), "agnes").await;
    let done = parse_ok(&second);
    assert_eq!(done["task"]["status"], "done");
}

#[tokio::test(flavor = "current_thread")]
async fn tasks_update_rejects_unknown_task() {
    let tmp = TempDir::new();
    let result = handle_tasks_update(
        &serde_json::json!({
            "task_id": "does-not-exist",
            "title": "Nope",
        }),
        tmp.path(),
        "agnes",
    )
    .await;
    assert!(result["isError"].as_bool().unwrap_or(false));
}

#[tokio::test(flavor = "current_thread")]
async fn tasks_list_filters_by_status() {
    let tmp = TempDir::new();
    // Create 3 tasks (sequential — parallel would race the SQLite lock)
    let a =
        handle_tasks_create(&serde_json::json!({ "title": "t1" }), tmp.path(), "agnes").await;
    let b =
        handle_tasks_create(&serde_json::json!({ "title": "t2" }), tmp.path(), "agnes").await;
    let _c =
        handle_tasks_create(&serde_json::json!({ "title": "t3" }), tmp.path(), "agnes").await;
    let _ = b;
    let a_id = parse_ok(&a)["task"]["id"].as_str().unwrap().to_string();

    // Mark one as done
    handle_tasks_complete(&serde_json::json!({ "task_id": a_id }), tmp.path(), "agnes").await;

    let todo = handle_tasks_list(
        &serde_json::json!({ "status": "todo" }),
        tmp.path(),
        "agnes",
    )
    .await;
    let tlist = parse_ok(&todo)["tasks"].as_array().unwrap().clone();
    assert_eq!(tlist.len(), 2);

    let done_list = handle_tasks_list(
        &serde_json::json!({ "status": "done" }),
        tmp.path(),
        "agnes",
    )
    .await;
    let dlist = parse_ok(&done_list)["tasks"].as_array().unwrap().clone();
    assert_eq!(dlist.len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn shared_skill_share_rejects_missing_skill_file() {
    let tmp = TempDir::new();
    // agnes has no SKILLS directory yet
    let result = handle_shared_skill_share(
        &serde_json::json!({ "skill_name": "nonexistent" }),
        tmp.path(),
        "agnes",
    )
    .await;
    assert!(result["isError"].as_bool().unwrap_or(false));
}

// ── H9-G goal contract freeze (harness-borrowings 2026-08 WP-D) ─────
// An agent-identity caller may never modify the acceptance criteria of a
// goal_mode task through the MCP `tasks_update` tool — only the
// dashboard/operator RPC path (`handlers.rs::handle_tasks_update`) may.

#[tokio::test(flavor = "current_thread")]
async fn tasks_update_denies_agent_edit_of_acceptance_criteria_on_goal_mode_task_with_audit() {
    let tmp = TempDir::new();
    let create = handle_tasks_create(
        &serde_json::json!({
            "title": "Goal task",
            "goal_mode": true,
            "acceptance_criteria": "must ship the report",
        }),
        tmp.path(),
        "agnes",
    )
    .await;
    let id = parse_ok(&create)["task"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let result = handle_tasks_update(
        &serde_json::json!({
            "task_id": id,
            "acceptance_criteria": "must ship literally anything",
        }),
        tmp.path(),
        "agnes",
    )
    .await;
    assert!(
        result["isError"].as_bool().unwrap_or(false),
        "agent must not be able to weaken a frozen goal contract: {result:?}"
    );

    // The stored criteria must be unchanged.
    let after = handle_tasks_list(&serde_json::json!({}), tmp.path(), "agnes").await;
    let tasks = parse_ok(&after)["tasks"].as_array().unwrap().clone();
    let task = tasks.iter().find(|t| t["id"] == id).unwrap();
    assert_eq!(task["acceptance_criteria"], "must ship the report");

    // The denial must be audited.
    let audit = fs::read_to_string(tmp.path().join("tool_calls.jsonl")).expect("audit written");
    assert!(audit.contains("tasks_update"), "got: {audit}");
    assert!(audit.contains("goal_contract_frozen"), "got: {audit}");
    assert!(audit.contains("\"success\":false"), "got: {audit}");
}

#[tokio::test(flavor = "current_thread")]
async fn tasks_update_on_a_non_goal_mode_task_leaves_acceptance_criteria_unsupported_as_before()
{
    // The H9-G reject-with-audit gate only fires for goal_mode tasks. On
    // an ordinary board task, `acceptance_criteria` was never part of the
    // agent-facing MCP update surface to begin with (unlike the dashboard
    // RPC, which forwards raw params straight to the store) — this must
    // stay unchanged: the field is silently dropped, any other supplied
    // field still applies, and no reject/audit fires.
    let tmp = TempDir::new();
    let create = handle_tasks_create(
        &serde_json::json!({ "title": "Plain task" }),
        tmp.path(),
        "agnes",
    )
    .await;
    let id = parse_ok(&create)["task"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let result = handle_tasks_update(
        &serde_json::json!({
            "task_id": id,
            "title": "Renamed plain task",
            "acceptance_criteria": "note to self",
        }),
        tmp.path(),
        "agnes",
    )
    .await;
    let updated = parse_ok(&result);
    assert_eq!(updated["task"]["title"], "Renamed plain task");
    assert!(
        updated["task"]["acceptance_criteria"].is_null(),
        "acceptance_criteria was never part of the MCP update surface, goal_mode or not: {updated}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn tasks_update_still_allows_other_fields_on_a_goal_mode_task() {
    // The gate is scoped to the `acceptance_criteria` key only — title/
    // description/priority/tags/depends_on remain agent-editable.
    let tmp = TempDir::new();
    let create = handle_tasks_create(
        &serde_json::json!({
            "title": "Goal task",
            "goal_mode": true,
            "acceptance_criteria": "must ship the report",
        }),
        tmp.path(),
        "agnes",
    )
    .await;
    let id = parse_ok(&create)["task"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let result = handle_tasks_update(
        &serde_json::json!({ "task_id": id, "title": "Renamed goal task" }),
        tmp.path(),
        "agnes",
    )
    .await;
    let updated = parse_ok(&result);
    assert_eq!(updated["task"]["title"], "Renamed goal task");
    assert_eq!(
        updated["task"]["acceptance_criteria"],
        "must ship the report"
    );
}
