use super::*;

/// Legacy string builder for a specific named agent. A reply that used CCR
/// source data is conservatively refused because this API cannot retain its
/// source lease through the channel send.
///
/// Instead of reading `default_agent` from config.toml, this directly resolves
/// the agent by `agent_name` in the registry.
pub async fn build_reply_for_agent(
    text: &str,
    ctx: &ReplyContext,
    agent_name: &str,
    session_id: &str,
    user_id: &str,
    on_progress: Option<ProgressCallback>,
) -> String {
    build_guarded_reply_for_agent(text, ctx, agent_name, session_id, user_id, on_progress)
        .await
        .into_legacy_parts()
        .0
}

/// Task C (O-4 Guide-path result cards): run [`build_reply_with_session_inner`]
/// inside a fresh [`crate::runtime::NATIVE_TOOL_COLLECTOR`] scope and pair its
/// raw text with any read-only `os_*` result artifact captured during the
/// turn (`os_operator::extract_readonly_result_artifact`) — OR, per T1
/// (`commercial/docs/specs/DESIGN-agent-body-network-2026-08.md` §5.2/§12), a
/// `wifi_password_request` artifact when the turn's LATEST `os_wifi_connect`
/// call failed with `wrong_password`
/// (`os_operator::extract_wifi_password_request_artifact`). The password
/// prompt is tried FIRST — it is time-sensitive and actionable in a way a
/// generic status card is not, so it wins even if the same turn also called
/// a qualifying read-only tool (e.g. a status check made before the connect
/// attempt). Unconditional and cheap for every caller: a non-`system_operator`
/// agent's turn never populates the collector at all (the capture hook inside
/// `spawn_claude_cli_with_env` is itself capability-gated), so this costs one
/// empty `Vec` allocation and is otherwise behavior-neutral — the returned
/// artifact is `None` exactly as often as before this existed.
pub(super) async fn build_reply_with_session_inner_capturing_operator_result(
    text: &str,
    ctx: &ReplyContext,
    agent_override: Option<&str>,
    session_id: &str,
    user_id: &str,
    on_progress: Option<ProgressCallback>,
) -> (
    String,
    Option<serde_json::Value>,
    Option<Vec<duduclaw_llm::CcrDeliveryGuards>>,
    Arc<CcrTurnDelivery>,
    Option<crate::trigger_route::AnsweredBy>,
) {
    // P10: one model-free status line if this turn stays silent too long
    // (off by default; external channel sessions only).
    let (on_progress, interim) = super::interim::arm(
        &ctx.home_dir,
        match agent_override {
            _ if !super::interim::wanted(&ctx.home_dir, session_id) => None,
            Some(a) => Some(a.to_string()),
            None => Some(resolve_agent_for_restore(ctx, session_id).await),
        },
        session_id,
        on_progress,
    );
    // Thinking display: one sink per turn, installed AFTER the interim wrap
    // so the watcher sees `Thinking` events (and can say "thinking"). No
    // sink at all for public visitors.
    let (on_progress, thinking_sink) =
        arm_thinking(ctx, agent_override, session_id, on_progress).await;
    let collector: std::sync::Arc<std::sync::Mutex<Vec<crate::runtime::NativeToolEvent>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let ccr_collector = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let delivery_collector = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    // The turn's CCR delivery gate: the inner pipeline persists the assistant
    // message and schedules distillation BEFORE the lease is rechecked, so
    // both register here and are rolled back / never started on a refusal.
    let delivery = Arc::new(CcrTurnDelivery::default());
    let answered: Arc<std::sync::Mutex<Option<crate::trigger_route::AnsweredBy>>> =
        Arc::new(std::sync::Mutex::new(None));
    // Keep the authenticated channel caller and exact conversation in scope
    // for the whole turn. CCR history validation runs before the native tool
    // loop, and handle persistence runs after it; scoping only `cli_future`
    // leaves both paths (and local-first inference) without an identity.
    // Boxed: the reply future is large, and the interim watcher must not
    // add a second copy of it to the caller's stack.
    let turn = Box::pin(reply_identity_scope(
        session_id,
        user_id,
        CCR_TURN_DELIVERY.scope(
            delivery.clone(),
            crate::ccr_runtime::DELIVERY_GUARD_COLLECTOR.scope(
                delivery_collector.clone(),
                crate::ccr_runtime::SAVED_RESULT_COLLECTOR.scope(
                    ccr_collector,
                    crate::runtime::NATIVE_TOOL_COLLECTOR.scope(
                        collector.clone(),
                        crate::trigger_route::ANSWERED_BY.scope(
                            answered.clone(),
                            crate::thinking::scope_turn(
                                thinking_sink.clone(),
                                build_reply_with_session_inner(
                                    text,
                                    ctx,
                                    agent_override,
                                    session_id,
                                    user_id,
                                    on_progress,
                                ),
                            ),
                        ),
                    ),
                ),
            ),
        ),
    ));
    let raw = match interim {
        Some(w) => w.run(turn).await,
        None => turn.await,
    };
    let operator_result_artifact = collector.lock().ok().and_then(|events| {
        crate::os_operator::extract_wifi_password_request_artifact(&events)
            .or_else(|| crate::os_operator::extract_readonly_result_artifact(&events))
    });
    let guards = delivery_collector
        .lock()
        .ok()
        .map(|mut held| std::mem::take(&mut *held));
    let answered_by = answered.lock().ok().and_then(|mut a| a.take());
    finish_thinking(thinking_sink.as_deref(), ctx, agent_override, session_id, &answered_by).await;
    (raw, operator_result_artifact, guards, delivery, answered_by)
}

