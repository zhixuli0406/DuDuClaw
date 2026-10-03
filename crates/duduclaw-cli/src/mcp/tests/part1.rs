use super::*;

/// The receipt must name the routine, say when it runs in plain zh-TW, and
/// point at the dashboard page — the three things a user needs to believe
/// the routine exists.
#[test]
fn cron_receipt_is_human_zh_tw_and_points_at_the_dashboard() {
    let r = cron_created_receipt("每日晨報", "0 9 * * *", Some("Asia/Taipei"));
    assert!(r.contains("每日晨報"), "{r}");
    assert!(r.contains("每天 09:00"), "{r}");
    assert!(r.contains("Asia/Taipei"), "{r}");
    assert!(r.contains("例行工作"), "{r}");
    // No raw cron expression leaked into the user-facing sentence.
    assert!(!r.contains("0 9 * * *"), "{r}");
}

/// Weekly / hourly / 6-field forms all render.
///
/// Values are asserted against the **real scheduler** below
/// (`humanize_cron_matches_the_cron_crate`); this case only pins the exact
/// wording so a rename can't silently change the user-visible string.
#[test]
fn humanize_cron_covers_the_shapes_users_dictate() {
    assert_eq!(
        humanize_cron_zh("30 8 * * *").as_deref(),
        Some("每天 08:30")
    );
    assert_eq!(humanize_cron_zh("0 0 30 8 * * *").as_deref(), None); // 7 fields
    assert_eq!(
        humanize_cron_zh("0 30 8 * * *").as_deref(),
        Some("每天 08:30")
    );
    assert_eq!(humanize_cron_zh("0 * * * *").as_deref(), Some("每小時整點"));
    assert_eq!(
        humanize_cron_zh("15 * * * *").as_deref(),
        Some("每小時第 15 分")
    );
}

/// **The weekday claim is checked against the scheduler, not against my
/// reading of it.** Asserting `"0 9 * * 1" == "每週一"` would just restate
/// the assumption under test; the `cron` crate is Quartz-flavoured
/// (1 = Sunday) while user-facing expressions are Unix crontab
/// (0/7 = Sunday) translated by `normalise_cron` at parse time — a
/// mismatch on either side names the wrong day with full confidence.
///
/// Here the oracle is the real pipeline: normalise exactly the way
/// `CronScheduler` does, parse with `cron::Schedule`, read the weekday
/// and clock off the next actual fire, and require the zh-TW sentence
/// to agree.
#[test]
fn humanize_cron_matches_the_cron_crate() {
    use chrono::{Datelike, Timelike};

    // The zh-TW weekday name for a real `chrono::Weekday`.
    fn zh_weekday(w: chrono::Weekday) -> &'static str {
        match w {
            chrono::Weekday::Sun => "日",
            chrono::Weekday::Mon => "一",
            chrono::Weekday::Tue => "二",
            chrono::Weekday::Wed => "三",
            chrono::Weekday::Thu => "四",
            chrono::Weekday::Fri => "五",
            chrono::Weekday::Sat => "六",
        }
    }

    for dow in 0..=7u32 {
        let expr = format!("0 9 * * {dow}");
        // Same normalisation `handle_schedule_task` applies.
        let schedule: cron::Schedule = duduclaw_core::cron_tz::normalise_cron(&expr)
            .parse()
            .unwrap_or_else(|e| panic!("{expr} must parse: {e}"));

        let next = schedule
            .upcoming(chrono::Utc)
            .next()
            .unwrap_or_else(|| panic!("{expr} must have an upcoming fire"));
        let expected = format!(
            "每週{} {:02}:{:02}",
            zh_weekday(next.weekday()),
            next.hour(),
            next.minute()
        );

        assert_eq!(
            humanize_cron_zh(&expr).as_deref(),
            Some(expected.as_str()),
            "{expr} fires on {} at {}:{:02} — the receipt must say so",
            next.weekday(),
            next.hour(),
            next.minute()
        );

        // Every subsequent fire must land on the same weekday, otherwise
        // "每週X" is the wrong shape of promise entirely.
        for fire in schedule.upcoming(chrono::Utc).take(5) {
            assert_eq!(fire.weekday(), next.weekday(), "{expr} drifts weekday");
        }
    }

    // Daily / hourly agree with the scheduler too.
    let daily: cron::Schedule = "0 30 8 * * *".parse().unwrap();
    let f = daily.upcoming(chrono::Utc).next().unwrap();
    assert_eq!(
        humanize_cron_zh("30 8 * * *").as_deref(),
        Some(format!("每天 {:02}:{:02}", f.hour(), f.minute()).as_str())
    );
}

