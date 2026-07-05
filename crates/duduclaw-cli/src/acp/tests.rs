//! Tests for the ACP module — Agent Card generation and A2A task lifecycle.

#[test]
fn test_agent_card_v1_0_required_fields() {
    use super::server::AcpServer;

    let card = AcpServer::default_agent_card();
    let json = serde_json::to_value(&card).unwrap();

    // A2A v1.0 required top-level fields (camelCase on the wire).
    assert_eq!(json["protocolVersion"], "1.0");
    assert!(json.get("name").is_some());
    assert!(json.get("description").is_some());
    assert!(json.get("url").is_some());
    assert!(json.get("version").is_some());
    assert!(json.get("capabilities").is_some());
    assert!(json.get("defaultInputModes").is_some());
    assert!(json.get("defaultOutputModes").is_some());
    assert!(json.get("skills").is_some());
    assert!(json.get("provider").is_some());

    // v1.0 capabilities shape. Streaming/push are honestly false: the server
    // answers message/stream + push-notification config with -32004.
    let caps = &json["capabilities"];
    assert!(!caps["streaming"].as_bool().unwrap());
    assert!(!caps["pushNotifications"].as_bool().unwrap());
    assert!(caps.get("stateTransitionHistory").is_some());

    // Provider identity.
    assert!(json["provider"]["organization"].as_str().is_some());
    assert!(json["provider"]["url"].as_str().is_some());

    // Skills are non-empty; each carries the v1.0 fields id/name/description/tags/examples.
    let skills = json["skills"].as_array().unwrap();
    assert!(!skills.is_empty());
    for skill in skills {
        assert!(skill.get("id").is_some());
        assert!(skill.get("name").is_some());
        assert!(skill.get("description").is_some());
        assert!(skill.get("tags").is_some());
        assert!(skill.get("examples").is_some());
    }

    // ADR-002 x-duduclaw capability-negotiation extension preserved.
    let ext = &json["extensions"]["x-duduclaw"];
    assert_eq!(ext["adr"], "ADR-002");
    assert!(ext["version"].as_str().is_some());
    assert!(ext["capabilities"].as_str().is_some());
    assert_eq!(ext["negotiationHeader"], "x-duduclaw-capabilities");
}

#[test]
fn test_both_well_known_paths_resolve() {
    use super::server::{
        resolve_well_known_card, WELL_KNOWN_AGENT_CARD_PATH, WELL_KNOWN_AGENT_CARD_PATH_LEGACY,
    };

    // v1.0 path.
    assert_eq!(WELL_KNOWN_AGENT_CARD_PATH, "/.well-known/agent-card.json");
    let v1 = resolve_well_known_card(WELL_KNOWN_AGENT_CARD_PATH).expect("v1.0 path resolves");
    // Legacy alias resolves to the same card.
    assert_eq!(WELL_KNOWN_AGENT_CARD_PATH_LEGACY, "/.well-known/agent.json");
    let legacy =
        resolve_well_known_card(WELL_KNOWN_AGENT_CARD_PATH_LEGACY).expect("legacy path resolves");
    assert_eq!(
        serde_json::to_value(&v1).unwrap(),
        serde_json::to_value(&legacy).unwrap(),
        "both well-known paths must serve the same card"
    );

    // Any other path is a miss (HTTP layer would 404).
    assert!(resolve_well_known_card("/.well-known/other.json").is_none());
}

#[test]
fn test_streaming_variants_return_unsupported_operation() {
    for method in [
        "message/stream",
        "tasks/resubscribe",
        "tasks/pushNotificationConfig/set",
        "tasks/pushNotificationConfig/get",
    ] {
        let id = serde_json::json!(7);
        let response = super::server::handle_unsupported_operation(&id, method);
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 7);
        // A2A spec-shaped UnsupportedOperationError, not a bare method-not-found.
        assert_eq!(response["error"]["code"], -32004);
        assert!(response["error"]["message"].as_str().unwrap().contains(method));
        assert!(response.get("result").is_none());
    }
}

