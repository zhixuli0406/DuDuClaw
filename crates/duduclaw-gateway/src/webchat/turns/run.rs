use super::*;

pub(in crate::webchat) struct TurnInput {
    pub state: Arc<WebChatState>,
    pub auth_user: String,
    pub credential: Option<crate::webchat::ChatCredential>,
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

struct RefuseCancelledDelivery<'a> {
    reply: &'a crate::channel_reply::GuardedReply,
    cancel: &'a CancellationToken,
}

impl Drop for RefuseCancelledDelivery<'_> {
    fn drop(&mut self) {
        if self.cancel.is_cancelled() { self.reply.refuse_delivery(); }
    }
}

/// The access verdict for a WebChat chat command: the fail-closed command
/// gate on the same session and sender a normal message is gated on.
/// `None` = may run; `Some("")` = silence; `Some(text)` = the same pairing
/// hint / `/pair` verdict a normal message would get (no other wording, so a
/// public widget visitor learns nothing about the policy).
pub(super) async fn webchat_command_gate(
    ctx: &crate::channel_reply::ReplyContext,
    sid: &str,
    user_id: &str,
    content: &str,
) -> Option<String> {
    crate::channel_reply::check_command_access_gate(ctx, sid, user_id, content).await
}

#[cfg(test)]
#[path = "run_command_gate_tests.rs"]
mod command_gate_tests;

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
        mut attachments,
        assign_event,
        resume_acl: _,
        arrival_seq,
    } = input;
    if !credential_allows_turn(
        state.chat_credential_validity(credential.as_ref(), true).await, &sink,
    ).await {
        return;
    }
    let sid = sid.as_str();
    let effective_agent_id = effective_agent.as_deref().unwrap_or(&agent_id);
    // `None`: no employee is named and none resolves — the no-responder
    // notice, authorised without a target (re-decided here, fail closed).
    let target = (!state.no_responder_turn(effective_agent_id, sid, &user_id).await).then_some(effective_agent_id);
    if let Err(frame) = state.authorize_turn(&auth_user, target, true).await {
        sink.frame(frame).await;
        return;
    }
    // Recheck after the FIFO wait, before attachments or accepted work.
    if let Err(text) = crate::webchat::entry::check(
        &state, &auth_user, effective_agent_id, sid, &user_id,
        &content, !attachments.is_empty(), &ticket.id,
    ).await {
        sink.frame(ChatMessage::error(&text)).await;
        if attachments.is_empty() || !crate::webchat::entry::continues_without_files(&content) { return; }
        attachments.clear();
    }
    // Persist any uploaded files and append path references
    // to the prompt — the agent (Claude CLI) reads them from
    // disk, exactly as channel attachments are handled.
    let mut full_content = content.clone();
    let mut originals: Vec<(String, String)> = if ticket.merged.is_empty() { Vec::new() } else {
        ticket.originals.iter().map(|(id, text, _)| (id.clone(), text.clone())).collect()
    };
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
        let refs = match refs {
            Ok(refs) => refs,
            Err(_) => {
                sink.frame(ChatMessage::error_coded(
                    "附件無法上傳，請減少檔案數量或大小，或稍後再試。", "attachment_upload_refused",
                )).await;
                return;
            }
        };
        if !refs.is_empty() {
            let mut references = refs.iter();
            for ((_, text), (_, _, count)) in originals.iter_mut().zip(&ticket.originals) {
                let own_refs = references.by_ref().take(*count).cloned().collect::<Vec<_>>().join("\n");
                if !own_refs.is_empty() {
                    if !text.trim().is_empty() { text.push_str("\n\n"); }
                    text.push_str(&own_refs);
                }
            }
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
        // The access gate a normal message meets at the top of the reply
        // pipeline, on the same `sid` / `user_id`, before any command
        // handling. Credential, target ACL and admin checks stay separate,
        // later restrictions.
        if let Some(gate_reply) = webchat_command_gate(&state.ctx, sid, &user_id, &content).await {
            if gate_reply.trim().is_empty() {
                // Kept silent on purpose, exactly like a gated message.
                return;
            }
            let done = ChatMessage::AssistantDone {
                content: gate_reply,
                tokens_used: 0,
                conv: conv_nonce.clone(),
                model: None,
                artifact: None,
                agent: None,
                agent_name: None,
                citations: None,
                thinking: None,
                task: None,
            };
            if let Ok(json) = serde_json::to_string(&done) {
                let _ = sink.send(Message::Text(json.into())).await;
            }
            return;
        }
        if let Some(mut cmd) = crate::chat_commands::parse_command(&content, None) {
            if let Err(frame) = state.authorize_command(&auth_user, &mut cmd).await {
                sink.frame(frame).await;
                return;
            }
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
                    let r = principal.scope(auth_user.clone(), Box::pin(crate::trigger_route::assign::run(
                        &state.ctx, assign_cmd, &ac,
                    ))).await;
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
    #[cfg(target_os = "linux")]
    let work = crate::isolation_mcp::ingress::webchat_scope(credential.clone(), work);
    let work = crate::computer_watch_links::delivery::webchat_scope(
        &state.ctx.home_dir,
        effective_agent_id,
        sid,
        !is_widget_visitor(&auth_user),
        watch_tx,
        work,
    );
    let (accepted_tx, mut accepted_rx) = mpsc::channel(1);
    let delivery = crate::channel_reply::WebChatTurnDelivery {
        state: state.clone(), user: auth_user.clone(), client_msg_id: sink.msg_id.clone(), accepted: accepted_tx,
        refused: Arc::new(AtomicBool::new(false)),
    };
    let work = crate::channel_reply::WEBCHAT_ORIGINAL_MESSAGES.scope(originals,
        crate::channel_reply::WEBCHAT_TURN_DELIVERY.scope(delivery.clone(), work));
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
            Some((stored_sid, id)) = accepted_rx.recv() => {
                sink.value(serde_json::json!({"type":"turn_accepted", "user_row_id":id, "session_id":stored_sid})).await;
            }
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
    if let Ok((stored_sid, id)) = accepted_rx.try_recv() {
        sink.value(serde_json::json!({"type":"turn_accepted", "user_row_id":id, "session_id":stored_sid})).await;
    }
    let actual_model = progress.finish(&sink.progress_dropped);
    let guarded = reply;
    // Runs before GuardedReply::drop even when cancellation drops an awaited
    // credential/commit future after begin_delivery has fenced ordinary cancel.
    let _cancel_delivery = RefuseCancelledDelivery { reply: &guarded, cancel: &ticket.delivery_cancel };
    // No await may expose a completed reply to cancellation before this fence.
    if !ticket.begin_delivery() {
        guarded.refuse_delivery();
        return;
    }
    let validity = state.chat_credential_validity(credential.as_ref(), true).await;
    if validity != crate::handlers::live_context::SocketValidity::Valid {
        guarded.refuse_delivery();
        sink.identity_unavailable(validity, false).await;
        return;
    }
    // Only a turn authorised without a target, answering with the fixed
    // no-responder notice, keeps the target-less re-check at delivery.
    let answering_agent = match &guarded.answered_by {
        Some(a) => Some(a.id.as_str()),
        None if target.is_none() && guarded.no_responder_notice() => None,
        None => Some(effective_agent_id),
    };
    if delivery.refused.load(Ordering::SeqCst)
        || state.authorize_turn(&auth_user, answering_agent, true).await.is_err() {
        guarded.refuse_delivery();
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
        persist_failure(&state.ctx.session_manager, &state.ctx.home_dir, sid, &ticket.id, crate::channel_reply::TurnFailureReason::Internal).await;
        let frame = ChatMessage::error_coded(NO_REPLY_TEXT, NO_REPLY_CODE);
        if let Ok(json) = serde_json::to_string(&frame) {
            let mut withheld = Vec::new();
            let sent = send_terminal(&state, &auth_user, answering_agent, sid, &ticket.delivery_cancel, credential.as_ref(), &mut sink, &guarded, &owned_archives, Message::Text(json.into()), &mut withheld).await;
            // An empty reply has no row to publish, so `Notice` alone does not
            // mean the frame was lost. Only destinations it did not reach get
            // the notice: one terminal frame per destination. Still one failure
            // row: the notice records under this turn's id, and
            // `record_turn_failure` keeps the first row of a turn.
            if sent == Err(Undelivered::Notice) && !withheld.is_empty() {
                notify_destinations(&state, answering_agent, sid, &sink.msg_id, &ticket.id,
                    &ticket.delivery_cancel, || withheld).await;
            }
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
        let send_result = send_terminal(&state, &auth_user, answering_agent, sid, &ticket.delivery_cancel, credential.as_ref(), &mut sink, &guarded, &owned_archives, Message::Text(json.into()), &mut Vec::new()).await;
        if send_result == Err(Undelivered::Notice) {
            notify_undelivered(&state, &auth_user, answering_agent, sid, credential.as_ref(), &sink.msg_id, &ticket.id,
                &ticket.delivery_cancel).await;
        }
        if send_result.is_err() {
            return;
        }
    } else {
        guarded.refuse_delivery();
        rollback_owned_webchat_archives(&owned_archives).await;
    }
}

/// Machine code of the failure notice sent when a finished reply could not
/// be delivered; the message is the fixed turn-failure wording.
const REPLY_UNDELIVERED_CODE: &str = "reply_undelivered";

/// Why `send_terminal` did not deliver the reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Undelivered {
    /// Cancellation, credential or access refusal. Those paths already send
    /// their own terminal (cancellation frame, identity notice) or must stay
    /// silent towards an account that lost access.
    Silent,
    /// A final delivery fence refused: wait budget exhausted, ACL generation
    /// or policy changed, source lease lost, or a store failed.
    Notice,
}

/// The client must not be left waiting for a reply that a final fence
/// refused. Send the fixed turn-failure notice (no reply, no operator text;
/// the generic wording, which is also what widget visitors always get) and
/// record the same notice in history, where the withdrawn reply is hidden.
/// The frame passes the same writer fences as a reply, with a content-free
/// source fence bound to the turn's cancellation. If the account's access to
/// this employee really changed, the writer refuses it and the client stays
/// silent: correct, because nothing may be delivered to an account without
/// access. A cancelled turn records and sends no notice at all.
///
/// A frame the writer withheld only because users.db stayed busy past the
/// wait budget wrote no byte, so it is retried for that destination alone,
/// with an awaited backoff; attempts start only within `NOTICE_RETRY_WINDOW`.
/// Each attempt has its own fence: only that frame's own contention leads to
/// another attempt, so a destination never receives the notice twice. An
/// attempt is never cut short by the window: it ends with the socket writer's
/// answer (or its frame's own `SEND_TIMEOUT`), so a frame withheld at the
/// window's end is still told apart from a lost ack and its give-up is logged.
/// The retries run inside the turn, which still holds its conversation's FIFO
/// ticket for at most `NOTICE_HOLD_LIMIT`, so no later turn of this
/// conversation can send a frame before the notice is settled; frames of other
/// conversations carry their own `session_id` and `msg_id`.
pub(super) async fn notify_undelivered(
    state: &WebChatState, auth_user: &str, agent: Option<&str>, sid: &str,
    credential: Option<&crate::webchat::ChatCredential>, msg_id: &str, turn: &str,
    cancel: &CancellationToken,
) {
    notify_destinations(state, agent, sid, msg_id, turn, cancel, || state.subscriptions.sinks(auth_user, sid, msg_id,
        agent, credential, crate::channel_reply::ReplyDeliveryFence::content_free().for_turn(cancel))).await;
}

/// [`notify_undelivered`] towards `destinations` only (each attempt replaces
/// a destination's source fence with a content-free one of its own).
async fn notify_destinations(
    state: &WebChatState, agent: Option<&str>, sid: &str, msg_id: &str, turn: &str,
    cancel: &CancellationToken, destinations: impl FnOnce() -> Vec<TurnSink>,
) {
    if cancel.is_cancelled() { return; }
    let reason = crate::channel_reply::TurnFailureReason::Unknown;
    persist_failure(&state.ctx.session_manager, &state.ctx.home_dir, sid, turn, reason).await;
    let language = match agent {
        Some(agent) => crate::prompt_identity::resolve_fixed_message_language_for_id(&state.ctx.home_dir, agent).await,
        None => None,
    };
    let Ok(notice) = serde_json::to_string(&ChatMessage::error_coded(reason.render(language.as_deref(), turn, crate::channel_reply::assembly::public_session(sid)), REPLY_UNDELIVERED_CODE))
        else { return; };
    let fence = || crate::channel_reply::ReplyDeliveryFence::content_free().for_turn(cancel);
    let deadline = tokio::time::Instant::now() + NOTICE_RETRY_WINDOW;
    let mut backoff = NOTICE_RETRY_FIRST_BACKOFF;
    let mut pending = destinations();
    loop {
        let mut contended = Vec::new();
        for destination in pending {
            if cancel.is_cancelled() { return; }
            let source = fence();
            let mut attempt = destination.clone().with_source(source.clone());
            // Not bounded by `deadline`: dropping an attempt before the writer
            // answers would read a withheld frame as a lost ack and end the
            // retries silently. `TurnSink::send` bounds it by its own frame
            // deadline, and the writer latches contention before answering.
            let sent = attempt.send(Message::Text(notice.clone().into())).await;
            // Refusal, closed socket, cancellation or a lost ack: never again.
            if sent.is_err() && source.was_contended() {
                #[cfg(test)]
                withheld_notice_attempts().lock().unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push((sid.to_string(), msg_id.to_string()));
                contended.push(destination);
            }
        }
        if contended.is_empty() { return; }
        let resume = (tokio::time::Instant::now() + backoff).min(deadline);
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep_until(resume) => {}
        }
        if tokio::time::Instant::now() >= deadline {
            // History already holds the notice; the client sees it on reload.
            warn!(session_id = %sid, msg_id = %msg_id, destinations = contended.len(),
                "WebChat failure notice not delivered: account database stayed busy");
            #[cfg(test)]
            abandoned_notices().lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(sid.to_string());
            return;
        }
        backoff = (backoff * 2).min(NOTICE_RETRY_MAX_BACKOFF);
        pending = contended;
    }
}

