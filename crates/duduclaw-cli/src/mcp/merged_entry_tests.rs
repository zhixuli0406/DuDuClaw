use super::*;

fn tool(name: &str) -> &'static ToolDef {
    tools()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("tool {name} must exist"))
}

/// O3: the six merged wiki tools all take `scope`, and it is optional —
/// an existing call that omits it keeps addressing the agent's own wiki.
#[test]
fn merged_wiki_tools_expose_an_optional_scope_param() {
    for name in [
        "wiki_ls",
        "wiki_read",
        "wiki_write",
        "wiki_search",
        "wiki_stats",
        "wiki_lint",
    ] {
        let p = tool(name)
            .params
            .iter()
            .find(|p| p.name == "scope")
            .unwrap_or_else(|| panic!("{name} must take a scope param"));
        assert!(!p.required, "{name}.scope must stay optional");
    }
}

/// O4: `tasks_create` is the merged creation entry — `kind` and
/// `schedule` both optional so today's callers are unaffected.
#[test]
fn tasks_create_exposes_optional_kind_and_schedule() {
    for param in ["kind", "schedule"] {
        let p = tool("tasks_create")
            .params
            .iter()
            .find(|p| p.name == param)
            .unwrap_or_else(|| panic!("tasks_create must take {param}"));
        assert!(!p.required, "tasks_create.{param} must stay optional");
    }
}

/// O13: `skill_search` is the merged search entry.
#[test]
fn skill_search_exposes_optional_source() {
    let p = tool("skill_search")
        .params
        .iter()
        .find(|p| p.name == "source")
        .expect("skill_search must take a source param");
    assert!(!p.required);
}

/// Deprecation contract: every deprecated alias is **still declared** in
/// `TOOLS` (hiding it from `tools/list` would make it uncallable, which is
/// the opposite of a deprecation window) and its description opens with a
/// machine-greppable `[deprecated → …]` marker naming the replacement.
#[test]
fn deprecated_aliases_stay_listed_and_are_marked() {
    for name in duduclaw_core::tool_catalog::DEPRECATED_MCP_TOOLS {
        let t = tool(name);
        assert!(
            t.description.starts_with("[deprecated → "),
            "{name} description must open with the deprecation marker; got: {}",
            &t.description[..t.description.len().min(60)]
        );
        assert!(
            t.description.contains("v1.68.0"),
            "{name} must name its removal version"
        );
    }
}

/// …and a non-deprecated tool must not carry the marker (so the test above
/// cannot pass by marking everything).
#[test]
fn merged_entry_points_carry_no_deprecation_marker() {
    for name in ["wiki_ls", "wiki_write", "tasks_create", "skill_search"] {
        assert!(
            !tool(name).description.starts_with("[deprecated"),
            "{name} is the replacement, not the alias"
        );
    }
}

/// O3 alias equivalence: the shared aliases and the merged names resolve
/// to the same scope, so a caller that keeps using the old name reaches
/// exactly the same handler.
#[test]
fn shared_wiki_aliases_resolve_to_the_same_scope_as_the_merged_name() {
    use crate::mcp_alias::{WikiScope, resolve_wiki_scope};
    let shared = serde_json::json!({"scope": "shared"});
    for (alias, merged) in [
        ("shared_wiki_ls", "wiki_ls"),
        ("shared_wiki_read", "wiki_read"),
        ("shared_wiki_write", "wiki_write"),
        ("shared_wiki_search", "wiki_search"),
        ("shared_wiki_stats", "wiki_stats"),
        ("shared_wiki_lint", "wiki_lint"),
    ] {
        assert_eq!(
            resolve_wiki_scope(alias, &serde_json::json!({})).unwrap(),
            resolve_wiki_scope(merged, &shared).unwrap(),
            "{alias} and {merged} scope=\"shared\" must agree"
        );
        assert_eq!(
            resolve_wiki_scope(alias, &serde_json::json!({})).unwrap(),
            WikiScope::Shared
        );
    }
}

/// The merged wiki entry point keeps `wiki:read` / `wiki:write` — adding a
/// `scope` parameter must not have widened either tool's authority.
#[test]
fn merged_wiki_tools_keep_their_original_scopes() {
    use crate::mcp_auth::{Scope, tool_requires_scope};
    for name in ["wiki_ls", "wiki_read", "wiki_search", "wiki_stats", "wiki_lint"] {
        assert_eq!(
            tool_requires_scope(name),
            Some(Scope::WikiRead),
            "{name} must stay wiki:read"
        );
    }
    assert_eq!(tool_requires_scope("wiki_write"), Some(Scope::WikiWrite));
    // …and the shared aliases keep theirs, including the destructive one
    // that was deliberately NOT merged.
    assert_eq!(
        tool_requires_scope("shared_wiki_delete"),
        Some(Scope::WikiWrite)
    );
}

