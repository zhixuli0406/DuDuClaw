use super::*;

/// RFC-24: resolve an open decision to a chosen option.
///
/// The caller (`agent_id`) can only resolve its own decisions — the engine keys
/// every decision by `agent_id`, so a foreign id resolves to `NotFound`
/// (fail-closed). Returns the chosen content on success so the agent can act
/// immediately without re-reading.
///
/// v1.83.0: on a channel conversation turn
/// ([`crate::mcp_memory_handlers::mcp_read_scope`]) only an open decision
/// recorded in the current session can be resolved; any other id gets the
/// same "not found" answer as an id that does not exist.
pub(crate) async fn handle_decision_resolve(
    params: &Value,
    memory: &SqliteMemoryEngine,
    agent_id: &str,
) -> Value {
    let scope = crate::mcp_memory_handlers::mcp_read_scope(agent_id);
    handle_decision_resolve_in(params, memory, agent_id, &scope).await
}

/// The answer for a decision id that is unknown (or not this caller's).
fn decision_not_found(decision_id: &str) -> Value {
    serde_json::json!({
        "content": [{"type": "text", "text": format!(
            "找不到決策 [{decision_id}](可能已過期、不存在、或不屬於你)。請勿臆測,改向使用者確認。"
        )}],
        "structuredContent": { "ok": false, "error": "not_found", "decision_id": decision_id },
        "isError": true
    })
}

/// [`handle_decision_resolve`] with an explicit scope.
pub(crate) async fn handle_decision_resolve_in(
    params: &Value,
    memory: &SqliteMemoryEngine,
    agent_id: &str,
    scope: &crate::mcp_memory_handlers::ReadScope,
) -> Value {
    let decision_id = params
        .get("decision_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let chosen_key = params
        .get("chosen_key")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if decision_id.is_empty() || chosen_key.is_empty() {
        return tool_error("Error: decision_id and chosen_key are both required");
    }

    // v1.83.0: a channel turn resolves only its own conversation's decisions.
    if let crate::mcp_memory_handlers::ReadScope::Conversation(session) = scope {
        let session = session.as_deref().unwrap_or("");
        match memory.is_open_decision_in_session(agent_id, decision_id, session).await {
            Ok(true) => {}
            Ok(false) => return decision_not_found(decision_id),
            Err(e) => return tool_error(&format!("Error resolving decision: {e}")),
        }
    }

    // P2-B: the agent's choice is recorded with its MCP turn as the source
    // (the engine adds the decision rows as parents).
    let provenance = match crate::mcp_memory_handlers::mcp_stamped_provenance(agent_id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "decision_resolve refused: malformed host env");
            return tool_error(&crate::mcp_memory_handlers::malformed_host_env(&e));
        }
    };
    match memory
        .resolve_decision(agent_id, decision_id, chosen_key, provenance)
        .await
    {
        Ok(duduclaw_memory::DecisionResolveOutcome::Resolved {
            chosen_key,
            chosen_content,
            question,
        }) => serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "已解決決策 [{decision_id}]「{question}」→ 選擇 {chosen_key}：{chosen_content}"
            )}],
            "structuredContent": {
                "ok": true,
                "decision_id": decision_id,
                "chosen_key": chosen_key,
                "chosen_content": chosen_content,
                "question": question,
            }
        }),
        Ok(duduclaw_memory::DecisionResolveOutcome::NotFound) => decision_not_found(decision_id),
        Ok(duduclaw_memory::DecisionResolveOutcome::AlreadyResolved(status)) => serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "決策 [{decision_id}] 已是「{status}」狀態,無需再次解決。"
            )}],
            "structuredContent": { "ok": false, "error": "already_resolved", "status": status },
            "isError": true
        }),
        Ok(duduclaw_memory::DecisionResolveOutcome::UnknownKey { available }) => {
            serde_json::json!({
                "content": [{"type": "text", "text": format!(
                    "選項 '{chosen_key}' 不在決策 [{decision_id}] 的選項中。可選:{}",
                    available.join(", ")
                )}],
                "structuredContent": { "ok": false, "error": "unknown_key", "available": available },
                "isError": true
            })
        }
        Err(e) => {
            if duduclaw_gateway::memory_provenance::record_fenced_error(
                &crate::mcp_isolation_identity::home(),
                agent_id,
                "mcp_decision_resolve",
                &e,
            ) {
                tool_error("Not resolved: the conversation this choice comes from was forgotten by the operator")
            } else {
                tool_error(&format!("Error resolving decision: {e}"))
            }
        }
    }
}

/// RFC-24: list the caller's currently-open decisions (read-only).
///
/// v1.83.0: on a channel conversation turn only the decisions recorded in the
/// current session (none when the session is unknown).
pub(crate) async fn handle_decision_list(
    params: &Value,
    memory: &SqliteMemoryEngine,
    agent_id: &str,
) -> Value {
    let scope = crate::mcp_memory_handlers::mcp_read_scope(agent_id);
    handle_decision_list_in(params, memory, agent_id, &scope).await
}

