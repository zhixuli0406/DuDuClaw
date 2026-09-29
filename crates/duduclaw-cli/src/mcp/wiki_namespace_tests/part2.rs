use super::*;

/// Verify that an admin-scoped caller is NOT rejected for authorization reasons.
/// (Index may not exist in temp dir, so we accept any non-auth error response.)
#[tokio::test(flavor = "current_thread")]
async fn audit_trail_query_proceeds_with_admin_scope() {
    let tmp = TempDir::new();
    let result = handle_audit_trail_query(
        &serde_json::json!({}),
        tmp.path(),
        "admin-client",
        true, // caller_is_admin = true
    )
    .await;
    // An admin call must NOT be rejected due to authorization.
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        !text.contains("Admin scope"),
        "admin call must not fail with auth scope error; got: {text}"
    );
}

// ── Test 6: wiki_search scoped to client namespace ────────────────────────
// External client has no wiki yet → response mentions "No wiki found"
// (rather than falling through to the internal default agent's wiki).
#[tokio::test(flavor = "current_thread")]
async fn wiki_search_scoped_to_client_namespace() {
    let tmp = TempDir::new();
    let client_id = "search-bot";
    let ctx = external_ns(client_id);
    let wiki_agent = wiki_agent_from_ns(&ctx, "dudu");

    // Create client agent dir but NO wiki inside it
    create_agent_dir(tmp.path(), client_id);
    // Also create dudu's wiki with a page — must NOT appear in search result
    create_agent_dir(tmp.path(), "dudu");
    let dudu_wiki = tmp.path().join("agents").join("dudu").join("wiki");
    fs::create_dir_all(&dudu_wiki).unwrap();
    fs::write(
        dudu_wiki.join("secret.md"),
        "---\ntitle: Secret\ncreated: 2026-04-29T00:00:00Z\nupdated: 2026-04-29T00:00:00Z\ntags: [internal]\nlayer: context\ntrust: 0.9\n---\nsecret internal content",
    )
    .unwrap();

    let args = serde_json::json!({ "query": "secret" });
    let result = handle_wiki_search(&args, tmp.path(), wiki_agent).await;
    let text = result["content"][0]["text"].as_str().unwrap_or("");

    assert!(
        text.contains("No wiki found") || text.contains("No wiki pages match"),
        "wiki_search must be scoped to client namespace; got: {text}"
    );
    assert!(
        !text.contains("secret internal content"),
        "internal agent content must not leak to external client search"
    );
}

// ── reliability_summary handler tests (W20-P0) ────────────────────────────

/// Non-admin caller must be rejected with isError=true.
#[tokio::test(flavor = "current_thread")]
async fn reliability_summary_denied_without_admin_scope() {
    let tmp = TempDir::new();
    let result = handle_reliability_summary(
        &serde_json::json!({"agent_id": "some-agent"}),
        tmp.path(),
        "non-admin-client",
        false,
    )
    .await;
    assert_eq!(result["isError"], serde_json::json!(true));
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("Admin scope"),
        "error must reference Admin scope; got: {text}"
    );
}

/// Missing agent_id parameter must return isError=true.
#[tokio::test(flavor = "current_thread")]
async fn reliability_summary_missing_agent_id() {
    let tmp = TempDir::new();
    let result =
        handle_reliability_summary(&serde_json::json!({}), tmp.path(), "admin-client", true)
            .await;
    assert_eq!(result["isError"], serde_json::json!(true));
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("agent_id"),
        "error must mention agent_id; got: {text}"
    );
}

/// Admin caller with valid params must NOT be rejected for auth reasons.
/// (Index may not exist in temp dir — we accept any non-auth response.)
#[tokio::test(flavor = "current_thread")]
async fn reliability_summary_proceeds_with_admin_scope() {
    let tmp = TempDir::new();
    let result = handle_reliability_summary(
        &serde_json::json!({"agent_id": "my-agent"}),
        tmp.path(),
        "admin-client",
        true,
    )
    .await;
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        !text.contains("Admin scope"),
        "admin call must not fail auth check; got: {text}"
    );
}

