use super::*;

// ── Test 1: external ns → client_id is wiki agent ─────────────────────────
#[test]
fn external_ns_resolves_to_client_id() {
    let ctx = external_ns("claude-desktop");
    assert_eq!(wiki_agent_from_ns(&ctx, "dudu"), "claude-desktop");
}

// ── Test 2: internal ns → falls back to default_agent ─────────────────────
#[test]
fn internal_ns_falls_back_to_default_agent() {
    let ctx = internal_ns("duduclaw-tl");
    assert_eq!(wiki_agent_from_ns(&ctx, "dudu"), "dudu");
}

// ── Test 3: default principal ns → falls back to default_agent ───────────
#[test]
fn default_internal_ns_falls_back() {
    let ctx = NamespaceContext {
        write_namespace: "internal/default".to_string(),
        read_namespaces: vec!["internal/default".to_string(), "shared/public".to_string()],
    };
    assert_eq!(wiki_agent_from_ns(&ctx, "dudu"), "dudu");
}

// ── Test 4: wiki_write external client uses client's namespace ────────────
#[tokio::test(flavor = "current_thread")]
async fn wiki_write_external_scoped_to_client_namespace() {
    let tmp = TempDir::new();
    let client_id = "claude-desktop";
    let ctx = external_ns(client_id);
    let wiki_agent = wiki_agent_from_ns(&ctx, "dudu");

    // Create the external client's agent directory (simulates provisioning)
    create_agent_dir(tmp.path(), client_id);

    // Args have NO agent_id — simulates the dispatcher stripping it
    let args = serde_json::json!({
        "page_path": "notes/hello.md",
        "content": "---\ntitle: Hello\ncreated: 2026-04-29T00:00:00Z\nupdated: 2026-04-29T00:00:00Z\ntags: [test]\nlayer: context\ntrust: 0.5\n---\nBody.",
    });

    let result = handle_wiki_write(&args, tmp.path(), wiki_agent).await;
    let is_err = result["isError"].as_bool().unwrap_or(false);
    assert!(
        !is_err,
        "wiki_write should succeed for external client: {:?}",
        result
    );

    // Page must be under the client's namespace, NOT under "dudu"
    assert!(
        tmp.path()
            .join("agents")
            .join(client_id)
            .join("wiki")
            .join("notes")
            .join("hello.md")
            .exists(),
        "wiki page must be written inside the client's namespace"
    );
    assert!(
        !tmp.path()
            .join("agents")
            .join("dudu")
            .join("wiki")
            .join("notes")
            .join("hello.md")
            .exists(),
        "wiki page must NOT be written to the default internal agent's wiki"
    );
}

