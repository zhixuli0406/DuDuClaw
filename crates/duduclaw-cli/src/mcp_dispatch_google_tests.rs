//! Dedicated Google keys must pass the real shared dispatcher, not a parallel
//! policy implementation. These tests stop at the named gate before any API.
use super::*;
use crate::mcp_auth::{authenticate_with_key_call, with_authenticated_call};
use serde_json::json;

const KEY: &str = "ddc_dev_0123456789abcdef0123456789abcdef";
fn fixture(capabilities: &str) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents/employee-a")).unwrap();
    std::fs::write(
        home.path().join("agents/employee-a/agent.toml"),
        format!("[agent]\nname='employee-a'\n{capabilities}\n"),
    )
    .unwrap();
    std::fs::write(home.path().join("config.toml"),format!(
        "[integrations]\ngoogle_workspace=true\n[mcp_keys.{KEY}]\nclient_id='employee-a'\nscopes=['google:read']\nis_external=true\ncreated_at='{}'\n",
        chrono::Utc::now().to_rfc3339(),
    )).unwrap();
    home
}
async fn dispatcher(home: &Path) -> McpDispatcher {
    McpDispatcher::new(
        home.to_path_buf(),
        reqwest::Client::new(),
        Arc::new(SqliteMemoryEngine::new(&home.join("memory.db")).unwrap()),
        "different-process-agent".into(),
        Arc::new(crate::odoo_pool::OdooConnectorPool::default()),
        RateLimiter::new(),
        DailyQuota::new(),
    )
}
fn namespace() -> NamespaceContext {
    NamespaceContext {
        write_namespace: "external/employee-a".into(),
        read_namespaces: vec!["external/employee-a".into()],
    }
}

#[tokio::test]
async fn dedicated_google_runs_denied_allowlist_action_policy_and_task_grant_gates() {
    for (capabilities, expected) in [
        (
            "[capabilities]\ndenied_tools=['gmail_search']",
            "denied_tools",
        ),
        (
            "[capabilities]\nallowed_tools=['gmail_read']",
            "allowed_tools",
        ),
        (
            "[[capabilities.action_rules]]\neffect='read'\nverdict='block'",
            "action_rules",
        ),
        (
            "[[capabilities.policy]]\ntool='gmail_search'\neffect='forbid'",
            "Denied by policy",
        ),
        ("[capabilities]\nscoped_tools=['gmail_search']", "授權"),
    ] {
        let home = fixture(capabilities);
        let parsed: PolicyOnlyConfig = toml::from_str(
            &std::fs::read_to_string(home.path().join("agents/employee-a/agent.toml")).unwrap(),
        )
        .unwrap();
        assert!(
            !parsed.capabilities.action_rules.malformed,
            "invalid action-rule fixture: {capabilities}"
        );
        let dispatcher = dispatcher(home.path()).await;
        let call = authenticate_with_key_call(KEY, home.path()).unwrap();
        let principal = call.principal().clone();
        let result = with_authenticated_call(
            call,
            dispatcher.dispatch_tool_call(
                &principal,
                &namespace(),
                &json!({"name":"gmail_search","arguments":{"query":"fixture"}}),
                &json!(1),
            ),
        )
        .await;
        assert_eq!(result["error"]["code"], -32003, "{capabilities}: {result}");
        assert!(
            result["error"]["message"]
                .as_str()
                .unwrap()
                .contains(expected),
            "{capabilities}: {result}"
        );
    }
}

#[tokio::test]
async fn dedicated_google_cannot_skip_actionguard_approval_when_broker_is_unavailable() {
    let home = fixture("[capabilities]\napproval_required_tools=['gmail_search']");
    std::fs::create_dir(home.path().join("approvals.db")).unwrap();
    let dispatcher = dispatcher(home.path()).await;
    let call = authenticate_with_key_call(KEY, home.path()).unwrap();
    let principal = call.principal().clone();
    let result = with_authenticated_call(
        call,
        dispatcher.dispatch_tool_call(
            &principal,
            &namespace(),
            &json!({"name":"gmail_search","arguments":{"query":"fixture"}}),
            &json!(2),
        ),
    )
    .await;
    assert_eq!(result["error"]["code"], -32003, "{result}");
    assert!(
        result["error"]["message"]
            .as_str()
            .unwrap()
            .contains("審批系統"),
        "{result}"
    );
}

#[tokio::test]
async fn dedicated_google_listing_matches_scope_employee_denial_and_task_grants() {
    let home = fixture(
        "[capabilities]\nallowed_tools=['mcp__duduclaw__*']\ndenied_tools=['gmail_read']\nscoped_tools=['sheets_read']",
    );
    let call = authenticate_with_key_call(KEY, home.path()).unwrap();
    let principal = call.principal().clone();
    let names = with_authenticated_call(call, async {
        crate::mcp::visible_tool_names(&principal, home.path(), "different-process-agent").await
    })
    .await;
    assert!(names.contains(&"gmail_search"), "{names:?}");
    for tool in [
        "gmail_read",
        "gmail_create_draft",
        "sheets_read",
        "github_status",
        "os_open",
        "mail_read",
    ] {
        assert!(!names.contains(&tool), "{tool}: {names:?}");
    }
    let names =
        crate::mcp::visible_tool_names(&principal, home.path(), "different-process-agent").await;
    assert!(
        !names.contains(&"gmail_search"),
        "missing receipt: {names:?}"
    );
}