/// window_days defaults to 7 when not provided.
#[tokio::test(flavor = "current_thread")]
async fn reliability_summary_default_window_days() {
    let tmp = TempDir::new();
    let result = handle_reliability_summary(
        &serde_json::json!({"agent_id": "my-agent"}),
        tmp.path(),
        "admin-client",
        true,
    )
    .await;
    assert!(
        !result["isError"].as_bool().unwrap_or(false),
        "DB open must succeed: {:?}",
        result
    );
    let wd = result["reliability_summary"]["window_days"]
        .as_u64()
        .unwrap_or(0);
    assert_eq!(wd, 7, "default window_days must be 7");
}

/// window_days > 365 must be clamped to 365.
#[tokio::test(flavor = "current_thread")]
async fn reliability_summary_window_days_clamped() {
    let tmp = TempDir::new();
    let result = handle_reliability_summary(
        &serde_json::json!({"agent_id": "my-agent", "window_days": 9999}),
        tmp.path(),
        "admin-client",
        true,
    )
    .await;
    assert!(
        !result["isError"].as_bool().unwrap_or(false),
        "DB open must succeed: {:?}",
        result
    );
    let wd = result["reliability_summary"]["window_days"]
        .as_u64()
        .unwrap_or(0);
    assert_eq!(wd, 365, "window_days must be clamped to 365");
}

/// window_days=1 must pass through without being clamped (lower bound is 1).
#[tokio::test(flavor = "current_thread")]
async fn reliability_summary_window_days_min_boundary() {
    let tmp = TempDir::new();
    let result = handle_reliability_summary(
        &serde_json::json!({"agent_id": "my-agent", "window_days": 1}),
        tmp.path(),
        "admin-client",
        true,
    )
    .await;
    assert!(
        !result["isError"].as_bool().unwrap_or(false),
        "DB open must succeed: {:?}",
        result
    );
    let wd = result["reliability_summary"]["window_days"]
        .as_u64()
        .unwrap_or(0);
    assert_eq!(wd, 1, "window_days=1 must not be clamped");
}

/// agent_id consisting only of whitespace must be rejected (not accepted as non-empty).
#[tokio::test(flavor = "current_thread")]
async fn reliability_summary_whitespace_agent_id_rejected() {
    let tmp = TempDir::new();
    let result = handle_reliability_summary(
        &serde_json::json!({"agent_id": "   "}),
        tmp.path(),
        "admin-client",
        true,
    )
    .await;
    assert!(
        result["isError"].as_bool().unwrap_or(false),
        "whitespace-only agent_id must be rejected as missing"
    );
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("agent_id"),
        "error message must mention agent_id; got: {text}"
    );
}

/// agent_id exceeding MAX_AGENT_ID_LEN must be rejected.
#[tokio::test(flavor = "current_thread")]
async fn reliability_summary_agent_id_too_long_rejected() {
    let tmp = TempDir::new();
    let long_id = "a".repeat(129);
    let result = handle_reliability_summary(
        &serde_json::json!({"agent_id": long_id}),
        tmp.path(),
        "admin-client",
        true,
    )
    .await;
    assert!(
        result["isError"].as_bool().unwrap_or(false),
        "agent_id longer than 128 chars must be rejected"
    );
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("128"),
        "error message must mention the 128-char limit; got: {text}"
    );
}

/// tool_requires_scope must map reliability_summary → Admin.
#[test]
fn reliability_summary_scope_is_admin() {
    use crate::mcp_auth::{Scope, tool_requires_scope};
    assert_eq!(
        tool_requires_scope("reliability_summary"),
        Some(Scope::Admin),
        "reliability_summary must require Admin scope"
    );
}

