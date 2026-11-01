//! P2-A security review Q7: the three responsibility tools go through the
//! same `McpDispatcher` gates as every other tool — `denied_tools`,
//! `scoped_tools` and the approval lists — and reach their handler only
//! after those pass.

use super::*;

const TOOLS: [&str; 3] = [
    "responsibility_get",
    "responsibility_followup",
    "responsibility_ask",
];

fn args_for(tool: &str) -> Value {
    match tool {
        "responsibility_followup" => {
            serde_json::json!({"responsibility_id": "r1", "due_at": "2099-01-01T00:00:00Z"})
        }
        "responsibility_ask" => serde_json::json!({"responsibility_id": "r1", "question": "q"}),
        _ => serde_json::json!({}),
    }
}

fn enable_feature(tmp: &tempfile::TempDir) {
    std::fs::write(
        tmp.path().join("config.toml"),
        "[dispatch]\nenabled = true\n\n[responsibilities]\nenabled = true\n",
    )
    .unwrap();
}

async fn call(tmp: &tempfile::TempDir, tool: &str, id: i64) -> Value {
    let dispatcher = make_dispatcher(tmp).await;
    dispatcher
        .dispatch_tool_call(
            &make_principal(vec![Scope::Admin], false),
            &make_ns_ctx(false),
            &make_params(tool, args_for(tool)),
            &serde_json::json!(id),
        )
        .await
}

#[tokio::test]
async fn denied_tools_blocks_each_responsibility_tool() {
    for (i, tool) in TOOLS.iter().enumerate() {
        let tmp = tempfile::TempDir::new().unwrap();
        enable_feature(&tmp);
        write_scoped_toml(
            &tmp,
            &format!("[capabilities]\ndenied_tools = [\"{tool}\"]\n"),
        );
        let result = call(&tmp, tool, 900 + i as i64).await;
        assert_eq!(result["error"]["code"], -32003, "{tool}: {result}");
        assert!(
            result["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("denied_tools"),
            "{tool}: {result}"
        );
    }
}

#[tokio::test]
async fn scoped_tools_without_a_grant_blocks_each_responsibility_tool() {
    for (i, tool) in TOOLS.iter().enumerate() {
        let tmp = tempfile::TempDir::new().unwrap();
        enable_feature(&tmp);
        write_scoped_toml(
            &tmp,
            &format!("[capabilities]\nscoped_tools = [\"{tool}\"]\n"),
        );
        let result = call(&tmp, tool, 910 + i as i64).await;
        assert_eq!(result["error"]["code"], -32003, "{tool}: {result}");
        assert!(
            result["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("capability_request"),
            "{tool}: {result}"
        );
    }
}

#[tokio::test]
async fn approval_required_tools_holds_each_responsibility_tool() {
    // The internal key is not an employee identity on its own. Production
    // verifies/stamps this process identity at MCP startup; isolate it here
    // so parallel tests never observe a process-global environment mutation.
    const CHILD: &str = "DUDU_TEST_RESP_APPROVAL_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "mcp_dispatch::tests::resp_tests::approval_required_tools_holds_each_responsibility_tool",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .env(duduclaw_core::ENV_AGENT_ID, "dudu")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    for tool in TOOLS {
        let tmp = tempfile::TempDir::new().unwrap();
        enable_feature(&tmp);
        write_acting_agent_toml(
            &tmp,
            &format!("[capabilities]\napproval_required_tools = [\"{tool}\"]\n"),
        );
        let (rec, result) =
            dispatch_held_then_decide(&tmp, internal_principal(), tool, args_for(tool), false)
                .await;
        assert_eq!(rec.agent_id, "dudu", "{tool}");
        assert_eq!(result["error"]["code"], -32003, "{tool}: {result}");
    }
}

/// With no list naming them, the calls reach the handlers (a handler-level
/// tool error, never a gate refusal).
#[tokio::test]
async fn unlisted_responsibility_tools_reach_their_handlers() {
    let tmp = tempfile::TempDir::new().unwrap();
    enable_feature(&tmp);
    write_scoped_toml(
        &tmp,
        "[agent]\nname = \"test-client\"\nstatus = \"active\"\n",
    );
    for (i, tool) in TOOLS.iter().enumerate() {
        let result = call(&tmp, tool, 920 + i as i64).await;
        assert!(result.get("error").is_none(), "{tool}: {result}");
        assert!(result.get("result").is_some(), "{tool}: {result}");
    }
}
