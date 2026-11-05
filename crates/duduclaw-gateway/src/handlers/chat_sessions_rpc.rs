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

    /// Shared conversation read gate for history JSON and attachment bytes.
    async fn history_session_access(&self, ctx: &UserContext, requested: &str) -> Result<(UserContext, String, String, Mode, Class), String> {
        let live = self.bound_history_live(ctx)?;
        let id = handles::resolve(&self.home_dir, &live.user_id, requested).await?;
        let rc = self.reply_ctx.read().await.clone().ok_or("session store unavailable")?;
        let owner = rc.session_manager.session_agent(&id).await
            .map_err(|_| "session not found")?.ok_or("session not found")?;
        let mode = privacy::mode(&self.home_dir, &live, &owner)?;
        let class = if mode == Mode::Filtered {
            let home = self.home_dir.clone();
            let user = live.user_id.clone();
            let raw = id.clone();
            let permit = read::permit()?;
            let metadata = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                read::metadata(&read::open(&home)?, &user, &raw)
            }).await.map_err(|_| "session not found")??.ok_or("session not found")?;
            if metadata.agent != owner || metadata.class == Class::Hidden { return Err("session not found".into()); }
            metadata.class
        } else { Class::Own };
        self.bound_history_recheck(&live, &owner, mode)?;
        Ok((live, id, owner, mode, class))
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

        if self.reply_ctx.read().await.is_none() { return WsFrame::error_response("", "session store unavailable"); }

        let (live, session_id, owner, initial, class) = match self.history_session_access(ctx, &session_id).await {
            Ok(access) => access, Err(e) => return WsFrame::error_response("", &e),
        };
        if let Class::External { channel } = class {
            return match self.bound_external_history(&live, &session_id, &owner, &channel, limit).await {
                Ok(value) => WsFrame::ok_response("", value), Err(e) => WsFrame::error_response("", &e),
            };
        }

        let admission = match crate::chat_attachments::limits::history(&self.home_dir, &live.user_id) {
            Ok(p) => std::sync::Arc::new(p), Err(e) => return WsFrame::error_response("", &e),
        };
        let home = self.home_dir.clone();
        let user = live.user_id.clone();
        let session = session_id.clone();
        let agent = owner.clone();
        let worker_admission = admission.clone();
        let projected = tokio::task::spawn_blocking(move || {
            let _admission = worker_admission;
            let index = crate::artifacts::HistoryIndex::load(&home, Some(&agent));
            let rows = crate::chat_attachments::history_rows(&home, &session, limit)?;
            let mut references_left = crate::chat_attachments::limits::MAX_REFERENCES;
            let mut attachments_left = crate::chat_attachments::limits::MAX_ATTACHMENTS;
            let projected: Vec<_> = rows.into_iter().map(|entry| {
                let (body, attachments) = crate::chat_attachments::project_indexed(
                    &home, &user, &session, &agent, entry.id, &entry.message.content,
                    &index, &mut references_left, &mut attachments_left,
                );
                (entry, body, attachments)
            }).collect();
            Ok::<_, String>((index, projected))
        }).await.map_err(|_| "history unavailable".to_owned()).and_then(|r| r);
        match projected {
            Ok((index, projected)) => {
                let store = match self.workflow_store().await {
                    Ok(s) => s, Err(e) => return WsFrame::error_response("", &e),
                };
                let mut messages = Vec::new();
                for (entry, body, candidates) in projected {
                    let m = &entry.message;
                    let mut attachments = Vec::new();
                    for attachment in candidates {
                        if crate::review_evidence::download::authorize_artifact_access_indexed(
                            &self.home_dir, &store, &live, Some(&owner), &attachment.filename, &index,
                        ).await.is_ok() {
                            attachments.push(attachment);
                        }
                    }
                    let (content, artifact) = chat_history_row_content_and_artifact(&m.role, &body);
                    let mut row = json!({
                        "role": m.role, "content": content, "timestamp": m.timestamp,
                        "tokens": m.tokens, "attachments": attachments, "id": entry.id,
                    });
                    if let Some(a) = artifact { row["artifact"] = a; }
                    if let Some(kind) = &entry.notice_kind { row["notice_kind"] = json!(kind); }
                    messages.push(row);
                }
                if let Err(e) = self.bound_history_recheck(&live, &owner, initial) {
                    return WsFrame::error_response("", &e);
                }
                let mut response=json!({"session_id":session_id,"agent_id":owner,"messages":messages});
                if initial==Mode::Filtered {response["privacy_mode"]=json!("own");response["channel"]=json!("webchat");}
                WsFrame::ok_response("",response)
            }
            Err(e) => WsFrame::error_response("", &format!("session history: {e}")),
        }
    }
    /// Membership is checked independently of cached routing data. No handles
    /// are issued here and no per-reference ledger scans are performed.
    async fn history_contains_attachment(&self, ctx: &UserContext, reference: &crate::chat_attachments::Reference, index: std::sync::Arc<crate::artifacts::HistoryIndex>) -> Result<(), String> {
        let (live, session, agent, mode, class) = self.history_session_access(ctx, &reference.session).await?;
        if matches!(class, Class::External { .. } | Class::Hidden) || agent != reference.agent { return Err("attachment unavailable".into()); }
        let home = self.home_dir.clone();
        let message = reference.message;
        let path = reference.path.clone();
        let worker_agent = agent.clone();
        let permit = read::permit()?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let conn = read::open(&home)?;
            let raw: String = conn.query_row(
                "SELECT content FROM session_messages WHERE id=?1 AND session_id=?2 AND hidden=0 AND undone_at IS NULL AND role IN ('user','assistant') AND length(CAST(content AS BLOB))<=1048576",
                rusqlite::params![message, session], |r| r.get(0),
            ).map_err(|_| "attachment unavailable")?;
            let (_, refs) = crate::chat_attachments::parse(&raw);
            if !refs.into_iter().any(|(_, raw)| crate::chat_attachments::archived_path_indexed(&home, &worker_agent, &raw, &index).as_deref() == Some(path.as_str())) {
                return Err("attachment unavailable".to_owned());
            }
            Ok(())
        }).await.map_err(|_| "attachment unavailable")??;
        self.bound_history_recheck(&live, &agent, mode)
    }

    pub(crate) async fn history_attachment_bytes(&self, ctx: &UserContext, id: &str, max: u64) -> Result<(Vec<u8>, String, std::sync::Arc<crate::chat_attachments::limits::Permit>), String> {
        let live = self.bound_history_live(ctx)?;
        let admission = std::sync::Arc::new(crate::chat_attachments::limits::download(&self.home_dir, &live.user_id, max)?);
        let raw = handles::resolve_attachment(&self.home_dir, &live.user_id, id).await?;
        let reference: crate::chat_attachments::Reference = serde_json::from_str(&raw).map_err(|_| "attachment unavailable")?;
        // Check conversation authority before any attachment disk I/O.
        let (_, _, agent, _, class) = self.history_session_access(&live, &reference.session).await?;
        if agent != reference.agent || matches!(class, Class::External { .. } | Class::Hidden) { return Err("attachment unavailable".into()); }
        let home = self.home_dir.clone();
        let path = reference.path.clone();
        let worker_admission = admission.clone();
        let (index, bytes, hash, filename) = tokio::task::spawn_blocking(move || {
            let _admission = worker_admission;
            let index = std::sync::Arc::new(crate::artifacts::HistoryIndex::load(&home, Some(&agent)));
            let (file, filename) = crate::chat_attachments::open(&home, &agent, &path)?;
            let (bytes, hash) = crate::chat_attachments::snapshot(file, max)?;
            Ok::<_, String>((index, bytes, hash, filename))
        }).await.map_err(|_| "attachment unavailable")??;
        self.history_contains_attachment(&live, &reference, index.clone()).await?;
        let store = self.workflow_store().await?;
        crate::review_evidence::download::authorize_artifact_access_snapshot(
            &self.home_dir, &store, &live, Some(&reference.agent), &filename, &hash, bytes.len() as u64, &index,
        ).await?;
        self.history_contains_attachment(&live, &reference, index).await?;
        Ok((bytes, filename, admission))
    }

    #[cfg(test)]
    pub(crate) async fn open_history_attachment(&self, ctx: &UserContext, id: &str) -> Result<(std::io::Cursor<Vec<u8>>, String), String> {
        let (bytes, name, _permit) = self.history_attachment_bytes(ctx, id, crate::chat_attachments::limits::MAX_DOWNLOAD).await?;
        Ok((std::io::Cursor::new(bytes), name))
    }
}
