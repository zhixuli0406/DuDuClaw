//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Autopilot handlers ──────────────────────────────────

    /// WP14-T14.7: list pending approvals for the approval center. Optional
    /// `agent_id` filter. Read-only; opens approvals.db per call.
    pub(crate) async fn handle_approvals_list(&self, params: Value) -> WsFrame {
        let agent_filter = params.get("agent_id").and_then(|v| v.as_str());
        // Optional exact `action_kind` filter (e.g. "knowledge_quarantine" for the
        // D6 curation queue). Matches the raw stored action_kind string.
        let action_kind_filter = params
            .get("action_kind")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let broker = match crate::approval::ApprovalBroker::open(&self.home_dir) {
            Ok(b) => b,
            Err(e) => return WsFrame::error_response("", &format!("open approvals: {e}")),
        };
        match broker.list_pending(agent_filter).await {
            Ok(rows) => {
                let mut items: Vec<Value> = Vec::with_capacity(rows.len());
                for r in rows
                    .iter()
                    .filter(|r| action_kind_filter.map_or(true, |k| r.action_kind == k))
                {
                    let kind = crate::governance::ApprovalKind::parse(&r.action_kind);
                    // E8 reverse handoff: "在 <通道> 中開啟" — the WP20
                    // `notify_channel`/`notify_chat_id` columns already record
                    // where this approval's decision card was actually pushed
                    // (the conversation to jump back to); `decision_message_store`
                    // has the exact message id for it when a card was recorded
                    // there. A goal-kickoff approval (`action_kind ==
                    // "goal_kickoff"`) files its card under the `goal_kickoff`
                    // namespace instead of `approval` (`goal_notify::notify_goal_kickoff`
                    // is self-notifying — see `SELF_NOTIFYING_KINDS` in
                    // `approval.rs`), so the namespace must match. Only the
                    // resolved URL (or nothing) crosses to the frontend — the raw
                    // `chat_id` never does.
                    let namespace = if r.action_kind == "goal_kickoff" {
                        crate::decision_action::DecisionSource::Kickoff.namespace()
                    } else {
                        crate::decision_action::DecisionSource::Approval.namespace()
                    };
                    let channel_link =
                        match (r.notify_channel.as_deref(), r.notify_chat_id.as_deref()) {
                            (Some(channel), Some(chat_id))
                                if !channel.is_empty() && !chat_id.is_empty() =>
                            {
                                let message_id =
                                    crate::decision_message_store::lookup_card_message(
                                        &self.home_dir,
                                        namespace,
                                        r.id.as_str(),
                                        channel,
                                        chat_id,
                                    )
                                    .map(|p| p.message_id);
                                // W2-7: Discord's guild id was snapshotted onto
                                // this card's entry at push time — pass it
                                // through rather than re-resolving it live.
                                let discord_guild_id =
                                    crate::decision_message_store::lookup_card_discord_guild_id(
                                        &self.home_dir,
                                        namespace,
                                        r.id.as_str(),
                                        channel,
                                        chat_id,
                                    );
                                crate::channel_link::resolve_conversation_link(
                                    &self.home_dir,
                                    channel,
                                    chat_id,
                                    message_id.as_deref(),
                                    discord_guild_id.as_deref(),
                                )
                                .await
                            }
                            _ => None,
                        };
                    items.push(json!({
                        "id": r.id.as_str(),
                        "agent_id": r.agent_id,
                        "kind": kind.as_str(),
                        "summary": r.summary,
                        "payload": r.payload,
                        "created_at": r.created_at,
                        "ttl_seconds": r.ttl_seconds,
                        // Epoch seconds the approval auto-denies at (TTL
                        // expiry counts as a denial — see approval.rs). Lets
                        // the dashboard render a live countdown without
                        // parsing `created_at` itself. `null` on an
                        // unparseable `created_at` (fail-safe).
                        "expires_at": r.expires_at_epoch(),
                        // D1/D2: the ActionGuard judge's forward-simulation
                        // narrative (world_state_change + risk_points), when the
                        // approval kind ran that judge. `null` for every other
                        // kind (the overwhelming majority) — the dashboard
                        // renders nothing when this is absent, purely additive.
                        "simulation": r.simulation,
                        // E8: platform name (safe to expose — no internal id)
                        // + a fully-resolved "open in channel" URL, or `null`
                        // when nothing could be constructed.
                        "channel": r.notify_channel,
                        "channel_link": channel_link,
                    }));
                }
                WsFrame::ok_response("", json!({ "approvals": items, "count": items.len() }))
            }
            Err(e) => WsFrame::error_response("", &format!("list approvals: {e}")),
        }
    }

    /// D5: list routing overrides + pending reroute proposals. Read-only view of
    /// `routing_overrides.json` (fail-safe: missing / corrupt ⇒ empty lists) so
    /// the dashboard can surface D5 topology-evolution state. Proposals are
    /// human-gated through the ApprovalBroker; approve/deny goes through the
    /// generic `approvals.decide` RPC.
    pub(crate) async fn handle_topology_list(&self) -> WsFrame {
        let doc = crate::topology_evolution::load_file(&self.home_dir);
        let overrides: Vec<Value> = doc
            .overrides
            .iter()
            .map(|o| {
                json!({
                    "task_class": o.task_class,
                    "from_agent": o.from_agent,
                    "to_agent": o.to_agent,
                    "approved_at": o.approved_at,
                    "observe_until": o.observe_until,
                    "status": o.status,
                    "baseline_reject_rate": o.baseline_reject_rate,
                    "extended": o.extended,
                })
            })
            .collect();
        let pending: Vec<Value> = doc
            .proposals
            .iter()
            .filter(|p| p.status == crate::topology_evolution::PROPOSAL_PENDING)
            .map(|p| {
                json!({
                    "id": p.id,
                    "task_class": p.task_class,
                    "from_agent": p.from_agent,
                    "to_agent": p.to_agent,
                    "created_at": p.created_at,
                    "approval_id": p.approval_id,
                    "samples": p.samples,
                    "reject_rate": p.reject_rate,
                })
            })
            .collect();
        WsFrame::ok_response(
            "",
            json!({
                "overrides": overrides,
                "pending_proposals": pending,
                "override_count": overrides.len(),
                "pending_count": pending.len(),
            }),
        )
    }

    /// W3-1: conversations a human has currently taken over (read-only).
    ///
    /// No write twin by design — see the `takeover.list` dispatch note.
    /// `holder_user_id` is deliberately omitted: the dashboard needs to show
    /// *who*, and the display name does that without exposing a channel
    /// account id to every manager who opens the page.
    pub(crate) fn handle_takeover_list(&self) -> WsFrame {
        let now = chrono::Utc::now();
        let items: Vec<Value> = duduclaw_core::takeover_state::list_active(&self.home_dir)
            .into_iter()
            .map(|r| {
                json!({
                    "conversation": r.conversation,
                    "channel": r.channel,
                    "channel_label": crate::takeover::channel_label(&r.channel),
                    "chat_id": r.chat_id,
                    "agent_id": r.agent_id,
                    "holder_display": r.holder_display,
                    "started_at": r.started_at.to_rfc3339(),
                    "until": r.until.to_rfc3339(),
                    "minutes_left": r.minutes_left(now),
                    "claimed_task_ids": r.claimed_task_ids,
                })
            })
            .collect();
        WsFrame::ok_response("", json!({ "count": items.len(), "items": items }))
    }
}