/// Reviewer counter-examples, kept as their own case so a regression names
/// itself. Day-of-week above `7` has no crontab meaning, and an hourly
/// expression pinned to one weekday is not "每小時".
#[test]
fn humanize_cron_rejects_the_shapes_it_would_describe_wrongly() {
    // Unix crontab: both 0 and 7 are Sunday; 8 is nothing — fall back.
    assert_eq!(
        humanize_cron_zh("0 9 * * 0").as_deref(),
        Some("每週日 09:00")
    );
    assert_eq!(
        humanize_cron_zh("0 9 * * 7").as_deref(),
        Some("每週日 09:00")
    );
    assert_eq!(humanize_cron_zh("0 9 * * 8"), None);
    // Hourly *restricted to one weekday* must not be sold as plain hourly.
    assert_eq!(humanize_cron_zh("0 * * * 2"), None);
    assert_eq!(humanize_cron_zh("15 * * * 5"), None);

    // The receipt then carries the raw expression, which the user can audit.
    let r = cron_created_receipt("每週報", "0 * * * 2", None);
    assert!(r.contains("排程 0 * * * 2"), "{r}");
}

/// End-to-end for the WP6 emission point: creating a routine the way a
/// channel conversation does (`tasks_create` with a cron `schedule`)
/// persists the row AND raises the `cron.changed` row the gateway tail turns
/// into a dashboard push. Without the second half, RoutinesPage stays blank
/// until a manual reload.
#[tokio::test]
async fn scheduled_tasks_create_persists_and_raises_cron_changed() {
    let tmp = TempDir::new();
    let home = tmp.path();

    let args = serde_json::json!({
        "title": "每日晨報",
        "description": "整理今天的行程",
        "schedule": "0 9 * * *",
        "assigned_to": "agnes",
        "cron_timezone": "Asia/Taipei",
    });
    let result = crate::mcp::caller_shims::handle_tasks_create(&args, home, "agnes").await;
    assert!(
        result.get("isError").is_none(),
        "tasks_create with a schedule should succeed: {result}"
    );

    // The routine is in the store the dashboard's `cron.list` reads.
    let store = duduclaw_gateway::cron_store::CronStore::open(home).unwrap();
    let rows = store.list_all().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "每日晨報");

    // The channel receipt is the zh-TW sentence, not a developer string.
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("已建立例行工作"), "{text}");
    assert!(text.contains("每天 09:00"), "{text}");

    // ...and the dashboard learns about it.
    duduclaw_gateway::dashboard_feedback::emit_for_tool(
        home,
        "tasks_create",
        &args,
        &result,
        "agnes",
    )
    .await;
    let bus = duduclaw_gateway::events_store::EventBusStore::open(home).unwrap();
    let events = bus.fetch_since(0, 50).await.unwrap();
    let row = events
        .iter()
        .find(|r| r.event == duduclaw_gateway::dashboard_feedback::EV_CRON_CHANGED)
        .expect("cron.changed must be raised");
    let payload: serde_json::Value = serde_json::from_str(&row.payload).unwrap();
    assert_eq!(payload["action"], "created");
    assert_eq!(payload["name"], "每日晨報");
    assert_eq!(payload["agent_id"], "agnes");
}

/// A rejected schedule (bad cron) persists nothing and must stay silent — a
/// dashboard refetch triggered by a phantom event would show the user the
/// absence of what they were just told about.
#[tokio::test]
async fn rejected_scheduled_tasks_create_raises_no_event() {
    let tmp = TempDir::new();
    let home = tmp.path();

    let args = serde_json::json!({
        "title": "壞排程",
        "schedule": "61 9 * * *",
    });
    let result = crate::mcp::caller_shims::handle_tasks_create(&args, home, "agnes").await;
    assert_eq!(result["isError"], true, "{result}");

    duduclaw_gateway::dashboard_feedback::emit_for_tool(
        home,
        "tasks_create",
        &args,
        &result,
        "agnes",
    )
    .await;
    let bus = duduclaw_gateway::events_store::EventBusStore::open(home).unwrap();
    let events = bus.fetch_since(0, 50).await.unwrap();
    assert!(
        events
            .iter()
            .all(|r| r.event != duduclaw_gateway::dashboard_feedback::EV_CRON_CHANGED)
    );
}