/// W2-B changed the semantics this test locks in. The fence is now the
/// Wiki root being written, and a writer waits out a short delivery read
/// instead of failing on it: another agent's lease never blocks the
/// write, and the owning agent's lease only delays it.
#[tokio::test(flavor = "current_thread")]
async fn wiki_mcp_write_waits_out_its_own_lease_and_ignores_another_agents() {
    let tmp = TempDir::new();
    let agent = "alice";
    create_agent_dir(tmp.path(), agent);
    create_agent_dir(tmp.path(), "bob");
    let args = serde_json::json!({
        "page_path": "notes/hello.md",
        "content": "---\ntitle: Hello\ntrust: 0.8\n---\ninitial body",
    });
    assert!(
        !handle_wiki_write(&args, tmp.path(), agent).await["isError"]
            .as_bool()
            .unwrap_or(false)
    );
    let disk = tmp.path().join("agents/alice/wiki/notes/hello.md");

    // Another agent's delivery window is a different fence entirely.
    let unrelated = duduclaw_memory::WikiDeliveryFence::for_agent_wiki(tmp.path(), "bob")
        .try_shared()
        .unwrap();
    let replacement = serde_json::json!({
        "page_path": "notes/hello.md",
        "content": "---\ntitle: Hello\ntrust: 0.8\n---\nreplacement body",
    });
    let written = handle_wiki_write(&replacement, tmp.path(), agent).await;
    assert!(!written["isError"].as_bool().unwrap_or(false));
    assert!(
        std::fs::read_to_string(&disk)
            .unwrap()
            .contains("replacement body")
    );
    drop(unrelated);

    // The owning agent's own read window delays the write, then releases.
    let lease = duduclaw_memory::WikiDeliveryFence::for_agent_wiki(tmp.path(), agent)
        .try_shared()
        .unwrap();
    let holder = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        drop(lease);
    });
    let third = serde_json::json!({
        "page_path": "notes/hello.md",
        "content": "---\ntitle: Hello\ntrust: 0.8\n---\nthird body",
    });
    let waited = handle_wiki_write(&third, tmp.path(), agent).await;
    holder.join().unwrap();
    assert!(!waited["isError"].as_bool().unwrap_or(false));
    assert!(
        std::fs::read_to_string(&disk)
            .unwrap()
            .contains("third body")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn wiki_mcp_read_withholds_live_quarantine() {
    let tmp = TempDir::new();
    let agent = "alice";
    create_agent_dir(tmp.path(), agent);
    let args = serde_json::json!({
        "page_path": "notes/hello.md",
        "content": "---\ntitle: Hello\ncreated: 2026-01-01\nupdated: 2026-01-01\ntrust: 0.8\n---\nprivate body",
    });
    assert!(
        !handle_wiki_write(&args, tmp.path(), agent).await["isError"]
            .as_bool()
            .unwrap_or(false)
    );
    let read_args = serde_json::json!({"page_path": "notes/hello.md"});
    let before = handle_wiki_read(&read_args, tmp.path(), agent).await;
    assert!(before.to_string().contains("private body"));
    let trust =
        duduclaw_memory::WikiTrustStore::open(tmp.path().join("wiki_trust.db")).unwrap();
    trust
        .manual_set(
            "notes/hello.md",
            agent,
            0.05,
            false,
            Some(true),
            Some("quarantine"),
        )
        .unwrap();
    let after = handle_wiki_read(&read_args, tmp.path(), agent).await;
    assert_eq!(after["isError"], true);
    assert!(!after.to_string().contains("private body"));
}

// ── Test 5: external client's agent_id ignored, namespace resolver used ───
// Verifies the full contract: even if an args map originally contained
// agent_id (before dispatcher strips it), the namespace resolver wins.
#[tokio::test(flavor = "current_thread")]
async fn external_client_agent_id_stripped_uses_namespace() {
    let tmp = TempDir::new();
    let client_id = "trusted-bot";
    let ctx = external_ns(client_id);
    let wiki_agent = wiki_agent_from_ns(&ctx, "dudu");

    create_agent_dir(tmp.path(), client_id);

    // After dispatcher strips agent_id, only page_path + content remain
    let stripped_args = serde_json::json!({
        "page_path": "secure/record.md",
        "content": "---\ntitle: Record\ncreated: 2026-04-29T00:00:00Z\nupdated: 2026-04-29T00:00:00Z\ntags: [secure]\nlayer: context\ntrust: 0.8\n---\nData.",
    });

    let result = handle_wiki_write(&stripped_args, tmp.path(), wiki_agent).await;
    assert!(
        !result["isError"].as_bool().unwrap_or(false),
        "wiki_write after agent_id strip should succeed: {:?}",
        result
    );

    let expected = tmp
        .path()
        .join("agents")
        .join(client_id)
        .join("wiki")
        .join("secure")
        .join("record.md");
    assert!(
        expected.exists(),
        "page must be in client's namespace: {:?}",
        expected
    );
}

// ── Catalog completeness guard ──────────────────────────────────────
// Complements the scope-drift test in `mcp_auth`: every tool this server
// ADVERTISES (the static `TOOLS` table behind tools/list) must appear in
// `duduclaw_core::tool_catalog::builtin_tool_catalog`, so the dashboard's
// capability editor (feature switches + picker) can always see/deny it.
// Adding a tool without a catalog entry fails this test.
#[test]
fn tool_catalog_covers_all_advertised_tools() {
    let catalog: std::collections::HashSet<String> =
        duduclaw_core::tool_catalog::builtin_tool_catalog()
            .iter()
            .map(|e| e.name.to_string())
            .collect();
    for tool in super::tools() {
        assert!(
            catalog.contains(tool.name),
            "advertised tool `{}` is missing from builtin_tool_catalog — add it \
                 (with the gate-enforced scope from mcp_auth::tool_requires_scope) to \
                 duduclaw-core/src/tool_catalog.rs",
            tool.name
        );
    }
}

// ── TC-INT-外部工具過濾: external tools/list returns exactly 7 tools ────────
#[tokio::test(flavor = "current_thread")]
async fn external_tools_list_returns_exactly_7_tools() {
    use serde_json::json;
    let id = json!(1);

    // External principal → should see exactly 7 whitelisted tools
    let response = super::handle_tools_list(
        &id,
        &super::test_principal(true),
        tmp_home_for_tools_list().path(),
    ).await;
    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools must be array");
    assert_eq!(
        tools.len(),
        7,
        "External principal must see exactly 7 tools, got {}: {:?}",
        tools.len(),
        tools
            .iter()
            .map(|t| t["name"].as_str().unwrap_or("?"))
            .collect::<Vec<_>>()
    );

    let names: Vec<&str> = tools
        .iter()
        .map(|t| t["name"].as_str().unwrap_or(""))
        .collect();
    for expected in &[
        "memory_search",
        "memory_store",
        "memory_read",
        "wiki_read",
        "wiki_write",
        "wiki_search",
        "send_message",
    ] {
        assert!(
            names.contains(expected),
            "External tool list must contain '{}'; got: {:?}",
            expected,
            names
        );
    }
}

// ── TC-INT-內部工具完整: internal tools/list returns full tool list ─────────
#[tokio::test(flavor = "current_thread")]
async fn internal_tools_list_returns_full_list() {
    use serde_json::json;
    let id = json!(1);

    // Internal principal → should see all tools (more than 7)
    let response = super::handle_tools_list(
        &id,
        &super::test_principal(false),
        tmp_home_for_tools_list().path(),
    ).await;
    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools must be array");
    assert!(
        tools.len() > 7,
        "Internal principal must see more than 7 tools, got {}",
        tools.len()
    );
}

// ── H8: GitHub tools follow the Google gate's deny-by-default rule ──────
#[tokio::test(flavor = "current_thread")]
async fn github_tools_are_hidden_until_the_integration_is_enabled() {
    use serde_json::json;
    let home = tmp_home_for_tools_list();

    // Fresh home, no `[integrations]` section ⇒ fail closed. Same
    // expectation the Google group has carried since v1.48.
    let names = names_of(&super::handle_tools_list(
        &json!(1),
        &super::test_principal(false),
        home.path(),
    ).await);
    for t in super::GITHUB_WORKSPACE_TOOLS {
        assert!(
            !names.contains(&t.to_string()),
            "{t} must not be listed while [integrations] github is off"
        );
    }
    assert!(
        !names.contains(&"gmail_search".to_string()),
        "google group stays gated too (guards against a copy-paste that widened both)"
    );

    std::fs::write(
        home.path().join("config.toml"),
        "[integrations]\ngithub = true\n",
    )
    .unwrap();
    let names = names_of(&super::handle_tools_list(
        &json!(1),
        &super::test_principal(false),
        home.path(),
    ).await);
    for t in super::GITHUB_WORKSPACE_TOOLS {
        assert!(names.contains(&t.to_string()), "{t} must appear once enabled");
    }
    assert!(
        !names.contains(&"gmail_search".to_string()),
        "enabling GitHub must not enable Google"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn internal_tools_list_hides_denied_tool() {
    use serde_json::json;
    let home = tmp_home_for_tools_list();
    let agent_dir = home.path().join("agents").join("worker");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("agent.toml"),
        "[capabilities]\ndenied_tools = [\"memory_store\"]\n",
    )
    .unwrap();

    let resp =
        super::handle_tools_list(&json!(1), &internal_principal_named("worker"), home.path()).await;
    let names = names_of(&resp);
    assert!(
        !names.contains(&"memory_store".to_string()),
        "denied tool must be hidden"
    );
    assert!(
        names.contains(&"memory_search".to_string()),
        "non-denied tools stay"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn internal_tools_list_allowlist_shows_only_allowed() {
    use serde_json::json;
    let home = tmp_home_for_tools_list();
    let agent_dir = home.path().join("agents").join("readonly");
    std::fs::create_dir_all(&agent_dir).unwrap();
    // Allowlist mode: only memory_search is callable → only it is advertised.
    // A qualified `mcp__duduclaw__` entry must match its bare tool too.
    std::fs::write(
        agent_dir.join("agent.toml"),
        "[capabilities]\nallowed_tools = [\"memory_search\", \"mcp__duduclaw__wiki_read\"]\n",
    )
    .unwrap();

    let resp = super::handle_tools_list(
        &json!(1),
        &internal_principal_named("readonly"),
        home.path(),
    ).await;
    let names = names_of(&resp);
    assert!(names.contains(&"memory_search".to_string()));
    assert!(
        names.contains(&"wiki_read".to_string()),
        "qualified allowlist entry matches bare tool"
    );
    assert!(
        !names.contains(&"memory_store".to_string()),
        "unlisted tool hidden in allowlist mode"
    );
    assert!(
        !names.contains(&"tasks_create".to_string()),
        "unlisted tool hidden in allowlist mode"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn role_member_tools_list_uses_ephemeral_capabilities() {
    use serde_json::json;
    let home = tmp_home_for_tools_list();
    let member = "eph-planner-123456";
    let dir = home.path().join("agents/.ephemeral").join(member);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agent.toml"),
        "[capabilities]\nallowed_tools = [\"team_handoff\"]\n",
    )
    .unwrap();
    let principal =
        internal_principal_named(duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID);
    let listed = super::handle_tools_list_for_agent(&json!(1), &principal, home.path(), member).await;
    assert_eq!(names_of(&listed), vec!["team_handoff"]);

    // Syntactically valid TOML with a wrong capability type must match
    // the dispatch gate's stricter policy-only parse and expose nothing.
    std::fs::write(
        dir.join("agent.toml"),
        "[capabilities]\nos_native = \"invalid-boolean\"\n",
    )
    .unwrap();
    let malformed =
        super::handle_tools_list_for_agent(&json!(3), &principal, home.path(), member).await;
    assert!(names_of(&malformed).is_empty());

    std::fs::remove_file(dir.join("agent.toml")).unwrap();
    let missing =
        super::handle_tools_list_for_agent(&json!(2), &principal, home.path(), member).await;
    assert!(names_of(&missing).is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn internal_tools_list_no_agent_toml_is_unrestricted() {
    use serde_json::json;
    // No agent.toml for this caller → empty gate → full list (matches the
    // dispatch gate's own fail-safe-to-empty posture).
    let resp = super::handle_tools_list(
        &json!(1),
        &internal_principal_named("ghost"),
        tmp_home_for_tools_list().path(),
    ).await;
    assert!(names_of(&resp).len() > 7);
}

// ── Test 7: wiki_write succeeds for external client WITHOUT pre-created dir ──
// BUG-QA-003: Reproduces the exact failure scenario — external MCP client
// (e.g. claude-desktop) connects for the first time with NO agent directory.
// Before the fix: resolve_wiki_dir returned "Agent does not exist".
// After the fix: agent dir is auto-created and wiki_write succeeds.
#[tokio::test(flavor = "current_thread")]
async fn wiki_write_external_client_auto_creates_dir_on_first_connect() {
    let tmp = TempDir::new();
    let client_id = "claude-desktop";
    let ctx = external_ns(client_id);
    let wiki_agent = wiki_agent_from_ns(&ctx, "dudu");

    // Deliberately do NOT call create_agent_dir — this is the BUG-QA-003 scenario
    assert!(
        !tmp.path().join("agents").join(client_id).exists(),
        "pre-condition: agent dir must NOT exist before first connect"
    );

    let args = serde_json::json!({
        "page_path": "notes/first-page.md",
        "content": "---\ntitle: First Page\ncreated: 2026-04-29T00:00:00Z\nupdated: 2026-04-29T00:00:00Z\ntags: [test]\nlayer: context\ntrust: 0.5\n---\nAuto-created on first connect.",
    });

    let result = handle_wiki_write(&args, tmp.path(), wiki_agent).await;
    assert!(
        !result["isError"].as_bool().unwrap_or(false),
        "wiki_write must succeed for external client on first connect (BUG-QA-003): {:?}",
        result
    );

    // Agent dir and wiki page must now exist
    assert!(
        tmp.path().join("agents").join(client_id).exists(),
        "agent dir must be auto-created after first wiki_write"
    );
    assert!(
        tmp.path()
            .join("agents")
            .join(client_id)
            .join("wiki")
            .join("notes")
            .join("first-page.md")
            .exists(),
        "wiki page must be written after auto-create"
    );
}

// ── Test 8: resolve_wiki_dir auto-creates agent dir ──────────────────────
#[test]
fn resolve_wiki_dir_auto_creates_missing_agent_dir() {
    let tmp = TempDir::new();
    let home = tmp.path();
    let agent_id = "new-external-bot";

    // Pre-condition: directory does not exist
    assert!(!home.join("agents").join(agent_id).exists());

    let result = resolve_wiki_dir(home, agent_id);
    assert!(
        result.is_ok(),
        "resolve_wiki_dir must succeed even when agent dir is absent: {:?}",
        result
    );

    let wiki_path = result.unwrap();
    assert_eq!(wiki_path, home.join("agents").join(agent_id).join("wiki"));
    assert!(
        home.join("agents").join(agent_id).exists(),
        "agent dir must be created by resolve_wiki_dir"
    );
}

// ── Test 9: resolve_wiki_dir leaves existing dir untouched ───────────────
#[test]
fn resolve_wiki_dir_existing_dir_unchanged() {
    let tmp = TempDir::new();
    let home = tmp.path();
    let agent_id = "existing-agent";

    // Pre-condition: agent dir already exists with a marker file
    let agent_dir = home.join("agents").join(agent_id);
    fs::create_dir_all(&agent_dir).unwrap();
    fs::write(
        agent_dir.join("agent.toml"),
        "[agent]\nname = \"existing-agent\"\n",
    )
    .unwrap();

    let result = resolve_wiki_dir(home, agent_id);
    assert!(
        result.is_ok(),
        "resolve_wiki_dir must succeed for existing dir: {:?}",
        result
    );
    assert_eq!(result.unwrap(), agent_dir.join("wiki"));

    // Marker file must still be present (existing contents untouched)
    assert!(
        agent_dir.join("agent.toml").exists(),
        "existing agent.toml must not be removed"
    );
}

// ── Test 10: resolve_wiki_dir rejects invalid agent_id ──────────────────
#[test]
fn resolve_wiki_dir_invalid_agent_id_rejected() {
    let tmp = TempDir::new();
    // Vector 1: Path traversal attempt (relative path traversal: "../")
    assert!(
        resolve_wiki_dir(tmp.path(), "../evil").is_err(),
        "../evil must be rejected"
    );
    assert!(
        resolve_wiki_dir(tmp.path(), "../../etc/passwd").is_err(),
        "../../etc/passwd must be rejected"
    );
    // Vector 2: Absolute path injection ("/absolute")
    assert!(
        resolve_wiki_dir(tmp.path(), "/absolute").is_err(),
        "/absolute must be rejected"
    );
    assert!(
        resolve_wiki_dir(tmp.path(), "/etc/passwd").is_err(),
        "/etc/passwd must be rejected"
    );
    // Vector 3: Empty string
    assert!(
        resolve_wiki_dir(tmp.path(), "").is_err(),
        "empty string must be rejected"
    );
    // Additional: Uppercase not allowed
    assert!(
        resolve_wiki_dir(tmp.path(), "Agent-Name").is_err(),
        "uppercase must be rejected"
    );
    // Additional: Null byte injection
    assert!(
        resolve_wiki_dir(tmp.path(), "agent\0id").is_err(),
        "null byte must be rejected"
    );
}

// ── Test: audit_trail_query requires Admin scope (H1 fix / OWASP A01) ──────
/// Verify that `handle_audit_trail_query` rejects callers who lack Admin scope,
/// providing a defence-in-depth guard independent of the dispatch-layer scope check.
#[tokio::test(flavor = "current_thread")]
async fn audit_trail_query_denied_without_admin_scope() {
    let tmp = TempDir::new();
    let result = handle_audit_trail_query(
        &serde_json::json!({}),
        tmp.path(),
        "non-admin-client",
        false, // caller_is_admin = false
    )
    .await;
    assert_eq!(
        result["isError"],
        serde_json::json!(true),
        "non-admin call must be rejected with isError=true"
    );
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("Admin scope"),
        "error message must reference the required scope; got: {text}"
    );
}