// ── TC-SKILL-RUN-01: skill_synthesis_run visible to internal principal ──────
// TDD 驗收：W20-P0 修復 — skill_synthesis_run 工具缺失
// 保證 internal agents（Cron pipeline、ENG-AGENT）可以看到此工具。
#[tokio::test(flavor = "current_thread")]
async fn skill_synthesis_run_visible_to_internal_principal() {
    use serde_json::json;
    let id = json!(1);

    let response = super::handle_tools_list(
        &id,
        &super::test_principal(false),
        tmp_home_for_tools_list().path(),
    ).await;
    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools must be array");

    let names: Vec<&str> = tools
        .iter()
        .map(|t| t["name"].as_str().unwrap_or(""))
        .collect();

    assert!(
        names.contains(&"skill_synthesis_run"),
        "skill_synthesis_run must appear in internal tools list; got: {:?}",
        names
    );
}

// ── TC-SKILL-RUN-02: skill_synthesis_run NOT visible to external principal ──
// 安全性驗收：外部 client（Claude Desktop 等）不應能觸發 pipeline。
#[tokio::test(flavor = "current_thread")]
async fn skill_synthesis_run_hidden_from_external_principal() {
    use serde_json::json;
    let id = json!(1);

    let response = super::handle_tools_list(
        &id,
        &super::test_principal(true),
        tmp_home_for_tools_list().path(),
    ).await;
    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools must be array");

    let names: Vec<&str> = tools
        .iter()
        .map(|t| t["name"].as_str().unwrap_or(""))
        .collect();

    assert!(
        !names.contains(&"skill_synthesis_run"),
        "skill_synthesis_run must NOT appear in external tools list (security); got: {:?}",
        names
    );
}

// ── TC-PIPELINE-MCP-01: rollout-to-skill-v2 pipeline tools all registered ───
// Regression guard for the 2026-05-07 incident: a stale gateway binary
// pre-dating commit 4bf65cb (W20-P0) reported "tool_not_in_mcp_registry"
// for the four tools listed below, even though the pipeline expected
// them. Pin every tool the pipeline touches so any future move that
// hides one of them under `is_external=true` (or removes it) breaks the
// build instead of silently breaking the pipeline.
#[tokio::test(flavor = "current_thread")]
async fn rollout_to_skill_pipeline_tools_visible_to_internal_principal() {
    use serde_json::json;

    let response = super::handle_tools_list(
        &json!(1),
        &super::test_principal(false),
        tmp_home_for_tools_list().path(),
    ).await;
    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools must be array");
    let names: Vec<&str> = tools
        .iter()
        .map(|t| t["name"].as_str().unwrap_or(""))
        .collect();

    for required in &[
        "memory_episodic_pressure",
        "skill_synthesis_status",
        "skill_synthesis_run",
        "activity_post",
    ] {
        assert!(
            names.contains(required),
            "internal principal must see {required}; got: {names:?}"
        );
    }
}

// ── TC-MCP-EXTERNAL-WHITELIST-01: pipeline tools NOT exposed to external ────
// Companion to the regression test above — security guard. External MCP
// clients (Claude Desktop, third-party connectors) must NEVER see the
// pipeline orchestration tools, regardless of how the registry is
// refactored. Hard-pinned to the W19-P0 BUG-QA-001 whitelist.
#[tokio::test(flavor = "current_thread")]
async fn rollout_to_skill_pipeline_tools_hidden_from_external_principal() {
    use serde_json::json;

    let response = super::handle_tools_list(
        &json!(1),
        &super::test_principal(true),
        tmp_home_for_tools_list().path(),
    ).await;
    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools must be array");
    let names: Vec<&str> = tools
        .iter()
        .map(|t| t["name"].as_str().unwrap_or(""))
        .collect();

    for forbidden in &[
        "memory_episodic_pressure",
        "skill_synthesis_status",
        "skill_synthesis_run",
        "activity_post",
    ] {
        assert!(
            !names.contains(forbidden),
            "external principal must NOT see {forbidden}; got: {names:?}"
        );
    }
}