/// Build this turn's thinking sink (see `crate::thinking`). The agent is only
/// resolved for WebChat sessions — the only place its `show_thinking` and
/// contract are consulted for text.
async fn arm_thinking(
    ctx: &ReplyContext,
    agent_override: Option<&str>,
    session_id: &str,
    on_progress: Option<ProgressCallback>,
) -> (
    Option<ProgressCallback>,
    Option<Arc<crate::thinking::ThinkingSink>>,
) {
    if crate::odoo_livechat::is_public_visitor_session(session_id) {
        return (on_progress, None);
    }
    let widget_visitor = crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL
        .try_with(|p| crate::webchat::is_widget_visitor(p))
        .unwrap_or(false);
    let agent_id = if crate::thinking::policy::is_webchat_session(session_id) && !widget_visitor {
        match agent_override {
            Some(a) => a.to_string(),
            None => resolve_agent_for_restore(ctx, session_id).await,
        }
    } else {
        String::new()
    };
    let agent_dir = (!agent_id.is_empty()).then(|| ctx.home_dir.join("agents").join(&agent_id));
    let policy = crate::thinking::policy::resolve(
        &ctx.home_dir,
        agent_dir.as_deref(),
        session_id,
        widget_visitor,
    );
    let sink_ctx = crate::thinking::SinkCtx {
        home: ctx.home_dir.clone(),
        agent_id,
        redaction: crate::thinking::redaction_for_display(
            ctx.redaction_manager.as_ref(),
            &ctx.home_dir,
        ),
    };
    crate::thinking::install(policy, on_progress, sink_ctx)
}

/// End of turn: the per-turn `run_steps` thinking row (seconds and tokens),
/// attributed like the reply itself (a called employee's own session).
async fn finish_thinking(
    sink: Option<&crate::thinking::ThinkingSink>,
    ctx: &ReplyContext,
    agent_override: Option<&str>,
    session_id: &str,
    answered_by: &Option<crate::trigger_route::AnsweredBy>,
) {
    let Some(sink) = sink.filter(|s| s.saw_any()) else {
        return;
    };
    let (agent, session) = match answered_by {
        Some(who) => restore_target(who),
        None => match agent_override {
            Some(a) => (a.to_string(), session_id.to_string()),
            None => (
                resolve_agent_for_restore(ctx, session_id).await,
                session_id.to_string(),
            ),
        },
    };
    sink.finish(&agent, &session).await;
}

pub(crate) async fn reply_identity_scope<T>(
    session_id: &str,
    user_id: &str,
    future: impl std::future::Future<Output = T>,
) -> T {
    duduclaw_memory::feedback::CURRENT_SESSION_ID
        .scope(
            Some(session_id.to_owned()),
            crate::claude_runner::CHANNEL_REPLY_USER_ID.scope(user_id.to_owned(), future),
        )
        .await
}

/// Build an agent reply and retain its source leases through channel send.
pub async fn build_guarded_reply_for_agent(
    text: &str,
    ctx: &ReplyContext,
    agent_name: &str,
    session_id: &str,
    user_id: &str,
    on_progress: Option<ProgressCallback>,
) -> GuardedReply {
    let (raw, operator_result_artifact, guards, delivery, answered_by) =
        build_reply_with_session_inner_capturing_operator_result(
            text,
            ctx,
            Some(agent_name),
            session_id,
            user_id,
            on_progress,
        )
        .await;
    let Some(guards) = guards else {
        delivery.settle(&ctx.session_manager, false).await;
        return GuardedReply::refused();
    };
    let (raw, pending_artifact) = strip_operator_pending_marker(&raw);
    let artifact = pending_artifact.or(operator_result_artifact);
    let raw = crate::cli_noise::strip_cli_noise(&raw).text;
    let (agent_id, turn_session) = match &answered_by {
        Some(who) => restore_target(who),
        None => (agent_name.to_string(), session_id.to_string()),
    };
    let restored = restore_for_channel(raw, ctx, &agent_id, &turn_session).await;
    let enforced = enforce_contract(restored, &ctx.home_dir, &agent_id).await;
    // The "rules changed / model switched" FYI is for the operator; a public
    // website visitor must not see it (it stays pending for the next
    // operator-facing reply).
    let final_text = if crate::odoo_livechat::is_public_visitor_session(session_id) {
        enforced
    } else {
        append_pending_agent_notice(enforced, &ctx.home_dir, agent_name)
    };
    attach_turn_rollback(final_text, artifact, guards, delivery, ctx)
        .await
        .with_answered_by(answered_by)
}

