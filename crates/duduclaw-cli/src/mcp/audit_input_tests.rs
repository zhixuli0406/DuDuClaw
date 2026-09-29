use super::*;
use std::fs;

struct TempDir(std::path::PathBuf);
impl TempDir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("duduclaw-aud-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn state_changing_call_captures_masked_input_in_audit_trail() {
    // HIGH-C: the tools/call dispatch site now records the tool's INPUT
    // arguments (masked) via append_tool_call_with_input — previously that
    // fn had zero production callers.
    let tmp = TempDir::new();
    let memory = SqliteMemoryEngine::new(&tmp.path().join("memory.db")).expect("memory engine");
    let odoo: OdooState = std::sync::Arc::new(crate::odoo_pool::OdooConnectorPool::default());
    let ns = crate::mcp_namespace::NamespaceContext {
        write_namespace: "internal/agnes".to_string(),
        read_namespaces: vec!["internal/agnes".to_string(), "shared/public".to_string()],
    };
    let quota = crate::mcp_memory_quota::DailyQuota::new();
    let http = reqwest::Client::new();

    // `pairing_manage` is state-changing (2026-07 allowlist addition);
    // smuggle a secret-shaped value through the args to prove masking.
    let params = serde_json::json!({
        "name": "pairing_manage",
        "arguments": {
            "action": "list",
            "api_key": "sk-ant-api03-super-secret-value",
        }
    });
    let _ = handle_tools_call(
        &serde_json::json!(1),
        &params,
        tmp.path(),
        &http,
        &memory,
        "agnes",
        &odoo,
        &ns,
        &quota,
        "default",
        true,
    )
    .await;

    let body = fs::read_to_string(tmp.path().join("tool_calls.jsonl"))
        .expect("audit record must be written for a state-changing tool");
    let line = body
        .lines()
        .find(|l| l.contains("pairing_manage"))
        .expect("pairing_manage audit line");
    let rec: serde_json::Value = serde_json::from_str(line).expect("valid JSONL");
    let input = rec["input"].as_str().expect("input must be captured");
    assert!(
        input.contains("action"),
        "input must carry the real args: {input}"
    );
    // Masked secret: neither the JSON line nor the captured input may
    // contain the raw secret value.
    assert!(
        !line.contains("super-secret-value"),
        "masked secret leaked into the audit line: {line}"
    );
}

/// B3b end-to-end: drives the REAL `handle_tools_call` dispatch (not a
/// hand-written JSONL fixture) for a state-changing tool, confirms the
/// audit row now carries `result_text`, then feeds that row into the
/// shared `duduclaw-core` grounding primitive — the same one
/// `dispatch_engine::grounding_precheck` wraps — to prove the B3
/// grounding pre-check's evidence source is genuinely live end-to-end
/// (previously every row lacked `result_text`, so the gate could only
/// ever observe `ResultTextMissing` and stay inert).
#[tokio::test(flavor = "current_thread")]
async fn state_changing_call_result_text_activates_grounding_evidence() {
    let tmp = TempDir::new();
    let memory = SqliteMemoryEngine::new(&tmp.path().join("memory.db")).expect("memory engine");
    let odoo: OdooState = std::sync::Arc::new(crate::odoo_pool::OdooConnectorPool::default());
    let ns = crate::mcp_namespace::NamespaceContext {
        write_namespace: "internal/agnes".to_string(),
        read_namespaces: vec!["internal/agnes".to_string(), "shared/public".to_string()],
    };
    let quota = crate::mcp_memory_quota::DailyQuota::new();
    let http = reqwest::Client::new();

    // `pairing_manage action=list` on a fresh (empty) access_control.json
    // deterministically returns "目前沒有已核准的 subject。" — no external
    // dependency, no other pairing_manage tool_calls.jsonl row from a
    // handler-internal audit call to disambiguate against.
    let params = serde_json::json!({
        "name": "pairing_manage",
        "arguments": { "action": "list" }
    });
    let resp = handle_tools_call(
        &serde_json::json!(1),
        &params,
        tmp.path(),
        &http,
        &memory,
        "agnes",
        &odoo,
        &ns,
        &quota,
        "default",
        true,
    )
    .await;
    let resp_text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        resp_text.contains("目前沒有已核准的 subject"),
        "unexpected tool response: {resp_text}"
    );

    let body = fs::read_to_string(tmp.path().join("tool_calls.jsonl"))
        .expect("audit record must be written for a state-changing tool");
    let line = body
        .lines()
        .find(|l| l.contains("pairing_manage"))
        .expect("pairing_manage audit line");
    let rec: serde_json::Value = serde_json::from_str(line).expect("valid JSONL");

    let result_text = rec["result_text"]
        .as_str()
        .expect("result_text must be captured by the B3b writer path");
    assert!(
        result_text.contains("目前沒有已核准的 subject"),
        "result_text must carry the tool's actual output: {result_text}"
    );

    // Feed the persisted row straight into the shared grounding
    // primitive — no dispatch_engine.rs edits, no hand-written fixture.
    let evidence = vec![duduclaw_core::grounding::ToolEvidence {
        tool_name: rec["tool_name"].as_str().unwrap().to_string(),
        result_text: rec["result_text"].as_str().map(String::from),
        input_text: rec["input"].as_str().map(String::from),
        is_error: !rec["success"].as_bool().unwrap(),
    }];

    let grounded = duduclaw_core::grounding::check_grounded(
        "目前沒有已核准的 subject。",
        &evidence,
        Some("pairing_manage"),
        8,
    );
    assert_eq!(
        grounded,
        duduclaw_core::grounding::GroundingOutcome::Grounded {
            tool_name: "pairing_manage".to_string()
        },
        "grounding gate must transition past ResultTextMissing once result_text is captured"
    );

    let not_grounded = duduclaw_core::grounding::check_grounded(
        "已成功核准使用者 alice。",
        &evidence,
        Some("pairing_manage"),
        8,
    );
    assert_eq!(
        not_grounded,
        duduclaw_core::grounding::GroundingOutcome::NotGrounded,
        "an unsupported claim must still be rejectable, not just skipped"
    );
}

