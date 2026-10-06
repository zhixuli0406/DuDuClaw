use super::*;

/// RFC-24: resolve an open decision to a chosen option.
///
/// The caller (`agent_id`) can only resolve its own decisions — the engine keys
/// every decision by `agent_id`, so a foreign id resolves to `NotFound`
/// (fail-closed). Returns the chosen content on success so the agent can act
/// immediately without re-reading.
pub(crate) async fn handle_decision_resolve(
    params: &Value,
    memory: &SqliteMemoryEngine,
    agent_id: &str,
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

    // P2-B: the agent's choice is recorded with its MCP turn as the source
    // (the engine adds the decision rows as parents).
    let provenance = match crate::mcp_memory_handlers::mcp_write_provenance(agent_id) {
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
        Ok(duduclaw_memory::DecisionResolveOutcome::NotFound) => serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "找不到決策 [{decision_id}](可能已過期、不存在、或不屬於你)。請勿臆測,改向使用者確認。"
            )}],
            "structuredContent": { "ok": false, "error": "not_found", "decision_id": decision_id },
            "isError": true
        }),
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
                &duduclaw_core::duduclaw_home(),
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
pub(crate) async fn handle_decision_list(
    params: &Value,
    memory: &SqliteMemoryEngine,
    agent_id: &str,
) -> Value {
    let limit = params
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(10)
        .clamp(1, 50) as usize;
    match memory.list_open_decisions(agent_id, limit).await {
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