// ── TC-MCP-SCHEMA-01: pipeline tool schemas are non-empty ────────────────
// Each pipeline tool must declare a non-empty description so generated
// tool catalogues remain self-documenting. Catches the failure mode
// where a refactor accidentally drops the description string.
#[tokio::test(flavor = "current_thread")]
async fn pipeline_tool_descriptions_are_non_empty() {
    use serde_json::json;

    let response = super::handle_tools_list(
        &json!(1),
        &super::test_principal(false),
        tmp_home_for_tools_list().path(),
    ).await;
    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools must be array");

    for required in &[
        "memory_episodic_pressure",
        "skill_synthesis_status",
        "skill_synthesis_run",
        "activity_post",
    ] {
        let tool = tools
            .iter()
            .find(|t| t["name"].as_str() == Some(*required))
            .unwrap_or_else(|| panic!("{required} must be present"));
        let desc = tool["description"].as_str().unwrap_or("");
        assert!(!desc.is_empty(), "{required} description must not be empty");
    }
}

// ── TC-SKILL-RUN-03: skill_synthesis_run schema is well-formed ───────────
// 驗收：工具定義包含正確的 name、description 和 parameters。
#[tokio::test(flavor = "current_thread")]
async fn skill_synthesis_run_schema_is_well_formed() {
    use serde_json::json;
    let id = json!(1);

    let response = super::handle_tools_list(
        &id,
        &super::test_principal(false),
        tmp_home_for_tools_list().path(),
    ).await;
    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools must be array");

    let tool = tools
        .iter()
        .find(|t| t["name"].as_str() == Some("skill_synthesis_run"))
        .expect("skill_synthesis_run must be present in internal tools list");

    // name
    assert_eq!(tool["name"].as_str(), Some("skill_synthesis_run"));

    // description must be non-empty
    let desc = tool["description"].as_str().unwrap_or("");
    assert!(
        !desc.is_empty(),
        "skill_synthesis_run must have a non-empty description"
    );

    // inputSchema must have properties: agent_id, dry_run, lookback_days
    let props = &tool["inputSchema"]["properties"];
    for param in &["agent_id", "dry_run", "lookback_days"] {
        assert!(
            props.get(param).is_some(),
            "skill_synthesis_run inputSchema must have property '{}'; schema: {}",
            param,
            tool["inputSchema"]
        );
    }
}

// ── B2: codrive_run must surface C-L2/C-L3, not just C-L1 coordinates ──
// Regression for the gap where `codrive_run`'s description/schema never
// mentioned `api_action` (C-L2 registry) or `locate` (C-L3 AT-SPI2) even
// though `CodriveStep` has supported both fields since CD-4a/CD-4b — an
// LLM caller reading only this tool's advertised shape could never
// produce a script that uses either rung.
#[tokio::test(flavor = "current_thread")]
async fn codrive_run_schema_surfaces_api_action_and_locate() {
    use serde_json::json;
    let id = json!(1);

    // O7: `codrive_run` is hidden from an agent without `[capabilities]
    // codrive` (discoverable ⇔ callable), so opt this caller in the way an
    // operator would before inspecting the advertised schema.
    let response = super::handle_tools_list(
        &id,
        &super::test_principal(false),
        tmp_home_with_all_capabilities().path(),
    ).await;
    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools must be array");

    let tool = tools
        .iter()
        .find(|t| t["name"].as_str() == Some("codrive_run"))
        .expect("codrive_run must be present in internal tools list");

    let desc = tool["description"].as_str().unwrap_or("");
    assert!(
        !desc.is_empty(),
        "codrive_run must have a non-empty description"
    );
    // The invariant is "an LLM reading this tool's ADVERTISED SHAPE can drive
    // either rung". O7 capped the prose at 200 bytes and moved the rung
    // detail into the parameter schema, which rides the same wire and is read
    // by the same model — so the keywords are checked over the whole
    // advertised tool object rather than the description string alone.
    let advertised = tool.to_string();
    for keyword in ["api_action", "locate", "C-L2", "C-L3", "target_app"] {
        assert!(
            advertised.contains(keyword),
            "codrive_run's advertised schema must mention '{keyword}'; got: {advertised}"
        );
    }

    // The `script` param's description is where this tool's whole
    // schema convention lives (see `build_tool_schema` — every param is
    // typed as a bare JSON-Schema string, so the actual object shape is
    // carried in the description text). It must spell out both fields'
    // shapes, not just name-drop them.
    let script_desc = tool["inputSchema"]["properties"]["script"]["description"]
        .as_str()
        .unwrap_or("");
    assert!(
        !script_desc.is_empty(),
        "codrive_run script param must have a non-empty description"
    );
    for keyword in ["api_action?:{action, params?}", "locate?:{role, name}"] {
        assert!(
            script_desc.contains(keyword),
            "codrive_run script description must spell out '{keyword}'; description: {script_desc}"
        );
    }
}

