//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── WebChat session history + resume handlers (WP3) ─────────
    //
    // These back the dashboard's "past conversations" picker and resume flow.
    // Sessions are keyed by agent in `sessions.db`; listing/history are
    // read-only and agent-scoped (same fail-closed intent as `runs.*`). The
    // resume write path itself lives in `webchat.rs` (the `/ws/chat` socket);
    // these RPCs only surface what exists so the client can pick a session id
    // to resume.

    pub(crate) async fn handle_chat_sessions_list(&self, params: Value) -> WsFrame {
        let agent_filter = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let Some(a) = agent_filter {
            if !is_valid_agent_id(a) {
                return WsFrame::error_response("", "Invalid agent_id format");
            }
        }
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(CHAT_SESSIONS_LIST_DEFAULT_LIMIT as u64)
            .min(CHAT_SESSIONS_LIST_MAX_LIMIT as u64) as usize;

        let rc = match self.reply_ctx.read().await.clone() {
            Some(c) => c,
            // Reply context not yet wired (very early startup) — no sessions to
            // show. Empty is the honest answer, not an error.
            None => return WsFrame::ok_response("", json!({ "sessions": [] })),
        };

        match rc.session_manager.list_sessions(agent_filter, limit).await {
            Ok(list) => {
                let sessions: Vec<Value> = list
                    .iter()
                    .map(|s| {
                        json!({
                            "session_id": s.id,
                            "agent_id": s.agent_id,
                            "title": s.title,
                            "last_active": s.last_active,
                            "turns": s.turn_count,
                            "tokens": s.total_tokens,
                            "lineage": s.lineage,
                        })
                    })
                    .collect();
                WsFrame::ok_response("", json!({ "sessions": sessions }))
            }
            Err(e) => WsFrame::error_response("", &format!("list sessions: {e}")),
        }
    }

    pub(crate) async fn handle_chat_sessions_history(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let session_id = params
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if session_id.is_empty() {
            return WsFrame::error_response("", "session_id is required");
        }
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(CHAT_HISTORY_DEFAULT_LIMIT as u64)
            .min(CHAT_HISTORY_MAX_LIMIT as u64) as usize;

        let rc = match self.reply_ctx.read().await.clone() {
            Some(c) => c,
            None => return WsFrame::error_response("", "session store unavailable"),
        };

        // Resolve the owning agent; fail closed on an unknown id (never expose
        // an empty transcript for a session that does not exist).
        let owner = match rc.session_manager.session_agent(&session_id).await {
            Ok(Some(a)) => a,
            Ok(None) => return WsFrame::error_response("", "session not found"),
            Err(e) => return WsFrame::error_response("", &format!("resolve session: {e}")),
        };
        // Agent-scoped authz — viewer level, fail-closed for non-admins (mirrors
        // runs.get). Prevents loading a conversation for an agent the caller
        // cannot see.
        if !ctx.is_admin() {
            if let Err(e) = acl::require_agent_access(ctx, &owner, AccessLevel::Viewer) {
                return WsFrame::error_response("", &e);
            }
        }

        match rc.session_manager.get_messages(&session_id).await {
            Ok(msgs) => {
                // F9: `get_messages` returns oldest→newest. Show the NEWEST
                // `limit` turns (where the user left off), not the oldest — on a
                // long resumed conversation the oldest window is stale scrollback.
                // Skip to the tail while preserving chronological order.
                let start = msgs.len().saturating_sub(limit);
                let messages: Vec<Value> = msgs
                    .iter()
                    .skip(start)
                    .map(|m| {
                        let (content, artifact) =
                            chat_history_row_content_and_artifact(&m.role, &m.content);
                        let mut row = json!({
                            "role": m.role,
                            "content": content,
                            "timestamp": m.timestamp,
                            "tokens": m.tokens,
                        });
                        if let Some(a) = artifact {
                            row["artifact"] = a;
                        }
                        row
                    })
                    .collect();
                WsFrame::ok_response(
                    "",
                    json!({
                        "session_id": session_id,
                        "agent_id": owner,
                        "messages": messages,
                    }),
                )
            }
            Err(e) => WsFrame::error_response("", &format!("session history: {e}")),
        }
    }
}
