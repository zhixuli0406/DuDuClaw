use super::*;
use crate::approval::{ApprovalBroker, ApprovalRecord, ApprovalStatus, RequestKind};
use std::collections::HashSet;

pub(super) fn started(input: &RunInput) -> Value {
    let mut event = json!({"type":"RUN_STARTED", "threadId":input.thread_id,
        "runId":input.run_id, "protocolVersion":"1.0"});
    if let Some(parent) = &input.parent_run_id { event["parentRunId"] = json!(parent); }
    event
}
pub(super) fn finished(input: &RunInput, outcome: Value) -> Value {
    json!({"type":"RUN_FINISHED", "threadId":input.thread_id, "runId":input.run_id, "outcome":outcome})
}
pub(super) fn failure(code: &str) -> Value {
    let message = if code == "run_timeout" {
        "已達網頁對話的 15 分鐘執行上限。較長的工作請改為交辦背景任務。"
    } else { "無法繼續這次執行，請確認登入狀態、權限及核准要求。" };
    json!({"type":"RUN_ERROR", "code":code, "message":message})
}
pub(super) async fn reject(connection: Connection, code: &str) {
    if send(&connection, started(&connection.input)).await.is_ok() {
        let _ = send(&connection, failure(code)).await;
    }
}
pub(super) async fn replay(connection: Connection) {
    if send(&connection, started(&connection.input)).await.is_ok() {
        let _ = send(&connection, finished(&connection.input, json!({"type":"success"}))).await;
    }
}

pub(super) fn may_decide(record: &ApprovalRecord, admin: bool) -> bool {
    admin && record.request_kind == RequestKind::Approval && record.binding.is_none()
        && matches!(record.action_kind.as_str(), "mcp_call" | "mcp_install")
        && !crate::approval_notify::is_dashboard_only_record(None, record)
        && record.payload.get("scope_epoch").is_none()
}

pub(super) fn interrupt(record: &ApprovalRecord, admin: bool) -> Value {
    let mut value = json!({"id":record.id.as_str(), "reason":"confirmation",
        "message":"此操作需要核准。無權在此決定的要求，請至原核准介面處理。",
        "responseSchema":{"type":"object", "properties":{"approved":{"type":"boolean"}},
            "required":["approved"], "additionalProperties":false},
        "metadata":{"duduclaw:canDecide":may_decide(record, admin)}});
    if let Some(expires) = record.deadline_rfc3339() { value["expiresAt"] = json!(expires); }
    value
}

async fn authorized(state: &AgUiState, user: &str, agent: &str, connection: &Connection) -> bool {
    state.chat.credential_validity(Some(&connection.claims), true).await
        == crate::handlers::live_context::SocketValidity::Valid
        && state.chat.authorize_target(user, agent, false).await.is_ok()
        && state.chat.ctx.registry.read().await.get(agent).is_some_and(|a| a.config.agent.status.is_operational())
}