#[test]
fn test_a2a_task_manager_lifecycle() {
    use super::handlers::{A2ATaskManager, A2ATaskState};

    let mut mgr = A2ATaskManager::new();

    // Create a task
    let task = mgr.create_task("ctx_1", "Summarize document");
    let task_id = task.id.clone();
    assert_eq!(task.state, A2ATaskState::Working);
    assert_eq!(task.context_id, "ctx_1");
    assert_eq!(task.description, "Summarize document");

    // Get task
    assert!(mgr.get_task(&task_id).is_some());
    assert!(mgr.get_task("nonexistent").is_none());

    // Complete task
    assert!(mgr.complete_task(&task_id, "Summary done".to_string()));
    let completed = mgr.get_task(&task_id).unwrap();
    assert_eq!(completed.state, A2ATaskState::Completed);
    assert_eq!(completed.result.as_deref(), Some("Summary done"));

    // Cancel a new task
    let task2 = mgr.create_task("ctx_2", "Another task");
    let task2_id = task2.id.clone();
    assert!(mgr.cancel_task(&task2_id));
    let cancelled = mgr.get_task(&task2_id).unwrap();
    assert_eq!(cancelled.state, A2ATaskState::Canceled);

    // Cancel nonexistent returns false
    assert!(!mgr.cancel_task("nonexistent"));
}

#[tokio::test]
async fn test_tasks_send_via_handler() {
    use super::handlers::A2ATaskManager;

    let mut mgr = A2ATaskManager::new();
    let params = serde_json::json!({
        "message": "Hello, agent!",
        "context_id": "test_ctx",
    });
    let id = serde_json::json!(1);

    // RFC-25 Phase 3: handle_tasks_send is now async and executes the target
    // agent. With an empty home (no agents), execution fails gracefully and the
    // task still completes with the error text — so the envelope shape holds.
    let home = std::env::temp_dir().join("duduclaw-acp-test-empty-home");
    let response = super::server::handle_tasks_send(&id, &params, &mut mgr, &home).await;
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);

    let result = &response["result"];
    assert!(result.get("task").is_some());
    assert!(result.get("artifacts").is_some());

    let task = &result["task"];
    // Empty test home → no agents → execution fails → state "failed"; a real
    // home with agents → "completed". Accept both (RFC-25 Phase 3 audit fix).
    let state = task["state"].as_str().unwrap();
    assert!(state == "completed" || state == "failed", "unexpected state: {state}");
    assert!(task["result"].as_str().is_some());

    let artifacts = result["artifacts"].as_array().unwrap();
    assert!(!artifacts.is_empty());
    assert_eq!(artifacts[0]["type"], "text");
}

#[tokio::test]
async fn test_tasks_send_missing_message() {
    use super::handlers::A2ATaskManager;

    let mut mgr = A2ATaskManager::new();
    let params = serde_json::json!({});
    let id = serde_json::json!(2);

    let home = std::env::temp_dir().join("duduclaw-acp-test-empty-home");
    let response = super::server::handle_tasks_send(&id, &params, &mut mgr, &home).await;
    assert!(response.get("error").is_some());
    assert_eq!(response["error"]["code"], -32602);
}

#[tokio::test]
async fn test_tasks_get_via_handler() {
    use super::handlers::A2ATaskManager;
    use super::message_send::BusTaskIndex;

    let mgr = A2ATaskManager::new();
    let bus_index = BusTaskIndex::default();
    let params = serde_json::json!({ "task_id": "nonexistent" });
    let id = serde_json::json!(3);

    let home = std::env::temp_dir().join("duduclaw-acp-test-empty-home");
    let response = super::server::handle_tasks_get(&id, &params, &mgr, &bus_index, &home).await;
    assert!(response.get("error").is_some());
    assert_eq!(response["error"]["code"], -32001);
}

#[test]
fn test_agent_discover() {
    let id = serde_json::json!(4);
    let response = super::server::handle_agent_discover(&id);

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 4);

    let result = &response["result"];
    assert_eq!(result["protocolVersion"], "1.0");
    assert_eq!(result["name"], "DuDuClaw Agent");
    assert!(result.get("capabilities").is_some());
    assert!(result.get("skills").is_some());
    // Card stays honest: streaming is unsupported (-32004 on message/stream).
    assert!(!result["capabilities"]["streaming"].as_bool().unwrap());
}

// ═════════════════════════════════════════════════════════════
// A2A v1.0 message/send (bus-backed) tests
// ═════════════════════════════════════════════════════════════