/// Fix-2 C1a regression: `tasks_complete` (and the rest of
/// `SELF_ECHO_TOOL_NAMES`) returns a response envelope whose
/// `result_summary` IS the caller's own `summary` argument. Before the
/// fix, the audit call site captured that as `result_text`, so an
/// agent's claim could be trivially "grounded" against its own words.
/// Drives the REAL dispatch path end to end (tasks_create → tasks_complete)
/// and asserts the persisted `tasks_complete` row carries NO
/// `result_text` at all — never Grounded, never (falsely) NotGrounded,
/// simply not evidence.
#[tokio::test(flavor = "current_thread")]
async fn tasks_complete_self_echo_never_captured_as_grounding_evidence() {
    let tmp = TempDir::new();
    let memory = SqliteMemoryEngine::new(&tmp.path().join("memory.db")).expect("memory engine");
    let odoo: OdooState = std::sync::Arc::new(crate::odoo_pool::OdooConnectorPool::default());
    let ns = crate::mcp_namespace::NamespaceContext {
        write_namespace: "internal/agnes".to_string(),
        read_namespaces: vec!["internal/agnes".to_string(), "shared/public".to_string()],
    };
    let quota = crate::mcp_memory_quota::DailyQuota::new();
    let http = reqwest::Client::new();

    let create_resp = handle_tools_call(
        &serde_json::json!(1),
        &serde_json::json!({
            "name": "tasks_create",
            "arguments": { "title": "退款作業", "assigned_to": "agnes" }
        }),
        tmp.path(),
        &http,
        &memory,
        "agnes",
        &odoo,
        &ns,
        &quota,
        "default",
        true,
    )
    .await;
    let create_text = create_resp["result"]["content"][0]["text"]
        .as_str()
        .expect("tasks_create must return text content");
    let created: serde_json::Value =
        serde_json::from_str(create_text).expect("tasks_create result must be JSON");
    let task_id = created["task"]["id"].as_str().expect("task id").to_string();

    // The self-reported summary the agent would want "grounded" — an
    // unsupported claim that must never pass just because the agent
    // said it in the completion call itself.
    let echoed_claim = "已成功處理退款 #9999，款項已退回原付款方式。";
    let _ = handle_tools_call(
        &serde_json::json!(2),
        &serde_json::json!({
            "name": "tasks_complete",
            "arguments": { "task_id": task_id, "summary": echoed_claim }
        }),
        tmp.path(),
        &http,
        &memory,
        "agnes",
        &odoo,
        &ns,
        &quota,
        "default",
        true,
    )
    .await;

    let body = fs::read_to_string(tmp.path().join("tool_calls.jsonl"))
        .expect("audit record must be written for a state-changing tool");
    let line = body
        .lines()
        .find(|l| l.contains("tasks_complete"))
        .expect("tasks_complete audit line");
    let rec: serde_json::Value = serde_json::from_str(line).expect("valid JSONL");
    assert!(
        rec.get("result_text").is_none(),
        "tasks_complete must never capture result_text (self-echo deny-list): {line}"
    );
    // Still gets the ordinary input + success audit fields — only the
    // grounding-evidence field is suppressed.
    assert!(rec["input"].as_str().is_some_and(|s| s.contains("task_id")));
    assert_eq!(rec["success"], serde_json::json!(true));

    // Even if some other caller fed the echoed claim itself into
    // `check_grounded` as evidence (hypothetically re-deriving the old
    // buggy behavior), no evidence exists to reason over — never a
    // false Grounded.
    let evidence: Vec<duduclaw_core::grounding::ToolEvidence> = body
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|r| r["tool_name"].as_str() == Some("tasks_complete"))
        .map(|r| duduclaw_core::grounding::ToolEvidence {
            tool_name: r["tool_name"].as_str().unwrap().to_string(),
            result_text: r["result_text"].as_str().map(String::from),
            input_text: r["input"].as_str().map(String::from),
            is_error: !r["success"].as_bool().unwrap(),
        })
        .collect();
    let outcome = duduclaw_core::grounding::check_grounded(echoed_claim, &evidence, None, 8);
    assert_eq!(
        outcome,
        duduclaw_core::grounding::GroundingOutcome::ResultTextMissing,
        "no result_text captured ⇒ ResultTextMissing (fail-open skip), never a false Grounded"
    );
}