async fn pending(broker: &ApprovalBroker, sid: &str, turn: &str, accepted: &HashMap<String, ResumeEntry>) -> Result<Vec<ApprovalRecord>, ()> {
    let records = broker.records_for_turn(sid, turn).await.map_err(|_| ())?;
    Ok(records.into_iter().filter(|r| r.status == ApprovalStatus::Pending && !accepted.contains_key(r.id.as_str())).collect())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn decide_resume(
    state: &AgUiState, broker: &ApprovalBroker, user: &str, agent: &str,
    sid: &str, turn: &str, connection: &Connection, cards: &[ApprovalRecord],
    accepted: &mut HashMap<String, ResumeEntry>,
) -> Result<(), &'static str> {
    if !authorized(state, user, agent, connection).await { return Err("authorization_changed"); }
    let entries = &connection.input.resume;
    if entries.len() != cards.len() || cards.iter().any(|r| !entries.iter().any(|e| e.interrupt_id == r.id.as_str())) {
        return Err("incomplete_interrupt_coverage");
    }
    let records = broker.records_for_turn(sid, turn).await.map_err(|_| "approval_unavailable")?;
    let admin = webchat_command_admin(&state.chat.user_db, user);
    let principal = duduclaw_core::HumanPrincipal::user(user).map(|p| p.to_string());
    let mut decisions = Vec::with_capacity(cards.len());
    for card in cards {
        let current = records.iter().find(|r| r.id == card.id).ok_or("stale_interrupt")?;
        if current.agent_id != agent || !crate::approval::same_ag_ui_request(card, current) {
            return Err("stale_interrupt");
        }
        let entry = entries.iter().find(|e| e.interrupt_id == current.id.as_str()).ok_or("unknown_interrupt")?;
        let approve = entry.approved()?;
        if let Some(previous) = accepted.get(current.id.as_str()) {
            if previous.status != entry.status || previous.payload != entry.payload {
                return Err("resume_decision_changed");
            }
        }
        match current.status {
            ApprovalStatus::Approved | ApprovalStatus::Denied => {
                // Confirm the original card's external decision without acting
                // as its decider. Employee access still gates continuation.
                if approve != (current.status == ApprovalStatus::Approved) {
                    return Err("resume_decision_changed");
                }
            }
            ApprovalStatus::Pending => {
                if !may_decide(current, admin) { return Err("approval_forbidden"); }
                if current.is_stale(chrono::Utc::now()) { return Err("expired_interrupt"); }
                if crate::on_behalf_of::self_approval::refuses(&state.chat.ctx.home_dir,
                    current.on_behalf_of.as_deref(), principal.as_deref(), approve) {
                    return Err("self_approval_forbidden");
                }
            }
            _ => return Err("stale_interrupt"),
        }
        decisions.push((current.clone(), approve));
    }
    if !authorized(state, user, agent, connection).await
        || (decisions.iter().any(|(r, _)| r.status == ApprovalStatus::Pending)
            && !webchat_command_admin(&state.chat.user_db, user)) {
        return Err("authorization_changed");
    }
    broker.decide_ag_ui_batch(sid, turn, &decisions, &format!("ag-ui:{user}")).await
        .map_err(|_| "decision_conflict")?;
    for entry in entries { accepted.insert(entry.interrupt_id.clone(), entry.clone()); }
    Ok(())
}