mod message_send_tests {
    use crate::acp::message_send::{
        append_bus_task_sync, build_bus_task_json, enqueue_and_respond, parse_message_send_params,
        probe_bus_task_state, BusProbe, BusTaskIndex, ParsedSendMessage, SendParamError,
        MAX_MESSAGE_TEXT_BYTES,
    };

    fn text_part(text: &str) -> serde_json::Value {
        serde_json::json!({ "kind": "text", "text": text })
    }

    fn send_params(parts: Vec<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({ "message": { "role": "user", "parts": parts } })
    }

    // ── Params parsing ──────────────────────────────────────

    #[test]
    fn parse_valid_single_text_part() {
        let parsed = parse_message_send_params(&send_params(vec![text_part("hello agent")]))
            .expect("valid params parse");
        assert_eq!(parsed.text, "hello agent");
        assert_eq!(parsed.skipped_parts, 0);
        assert_eq!(parsed.context_id, None);
        assert!(!parsed.blocking_requested);
    }

    #[test]
    fn parse_multi_part_concatenates_and_skips_non_text() {
        let params = serde_json::json!({
            "message": {
                "role": "user",
                "messageId": "client-msg-1",
                "contextId": "agent-b",
                "parts": [
                    text_part("first"),
                    { "kind": "file", "file": { "uri": "https://example.com/x.png" } },
                    text_part("second"),
                ],
            },
            "configuration": { "blocking": true },
        });
        let parsed = parse_message_send_params(&params).expect("parses");
        assert_eq!(parsed.text, "first\nsecond");
        assert_eq!(parsed.skipped_parts, 1);
        assert_eq!(parsed.context_id.as_deref(), Some("agent-b"));
        assert_eq!(parsed.client_message_id.as_deref(), Some("client-msg-1"));
        assert!(parsed.blocking_requested);
    }

    #[test]
    fn parse_rejects_missing_message_and_empty_text() {
        // Missing message object entirely.
        let err = parse_message_send_params(&serde_json::json!({})).unwrap_err();
        assert!(matches!(err, SendParamError::Invalid(_)));

        // Parts present but only whitespace text.
        let err =
            parse_message_send_params(&send_params(vec![text_part("   \n  ")])).unwrap_err();
        assert!(matches!(err, SendParamError::Invalid(_)));

        // Empty parts array.
        let err = parse_message_send_params(&send_params(vec![])).unwrap_err();
        assert!(matches!(err, SendParamError::Invalid(_)));

        // Invalid errors map to JSON-RPC -32602.
        let response = err.to_jsonrpc(&serde_json::json!(9));
        assert_eq!(response["error"]["code"], -32602);
    }

    #[test]
    fn parse_rejects_oversized_text() {
        let big = "a".repeat(MAX_MESSAGE_TEXT_BYTES + 1);
        let err = parse_message_send_params(&send_params(vec![text_part(&big)])).unwrap_err();
        match err {
            SendParamError::Invalid(msg) => assert!(msg.contains("too large")),
            other => panic!("expected Invalid, got {other:?}"),
        }

        // Two parts whose SUM exceeds the cap are rejected too.
        let half = "b".repeat(MAX_MESSAGE_TEXT_BYTES / 2 + 10);
        let err = parse_message_send_params(&send_params(vec![
            text_part(&half),
            text_part(&half),
        ]))
        .unwrap_err();
        assert!(matches!(err, SendParamError::Invalid(_)));
    }

    #[test]
    fn parse_rejects_task_continuation_with_unsupported_operation() {
        let params = serde_json::json!({
            "message": {
                "role": "user",
                "taskId": "prior-task",
                "parts": [text_part("continue please")],
            }
        });
        let err = parse_message_send_params(&params).unwrap_err();
        assert!(matches!(err, SendParamError::Unsupported(_)));
        let response = err.to_jsonrpc(&serde_json::json!(1));
        assert_eq!(response["error"]["code"], -32004);
    }

    // ── Bus task schema golden ──────────────────────────────

