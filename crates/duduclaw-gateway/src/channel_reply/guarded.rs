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
    Vec<duduclaw_knowledge::cite::CitationRef>,
    TurnBuildRollback,
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
        arm_thinking(ctx, agent_override, session_id, user_id, on_progress).await;
    let collector: std::sync::Arc<std::sync::Mutex<Vec<crate::runtime::NativeToolEvent>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let ccr_collector = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let delivery_collector = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    // The turn's CCR delivery gate: the inner pipeline persists the assistant
    // message and schedules distillation BEFORE the lease is rechecked, so
    // both register here and are rolled back / never started on a refusal.
    let delivery = Arc::new(CcrTurnDelivery::default());
    let build_rollback = TurnBuildRollback::new(delivery.clone(), ctx.session_manager.clone());
    let answered: Arc<std::sync::Mutex<Option<crate::trigger_route::AnsweredBy>>> =
        Arc::new(std::sync::Mutex::new(None));
    // Document knowledge base: attachments the adapter saved for this
    // message belong to this conversation and sender (first claim wins);
    // the person this turn is for (resolved once, before any runtime call)
    // and the sources the reply cites.
    crate::knowledge::chat_attachments::claim_in_text(session_id, user_id, text);
    let requester =
        crate::knowledge::requester_map::requester_for_reply(&ctx.home_dir, session_id, user_id)
            .await;
    // The same person, as the `on_behalf_of` every spawn of this turn
    // carries (`DUDUCLAW_ON_BEHALF_OF`), and every delegation or task the
    // employee starts from it inherits.
    let principal = crate::on_behalf_of::principal_for_requester(&ctx.home_dir, &requester);
    let cited: Arc<std::sync::Mutex<Vec<duduclaw_knowledge::cite::CitationRef>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    // Keep the authenticated channel caller and exact conversation in scope
    // for the whole turn. CCR history validation runs before the native tool
    // loop, and handle persistence runs after it; scoping only `cli_future`
    // leaves both paths (and local-first inference) without an identity.
    // Boxed twice, innermost first. In an unoptimised build every
    // task-local layer below keeps by-value copies of the future it wraps in
    // its own poll frame (construction temporaries, `Pin::set(None)` on
    // completion), so wrapping the ~10 KiB pipeline future directly cost
    // 20-40 KiB of stack per layer (about 600 KiB for the whole chain) and
    // overflowed a tokio worker's 2 MiB stack on a plain LINE message.
    // Wrapping a `Pin<Box<_>>` makes each layer pointer-sized. The outer box
    // keeps the interim watcher from adding another copy to the caller's
    // stack. `channel_reply/tests/future_size.rs` pins this shape.
    let pipeline = Box::pin(build_reply_with_session_inner(
        text,
        ctx,
        agent_override,
        session_id,
        user_id,
        on_progress,
    ));
    let pipeline = crate::autonomous_runtime::human_interactive(&ctx.home_dir, pipeline);
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
                                crate::knowledge::turn_requesters::CURRENT_REQUESTER.scope(
                                    requester,
                                    crate::knowledge::cite_turn::TURN_CITATIONS.scope(
                                        cited.clone(),
                                        pipeline,
                                    ),
                                ),
                            ),
                        ),
                    ),
                ),
            ),
        ),
    ));
    // Scope the already boxed turn, keeping every added layer pointer-sized
    // and preserving the innermost boxed reply pipeline.
    let turn = Box::pin(crate::on_behalf_of::scope_first(principal, turn));
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
    let cited = cited
        .lock()
        .map(|mut c| std::mem::take(&mut *c))
        .unwrap_or_default();
    (raw, operator_result_artifact, guards, delivery, answered_by, cited, build_rollback)
}

