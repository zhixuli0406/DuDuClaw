use super::*;
use crate::mcp::caller_shims::handle_tasks_create;

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

/// v1.69.0: the removed aliases are no longer declared, so they are gone
/// from `tools/list`; what replaces each one is still declared.
#[test]
fn removed_aliases_are_not_declared_and_their_replacements_are() {
    for removed in duduclaw_core::tool_catalog::REMOVED_MCP_TOOLS {
        assert!(
            !tools().any(|t| t.name == removed.name),
            "{} was removed in v{} and must not be declared",
            removed.name,
            removed.removed_in
        );
        tool(removed.replacement);
    }
    // Never aliases, so never removed.
    tool("shared_wiki_delete");
    tool("wiki_share");
}

/// No declared tool still advertises a deprecation window: a leftover
/// `[deprecated → …]` marker would promise a name that no longer exists.
#[test]
fn no_declared_tool_carries_a_deprecation_marker() {
    for t in tools() {
        assert!(
            !t.description.starts_with("[deprecated"),
            "{} still carries a deprecation marker",
            t.name
        );
    }
}

/// O3: `scope` alone decides which wiki a `wiki_*` call addresses.
#[test]
fn wiki_scope_comes_from_the_scope_argument_only() {
    use crate::mcp_alias::{WikiScope, resolve_wiki_scope};
    assert_eq!(
        resolve_wiki_scope(&serde_json::json!({"scope": "shared"})).unwrap(),
        WikiScope::Shared
    );
    assert_eq!(resolve_wiki_scope(&serde_json::json!({})).unwrap(), WikiScope::Agent);
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
    // The destructive shared-wiki tool was deliberately NOT merged and keeps
    // its own name and scope.
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
/// the reminder rail.
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
    // A cron row was written.
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
    )
    .await;
    assert!(bad["isError"].as_bool().unwrap_or(false));
    let _ = std::fs::remove_dir_all(&home);
}