// ── Live Canvas tools (G15) ──────────────────────────────

/// Push → get roundtrip through the real tool handlers: hostile markup is
/// stripped at write time, benign markup and CJK content survive, and the
/// stored row is what a later reader (gateway `canvas.get`) sees.
#[tokio::test]
async fn canvas_push_get_roundtrip_sanitizes() {
    let tmp = TempDir::new();
    let args = serde_json::json!({
        "html": "<h1>週報</h1><script>alert(1)</script><img src=\"https://ok.example/c.png\" onerror=\"x()\"><p>營收 100</p>",
        "title": "本週儀表板",
    });
    let result = handle_canvas_push(&args, tmp.path(), "agnes").await;
    let text = result["content"][0]["text"].as_str().unwrap_or_default();
    assert!(
        result.get("isError").is_none() || result["isError"] == serde_json::json!(false),
        "push must succeed: {result}"
    );
    assert!(text.contains("Canvas updated"), "got: {text}");

    let store = duduclaw_gateway::canvas::CanvasStore::open(tmp.path()).unwrap();
    let row = store
        .current("agnes")
        .await
        .unwrap()
        .expect("stored canvas");
    assert_eq!(row.title, "本週儀表板");
    assert!(
        !row.html.contains("script") && !row.html.contains("onerror"),
        "stored: {}",
        row.html
    );
    assert!(row.html.contains("<h1>週報</h1>") && row.html.contains("營收 100"));
    assert!(row.html.contains("https://ok.example/c.png"));
}

/// Oversize pushes are rejected fail-closed (nothing stored) with a clear
/// error, and `canvas_clear` appends the empty tombstone version.
#[tokio::test]
async fn canvas_push_size_cap_and_clear() {
    let tmp = TempDir::new();
    let big = format!(
        "<p>{}</p>",
        "x".repeat(duduclaw_gateway::canvas::MAX_CANVAS_BYTES + 1)
    );
    let result =
        handle_canvas_push(&serde_json::json!({ "html": big }), tmp.path(), "agnes").await;
    let text = result["content"][0]["text"].as_str().unwrap_or_default();
    assert!(text.contains("too large"), "got: {text}");
    let store = duduclaw_gateway::canvas::CanvasStore::open(tmp.path()).unwrap();
    assert!(
        store.current("agnes").await.unwrap().is_none(),
        "fail-closed: nothing stored"
    );

    // Now a valid push followed by a clear.
    handle_canvas_push(
        &serde_json::json!({ "html": "<p>hi</p>" }),
        tmp.path(),
        "agnes",
    )
    .await;
    let result = handle_canvas_clear(tmp.path(), "agnes").await;
    let text = result["content"][0]["text"].as_str().unwrap_or_default();
    assert!(text.contains("Canvas cleared"), "got: {text}");
    let cur = store.current("agnes").await.unwrap().expect("tombstone");
    assert_eq!(cur.html, "");
}

