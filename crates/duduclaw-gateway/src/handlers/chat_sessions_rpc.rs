//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;
use crate::history_privacy::{self as privacy, Class, Mode, handles, read};

impl MethodHandler {
    // ── WebChat session history + resume handlers (WP3) ─────────
    //
    // These back the dashboard's "past conversations" picker and resume flow.
    // Sessions are keyed by agent in `sessions.db`; listing/history are
    // read-only and agent-scoped (same fail-closed intent as `runs.*`). The
    // resume write path itself lives in `webchat.rs` (the `/ws/chat` socket);
    // these RPCs only surface what exists so the client can pick a session id
    // to resume.

    pub(crate) async fn handle_chat_sessions_list(&self, params: Value, ctx: &UserContext) -> WsFrame {
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

        let live = match self.bound_history_live(ctx) { Ok(c) => c, Err(e) => return WsFrame::error_response("", &e) };
        let initial = if let Some(agent) = agent_filter {
            match privacy::mode(&self.home_dir, &live, agent) { Ok(m) => m, Err(_) => return WsFrame::error_response("", "session not found") }
        } else if live.is_admin() { Mode::Full } else { return WsFrame::error_response("", "agent_id required"); };
        if initial == Mode::Filtered {
            return match self.bound_history_list(&live, agent_filter.unwrap(), limit).await {
                Ok(sessions) => WsFrame::ok_response("",json!({"sessions":sessions})),
                Err(e) => WsFrame::error_response("",&e),
            };
        }

        let rc = match self.reply_ctx.read().await.clone() {
            Some(c) => c,
            // Reply context not yet wired (very early startup) — no sessions to
            // show. Empty is the honest answer, not an error.
            None => return WsFrame::ok_response("", json!({ "sessions": [] })),
        };

        match rc.session_manager.list_sessions(agent_filter, limit).await {
            Ok(list) => {
                let valid = if let Some(agent) = agent_filter {
                    self.bound_history_recheck(&live, agent, initial).is_ok()
                } else { self.bound_history_live(&live).is_ok_and(|c|c.is_admin()) };
                if !valid { return WsFrame::error_response("", "history authority changed; refresh"); }
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
        let mut session_id = params
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

        let live = match self.bound_history_live(ctx) { Ok(c) => c, Err(e) => return WsFrame::error_response("",&e) };
        session_id = match handles::resolve(&self.home_dir,&live.user_id,&session_id).await {
            Ok(s) => s, Err(_) => return WsFrame::error_response("","session not found"),
        };
        // Resolve the owning agent; fail closed on an unknown id (never expose
        // an empty transcript for a session that does not exist).
        let owner = match rc.session_manager.session_agent(&session_id).await {
            Ok(Some(a)) => a,
            Ok(None) => return WsFrame::error_response("", "session not found"),
            Err(e) => return WsFrame::error_response("", &format!("resolve session: {e}")),
        };
        let initial = match privacy::mode(&self.home_dir,&live,&owner) {
            Ok(m) => m, Err(_) => return WsFrame::error_response("","session not found"),
        };
        if initial == Mode::Filtered {
            let home=self.home_dir.clone(); let user=live.user_id.clone(); let id=session_id.clone();
            let permit=match read::permit() { Ok(p)=>p,Err(e)=>return WsFrame::error_response("",&e) };
            let metadata=tokio::task::spawn_blocking(move || {let _permit=permit;read::metadata(&read::open(&home)?,&user,&id)}).await;
            let class=match metadata { Ok(Ok(Some(m))) => m.class, _ => return WsFrame::error_response("","session not found") };
            match class {
                Class::Hidden => return WsFrame::error_response("","session not found"),
                Class::External {channel} => return match self.bound_external_history(&live,&session_id,&owner,&channel,limit).await {
                    Ok(value) => WsFrame::ok_response("",value), Err(e) => WsFrame::error_response("",&e),
                },
                Class::Own => {},
            }
        }

        // Row ids ride along (when both reads agree) so a resumed
        // conversation can re-attach records keyed by message id, such as
        // knowledge-base citations.
        let ids = rc
            .session_manager
            .get_message_ids(&session_id)
            .await
            .unwrap_or_default();
        match rc.session_manager.get_messages(&session_id).await {
            Ok(msgs) => {
                if let Err(e)=self.bound_history_recheck(&live,&owner,initial) { return WsFrame::error_response("",&e); }
                let ids = (ids.len() == msgs.len()).then_some(ids);
                // F9: `get_messages` returns oldest→newest. Show the NEWEST
                // `limit` turns (where the user left off), not the oldest — on a
                // long resumed conversation the oldest window is stale scrollback.
                // Skip to the tail while preserving chronological order.
                let start = msgs.len().saturating_sub(limit);
                let messages: Vec<Value> = msgs
                    .iter()
                    .enumerate()
                    .skip(start)
                    .map(|(i, m)| {
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
                        if let Some(id) = ids.as_ref().and_then(|v| v.get(i)) {
                            row["id"] = json!(id);
                        }
                        row
                    })
                    .collect();
                let mut response=json!({"session_id":session_id,"agent_id":owner,"messages":messages});
                if initial==Mode::Filtered {response["privacy_mode"]=json!("own");response["channel"]=json!("webchat");}
                WsFrame::ok_response("",response)
            }
            Err(e) => WsFrame::error_response("", &format!("session history: {e}")),
        }
    }
}