    #[test]
    fn bus_task_json_matches_dispatcher_schema_field_for_field() {
        // The gateway AgentDispatcher (dispatcher.rs `BusMessage`) consumes:
        //   type / message_id / agent_id / payload / timestamp
        //   + delegation-safety fields delegation_depth / origin_agent / sender_agent.
        // Optional response-side fields (response, in_reply_to, coalesced_ids,
        // turn_id, session_id) must be ABSENT on submissions, matching the
        // dispatcher's own skip_serializing_if behavior.
        let v = build_bus_task_json("task-1", "agent-x", "do the thing", "2026-07-05T00:00:00Z");
        let obj = v.as_object().expect("object");

        assert_eq!(obj.len(), 8, "exactly the 8 submission fields, got: {obj:?}");
        assert_eq!(v["type"], "agent_message");
        assert_eq!(v["message_id"], "task-1");
        assert_eq!(v["agent_id"], "agent-x");
        assert_eq!(v["payload"], "do the thing");
        assert_eq!(v["timestamp"], "2026-07-05T00:00:00Z");
        assert_eq!(v["delegation_depth"], 0);
        assert_eq!(v["origin_agent"], "a2a-client");
        assert_eq!(v["sender_agent"], "a2a-client");
        for absent in ["response", "in_reply_to", "coalesced_ids", "turn_id", "session_id"] {
            assert!(obj.get(absent).is_none(), "{absent} must be omitted");
        }
    }

    // ── Locked append (write-path integration) ──────────────

    #[test]
    fn append_bus_task_writes_whole_lines_and_takes_advisory_lock() {
        let home = tempfile::TempDir::new().unwrap();
        let line1 = build_bus_task_json("t1", "agent-x", "one", "ts").to_string();
        let line2 = build_bus_task_json("t2", "agent-x", "two", "ts").to_string();
        append_bus_task_sync(home.path(), &line1).expect("first append");
        append_bus_task_sync(home.path(), &line2).expect("second append");

        let queue = home.path().join("bus_queue.jsonl");
        let content = std::fs::read_to_string(&queue).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in &lines {
            let v: serde_json::Value = serde_json::from_str(line).expect("each line whole JSON");
            assert_eq!(v["type"], "agent_message");
        }
        // Convention #3: the with_file_lock advisory lock file must exist.
        assert!(
            home.path().join("bus_queue.jsonl.lock").exists(),
            "advisory lock file missing — append did not go through with_file_lock"
        );
    }

    // ── Queue probe state mapping ───────────────────────────

    #[test]
    fn probe_maps_queue_observations_to_states() {
        let queued = build_bus_task_json("t-queued", "agent-x", "hi", "ts").to_string();
        let response = serde_json::json!({
            "type": "agent_response",
            "message_id": "resp-1",
            "agent_id": "agent-x",
            "payload": "the answer",
            "timestamp": "ts",
            "in_reply_to": "t-done",
        })
        .to_string();
        let content = format!("{queued}\n{response}\nnot-json garbage line\n");

        // agent_message still queued → Queued (state "submitted").
        assert_eq!(probe_bus_task_state(&content, "t-queued"), BusProbe::Queued);
        // agent_response present → Responded with payload ("completed").
        assert_eq!(
            probe_bus_task_state(&content, "t-done"),
            BusProbe::Responded("the answer".to_string())
        );
        // Neither line → Unknown (mapped to "working" with an honest note).
        assert_eq!(probe_bus_task_state(&content, "t-gone"), BusProbe::Unknown);
        // Empty file → Unknown.
        assert_eq!(probe_bus_task_state("", "t-queued"), BusProbe::Unknown);
    }

    // ── Response shape + tasks/get end-to-end (tempdir home) ─

    fn parsed(text: &str) -> ParsedSendMessage {
        ParsedSendMessage {
            text: text.to_string(),
            context_id: None,
            client_message_id: None,
            skipped_parts: 0,
            blocking_requested: false,
        }
    }

    #[tokio::test]
    async fn enqueue_and_respond_returns_submitted_task_and_writes_bus_line() {
        let home = tempfile::TempDir::new().unwrap();
        let mut index = BusTaskIndex::default();
        let id = serde_json::json!(11);

        let response = enqueue_and_respond(
            &id,
            &parsed("run the report"),
            "default",
            "agent-main",
            home.path(),
            &mut index,
        )
        .await;

        // Spec-shaped A2A Task, honest "submitted" (async dispatch).
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 11);
        let task = &response["result"];
        assert_eq!(task["kind"], "task");
        assert_eq!(task["contextId"], "default");
        assert_eq!(task["status"]["state"], "submitted");
        assert!(task["status"]["timestamp"].as_str().is_some());
        let task_id = task["id"].as_str().expect("task id present");
        assert!(uuid::Uuid::parse_str(task_id).is_ok(), "task id is a uuid");
        assert_eq!(task["metadata"]["targetAgent"], "agent-main");