/// O4: `tasks_create` with a malformed `kind` / `schedule` fails closed —
/// it must never silently fall back to creating a plain board task.
#[tokio::test(flavor = "current_thread")]
async fn tasks_create_rejects_unknown_kind_and_malformed_schedule() {
    let home = std::env::temp_dir().join(format!("duduclaw-o4-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();

    let bad_kind = handle_tasks_create(
        &serde_json::json!({"title": "t", "kind": "goals"}),
        &home,
        "a",
    )
    .await;
    assert!(bad_kind["isError"].as_bool().unwrap_or(false));

    let bad_sched = handle_tasks_create(
        &serde_json::json!({"title": "t", "schedule": "tomorrow"}),
        &home,
        "a",
    )
    .await;
    assert!(bad_sched["isError"].as_bool().unwrap_or(false));

    // A goal cannot also be scheduled — refused, never half-applied.
    let both = handle_tasks_create(
        &serde_json::json!({"title": "t", "kind": "goal", "schedule": "0 9 * * *"}),
        &home,
        "a",
    )
    .await;
    assert!(both["isError"].as_bool().unwrap_or(false));

    // A one-shot schedule with nowhere to deliver is refused rather than
    // creating an undeliverable reminder.
    let orphan = handle_tasks_create(
        &serde_json::json!({"title": "t", "schedule": "2099-01-01T09:00:00Z"}),
        &home,
        "a",
    )
    .await;
    assert!(orphan["isError"].as_bool().unwrap_or(false));

    let _ = std::fs::remove_dir_all(&home);
}

/// O4: `kind="goal"` produces a goal-mode task whose acceptance contract
/// is frozen at creation — the same invariant the dashboard rail asserts,
/// because both now run `goal_create_core::create_goal_task`.
#[tokio::test(flavor = "current_thread")]
async fn tasks_create_kind_goal_freezes_the_acceptance_contract() {
    let home = std::env::temp_dir().join(format!("duduclaw-o4g-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();

    let out = handle_tasks_create(
        &serde_json::json!({
            "title": "整理九月報表",
            "kind": "goal",
            "acceptance_criteria": "含營收圖表",
        }),
        &home,
        "a",
    )
    .await;
    assert!(
        !out["isError"].as_bool().unwrap_or(false),
        "goal create failed: {out}"
    );
    let text = out["content"][0]["text"].as_str().unwrap_or("");
    let parsed: serde_json::Value = serde_json::from_str(text).expect("json payload");
    assert_eq!(parsed["kind"], "goal");
    assert_eq!(parsed["task"]["goal_mode"], serde_json::json!(true));
    assert_eq!(parsed["task"]["acceptance_criteria_baseline"], "含營收圖表");

    let _ = std::fs::remove_dir_all(&home);
}

/// O4: a cron `schedule` lands on the cron rail and a one-shot lands on
/// the reminder rail — the same objects the deprecated aliases produce.
#[tokio::test(flavor = "current_thread")]
async fn tasks_create_schedule_routes_to_the_cron_and_reminder_rails() {
    let home = std::env::temp_dir().join(format!("duduclaw-o4s-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();

    let cron = handle_tasks_create(
        &serde_json::json!({
            "title": "daily-report",
            "description": "整理昨日數據",
            "schedule": "0 9 * * *",
            "cron_timezone": "Asia/Taipei",
        }),
        &home,
        "a",
    )
    .await;
    assert!(
        !cron["isError"].as_bool().unwrap_or(false),
        "cron schedule failed: {cron}"
    );
    // Same receipt shape `schedule_task` produces (a cron row was written).
    let rows = duduclaw_gateway::cron_store::CronStore::open(&home)
        .expect("cron store")
        .list_all()
        .await
        .expect("list");
    assert_eq!(rows.len(), 1, "exactly one cron row must exist");
    assert_eq!(rows[0].cron, "0 9 * * *");
    assert_eq!(rows[0].agent_id, "a");

    // Within the reminder scheduler's MAX_FUTURE_DAYS window — a fixed
    // far-future literal would start failing once it aged past the cap.
    let at = (chrono::Utc::now() + chrono::Duration::days(30)).to_rfc3339();
    let once = handle_tasks_create(
        &serde_json::json!({
            "title": "one-shot",
            "description": "提醒我結帳",
            "schedule": at,
            "notify_channel": "telegram",
            "notify_chat_id": "12345",
        }),
        &home,
        "a",
    )
    .await;
    assert!(
        !once["isError"].as_bool().unwrap_or(false),
        "one-shot schedule failed: {once}"
    );
    let reminders =
        duduclaw_gateway::reminder_scheduler::list_reminders(&home, None, Some("a")).await;
    assert_eq!(reminders.len(), 1, "exactly one reminder must exist");
    assert_eq!(reminders[0].channel, "telegram");
    assert_eq!(reminders[0].prompt.as_deref(), Some("提醒我結帳"));

    let _ = std::fs::remove_dir_all(&home);
}

/// O13: `source="bank"` is honest about an empty store instead of
/// silently returning hub results.
#[tokio::test(flavor = "current_thread")]
async fn skill_search_bank_source_reports_an_empty_store() {
    let home = std::env::temp_dir().join(format!("duduclaw-o13-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let out = handle_skill_search(
        &serde_json::json!({"query": "pdf", "source": "bank"}),
        &home,
        "skill_search",
    )
    .await;
    let text = out["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("learned skill bank holds no entries"),
        "expected an honest empty-store note; got: {text}"
    );
    // `hub` is meaningless for the bank and is refused rather than ignored.
    let bad = handle_skill_search(
        &serde_json::json!({"query": "pdf", "source": "bank", "hub": "github"}),
        &home,
        "skill_search",
    )
    .await;
    assert!(bad["isError"].as_bool().unwrap_or(false));
    let _ = std::fs::remove_dir_all(&home);
}