/// Time within which attempts of the failure notice frame may start while
/// users.db stays busy: WebChat's terminal send deadline.
pub(super) const NOTICE_RETRY_WINDOW: std::time::Duration = super::SEND_TIMEOUT;
/// Longest one notice attempt runs: its frame's own deadline, which
/// `TurnSink::send` sets and the socket writer enforces.
const NOTICE_ATTEMPT_LIMIT: std::time::Duration = super::SEND_TIMEOUT;
/// Longest the notice holds the turn (and so its conversation's FIFO): an
/// attempt that started inside the window runs to its writer's answer. In
/// practice an attempt withheld for contention ends one ACL wait budget
/// (250 ms) after it reached the writer. Tests bound their waits by it.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) const NOTICE_HOLD_LIMIT: std::time::Duration = NOTICE_RETRY_WINDOW.saturating_add(NOTICE_ATTEMPT_LIMIT);
/// First pause between notice attempts; doubled after every contended attempt.
const NOTICE_RETRY_FIRST_BACKOFF: std::time::Duration = std::time::Duration::from_millis(250);
/// Longest pause between notice attempts.
const NOTICE_RETRY_MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(2);

/// Sessions whose notice frame was given up for a busy database (one entry
/// per give-up, i.e. per WARN). Server-spawned turn tasks log to no
/// test-local subscriber, so tests observe the give-up here.
#[cfg(test)]
pub(super) fn abandoned_notices() -> &'static std::sync::Mutex<Vec<String>> {
    static ABANDONED: std::sync::OnceLock<std::sync::Mutex<Vec<String>>> = std::sync::OnceLock::new();
    ABANDONED.get_or_init(Default::default)
}

