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
        // RM-173: `archived: true` lists only archived (「已收起」) conversations,
        // with exactly the same authority and privacy filters as the live list.
        let archived = match params.get("archived") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => return WsFrame::error_response("", "archived must be a boolean"),
        };

        let live = match self.bound_history_live(ctx) { Ok(c) => c, Err(e) => return WsFrame::error_response("", &e) };
        let initial = if let Some(agent) = agent_filter {
            match privacy::mode(&self.home_dir, &live, agent) { Ok(m) => m, Err(_) => return WsFrame::error_response("", "session not found") }
        } else if live.is_admin() { Mode::Full } else { return WsFrame::error_response("", "agent_id required"); };
        if initial == Mode::Filtered {
            return match self.bound_history_list(&live, agent_filter.unwrap(), limit, archived).await {
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

        // v1.81 P1: an unscoped (Admin) listing hides Personal employees the
        // caller does not own; a scoped one was gated at dispatch. Applies to
        // the archived (RM-173) listing exactly as to the live one.
        let hidden = if agent_filter.is_none() {
            match self.personal_hidden_agents(&live).await {
                Ok(hidden) => hidden,
                Err(e) => return WsFrame::error_response("", e),
            }
        } else { Default::default() };
        match rc.session_manager.list_sessions_in(agent_filter, limit, archived).await {
            Ok(list) => {
                let valid = if let Some(agent) = agent_filter {
                    self.bound_history_recheck(&live, agent, initial).is_ok()
                } else { self.bound_history_live(&live).is_ok_and(|c|c.is_admin()) };
                if !valid { return WsFrame::error_response("", "history authority changed; refresh"); }
                let sessions: Vec<Value> = list
                    .iter()
                    .filter(|s| !hidden.contains(&s.agent_id))
                    .map(|s| {
                        let mut row = json!({
                            "session_id": s.id,
                            "agent_id": s.agent_id,
                            "title": s.title,
                            "last_active": s.last_active,
                            "turns": s.turn_count,
                            "tokens": s.total_tokens,
                            "lineage": s.lineage,
                            "archived": archived,
                        });
                        if archived {
                            // RM-173: `user` (cleared by its user: `/new`, a
                            // channel reset) or `retention` (inactivity).
                            row["archive_reason"] = json!(s
                                .archive_reason
                                .as_deref()
                                .unwrap_or(crate::session::ARCHIVE_REASON_RETENTION));
                        }
                        row
                    })
                    .collect();
                WsFrame::ok_response("", json!({ "sessions": sessions }))
            }
            Err(e) => WsFrame::error_response("", &format!("list sessions: {e}")),
        }
    }

    /// Shared conversation read gate for history JSON and attachment bytes.
    /// `step_up` is the Admin break-glass CSRF token for a Personal employee
    /// (v1.81 P1); attachment downloads carry none, so an Admin who is not
    /// the owner cannot fetch a Personal employee's attachments.
    async fn history_session_access(&self, ctx: &UserContext, requested: &str, step_up: Option<[u8; 32]>) -> Result<(UserContext, String, String, Mode, Class), String> {
        self.history_session_access_in(ctx, requested, false, "chat.sessions.history", step_up).await
    }

    /// [`Self::history_session_access`] for a live (`archived = false`) or an
    /// archived (`archived = true`, RM-173) conversation. In Filtered mode the
    /// other archive state reads as "session not found". `method` names the
    /// calling RPC in the P1 personal-data gate's audit row.
    async fn history_session_access_in(&self, ctx: &UserContext, requested: &str, archived: bool, method: &str, step_up: Option<[u8; 32]>) -> Result<(UserContext, String, String, Mode, Class), String> {
        let live = self.bound_history_live(ctx)?;
        let id = handles::resolve(&self.home_dir, &live.user_id, requested).await?;
        let rc = self.reply_ctx.read().await.clone().ok_or("session store unavailable")?;
        let owner = rc.session_manager.session_agent(&id).await
            .map_err(|_| "session not found")?.ok_or("session not found")?;
        // v1.81 P1d: a non-Admin cannot tell another person's Personal
        // employee's session from an unknown one.
        let admin = live.is_admin();
        self.personal_data_gate(&live, super::personal_data_gate::Target {
            method,
            category: crate::personal_visibility::DataCategory::Sessions,
            agent: &owner,
        }, step_up).await.map_err(|e| if admin { e.to_string() } else { "session not found".to_string() })?;
        let mode = privacy::mode(&self.home_dir, &live, &owner)?;
        let class = if mode == Mode::Filtered {
            let home = self.home_dir.clone();
            let user = live.user_id.clone();
            let raw = id.clone();
            let permit = read::permit()?;
            let metadata = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                read::metadata_in(&read::open(&home)?, &user, &raw, archived)
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

        let step_up = params.get(super::personal_data_gate::STEP_UP_FIELD)
            .and_then(|raw| serde_json::from_value::<[u8; 32]>(raw.clone()).ok());
        let (live, session_id, owner, initial, class) = match self.history_session_access(ctx, &session_id, step_up).await {
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
                    if let Some(id) = &entry.client_msg_id { row["client_msg_id"] = json!(id); }
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
    /// RM-173 `chat.sessions.restore {session_id}` — bring back one of the
    /// caller's own archived (「已收起」) conversations.
    ///
    /// Authority is exactly the read gate used to open a conversation
    /// ([`Self::history_session_access_in`]: live account, handle resolution,
    /// agent binding, privacy mode, stored-origin classification), restricted
    /// further to conversations the caller may write to: in the privacy-
    /// filtered mode only their own WebChat (`Class::Own`) — a masked,
    /// read-only external conversation cannot be restored. A conversation a
    /// user cleared (`archive_reason = 'user'` or the sticky
    /// `user_cleared_at`) is restorable only by a live Admin or its own
    /// WebChat owner, whatever the privacy mode. Audited
    /// (`chat_session_restored`, fail closed) before the write.
    pub(crate) async fn handle_chat_sessions_restore(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let requested = params
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if requested.is_empty() || requested.len() > 1024 {
            return WsFrame::error_response("", "session_id is required");
        }
        let Some(rc) = self.reply_ctx.read().await.clone() else {
            return WsFrame::error_response("", "session store unavailable");
        };
        // v1.81 P1: the same personal-data gate as opening the conversation
        // (another person's Personal employee: refused, or an audited Admin
        // break-glass step-up).
        let step_up = params.get(super::personal_data_gate::STEP_UP_FIELD)
            .and_then(|raw| serde_json::from_value::<[u8; 32]>(raw.clone()).ok());
        let (live, id, owner, mode, class) = match self.history_session_access_in(ctx, &requested, true, "chat.sessions.restore", step_up).await {
            Ok(access) => access,
            Err(e) => return WsFrame::error_response("", &e),
        };
        if class != Class::Own {
            return WsFrame::error_response("", "permission denied");
        }
        let state = match rc.session_manager.archive_state(&id).await {
            Ok(a) => a,
            Err(_) => return WsFrame::error_response("", "session not found"),
        };
        let Some((archived_at, archive_reason)) = state else {
            // Already live (e.g. a new message restored it): nothing to do.
            return WsFrame::ok_response("", json!({"session_id": requested, "restored": false}));
        };
        // RM-173: a conversation a USER cleared is theirs. Full mode grants
        // `Class::Own` to every Full-mode viewer (Admin, the agent's Owner, a
        // Viewer when bound_history_privacy is off), so that is not enough
        // here: only a live Admin or the conversation's own WebChat owner may
        // restore it. The sticky `user_cleared_at` keeps this true after an
        // earlier restore and re-archive. Refused like a missing session.
        let user_cleared = match rc.session_manager.user_cleared_at(&id).await {
            Ok(mark) => mark.is_some() || archive_reason == crate::session::ARCHIVE_REASON_USER,
            Err(_) => return WsFrame::error_response("", "session not found"),
        };
        if user_cleared && !live.is_admin() {
            let own_webchat = matches!(
                rc.session_manager.session_origin(&id).await,
                Ok(Some(origin)) if origin.kind() == crate::session_origin::OriginKind::WebchatUser
                    && origin.owner_user_id() == Some(live.user_id.as_str())
            );
            if !own_webchat {
                return WsFrame::error_response("", "session not found");
            }
        }
        if let Err(e) = self.bound_history_recheck(&live, &owner, mode) {
            return WsFrame::error_response("", &e);
        }
        let digest = match crate::team_history::audit::session_digest(&self.home_dir, &id) {
            Ok(d) => d,
            Err(_) => return WsFrame::error_response("", "audit unavailable"),
        };
        if crate::team_history::audit::write(
            &self.home_dir,
            "chat_session_restored",
            &live.user_id,
            &owner,
            json!({"actor_user_id": live.user_id, "agent": owner, "session": digest,
                   "archived_at": archived_at, "archive_reason": archive_reason, "user_cleared": user_cleared, "privacy_mode": if mode == Mode::Full { "full" } else { "own" }}),
            None,
        )
        .is_err()
        {
            return WsFrame::error_response("", "audit unavailable");
        }
        match rc.session_manager.restore_archived(&id).await {
            Ok(_) => WsFrame::ok_response("", json!({"session_id": requested, "restored": true})),
            Err(_) => WsFrame::error_response("", "restore failed"),
        }
    }

    /// Membership is checked independently of cached routing data. No handles
    /// are issued here and no per-reference ledger scans are performed.
    async fn history_contains_attachment(&self, ctx: &UserContext, reference: &crate::chat_attachments::Reference, index: std::sync::Arc<crate::artifacts::HistoryIndex>) -> Result<(), String> {
        let (live, session, agent, mode, class) = self.history_session_access(ctx, &reference.session, None).await?;
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
        let (_, _, agent, _, class) = self.history_session_access(&live, &reference.session, None).await?;
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