        // Bus line landed with the dispatcher schema and same id.
        let content =
            std::fs::read_to_string(home.path().join("bus_queue.jsonl")).expect("queue written");
        let line: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(line["type"], "agent_message");
        assert_eq!(line["message_id"], task_id);
        assert_eq!(line["agent_id"], "agent-main");
        assert_eq!(line["payload"], "run the report");

        // Index remembers the task for tasks/get.
        assert!(index.get(task_id).is_some());
    }

    #[tokio::test]
    async fn tasks_get_reports_bus_task_lifecycle_states() {
        use crate::acp::handlers::A2ATaskManager;

        let home = tempfile::TempDir::new().unwrap();
        let mgr = A2ATaskManager::new();
        let mut index = BusTaskIndex::default();
        let id = serde_json::json!(21);

        // Submit — line queued.
        let send = enqueue_and_respond(
            &id,
            &parsed("status probe"),
            "default",
            "agent-main",
            home.path(),
            &mut index,
        )
        .await;
        let task_id = send["result"]["id"].as_str().unwrap().to_string();
        let queue = home.path().join("bus_queue.jsonl");

        // 1) Still queued → submitted. tasks/get accepts A2A `id` param name.
        let params = serde_json::json!({ "id": task_id });
        let got = crate::acp::server::handle_tasks_get(&id, &params, &mgr, &index, home.path())
            .await;
        assert_eq!(got["result"]["status"]["state"], "submitted");
        assert_eq!(got["result"]["kind"], "task");

        // 2) Dispatcher consumed the line, no response yet → working (honest note).
        std::fs::write(&queue, "").unwrap();
        let got = crate::acp::server::handle_tasks_get(&id, &params, &mgr, &index, home.path())
            .await;
        assert_eq!(got["result"]["status"]["state"], "working");
        assert!(got["result"]["metadata"]["note"]
            .as_str()
            .unwrap()
            .contains("channels"));

        // 3) agent_response appended → completed with the text as an artifact.
        let response_line = serde_json::json!({
            "type": "agent_response",
            "message_id": "r1",
            "agent_id": "agent-main",
            "payload": "all done",
            "timestamp": "ts",
            "in_reply_to": task_id,
        });
        std::fs::write(&queue, format!("{response_line}\n")).unwrap();
        let got = crate::acp::server::handle_tasks_get(&id, &params, &mgr, &index, home.path())
            .await;
        assert_eq!(got["result"]["status"]["state"], "completed");
        assert_eq!(
            got["result"]["artifacts"][0]["parts"][0]["text"],
            "all done"
        );

        // 4) Unknown id (not in index) → TaskNotFoundError.
        let params = serde_json::json!({ "id": "never-submitted" });
        let got = crate::acp::server::handle_tasks_get(&id, &params, &mgr, &index, home.path())
            .await;
        assert_eq!(got["error"]["code"], -32001);
    }

    #[tokio::test]
    async fn message_send_rejects_unknown_target_agent() {
        // Home with an EMPTY agents dir: registry scan succeeds but resolves
        // no Main-role agent and no named agent → invalid-params error, and
        // nothing is written to the bus (fail-closed).
        let home = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(home.path().join("agents")).unwrap();
        let mut index = BusTaskIndex::default();

        let params = serde_json::json!({
            "message": {
                "role": "user",
                "contextId": "ghost-agent",
                "parts": [ { "kind": "text", "text": "hello?" } ],
            }
        });
        let id = serde_json::json!(31);
        let response = crate::acp::message_send::handle_message_send(
            &id,
            &params,
            home.path(),
            &mut index,
        )
        .await;
        assert_eq!(response["error"]["code"], -32602);
        assert!(response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("ghost-agent"));
        assert!(!home.path().join("bus_queue.jsonl").exists());
    }
}