// ── A2: `codrive_status` is advertised, Admin-scoped, and catalogued ──
// The drift guard for the read-only driving-state query. It must be
// discoverable (an undeclared MCP tool is an uncallable one), it must
// name the three modes it can return so a caller can act on them, and
// it must be in the shared capability catalog with the same scope the
// security gate enforces (`mcp_auth::tool_requires_scope`, whose own
// `test_catalog_scopes_match_tool_requires_scope` closes the loop).
#[tokio::test(flavor = "current_thread")]
async fn codrive_status_is_advertised_and_names_the_three_modes() {
    use serde_json::json;
    let id = json!(1);

    // O7: same codrive capability opt-in as the sibling test above.
    let response = super::handle_tools_list(
        &id,
        &super::test_principal(false),
        tmp_home_with_all_capabilities().path(),
    ).await;
    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools must be array");

    let tool = tools
        .iter()
        .find(|t| t["name"].as_str() == Some("codrive_status"))
        .expect("codrive_status must be present in internal tools list");

    let desc = tool["description"].as_str().unwrap_or("");
    assert!(
        !desc.is_empty(),
        "codrive_status must have a non-empty description"
    );
    for keyword in ["human", "codrive", "handover", "shadow", "watch"] {
        assert!(
            desc.contains(keyword),
            "codrive_status description must mention '{keyword}'; description: {desc}"
        );
    }
    // A read-only query takes no parameters — pinned so nobody quietly
    // grows it an identity override it has no use for.
    let props = tool["inputSchema"]["properties"]
        .as_object()
        .expect("inputSchema.properties must be an object");
    assert!(
        props.is_empty(),
        "codrive_status must take no parameters: {props:?}"
    );
}

#[test]
fn codrive_status_is_in_the_builtin_tool_catalog_under_the_codrive_category() {
    let entry = duduclaw_core::tool_catalog::builtin_tool_catalog()
        .into_iter()
        .find(|e| e.name == "codrive_status")
        .expect("codrive_status must be in the built-in tool catalog");
    assert_eq!(entry.scope, "admin");
    assert_eq!(entry.category, "codrive");
    assert_eq!(entry.kind, "mcp");
    assert_eq!(entry.qualified, "mcp__duduclaw__codrive_status");
}

// ── G5 hub + curator tools ──────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn g5_skill_tools_visible_internal_hidden_external() {
    use serde_json::json;
    let internal = super::handle_tools_list(
        &json!(1),
        &super::test_principal(false),
        tmp_home_for_tools_list().path(),
    ).await;
    let internal_names: Vec<&str> = internal["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap_or(""))
        .collect();
    for tool in &["skill_hub_install", "skill_curator_status", "skill_pin"] {
        assert!(
            internal_names.contains(tool),
            "{tool} must be in the internal list"
        );
    }
    // skill_search must expose the optional hub param.
    let search = internal["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"].as_str() == Some("skill_search"))
        .unwrap();
    assert!(search["inputSchema"]["properties"].get("hub").is_some());

    let external = super::handle_tools_list(
        &json!(1),
        &super::test_principal(true),
        tmp_home_for_tools_list().path(),
    ).await;
    let external_names: Vec<&str> = external["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap_or(""))
        .collect();
    for tool in &["skill_hub_install", "skill_curator_status", "skill_pin"] {
        assert!(
            !external_names.contains(tool),
            "{tool} must NOT be exposed to external principals"
        );
    }
}