/// Shapes it cannot describe correctly fall back to the raw expression —
/// never a confident wrong description the user cannot audit.
#[test]
fn unhandled_cron_shapes_fall_back_to_the_raw_expression() {
    assert_eq!(humanize_cron_zh("0 9 1 * *"), None); // day-of-month
    assert_eq!(humanize_cron_zh("*/5 * * * *"), None); // step
    assert_eq!(humanize_cron_zh("0 9 * 3 *"), None); // month
    assert_eq!(humanize_cron_zh("garbage"), None);

    let r = cron_created_receipt("季報", "*/5 * * * *", None);
    assert!(r.contains("排程 */5 * * * *"), "{r}");
    assert!(r.contains("例行工作"), "{r}");
}

/// `os_watch_status` must key its lookup on the *calling* agent (the value
/// the dispatch now threads through from `principal.client_id` /
/// `caller_client_id`), never a shared default. A wrong-agent key would leak
/// one agent's watched paths to another or spuriously report "no watch".
#[tokio::test]
async fn os_watch_status_reports_caller_agent_only() {
    let tmp = TempDir::new();
    let home = tmp.path();
    let stats = serde_json::json!({
        "updated_at": "2026-07-22T00:00:00Z",
        "agents": {
            "alice": { "watched_paths": ["/home/alice/inbox"], "emitted": 3, "dropped": 0 },
            "bob":   { "watched_paths": ["/home/bob/downloads"], "emitted": 7, "dropped": 1 }
        }
    });
    fs::write(
        home.join(duduclaw_gateway::os_events::STATS_FILE_NAME),
        serde_json::to_string(&stats).unwrap(),
    )
    .unwrap();

    // Bob's call surfaces Bob's paths, never Alice's (cross-agent leak).
    let out = handle_os_watch_status(home, "bob").await;
    let text = out["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("/home/bob/downloads"),
        "bob sees own paths: {text}"
    );
    assert!(
        !text.contains("/home/alice/inbox"),
        "bob must NOT see alice: {text}"
    );

    // An agent with no entry is told it has no active watch — not handed
    // another agent's data.
    let out = handle_os_watch_status(home, "carol").await;
    let text = out["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("carol"),
        "carol gets own no-watch notice: {text}"
    );
    assert!(!text.contains("/home/alice/inbox"));
    assert!(!text.contains("/home/bob/downloads"));
}

#[tokio::test]
async fn delegation_parent_to_child_allowed() {
    let tmp = delegation_home();
    let result = check_delegation_allowed(tmp.path(), "sales-lead", "sales-rep", "t").await;
    assert!(result.is_ok(), "parent→child must be allowed: {result:?}");
}

#[tokio::test]
async fn delegation_child_to_parent_allowed() {
    let tmp = delegation_home();
    let result = check_delegation_allowed(tmp.path(), "sales-rep", "sales-lead", "t").await;
    assert!(result.is_ok(), "child→parent must be allowed: {result:?}");
}

/// Behaviour change vs. pre-WP21: the old direct-parent-only check denied
/// this, so skip-level assignment was impossible.
#[tokio::test]
async fn delegation_grandparent_to_grandchild_now_allowed() {
    let tmp = delegation_home();
    let home = tmp.path();
    assert!(
        check_delegation_allowed(home, "ceo", "sales-rep", "t")
            .await
            .is_ok(),
        "skip-level command must be allowed"
    );
    assert!(
        check_delegation_allowed(home, "sales-rep", "ceo", "t")
            .await
            .is_ok(),
        "skip-level escalation must be allowed"
    );
}

#[tokio::test]
async fn delegation_same_department_peers_allowed() {
    let tmp = delegation_home();
    // Siblings: no ancestor relation either way, same non-empty department.
    let result = check_delegation_allowed(tmp.path(), "sales-rep", "sales-rep2", "t").await;
    assert!(
        result.is_ok(),
        "same-department peers must be allowed: {result:?}"
    );
}

// ── WP21 T10 review: the three rails that reached agent execution
//    without ever meeting the predicate ──────────────────────────────