async fn interrupts_valid(broker: &ApprovalBroker, sid: &str, turn: &str, cards: &[ApprovalRecord]) -> bool {
    let Ok(records) = broker.records_for_turn(sid, turn).await else { return false; };
    cards.iter().all(|card| records.iter().any(|current| current.id == card.id
        && crate::approval::same_ag_ui_request(card, current)
        && matches!(current.status, ApprovalStatus::Pending | ApprovalStatus::Approved | ApprovalStatus::Denied)
        && !current.is_stale(chrono::Utc::now())))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn start(
    state: Arc<AgUiState>, broker: ApprovalBroker, _lease: RunLease, user: String,
    agent: String, sid: String, turn: String, principal: String, text: String, connection: Connection,
    mut incoming: mpsc::Receiver<Connection>, waiting: Arc<AtomicBool>,
) {
    let work = execute(&state, &broker, &user, &agent, &sid, &turn, &text, connection, &mut incoming, &waiting);
    use futures_util::FutureExt;
    let outcome = std::panic::AssertUnwindSafe(crate::channel_reply::with_delivery_cleanup(
        tokio::time::timeout(RUN_LIFETIME + SEND_TIMEOUT + SEND_TIMEOUT, Box::pin(work)),
    )).catch_unwind().await;
    let success = matches!(&outcome, Ok((Ok(true), Ok(()))));
    let mut fenced = success;
    if !success {
        if let Err(error) = broker.withdraw_turn_for_principal(&sid, &turn, Some(&principal)).await {
            tracing::warn!(%error, "AG-UI cancellation fence failed");
        } else { fenced = true; }
    }
    if let Ok((_, Err(error))) = outcome { tracing::warn!(%error, "AG-UI delivery cleanup failed"); }
    if let Some(journal) = state.journal.as_ref().filter(|_| fenced) {
        if let Err(error) = journal.finish(&sid, &turn).await { tracing::warn!(%error, "AG-UI journal finish failed"); }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute(
    state: &AgUiState, broker: &ApprovalBroker, user: &str, agent: &str, sid: &str,
    turn: &str, text: &str, mut connection: Connection, incoming: &mut mpsc::Receiver<Connection>,
    waiting: &AtomicBool,
) -> bool {
    if send(&connection, started(&connection.input)).await.is_err() { return false; }
    if !authorized(state, user, agent, &connection).await { let _ = send(&connection, failure("authorization_changed")).await; return false; }
    let (progress_tx, mut progress_rx) = mpsc::channel(64);
    let overflow = Arc::new(AtomicBool::new(false));
    let overflow_callback = overflow.clone();
    let callback: crate::channel_reply::ProgressCallback = Box::new(move |event| {
        // Never stream reasoning, arguments, paths, interim text or tool output.
        if let crate::channel_reply::ProgressEvent::ToolUse { tool, .. } = event {
            if progress_tx.try_send(tool).is_err() { overflow_callback.store(true, Ordering::Release); }
        }
    });
    let caller = format!("webchat:{}:ag-ui", webchat_owner_tag(user));
    let pipeline = Box::pin(crate::channel_reply::build_guarded_reply_for_agent(text, &state.chat.ctx, agent, sid, &caller, Some(callback)));
    let pipeline = Box::pin(crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(user.to_string(), pipeline));
    let pipeline = Box::pin(super::super::turns::TURN_ID.scope(turn.to_string(), pipeline));
    // Use the same hidden assistant rows, live commit checks and durable
    // retraction path as WebChat, including early knowledge-only replies.
    let (accepted_tx, _accepted_rx) = mpsc::channel(1);
    let delivery = crate::channel_reply::WebChatTurnDelivery {
        state: state.chat.clone(), user: user.to_owned(), client_msg_id: turn.to_owned(), accepted: accepted_tx,
        refused: Arc::new(AtomicBool::new(false)),
    };
    let pipeline = crate::channel_reply::WEBCHAT_TURN_DELIVERY.scope(delivery.clone(), pipeline);
    let pipeline = crate::approval::scope_turn(sid, turn, crate::channel_reply::track_failure(pipeline));
    tokio::pin!(pipeline);
    let mut reply = None;
    let mut failed = false;
    let mut accepted = HashMap::new();
    let mut runs = HashSet::from([connection.input.run_id.clone()]);
    let mut receipt: Vec<ResumeEntry> = Vec::new();
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    let mut tool_seq = 0u64;
    let deadline = tokio::time::Instant::now() + RUN_LIFETIME;
    loop {
        let mut check_pending = false;
        tokio::select! {
            biased;
            _ = connection.output.closed() => return false,
            _ = tokio::time::sleep_until(deadline) => {
                let _ = send(&connection, failure("run_timeout")).await; return false;
            },
            _ = tick.tick() => {
                if !authorized(state, user, agent, &connection).await
                    || crate::approval::is_turn_cancelled(&state.chat.ctx.home_dir, sid, turn).await != Ok(false) {
                    let _ = send(&connection, failure("authorization_changed")).await;
                    return false;
                }
                check_pending = true;
            },
            result = &mut pipeline, if reply.is_none() => { reply = Some(result.0); failed = result.1; check_pending = true; },
            Some(tool) = progress_rx.recv(), if reply.is_none() => {
                if overflow.load(Ordering::Acquire) {
                    let _ = send(&connection, failure("progress_overflow")).await; return false;
                }
                if !authorized(state, user, agent, &connection).await { return false; }
                tool_seq += 1;
                let id = format!("{turn}:{tool_seq}");
                // TOOL_CALL_END closes the argument envelope, not execution.
                // This progress API has no raw arguments/results or call ids.
                if send(&connection, json!({"type":"TOOL_CALL_START", "toolCallId":id, "toolCallName":tool})).await.is_err()
                    || send(&connection, json!({"type":"TOOL_CALL_END", "toolCallId":id})).await.is_err() { return false; }
            }
        }
        if !check_pending { continue; }
        let mut cards = match pending(broker, sid, turn, &accepted).await {
            Ok(cards) => cards,
            Err(_) => { let _ = send(&connection, failure("approval_unavailable")).await; return false; }
        };
        if !cards.is_empty() {
            if cards.iter().any(|r| r.is_stale(chrono::Utc::now())) {
                let _ = send(&connection, failure("expired_interrupt")).await; return false;
            }
            let admin = webchat_command_admin(&state.chat.user_db, user);
            // Only host-owned state and this turn's text; never echo client
            // state, forwardedProps, forged history or approval payloads.
            if send(&connection, json!({"type":"STATE_SNAPSHOT", "snapshot":{}})).await.is_err()
                || send(&connection, json!({"type":"MESSAGES_SNAPSHOT", "messages":[{"id":turn,"role":"user","content":text}]})).await.is_err() { return false; }
            waiting.store(true, Ordering::Release);
            if send(&connection, finished(&connection.input, json!({"type":"interrupt",
                "interrupts":cards.iter().map(|r| interrupt(r, admin)).collect::<Vec<_>>()}))).await.is_err() { return false; }
            let (closed, receiver) = mpsc::channel(1);
            drop(receiver);
            drop(std::mem::replace(&mut connection.output, closed));
            loop {
                let next = tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => return false,
                    next = incoming.recv() => match next { Some(next) => next, None => return false },
                    _ = tick.tick() => {
                        if !authorized(state, user, agent, &connection).await
                            || !interrupts_valid(broker, sid, turn, &cards).await { return false; }
                        continue;
                    }
                };
                if runs.len() >= 256 { reject(next, "run_limit").await; return false; }
                if !runs.insert(next.input.run_id.clone()) {
                    reject(next, "run_id_reused").await; waiting.store(true, Ordering::Release); continue;
                }
                if send(&next, started(&next.input)).await.is_err() { return false; }
                // A runtime may have queued parallel approval requests just
                // before suspension. Publish the enlarged set before deciding any.
                let fresh = match pending(broker, sid, turn, &accepted).await {
                    Ok(cards) => cards,
                    Err(_) => { let _ = send(&next, failure("approval_unavailable")).await; return false; }
                };
                let mut changed = false;
                for card in fresh {
                    if !cards.iter().any(|old| old.id == card.id) { cards.push(card); changed = true; }
                }
                if changed {
                    let admin = webchat_command_admin(&state.chat.user_db, user);
                    waiting.store(true, Ordering::Release);
                    if send(&next, json!({"type":"STATE_SNAPSHOT", "snapshot":{}})).await.is_err()
                        || send(&next, json!({"type":"MESSAGES_SNAPSHOT", "messages":[{"id":turn,"role":"user","content":text}]})).await.is_err()
                        || send(&next, finished(&next.input, json!({"type":"interrupt",
                            "interrupts":cards.iter().map(|r| interrupt(r, admin)).collect::<Vec<_>>()}))).await.is_err() { return false; }
                    continue;
                }
                match decide_resume(state, broker, user, agent, sid, turn, &next, &cards, &mut accepted).await {
                    Ok(()) => { receipt = next.input.resume.clone(); connection = next; break; }
                    Err(code) => {
                        let _ = send(&next, failure(code)).await;
                        waiting.store(true, Ordering::Release);
                    }
                }
            }
        }
        if let Some(guarded) = reply.take() {
            if failed || overflow.load(Ordering::Acquire) {
                guarded.refuse_delivery();
                let _ = send(&connection, failure(if failed { "runtime_failed" } else { "progress_overflow" })).await;
                return false;
            }
            while let Ok(tool) = progress_rx.try_recv() {
                if !authorized(state, user, agent, &connection).await { guarded.refuse_delivery(); return false; }
                tool_seq += 1;
                let id = format!("{turn}:{tool_seq}");
                if send(&connection, json!({"type":"TOOL_CALL_START", "toolCallId":id, "toolCallName":tool})).await.is_err()
                    || send(&connection, json!({"type":"TOOL_CALL_END", "toolCallId":id})).await.is_err() {
                    guarded.refuse_delivery(); return false;
                }
            }
            if !authorized(state, user, agent, &connection).await || !guarded.still_valid().await || guarded.text.trim().is_empty()
                || crate::approval::is_turn_cancelled(&state.chat.ctx.home_dir, sid, turn).await != Ok(false) {
                guarded.refuse_delivery();
                let _ = send(&connection, failure("reply_unavailable")).await;
                return false;
            }
            let delivered = !delivery.refused.load(Ordering::SeqCst)
                && super::delivery::reply(state, user, agent, sid, turn, &connection, &guarded).await.is_ok();
            if !delivered { guarded.refuse_delivery(); }
            if delivered && !receipt.is_empty() {
                if let Ok(mut receipts) = state.receipts.lock() {
                    receipts.retain(|_, (at, _)| at.elapsed() < RUN_LIFETIME);
                    if receipts.len() >= 128 { receipts.clear(); }
                    receipts.insert(sid.to_string(), (std::time::Instant::now(), receipt));
                }
            }
            return delivered;
        }
    }
}