/// Build this turn's thinking sink (see `crate::thinking`). The agent is only
/// resolved for WebChat sessions — the only place its `show_thinking` and
/// contract are consulted for text.
async fn arm_thinking(
    ctx: &ReplyContext,
    agent_override: Option<&str>,
    session_id: &str,
    user_id: &str,
    on_progress: Option<ProgressCallback>,
) -> (
    Option<ProgressCallback>,
    Option<Arc<crate::thinking::ThinkingSink>>,
) {
    if crate::odoo_livechat::is_public_visitor_session(session_id) {
        return (on_progress, None);
    }
    // Messaging apps whose handler installed a thinking presenter: what this
    // sender may see (`thinking::channel_policy`). Without a presenter the
    // channel keeps the text-less signal below.
    if let Some(presenter) = crate::thinking::channel_presenter::current()
        .filter(|_| super::interim::external_channel(session_id).is_some())
    {
        let agent_id = match agent_override {
            Some(a) => a.to_string(),
            None => resolve_agent_for_restore(ctx, session_id).await,
        };
        let show = crate::thinking::channel_policy::resolve_channel(
            &ctx.home_dir,
            &agent_id,
            presenter.channel(),
            user_id,
            presenter.is_group(),
        );
        // When `@` / trigger routing hands the turn to another employee, the
        // presenter decides again for the one that actually answers.
        let resolver: crate::thinking::channel_presenter::ShowResolver = {
            let home = ctx.home_dir.clone();
            let channel = presenter.channel();
            let sender = user_id.to_string();
            let group = presenter.is_group();
            Arc::new(move |agent: &str| {
                crate::thinking::channel_policy::resolve_channel(&home, agent, channel, &sender, group)
            })
        };
        presenter.set_audience(&agent_id, show, resolver);
        let policy = crate::thinking::policy::channel_policy_for(&ctx.home_dir, show);
        let sink_ctx = crate::thinking::SinkCtx {
            home: ctx.home_dir.clone(),
            agent_id,
            redaction: crate::thinking::redaction_for_display(
                ctx.redaction_manager.as_ref(),
                &ctx.home_dir,
            ),
        };
        return crate::thinking::install(Some(policy), on_progress, sink_ctx);
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
    // Boxed so every channel adapter that wraps this future in its own
    // task-local scopes carries a pointer, not the turn state (see the
    // stack note in `build_reply_with_session_inner_capturing_operator_result`).
    let (raw, operator_result_artifact, guards, delivery, answered_by, cited, mut build_rollback) =
        Box::pin(build_reply_with_session_inner_capturing_operator_result(
            text,
            ctx,
            Some(agent_name),
            session_id,
            user_id,
            on_progress,
        ))
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
    let final_text = crate::knowledge::cite_turn::with_footer(final_text, session_id, &cited);
    let reply = attach_turn_rollback(final_text, artifact, guards, delivery, ctx)
        .await
        .with_answered_by(answered_by)
        .with_citations(cited);
    build_rollback.disarm();
    reply
}

/// Keep WebChat's persisted assistant row revocable until delivery even when
/// no CCR lease was used. Other channels retain the existing lease-only gate.
pub(super) async fn attach_turn_rollback(
    text: String,
    artifact: Option<serde_json::Value>,
    guards: Vec<duduclaw_llm::CcrDeliveryGuards>,
    delivery: Arc<CcrTurnDelivery>,
    ctx: &ReplyContext,
) -> GuardedReply {
    if !delivery.waits_for_turn_delivery() && guards
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
    // Boxed for the same reason as in `build_guarded_reply_for_agent`.
    let (raw, operator_result_artifact, guards, delivery, answered_by, cited, mut build_rollback) =
        Box::pin(build_reply_with_session_inner_capturing_operator_result(
            text,
            ctx,
            None,
            session_id,
            user_id,
            on_progress,
        ))
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
    let with_sources = crate::knowledge::cite_turn::with_footer(labelled, session_id, &cited);
    let final_text = append_product_footer(with_sources, session_id);
    let reply = attach_turn_rollback(final_text, artifact, guards, delivery, ctx)
        .await
        .with_answered_by(answered_by)
        .with_citations(cited);
    build_rollback.disarm();
    reply
}

/// Fixed product attribution on external messaging replies. Internal sessions,
/// the dashboard console and deliberate silence remain unchanged.
pub(super) const PRODUCT_FOOTER: &str = "— Powered by DuDuClaw";

/// The text [`append_product_footer`] adds to a reply (for channels that
/// insert more lines above it).
pub(crate) fn product_footer_suffix() -> String {
    format!("\n\n{PRODUCT_FOOTER}")
}

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

/// A conversation on an external messaging platform (LINE, Telegram, …):
/// replies there carry plain-text footers and senders are channel users.
pub fn is_messaging_session(session_id: &str) -> bool {
    footer_applies_to_session(session_id)
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