/// An agent named `cron` would satisfy `is_system_sender` and thereby
/// clear C1/C2/C3 unconditionally *and* skip the C4 subtree rule — a
/// self-service escalation available to anyone who can call
/// `create_agent`. The reserved-id list closes it.
#[tokio::test]
async fn create_agent_refuses_system_sender_names() {
    let tmp = delegation_home();
    let home = tmp.path();
    // (`__…` ids are already impossible here — the CLI's `is_valid_agent_id`
    // allows only lowercase/digits/hyphens — but core reserves them anyway
    // for the paths that use the looser core validator.)
    for name in ["cron", "dashboard", "autopilot", "a2a-client", "default"] {
        let params = serde_json::json!({
            "name": name,
            "display_name": name,
        });
        let out = handle_create_agent(&params, home, "sales-rep").await;
        assert_eq!(out["isError"], true, "'{name}' must be refused: {out}");
        assert!(
            out["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("保留名稱"),
            "{out}"
        );
        assert!(
            !home.join("agents").join(name).exists(),
            "'{name}' must not be scaffolded"
        );
    }
    // A normal name that merely *contains* a reserved word still works.
    let ok = handle_create_agent(
        &serde_json::json!({ "name": "cron-helper", "display_name": "小排程" }),
        home,
        "sales-rep",
    )
    .await;
    assert!(ok.get("isError").is_none(), "{ok}");
}

// ── WP22 T4: reject a create that would collide an agent's registry
//    identity (directory name / `[agent] name`) with an existing one ──

/// The plain case: the new agent's directory name (== its `name`, always
/// true on the create path) matches an existing directory exactly. Also
/// caught pre-WP22 by the `agent_dir.exists()` check — this pins that
/// behaviour stays intact alongside the new collision check.
#[tokio::test]
async fn create_agent_refuses_name_matching_existing_dir() {
    let tmp = delegation_home();
    let home = tmp.path();
    let params = serde_json::json!({
        "name": "ceo",
        "display_name": "另一個 CEO",
    });
    let out = handle_create_agent(&params, home, "ceo").await;
    assert_eq!(out["isError"], true, "{out}");
}

/// The gap WP22 T4 actually closes: an existing agent's directory name
/// and its `[agent] name` field have drifted apart (hand-edited
/// agent.toml, or a directory renamed without updating the field). A
/// new `create_agent` whose `name` matches that *field* — not any
/// directory — must still be refused with the zh-TW collision message,
/// even though `agents/<name>` does not exist yet.
#[tokio::test]
async fn create_agent_refuses_name_matching_existing_name_field_in_other_dir() {
    let tmp = delegation_home();
    let home = tmp.path();
    let agents_dir = home.join("agents");
    let mismatched_dir = agents_dir.join("legacy-sales");
    fs::create_dir_all(&mismatched_dir).unwrap();
    fs::write(
        mismatched_dir.join("agent.toml"),
        r#"[agent]
name = "sales-alias"
display_name = "Sales Alias"
role = "specialist"
status = "active"
trigger = "@sales-alias"
reports_to = "ceo"
icon = "🤖"
department = ""
"#,
    )
    .unwrap();

    let params = serde_json::json!({
        "name": "sales-alias",
        "display_name": "新業務",
    });
    let out = handle_create_agent(&params, home, "ceo").await;
    assert_eq!(out["isError"], true, "{out}");
    assert!(
        out["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("已有同名的 AI 員工"),
        "{out}"
    );
    assert!(
        !agents_dir.join("sales-alias").exists(),
        "a rejected create must not scaffold a directory"
    );
}

/// Control: a genuinely fresh name — colliding with neither a directory
/// nor any existing `[agent] name` — still creates normally.
#[tokio::test]
async fn create_agent_allows_non_colliding_name() {
    let tmp = delegation_home();
    let home = tmp.path();
    let params = serde_json::json!({
        "name": "brand-new-agent",
        "display_name": "全新員工",
    });
    let out = handle_create_agent(&params, home, "ceo").await;
    assert!(out.get("isError").is_none(), "{out}");
    assert!(home.join("agents").join("brand-new-agent").exists());
}

/// A cron row fires under the `cron` identity, so the row's creation is the
/// delegation. Without this gate any agent could schedule work for
/// any other agent and launder it through the scheduler.
#[tokio::test]
async fn schedule_task_for_a_stranger_is_denied() {
    let tmp = delegation_home();
    let home = tmp.path();
    let args = serde_json::json!({
        "name": "偷派工",
        "cron": "0 9 * * *",
        "task": "幫我做行銷報告",
        "agent_id": "mkt-rep",
    });
    let denied = handle_schedule_task(&args, home, "sales-rep").await;
    assert_eq!(denied["isError"], true, "{denied}");
    assert!(
        denied["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("委派遭拒"),
        "{denied}"
    );
    let store = duduclaw_gateway::cron_store::CronStore::open(home).unwrap();
    assert!(
        store.list_all().await.unwrap().is_empty(),
        "a denied schedule must persist nothing"
    );

    // Scheduling for yourself, and for someone you command, still works.
    for (caller, target) in [("sales-rep", "sales-rep"), ("sales-lead", "sales-rep")] {
        let ok = handle_schedule_task(
            &serde_json::json!({
                "name": format!("ok-{target}"),
                "cron": "0 9 * * *",
                "task": "日報",
                "agent_id": target,
            }),
            home,
            caller,
        )
        .await;
        assert!(ok.get("isError").is_none(), "{caller}→{target}: {ok}");
    }
}

/// `create_task` steps name the agent that will execute them, and the
/// gateway's TaskSpec executor spawns it directly (never via the bus). The
/// plan is refused up front rather than failing one step at a time.
#[tokio::test]
async fn create_task_step_targeting_a_stranger_is_denied() {
    let tmp = delegation_home();
    let home = tmp.path();
    let params = serde_json::json!({
        "goal": "季度報告",
        "steps": [
            { "description": "整理業務數字" },
            { "description": "跨部門要資料", "agent": "mkt-rep" },
        ],
    });
    let out = handle_create_task(&params, home, "sales-rep").await;
    assert_eq!(out["isError"], true, "{out}");
    let text = out["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("委派遭拒"), "{text}");
    assert!(text.contains("mkt-rep"), "{text}");

    // Steps on the caller itself, or on someone it commands, still plan.
    let ok = handle_create_task(
        &serde_json::json!({
            "goal": "季度報告",
            "steps": [
                { "description": "自己做" },
                { "description": "交給下屬", "agent": "sales-rep" },
            ],
        }),
        home,
        "sales-lead",
    )
    .await;
    assert!(ok.get("isError").is_none(), "{ok}");
}

#[tokio::test]
async fn ephemeral_member_cannot_create_unscanned_taskspec() {
    let home = tempfile::tempdir().unwrap();
    let result = handle_create_task(
        &serde_json::json!({
            "goal": "做一份摘要",
            "steps": [{"description": "摘要"}],
        }),
        home.path(),
        "eph-planner-123456",
    )
    .await;
    assert_eq!(result["isError"], true);
    assert!(
        !home.path().join("agents/eph-planner-123456").exists(),
        "a role member must not create a task outside the dispatcher scan"
    );
}

#[tokio::test]
async fn delegation_cross_department_peers_denied() {
    let tmp = delegation_home();
    let home = tmp.path();
    let err = check_delegation_allowed(home, "sales-rep", "mkt-rep", "send_to_agent")
        .await
        .expect_err("cross-department peers must be denied");
    assert!(
        err.contains("sales-rep") && err.contains("mkt-rep"),
        "got: {err}"
    );
    assert!(err.contains("委派遭拒"), "message must be zh-TW: {err}");

    // The denial is auditable.
    let audit = fs::read_to_string(home.join("tool_calls.jsonl")).expect("audit row written");
    assert!(audit.contains("delegation_denied"), "got: {audit}");
    assert!(audit.contains("different_department"), "got: {audit}");
    assert!(audit.contains("send_to_agent"), "got: {audit}");
}

/// The pre-WP21 sibling case: `researcher` and `writer` both report to
/// `ceo` and neither declares a department. Blank is never "the same
/// department", so they stay denied — with a reason that says why.
#[tokio::test]
async fn delegation_departmentless_siblings_denied() {
    let tmp = delegation_home();
    let home = tmp.path();
    let err = check_delegation_allowed(home, "researcher", "writer", "t")
        .await
        .expect_err("department-less siblings must stay denied");
    assert!(
        err.contains("未設定部門"),
        "expected the missing-department reason: {err}"
    );
    let audit = fs::read_to_string(home.join("tool_calls.jsonl")).unwrap();
    assert!(audit.contains("missing_department"), "got: {audit}");
}

#[tokio::test]
async fn delegation_self_denied() {
    let tmp = delegation_home();
    let err = check_delegation_allowed(tmp.path(), "sales-rep", "sales-rep", "t")
        .await
        .expect_err("self-delegation must be denied");
    assert!(err.contains("不可委派給自己"), "got: {err}");
}

/// The documented escape hatch: `[delegation] policy = "open"` restores the
/// pre-WP21 permissiveness for everything except self-delegation.
#[tokio::test]
async fn delegation_open_policy_restores_old_behaviour() {
    let tmp = delegation_home();
    let home = tmp.path();
    fs::write(
        home.join("config.toml"),
        "[delegation]\npolicy = \"open\"\n",
    )
    .unwrap();
    assert!(
        check_delegation_allowed(home, "sales-rep", "mkt-rep", "t")
            .await
            .is_ok()
    );
    assert!(
        check_delegation_allowed(home, "sales-rep", "sales-rep", "t")
            .await
            .is_err()
    );
}