/// Hand the turn's rollback handle to the reply when (and only when) the
/// answer holds a CCR lease. A lease-free turn has nothing a later revocation
/// could undo, so it publishes the delivery verdict here exactly as before —
/// keeping the overwhelming majority of replies byte-identical.
pub(super) async fn attach_turn_rollback(
    text: String,
    artifact: Option<serde_json::Value>,
    guards: Vec<duduclaw_llm::CcrDeliveryGuards>,
    delivery: Arc<CcrTurnDelivery>,
    ctx: &ReplyContext,
) -> GuardedReply {
    if guards
        .iter()
        .all(duduclaw_llm::CcrDeliveryGuards::is_empty)
    {
        delivery.settle(&ctx.session_manager, true).await;
        return GuardedReply::new(text, artifact, guards, None).await;
    }
    let rollback = TurnRollback {
        delivery,
        session_mgr: ctx.session_manager.clone(),
    };
    GuardedReply::new(text, artifact, guards, Some(rollback)).await
}

/// Build a session reply and retain its source leases through channel send.
///
/// `user_id` should be the stable per-user identifier from the channel
/// (e.g., the sender's account id — never a shared chat/room id, which would
/// merge different people into one CCR retrieval scope). This also feeds the
/// prediction engine's per-user statistical models.
pub async fn build_guarded_reply_with_session(
    text: &str,
    ctx: &ReplyContext,
    session_id: &str,
    user_id: &str,
    on_progress: Option<ProgressCallback>,
) -> GuardedReply {
    let (raw, operator_result_artifact, guards, delivery, answered_by) =
        build_reply_with_session_inner_capturing_operator_result(
            text,
            ctx,
            None,
            session_id,
            user_id,
            on_progress,
        )
        .await;
    let Some(guards) = guards else {
        delivery.settle(&ctx.session_manager, false).await;
        return GuardedReply::refused();
    };
    let (raw, pending_artifact) = strip_operator_pending_marker(&raw);
    let artifact = pending_artifact.or(operator_result_artifact);
    // WP11-A: last-line-of-defence filter for AI-runtime internal messages
    // (CLI TUI chrome, `CLAUDE_CODE_*` operator hints, paste/mode markers).
    // Placed here so every channel that funnels through `build_reply_*` is
    // covered, not just the one that reported the leak. See `cli_noise`.
    let raw = crate::cli_noise::strip_cli_noise(&raw).text;
    // Restore and contract-check as the employee that actually answered (an
    // `@`-called employee, not the default one).
    let (agent_id, turn_session) = match &answered_by {
        Some(who) => restore_target(who),
        None => (resolve_agent_for_restore(ctx, session_id).await, session_id.to_string()),
    };
    let restored = restore_for_channel(raw, ctx, &agent_id, &turn_session).await;
    let enforced = enforce_contract(restored, &ctx.home_dir, &agent_id).await;
    let with_notice = append_pending_agent_notice(enforced, &ctx.home_dir, &agent_id);
    // Messaging apps: label replies from a called (non-default) employee.
    let labelled = match &answered_by {
        Some(who) if who.via_trigger && footer_applies_to_session(session_id) => {
            crate::trigger_route::with_reply_prefix(with_notice, &who.display_name)
        }
        _ => with_notice,
    };
    let final_text = append_product_footer(labelled, session_id);
    attach_turn_rollback(final_text, artifact, guards, delivery, ctx)
        .await
        .with_answered_by(answered_by)
}

/// Fixed product attribution on external messaging replies. Internal sessions,
/// the dashboard console and deliberate silence remain unchanged.
pub(super) const PRODUCT_FOOTER: &str = "— Powered by DuDuClaw";

/// Session prefixes that reach end customers (external messaging platforms).
pub(super) const FOOTER_CHANNELS: &[&str] = &[
    "telegram",
    "discord",
    "slack",
    "line",
    "whatsapp",
    "feishu",
    "googlechat",
    "teams",
    "wecom",
    "dingtalk",
];

pub(super) fn footer_applies_to_session(session_id: &str) -> bool {
    FOOTER_CHANNELS.iter().any(|c| {
        session_id
            .strip_prefix(c)
            .is_some_and(|rest| rest.starts_with(':'))
    })
}

pub(super) fn append_product_footer(reply: String, session_id: &str) -> String {
    if reply.is_empty() || !footer_applies_to_session(session_id) {
        return reply;
    }
    format!("{reply}\n\n{PRODUCT_FOOTER}")
}

/// The employee and session a reply's secrets are restored and its contract
/// checked with: those of the turn that produced it (a called employee runs
/// in its own session bucket).
pub(super) fn restore_target(who: &crate::trigger_route::AnsweredBy) -> (String, String) {
    (who.id.clone(), who.session_id.clone())
}

#[cfg(test)]
pub(crate) fn restore_target_for_test(who: &crate::trigger_route::AnsweredBy) -> (String, String) {
    restore_target(who)
}