/// `(session, msg_id)` of every notice attempt the writer withheld for a busy
/// database (one entry per withheld attempt), so tests wait for an attempt
/// to meet the busy database instead of sleeping for it.
#[cfg(test)]
pub(super) fn withheld_notice_attempts() -> &'static std::sync::Mutex<Vec<(String, String)>> {
    static WITHHELD: std::sync::OnceLock<std::sync::Mutex<Vec<(String, String)>>> = std::sync::OnceLock::new();
    WITHHELD.get_or_init(Default::default)
}

/// The surrounding delivery-cleanup scope waits for row revocation before
/// releasing the session FIFO. Transport loss alone keeps the persisted reply.
/// `withheld` receives every destination this frame did not reach because
/// its writer withheld it for a busy users.db, or because fan-out stopped
/// before it was tried; refused, closed or unanswered destinations are not in it.
#[allow(clippy::too_many_arguments)]
async fn send_terminal(
    state: &WebChatState,
    auth_user: &str,
    answering_agent: Option<&str>,
    sid: &str,
    cancel: &CancellationToken,
    credential: Option<&crate::webchat::ChatCredential>,
    sink: &mut TurnSink,
    guarded: &crate::channel_reply::GuardedReply,
    owned_archives: &[PathBuf],
    frame: Message,
    withheld: &mut Vec<TurnSink>,
) -> Result<(), Undelivered> {
    if cancel.is_cancelled() || state.authorize_turn(auth_user, answering_agent, true).await.is_err() {
        guarded.refuse_delivery();
        rollback_owned_webchat_archives(owned_archives).await;
        return Err(Undelivered::Silent);
    }
    let validity = state.chat_credential_validity(credential, true).await;
    let mut recovery = crate::handlers::live_context::SocketRecovery::default();
    if cancel.is_cancelled() || state.authorize_turn(auth_user, answering_agent, true).await.is_err()
        || !rollback_before_final_reply(validity, &mut recovery, owned_archives).await {
        guarded.refuse_delivery();
        rollback_owned_webchat_archives(owned_archives).await;
        sink.identity_unavailable(validity, false).await;
        return Err(Undelivered::Silent);
    }
    let terminal = match &frame {
        Message::Text(text) => serde_json::from_str::<serde_json::Value>(text).ok(),
        _ => None,
    };
    let content = terminal.as_ref().and_then(|value| value.get("content"))
        .and_then(|v| v.as_str()).map(str::to_owned);
    let actual_model = terminal.as_ref().filter(|value| value["type"] == "assistant_done")
        .and_then(|value| value.get("model")).and_then(|v| v.as_str());
    let has_reply = content.is_some();
    if let Some(content) = content {
        if guarded.persist_webchat(&state.ctx.session_manager, sid, &content).await.is_err() {
            rollback_owned_webchat_archives(owned_archives).await;
            return Err(Undelivered::Notice);
        }
    }
    if state.authorize_turn(auth_user, answering_agent, true).await.is_err() {
        guarded.refuse_delivery();
        rollback_owned_webchat_archives(owned_archives).await;
        return Err(Undelivered::Silent);
    }
    // The pipeline staged a tombstoned row. Socket failure alone still permits
    // publication after fan-out; source or authority failure leaves it hidden.
    // A frame the writer withheld for an exhausted wait budget latches this
    // turn's source as refused, so the row is not published without it.
    let mut denied = false;
    let mut delivered = Vec::new();
    let mut destinations = state.subscriptions.sinks(auth_user, sid, &sink.msg_id, answering_agent, credential,
        guarded.delivery_fence().for_turn(cancel)).into_iter();
    while let Some(destination) = destinations.next() {
        if state.authorize_turn(auth_user, answering_agent, true).await.is_err() {
            denied = true;
            guarded.refuse_delivery();
        }
        if !guarded.still_valid().await {
            rollback_owned_webchat_archives(owned_archives).await;
            if denied { return Err(Undelivered::Silent); }
            withheld.push(destination);
            withheld.extend(destinations);
            return Err(Undelivered::Notice);
        }
        // A fence of its own over the reply's shared refusal latch, so only
        // this destination's own withheld frame marks it as withheld.
        let source = guarded.delivery_fence().for_turn(cancel);
        let mut attempt = destination.clone().with_source(source.clone());
        match attempt.send(frame.clone()).await {
            Ok(()) => delivered.push(attempt),
            Err(()) if source.was_contended() => withheld.push(destination),
            Err(()) => {}
        }
    }
    let users = state.user_db.clone();
    let generation = tokio::task::spawn_blocking(move || users.acl_generation()).await
        .ok().and_then(Result::ok);
    let validity = state.chat_credential_validity(credential, true).await;
    if cancel.is_cancelled() || state.authorize_turn(auth_user, answering_agent, true).await.is_err()
        || !rollback_before_final_reply(validity, &mut recovery, owned_archives).await {
        guarded.refuse_delivery();
        rollback_owned_webchat_archives(owned_archives).await;
        return Err(Undelivered::Silent);
    }
    let policy = state.turn_policy(auth_user, answering_agent, true).await;
    let published = match (generation, policy) {
        (Some(generation), Ok(policy)) => guarded.publish_webchat(&state.ctx.session_manager, sid,
            &state.user_db, generation, &policy, cancel).await,
        _ => { guarded.refuse_delivery(); Err(()) }
    };
    if published.is_err() { rollback_owned_webchat_archives(owned_archives).await; }
    if published.is_ok() {
        if has_reply { sink.complete(); }
        if let Some(model) = actual_model {
            let mut info = access::user_session_info(state, sid, answering_agent, auth_user).await;
            apply_executed_model(&mut info, model);
            if let Ok(json) = serde_json::to_string(&info) {
                // Only destinations that acknowledged the terminal get its
                // metadata. Reuse their fenced sender, including cancellation,
                // source lease, current ACL and credential checks at the writer.
                for mut destination in delivered {
                    let _ = destination.send(Message::Text(json.clone().into())).await;
                }
            }
        }
    }
    published.map_err(|()| Undelivered::Notice)
}