// ── Internal-key agent attribution (1.64.0 regression) ────────────────
//
// Every gateway-spawned agent authenticates with the ONE shared internal
// MCP key, so `caller_client_id` is "gateway-internal" for all of them and
// the acting agent only arrives as `default_agent`. Handlers that treat
// the client_id as an agent directory name therefore denied everything.

#[test]
fn acting_agent_id_maps_only_internal_and_empty() {
    // Internal key and the legacy empty stdio client_id → acting agent.
    assert_eq!(
        acting_agent_id(
            duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID,
            "main"
        ),
        "main"
    );
    assert_eq!(acting_agent_id("", "main"), "main");
    // Everything else is returned verbatim — an external client must never
    // inherit the process's default agent.
    assert_eq!(acting_agent_id("some-external", "main"), "some-external");
    // Exact equality, never a prefix/substring match.
    assert_eq!(
        acting_agent_id("gateway-internal-evil", "main"),
        "gateway-internal-evil"
    );
    assert_eq!(acting_agent_id("gateway", "main"), "gateway");
}

/// End-to-end through the REAL `handle_tools_call` dispatch: the grant
/// `agent_update` writes to `agents/main/agent.toml` must be the grant
/// `db_sources` reads back when the caller is the internal key.
#[tokio::test(flavor = "current_thread")]
async fn db_tools_resolve_the_internal_key_to_the_default_agent() {
    let tmp = TempDir::new();
    let home = tmp.path();
    fs::write(
        home.join("config.toml"),
        "[db_sources.crm]\nlabel = \"客戶 CRM\"\ndriver = \"sqlite\"\n\
             url = \"/tmp/duduclaw-test-crm.sqlite\"\nallowed_tables = [\"customers\"]\n",
    )
    .unwrap();
    let agent_dir = home.join("agents").join("main");
    fs::create_dir_all(&agent_dir).unwrap();
    fs::write(
        agent_dir.join("agent.toml"),
        "[capabilities]\ndb_sources = [\"crm\"]\n",
    )
    .unwrap();

    let memory = SqliteMemoryEngine::new(&home.join("memory.db")).expect("memory engine");
    let odoo: OdooState = std::sync::Arc::new(crate::odoo_pool::OdooConnectorPool::default());
    let ns = crate::mcp_namespace::NamespaceContext {
        write_namespace: "internal/main".to_string(),
        read_namespaces: vec!["internal/main".to_string()],
    };
    let quota = crate::mcp_memory_quota::DailyQuota::new();
    let http = reqwest::Client::new();
    let params = serde_json::json!({ "name": "db_sources", "arguments": {} });

    let call = async |client_id: &str| {
        handle_tools_call(
            &serde_json::json!(1),
            &params,
            home,
            &http,
            &memory,
            "main",
            &odoo,
            &ns,
            &quota,
            client_id,
            true,
        )
        .await
    };

    // The internal key acts as `main`, so `main`'s grant is honoured.
    let res = call(duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID).await;
    let text = res["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(text.contains("crm"), "internal key must act as main: {res}");
    assert!(
        !text.contains("沒有任何資料庫來源授權"),
        "the 1.64.0 regression: {res}"
    );

    // An external client id does NOT inherit the default agent.
    let res = call("some-external").await;
    let text = res["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("沒有任何資料庫來源授權"),
        "an external client must not inherit main's grants: {res}"
    );
}

/// Caller identity is validated before any I/O (path-traversal guard).
#[tokio::test]
async fn canvas_tools_reject_invalid_agent_id() {
    let tmp = TempDir::new();
    let result = handle_canvas_push(
        &serde_json::json!({ "html": "<p>x</p>" }),
        tmp.path(),
        "../evil",
    )
    .await;
    let text = result["content"][0]["text"].as_str().unwrap_or_default();
    assert!(text.contains("Invalid agent ID"), "got: {text}");
    let result = handle_canvas_clear(tmp.path(), "../evil").await;
    let text = result["content"][0]["text"].as_str().unwrap_or_default();
    assert!(text.contains("Invalid agent ID"), "got: {text}");
}
