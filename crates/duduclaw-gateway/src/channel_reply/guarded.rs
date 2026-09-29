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
/// (`commercial/docs/DESIGN-agent-body-network-2026-08.md` §5.2/§12), a
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
) {
    let collector: std::sync::Arc<std::sync::Mutex<Vec<crate::runtime::NativeToolEvent>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let ccr_collector = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let delivery_collector = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    // The turn's CCR delivery gate: the inner pipeline persists the assistant
    // message and schedules distillation BEFORE the lease is rechecked, so
    // both register here and are rolled back / never started on a refusal.
    let delivery = Arc::new(CcrTurnDelivery::default());
    // Keep the authenticated channel caller and exact conversation in scope
    // for the whole turn. CCR history validation runs before the native tool
    // loop, and handle persistence runs after it; scoping only `cli_future`
    // leaves both paths (and local-first inference) without an identity.
    let raw = reply_identity_scope(
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
    )
    .await;
    let operator_result_artifact = collector.lock().ok().and_then(|events| {
        crate::os_operator::extract_wifi_password_request_artifact(&events)
            .or_else(|| crate::os_operator::extract_readonly_result_artifact(&events))
    });
    let guards = delivery_collector
        .lock()
        .ok()
        .map(|mut held| std::mem::take(&mut *held));
    (raw, operator_result_artifact, guards, delivery)
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
    let (raw, operator_result_artifact, guards, delivery) =
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
    let restored = restore_for_channel(raw, ctx, agent_name, session_id).await;
    let enforced = enforce_contract(restored, &ctx.home_dir, agent_name).await;
    let final_text = append_pending_agent_notice(enforced, &ctx.home_dir, agent_name);
    attach_turn_rollback(final_text, artifact, guards, delivery, ctx).await
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
    let (raw, operator_result_artifact, guards, delivery) =
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
    let agent_id = resolve_agent_for_restore(ctx, session_id).await;
    let restored = restore_for_channel(raw, ctx, &agent_id, session_id).await;
    let enforced = enforce_contract(restored, &ctx.home_dir, &agent_id).await;
    let with_notice = append_pending_agent_notice(enforced, &ctx.home_dir, &agent_id);
    let final_text = append_branding_footer(with_notice, &ctx.home_dir, session_id).await;
    attach_turn_rollback(final_text, artifact, guards, delivery, ctx).await
}

/// WP1.4 (ecosystem, 2026-08-13 拍板): free-tier branding footer on
/// end-customer channel replies. Free tiers (OpenSource / Hobby) always show
/// it; paid tiers may opt out via `config.toml [branding] reply_footer =
/// false` — the config is license-gated, so flipping it on a free install is
/// a no-op. Consistent with the edition principle: quota/branding-locked,
/// never capability-locked.
///
/// Scope: EXTERNAL channel sessions only (the surfaces end customers see).
/// The dashboard's own WebChat console and internal sessions (cron / bus /
/// "default") stay clean — branding the owner's console serves nobody.
pub(super) const BRANDING_FOOTER: &str = "— Powered by DuDuClaw 🐾";

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

/// `[branding] reply_footer` from config.toml; absent/malformed ⇒ `true`
/// (footer on) — fail-open to visibility, never to silence.
pub(super) fn branding_footer_enabled(home_dir: &std::path::Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(home_dir.join("config.toml")) else {
        return true;
    };
    let Ok(v) = raw.parse::<toml::Value>() else {
        return true;
    };
    v.get("branding")
        .and_then(|b| b.get("reply_footer"))
        .and_then(|x| x.as_bool())
        .unwrap_or(true)
}

pub(super) async fn append_branding_footer(
    reply: String,
    home_dir: &std::path::Path,
    session_id: &str,
) -> String {
    // Deliberate silences (gates upstream) stay silent; non-customer
    // sessions stay unbranded.
    if reply.is_empty() || !footer_applies_to_session(session_id) {
        return reply;
    }
    // Paid tiers may opt out; free tiers (and no-license installs) always
    // show the footer. `global()` absent ⇒ treat as free (fail to visible).
    let paid = match crate::license_runtime::global() {
        Some(rt) => !matches!(
            rt.current_tier().await,
            duduclaw_license::LicenseTier::OpenSource | duduclaw_license::LicenseTier::Hobby
        ),
        None => false,
    };
    if paid && !branding_footer_enabled(home_dir) {
        return reply;
    }
    format!("{reply}\n\n{BRANDING_FOOTER}")
}