/// [`handle_decision_list`] with an explicit scope.
pub(crate) async fn handle_decision_list_in(
    params: &Value,
    memory: &SqliteMemoryEngine,
    agent_id: &str,
    scope: &crate::mcp_memory_handlers::ReadScope,
) -> Value {
    let limit = params
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(10)
        .clamp(1, 50) as usize;
    let listed = match scope {
        crate::mcp_memory_handlers::ReadScope::Employee => {
            memory.list_open_decisions(agent_id, limit).await
        }
        crate::mcp_memory_handlers::ReadScope::Conversation(session) => {
            memory
                .list_open_decisions_in_session(agent_id, session.as_deref().unwrap_or(""), limit)
                .await
        }
    };
    match listed {
        Ok(decisions) => {
            if decisions.is_empty() {
                return serde_json::json!({
                    "content": [{"type": "text", "text": "目前沒有未決決策。"}],
                    "structuredContent": { "decisions": [] }
                });
            }
            let text = decisions
                .iter()
                .map(|d| {
                    let opts = d
                        .options
                        .iter()
                        .map(|(k, c)| format!("  - {k}：{c}"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    format!("[decision:{}] {}\n{}", d.id, d.question, opts)
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            serde_json::json!({
                "content": [{"type": "text", "text": text}],
                "structuredContent": { "decisions": decisions }
            })
        }
        Err(e) => tool_error(&format!("Error listing decisions: {e}")),
    }
}

#[cfg(test)]
mod conversation_scope_tests {
    use super::*;
    use crate::mcp_memory_handlers::ReadScope;

    fn in_session(s: &str) -> ReadScope {
        ReadScope::Conversation(Some(s.to_string()))
    }

    /// A decision recorded in `session-a` of employee `shared-agent`.
    async fn engine_with_a_decision() -> (tempfile::TempDir, SqliteMemoryEngine, String) {
        let dir = tempfile::tempdir().unwrap();
        let engine = SqliteMemoryEngine::new(&dir.path().join("memory.db")).unwrap();
        let id = duduclaw_gateway::decision_capture::decision_id("shared-agent", "session-a-msg");
        let draft = duduclaw_gateway::decision_capture::DecisionDraft {
            question: "Alice private deployment choice".into(),
            options: vec![("A".into(), "Deploy Alice project".into()), ("B".into(), "Wait".into())],
        };
        duduclaw_gateway::decision_capture::persist_decision(
            &engine,
            "shared-agent",
            &id,
            &draft,
            serde_json::json!({"session_id": "session-a"}),
            duduclaw_memory::lineage::Provenance::source(
                duduclaw_memory::lineage::SourceRef::channel_message(
                    "session-a", 1, chrono::Utc::now(), None,
                ),
            ),
        )
        .await
        .unwrap();
        (dir, engine, id)
    }

    fn listed_ids(v: &Value) -> Vec<String> {
        v["structuredContent"]["decisions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["id"].as_str().unwrap().to_string())
            .collect()
    }

    /// Session B neither lists nor resolves session A's decision (the refusal
    /// is the plain not-found answer), and A's decision stays open.
    #[tokio::test]
    async fn another_session_neither_lists_nor_resolves_the_decision() {
        let (_dir, engine, id) = engine_with_a_decision().await;
        let agent = "shared-agent";
        for scope in [in_session("session-b"), ReadScope::Conversation(None)] {
            let list = handle_decision_list_in(&serde_json::json!({}), &engine, agent, &scope).await;
            assert!(listed_ids(&list).is_empty(), "{scope:?}: {list}");
            assert!(!list.to_string().contains("Alice private deployment choice"));
            let resolve = handle_decision_resolve_in(
                &serde_json::json!({"decision_id": id, "chosen_key": "A"}),
                &engine,
                agent,
                &scope,
            )
            .await;
            assert_eq!(resolve, decision_not_found(&id), "{scope:?}");
        }
        assert_eq!(engine.decision_status(agent, &id).await.unwrap().as_deref(), Some("open"));
        // The not-found answer is the one an unknown id gets.
        let missing = handle_decision_resolve_in(
            &serde_json::json!({"decision_id": "no-such-decision", "chosen_key": "A"}),
            &engine,
            agent,
            &ReadScope::Employee,
        )
        .await;
        assert_eq!(missing, decision_not_found("no-such-decision"));
    }

    /// Session A lists and resolves its own decision; a non-channel caller
    /// (dispatch run, cron) still lists employee-wide.
    #[tokio::test]
    async fn own_session_lists_and_resolves() {
        let (_dir, engine, id) = engine_with_a_decision().await;
        let agent = "shared-agent";
        let employee =
            handle_decision_list_in(&serde_json::json!({}), &engine, agent, &ReadScope::Employee).await;
        assert_eq!(listed_ids(&employee), vec![id.clone()]);
        let own = handle_decision_list_in(&serde_json::json!({}), &engine, agent, &in_session("session-a")).await;
        assert_eq!(listed_ids(&own), vec![id.clone()]);
        let resolve = handle_decision_resolve_in(
            &serde_json::json!({"decision_id": id, "chosen_key": "A"}),
            &engine,
            agent,
            &in_session("session-a"),
        )
        .await;
        assert_eq!(resolve["structuredContent"]["ok"], true, "{resolve}");
        assert_eq!(engine.decision_status(agent, &id).await.unwrap().as_deref(), Some("resolved:A"));
    }
}