/// Refresh metadata with the actual primary/fallback model reported by the turn.
fn apply_executed_model(info: &mut ChatMessage, actual: &str) {
    if let ChatMessage::SessionInfo { model, refresh, supports_vision, .. } = info {
        *model = actual.to_string();
        *refresh = true;
        let bare = actual.strip_suffix("（備援）").unwrap_or(actual);
        if let Some(provider) = crate::runtime_config::infer_provider_for_model(bare) {
            *supports_vision = crate::model_capabilities::supports_vision(provider, bare);
        }
    }
}

#[cfg(test)]
mod model_frame_tests {
    use super::*;

    /// Exercise the real terminal sender and its subscription acknowledgements.
    async fn delivered_frames(actual: &str, refused: bool, acknowledge: bool) -> Vec<serde_json::Value> {
        // The real turn supervisor holds delivery open through terminal publication.
        let (frames, cleanup) = crate::channel_reply::with_delivery_cleanup(async {
        let home = tempfile::tempdir().unwrap();
        crate::team_edition::record(home.path(), duduclaw_core::EditionProfile::Enterprise);
        std::fs::create_dir_all(home.path().join("agents")).unwrap();
        std::fs::write(home.path().join("config.toml"), "").unwrap();
        crate::mfa::reload_policy(home.path());
        let users = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
        let admin = users.create_user("model-frame@test.invalid", "Admin", "Test-Password-12345",
            duduclaw_auth::UserRole::Admin).unwrap();
        let state = Arc::new(WebChatState::new(
            crate::agent_channels::e2e_tests::reply_ctx(home.path()).await,
            Arc::new(JwtConfig::new(b"model-frame-test-key")), users,
        ));
        let sid = "webchat:model-frame";
        let guarded = crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(
            admin.id.clone(), crate::channel_reply::build_guarded_reply_with_session(
                "hello", &state.ctx, sid, &admin.id, None,
            ),
        ).await;
        assert!(guarded.no_responder_notice());
        if refused { guarded.refuse_delivery(); }
        let (tx, mut rx) = outbound_channel();
        let subscription = state.subscriptions.register(&admin.id, tx.clone());
        subscription.subscribe(sid, None);
        let mut sink = TurnSink::new(tx, sid, None, "model-frame");
        let frame = Message::Text(serde_json::json!({
            "type": "assistant_done", "content": guarded.text, "model": actual,
        }).to_string().into());
        let task = tokio::spawn(async move {
            let result = send_terminal(&state, &admin.id, None, sid, &CancellationToken::new(),
                None, &mut sink, &guarded, &[], frame, &mut Vec::new()).await;
            if refused { assert!(result.is_err()); } else { assert!(result.is_ok()); }
        });
        tokio::pin!(task);
        let mut frames = Vec::new();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                tokio::select! {
                    biased;
                    Some(output) = rx.recv() => {
                        assert!(output.route.is_some(), "metadata must use the fenced subscription");
                        if let Message::Text(text) = &output.message {
                            frames.push(serde_json::from_str::<serde_json::Value>(text).unwrap());
                        }
                        if let Some(ack) = output.ack { let _ = ack.send(acknowledge); }
                    }
                    result = &mut task => { result.unwrap(); break; }
                }
            }
        }).await.expect("terminal sender did not finish");
        frames
        }).await;
        cleanup.unwrap();
        frames
    }

    #[tokio::test]
    async fn per_turn_metadata_reports_executed_primary_and_fallback_models() {
        for actual in ["gpt-5.6-sol", "claude-opus-4-6（備援）"] {
            let frames = delivered_frames(actual, false, true).await;
            assert_eq!(frames.len(), 2, "{frames:?}");
            assert_eq!(frames[0]["type"], "assistant_done");
            assert_eq!(frames[0]["model"], actual);
            assert_eq!(frames[1]["type"], "session_info");
            assert_eq!(frames[1]["model"], actual);
            assert_eq!(frames[1]["refresh"], true);
        }
    }

    #[tokio::test]
    async fn no_session_info_update_when_terminal_delivery_is_refused() {
        assert!(delivered_frames("gpt-5.6-sol", true, true).await.is_empty());
        // Publication may succeed despite transport loss; that socket still
        // must not receive metadata for a terminal it did not acknowledge.
        let frames = delivered_frames("gpt-5.6-sol", false, false).await;
        assert_eq!(frames.len(), 1, "{frames:?}");
        assert_eq!(frames[0]["type"], "assistant_done");
    }
}

#[cfg(test)]
#[path = "run_entry_tests.rs"]
mod entry_tests;
