use super::*;

pub(in crate::webchat) struct TurnInput {
    pub state: Arc<WebChatState>,
    pub auth_user: String,
    pub credential: Option<duduclaw_auth::jwt::Claims>,
    pub user_id: String,
    pub agent_id: String,
    pub requested_agent: Option<String>,
    pub effective_agent: Option<String>,
    pub sid: String,
    pub conv_nonce: Option<String>,
    pub content: String,
    pub attachments: Vec<ChatAttachment>,
    pub assign_event: Option<String>,
    pub resume_acl: bool,
    pub arrival_seq: i64,
}

pub(super) async fn run(input: TurnInput, mut sink: TurnSink, ticket: &Ticket) {
    let TurnInput {
        state,
        requested_agent: _,
        auth_user,
        credential,
        user_id,
        agent_id,
        effective_agent,
        sid,
        conv_nonce,
        content,
        attachments,
        assign_event,
        resume_acl: _,
        arrival_seq,
    } = input;
    if !credential_allows_turn(
        state.credential_validity(credential.as_ref(), true).await, &sink,
    ).await {
        return;
    }
    let sid = sid.as_str();
    let effective_agent_id = effective_agent.as_deref().unwrap_or(&agent_id);
    // Persist any uploaded files and append path references
    // to the prompt — the agent (Claude CLI) reads them from
    // disk, exactly as channel attachments are handled.
    let mut full_content = content.clone();
    if !attachments.is_empty() {
        // WP1.3: land inbound files under the resolved agent's dir.
        let attach_base = crate::channel_reply::resolve_attachment_base(
            state.ctx.as_ref(),
            Some(effective_agent_id),
        )
        .await;
        let refs = crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL
            .scope(
                auth_user.clone(),
                save_webchat_attachments(&attach_base, &attachments),
            )
            .await;
        if !refs.is_empty() {
            if !full_content.trim().is_empty() {
                full_content.push_str("\n\n");
            }
            full_content.push_str(&refs.join("\n"));
        }
    }

    // Check for chat commands first
    if crate::chat_commands::is_command(&content) {
        if !ticket.begin_delivery() {
            return;
        }
        if let Some(cmd) = crate::chat_commands::parse_command(&content, None) {
            // WebChat carries no addressable per-sender channel
            // identity (no `agent_binding`-style account to bind
            // in the dashboard), so `/rules off` can never resolve
            // a verified Admin/Manager here — `handle_rules_off`
            // correctly refuses with "use the dashboard" rather
            // than guessing at an identity.
            // v1.68.0: admin-gated commands (`!STOP ALL`,
            // `!RESUME`, `/model <x>`) need an active
            // dashboard Admin account — never a
            // public-widget visitor (was hardcoded `true`).
            let is_admin = webchat_command_admin(&state.user_db, &auth_user);
            let caller = crate::chat_commands::CommandCaller {
                channel: "webchat",
                channel_user_id: if is_widget_visitor(&auth_user) {
                    ""
                } else {
                    &auth_user
                },
                access: state.sender_access(&auth_user).await,
                event: assign_event.as_deref(),
                seq: arrival_seq,
            };
            // `/交辦` keeps the task for a card; every other
            // command answers with text only. `/rules off`
            // still sees no channel identity here. The other
            // commands run in the same principal scope as a
            // reply turn, so one that creates the session
            // (`/status`) records who owns it (B5 session origin).
            let principal = &crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL;
            let (reply, task) = match &cmd {
                crate::chat_commands::ChatCommand::Assign(assign_cmd) => {
                    let ac =
                        caller.assign_caller(&state.ctx.home_dir, sid, Some(effective_agent_id));
                    let r = Box::pin(crate::trigger_route::assign::run(
                        &state.ctx, assign_cmd, &ac,
                    ))
                    .await;
                    (r.text, r.task)
                }
                crate::chat_commands::ChatCommand::Rules { .. } => (
                    principal
                        .scope(
                            auth_user.clone(),
                            crate::chat_commands::handle_command(
                                &cmd,
                                &state.ctx,
                                sid,
                                effective_agent_id,
                                is_admin,
                                "",
                            ),
                        )
                        .await,
                    None,
                ),
                _ => (
                    principal
                        .scope(
                            auth_user.clone(),
                            crate::chat_commands::handle_command_with_caller(
                                &cmd,
                                &state.ctx,
                                sid,
                                effective_agent_id,
                                is_admin,
                                &caller,
                            ),
                        )
                        .await,
                    None,
                ),
            };
            let done = ChatMessage::AssistantDone {
                content: reply,
                tokens_used: 0,
                conv: conv_nonce.clone(),
                model: None,
                // Chat commands (`/xxx`) never go through
                // `build_reply_*` / O-4 at all — no marker
                // can exist on this path.
                artifact: None,
                agent: None,
                agent_name: None,
                citations: None,
                thinking: None,
                task: task.map(|t| serde_json::json!({ "id": t.id, "agent": t.agent })),
            };
            if let Ok(json) = serde_json::to_string(&done) {
                let _ = sink.send(Message::Text(json.into())).await;
            }
            return;
        }
    }

    // Build AI reply, interleaving progress events
    // (tool activity / TODO board) onto the socket
    // while the task runs.
    let progress = Arc::new(super::progress::ProgressInbox::default());
    let ptx = progress.clone();
    let dropped = sink.progress_dropped.clone();
    let (watch_tx, mut watch_rx) =
        tokio::sync::mpsc::channel(crate::computer_watch_links::delivery::WEBCHAT_NOTICE_CAPACITY);
    let on_progress: crate::channel_reply::ProgressCallback = Box::new(move |event| {
        ptx.push(event, &dropped);
    });
    // Route to the selected agent (per-agent
    // directory / SOUL / session) when one was
    // resolved, else the default-agent path. Both
    // branches consume `on_progress`; they are
    // mutually exclusive so the single move is valid.
    // O-4→O-3: the guarded builders return the
    // stripped reply text plus any O-4 pending-op
    // marker mapped to an O-3 chat-artifact. WebChat is
    // the only caller that reads the second half — see
    // `channel_reply::build_guarded_reply_with_session`
    // (these replaced the former `_with_artifact` siblings).
    // B10 review L1: learn whether an empty answer is a
    // deliberate silence (no `no_reply` frame then).
    let work = crate::computer_use_sessions::attended::verified_webchat_message(
        sid,
        Some(auth_user.as_str()),
        crate::channel_reply::track_silence(
            crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(auth_user.clone(), async {
                match &effective_agent {
                    Some(a) => {
                        crate::channel_reply::build_guarded_reply_for_agent(
                            &full_content,
                            &state.ctx,
                            a,
                            sid,
                            &user_id,
                            Some(on_progress),
                        )
                        .await
                    }
                    None => {
                        crate::channel_reply::build_guarded_reply_with_session(
                            &full_content,
                            &state.ctx,
                            sid,
                            &user_id,
                            Some(on_progress),
                        )
                        .await
                    }
                }
            }),
        ),
    );
    let work = crate::computer_watch_links::delivery::webchat_scope(
        &state.ctx.home_dir,
        effective_agent_id,
        sid,
        !is_widget_visitor(&auth_user),
        watch_tx,
        work,
    );
    tokio::pin!(work);
    // The model the backend ACTUALLY answered with
    // (ProgressEvent::ModelInfo) — stamped onto the
    // assistant_done frame below.

    // Summary of the thinking frames actually sent,
    // stamped onto the assistant_done frame below.
    let mut thinking_meta = crate::thinking::DoneMetaBuilder::default();
    let mut thinking_visibility = super::progress::ThinkingVisibility::default();
    let (reply, deliberate_silence) = loop {
        thinking_visibility.flush_redaction(&progress, &mut sink).await;
        tokio::select! {
            r = &mut work => break r,
            Some(notice) = watch_rx.recv() => {
                let msg = ChatMessage::ComputerWatch {
                    agent_id: notice.agent_id, href: notice.href,
                    conv: conv_nonce.clone(),
                };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = sink.send(Message::Text(json.into())).await;
                }
            }
            ev = progress.recv() => {
                use crate::channel_reply::ProgressEvent;
                let content = ev.to_display();
                let msg = match ev {
                    ProgressEvent::ModelInfo { .. } => continue,
                    ProgressEvent::ToolUse { tool, detail } => ChatMessage::Progress {
                        content, kind: Some("tool".into()), tool: Some(tool), detail,
                        conv: conv_nonce.clone(),
                    },
                    ProgressEvent::TodoUpdate { .. } => ChatMessage::Progress {
                        content, kind: Some("todo".into()), tool: None, detail: None,
                        conv: conv_nonce.clone(),
                    },
                    ProgressEvent::Keepalive => ChatMessage::Progress {
                        content, kind: Some("keepalive".into()), tool: None, detail: None,
                        conv: conv_nonce.clone(),
                    },
                    ProgressEvent::Interim { .. } => ChatMessage::Progress {
                        content, kind: Some("status".into()), tool: None, detail: None,
                        conv: conv_nonce.clone(),
                    },
                    // Already gated (masked / truncated) by the
                    // turn's thinking sink; widget visitors get
                    // nothing at all.
                    ProgressEvent::Thinking(t) => {
                        let widget = is_widget_visitor(&auth_user);
                        if widget { continue; }
                        if !thinking_visibility.admit(&progress, &t.block_id) {
                            sink.progress_dropped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            continue;
                        }
                        thinking_meta.observe(&t);
                        match thinking_frame(t, conv_nonce.clone(), widget) {
                            Some(frame) => frame,
                            None => continue,
                        }
                    }
                    // C-P1: structured step boundary → dedicated `step` frame.
                    ProgressEvent::Step(step) => ChatMessage::Step {
                        phase: step.phase.as_str().to_string(),
                        tool: step.tool,
                        summary: step.summary,
                        depth: step.depth,
                        ts: step.ts_ms,
                        conv: conv_nonce.clone(),
                    },
                };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = sink.send(Message::Text(json.into())).await;
                }
            }
        }
    };
    let actual_model = progress.finish(&sink.progress_dropped);
    // No await may expose a completed reply to cancellation before this fence.
    if !ticket.begin_delivery() {
        reply.refuse_delivery();
        return;
    }
    if state.credential_validity(credential.as_ref(), true).await
        != crate::handlers::live_context::SocketValidity::Valid {
        reply.refuse_delivery();
        return;
    }
    thinking_visibility.flush_redaction(&progress, &mut sink).await;
    for notice in crate::computer_watch_links::delivery::take_pending(&mut watch_rx) {
        let msg = ChatMessage::ComputerWatch {
            agent_id: notice.agent_id,
            href: notice.href,
            conv: conv_nonce.clone(),
        };
        if let Ok(json) = serde_json::to_string(&msg) {
            let _ = sink.send(Message::Text(json.into())).await;
        }
    }
    // O-4→O-3: split off the artifact half early — every
    // transformation below (📎DELIVER parsing, cli-noise,
    // branding, etc.) only ever operates on the text; the
    // artifact rides along untouched to the final frame.
    let guarded = reply;
    let reply = guarded.text.clone();
    let operator_artifact = guarded.artifact.clone();
    let answered_by = guarded.answered_by.clone();
    let citations = (!guarded.citations.is_empty()).then(|| guarded.citations.clone());

    // WP1.3 + live-verification fix (2026-08-15):
    // 📎DELIVER: handling. WebChat has no in-band
    // binary frame, so the archival copy IS the
    // delivery — earlier code only validated the
    // path and appended a "downloadable from the
    // Files panel" note without archiving anything
    // (a false promise) and without running the
    // WP-4H delivery gate. Now each declared file
    // goes through the same channel-agnostic
    // stage_deliverable() as every other channel:
    // validate → delivery gate → archive + ledger.
    // Gate rejections surface as actionable text;
    // the download note is only made when a copy
    // really landed in attachments/.
    let mut owned_archives: Vec<PathBuf> = Vec::new();
    let reply = {
        let (cleaned, paths) = crate::office_docs::parse_deliverables(&reply);
        if paths.is_empty() {
            reply
        } else {
            let agent_dir = state.ctx.home_dir.join("agents").join(effective_agent_id);
            let mut delivered: Vec<String> = Vec::new();
            let mut failed: Vec<String> = Vec::new();
            // `office_docs::DeliveryCheck` is a sync
            // `dyn Fn() -> bool`; the awaited rechecks
            // around this loop carry the rollback.
            let delivery_valid = || guarded.still_valid_blocking();
            for p in &paths {
                if !guarded.still_valid().await {
                    break;
                }
                match stage_owned_webchat_deliverable(
                    &auth_user,
                    p,
                    &agent_dir,
                    &state.ctx.home_dir,
                    Some(&delivery_valid),
                )
                .await
                {
                    Ok(staged) if !guarded.still_valid().await => {
                        staged.rollback_new_archive().await;
                        break;
                    }
                    Ok(staged) if staged.archived => {
                        if let Some(path) = staged.owned_archive_path() {
                            owned_archives.push(path.to_path_buf());
                        }
                        delivered.push(staged.filename)
                    }
                    Ok(staged) => failed.push(format!(
                        "{}：封存副本寫入失敗，檔案仍在工作目錄",
                        staged.filename
                    )),
                    Err(e) => {
                        // The gate/validator message is
                        // already zh-TW actionable; the
                        // raw path never echoes here.
                        let name = std::path::Path::new(p)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("檔案");
                        failed.push(format!("{name}：{e}"));
                    }
                }
            }
            if !guarded.still_valid().await {
                rollback_owned_webchat_archives(&owned_archives).await;
                owned_archives.clear();
                String::new()
            } else {
                let mut notes: Vec<String> = Vec::new();
                if !delivered.is_empty() {
                    notes.push(format!(
                        "📎 已生成檔案：{}（可至 Dashboard 檔案面板下載）",
                        delivered.join("、")
                    ));
                }
                for f in &failed {
                    notes.push(format!("⚠️ 交付被攔下——{f}"));
                }
                if notes.is_empty() {
                    cleaned
                } else if cleaned.is_empty() {
                    notes.join("\n")
                } else {
                    format!("{cleaned}\n\n{}", notes.join("\n"))
                }
            }
        }
    };

    // Guard: don't send empty replies
    if !guarded.still_valid().await {
        rollback_owned_webchat_archives(&owned_archives).await;
        warn!("WebChat: source changed before reply delivery — skipping send");
        return;
    }
    if reply.trim().is_empty() && deliberate_silence {
        // Kept silent on purpose (failsafe mute, breaker,
        // access gate): no frame, as before (review L1).
        guarded.refuse_delivery();
        rollback_owned_webchat_archives(&owned_archives).await;
        warn!("WebChat: reply withheld by design — skipping send");
        return;
    }
    if reply.trim().is_empty() {
        // B10: an explicit frame, so the page and the
        // widget stop waiting for this message (the
        // dashboard keeps its input locked until a
        // reply or an error arrives).
        warn!("WebChat: reply is empty — telling the client");
        let frame = ChatMessage::error_coded(NO_REPLY_TEXT, NO_REPLY_CODE);
        if let Ok(json) = serde_json::to_string(&frame) {
            let _ = send_terminal(&state, credential.as_ref(), &mut sink, &guarded, &owned_archives, Message::Text(json.into())).await;
        } else {
            guarded.refuse_delivery();
            rollback_owned_webchat_archives(&owned_archives).await;
        }
        return;
    }

    let tokens = crate::cost_telemetry::estimate_tokens(&reply) as u32;
    let done = ChatMessage::AssistantDone {
        content: reply,
        tokens_used: tokens,
        conv: conv_nonce.clone(),
        model: actual_model,
        artifact: operator_artifact,
        agent: answered_by.as_ref().map(|a| a.id.clone()),
        agent_name: answered_by.map(|a| a.display_name),
        citations,
        thinking: thinking_meta.build(),
        task: None,
    };
    if let Ok(json) = serde_json::to_string(&done) {
        if !guarded.still_valid().await {
            rollback_owned_webchat_archives(&owned_archives).await;
            return;
        }
        let send_result = send_terminal(&state, credential.as_ref(), &mut sink, &guarded, &owned_archives, Message::Text(json.into())).await;
        if !guarded.still_valid().await {
            rollback_owned_webchat_archives(&owned_archives).await;
            warn!("WebChat: source changed during reply delivery");
        }
        if send_result.is_err() {
            return;
        }
    } else {
        guarded.refuse_delivery();
        rollback_owned_webchat_archives(&owned_archives).await;
    }
}

/// The surrounding delivery-cleanup scope waits for row revocation before
/// releasing the session FIFO. A successful ack remains delivered on disconnect.
async fn send_terminal(
    state: &WebChatState,
    credential: Option<&duduclaw_auth::jwt::Claims>,
    sink: &mut TurnSink,
    guarded: &crate::channel_reply::GuardedReply,
    owned_archives: &[PathBuf],
    frame: Message,
) -> Result<(), ()> {
    let validity = state.credential_validity(credential, true).await;
    let mut recovery = crate::handlers::live_context::SocketRecovery::default();
    if !rollback_before_final_reply(validity, &mut recovery, owned_archives).await {
        guarded.refuse_delivery();
        return Err(());
    }
    let result = guarded.confirm_send(sink.send(frame)).await;
    if result.is_err() {
        rollback_owned_webchat_archives(owned_archives).await;
    }
    result
}
